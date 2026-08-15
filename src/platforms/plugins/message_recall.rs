use super::{require_ai_confirmation, PlatformPlugin, PlatformTurnInput, PluginDescriptor};
use crate::config::QqMessageRecallPluginSettings;
use crate::platforms::{
    ConversationKind, OutboundMessage, PlatformInboundEvent, PlatformInboundEventKind,
    PlatformMessageInfo, PlatformTurnContext, SendReceipt,
};
use crate::tools::{ToolRegistry, ToolSpec};
use anyhow::{bail, Context, Result};
use futures_util::future::BoxFuture;
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::watch;

pub(crate) const MESSAGE_RECALL_PLUGIN_ID: &str = "qq_message_recall";
const MAX_SCOPES: usize = 512;

#[derive(Default)]
struct ScopeState {
    sent: VecDeque<String>,
    recalled: HashMap<String, Instant>,
    pending: HashMap<String, watch::Sender<bool>>,
    touched_at: Option<Instant>,
}

#[derive(Default)]
struct RecallState {
    scopes: HashMap<String, ScopeState>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TargetSource {
    Argument,
    Reply,
}

impl TargetSource {
    fn as_str(self) -> &'static str {
        match self {
            Self::Argument => "argument",
            Self::Reply => "reply",
        }
    }
}

struct RecallTarget {
    message_id: String,
    source: TargetSource,
}

pub(crate) struct MessageRecallPlugin {
    state: Arc<Mutex<RecallState>>,
}

impl MessageRecallPlugin {
    pub(crate) fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(RecallState::default())),
        }
    }

    fn prune(state: &mut RecallState, now: Instant, ttl: Duration) {
        state.scopes.retain(|_, scope| {
            scope.recalled.retain(|_, at| now.duration_since(*at) < ttl);
            scope
                .touched_at
                .is_some_and(|at| now.duration_since(at) < ttl)
                || !scope.pending.is_empty()
        });
        while state.scopes.len() > MAX_SCOPES {
            let Some(oldest) = state
                .scopes
                .iter()
                .filter(|(_, scope)| scope.pending.is_empty())
                .min_by_key(|(_, scope)| scope.touched_at)
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            state.scopes.remove(&oldest);
        }
    }

    fn recalled(&self, context: &PlatformTurnContext) -> bool {
        let Some(message_id) = context
            .inbound_event()
            .map(|event| event.message_id.as_str())
        else {
            return false;
        };
        let settings = recall_settings(context).unwrap_or_default();
        let mut state = self.state.lock().unwrap();
        Self::prune(
            &mut state,
            Instant::now(),
            Duration::from_secs(settings.cancel_record_ttl_seconds),
        );
        state
            .scopes
            .get(&context.conversation.scope_key())
            .is_some_and(|scope| scope.recalled.contains_key(message_id))
    }

    fn belongs(context: &PlatformTurnContext, info: &PlatformMessageInfo) -> bool {
        match (info.conversation_kind, info.conversation_id.as_deref()) {
            (Some(kind), Some(id)) => {
                kind == context.conversation.kind && id == context.conversation.conversation_id
            }
            _ => false,
        }
    }

    async fn withdraw(&self, context: Arc<PlatformTurnContext>, args: Value) -> Result<String> {
        let target = match select_target(explicit_id(&args)?, reply_id(&context)) {
            Ok(target) => target,
            Err(error) => {
                return failure_response(
                    "target_required",
                    false,
                    "无法确定目标消息。请回复需要撤回的消息后重试。",
                    json!({ "detail": error.to_string() }),
                );
            }
        };
        let id = &target.message_id;
        let settings = recall_settings(&context)?;
        let reason = reason(&args, settings.max_reason_length)?;
        let info = match replied_info(&context, id) {
            Some(info) => info.clone(),
            None => match context.message_info(id).await {
                Ok(Some(info)) => info,
                Ok(None) => {
                    return failure_response(
                        "message_not_found",
                        false,
                        "目标消息不存在或已无法查询，请不要改撤其他消息。",
                        json!({ "message_id": id, "target_source": target.source.as_str() }),
                    );
                }
                Err(error) => {
                    return failure_response(
                        "message_lookup_failed",
                        false,
                        "无法核验目标消息，未执行撤回，请不要改撤其他消息。",
                        json!({
                            "message_id": id,
                            "target_source": target.source.as_str(),
                            "detail": error.to_string()
                        }),
                    );
                }
            },
        };
        if !Self::belongs(&context, &info) {
            return failure_response(
                "wrong_conversation",
                false,
                "目标消息不属于当前会话",
                json!({ "message_id": id, "target_source": target.source.as_str() }),
            );
        }
        let own_message = info.sender_id == context.conversation.account_id;
        if !own_message {
            if context.conversation.kind != ConversationKind::Group {
                return failure_response(
                    "permission_denied",
                    false,
                    "私聊中只能撤回 Miyu 自己发送的消息",
                    json!({ "message_id": id }),
                );
            }
            if !context.bot_group_role().await.can_manage() {
                return failure_response(
                    "permission_denied",
                    false,
                    "Miyu 不是当前群的管理员，无法撤回群友消息",
                    json!({ "message_id": id }),
                );
            }
            if let Some(prompt) = require_ai_confirmation(
                &context,
                "qq_withdraw_message",
                &json!({
                    "message_id": id,
                    "reason": reason.clone(),
                    "target_source": target.source.as_str(),
                }),
            )
            .await?
            {
                return Ok(prompt);
            }
        }
        if let Err(error) = context.delete_message(id).await {
            tracing::warn!(
                target: "miyu::qq",
                error = %error,
                message_id = %id,
                target_source = target.source.as_str(),
                conversation = %context.conversation.scope_key(),
                "{}",
                crate::i18n::text("QQ message recall failed", "QQ 消息撤回失败")
            );
            return recall_failure_response(
                &error,
                id,
                target.source,
                if own_message { "miyu" } else { "group_member" },
            );
        }
        if own_message {
            if let Some(scope) = self
                .state
                .lock()
                .unwrap()
                .scopes
                .get_mut(&context.conversation.scope_key())
            {
                scope.sent.retain(|old| old != id);
            }
        }
        response(
            true,
            "消息已撤回",
            json!({
                "message_id": id,
                "sender_id": info.sender_id,
                "target_kind": if own_message { "miyu" } else { "group_member" },
                "reason": reason,
                "target_source": target.source.as_str()
            }),
        )
    }
}

