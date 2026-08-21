//! daemon — GQY 统一后台服务。
//!
//! 一个二进制同时承载：终端 REPL、内置 WebUI 与第三方通信平台（当前
//! QQ/OneBot）。本模块负责 daemon 进程的入口与第三方平台的生命周期
//! 托管——平台传输在 `crate::platforms::transports` 抽象，具体启动/
//! 停止/热重载在这里驱动。

mod platforms;
pub(crate) use platforms::*;

use crate::cli::WebArgs;
use crate::paths::GQYPaths;
use anyhow::Result;

/// Unified background host for IPC, WebUI and configured platform transports.
/// Transport-specific HTTP handlers remain in `web`; lifecycle ownership lives
/// here so future entrypoints do not acquire a second process model.
pub(crate) async fn run(paths: GQYPaths, web: WebArgs) -> Result<()> {
    crate::web::run(paths, web).await
}
