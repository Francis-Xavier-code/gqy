//! gates — 自 src/platforms/plugins/real_context/mod.rs 拆分。

use super::*;

impl DynamicGate {
    pub(crate) async fn acquire(
        &self,
        limit: usize,
        timeout: Duration,
    ) -> Option<DynamicGatePermit<'_>> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let current = self.active.load(Ordering::Acquire);
            if current < limit.max(1)
                && self
                    .active
                    .compare_exchange(current, current + 1, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
            {
                return Some(DynamicGatePermit { gate: self });
            }
            if tokio::time::timeout_at(deadline, self.notify.notified())
                .await
                .is_err()
            {
                return None;
            }
        }
    }
}

pub(crate) struct DynamicGatePermit<'a> {
    gate: &'a DynamicGate,
}

impl Drop for DynamicGatePermit<'_> {
    fn drop(&mut self) {
        self.gate.active.fetch_sub(1, Ordering::AcqRel);
        self.gate.notify.notify_one();
    }
}

pub(super) fn group_key(context: &PlatformTurnContext) -> Result<GroupKey> {
    group_key_for(context, &context.conversation.conversation_id)
}

pub(super) fn group_key_for(context: &PlatformTurnContext, group_id: &str) -> Result<GroupKey> {
    GroupKey::new(
        context.conversation.platform.clone(),
        context.conversation.account_id.clone(),
        group_id.to_string(),
    )
}

pub(super) fn account_key(context: &PlatformTurnContext) -> Result<AccountKey> {
    AccountKey::new(
        context.conversation.platform.clone(),
        context.conversation.account_id.clone(),
    )
}

pub(crate) fn runtime_session_key(context: &PlatformTurnContext) -> String {
    format!(
        "{}|persona:{}",
        context.conversation.scope_key(),
        context.config.active_persona_scope()
    )
}

pub(crate) fn active_reply_target(event: &PlatformInboundEvent) -> ActiveReplyTarget {
    let supplemental = event.text.trim().is_empty()
        && !event.media.is_empty()
        && event.media.iter().all(|media| {
            matches!(
                media.kind,
                PlatformMediaKind::Image | PlatformMediaKind::Emoji
            )
        });
    let replied = event.replied_message.as_ref();
    ActiveReplyTarget {
        message_id: event.message_id.clone(),
        sender_id: event.sender_id.clone(),
        sender_name: event.sender_display_name.clone(),
        timestamp: event.timestamp,
        content: truncate_utf8(event.text.trim(), 4_096).to_string(),
        reply_message_id: event
            .reply_to_message_id
            .clone()
            .or_else(|| replied.map(|message| message.message_id.clone())),
        reply_sender_id: replied.map(|message| message.sender_id.clone()),
        reply_sender_name: replied.map(|message| message.sender_display_name.clone()),
        reply_content: replied
            .map(|message| truncate_utf8(message.text.trim(), 2_048).to_string())
            .filter(|content| !content.is_empty()),
        mentioned_user_ids: event.mentioned_user_ids.clone(),
        mentioned_users: event.mentioned_users.clone(),
        supplemental,
    }
}

pub(crate) fn normalize_active_targets(targets: &mut Vec<ActiveReplyTarget>, sender_id: &str) {
    targets.retain(|target| target.sender_id == sender_id);
    let mut seen = std::collections::HashSet::new();
    targets.retain(|target| target.message_id.is_empty() || seen.insert(target.message_id.clone()));
    while targets.iter().filter(|target| !target.supplemental).count() > MAX_ACTIVE_TARGET_MESSAGES
    {
        if let Some(index) = targets.iter().position(|target| !target.supplemental) {
            targets.remove(index);
        }
    }
    while targets.iter().filter(|target| target.supplemental).count()
        > MAX_ACTIVE_SUPPLEMENT_MESSAGES
    {
        if let Some(index) = targets.iter().position(|target| target.supplemental) {
            targets.remove(index);
        }
    }
}

pub(crate) fn active_targets_from_context(context: &PlatformTurnContext) -> Vec<ActiveReplyTarget> {
    context
        .plugin_value(ACTIVE_TARGETS_KEY)
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default()
}