impl PlatformPlugin for MessageRecallPlugin {
    fn descriptor(&self) -> PluginDescriptor {
        PluginDescriptor {
            id: MESSAGE_RECALL_PLUGIN_ID,
            priority: 190,
            default_enabled: true,
        }
    }

    fn register_tools(
        &self,
        registry: &mut ToolRegistry,
        context: Arc<PlatformTurnContext>,
    ) -> Result<()> {
        let settings = recall_settings(&context)?;
        if !settings.enable_tool {
            return Ok(());
        }
        let plugin = self.clone();
        registry.register(
            ToolSpec::new(
                "qq_withdraw_message",
                "Recall exactly one QQ message in the current conversation. If the current user message replies to a target, omit message_id and the trusted reply target is used. Without a reply, message_id is required. Never guess a recent message and never retry with another target after a failure.",
                schema(),
                move |args| {
                    let plugin = plugin.clone();
                    let context = context.clone();
                    async move { plugin.withdraw(context, args).await }
                },
            )
            .writes()
            .with_display_name("撤回 QQ 消息"),
        );
        Ok(())
    }

    fn before_turn<'a>(
        &'a self,
        context: &'a PlatformTurnContext,
        input: &'a mut PlatformTurnInput,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let settings = recall_settings(context)?;
            if !settings.enable_tool {
                return Ok(());
            }
            input.system_context.push(
                "QQ 撤回规则：使用 qq_withdraw_message。当前消息有引用目标时省略 message_id，系统会采用可信引用；没有引用时必须提供明确的 message_id。“这条/那条消息”本身不能确定目标，必须请用户回复目标消息。工具失败后不得改撤其他消息，也不得声称撤回成功。"
                    .to_string(),
            );
            Ok(())
        })
    }

    fn turn_started(&self, context: &PlatformTurnContext, cancel: watch::Sender<bool>) {
        let Some(message_id) = context
            .inbound_event()
            .map(|event| event.message_id.clone())
            .filter(|id| !id.is_empty())
        else {
            return;
        };
        let now = Instant::now();
        let settings = recall_settings(context).unwrap_or_default();
        let mut state = self.state.lock().unwrap();
        Self::prune(
            &mut state,
            now,
            Duration::from_secs(settings.cancel_record_ttl_seconds),
        );
        let scope = state
            .scopes
            .entry(context.conversation.scope_key())
            .or_default();
        scope.touched_at = Some(now);
        if scope.recalled.contains_key(&message_id) {
            cancel.send_replace(true);
        } else {
            scope.pending.insert(message_id, cancel);
        }
    }

    fn observe_inbound<'a>(
        &'a self,
        context: &'a PlatformTurnContext,
        event: &'a PlatformInboundEvent,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            if event.kind != PlatformInboundEventKind::MessageRecall || event.message_id.is_empty()
            {
                return Ok(());
            }
            let now = Instant::now();
            let settings = recall_settings(context).unwrap_or_default();
            let mut state = self.state.lock().unwrap();
            Self::prune(
                &mut state,
                now,
                Duration::from_secs(settings.cancel_record_ttl_seconds),
            );
            let scope = state
                .scopes
                .entry(context.conversation.scope_key())
                .or_default();
            scope.touched_at = Some(now);
            scope.recalled.insert(event.message_id.clone(), now);
            scope
                .sent
                .retain(|message_id| message_id != &event.message_id);
            if let Some(cancel) = scope.pending.remove(&event.message_id) {
                cancel.send_replace(true);
            }
            Ok(())
        })
    }

    fn turn_is_superseded(&self, context: &PlatformTurnContext) -> bool {
        self.recalled(context)
    }

    fn after_send<'a>(
        &'a self,
        context: &'a PlatformTurnContext,
        _message: &'a OutboundMessage,
        receipt: &'a SendReceipt,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let settings = recall_settings(context)?;
            if !settings.capture_outgoing_messages {
                return Ok(());
            }
            let now = Instant::now();
            let mut state = self.state.lock().unwrap();
            let scope = state
                .scopes
                .entry(context.conversation.scope_key())
                .or_default();
            scope.touched_at = Some(now);
            if let Some(id) = context.inbound_event().map(|event| &event.message_id) {
                scope.pending.remove(id);
            }
            for id in &receipt.message_ids {
                if !id.is_empty() {
                    scope.sent.retain(|old| old != id);
                    scope.sent.push_back(id.clone());
                }
            }
            while scope.sent.len() > settings.max_messages_per_conversation {
                scope.sent.pop_front();
            }
            Ok(())
        })
    }

    fn after_turn_aborted<'a>(
        &'a self,
        context: &'a PlatformTurnContext,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            if let Some(id) = context.inbound_event().map(|event| &event.message_id) {
                if let Some(scope) = self
                    .state
                    .lock()
                    .unwrap()
                    .scopes
                    .get_mut(&context.conversation.scope_key())
                {
                    scope.pending.remove(id);
                }
            }
            Ok(())
        })
    }
}

