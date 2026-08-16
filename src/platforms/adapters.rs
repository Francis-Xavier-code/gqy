//! adapters — 自 src/platforms/mod.rs 拆分。

pub(crate) use super::*;

pub(crate) struct RateWindow {
    last_prune: Instant,
    conversations: HashMap<String, SenderWindow>,
}

pub(crate) struct SenderWindow {
    window_start: Instant,
    window: Duration,
    count: u32,
    notified: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RateDecision {
    Allow,
    /// Over quota and already warned this window.
    DropSilently,
    /// Over quota for the first time this window: send one notice.
    DropWithNotice,
}

impl RateWindow {
    pub(crate) fn new() -> Self {
        Self {
            last_prune: Instant::now(),
            conversations: HashMap::new(),
        }
    }

    pub(crate) fn check(&mut self, conversation: &str, limit: PlatformRateLimit) -> RateDecision {
        self.check_at(Instant::now(), conversation, limit)
    }

    pub(crate) fn available(&mut self, conversation: &str, limit: PlatformRateLimit) -> bool {
        self.available_at(Instant::now(), conversation, limit)
    }

    pub(crate) fn available_at(
        &mut self,
        now: Instant,
        conversation: &str,
        limit: PlatformRateLimit,
    ) -> bool {
        self.prune_at(now);
        if limit.max_messages == 0 {
            return true;
        }
        let configured_window = Duration::from_secs(u64::from(limit.window_seconds));
        self.conversations.get(conversation).is_none_or(|entry| {
            entry.window != configured_window
                || now.duration_since(entry.window_start) >= configured_window
                || entry.count < limit.max_messages
        })
    }

    pub(crate) fn check_at(
        &mut self,
        now: Instant,
        conversation: &str,
        limit: PlatformRateLimit,
    ) -> RateDecision {
        self.prune_at(now);
        if limit.max_messages == 0 {
            return RateDecision::Allow;
        }
        let configured_window = Duration::from_secs(u64::from(limit.window_seconds));
        let entry = self
            .conversations
            .entry(conversation.to_string())
            .or_insert(SenderWindow {
                window_start: now,
                window: configured_window,
                count: 0,
                notified: false,
            });
        if entry.window != configured_window
            || now.duration_since(entry.window_start) >= configured_window
        {
            *entry = SenderWindow {
                window_start: now,
                window: configured_window,
                count: 0,
                notified: false,
            };
        }
        if entry.count < limit.max_messages {
            entry.count += 1;
            return RateDecision::Allow;
        }
        if entry.notified {
            return RateDecision::DropSilently;
        }
        entry.notified = true;
        RateDecision::DropWithNotice
    }

