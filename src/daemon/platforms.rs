//! daemon 侧第三方平台生命周期托管。
//!
//! `transports` 模块定义平台传输抽象；这里提供 daemon 视角的统一管理
//! 入口：启动全部已启用平台、关闭全部平台、查询单平台状态、重启单
//! 平台。WebUI/IPC 的配置热重载走 `transports::prepare_platform_configs`，
//! 进程启动/退出则调用本模块。

use crate::platforms::transports::{
    platform_enabled, PlatformRuntimeStatus, PlatformTransportRegistry,
};
use crate::web::DaemonState;
use anyhow::Result;

/// 启动所有已注册且已启用的第三方平台传输（daemon 启动路径）。
pub(crate) async fn start_all(state: &DaemonState) -> Result<()> {
    for transport in state.platforms.transports.all() {
        transport.start(state).await?;
    }
    Ok(())
}

/// 关闭所有第三方平台传输（daemon 退出路径）。逐个容错，单个平台
/// 停止失败不影响其他平台收尾。
pub(crate) async fn shutdown_all(state: &DaemonState) {
    for transport in state.platforms.transports.all() {
        let _ = transport.stop(state).await;
    }
}

/// 查询单平台状态；未注册的平台返回 None。
pub(crate) async fn status(
    state: &DaemonState,
    registry: &PlatformTransportRegistry,
    id: &str,
) -> Option<PlatformRuntimeStatus> {
    let transport = registry.get(id)?;
    let mut status = transport.status(state).await.ok()?;
    let config = state.manager.lock().unwrap().config.platforms.clone();
    status.enabled = platform_enabled(&config, id);
    Some(status)
}

/// 重启单平台（先停后起）；未注册或启动失败返回错误。
pub(crate) async fn restart(state: &DaemonState, id: &str) -> Result<PlatformRuntimeStatus> {
    let transport = state
        .platforms
        .transports
        .get(id)
        .ok_or_else(|| anyhow::anyhow!("unknown platform: {id}"))?;
    let _ = transport.stop(state).await;
    transport.start(state).await?;
    let config = state.manager.lock().unwrap().config.platforms.clone();
    let mut status = transport.status(state).await?;
    status.enabled = platform_enabled(&config, id);
    Ok(status)
}
