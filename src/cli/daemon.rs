//! daemon — 自 src/cli.rs 拆分。

use super::*;

#[derive(Debug, Args)]
pub struct KbRemoveArgs {
    pub file: String,
}

#[derive(Debug, Args)]
pub struct KbEmbedArgs {
    #[command(subcommand)]
    pub command: KbEmbedCommand,
}

#[derive(Debug, Subcommand)]
pub enum KbEmbedCommand {
    Reindex(KbEmbedReindexArgs),
}

#[derive(Debug, Args)]
pub struct KbEmbedReindexArgs {
    #[arg(long)]
    pub quiet: bool,
}

#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    Validate,
    Paths,
    #[command(hide = true)]
    PromptSource,
}

pub async fn run(cli: Cli, paths: GQYPaths) -> Result<()> {
    if cli.shell_classify {
        let shell_name = cli.shell.as_deref().unwrap_or("fish");
        let message = shell_message_from_input(cli.stdin, cli.message)?;
        return run_shell_classify(shell_name, &message);
    }

    if cli.clipboard_paste {
        return run_clipboard_paste(&paths);
    }
    // A log viewer must not append its own startup record to the file it is
    // about to display. Apart from being confusing, that made `-n 1` return
    // the viewer's initialization line instead of the daemon's latest event.
    let skip_diagnostic_logging = matches!(
        &cli.command,
        Some(Command::Daemon(DaemonArgs {
            command: Some(DaemonCommand::Logs(_)),
            ..
        }))
    );
    let _logging_guard = if skip_diagnostic_logging {
        None
    } else {
        match crate::logging::init(&paths, cli.debug) {
            Ok(guard) => Some(guard),
            Err(err) => {
                eprintln!(
                    "{}: {err:#}",
                    t(
                        "warning: diagnostic logging is unavailable",
                        "警告：诊断日志不可用"
                    )
                );
                None
            }
        }
    };
    let mode = AgentMode::Normal;

    if cli.shell_intercept {
        let shell_name = cli.shell.as_deref().unwrap_or("fish");
        let message = shell_message_from_input(cli.stdin, cli.message)?;
        return run_shell_intercept(&paths, shell_name, message).await;
    }

    if !paths.config_file.exists()
        && !matches!(
            cli.command,
            Some(Command::Init)
                | Some(Command::FishInit)
                | Some(Command::BashInit)
                | Some(Command::ZshInit)
                | Some(Command::RemoveShellHook)
                | Some(Command::Paths)
                | Some(Command::Import(_))
        )
    {
        run_init(&paths, InitKind::FirstRun)?;
    }

    // Captured before `cli.command` is moved out: one-shot entry points below
    // need them to pick the session their turn lands in.
    let session_arg = cli.session.clone();
    let continue_session = cli.continue_session;

    match cli.command {
        Some(Command::AlarmWorker(args)) => run_alarm_worker(args),
        Some(Command::DaemonWorker(args)) => {
            let _logging_guard = crate::logging::init(&paths, cli.debug).ok();
            crate::daemon::run(paths, args).await
        }
        Some(Command::Tool(args)) => run_tool(&paths, mode, args).await,
        Some(Command::Ask(args)) => {
            let session =
                one_shot_session(&paths, session_arg.as_deref(), continue_session).await?;
            run_chat_with_options(
                &paths,
                join_message(args.message),
                None,
                cli.stdout,
                mode,
                session,
            )
            .await
        }
        Some(Command::Init) => run_init(&paths, InitKind::Explicit),
        Some(Command::Paths) => {
            paths.print();
            Ok(())
        }
        Some(Command::Config(args)) => {
            let saved = run_config(&paths, args).await?;
            if saved && ipc::daemon_info(&paths).await.is_some() {
                reload_daemon_if_running(&paths).await
            } else {
                if saved {
                    let config = AppConfig::load_or_default(&paths)?;
                    if config.platforms.qq.enabled {
                        println!(
                            "{}",
                            t(
                                "Tencent QQ is enabled; run `gqy daemon start` to begin listening.",
                                "腾讯 QQ 已启用；执行 `gqy daemon start` 后开始监听。",
                            )
                        );
                    }
                }
                Ok(())
            }
        }
        Some(Command::Reload) => run_reload(&paths).await,
        Some(Command::Models(args)) => {
            initialize_models_cache(&paths);
            run_models(&paths, args).await
        }
        Some(Command::Export(args)) => run_export(&paths, args),
        Some(Command::Import(args)) => run_import(&paths, args).await,
        Some(Command::ListModels) => {
            initialize_models_cache(&paths);
            run_list_models(&paths)
        }
        Some(Command::Variant(args)) => {
            initialize_models_cache(&paths);
            run_variant(&paths, args)?;
            reload_daemon_if_running(&paths).await
        }
        Some(Command::FishInit) => shell::fish::install(&paths),
        Some(Command::BashInit) => shell::bash::install(&paths),
        Some(Command::ZshInit) => shell::zsh::install(&paths),
        Some(Command::RemoveShellHook) => remove_shell_hooks(&paths),
        Some(Command::History(args)) => run_history(&paths, args),
        Some(Command::Pop(args)) => {
            if ipc::daemon_info(&paths).await.is_some() {
                run_pop_via_daemon(&paths, args).await
            } else {
                run_pop(&paths, args)
            }
        }
        Some(Command::Kb(args)) => run_kb(&paths, args).await,
        Some(Command::UpdateDefaultKb) => run_update_default_kb(&paths).await,
        Some(Command::Memory(args)) => run_memory(&paths, args),
        Some(Command::Skills(args)) => run_skills(&paths, args),
        Some(Command::ResetMemoryCli) => run_reset_memory_command(&paths).await,
        Some(Command::Reset) => {
            if ipc::daemon_info(&paths).await.is_some() {
                send_ipc_admin(
                    &paths,
                    IpcCommand::ResetConversation {
                        target: crate::ipc::SessionRef::Current,
                    },
                )
                .await?;
            } else {
                run_reset(&paths).await?;
            }
            print_reset_message();
            Ok(())
        }
        Some(Command::Wipe(args)) => run_wipe(&paths, args.yes).await,
        Some(Command::ToolCallCmd(args)) => run_tool_call(&paths, args).await,
        Some(Command::Normal) => run_repl(&paths, AgentMode::Normal).await,
        Some(Command::Dev) => run_repl(&paths, AgentMode::Dev).await,
        Some(Command::Web(args)) => run_web(&paths, args).await,
        Some(Command::Daemon(args)) => run_daemon_command(&paths, args).await,
        None => {
            let message = join_message(cli.message);
            if message.is_empty() && io::stdin().is_terminal() {
                if session_arg.is_some() || continue_session {
                    bail!(
                        "{}",
                        t(
                            "--session and --continue only apply to one-shot commands; use /session inside the REPL",
                            "--session 与 --continue 仅用于一次性命令；REPL 内请使用 /session 切换"
                        )
                    );
                }
                // 裸 gqy:按 default_mode 配置分流;未配置则打印模式说明,
                // 逼一次显式选择(gqy normal / gqy dev)。
                let default_mode = AppConfig::load_or_default(&paths)
                    .map(|config| config.default_mode.trim().to_ascii_lowercase())
                    .unwrap_or_default();
                match default_mode.as_str() {
                    "normal" => run_repl(&paths, AgentMode::Normal).await,
                    "dev" => run_repl(&paths, AgentMode::Dev).await,
                    "" => {
                        print_mode_help();
                        Ok(())
                    }
                    other => bail!(
                        "{}: {other}",
                        t(
                            "invalid default_mode (expected normal or dev)",
                            "default_mode 配置无效(应为 normal 或 dev)"
                        )
                    ),
                }
            } else {
                let session =
                    one_shot_session(&paths, session_arg.as_deref(), continue_session).await?;
                run_chat_with_options(&paths, message, None, cli.stdout, mode, session).await
            }
        }
    }
}