    pub(crate) fn prune_at(&mut self, now: Instant) {
        if now.duration_since(self.last_prune) >= RATE_PRUNE_INTERVAL {
            self.last_prune = now;
            self.conversations.retain(|_, entry| {
                now.checked_duration_since(entry.window_start)
                    .is_some_and(|elapsed| elapsed < entry.window)
            });
        }
    }
}

/// Finds or creates the dedicated user session for a stable external
/// conversation identity. The visible session name can be edited freely;
/// routing never depends on it after the binding has been created.
/// Per-conversation fixed-window rate limiter shared by all platforms.
pub(crate) fn resolve_platform_session(
    state: &DaemonState,
    conversation: &PlatformConversation,
    persona: &str,
    participant_id: Option<String>,
    name: &str,
    legacy_name: Option<&str>,
) -> Result<Arc<str>> {
    let key = PlatformSessionBindingKey {
        platform: conversation.platform.clone(),
        account_id: conversation.account_id.clone(),
        conversation_kind: conversation.kind.as_str().to_string(),
        conversation_id: conversation.conversation_id.clone(),
        participant_id,
        persona: persona.to_string(),
    };
    if let Some(session_id) = state.state_store.find_platform_session_binding(&key)? {
        let record = state
            .state_store
            .session_record(&session_id)?
            .with_context(|| format!("bound platform session is missing: {session_id}"))?;
        return Ok(record.session_id.into());
    }

    // Adopt the pre-binding name only when it identifies exactly one session.
    // If multiple bot accounts race for the same legacy name, the first bind
    // wins and every later account gets a fresh, correctly isolated session.
    let mut candidates = state
        .state_store
        .list_sessions(persona)?
        .into_iter()
        .filter(|overview| {
            overview.record.kind == "user"
                && (overview.record.name == name
                    || legacy_name.is_some_and(|legacy| overview.record.name == legacy))
        })
        .map(|overview| overview.record)
        .collect::<Vec<_>>();
    if candidates.len() == 1 {
        let record = candidates.pop().expect("length checked");
        match state
            .state_store
            .claim_platform_session(&key, &record.session_id)
        {
            Ok(session_id) if session_id == record.session_id => {
                return Ok(record.session_id.into());
            }
            Ok(session_id) => return Ok(session_id.into()),
            Err(error) => {
                tracing::warn!(error = %error, session_id = %record.session_id, "{}", crate::i18n::text("legacy platform session could not be bound", "无法绑定旧版平台会话"));
                if let Some(session_id) = state.state_store.find_platform_session_binding(&key)? {
                    return Ok(session_id.into());
                }
            }
        }
    } else if candidates.len() > 1 {
        tracing::warn!(
            name,
            "{}",
            crate::i18n::text(
                "legacy platform session name is ambiguous; creating a new session",
                "旧版平台会话名称存在歧义；正在创建新会话",
            )
        );
    }

    let (record, created) = state
        .state_store
        .create_or_get_platform_session(&key, name)?;
    if created {
        state.events.publish(
            "session.created",
            serde_json::json!({
                "session_id": record.session_id,
                "name": record.name,
                "platform": conversation.platform,
                "account_id": conversation.account_id,
                "conversation_kind": conversation.kind.as_str(),
                "conversation_id": conversation.conversation_id,
            }),
        );
    }
    Ok(record.session_id.into())
}

pub(crate) struct TurnOutcome {
    pub(crate) run_id: String,
    pub(crate) text: String,
    pub(crate) provider_id: Option<String>,
    pub(crate) model: Option<String>,
    /// Image asset ids published during the turn (`tool.image` events);
    /// bridges load the bytes and re-send them platform-natively.
    pub(crate) image_assets: Vec<String>,
    /// Byte ranges produced after confirmed direct long-image tool sends.
    /// Direct-send acknowledgements are removed from the final fallback text.
    pub(crate) suppressed_reply_ranges: Vec<(usize, usize)>,
    /// The last response segment was delivered by a successful direct tool
    /// send, so an otherwise empty platform reply must not add a placeholder.
    pub(crate) final_reply_already_sent: bool,
}

#[derive(Default)]
pub(crate) struct ReplySuppression {
    ranges: Vec<(usize, usize)>,
    open_at: Option<usize>,
    final_reply_already_sent: bool,
}

impl ReplySuppression {
    pub(crate) fn direct_send_succeeded(&mut self, text_len: usize) {
        self.open_at = Some(
            self.open_at
                .map_or(text_len, |existing| existing.min(text_len)),
        );
        self.final_reply_already_sent = true;
    }

    pub(crate) fn model_started(&mut self) {
        self.ranges.clear();
        // A direct tool send answers the same prompt across its model
        // continuation, so suppress that continuation from its first byte.
        self.open_at = self.final_reply_already_sent.then_some(0);
    }

    pub(crate) fn queued_prompt_consumed(&mut self) {
        self.ranges.clear();
        self.open_at = None;
        self.final_reply_already_sent = false;
    }

    pub(crate) fn finish(mut self, text_len: usize) -> (Vec<(usize, usize)>, bool) {
        self.close_range(text_len);
        (self.ranges, self.final_reply_already_sent)
    }

    pub(crate) fn close_range(&mut self, text_len: usize) {
        if let Some(start) = self.open_at.take() {
            if start < text_len {
                self.ranges.push((start, text_len));
            }
        }
    }