impl Clone for MessageRecallPlugin {
    fn clone(&self) -> Self {
        Self {
            state: self.state.clone(),
        }
    }
}

fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "message_id": {
                "type": ["string", "integer"],
                "description": "Required only when the current QQ message does not reply to a target. A trusted reply always overrides this argument. Never pass the current request message ID and never guess a recent message."
            },
            "reason": { "type": "string", "maxLength": 500 }
            ,"confirmation_token": { "type": "string" }
        },
        "additionalProperties": false
    })
}
fn explicit_id(args: &Value) -> Result<Option<String>> {
    let Some(value) = args.get("message_id") else {
        return Ok(None);
    };
    let id = match value {
        Value::Null => return Ok(None),
        Value::String(id) => id.trim().to_string(),
        Value::Number(id) => id.to_string(),
        _ => bail!("message_id must be a numeric string or integer"),
    };
    if id.is_empty() {
        return Ok(None);
    }
    if !id.bytes().all(|b| b.is_ascii_digit()) {
        bail!("message_id must be numeric");
    }
    let numeric = id
        .parse::<u64>()
        .context("message_id is outside the supported numeric range")?;
    if numeric > i32::MAX as u64 {
        bail!("message_id is outside the supported OneBot range");
    }
    Ok(Some(id))
}
fn reply_id(context: &PlatformTurnContext) -> Option<String> {
    context
        .inbound_event()
        .and_then(|event| event.reply_to_message_id.clone())
        .filter(|id| !id.is_empty())
}

