//! OneBot v11（NapCat / QQ）传输实现。

pub(crate) use super::*;

use crate::platforms::onebot::{onebot_config, Target};
use crate::web::DaemonState;

use anyhow::Result;

/// OneBot v11 传输实现。生命周期托管 `QqListenerManager`（reverse-WS
/// 监听）与 `ConnectionRegistry`（NapCat 连接注册）；消息收发复用
/// `onebot` 模块已有的适配器编解码。
#[derive(Clone, Default)]
pub(crate) struct OneBotTransport;

impl PlatformTransport for OneBotTransport {
    fn id(&self) -> &'static str {
        PLATFORM_ID_ONEBOT
    }

    fn label(&self) -> &'static str {
        "QQ"
    }

    fn transport(&self) -> &'static str {
        "onebot-v11"
    }

    fn start<'a>(&'a self, state: &'a DaemonState) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let config = onebot_config(state);
            if config.enabled {
                state
                    .platforms
                    .qq_listener
                    .prepare(state, None, &config)
                    .await?
                    .commit();
            }
            Ok(())
        })
    }

    fn stop<'a>(&'a self, state: &'a DaemonState) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            state.platforms.qq_listener.shutdown(state).await;
            Ok(())
        })
    }

    fn status<'a>(&'a self, state: &'a DaemonState) -> BoxFuture<'a, Result<PlatformRuntimeStatus>> {
        Box::pin(async move {
            let config = onebot_config(state);
            let port = state.platforms.qq_listener.active_port();
            let accounts = state
                .platforms
                .onebot
                .lock()
                .unwrap()
                .connected_accounts();
            Ok(PlatformRuntimeStatus {
                id: PLATFORM_ID_ONEBOT.to_string(),
                label: "QQ".to_string(),
                transport: "onebot-v11".to_string(),
                enabled: config.enabled,
                running: config.enabled && port.is_some(),
                listen_port: port,
                connected_accounts: accounts,
            })
        })
    }

    fn send<'a>(
        &'a self,
        state: &'a DaemonState,
        message: &'a TransportMessage,
    ) -> BoxFuture<'a, Result<crate::platforms::SendReceipt>> {
        Box::pin(async move {
            let (account_id, target) = match &message.target {
                TransportTarget::Private { account_id, user_id } => {
                    (*account_id, Target::Private { user_id: *user_id })
                }
                TransportTarget::Group { account_id, group_id } => {
                    (*account_id, Target::Group { group_id: *group_id })
                }
            };
            let Some(handle) = state
                .platforms
                .onebot
                .lock()
                .unwrap()
                .handle(account_id)
            else {
                anyhow::bail!("the QQ account is not connected");
            };
            let adapter = crate::platforms::onebot::OneBotAdapter {
                conn: handle,
                registry: state.platforms.onebot.clone(),
                http: state.platforms.http_client()?,
                self_id: account_id,
                target,
                max_reply_chars: onebot_config(state).max_reply_chars,
            };
            adapter
                .send_message(crate::platforms::OutboundMessage {
                    body: message.body.clone(),
                    response_target: message.response_target.clone(),
                    origin: crate::platforms::OutboundOrigin::Command,
                    metadata: std::collections::BTreeMap::new(),
                })
                .await
        })
    }
}
