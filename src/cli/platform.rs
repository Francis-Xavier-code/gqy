//! 第三方通信平台 CLI（`gqy platform …`）。
//!
//! 提供平台状态查看、启用/禁用、重启与列表。启用/禁用直接改写
//! `platforms` 配置段；daemon 运行时自动热重载，未运行则写配置即可。
//! 重启走 IPC（要求 daemon 运行中）。

pub(crate) use super::*;

use crate::platforms::transports::{
    daemon_running, is_known_platform, platform_config_section,
};
use anyhow::{bail, Result};

/// `gqy platform` 顶层入口。
pub(crate) async fn run_platform(paths: &GQYPaths, args: PlatformArgs) -> Result<()> {
    match args.command {
        PlatformCommand::Status => run_platform_status(paths, None).await,
        PlatformCommand::List => run_platform_list(paths).await,
        PlatformCommand::Show(args) => run_platform_status(paths, Some(&args.name)).await,
        PlatformCommand::Enable(args) => run_platform_set_enabled(paths, &args.name, true).await,
        PlatformCommand::Disable(args) => run_platform_set_enabled(paths, &args.name, false).await,
        PlatformCommand::Restart(args) => run_platform_restart(paths, &args.name).await,
    }
}

fn platform_status_lines(status: &crate::platforms::transports::PlatformRuntimeStatus) -> Vec<String> {
    let mut lines = Vec::new();
    lines.push(format!(
        "{} ({}) — {}",
        status.label,
        status.transport,
        t("third-party communication platform", "第三方通信平台")
    ));
    lines.push(format!(
        "{}: {}",
        t("id", "id"),
        status.id
    ));
    lines.push(format!(
        "{}: {}",
        t("enabled", "启用"),
        if status.enabled {
            t("yes", "是")
        } else {
            t("no", "否")
        }
    ));
    lines.push(format!(
        "{}: {}",
        t("running", "运行中"),
        if status.running {
            t("yes", "是")
        } else {
            t("no", "否")
        }
    ));
    if let Some(port) = status.listen_port {
        lines.push(format!(
            "{}: ws://localhost:{port}/ws",
            t("listen", "监听")
        ));
    }
    if status.connected_accounts.is_empty() {
        lines.push(format!(
            "{}: {}",
            t("connected accounts", "已连接账号"),
            t("none", "无")
        ));
    } else {
        let accounts = status
            .connected_accounts
            .iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        lines.push(format!(
            "{}: {accounts}",
            t("connected accounts", "已连接账号")
        ));
    }
    lines
}

fn print_platform_status(status: &crate::platforms::transports::PlatformRuntimeStatus) {
    for line in platform_status_lines(status) {
        println!("{line}");
    }
}