    /// Ranges to cut when the current round's text is flushed mid-turn as an
    /// intermediate reply. Leaves the state untouched so the `model_started`
    /// reset that follows keeps its existing semantics.
    pub(crate) fn round_ranges(&self, text_len: usize) -> Vec<(usize, usize)> {
        let mut ranges = self.ranges.clone();
        if let Some(start) = self.open_at {
            if start < text_len {
                ranges.push((start, text_len));
            }
        }
        ranges
    }
}

pub(crate) fn cut_suppressed_ranges(text: &str, ranges: &[(usize, usize)]) -> String {
    if ranges.is_empty() {
        return text.to_string();
    }
    let mut result = String::with_capacity(text.len());
    let mut cursor = 0;
    for &(start, end) in ranges {
        let start = start.clamp(cursor, text.len());
        let end = end.clamp(start, text.len());
        let (Some(prefix), Some(_suppressed)) = (text.get(cursor..start), text.get(start..end))
        else {
            continue;
        };
        result.push_str(prefix);
        cursor = end;
    }
    if let Some(suffix) = text.get(cursor..) {
        result.push_str(suffix);
    }
    result
}

pub(crate) fn start_model_reply(text: &mut String, suppression: &mut ReplySuppression) {
    text.clear();
    suppression.model_started();
}

/// Sends the just-finished model round as its own platform message. The
/// round's direct-send suppression ranges still apply, so text a tool already
/// delivered is not repeated.
pub(crate) async fn flush_intermediate_reply(
    context: &PlatformTurnContext,
    text: &str,
    suppression: &ReplySuppression,
) {
    if context.turn_is_superseded() {
        return;
    }
    let visible = cut_suppressed_ranges(text, &suppression.round_ranges(text.len()));
    let visible = visible.trim();
    if visible.is_empty() {
        return;
    }
    match context
        .send(OutboundMessage::markdown(
            OutboundOrigin::IntermediateReply,
            visible.to_string(),
        ))
        .await
    {
        Ok(_) => tracing::info!(
            target: "gqy::qq",
            chars = visible.chars().count(),
            "{}",
            crate::i18n::text(
                "sent an intermediate platform reply",
                "已发送平台中间消息",
            )
        ),
        Err(error) => tracing::warn!(
            target: "gqy::qq",
            error = %error,
            "{}",
            crate::i18n::text(
                "sending an intermediate platform reply failed",
                "发送平台中间消息失败",
            )
        ),
    }
}

pub(crate) fn format_platform_tool_payload(payload: &str) -> String {
    format_platform_tool_payload_for(payload, crate::i18n::locale())
}

pub(crate) fn format_platform_tool_payload_for(payload: &str, locale: Locale) -> String {
    let sanitized = sanitize_platform_log_text(payload.trim());
    let text = sanitized.as_str();
    if text.chars().count() > PLATFORM_TOOL_LOG_MAX_CHARS {
        return truncate_platform_tool_log(text, locale);
    }
    let formatted = serde_json::from_str::<Value>(text)
        .ok()
        .and_then(|value| serde_json::to_string_pretty(&value).ok())
        .unwrap_or_else(|| text.to_string());
    truncate_platform_tool_log(&formatted, locale)
}

pub(crate) fn truncate_platform_tool_log(text: &str, locale: Locale) -> String {
    truncate_platform_log(text, PLATFORM_TOOL_LOG_MAX_CHARS, locale)
}

pub(crate) fn truncate_platform_reply_log(text: &str) -> String {
    truncate_platform_reply_log_for(text, crate::i18n::locale())
}

pub(crate) fn truncate_platform_reply_log_for(text: &str, locale: Locale) -> String {
    sanitize_platform_log_text(&truncate_platform_log(
        text,
        PLATFORM_REPLY_LOG_MAX_CHARS,
        locale,
    ))
}

pub(crate) fn sanitize_platform_log_text(text: &str) -> String {
    let mut sanitized = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '\n' | '\t' => sanitized.push(character),
            character if character.is_control() => sanitized.extend(character.escape_default()),
            character => sanitized.push(character),
        }
    }
    sanitized
}