pub(crate) fn initialize_models_cache(paths: &GQYPaths) {
    crate::models_cache::try_load(paths);
    crate::models_cache::spawn_background_refresh(paths.clone());
    if let Ok(config) = AppConfig::load_or_default(paths) {
        crate::models_cache::spawn_provider_api_refresh(config.providers);
    }
}

pub(crate) async fn run_web(paths: &GQYPaths, mut args: WebArgs) -> Result<()> {
    if let Some(info) = ipc::daemon_info(paths).await {
        if info.build_id == ipc::BUILD_ID {
            if args.port_explicit || args.password.is_some() || args.password_file.is_some() {
                bail!(
                    "{}",
                    t(
                        "the running GQY daemon already owns Web settings; restart it to change them",
                        "当前 GQY daemon 已接管 Web 设置；如需修改请先重启 daemon"
                    )
                );
            }
            for url in daemon_web_access_urls(&info) {
                println!("GQY WebUI: {url}");
            }
            return Ok(());
        }
    }

    if args.password.as_deref() == Some("") {
        args.password = Some(rpassword::prompt_password(t(
            "WebUI password: ",
            "WebUI 密码：",
        ))?);
    }
    let launch = web_launch_config(paths, &args)?;
    let info = ipc::ensure_daemon(paths, launch.as_ref()).await?;
    for url in daemon_web_access_urls(&info) {
        println!("GQY WebUI: {url}");
    }
    Ok(())
}

pub(crate) fn web_launch_config(
    paths: &GQYPaths,
    args: &WebArgs,
) -> Result<Option<ipc::DaemonLaunchConfig>> {
    if !args.port_explicit
        && args.bind.is_none()
        && args.password.is_none()
        && args.password_file.is_none()
    {
        return Ok(None);
    }
    let password_file = match args.password.as_deref() {
        Some("") => bail!(
            "{}",
            t("WebUI password cannot be empty", "WebUI 密码不能为空")
        ),
        Some(password) if password.chars().count() > 1_024 => bail!(
            "{}",
            t(
                "WebUI password cannot exceed 1,024 characters",
                "WebUI 密码不能超过 1,024 个字符"
            )
        ),
        Some(password) => Some(ipc::stage_managed_web_password(paths, password)?),
        None => args
            .password_file
            .as_deref()
            .map(|path| ipc::stage_web_password_file(paths, path))
            .transpose()?,
    };
    Ok(Some(ipc::DaemonLaunchConfig {
        port: args.port,
        password_file,
        bind: args.bind,
    }))
}

pub(crate) fn daemon_web_access_urls(info: &ipc::DaemonInfo) -> Vec<String> {
    let bind = info
        .web_bind
        .unwrap_or(std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED));
    ipc::web_access_urls_for(bind, info.web_port)
}