pub(crate) fn set_active_targets(context: &PlatformTurnContext, targets: &[ActiveReplyTarget]) {
    if let Ok(value) = serde_json::to_value(targets) {
        context.set_plugin_value(ACTIVE_TARGETS_KEY, value);
    }
}

pub(crate) fn format_mentioned_users(
    users: &[PlatformMention],
    user_ids: &[String],
    show_ids: bool,
) -> Option<String> {
    let users = if users.is_empty() {
        user_ids
            .iter()
            .map(|user_id| PlatformMention {
                user_id: user_id.clone(),
                display_name: None,
            })
            .collect::<Vec<_>>()
    } else {
        users.to_vec()
    };
    if users.is_empty() {
        return None;
    }
    Some(
        users
            .iter()
            .map(|user| match user.display_name.as_deref() {
                Some(name) if show_ids => format!(
                    "{}(QQ:{})",
                    safe_prompt_field(name),
                    safe_prompt_field(&user.user_id)
                ),
                Some(name) => safe_prompt_field(name),
                None if show_ids => format!("QQ:{}", safe_prompt_field(&user.user_id)),
                None => "名称解析失败的群成员".to_string(),
            })
            .collect::<Vec<_>>()
            .join("、"),
    )
}

pub(crate) fn active_target_prompt(
    context: &PlatformTurnContext,
    event: &PlatformInboundEvent,
    current_content: &str,
) -> String {
    let mut targets = active_targets_from_context(context);
    if !targets
        .iter()
        .any(|target| target.message_id == event.message_id)
    {
        targets.push(active_reply_target(event));
    }
    normalize_active_targets(&mut targets, &event.sender_id);
    if !targets.iter().any(|target| !target.supplemental) {
        if let Some(current) = targets
            .iter_mut()
            .find(|target| target.message_id == event.message_id)
        {
            current.supplemental = false;
        }
    }

    let show_ids = context.config.platforms.qq.user_identification;
    let format_target = |target: &ActiveReplyTarget| {
        let content = if target.message_id == event.message_id {
            truncate_utf8(current_content.trim(), MAX_ACTIVE_CURRENT_CONTENT_BYTES)
        } else {
            target.content.trim()
        };
        let content = if content.is_empty() {
            "（无文字内容；包含图片或表情）".to_string()
        } else {
            content.to_string()
        };
        let sender = if show_ids {
            format!(
                "{}(QQ:{})",
                safe_prompt_field(&target.sender_name),
                safe_prompt_field(&target.sender_id)
            )
        } else {
            safe_prompt_field(&target.sender_name)
        };
        let mut line = format!(
            "[{}] {} [msg={}]: {}",
            format_history_time(target.timestamp),
            sender,
            safe_prompt_field(&target.message_id),
            safe_prompt_field(&content)
        );
        if let Some(message_id) = target.reply_message_id.as_ref() {
            line.push_str(&format!(
                "\n  回复引用: msg={}",
                safe_prompt_field(message_id)
            ));
            if let Some(name) = target.reply_sender_name.as_ref() {
                line.push_str(&format!(" | {}", safe_prompt_field(name)));
            }
            if show_ids {
                if let Some(id) = target.reply_sender_id.as_ref() {
                    line.push_str(&format!("(QQ:{})", safe_prompt_field(id)));
                }
            }
            if let Some(content) = target.reply_content.as_ref() {
                line.push_str(&format!(" | {}", safe_prompt_field(content)));
            }
        }
        if let Some(mentions) = format_mentioned_users(
            &target.mentioned_users,
            &target.mentioned_user_ids,
            show_ids,
        ) {
            line.push_str(&format!("\n  @对象: {mentions}"));
        }
        line
    };

    let primary = targets
        .iter()
        .filter(|target| !target.supplemental)
        .map(&format_target)
        .collect::<Vec<_>>();
    let supplements = targets
        .iter()
        .filter(|target| target.supplemental)
        .map(format_target)
        .collect::<Vec<_>>();
    let current = current_content.trim().to_string();
    let previous = primary
        .into_iter()
        .filter(|line| !line.contains(&format!("[msg={}]", event.message_id)))
        .collect::<Vec<_>>();
    // 块标记同样只描述内容本身。原来结尾那条「只回复当前消息…补充材料不应被单独
    // 回复。需要调用工具时…」整条删除:前两句是跨轮指令丢失的语义来源,末句是多余
    // 的输出约束,而唯一有信息量的「以后文为准」已由标记里的"按时间先后排列"覆盖。
    let head = format!("[本轮新收到的消息]\n{current}");
    let mut sections = vec![head.clone()];
    if !previous.is_empty() {
        sections.extend([
            "\n[同一发送者本轮更早发送的消息，按时间先后排列]".to_string(),
            previous.join("\n"),
        ]);
    }
    if !supplements.is_empty() {
        sections.extend([
            "\n[同一发送者随后补发的消息，按时间先后排列]".to_string(),
            supplements.join("\n"),
        ]);
    }
    let body = sections.join("\n");
    let body = if body.len() > MAX_ACTIVE_TARGET_PROMPT_BYTES {
        let marker = "\n\n（较早合并消息因长度限制省略）\n";
        let suffix_budget = MAX_ACTIVE_TARGET_PROMPT_BYTES
            .saturating_sub(head.len())
            .saturating_sub(marker.len());
        format!("{head}{marker}{}", truncate_utf8_tail(&body, suffix_budget))
    } else {
        body
    };
    body
}