pub(crate) fn truncate_platform_log(text: &str, max_chars: usize, locale: Locale) -> String {
    let total = text.chars().count();
    if total <= max_chars {
        return text.to_string();
    }
    let omitted = total - max_chars;
    format!(
        "{}\n{}",
        text.chars().take(max_chars).collect::<String>(),
        if locale == Locale::Zh {
            format!("... 已截断 {omitted} 字符 ...")
        } else {
            format!("... truncated {omitted} characters ...")
        }
    )
}

pub(crate) fn format_platform_final_reply_log(
    outcome: &TurnOutcome,
    context: &PlatformTurnContext,
    reply_text: &str,
    image_count: usize,
) -> String {
    format_platform_final_reply_log_for(
        outcome,
        context,
        reply_text,
        image_count,
        crate::i18n::locale(),
    )
}

pub(crate) fn format_platform_final_reply_log_for(
    outcome: &TurnOutcome,
    context: &PlatformTurnContext,
    reply_text: &str,
    image_count: usize,
    locale: Locale,
) -> String {
    let endpoint = match (
        outcome
            .provider_id
            .as_deref()
            .filter(|value| !value.is_empty()),
        outcome.model.as_deref().filter(|value| !value.is_empty()),
    ) {
        (Some(provider), Some(model)) => format!("{provider} / {model}"),
        (Some(provider), None) => provider.to_string(),
        (None, Some(model)) => model.to_string(),
        (None, None) => text_for(locale, "unknown", "未知").to_string(),
    };
    let endpoint = sanitize_platform_log_text(&endpoint);
    let body = if reply_text.trim().is_empty() {
        if outcome.final_reply_already_sent {
            text_for(
                locale,
                "[reply was sent directly by a tool]",
                "[回复已由工具直接发送]",
            )
            .to_string()
        } else if image_count > 0 {
            if locale == Locale::Zh {
                format!("[无文本，发送 {image_count} 张图片]")
            } else {
                format!("[no text; sent {image_count} images]")
            }
        } else {
            text_for(locale, "[empty reply]", "[空回复]").to_string()
        }
    } else {
        truncate_platform_reply_log_for(reply_text.trim(), locale)
    };
    let conversation_kind = match (locale, context.conversation.kind) {
        (Locale::Zh, ConversationKind::Group) => "群聊",
        (Locale::Zh, ConversationKind::Private) => "私聊",
        (_, kind) => kind.as_str(),
    };
    if locale == Locale::Zh {
        format!(
            "【AI 最终回复】\n运行：{}\n会话：{} {}（机器人账号 {}）\n模型：{}\n内容：\n{}",
            outcome.run_id,
            conversation_kind,
            context.conversation.conversation_id,
            context.conversation.account_id,
            endpoint,
            body
        )
    } else {
        format!(
            "[AI final reply]\nRun: {}\nConversation: {} {} (bot account {})\nModel: {}\nContent:\n{}",
            outcome.run_id,
            conversation_kind,
            context.conversation.conversation_id,
            context.conversation.account_id,
            endpoint,
            body
        )
    }
}

pub(crate) fn format_platform_tool_name(name: &str, display_name: Option<&str>) -> String {
    display_name
        .filter(|display_name| *display_name != name)
        .map(sanitize_platform_tool_label)
        .unwrap_or_else(|| sanitize_platform_tool_label(name))
}

pub(crate) fn sanitize_platform_tool_label(value: &str) -> String {
    let compact = sanitize_platform_log_text(value)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if compact.is_empty() {
        "unknown".to_string()
    } else {
        compact.chars().take(128).collect()
    }
}

pub(crate) fn format_platform_tool_started_log(run_id: &str, data: &Value) -> String {
    format_platform_tool_started_log_for(run_id, data, crate::i18n::locale())
}