pub(crate) async fn run_daemon_command(paths: &GQYPaths, args: DaemonArgs) -> Result<()> {
    let command = args.command.unwrap_or(DaemonCommand::Start);
    if args.port.is_some() && !matches!(command, DaemonCommand::Start | DaemonCommand::Restart) {
        bail!(
            "{}",
            t(
                "--port only applies to daemon start or restart",
                "--port 仅适用于 daemon start 或 restart"
            )
        );
    }

    match command {
        DaemonCommand::Start => {
            let launch = args
                .port
                .map(|port| ipc::daemon_launch_config_with_port(paths, port))
                .transpose()?;
            if launch.is_some()
                && ipc::daemon_info(paths)
                    .await
                    .is_some_and(|info| info.build_id == ipc::BUILD_ID)
            {
                bail!(
                    "{}",
                    t(
                        "the running GQY daemon already owns Web settings; use `gqy daemon restart` to change the port",
                        "当前 GQY daemon 已接管 Web 设置；如需修改端口请使用 `gqy daemon restart`"
                    )
                );
            }
            ipc::ensure_daemon(paths, launch.as_ref()).await?;
            let refreshed = GQYPaths::new()?;
            print_daemon_status(&refreshed).await
        }
        DaemonCommand::Stop => stop_daemon(paths).await,
        DaemonCommand::Restart => {
            let pending_launch = if let Some(port) = args.port {
                Some(ipc::daemon_launch_config_with_port(paths, port)?)
            } else {
                match ipc::daemon_info(paths).await {
                    Some(info) => ipc::recover_daemon_launch_if_missing(paths, info.pid)?,
                    None => None,
                }
            };
            if let Err(error) = stop_daemon(paths).await {
                if let Some(launch) = &pending_launch {
                    ipc::discard_daemon_launch_candidate(paths, launch);
                }
                return Err(error);
            };
            let refreshed = match GQYPaths::new() {
                Ok(paths) => paths,
                Err(error) => {
                    if let Some(launch) = &pending_launch {
                        ipc::discard_daemon_launch_candidate(paths, launch);
                    }
                    return Err(error);
                }
            };
            if let Err(error) = ipc::ensure_daemon(&refreshed, pending_launch.as_ref()).await {
                if let Some(launch) = &pending_launch {
                    ipc::discard_daemon_launch_candidate(&refreshed, launch);
                }
                return Err(error);
            }
            print_daemon_status(&refreshed).await
        }
        DaemonCommand::Status => print_daemon_status(paths).await,
        DaemonCommand::Logs(args) => run_daemon_logs(paths, args).await,
    }
}

pub(crate) async fn stop_daemon(paths: &GQYPaths) -> Result<()> {
    let Some(info) = ipc::daemon_info(paths).await else {
        println!("{}", t("GQY daemon is not running", "GQY daemon 未运行"));
        return Ok(());
    };
    ipc::shutdown_daemon(paths, &info).await?;
    println!("{}", t("GQY daemon stopped", "GQY daemon 已停止"));
    Ok(())
}

pub(crate) async fn print_daemon_status(paths: &GQYPaths) -> Result<()> {
    let Some(info) = ipc::daemon_info(paths).await else {
        println!("{}", t("GQY daemon: stopped", "GQY daemon：已停止"));
        return Ok(());
    };
    let (_, data) = send_ipc_admin(paths, IpcCommand::GetStatus).await?;
    println!(
        "{} {} (PID {})",
        t("GQY daemon:", "GQY daemon："),
        t("running", "运行中"),
        info.pid,
    );
    for line in daemon_web_status_lines(t("WebUI:", "WebUI："), &daemon_web_access_urls(&info)) {
        println!("{line}");
    }
    let engine = data
        .pointer("/runtime/turn_engine")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("ready");
    println!("{} {}", t("Turn engine:", "TurnEngine："), engine);

    let qq = data.pointer("/platforms/qq");
    let enabled = qq
        .and_then(|value| value.get("enabled"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    if !enabled {
        println!(
            "{} {}",
            t("Tencent QQ:", "腾讯 QQ："),
            t("disabled", "未启用")
        );
        return Ok(());
    }
    let port = qq
        .and_then(|value| value.get("listen_port"))
        .and_then(serde_json::Value::as_u64)
        .unwrap_or_default();
    let accounts = qq
        .and_then(|value| value.get("connected_accounts"))
        .and_then(serde_json::Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(serde_json::Value::as_i64)
                .map(|value| value.to_string())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let connection = if accounts.is_empty() {
        t("not connected", "尚未连接").to_string()
    } else {
        format!("{}: {}", t("connected", "已连接"), accounts.join(", "))
    };
    println!(
        "{} ws://localhost:{port}/ws · {connection}",
        t("Tencent QQ:", "腾讯 QQ：")
    );
    Ok(())
}

pub(crate) fn daemon_web_status_lines(label: &str, urls: &[String]) -> Vec<String> {
    let Some((first, remaining)) = urls.split_first() else {
        return vec![label.to_string()];
    };
    let indent = " ".repeat(visible_width(label).saturating_add(1));
    std::iter::once(format!("{label} {first}"))
        .chain(remaining.iter().map(|url| format!("{indent}{url}")))
        .collect()
}

/// `gqy daemon logs request`:监控期间开启录制,滚动打印每个出网请求
/// 的摘要行;完整请求体在 JSONL 文件里(整段 prompt 打终端没法看)。
/// Ctrl+C 退出时关闭录制——开关是 daemon 进程级内存位,不落配置。
pub(crate) async fn run_request_monitor(paths: &GQYPaths) -> Result<()> {
    if ipc::daemon_info(paths).await.is_none() {
        bail!(
            "{}",
            t(
                "the daemon is not running; start it first (gqy daemon start)",
                "daemon 未运行;先 gqy daemon start"
            )
        );
    }
    let (_, data) = send_ipc_admin(paths, IpcCommand::SetRequestLogging { enabled: true }).await?;
    let file = data
        .get("file")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| {
            paths
                .logs_dir()
                .join("requests-<date>.jsonl")
                .display()
                .to_string()
        });
    println!(
        "{}",
        t(
            "request recording is ON; full bodies append to:",
            "出网请求录制已开启;完整请求体实时追加到:"
        )
    );
    println!("  {file}");
    println!(
        "[2m{}[0m",
        t(
            "monitoring (one summary line per request) · Ctrl+C stops and turns recording off",
            "实时监控中(每请求一行摘要) · Ctrl+C 停止并关闭录制"
        )
    );
    let path = std::path::PathBuf::from(&file);
    let mut offset = std::fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0);
    let mut carry = String::new();
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            _ = tokio::time::sleep(Duration::from_millis(300)) => {}
        }
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        if meta.len() < offset {
            offset = 0; // 跨日换文件或被清空
        }
        if meta.len() == offset {
            continue;
        }
        use std::io::{Read as _, Seek as _};
        let Ok(mut handle) = std::fs::File::open(&path) else {
            continue;
        };
        if handle.seek(std::io::SeekFrom::Start(offset)).is_err() {
            continue;
        }
        let mut chunk = String::new();
        if handle.read_to_string(&mut chunk).is_err() {
            continue;
        }
        offset = meta.len();
        carry.push_str(&chunk);
        while let Some(newline) = carry.find('\n') {
            let line: String = carry.drain(..=newline).collect();
            let Ok(entry) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
                continue;
            };
            let text = |key: &str| {
                entry
                    .get(key)
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("?")
            };
            let body = entry.get("body");
            let messages = body
                .and_then(|body| body.get("messages").or_else(|| body.get("input")))
                .and_then(serde_json::Value::as_array)
                .map(|items| items.len());
            let size_kb = line.len() as f64 / 1024.0;
            let stamp = text("ts").get(11..19).unwrap_or("--:--:--").to_string();
            println!(
                "{stamp}  {}/{}  {}  scope={}  {:.1}KB{}",
                text("provider"),
                text("model"),
                text("kind"),
                text("scope"),
                size_kb,
                messages
                    .map(|count| format!("  messages={count}"))
                    .unwrap_or_default(),
            );
        }
    }
    let _ = send_ipc_admin(paths, IpcCommand::SetRequestLogging { enabled: false }).await;
    println!(
        "
{}
  {file}",
        t(
            "recording is OFF; inspect full bodies with jq:",
            "录制已关闭;用 jq 查看完整请求体:"
        )
    );
    Ok(())
}

