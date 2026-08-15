use super::{ToolRegistry, ToolSpec};
use crate::config::{AppConfig, DiagnosticsPluginConfig};
use anyhow::{bail, Result};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::process::Stdio;
use tokio::process::Command;
use tokio::time::{timeout, Duration};

pub fn register(registry: &mut ToolRegistry, config: AppConfig) {
    registry.register(ToolSpec::new(
        "check_issue",
        "Collect read-only diagnostic evidence for a concrete local issue. This tool gathers facts only; it does not diagnose, rank root causes, or recommend fixes.",
        json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "Original user issue. Used for auto area/target inference." },
                "area": { "type": "string", "enum": ["auto", "system", "app", "input_method", "display", "audio", "package", "gpu", "network", "storage"], "description": "Evidence collection area." },
                "target": { "type": "string", "description": "Optional app, process, command, package, or subsystem target." },
                "symptom": { "type": "string", "description": "Optional symptom label." },
                "depth": { "type": "string", "enum": ["quick", "normal", "full"], "description": "Probe depth." },
                "recent_minutes": { "type": "integer", "description": "Recent log window in minutes, clamped to 1..1440." },
                "platform": { "type": "string", "enum": ["auto", "macos"], "description": "Platform override. Prefer auto." },
                "allow_launch_probe": { "type": "boolean", "description": "For app/input_method evidence only: explicitly allow launching target to sample runtime facts. Defaults to false." },
                "launch_timeout_seconds": { "type": "integer", "description": "Seconds to wait after launch probe before sampling pids. Defaults to 3, max 15." }
            },
            "required": [],
            "additionalProperties": false
        }),
        move |args| {
            let config = config.clone();
            async move { check_issue(args, config.plugins.diagnostics.clone()).await }
        },
    ));
}