pub(crate) fn format_platform_tool_started_log_for(
    run_id: &str,
    data: &Value,
    locale: Locale,
) -> String {
    let tool_id = data
        .get("tool_id")
        .and_then(Value::as_str)
        .unwrap_or_else(|| text_for(locale, "unknown", "未知"));
    let name = data
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_else(|| text_for(locale, "unknown", "未知"));
    let display_name = data.get("display_name").and_then(Value::as_str);
    let arguments = data
        .get("arguments")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let tool_name = sanitize_platform_tool_label(name);
    let display_name = format_platform_tool_name(name, display_name);
    let arguments = format_platform_tool_payload_for(arguments, locale);
    if locale == Locale::Zh {
        let mut lines = vec![
            format!("【工具：{tool_name}】"),
            format!("运行：{run_id}"),
            format!("调用 ID：{tool_id}"),
        ];
        if display_name != tool_name {
            lines.push(format!("显示名称：{display_name}"));
        }
        lines.push(format!("参数：\n{arguments}"));
        lines.join("\n")
    } else {
        let mut lines = vec![
            format!("[Tool: {tool_name}]"),
            format!("Run: {run_id}"),
            format!("Call ID: {tool_id}"),
        ];
        if display_name != tool_name {
            lines.push(format!("Display name: {display_name}"));
        }
        lines.push(format!("Arguments:\n{arguments}"));
        lines.join("\n")
    }
}

pub(crate) fn format_platform_tool_finished_log(run_id: &str, data: &Value) -> String {
    format_platform_tool_finished_log_for(run_id, data, crate::i18n::locale())
}

pub(crate) fn format_platform_tool_finished_log_for(
    run_id: &str,
    data: &Value,
    locale: Locale,
) -> String {
    let tool_id = data
        .get("tool_id")
        .and_then(Value::as_str)
        .unwrap_or_else(|| text_for(locale, "unknown", "未知"));
    let name = data
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_else(|| text_for(locale, "unknown", "未知"));
    let display_name = data.get("display_name").and_then(Value::as_str);
    let ok = data.get("ok").and_then(Value::as_bool).unwrap_or(false);
    let output = data
        .get("output")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let tool_name = sanitize_platform_tool_label(name);
    let display_name = format_platform_tool_name(name, display_name);
    let output = format_platform_tool_payload_for(output, locale);
    if locale == Locale::Zh {
        let mut lines = vec![
            format!("【工具结果：{tool_name}】"),
            format!("运行：{run_id}"),
            format!("调用 ID：{tool_id}"),
        ];
        if display_name != tool_name {
            lines.push(format!("显示名称：{display_name}"));
        }
        lines.push(format!("状态：{}", if ok { "成功" } else { "失败" }));
        lines.push(format!("结果：\n{output}"));
        lines.join("\n")
    } else {
        let mut lines = vec![
            format!("[Tool result: {tool_name}]"),
            format!("Run: {run_id}"),
            format!("Call ID: {tool_id}"),
        ];
        if display_name != tool_name {
            lines.push(format!("Display name: {display_name}"));
        }
        lines.push(format!("Status: {}", if ok { "success" } else { "failed" }));
        lines.push(format!("Result:\n{output}"));
        lines.join("\n")
    }
}

pub(crate) enum TurnDispatch {
    Completed(TurnOutcome),
    Failed(String),
}