pub(crate) async fn run_daemon_logs(paths: &GQYPaths, args: DaemonLogsArgs) -> Result<()> {
    match args.topic.as_deref().map(str::trim) {
        None => {}
        Some("request" | "requests") => return run_request_monitor(paths).await,
        Some(other) => bail!(
            "{}: {other}",
            t(
                "unknown logs topic (try: request)",
                "未知日志主题(可用: request)"
            )
        ),
    }
    let ansi = io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();
    if let Some(lines) = args.lines {
        if !(1..=100_000).contains(&lines) {
            bail!(
                "{}",
                t(
                    "--lines must be between 1 and 100000",
                    "--lines 必须在 1 到 100000 之间"
                )
            );
        }
        let snapshot = recent_daemon_log_snapshot(paths, lines)?;
        write_daemon_log_lines(&snapshot.lines, ansi)?;
        return Ok(());
    }

    let snapshot = recent_daemon_log_snapshot(paths, 50)?;
    write_daemon_log_lines(&snapshot.lines, ansi)?;
    // The cursor is tied to the exact EOF captured by the snapshot reader.
    // Bytes appended while the snapshot is printed or while the status probe
    // runs are therefore consumed by follow instead of being skipped.
    let cursor = snapshot.cursor;
    let Some(daemon) = ipc::daemon_info(paths).await else {
        bail!("{}", t("GQY daemon is not running", "GQY daemon 未运行"));
    };
    follow_daemon_log(paths, ansi, cursor, daemon.pid).await
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct ParsedDaemonLogLine<'a> {
    timestamp: &'a str,
    pub(crate) level: &'a str,
    pub(crate) module: &'a str,
    pub(crate) message: &'a str,
}

pub(crate) fn parse_daemon_log_line(line: &str) -> Option<ParsedDaemonLogLine<'_>> {
    let timestamp_end = line.find(char::is_whitespace)?;
    let timestamp = &line[..timestamp_end];
    DateTime::parse_from_rfc3339(timestamp).ok()?;

    let remainder = line[timestamp_end..].trim_start();
    let level_end = remainder.find(char::is_whitespace)?;
    let level = &remainder[..level_end];
    if !matches!(level, "TRACE" | "DEBUG" | "INFO" | "WARN" | "ERROR") {
        return None;
    }

    let remainder = remainder[level_end..].trim_start();
    let (module, message) = remainder
        .split_once(": ")
        .filter(|(candidate, _)| is_gqy_log_target(candidate))
        .unwrap_or(("gqy", remainder));
    Some(ParsedDaemonLogLine {
        timestamp,
        level,
        module,
        message,
    })
}

pub(crate) fn is_gqy_log_target(value: &str) -> bool {
    value == "gqy"
        || value
            .strip_prefix("gqy::")
            .is_some_and(|suffix| !suffix.is_empty())
}

pub(crate) fn format_daemon_log_line(line: &str, ansi: bool) -> String {
    let mut decision_color = None;
    format_daemon_log_line_with_state(line, ansi, &mut decision_color)
}

pub(crate) fn format_daemon_log_line_with_state(
    line: &str,
    ansi: bool,
    decision_color: &mut Option<Color>,
) -> String {
    let Some(parsed) = parse_daemon_log_line(line) else {
        if let Some(color) = active_reply_log_color(line) {
            *decision_color = Some(color);
        }
        return decision_color.map_or_else(
            || line.to_string(),
            |color| color_log_part(line.to_string(), color, ansi),
        );
    };
    *decision_color = active_reply_log_color(parsed.message);
    let timestamp = DateTime::parse_from_rfc3339(parsed.timestamp)
        .map(|value| {
            value
                .with_timezone(&Local)
                .format("%H:%M:%S%.3f")
                .to_string()
        })
        .unwrap_or_else(|_| parsed.timestamp.to_string());
    let timestamp = color_log_part(format!("[{timestamp}]"), Color::DarkGreen, ansi);
    let level = color_log_part(
        format!("[{}]", parsed.level),
        log_level_color(parsed.level),
        ansi,
    );
    let module = color_log_part(format!("[{}]", parsed.module), Color::DarkCyan, ansi);
    if parsed.message.is_empty() {
        format!("{timestamp} {level} {module}")
    } else {
        format!(
            "{timestamp} {level} {module} {}",
            decision_color.map_or_else(
                || parsed.message.to_string(),
                |color| color_log_part(parsed.message.to_string(), color, ansi),
            )
        )
    }
}