pub(crate) fn response_target(
    event: &PlatformInboundEvent,
    settings: &RealContextPluginSettings,
) -> Option<ResponseTarget> {
    if !settings.reply_target_enable {
        return None;
    }
    let target = ResponseTarget {
        message_id: event.message_id.clone(),
        user_id: event.sender_id.clone(),
        quote: settings.reply_target_quote_enable,
        mention: settings.reply_target_mention_enable,
        explicit_mention_user_ids: Vec::new(),
    };
    target.is_effective().then_some(target)
}

pub(crate) fn adaptive_response_target(
    context: &PlatformTurnContext,
    event: &PlatformInboundEvent,
    settings: &RealContextPluginSettings,
) -> Option<ResponseTarget> {
    let target = response_target(event, settings);
    context.set_adaptive_response_target(
        target.clone(),
        AdaptiveResponseTargetPolicy::new(
            event.message_position,
            event.received_at,
            settings.reply_target_quote_after_other_messages,
            settings.reply_target_mention_after_seconds,
        ),
    );
    target
}

pub(crate) fn restore_core_trigger(
    context: &PlatformTurnContext,
    decision: &mut TriggerDecision,
    fallback: &TriggerDecision,
) {
    restore_trigger_decision(decision, fallback);
    context.set_response_target(decision.response_target.clone());
}

pub(crate) fn restore_trigger_decision(decision: &mut TriggerDecision, fallback: &TriggerDecision) {
    *decision = fallback.clone();
}

pub(crate) fn identity_warning(
    context: &PlatformTurnContext,
    settings: &RealContextPluginSettings,
) -> Option<String> {
    if !context.config.platforms.qq.user_identification {
        return None;
    }
    let actual_id = context.sender_id.parse::<i64>().ok()?;
    if let Some(mapping) = settings.identity_mappings.iter().find(|mapping| {
        mapping.nickname == context.sender_display_name && mapping.user_id != actual_id
    }) {
        return Some(format!(
            "<qq-identity-warning>受保护昵称 {} 预期属于 QQ {}，但当前发送者是 QQ {}。不得把当前发送者当成预期用户。</qq-identity-warning>",
            safe_prompt_string(&mapping.nickname), mapping.user_id, actual_id
        ));
    }
    if !settings.identity_mappings.is_empty() {
        if let Some(mapping) = settings.identity_mappings.iter().find(|mapping| {
            context.sender_display_name.contains(&mapping.nickname) && mapping.user_id != actual_id
        }) {
            return Some(format!(
                "<qq-identity-warning>当前昵称 {} 包含受保护昵称 {}，但当前 QQ {} 并非预期 QQ {}。请按 QQ 号区分身份。</qq-identity-warning>",
                safe_prompt_string(&context.sender_display_name), safe_prompt_string(&mapping.nickname), actual_id, mapping.user_id
            ));
        }
    }
    None
}