/// Drives one agent turn for an inbound IM message and waits for the
/// final result. Mirrors `handle_ipc_turn`, minus the client stream.
pub(crate) async fn run_platform_turn(
    state: &DaemonState,
    session_id: Arc<str>,
    content: String,
    images: Vec<Option<ImageAttachment>>,
    mut profile: TurnProfile,
) -> Result<TurnDispatch> {
    let content = validate_content(content).map_err(|error| anyhow!(error.message))?;
    state.state_store.recover_stale_turns()?;

    let _global_permit = state
        .platforms
        .turn_permits
        .clone()
        .acquire_owned()
        .await
        .map_err(|_| anyhow!("the platform turn scheduler is closed"))?;

    let run_id = random_id("run", 18);
    let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
    let platform_context = profile.platform.clone();
    let intermediate_replies = platform_context.as_ref().is_some_and(|context| {
        let qq = &context.config.platforms.qq;
        match context.conversation.kind {
            ConversationKind::Group => qq.group_intermediate_messages,
            ConversationKind::Private => qq.private_intermediate_messages,
        }
    });
    let platform_followup = platform_context
        .as_ref()
        .map(|context| PlatformFollowupRun::new(context.clone()));
    profile.followup = platform_followup.clone();
    {
        let mut manager = state.manager.lock().unwrap();
        if manager.admin_busy {
            bail!("GQY is busy with another operation");
        }
        manager.active_runs.insert(
            run_id.clone(),
            RunInfo {
                session_id: session_id.clone(),
                mode: AgentMode::Normal,
                audience: PromptAudience::External,
                cancel: cancel_tx.clone(),
                turn_id: None,
                queue_target: None,
                supersede: Arc::new(crate::agent::TurnSupersedeSignal::default()),
                platform_followup,
                operation: crate::web::RunOperation::Create,
                job_wake: false,
                job_wake_label: None,
                // 平台真实入站消息;wake 合成轮的来源细分待平台 goal 支持时一并做。
                turn_origin: crate::tools::workspace::TurnOrigin::Human,
            },
        );
    }
    if let Some(context) = platform_context.as_ref() {
        context.turn_started(cancel_tx);
    }
    if platform_context
        .as_ref()
        .is_some_and(|context| context.turn_is_superseded())
    {
        crate::web::finish_run(&state.manager, &run_id, None);
        return Ok(TurnDispatch::Failed(
            crate::i18n::text("the turn was superseded", "本轮已被新消息覆盖").to_string(),
        ));
    }
    let after = state.events.latest_id();
    let mut subscription = state.events.subscribe_after(after);
    if state
        .actor_tx
        .send(ActorCommand::StartTurn {
            run_id: run_id.clone(),
            session_id,
            display_content: content.clone(),
            content,
            attachment_run_id: None,
            mode: AgentMode::Normal,
            images,
            cwd: None,
            origin_tty: None,
            audience: PromptAudience::External,
            profile: Some(profile),
            cancel: cancel_rx,
            turn_origin: Box::new(crate::tools::workspace::TurnOrigin::Human),
        })
        .is_err()
    {
        crate::web::finish_run(&state.manager, &run_id, None);
        bail!("GQY core worker is unavailable");
    }
    // Cancels the run if this task dies before the turn settles.
    let mut run_guard = IpcRunGuard {
        manager: state.manager.clone(),
        run_id: run_id.clone(),
        finished: false,
    };

    let deadline = tokio::time::Instant::now() + PLATFORM_TURN_TIMEOUT;
    let mut text = String::new();
    let mut image_assets = Vec::new();
    let mut reply_suppression = ReplySuppression::default();
    let mut last_id = after;
    let dispatch = loop {
        let record = if let Some(record) = subscription.pending.pop_front() {
            record
        } else {
            match tokio::time::timeout_at(deadline, subscription.receiver.recv()).await {
                Err(_) => {
                    break TurnDispatch::Failed(
                        crate::i18n::text("the reply timed out", "回复超时，本轮已取消")
                            .to_string(),
                    );
                }
                Ok(Ok(record)) => record,
                Ok(Err(broadcast::error::RecvError::Lagged(_))) => {
                    subscription.pending = state.events.replay_after(last_id);
                    continue;
                }
                Ok(Err(broadcast::error::RecvError::Closed)) => {
                    break TurnDispatch::Failed(
                        crate::i18n::text("GQY core stopped", "GQY 核心已停止").to_string(),
                    );
                }
            }
        };
        if record.kind == "resync_required" {
            break TurnDispatch::Failed(
                crate::i18n::text(
                    "event history was exhausted; the turn was cancelled",
                    "事件缓冲耗尽，本轮已取消",
                )
                .to_string(),
            );
        }
        last_id = record.id;
        let Ok(data) = serde_json::from_str::<Value>(&record.data) else {
            continue;
        };
        if data.get("run_id").and_then(Value::as_str) != Some(run_id.as_str()) {
            continue;
        }
        match record.kind.as_str() {
            "reasoning.start" => {
                if intermediate_replies {
                    if let Some(context) = platform_context.as_ref() {
                        flush_intermediate_reply(context, &text, &reply_suppression).await;
                    }
                }
                start_model_reply(&mut text, &mut reply_suppression);
            }
            "assistant.delta" => {
                if let Some(delta) = data.get("delta").and_then(Value::as_str) {
                    text.push_str(delta);
                }
            }
            "generation.superseded" => {
                text.clear();
                reply_suppression.model_started();
            }
            "tool.started" => {
                let readable = format_platform_tool_started_log(&run_id, &data);
                tracing::info!(target: "gqy::qq", "\n{readable}");
            }
            "tool.image" => {
                if let Some(id) = data
                    .get("asset")
                    .and_then(|asset| asset.get("id"))
                    .and_then(Value::as_str)
                {
                    image_assets.push(id.to_string());
                }
            }
            "tool.finished" => {
                let readable = format_platform_tool_finished_log(&run_id, &data);
                tracing::info!(target: "gqy::qq", "\n{readable}");
                let suppression_start = platform_context
                    .as_ref()
                    .and_then(|context| context.take_final_reply_suppression_start(text.len()));
                if let Some(start) = suppression_start {
                    reply_suppression.direct_send_succeeded(start);
                }
            }
            "queue.consumed" => {
                // Flush before the suppression reset below: the flushed text
                // still needs the direct-send ranges of the round it came
                // from, and the next round answers the newly consumed prompt.
                if intermediate_replies {
                    if let Some(context) = platform_context.as_ref() {
                        flush_intermediate_reply(context, &text, &reply_suppression).await;
                    }
                    text.clear();
                }
                reply_suppression.queued_prompt_consumed();
            }
            "run.completed" => {
                run_guard.finish();
                let (suppressed_reply_ranges, final_reply_already_sent) =
                    reply_suppression.finish(text.len());
                break TurnDispatch::Completed(TurnOutcome {
                    run_id: run_id.clone(),
                    text,
                    provider_id: data
                        .get("provider_id")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    model: data
                        .get("model")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    image_assets,
                    suppressed_reply_ranges,
                    final_reply_already_sent,
                });
            }
            "run.failed" => {
                run_guard.finish();
                let message = data
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown error")
                    .to_string();
                break TurnDispatch::Failed(message);
            }
            "run.cancelled" => {
                run_guard.finish();
                break TurnDispatch::Failed(
                    crate::i18n::text("the turn was cancelled", "本轮被取消了").to_string(),
                );
            }
            _ => {}
        }
    };
    Ok(dispatch)
}