pub(crate) fn active_reply_log_color(value: &str) -> Option<Color> {
    match value.trim_start().lines().next().unwrap_or_default() {
        "【续聊窗口判断：回复】"
        | "【主动回复判断：回复】"
        | "[Continuation decision: reply]"
        | "[Active reply decision: reply]" => Some(Color::Green),
        "【续聊窗口判断：不回复】"
        | "【主动回复判断：不回复】"
        | "[Continuation decision: no reply]"
        | "[Active reply decision: no reply]" => Some(Color::DarkGrey),
        _ => None,
    }
}

pub(crate) fn log_level_color(level: &str) -> Color {
    match level {
        "ERROR" => Color::Red,
        "WARN" => Color::Yellow,
        "INFO" => Color::Green,
        "DEBUG" => Color::Cyan,
        _ => Color::DarkGrey,
    }
}

pub(crate) fn color_log_part(value: String, color: Color, ansi: bool) -> String {
    if ansi {
        format!("{}", value.with(color))
    } else {
        value
    }
}

pub(crate) fn write_daemon_log_lines(lines: &[String], ansi: bool) -> Result<()> {
    let stdout = io::stdout();
    let mut output = stdout.lock();
    let mut decision_color = None;
    for line in lines {
        writeln!(
            output,
            "{}",
            format_daemon_log_line_with_state(line, ansi, &mut decision_color)
        )?;
    }
    output.flush()?;
    Ok(())
}

#[derive(Default)]
pub(crate) struct DaemonLogStreamFormatter {
    pending: Vec<u8>,
    decision_color: Option<Color>,
}

impl DaemonLogStreamFormatter {
    pub(crate) fn push(
        &mut self,
        bytes: &[u8],
        ansi: bool,
        output: &mut impl Write,
    ) -> io::Result<()> {
        self.pending.extend_from_slice(bytes);
        let Some(last_newline) = self.pending.iter().rposition(|byte| *byte == b'\n') else {
            return Ok(());
        };
        let remainder = self.pending.split_off(last_newline + 1);
        let complete = std::mem::replace(&mut self.pending, remainder);
        for mut line in complete[..last_newline].split(|byte| *byte == b'\n') {
            if line.last() == Some(&b'\r') {
                line = &line[..line.len() - 1];
            }
            write_daemon_log_line_bytes(line, ansi, &mut self.decision_color, output)?;
        }
        Ok(())
    }

    pub(crate) fn finish(&mut self, ansi: bool, output: &mut impl Write) -> io::Result<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let mut line = std::mem::take(&mut self.pending);
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        write_daemon_log_line_bytes(&line, ansi, &mut self.decision_color, output)
    }
}

pub(crate) fn write_daemon_log_line_bytes(
    line: &[u8],
    ansi: bool,
    decision_color: &mut Option<Color>,
    output: &mut impl Write,
) -> io::Result<()> {
    writeln!(
        output,
        "{}",
        format_daemon_log_line_with_state(&String::from_utf8_lossy(line), ansi, decision_color,)
    )
}

pub(crate) fn daemon_log_files(paths: &GQYPaths) -> Result<Vec<PathBuf>> {
    let mut files = match std::fs::read_dir(paths.logs_dir()) {
        Ok(entries) => entries
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("gqy.") && name.ends_with(".log"))
            })
            .collect::<Vec<_>>(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error.into()),
    };
    files.sort();
    // The daemon's inherited stdout/stderr goes to daemon.log. Keep it in
    // the history even when tracing has already created rolling log files;
    // startup failures and panics can happen before the tracing layer writes
    // anything useful.
    let fallback = paths.logs_dir().join("daemon.log");
    if fallback.is_file() && !files.iter().any(|path| path == &fallback) {
        // Treat the unstructured process stream as the oldest source for the
        // bounded recent view. The newest rolling file remains last.
        files.insert(0, fallback);
    }
    Ok(files)
}

