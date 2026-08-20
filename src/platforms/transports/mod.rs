//! 第三方通信平台传输层抽象。
//!
//! 统一第三方平台的运行生命周期与消息收发接口。daemon 通过
//! [`PlatformTransportRegistry`] 统一托管所有已配置平台；平台自身的
//! 协议细节（WebSocket 连接、事件解析、发送编码）保留在各平台的
//! 实现模块里（OneBot 实现在 `transports::onebot`）。
//!
//! 注册表按「平台 id」索引，每个条目是一个 [`PlatformTransport`] trait
//! 对象。生命周期由 daemon 通过 `start/stop` 驱动；配置热重载通过
//! [`prepare_platform_configs`] 统一校验后提交。`qq_official` /
//! `telegram` / `wechat` / `slack` 等平台 id 已预留为扩展位。

pub(crate) mod onebot;
pub(crate) use onebot::*;

use crate::config::{AppConfig, PlatformTransportConfig, PlatformsConfig};
use crate::platforms::onebot::onebot_config;
use crate::web::DaemonState;
use crate::{ipc, web};

use anyhow::Result;
use futures_util::future::BoxFuture;
use serde::Serialize;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

pub(crate) const PLATFORM_ID_ONEBOT: &str = "qq";
/// 预留平台扩展位：后续平台（QQ 官方 / Telegram / 微信 / Slack…）
/// 在这里追加 id，并在注册表与配置归一化中登记。
pub(crate) const RESERVED_PLATFORM_IDS: &[&str] = &[
    "qq",
    "qq_official",
    "telegram",
    "wechat",
    "slack",
];

/// 传输层消息目标：发送前解析到具体账号与会话。
#[derive(Clone, Debug)]
pub(crate) enum TransportTarget {
    Private { account_id: i64, user_id: i64 },
    Group { account_id: i64, group_id: i64 },
}

/// 传输层收发消息的统一载体。
#[derive(Clone, Debug)]
pub(crate) struct TransportMessage {
    pub(crate) target: TransportTarget,
    pub(crate) body: crate::platforms::OutboundBody,
    pub(crate) response_target: Option<crate::platforms::ResponseTarget>,
}

/// 第三方平台传输 trait：start/stop 管理运行生命周期，send/receive
/// 管理消息收发。
pub(crate) trait PlatformTransport: Send + Sync {
    fn id(&self) -> &'static str;

    fn label(&self) -> &'static str;

    fn transport(&self) -> &'static str;

    /// 启动平台（监听、连接、注册消息入口）。
    fn start<'a>(&'a self, state: &'a DaemonState) -> BoxFuture<'a, Result<()>>;

    /// 停止平台（断开连接、关闭监听）。
    fn stop<'a>(&'a self, state: &'a DaemonState) -> BoxFuture<'a, Result<()>>;

    /// 平台运行状态快照。
    fn status<'a>(&'a self, state: &'a DaemonState) -> BoxFuture<'a, Result<PlatformRuntimeStatus>>;

    /// 发送一条消息。实现方按 `target` 解析连接与会话。
    fn send<'a>(
        &'a self,
        _state: &'a DaemonState,
        _message: &'a TransportMessage,
    ) -> BoxFuture<'a, Result<crate::platforms::SendReceipt>>;

    /// 接收（投递）一条入站事件。推送型平台（如 OneBot）的入站由连接
    /// 循环直接驱动，此钩子保持为空实现；轮询型平台在此统一入口。
    fn receive<'a>(
        &'a self,
        _state: &'a DaemonState,
        _event: crate::platforms::PlatformInboundEvent,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }
}

/// 平台运行时快照，供 CLI / WebUI 状态展示。字段名保持稳定。
#[derive(Clone, Debug, Serialize)]
pub(crate) struct PlatformRuntimeStatus {
    pub(crate) id: String,
    pub(crate) label: String,
    pub(crate) transport: String,
    pub(crate) enabled: bool,
    pub(crate) running: bool,
    pub(crate) listen_port: Option<u16>,
    pub(crate) connected_accounts: Vec<i64>,
}

/// 平台传输统一注册表。daemon 持有唯一实例；平台状态、配置热重载与
/// WebUI 路由都从这里取。
#[derive(Clone, Default)]
pub(crate) struct PlatformTransportRegistry {
    pub(crate) inner: Arc<Mutex<BTreeMap<&'static str, Arc<dyn PlatformTransport>>>>,
}

impl PlatformTransportRegistry {
    pub(crate) fn new() -> Self {
        let mut entries: BTreeMap<&'static str, Arc<dyn PlatformTransport>> = BTreeMap::new();
        entries.insert(PLATFORM_ID_ONEBOT, Arc::new(OneBotTransport));
        // 预留扩展位：后续平台在此注册实现，例如
        // entries.insert("telegram", Arc::new(TelegramTransport));
        Self {
            inner: Arc::new(Mutex::new(entries)),
        }
    }