pub(crate) fn safe_prompt_string(value: &str) -> String {
    let encoded = serde_json::to_string(value).unwrap_or_else(|_| "\"?\"".to_string());
    // 中文聊天正文绝大多数不含这三个字符;命中才走三段全量复制的转义链。
    if !encoded
        .bytes()
        .any(|byte| matches!(byte, b'&' | b'<' | b'>'))
    {
        return encoded;
    }
    encoded
        .replace('&', "\\u0026")
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
}

pub(crate) fn safe_prompt_field(value: &str) -> String {
    let encoded = safe_prompt_string(value);
    encoded
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .unwrap_or(&encoded)
        .to_string()
}

pub(crate) fn moderation_notice(moderation: &judge::ModerationResult) -> String {
    format!(
        "疑似违规初判：严重度 {:.1}/10；类型：{}；证据：{}；规则依据：{}；理由：{}；相关 QQ：{}；相关消息 ID：{}。",
        moderation.severity,
        empty_as(&moderation.category, "未分类"),
        empty_as(&moderation.evidence, "未提供"),
        empty_as(&moderation.rule_basis, "固定安全底线"),
        empty_as(&moderation.reasoning, "未提供"),
        moderation.related_user_ids.join(", "),
        moderation.related_message_ids.join(", "),
    )
}

pub(crate) fn empty_as<'a>(value: &'a str, fallback: &'a str) -> &'a str {
    if value.trim().is_empty() {
        fallback
    } else {
        value
    }
}

pub(crate) fn find_keyword<'a>(keywords: &'a [String], text: &str) -> Option<&'a str> {
    let mut folded = None;
    keywords
        .iter()
        .find(|keyword| {
            if keyword.is_ascii() {
                return contains_ascii_case_insensitive(text, keyword);
            }
            if !keyword
                .chars()
                .any(|character| character.is_lowercase() || character.is_uppercase())
            {
                return text.contains(keyword.as_str());
            }
            folded
                .get_or_insert_with(|| text.to_lowercase())
                .contains(&keyword.to_lowercase())
        })
        .map(String::as_str)
}

pub(crate) fn contains_ascii_case_insensitive(text: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    text.as_bytes()
        .windows(needle.len())
        .any(|window| window.eq_ignore_ascii_case(needle.as_bytes()))
}

pub(crate) fn history_query_limit(configured: usize) -> usize {
    configured.saturating_add(1).min(200)
}

pub(crate) fn prepare_history(
    messages: &mut Vec<HistoryMessage>,
    message_id: &str,
    maximum: usize,
) {
    if !message_id.is_empty() {
        messages.retain(|message| message.message_id != message_id);
    }
    if messages.len() > maximum {
        messages.drain(..messages.len() - maximum);
    }
}

pub(crate) fn restraint_adjustments(enabled: bool, strength: &str, heat: f64) -> (f64, f64) {
    if !enabled {
        return (0.0, 0.0);
    }
    let (penalty_per_heat, max_penalty, threshold_per_heat, max_threshold) = match strength {
        "light" => (0.01, 0.13, 0.015, 0.05),
        "strong" => (0.11, 0.40, 0.04, 0.12),
        _ => (0.05, 0.25, 0.025, 0.08),
    };
    (
        (heat * penalty_per_heat).min(max_penalty),
        (heat * threshold_per_heat).min(max_threshold),
    )
}

pub(crate) fn short_message_boost(
    event: &PlatformInboundEvent,
    continuation_boost: f64,
    system_boost: f64,
    strength: &str,
) -> f64 {
    if continuation_boost > 0.0 || system_boost > 0.0 || !event.media.is_empty() {
        return 0.0;
    }
    let (maximum, boost) = match strength {
        "light" => (6, 0.03),
        "strong" => (8, 0.08),
        _ => (6, 0.05),
    };
    let length = event
        .text
        .chars()
        .filter(|character| !character.is_whitespace())
        .count();
    if length > 0 && length <= maximum {
        boost
    } else {
        0.0
    }
}