pub(crate) fn is_daemon_fallback_log(path: &Path) -> bool {
    path.file_name().and_then(|name| name.to_str()) == Some("daemon.log")
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DaemonLogFollowCursor {
    pub(crate) current: Option<PathBuf>,
    pub(crate) current_offset: u64,
    pub(crate) fallback: Option<PathBuf>,
    pub(crate) fallback_offset: u64,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct DaemonLogSnapshot {
    pub(crate) lines: Vec<String>,
    pub(crate) cursor: DaemonLogFollowCursor,
}

pub(crate) fn daemon_log_follow_cursor_for_files(
    files: &[PathBuf],
    offsets: &[(PathBuf, u64)],
) -> DaemonLogFollowCursor {
    let fallback = files
        .iter()
        .find(|path| is_daemon_fallback_log(path))
        .cloned();
    let current = files
        .iter()
        .rev()
        .find(|path| !is_daemon_fallback_log(path))
        .cloned()
        .or_else(|| fallback.clone());
    let file_offset = |path: Option<&PathBuf>| {
        path.and_then(|path| {
            offsets
                .iter()
                .find(|(candidate, _)| candidate == path)
                .map(|(_, offset)| *offset)
        })
        .unwrap_or_else(|| {
            path.and_then(|path| std::fs::metadata(path).ok())
                .map_or(0, |metadata| metadata.len())
        })
    };
    DaemonLogFollowCursor {
        current_offset: file_offset(current.as_ref()),
        fallback_offset: file_offset(fallback.as_ref()),
        current,
        fallback,
    }
}

pub(crate) fn recent_daemon_log_snapshot(
    paths: &GQYPaths,
    limit: usize,
) -> Result<DaemonLogSnapshot> {
    let files = daemon_log_files(paths)?;
    if limit == 0 {
        return Ok(DaemonLogSnapshot {
            lines: Vec::new(),
            cursor: daemon_log_follow_cursor_for_files(&files, &[]),
        });
    }
    // Record the initial EOF of every source before reading any tails. A
    // writer that appends after this point is intentionally left for follow;
    // it can never be lost in the snapshot-to-follow hand-off.
    let mut offsets = files
        .iter()
        .map(|path| {
            let offset = std::fs::metadata(path).map_or(0, |metadata| metadata.len());
            (path.clone(), offset)
        })
        .collect::<Vec<_>>();
    let mut lines = Vec::with_capacity(limit.min(1024));
    for path in files.iter().rev() {
        let remaining = limit.saturating_sub(lines.len());
        if remaining == 0 {
            break;
        }
        let (mut file_lines, end_offset) = tail_file_lines_with_end(path, remaining)?;
        if let Some((_, offset)) = offsets.iter_mut().find(|(candidate, _)| candidate == path) {
            *offset = end_offset;
        }
        file_lines.extend(lines);
        lines = file_lines;
    }
    if lines.len() > limit {
        lines.drain(..lines.len() - limit);
    }
    Ok(DaemonLogSnapshot {
        lines,
        cursor: daemon_log_follow_cursor_for_files(&files, &offsets),
    })
}

pub(crate) fn recent_daemon_log_lines(paths: &GQYPaths, limit: usize) -> Result<Vec<String>> {
    Ok(recent_daemon_log_snapshot(paths, limit)?.lines)
}

pub(crate) fn tail_file_lines_with_end(path: &Path, limit: usize) -> Result<(Vec<String>, u64)> {
    pub(crate) const CHUNK: usize = 8192;
    let mut file = std::fs::File::open(path)?;
    let mut position = file.seek(SeekFrom::End(0))?;
    let end_offset = position;
    let mut bytes = Vec::new();
    let mut newline_count = 0usize;
    while position > 0 && newline_count <= limit {
        let read_len = usize::try_from(position.min(CHUNK as u64)).unwrap_or(CHUNK);
        position -= read_len as u64;
        file.seek(SeekFrom::Start(position))?;
        let mut chunk = vec![0_u8; read_len];
        file.read_exact(&mut chunk)?;
        newline_count += chunk.iter().filter(|byte| **byte == b'\n').count();
        chunk.extend(bytes);
        bytes = chunk;
    }
    let text = String::from_utf8_lossy(&bytes);
    let mut lines = text.lines().map(str::to_string).collect::<Vec<_>>();
    if lines.len() > limit {
        lines.drain(..lines.len() - limit);
    }
    Ok((lines, end_offset))
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct DaemonLogDelta {
    bytes: Vec<u8>,
    next_offset: u64,
    reset: bool,
}

pub(crate) fn read_daemon_log_delta(path: &Path, offset: u64) -> Result<DaemonLogDelta> {
    let mut file = std::fs::File::open(path)?;
    let len = file.metadata()?.len();
    let reset = len < offset;
    let start = if reset { 0 } else { offset };
    if len == start {
        return Ok(DaemonLogDelta {
            bytes: Vec::new(),
            next_offset: start,
            reset,
        });
    }
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::with_capacity(usize::try_from(len - start).unwrap_or(0));
    file.read_to_end(&mut bytes)?;
    Ok(DaemonLogDelta {
        bytes,
        next_offset: file.stream_position()?,
        reset,
    })
}

pub(crate) fn write_daemon_log_delta(
    path: &Path,
    offset: &mut u64,
    formatter: &mut DaemonLogStreamFormatter,
    ansi: bool,
    output: &mut impl Write,
) -> Result<bool> {
    let delta = read_daemon_log_delta(path, *offset)?;
    if delta.reset {
        formatter.finish(ansi, output)?;
    }
    *offset = delta.next_offset;
    if delta.bytes.is_empty() {
        return Ok(false);
    }
    formatter.push(&delta.bytes, ansi, output)?;
    Ok(true)
}

#[cfg(unix)]
pub(crate) fn daemon_process_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    let result = unsafe { libc::kill(pid as libc::pid_t, 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(unix))]
pub(crate) fn daemon_process_alive(_pid: u32) -> bool {
    // The IPC probe remains the authoritative check on platforms without a
    // portable process-existence primitive.
    true
}

pub(crate) fn finish_daemon_log_formatters(
    ansi: bool,
    current: Option<&PathBuf>,
    fallback: Option<&PathBuf>,
    formatter: &mut DaemonLogStreamFormatter,
    fallback_formatter: &mut DaemonLogStreamFormatter,
    output: &mut impl Write,
) -> io::Result<()> {
    if fallback == current {
        fallback_formatter.finish(ansi, output)?;
    } else {
        formatter.finish(ansi, output)?;
        fallback_formatter.finish(ansi, output)?;
    }
    output.flush()
}

pub(crate) async fn follow_daemon_log(
    paths: &GQYPaths,
    ansi: bool,
    cursor: DaemonLogFollowCursor,
    initial_pid: u32,
) -> Result<()> {
    let mut current = cursor.current;
    let mut offset = cursor.current_offset;
    let mut fallback = cursor.fallback;
    let mut fallback_offset = cursor.fallback_offset;
    let mut interval = tokio::time::interval(Duration::from_millis(500));
    let mut formatter = DaemonLogStreamFormatter::default();
    let mut fallback_formatter = DaemonLogStreamFormatter::default();
    let mut known_pid = Some(initial_pid);
    let mut daemon_misses = 0_u8;

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                let mut stdout = io::stdout().lock();
                finish_daemon_log_formatters(
                    ansi,
                    current.as_ref(),
                    fallback.as_ref(),
                    &mut formatter,
                    &mut fallback_formatter,
                    &mut stdout,
                )?;
                return Ok(());
            },
            _ = interval.tick() => {
                let files = daemon_log_files(paths)?;
                let latest = files
                    .iter()
                    .rev()
                    .find(|path| !is_daemon_fallback_log(path))
                    .cloned()
                    .or_else(|| files.iter().find(|path| is_daemon_fallback_log(path)).cloned());
                let latest_fallback = files.iter().find(|path| is_daemon_fallback_log(path)).cloned();
                if latest_fallback != fallback {
                    let mut stdout = io::stdout().lock();
                    if fallback.as_ref().is_some_and(|path| path.is_file()) {
                        write_daemon_log_delta(
                            fallback.as_ref().unwrap(),
                            &mut fallback_offset,
                            &mut fallback_formatter,
                            ansi,
                            &mut stdout,
                        )?;
                    }
                    fallback_formatter.finish(ansi, &mut stdout)?;
                    stdout.flush()?;
                    fallback = latest_fallback;
                    fallback_offset = 0;
                }
                if latest != current {
                    let mut stdout = io::stdout().lock();
                    if let Some(previous) = current
                        .as_ref()
                        .filter(|path| path.is_file() && Some(*path) != fallback.as_ref())
                    {
                        write_daemon_log_delta(
                            previous,
                            &mut offset,
                            &mut formatter,
                            ansi,
                            &mut stdout,
                        )?;
                    }
                    formatter.finish(ansi, &mut stdout)?;
                    stdout.flush()?;
                    current = latest;
                    offset = 0;
                }
                let mut changed = false;
                let mut stdout = io::stdout().lock();
                if let Some(path) = fallback.as_ref().filter(|path| path.is_file()) {
                    changed |= write_daemon_log_delta(
                        path,
                        &mut fallback_offset,
                        &mut fallback_formatter,
                        ansi,
                        &mut stdout,
                    )?;
                }
                if current.as_ref() != fallback.as_ref() {
                    if let Some(path) = current.as_ref().filter(|path| path.is_file()) {
                        changed |= write_daemon_log_delta(
                            path,
                            &mut offset,
                            &mut formatter,
                            ansi,
                            &mut stdout,
                        )?;
                    }
                }
                stdout.flush()?;
                drop(stdout);

                if changed {
                    daemon_misses = 0;
                    continue;
                }

                if let Some(info) = ipc::daemon_info(paths).await {
                    known_pid = Some(info.pid);
                    daemon_misses = 0;
                    continue;
                }

                // `daemon_info` deliberately has a short timeout. A busy
                // daemon can miss one or more probes while still being alive;
                // use the last known PID and a small grace window before
                // treating the stream as finished.
                let alive = known_pid.is_some_and(daemon_process_alive);
                let socket_exists = paths.ipc_socket().exists();
                if socket_exists && (alive || known_pid.is_none()) {
                    daemon_misses = 0;
                    continue;
                }
                daemon_misses = daemon_misses.saturating_add(1);
                if daemon_misses < 3 {
                    continue;
                }

                let mut stdout = io::stdout().lock();
                if let Some(path) = fallback.as_ref().filter(|path| path.is_file()) {
                    write_daemon_log_delta(
                        path,
                        &mut fallback_offset,
                        &mut fallback_formatter,
                        ansi,
                        &mut stdout,
                    )?;
                }
                if current.as_ref() != fallback.as_ref() {
                    if let Some(path) = current.as_ref().filter(|path| path.is_file()) {
                        write_daemon_log_delta(
                            path,
                            &mut offset,
                            &mut formatter,
                            ansi,
                            &mut stdout,
                        )?;
                    }
                }
                finish_daemon_log_formatters(
                    ansi,
                    current.as_ref(),
                    fallback.as_ref(),
                    &mut formatter,
                    &mut fallback_formatter,
                    &mut stdout,
                )?;
                return Ok(());
            }
        }
    }
}

