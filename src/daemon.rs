use crate::cli::WebArgs;
use crate::paths::GQYPaths;
use anyhow::Result;

/// Unified background host for IPC, WebUI and configured platform transports.
/// Transport-specific HTTP handlers remain in `web`; lifecycle ownership lives
/// here so future entrypoints do not acquire a second process model.
pub(crate) async fn run(paths: GQYPaths, web: WebArgs) -> Result<()> {
    crate::web::run(paths, web).await
}