pub(super) fn format_history(
    messages: &[HistoryMessage],
    maximum_bytes: usize,
    show_user_ids: bool,
) -> String {
    format_history_internal(messages, maximum_bytes, show_user_ids, 0, false).text
}

pub(crate) struct FormattedHistory {
    text: String,
    images: Vec<crate::platforms::PlatformContextImageRef>,
    message_count: usize,
}

pub(crate) fn format_history_for_turn(
    messages: &[HistoryMessage],
    maximum_bytes: usize,
    show_user_ids: bool,
    maximum_images: usize,
) -> FormattedHistory {
    format_history_internal(
        messages,
        maximum_bytes,
        show_user_ids,
        maximum_images,
        false,
    )
}

/// 只收图片引用的入口:与 `format_history_for_turn(...).images` 逐项一致
/// (含预算截断语义),但图片收满即提前停止,不再渲染剩余文本。
pub(crate) fn context_image_refs(
    messages: &[HistoryMessage],
    maximum_bytes: usize,
    show_user_ids: bool,
    maximum_images: usize,
) -> Vec<crate::platforms::PlatformContextImageRef> {
    format_history_internal(messages, maximum_bytes, show_user_ids, maximum_images, true).images
}

pub(crate) fn format_history_internal(
    messages: &[HistoryMessage],
    maximum_bytes: usize,
    show_user_ids: bool,
    maximum_images: usize,
    stop_when_images_full: bool,
) -> FormattedHistory {
    let mut lines = Vec::with_capacity(messages.len());
    let mut used_bytes = 0_usize;
    let mut images = Vec::new();
    let mut source_ids = HashMap::<(String, usize), String>::new();
    for message in messages.iter().rev() {
        // 预算触底即 break(:超限检查),唯一需要撤销的就是触底那一条:记下
        // 本条新增,失败时截回去——替代原先每条消息克隆两份集合的 O(n²)。
        let images_before = images.len();
        let mut added_sources = Vec::new();
        let mut image_index = 0_usize;
        let media = message
            .content
            .media
            .iter()
            .map(|media| {
                let image_id = if media.kind == MediaKind::Image {
                    image_index += 1;
                    if image_index > MAX_CONTEXT_IMAGES_PER_MESSAGE {
                        return format_history_media(media, None);
                    }
                    let source = (message.message_id.clone(), image_index);
                    source_ids.get(&source).cloned().or_else(|| {
                        if images.len() >= maximum_images {
                            return None;
                        }
                        // Derived from the message it came from, not from its
                        // position in the rendered window. The old
                        // `context_image_{n}` counted backwards from the newest
                        // image, so every new picture shifted every id: a
                        // reference written down in one turn pointed at a
                        // different photo in the next, and `vision_analyze`
                        // resolved it without complaint. A stale id now simply
                        // fails to resolve, which the model can act on.
                        let id = format!(
                            "img_{}_{}",
                            safe_prompt_field(&message.message_id),
                            image_index
                        );
                        source_ids.insert(source.clone(), id.clone());
                        added_sources.push(source);
                        images.push(crate::platforms::PlatformContextImageRef {
                            id: id.clone(),
                            message_id: message.message_id.clone(),
                            image_index,
                        });
                        Some(id)
                    })
                } else {
                    None
                };
                format_history_media(media, image_id.as_deref())
            })
            .collect::<Vec<_>>();
        let sender = if message.is_bot {
            "[你]".to_string()
        } else if show_user_ids {
            format!(
                "{}(QQ:{})",
                safe_prompt_field(&message.sender_name),
                safe_prompt_field(&message.sender_id)
            )
        } else {
            safe_prompt_field(&message.sender_name)
        };
        let mut content = truncate_utf8(message.content.text.trim(), 4_096).to_string();
        if !media.is_empty() {
            if !content.is_empty() {
                content.push(' ');
            }
            content.push_str(&media.join(" "));
        }
        if content.is_empty() {
            content.push_str("[无文字内容]");
        }
        let mut line = format!(
            "[{}] {} [msg={}]: {}",
            format_history_time(message.sent_at),
            sender,
            safe_prompt_field(&message.message_id),
            safe_prompt_field(&content)
        );
        if let Some(reply_to) = message.reply_to_message_id.as_ref() {
            line.push_str(&format!(
                "\n  回复引用: msg={}",
                safe_prompt_field(reply_to)
            ));
        }
        if let Some(mentions) = format_mentioned_users(
            &message.content.mentioned_users,
            &message.content.mentioned_user_ids,
            show_user_ids,
        ) {
            line.push_str(&format!("\n  @对象: {mentions}"));
        }
        line.push('\n');
        if used_bytes.saturating_add(line.len()) > maximum_bytes {
            images.truncate(images_before);
            for source in added_sources {
                source_ids.remove(&source);
            }
            break;
        }
        used_bytes += line.len();
        lines.push(line);
        if stop_when_images_full && images.len() >= maximum_images {
            // 图片集合已定格:上限过滤保证之后的消息不可能再改动 images,
            // 只收图的调用者无需继续陪跑剩余文本渲染。预算先触底、收满先
            // 发生、两者都不发生三种情形的 .images 输出均与全量渲染一致
            // (有对拍测试)。
            break;
        }
    }
    let message_count = lines.len();
    lines.reverse();
    FormattedHistory {
        text: lines.concat().trim_end().to_string(),
        images,
        message_count,
    }
}