pub(crate) async fn reload_daemon_if_running(paths: &GQYPaths) -> Result<()> {
    if ipc::daemon_info(paths).await.is_some() {
        retry_config_reload(RELOAD_MAX_ATTEMPTS, RELOAD_RETRY_INTERVAL, || {
            request_config_reload(paths)
        })
        .await
        .with_context(|| {
            t(
                "configuration was saved, but the running daemon did not reload it",
                "配置已保存，但正在运行的 daemon 未能重新加载配置",
            )
        })?;
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ConfigReloadResponse {
    Reloaded,
    Busy,
}

pub(crate) fn validate_config_reload_response(
    frame: Option<IpcFrame>,
) -> Result<ConfigReloadResponse> {
    if let Some(IpcFrame::Error { code, message }) = &frame {
        if *code == Some(ipc::ErrorCode::Busy)
            || (code.is_none() && message == ipc::ADMIN_BUSY_MESSAGE)
        {
            return Ok(ConfigReloadResponse::Busy);
        }
    }
    validate_ipc_command_response(frame)?;
    Ok(ConfigReloadResponse::Reloaded)
}

pub(crate) async fn request_config_reload(paths: &GQYPaths) -> Result<ConfigReloadResponse> {
    request_config_reload_at(&paths.ipc_socket(), RELOAD_RESPONSE_TIMEOUT).await
}

pub(crate) async fn request_config_reload_at(
    socket: &Path,
    response_timeout: Duration,
) -> Result<ConfigReloadResponse> {
    tokio::time::timeout(response_timeout, async {
        let mut stream = ipc::connect(socket).await?;
        ipc::send(&mut stream, &IpcRequest::new(IpcCommand::ReloadConfig)).await?;
        validate_config_reload_response(ipc::receive::<IpcFrame>(&mut stream).await?)
    })
    .await
    .with_context(|| {
        t(
            "timed out waiting for GQY daemon to reload configuration",
            "等待 GQY daemon 重新加载配置超时",
        )
    })?
}

pub(crate) async fn run_reload(paths: &GQYPaths) -> Result<()> {
    if ipc::daemon_info(paths).await.is_none() {
        bail!("{}", t("GQY daemon is not running", "GQY daemon 未运行"));
    }
    retry_config_reload(RELOAD_MAX_ATTEMPTS, RELOAD_RETRY_INTERVAL, || {
        request_config_reload(paths)
    })
    .await?;
    println!("{}", t("configuration reloaded", "配置已重新加载"));
    Ok(())
}

pub(crate) async fn retry_config_reload<F, Fut>(
    max_attempts: usize,
    retry_interval: Duration,
    mut request_reload: F,
) -> Result<()>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<ConfigReloadResponse>>,
{
    if max_attempts == 0 {
        bail!("reload must allow at least one attempt");
    }

    for attempt in 1..=max_attempts {
        match request_reload().await? {
            ConfigReloadResponse::Reloaded => return Ok(()),
            ConfigReloadResponse::Busy if attempt < max_attempts => {
                let seconds = retry_interval.as_secs();
                let message = if is_zh() {
                    format!(
                        "GQY daemon 正忙；将在 {seconds} 秒后重试配置重载（{attempt}/{max_attempts}）"
                    )
                } else {
                    format!(
                        "GQY daemon is busy; retrying configuration reload in {seconds} seconds ({attempt}/{max_attempts})"
                    )
                };
                eprintln!("{message}");
                tokio::time::sleep(retry_interval).await;
            }
            ConfigReloadResponse::Busy => {
                let message = if is_zh() {
                    format!("GQY daemon 在 {max_attempts} 次配置重载尝试后仍然忙碌")
                } else {
                    format!(
                        "GQY daemon remained busy after {max_attempts} configuration reload attempts"
                    )
                };
                bail!("{message}");
            }
        }
    }
    unreachable!("reload loop always returns on its final attempt")
}

pub(crate) async fn run_tool(paths: &GQYPaths, mode: AgentMode, args: ToolArgs) -> Result<()> {
    let config = AppConfig::load_or_default(paths)?;
    let registry = build_tool_registry(&config, paths, mode, false)?;
    let output = registry
        .call(&args.name, args.arguments.as_deref().unwrap_or("{}"))
        .await?;
    println!("{output}");
    Ok(())
}

/// 工具桥客户端(任务#12)。bash 就是编排层:脚本里循环/管道串结构化
/// 工具,中间数据本地流动、不经模型上下文往返;每次内层调用都以本回合的
/// 会话身份与来源在 daemon 侧过 guard/超时管线。daemon 不在(直连调试
/// 形态)则本地执行,语义一致但 jobs 等 daemon 态不可见。
pub(crate) async fn run_tool_call(paths: &GQYPaths, args: ToolCallArgs) -> Result<()> {
    let config = AppConfig::load_or_default(paths)?;
    let env_mode = std::env::var("GQY_TURN_MODE").unwrap_or_default();
    let mode = if env_mode == "dev" {
        AgentMode::Dev
    } else {
        AgentMode::Normal
    };
    if args.list || args.describe {
        let registry = build_tool_registry(&config, paths, mode, false)?;
        if args.list {
            let mut names = registry.tool_names();
            names.sort();
            for name in names {
                let display = registry.display_name(&name).unwrap_or_default();
                if display.is_empty() {
                    println!("{name}");
                } else {
                    println!("{name}\t{display}");
                }
            }
            return Ok(());
        }
        let Some(name) = args.name.as_deref() else {
            bail!("--describe 需要工具名");
        };
        let Some(spec) = registry.get(name) else {
            bail!("unknown tool: {name}");
        };
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "name": spec.name,
                "description": spec.description,
                "parameters": spec.parameters,
            }))?
        );
        return Ok(());
    }
    let Some(name) = args.name.clone() else {
        // 裸 `gqy tool-call` 是来问路的,给完整帮助而不是一行报错。
        localized_command()
            .find_subcommand_mut("tool-call")
            .expect("tool-call subcommand exists")
            .print_help()?;
        return Ok(());
    };
    let arguments = if args.args_stdin {
        let mut raw = String::new();
        use std::io::Read as _;
        io::stdin().read_to_string(&mut raw)?;
        raw
    } else if let Some(file) = &args.args_file {
        std::fs::read_to_string(file)?
    } else {
        args.arguments.clone().unwrap_or_else(|| "{}".to_string())
    };
    let session = std::env::var("GQY_SESSION").ok().filter(|s| !s.is_empty());
    let origin = std::env::var("GQY_TURN_ORIGIN")
        .ok()
        .filter(|s| !s.is_empty());
    let depth: u32 = std::env::var("GQY_BRIDGE_DEPTH")
        .ok()
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(0);

    if ipc::daemon_info(paths).await.is_some() {
        let (_, data) = send_ipc_admin(
            paths,
            IpcCommand::ToolCall {
                session,
                name,
                arguments,
                origin,
                depth,
            },
        )
        .await?;
        let output = data
            .get("output")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        println!("{output}");
        return Ok(());
    }

    // 直连回退:本地建 registry(guard/超时同源),会话与来源按环境作用域化。
    // jobs 等 daemon 内存态在本地进程不可见,直连调试形态可接受。
    if depth >= crate::tools::workspace::MAX_BRIDGE_DEPTH {
        bail!("tool bridge recursion limit reached (depth {depth})");
    }
    let registry = build_tool_registry(&config, paths, mode, false)?;
    let turn_origin: crate::tools::workspace::TurnOrigin = origin
        .as_deref()
        .and_then(|raw| serde_json::from_str(raw).ok())
        .unwrap_or(crate::tools::workspace::TurnOrigin::Human);
    let invoke = crate::tools::workspace::with_turn_origin(
        turn_origin,
        crate::tools::workspace::with_bridge_depth(depth + 1, async {
            registry.call(&name, &arguments).await
        }),
    );
    let output = match session {
        Some(session) => {
            let session: std::sync::Arc<str> = session.into();
            crate::tools::workspace::with_session(session, invoke).await?
        }
        None => invoke.await?,
    };
    println!("{output}");
    Ok(())
}