fn select_target(explicit: Option<String>, reply: Option<String>) -> Result<RecallTarget> {
    if let Some(message_id) = reply {
        // The reply relation is trusted platform metadata. Models often
        // confuse the current request ID with the quoted target ID, so a
        // quoted group message always owns the recall target.
        return Ok(RecallTarget {
            message_id,
            source: TargetSource::Reply,
        });
    }
    explicit
        .map(|message_id| RecallTarget {
            message_id,
            source: TargetSource::Argument,
        })
        .context("message_id or a replied-to message is required")
}
fn replied_info<'a>(
    context: &'a PlatformTurnContext,
    message_id: &str,
) -> Option<&'a PlatformMessageInfo> {
    context
        .inbound_event()
        .and_then(|event| event.replied_message.as_ref())
        .filter(|message| message.message_id == message_id)
}
fn recall_settings(context: &PlatformTurnContext) -> Result<QqMessageRecallPluginSettings> {
    context
        .config
        .platforms
        .qq
        .plugins
        .get(MESSAGE_RECALL_PLUGIN_ID)
        .map(QqMessageRecallPluginSettings::from_instance)
        .transpose()
        .map(|settings| settings.unwrap_or_default())
}
fn reason(args: &Value, maximum: usize) -> Result<String> {
    let value = args
        .get("reason")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    if value.chars().count() > maximum {
        bail!("reason exceeds configured maximum length");
    }
    Ok(value.to_string())
}
fn response(success: bool, message: &str, data: Value) -> Result<String> {
    Ok(json!({ "success": success, "message": message, "data": data }).to_string())
}

fn failure_response(code: &str, retryable: bool, message: &str, data: Value) -> Result<String> {
    Ok(json!({
        "success": false,
        "code": code,
        "retryable": retryable,
        "message": message,
        "data": data
    })
    .to_string())
}

fn recall_failure_response(
    error: &anyhow::Error,
    message_id: &str,
    source: TargetSource,
    target_kind: &str,
) -> Result<String> {
    let detail = error.to_string();
    let decode_failed = detail.contains("retcode=1200") && detail.contains("decode failed");
    failure_response(
        if decode_failed {
            "napcat_recall_decode_failed"
        } else {
            "qq_recall_failed"
        },
        false,
        if decode_failed {
            "QQ 内核拒绝了撤回请求，消息没有被撤回。不要重试或改撤其他消息。"
        } else {
            "QQ 撤回接口调用失败，消息没有被撤回。不要改撤其他消息。"
        },
        json!({
            "message_id": message_id,
            "target_source": source.as_str(),
            "target_kind": target_kind,
            "detail": detail
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_message_id_must_be_numeric() {
        assert_eq!(
            explicit_id(&json!({ "message_id": "123" })).unwrap(),
            Some("123".to_string())
        );
        assert!(explicit_id(&json!({ "message_id": "abc" })).is_err());
        assert_eq!(
            explicit_id(&json!({ "message_id": 123 })).unwrap(),
            Some("123".to_string())
        );
        assert_eq!(explicit_id(&json!({})).unwrap(), None);
        assert!(explicit_id(&json!({ "message_id": i32::MAX as u64 + 1 })).is_err());
    }

    #[test]
    fn response_contract_is_structured_json() {
        let value: Value =
            serde_json::from_str(&response(true, "ok", json!({ "message_id": "1" })).unwrap())
                .unwrap();
        assert_eq!(value["success"], true);
        assert_eq!(value["message"], "ok");
    }

    #[test]
    fn quoted_group_target_overrides_any_model_argument() {
        let target = select_target(
            Some("current-request-id".to_string()),
            Some("quoted-target-id".to_string()),
        )
        .unwrap();
        assert_eq!(target.message_id, "quoted-target-id");
        assert_eq!(target.source, TargetSource::Reply);

        let target = select_target(None, Some("quoted-target-id".to_string())).unwrap();
        assert_eq!(target.message_id, "quoted-target-id");
        assert_eq!(target.source, TargetSource::Reply);
    }

    #[test]
    fn non_reply_group_target_uses_verified_argument() {
        let target = select_target(Some("history-id".to_string()), None).unwrap();
        assert_eq!(target.message_id, "history-id");
        assert_eq!(target.source, TargetSource::Argument);
        assert!(select_target(None, None).is_err());
    }

    #[test]
    fn napcat_decode_failure_is_non_retryable_and_truthful() {
        let error = anyhow::anyhow!("OneBot API delete_msg failed: retcode=1200, decode failed");
        let value: Value = serde_json::from_str(
            &recall_failure_response(&error, "600025761", TargetSource::Reply, "group_member")
                .unwrap(),
        )
        .unwrap();
        assert_eq!(value["success"], false);
        assert_eq!(value["retryable"], false);
        assert_eq!(value["code"], "napcat_recall_decode_failed");
        assert_eq!(value["data"]["message_id"], "600025761");
        assert!(value["message"].as_str().unwrap().contains("没有被撤回"));
    }
}