async fn run_platform_status(paths: &GQYPaths, name: Option<&str>) -> Result<()> {
    let config = AppConfig::load_or_default(paths)?;
    let running = daemon_running(paths).await;
    let name = name.map(str::trim).filter(|name| !name.is_empty());
    match name {
        Some(name) => {
            if !is_known_platform(name) {
                bail!(
                    "{}: {name}",
                    t("unknown platform (expected qq)", "未知平台（当前支持 qq）")
                );
            }
            if running {
                let (_, data) = send_ipc_admin(paths, IpcCommand::GetStatus).await?;
                let statuses = data
                    .pointer("/transports")
                    .and_then(serde_json::Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let found = statuses
                    .into_iter()
                    .find(|value| value.get("id").and_then(serde_json::Value::as_str) == Some(name));
                match found {
                    Some(value) => {
                        let status = serde_json::from_value::<
                            crate::platforms::transports::PlatformRuntimeStatus,
                        >(value)
                        .unwrap_or_else(|_| crate::platforms::transports::PlatformRuntimeStatus {
                            id: name.to_string(),
                            label: name.to_string(),
                            transport: "unknown".to_string(),
                            enabled: false,
                            running: false,
                            listen_port: None,
                            connected_accounts: Vec::new(),
                        });
                        print_platform_status(&status);
                    }
                    None => {
                        bail!(
                            "{}: {name}",
                            t("platform is not registered", "平台未注册")
                        );
                    }
                }
            } else {
                let Some(section) = platform_config_section(&config, name) else {
                    bail!(
                        "{}: {name}",
                        t("platform is not registered", "平台未注册")
                    );
                };
                println!(
                    "{} ({}) — {}",
                    name,
                    "onebot-v11",
                    t("third-party communication platform", "第三方通信平台")
                );
                println!(
                    "{}: {}",
                    t("enabled", "启用"),
                    if section.enabled {
                        t("yes", "是")
                    } else {
                        t("no", "否")
                    }
                );
                if let Some(port) = section.port {
                    println!("{}: ws://localhost:{port}/ws", t("listen", "监听"));
                }
                println!(
                    "{}",
                    t(
                        "daemon is not running; start `gqy daemon start` to begin listening",
                        "daemon 未运行；执行 `gqy daemon start` 后开始监听。"
                    )
                );
            }
        }
        None => {
            if running {
                let (_, data) = send_ipc_admin(paths, IpcCommand::GetStatus).await?;
                let statuses: Vec<crate::platforms::transports::PlatformRuntimeStatus> =
                    data.pointer("/transports")
                        .and_then(serde_json::Value::as_array)
                        .map(|values| {
                            values
                                .iter()
                                .filter_map(|value| {
                                    serde_json::from_value(value.clone()).ok()
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                if statuses.is_empty() {
                    println!(
                        "{}",
                        t("no third-party communication platforms are registered", "没有已注册的第三方通信平台")
                    );
                } else {
                    for status in &statuses {
                        print_platform_status(status);
                        println!();
                    }
                }
            } else {
                for status in local_platform_statuses(&config) {
                    print_platform_status(&status);
                    println!();
                }
            }
        }
    }
    Ok(())
}

/// daemon 未运行时的本地平台状态（仅配置层面）。
fn local_platform_statuses(
    config: &AppConfig,
) -> Vec<crate::platforms::transports::PlatformRuntimeStatus> {
    let mut statuses = Vec::new();
    if let Some(section) = platform_config_section(config, "qq") {
        statuses.push(crate::platforms::transports::PlatformRuntimeStatus {
            id: "qq".to_string(),
            label: "QQ".to_string(),
            transport: "onebot-v11".to_string(),
            enabled: section.enabled,
            running: false,
            listen_port: section.port,
            connected_accounts: Vec::new(),
        });
    }
    // 预留扩展位：后续平台在此追加本地状态。
    statuses
}

async fn run_platform_list(paths: &GQYPaths) -> Result<()> {
    let config = AppConfig::load_or_default(paths)?;
    if daemon_running(paths).await {
        let (_, data) = send_ipc_admin(paths, IpcCommand::GetStatus).await?;
        let statuses: Vec<crate::platforms::transports::PlatformRuntimeStatus> = data
            .pointer("/transports")
            .and_then(serde_json::Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(|value| serde_json::from_value(value.clone()).ok())
                    .collect()
            })
            .unwrap_or_default();
        for status in statuses {
            println!(
                "{}\t{}\t{}\t{}",
                status.id,
                status.label,
                if status.enabled { "enabled" } else { "disabled" },
                if status.running { "running" } else { "stopped" },
            );
        }
    } else {
        for status in local_platform_statuses(&config) {
            println!(
                "{}\t{}\t{}\t{}",
                status.id,
                status.label,
                if status.enabled { "enabled" } else { "disabled" },
                "stopped",
            );
        }
    }
    Ok(())
}

async fn run_platform_set_enabled(paths: &GQYPaths, name: &str, enabled: bool) -> Result<()> {
    let name = name.trim();
    if !is_known_platform(name) {
        bail!(
            "{}: {name}",
            t("unknown platform (expected qq)", "未知平台（当前支持 qq）")
        );
    }
    let mut config = AppConfig::load_or_default(paths)?;
    match name {
        "qq" => {
            config.platforms.qq.enabled = enabled;
        }
        _ => {
            let entry = config
                .platforms
                .transports
                .entry(name.to_string())
                .or_default();
            entry.enabled = enabled;
        }
    }
    config.save(paths)?;
    println!(
        "{} {name}",
        if enabled {
            t("enabled platform", "已启用平台")
        } else {
            t("disabled platform", "已禁用平台")
        }
    );
    if enabled {
        println!(
            "{}",
            t(
                "run `gqy daemon restart` or `gqy platform restart qq` to begin listening",
                "执行 `gqy daemon restart` 或 `gqy platform restart qq` 后开始监听。"
            )
        );
    }
    // daemon 运行中时触发热重载，使配置立即生效。
    reload_daemon_if_running(paths).await
}

async fn run_platform_restart(paths: &GQYPaths, name: &str) -> Result<()> {
    let name = name.trim();
    if !is_known_platform(name) {
        bail!(
            "{}: {name}",
            t("unknown platform (expected qq)", "未知平台（当前支持 qq）")
        );
    }
    if !daemon_running(paths).await {
        bail!(
            "{}",
            t(
                "GQY daemon is not running; start it with `gqy daemon start` first",
                "GQY daemon 未运行；请先执行 `gqy daemon start`。"
            )
        );
    }
    let (_, data) = send_ipc_admin(paths, IpcCommand::RestartPlatform { id: name.to_string() }).await?;
    let status = data
        .pointer("/platform")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or(crate::platforms::transports::PlatformRuntimeStatus {
            id: name.to_string(),
            label: name.to_string(),
            transport: "unknown".to_string(),
            enabled: false,
            running: false,
            listen_port: None,
            connected_accounts: Vec::new(),
        });
    println!(
        "{} {name}",
        t("restarted platform", "已重启平台")
    );
    print_platform_status(&status);
    Ok(())
}