#[derive(Debug, Clone)]
struct CheckIssueArgs {
    query: Option<String>,
    area: Area,
    target: Option<String>,
    symptom: Option<String>,
    depth: Depth,
    recent_minutes: u64,
    platform: PlatformArg,
    allow_launch_probe: bool,
    launch_timeout_seconds: u64,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum Area {
    System,
    App,
    InputMethod,
    Display,
    Audio,
    Package,
    Gpu,
    Network,
    Storage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Depth {
    Quick,
    Normal,
    Full,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlatformArg {
    Auto,
    Macos,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Platform {
    Macos,
    Unsupported,
}

#[derive(Debug, Serialize)]
struct EvidenceReport {
    ok: bool,
    kind: &'static str,
    platform: Platform,
    query: Option<String>,
    area: Area,
    target: Option<String>,
    symptom: Option<String>,
    depth: Depth,
    facts: BTreeMap<String, Value>,
    checks: Vec<Check>,
    logs: Vec<LogExcerpt>,
    missing_evidence: Vec<String>,
    safety_notes: Vec<String>,
    recommended_next_probes: Vec<String>,
}

#[derive(Debug, Serialize)]
struct Check {
    id: String,
    status: CheckStatus,
    detail: String,
    evidence: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum CheckStatus {
    Ok,
    Warn,
    Error,
    Unknown,
}

#[derive(Debug, Serialize)]
struct LogExcerpt {
    source: String,
    message: String,
}

#[derive(Debug)]
struct ProbeOutput {
    status: Option<i32>,
    stdout: String,
    stderr: String,
    timed_out: bool,
}

async fn check_issue(args: Value, config: DiagnosticsPluginConfig) -> Result<String> {
    if !config.enabled {
        bail!("diagnostics plugin is disabled");
    }
    let args = parse_args(args)?;
    let platform = detect_platform(args.platform);
    let mut report = EvidenceReport {
        ok: true,
        kind: "diagnostic_evidence",
        platform,
        query: args.query.clone(),
        area: args.area,
        target: args.target.clone(),
        symptom: args.symptom.clone(),
        depth: args.depth,
        facts: BTreeMap::new(),
        checks: Vec::new(),
        logs: Vec::new(),
        missing_evidence: Vec::new(),
        safety_notes: vec![
            "check_issue uses fixed read-only probes and does not diagnose or apply fixes"
                .to_string(),
        ],
        recommended_next_probes: Vec::new(),
    };

    match platform {
        Platform::Macos => collect_macos_evidence(&args, &config, &mut report).await,
        Platform::Unsupported => {
            report.ok = false;
            report.checks.push(Check {
                id: "platform.supported".to_string(),
                status: CheckStatus::Error,
                detail: "check_issue currently supports macOS only".to_string(),
                evidence: vec![std::env::consts::OS.to_string()],
            });
        }
    }
    Ok(serde_json::to_string_pretty(&report)?)
}

fn parse_args(args: Value) -> Result<CheckIssueArgs> {
    let query = optional_string(&args, "query", 500);
    let mut target = optional_string(&args, "target", 160);
    let symptom = optional_string(&args, "symptom", 200);
    let area_raw = args
        .get("area")
        .or_else(|| args.get("mode"))
        .and_then(Value::as_str)
        .unwrap_or("auto")
        .trim();
    let area = if area_raw == "auto" {
        let inferred = infer_area(query.as_deref(), target.as_deref())?;
        if target.is_none() {
            target = infer_target(query.as_deref().unwrap_or_default());
        }
        inferred
    } else {
        parse_area(area_raw)?
    };
    Ok(CheckIssueArgs {
        query,
        area,
        target,
        symptom,
        depth: parse_depth(
            args.get("depth")
                .and_then(Value::as_str)
                .unwrap_or("normal"),
        )?,
        recent_minutes: args
            .get("recent_minutes")
            .and_then(Value::as_u64)
            .unwrap_or(30)
            .clamp(1, 1440),
        platform: parse_platform_arg(
            args.get("platform")
                .and_then(Value::as_str)
                .unwrap_or("auto"),
        )?,
        allow_launch_probe: args
            .get("allow_launch_probe")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        launch_timeout_seconds: args
            .get("launch_timeout_seconds")
            .and_then(Value::as_u64)
            .unwrap_or(3)
            .clamp(1, 15),
    })
}

fn optional_string(args: &Value, name: &str, max_chars: usize) -> Option<String> {
    args.get(name)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.chars().take(max_chars).collect())
}

fn infer_area(query: Option<&str>, target: Option<&str>) -> Result<Area> {
    let text = query.unwrap_or_default();
    let lower = text.to_ascii_lowercase();
    if contains_any(text, &["输入法", "打不了中文", "候选框", "拼音"])
        || contains_any(&lower, &["ime", "input method"])
    {
        Ok(Area::InputMethod)
    } else if contains_any(text, &["没声音", "声音", "麦克风", "耳机"])
        || contains_any(&lower, &["audio", "sound"])
    {
        Ok(Area::Audio)
    } else if contains_any(text, &["屏幕分享", "黑屏", "截图", "录屏", "显示器"])
        || contains_any(&lower, &["display", "screen"])
    {
        Ok(Area::Display)
    } else if contains_any(text, &["更新", "安装包", "依赖", "包管理"])
        || contains_any(&lower, &["brew", "homebrew", "cask"])
    {
        Ok(Area::Package)
    } else if contains_any(text, &["显卡", "驱动", "独显", "核显"])
        || contains_any(&lower, &["gpu", "nvidia", "amd", "metal"])
    {
        Ok(Area::Gpu)
    } else if contains_any(text, &["网络", "联网", "断网", "网卡", "wifi"])
        || contains_any(&lower, &["network", "internet", "wifi", "dns"])
    {
        Ok(Area::Network)
    } else if contains_any(text, &["磁盘", "硬盘", "空间", "挂载"])
        || contains_any(&lower, &["disk", "storage", "mount", "filesystem"])
    {
        Ok(Area::Storage)
    } else if target.is_some()
        || contains_any(text, &["打不开", "启动不了", "闪退", "崩溃", "报错"])
        || contains_any(&lower, &["crash", "cannot start", "won't open", "not open"])
    {
        Ok(Area::App)
    } else if text.trim().is_empty() {
        bail!("area is auto but query is empty; provide query or structured area")
    } else {
        Ok(Area::System)
    }
}

fn infer_target(text: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    for (needle, target) in [
        ("opencode", "opencode"),
        ("qq", "qq"),
        ("微信", "wechat"),
        ("wechat", "wechat"),
        ("steam", "steam"),
        ("firefox", "firefox"),
        ("chrome", "chrome"),
        ("chromium", "chromium"),
        ("vscode", "code"),
        ("code", "code"),
    ] {
        if lower.contains(needle) || text.contains(needle) {
            return Some(target.to_string());
        }
    }
    None
}

fn parse_area(value: &str) -> Result<Area> {
    match value.trim() {
        "system" => Ok(Area::System),
        "app" => Ok(Area::App),
        "input_method" => Ok(Area::InputMethod),
        "display" => Ok(Area::Display),
        "audio" => Ok(Area::Audio),
        "package" | "package_update" => Ok(Area::Package),
        "gpu" => Ok(Area::Gpu),
        "network" => Ok(Area::Network),
        "storage" => Ok(Area::Storage),
        _ => bail!("unsupported diagnostic area: {value}"),
    }
}

fn parse_depth(value: &str) -> Result<Depth> {
    match value.trim() {
        "quick" => Ok(Depth::Quick),
        "normal" => Ok(Depth::Normal),
        "full" => Ok(Depth::Full),
        _ => bail!("unsupported diagnostic depth: {value}"),
    }
}

fn parse_platform_arg(value: &str) -> Result<PlatformArg> {
    match value.trim() {
        "" | "auto" => Ok(PlatformArg::Auto),
        "macos" => Ok(PlatformArg::Macos),
        other => bail!("unsupported platform override: {other}"),
    }
}

fn detect_platform(arg: PlatformArg) -> Platform {
    match arg {
        PlatformArg::Auto => {
            if std::env::consts::OS == "macos" {
                Platform::Macos
            } else {
                Platform::Unsupported
            }
        }
        PlatformArg::Macos => Platform::Macos,
    }
}

/// macOS 证据采集:系统事实 + 目标应用/进程检查。各 area 共享同一组
/// 只读探针(sw_vers / pgrep / which),不做系统级改动。
async fn collect_macos_evidence(
    args: &CheckIssueArgs,
    config: &DiagnosticsPluginConfig,
    report: &mut EvidenceReport,
) {
    fact_env(report, "env.shell", "SHELL");
    fact_env(report, "env.term", "TERM");
    fact_env(report, "env.lang", "LANG");
    let sw_vers = run_command(config, "sw_vers", &[], 2).await;
    push_log_if_stdout(report, "sw_vers", &sw_vers);
    if let Some(text) = crate::host_info::macos_system_version_text() {
        report.facts.insert(
            "os.system_version".to_string(),
            crate::host_info::parse_macos_system_version(Some(&text)),
        );
    }
    if matches!(args.area, Area::App | Area::InputMethod) {
        if let Some(target) = args.target.as_deref() {
            command_exists_check(config, report, target).await;
            process_check(config, report, target).await;
        } else {
            report
                .missing_evidence
                .push("target app was not provided".to_string());
        }
    }
}

async fn command_exists_check(
    config: &DiagnosticsPluginConfig,
    report: &mut EvidenceReport,
    name: &str,
) {
    let path = command_path(config, name).await;
    report.checks.push(Check {
        id: format!("command.{name}.exists"),
        status: if path.is_some() {
            CheckStatus::Ok
        } else {
            CheckStatus::Unknown
        },
        detail: if path.is_some() {
            format!("{name} is available")
        } else {
            format!("{name} is not available")
        },
        evidence: path.into_iter().collect(),
    });
}

async fn process_check(
    config: &DiagnosticsPluginConfig,
    report: &mut EvidenceReport,
    name: &str,
) -> Vec<u32> {
    let output = run_command(config, "pgrep", &["-af", name], 2).await;
    let matches = filtered_process_matches(&output.stdout, name);
    report.checks.push(Check {
        id: format!("process.{name}.running"),
        status: if matches.is_empty() {
            CheckStatus::Unknown
        } else {
            CheckStatus::Ok
        },
        detail: if matches.is_empty() {
            format!("no process matching {name} was found")
        } else {
            format!("process matching {name} is running")
        },
        evidence: if matches.is_empty() {
            Vec::new()
        } else {
            vec![clip(&matches.join("
"), 1_000)]
        },
    });
    matches
        .iter()
        .filter_map(|line| line.split_whitespace().next()?.parse::<u32>().ok())
        .collect()
}
fn filtered_process_matches(output: &str, name: &str) -> Vec<String> {
    let name_lower = name.to_ascii_lowercase();
    let mut matches = output
        .lines()
        .filter(|line| {
            let lower = line.to_ascii_lowercase();
            lower.contains(&name_lower)
                && !lower.contains("pgrep -af")
                && !lower.contains("/usr/bin/bash -c")
                && !lower.contains("/bin/sh -c")
                && !line_starts_with_pid(line, std::process::id())
        })
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    matches.sort();
    matches
}

fn line_starts_with_pid(line: &str, pid: u32) -> bool {
    line.split_whitespace()
        .next()
        .and_then(|value| value.parse::<u32>().ok())
        == Some(pid)
}

async fn command_path(config: &DiagnosticsPluginConfig, command: &str) -> Option<String> {
    if !safe_command_name(command) {
        return None;
    }
    let output = run_command(config, "which", &[command], 2).await;
    (output.status == Some(0))
        .then(|| {
            output
                .stdout
                .lines()
                .next()
                .unwrap_or_default()
                .trim()
                .to_string()
        })
        .filter(|value| !value.is_empty())
}

async fn run_command(
    config: &DiagnosticsPluginConfig,
    command: &str,
    args: &[&str],
    timeout_seconds: u64,
) -> ProbeOutput {
    if !safe_command_name(command) {
        return ProbeOutput {
            status: None,
            stdout: String::new(),
            stderr: String::new(),
            timed_out: false,
        };
    }
    let result = timeout(
        Duration::from_secs(timeout_seconds.min(config.command_timeout_seconds).max(1)),
        Command::new(command)
            .args(args)
            .stdin(Stdio::null())
            .output(),
    )
    .await;
    match result {
        Ok(Ok(output)) => ProbeOutput {
            status: output.status.code(),
            stdout: clip(
                &String::from_utf8_lossy(&output.stdout),
                config.max_stdout_chars,
            ),
            stderr: clip(
                &String::from_utf8_lossy(&output.stderr),
                config.max_stderr_chars,
            ),
            timed_out: false,
        },
        Ok(Err(err)) => ProbeOutput {
            status: None,
            stdout: String::new(),
            stderr: err.to_string(),
            timed_out: false,
        },
        Err(_) => ProbeOutput {
            status: None,
            stdout: String::new(),
            stderr: String::new(),
            timed_out: true,
        },
    }
}

fn fact_env(report: &mut EvidenceReport, key: &str, env: &str) {
    if let Ok(value) = std::env::var(env) {
        if !value.trim().is_empty() {
            report.facts.insert(key.to_string(), json!(redact(&value)));
        }
    }
}

fn push_log_if_stdout(report: &mut EvidenceReport, source: &str, output: &ProbeOutput) {
    if !output.stdout.trim().is_empty() {
        push_log(report, source, &output.stdout);
    }
}

fn push_log(report: &mut EvidenceReport, source: &str, message: &str) {
    if !message.trim().is_empty() {
        report.logs.push(LogExcerpt {
            source: source.to_string(),
            message: clip(message, 2_000),
        });
    }
}
fn redact(value: impl AsRef<str>) -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    if home.is_empty() {
        value.as_ref().to_string()
    } else {
        value.as_ref().replace(&home, "$HOME")
    }
}

fn clip(value: &str, max_chars: usize) -> String {
    let value = value.trim();
    if value.chars().count() <= max_chars {
        value.to_string()
    } else {
        format!(
            "{}...",
            value
                .chars()
                .take(max_chars.saturating_sub(3))
                .collect::<String>()
        )
    }
}

fn contains_any(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| text.contains(needle))
}

fn safe_command_name(value: &str) -> bool {
    !value.trim().is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '+'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_issue_infers_input_method_area() {
        let args = parse_args(json!({"query": "QQ 打不了中文", "target": "qq"})).unwrap();
        assert!(matches!(args.area, Area::InputMethod));
    }

    #[test]
    fn check_issue_infers_package_area_for_brew() {
        let args = parse_args(json!({"query": "brew install 报错"})).unwrap();
        assert!(matches!(args.area, Area::Package));
    }

    #[test]
    fn platform_override_accepts_auto_and_macos() {
        assert!(matches!(
            parse_platform_arg("auto").unwrap(),
            PlatformArg::Auto
        ));
        assert!(matches!(
            parse_platform_arg("macos").unwrap(),
            PlatformArg::Macos
        ));
        assert!(parse_platform_arg("linux").is_err());
    }
}