pub(crate) fn format_history_media(media: &MediaPlaceholder, image_id: Option<&str>) -> String {
    match (image_id, media.label.as_deref()) {
        (Some(id), Some(label)) => format!(
            "[{} id={}, label={}]",
            media_label(media.kind),
            id,
            safe_prompt_field(label)
        ),
        (Some(id), None) => format!("[{} id={}]", media_label(media.kind), id),
        (None, Some(label)) => format!(
            "[{}: {}]",
            media_label(media.kind),
            safe_prompt_field(label)
        ),
        (None, None) => format!("[{}]", media_label(media.kind)),
    }
}

pub(crate) fn format_history_time(timestamp: i64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp(timestamp, 0)
        .map(|time| {
            time.with_timezone(&chrono::Local)
                .format("%H:%M:%S")
                .to_string()
        })
        .unwrap_or_else(|| timestamp.to_string())
}

pub(crate) fn media_label(kind: MediaKind) -> &'static str {
    match kind {
        MediaKind::Image => "图片",
        MediaKind::Sticker => "表情",
        MediaKind::File => "文件",
        MediaKind::Audio => "语音",
        MediaKind::Video => "视频",
        MediaKind::Other => "媒体",
    }
}

pub(crate) fn outbound_text(message: &OutboundMessage) -> String {
    let mut parts = Vec::new();
    match &message.body {
        OutboundBody::Segments(segments) => append_segment_text(&mut parts, segments),
        OutboundBody::Forward(nodes) => {
            for node in nodes {
                append_segment_text(&mut parts, &node.segments);
            }
        }
    }
    parts.join("\n").trim().to_string()
}

pub(crate) fn append_segment_text(parts: &mut Vec<String>, segments: &[OutboundSegment]) {
    for segment in segments {
        match segment {
            OutboundSegment::Markdown(text) | OutboundSegment::Text(text) => {
                if !text.trim().is_empty() {
                    parts.push(text.clone());
                }
            }
            OutboundSegment::Mention(user_id) => parts.push(format!("@{user_id}")),
            _ => {}
        }
    }
}

pub(crate) fn truncate_utf8(value: &str, maximum_bytes: usize) -> &str {
    if value.len() <= maximum_bytes {
        return value;
    }
    let mut end = maximum_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

pub(crate) fn truncate_utf8_tail(value: &str, maximum_bytes: usize) -> &str {
    if value.len() <= maximum_bytes {
        return value;
    }
    let mut start = value.len().saturating_sub(maximum_bytes);
    while !value.is_char_boundary(start) {
        start += 1;
    }
    &value[start..]
}

pub(crate) fn normalized_timestamp(timestamp: i64) -> i64 {
    if timestamp > 0 {
        timestamp
    } else {
        now_unix()
    }
}

pub(crate) fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