/// Strips markdown decoration for plain-text IM surfaces (QQ renders no
/// markup). Deliberately conservative: fenced code bodies are kept
/// verbatim, single `*` stays (could be math), lists and newlines stay.
pub(crate) fn markdown_to_plain(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_fence = false;
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            out.push_str(line);
            out.push('\n');
            continue;
        }
        let stripped = if trimmed.starts_with('#') {
            trimmed.trim_start_matches('#').trim_start()
        } else if let Some(rest) = trimmed.strip_prefix("> ") {
            rest
        } else {
            line
        };
        out.push_str(&strip_inline_markup(stripped));
        out.push('\n');
    }
    out.trim_end().to_string()
}

/// Removes `**`, `__`, backticks and rewrites `[text](url)` → `text (url)`.
pub(crate) fn strip_inline_markup(line: &str) -> String {
    let chars: Vec<char> = line.chars().collect();
    let mut out = String::with_capacity(line.len());
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '*' if chars.get(i + 1) == Some(&'*') => i += 2,
            '_' if chars.get(i + 1) == Some(&'_') => i += 2,
            '`' => i += 1,
            '[' => {
                // Try [text](url); anything else is emitted verbatim.
                let close = chars[i + 1..].iter().position(|&c| c == ']');
                let parsed = close.and_then(|offset| {
                    let close = i + 1 + offset;
                    if chars.get(close + 1) == Some(&'(') {
                        let end = chars[close + 2..].iter().position(|&c| c == ')');
                        end.map(|len| {
                            let text: String = chars[i + 1..close].iter().collect();
                            let url: String = chars[close + 2..close + 2 + len].iter().collect();
                            (close + 2 + len + 1, text, url)
                        })
                    } else {
                        None
                    }
                });
                match parsed {
                    Some((next, text, url)) => {
                        out.push_str(&text);
                        if !url.is_empty() && url != text {
                            out.push_str(" (");
                            out.push_str(&url);
                            out.push(')');
                        }
                        i = next;
                    }
                    None => {
                        out.push('[');
                        i += 1;
                    }
                }
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

/// Splits an over-long reply on paragraph, then line, then raw char
/// boundaries. Char-based so CJK never gets cut mid-codepoint.
pub(crate) fn split_reply(text: &str, max_chars: usize) -> Vec<String> {
    let text = text.trim();
    if text.is_empty() {
        return Vec::new();
    }
    if max_chars == 0 || text.chars().count() <= max_chars {
        return vec![text.to_string()];
    }
    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut current_chars = 0;
    let flush = |current: &mut String, current_chars: &mut usize, chunks: &mut Vec<String>| {
        let piece = current.trim();
        if !piece.is_empty() {
            chunks.push(piece.to_string());
        }
        current.clear();
        *current_chars = 0;
    };
    for paragraph in text.split("\n\n") {
        let unit_chars = paragraph.chars().count();
        if unit_chars > max_chars {
            flush(&mut current, &mut current_chars, &mut chunks);
            // Oversized paragraph: pack by lines, hard-split huge lines.
            for line in paragraph.lines() {
                let line_chars = line.chars().count();
                if line_chars > max_chars {
                    flush(&mut current, &mut current_chars, &mut chunks);
                    let mut buffer = String::new();
                    let mut count = 0;
                    for c in line.chars() {
                        buffer.push(c);
                        count += 1;
                        if count == max_chars {
                            chunks.push(buffer.clone());
                            buffer.clear();
                            count = 0;
                        }
                    }
                    if !buffer.trim().is_empty() {
                        chunks.push(buffer.trim().to_string());
                    }
                    continue;
                }
                if current_chars + line_chars + 1 > max_chars {
                    flush(&mut current, &mut current_chars, &mut chunks);
                }
                if !current.is_empty() {
                    current.push('\n');
                    current_chars += 1;
                }
                current.push_str(line);
                current_chars += line_chars;
            }
            flush(&mut current, &mut current_chars, &mut chunks);
            continue;
        }
        if current_chars + unit_chars + 2 > max_chars {
            flush(&mut current, &mut current_chars, &mut chunks);
        }
        if !current.is_empty() {
            current.push_str("\n\n");
            current_chars += 2;
        }
        current.push_str(paragraph);
        current_chars += unit_chars;
    }
    flush(&mut current, &mut current_chars, &mut chunks);
    chunks
}

/// Sniffs the mime type of downloaded image bytes by magic numbers.
pub(crate) fn sniff_image_mime(bytes: &[u8]) -> &'static str {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        "image/png"
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        "image/jpeg"
    } else if bytes.starts_with(b"GIF8") {
        "image/gif"
    } else if bytes.len() > 11 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        "image/webp"
    } else {
        "image/png"
    }
}

/// Downloads a URL with a byte cap enforced while streaming, so an
/// oversized (or length-less) body can never balloon memory.
pub(crate) async fn download_capped(
    client: &reqwest::Client,
    url: &str,
    max_bytes: usize,
    timeout: Duration,
) -> Result<(Vec<u8>, Option<String>)> {
    let response = client
        .get(url)
        .timeout(timeout)
        .send()
        .await
        .with_context(|| format!("requesting {url}"))?
        .error_for_status()
        .with_context(|| format!("downloading {url}"))?;
    if let Some(length) = response.content_length() {
        if length as usize > max_bytes {
            bail!(
                "the file is larger than the {}MB limit",
                max_bytes / 1024 / 1024
            );
        }
    }
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.with_context(|| format!("reading {url}"))?;
        if bytes.len() + chunk.len() > max_bytes {
            bail!(
                "the file is larger than the {}MB limit",
                max_bytes / 1024 / 1024
            );
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok((bytes, content_type))
}