    pub(crate) fn get(&self, id: &str) -> Option<Arc<dyn PlatformTransport>> {
        self.inner.lock().unwrap().get(id).cloned()
    }

    pub(crate) fn all(&self) -> Vec<Arc<dyn PlatformTransport>> {
        self.inner
            .lock()
            .unwrap()
            .values()
            .cloned()
            .collect::<Vec<_>>()
    }

    pub(crate) fn ids(&self) -> Vec<&'static str> {
        self.inner.lock().unwrap().keys().copied().collect()
    }
}

/// 所有已注册平台的状态快照（CLI / WebUI 共用）。
pub(crate) async fn runtime_statuses(
    state: &DaemonState,
    config: Option<&PlatformsConfig>,
) -> Vec<PlatformRuntimeStatus> {
    let mut statuses = Vec::new();
    for transport in state.platforms.transports.all() {
        let Some(mut status) = transport.status(state).await.ok() else {
            continue;
        };
        if let Some(config) = config {
            status.enabled = platform_enabled(config, transport.id());
        }
        statuses.push(status);
    }
    statuses
}

/// 判断第三方平台是否启用（按平台 id）。未知平台视为未启用。
pub(crate) fn platform_enabled(config: &PlatformsConfig, id: &str) -> bool {
    match id {
        PLATFORM_ID_ONEBOT => config.qq.enabled,
        _ => config
            .transports
            .get(id)
            .map(|entry| entry.enabled)
            .unwrap_or(false),
    }
}

/// 平台 id → 配置段默认值（CLI 展示平台与配置映射时使用）。
pub(crate) fn platform_config_section(
    config: &AppConfig,
    id: &str,
) -> Option<PlatformTransportConfig> {
    match id {
        PLATFORM_ID_ONEBOT => Some(PlatformTransportConfig {
            enabled: config.platforms.qq.enabled,
            port: Some(config.platforms.qq.reverse_ws_port),
        }),
        _ => config.platforms.transports.get(id).cloned(),
    }
}

/// 本地 daemon 是否在运行（供 CLI 平台命令判断是否走 IPC）。
pub(crate) async fn daemon_running(paths: &crate::paths::GQYPaths) -> bool {
    ipc::daemon_info(paths).await.is_some()
}

/// 平台 id 是否在预留集合中（CLI `platform list` 等展示）。
pub(crate) fn is_known_platform(id: &str) -> bool {
    RESERVED_PLATFORM_IDS.contains(&id)
}

/// 从配置与运行状态生成单平台状态；未知平台返回 None。
pub(crate) async fn status_for(
    state: &DaemonState,
    config: &PlatformsConfig,
    id: &str,
) -> Option<PlatformRuntimeStatus> {
    let transport = state.platforms.transports.get(id)?;
    let mut status = transport.status(state).await.ok()?;
    status.enabled = platform_enabled(config, id);
    Some(status)
}

/// 单个平台的已校验配置变更；调用方在 actor 成功应用配置后
/// `commit()` 才会真正生效（与 QQ listener 的事务语义保持一致）。
pub(crate) struct PreparedPlatform {
    pub(crate) id: String,
    pub(crate) prepared: crate::platforms::onebot::PreparedQqListener,
}

impl PreparedPlatform {
    pub(crate) fn commit(self) {
        self.prepared.commit();
    }
}

/// 为所有已注册平台准备配置变更（先校验，不提交）。当前实现把
/// OneBot 的 reverse-WS 监听差异在此统一校验；后续平台在
/// `match id` 中追加各自的 prepare。
pub(crate) async fn prepare_platform_configs(
    state: &DaemonState,
    previous: Option<&PlatformsConfig>,
    next: &PlatformsConfig,
) -> Result<Vec<PreparedPlatform>> {
    let previous_qq = previous.map(|config| config.qq.clone());
    let qq = state
        .platforms
        .qq_listener
        .prepare(state, previous_qq.as_ref(), &next.qq)
        .await
        .map_err(|error| {
            anyhow::anyhow!(
                "Tencent QQ listener configuration failed: {}",
                web::safe_error_message(error)
            )
        })?;
    Ok(vec![PreparedPlatform {
        id: PLATFORM_ID_ONEBOT.to_string(),
        prepared: qq,
    }])
}

/// 启动所有已注册且已启用的平台传输。daemon 启动路径调用；每个平台
/// 的 `start` 内部自行判断配置是否启用。
pub(crate) async fn start_platform_transports(state: &DaemonState) -> Result<()> {
    for transport in state.platforms.transports.all() {
        transport.start(state).await?;
    }
    Ok(())
}

/// 关闭所有第三方平台传输（daemon 退出路径）。
pub(crate) async fn shutdown_platform_transports(state: &DaemonState) {
    for transport in state.platforms.transports.all() {
        let _ = transport.stop(state).await;
    }
}

/// 便捷函数：当前 OneBot 配置的启用状态（供 CLI/WebUI 快速判断）。
pub(crate) fn onebot_enabled(state: &DaemonState) -> bool {
    onebot_config(state).enabled
}
