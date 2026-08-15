//! assets — 自 src/web.rs 拆分。

pub(crate) use super::*;

impl From<ArtifactAsset> for SafeArtifactAsset {
    fn from(asset: ArtifactAsset) -> Self {
        Self {
            url: format!("/api/artifacts/{}", asset.asset_id),
            id: asset.asset_id,
            name: asset.file_name,
            mime: asset.mime,
            kind: asset.kind,
            type_label: artifact_type_label(&asset.source_key),
            size: asset.size_bytes,
            updated_at: asset.updated_at,
        }
    }
}

pub(crate) fn artifact_type_label(source_key: &str) -> String {
    let extension = FilePath::new(source_key)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_uppercase();
    match extension.as_str() {
        "MARKDOWN" => "MD".to_string(),
        "HTML" | "HTM" => "HTML".to_string(),
        "JSONL" => "JSONL".to_string(),
        "JSON" => "JSON".to_string(),
        "PDF" => "PDF".to_string(),
        value if value.len() <= 6 && !value.is_empty() => value.to_string(),
        _ => "FILE".to_string(),
    }
}

impl SafeImageAsset {
    pub(crate) fn from_asset(asset: ImageAsset, hide_caption: bool) -> Self {
        Self {
            url: format!("/api/assets/{}", asset.asset_id),
            id: asset.asset_id,
            mime: asset.mime,
            width: asset.width,
            height: asset.height,
            alt: asset.alt,
            hide_caption,
        }
    }
}

impl From<ImageAsset> for SafeImageAsset {
    fn from(asset: ImageAsset) -> Self {
        Self::from_asset(asset, false)
    }
}

pub(crate) fn meme_asset_caption_hidden(asset: &ImageAsset, reports: &[String]) -> bool {
    pub(crate) const MAX_DESCRIPTION_CHARS: usize = 120;

    let description = asset.alt.split_whitespace().collect::<Vec<_>>().join(" ");
    if description.is_empty() {
        return false;
    }
    let mut characters = description.chars();
    let mut compact = characters
        .by_ref()
        .take(MAX_DESCRIPTION_CHARS)
        .collect::<String>();
    if characters.next().is_some() {
        compact.push('…');
    }
    let escaped = compact
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    let marker = format!("description={escaped}</sent_meme>");
    reports
        .iter()
        .any(|report| report.starts_with("<sent_meme>") && report.contains(&marker))
}

impl From<TurnFollowup> for SafeFollowup {
    fn from(followup: TurnFollowup) -> Self {
        Self {
            id: followup.prompt_id,
            content: followup.display_content,
            submitted_at: followup.submitted_at,
            preceding_assistant_content: followup
                .preceding_assistant_content
                .map(|content| redact_internal_assistant_text(&content)),
            preceding_assistant_reasoning: followup
                .preceding_assistant_reasoning
                .map(|reasoning| redact_internal_assistant_text(&reasoning)),
            provider_id: followup.preceding_assistant_provider_id,
            model: followup.preceding_assistant_model,
            attachments: followup
                .uploaded_attachments
                .into_iter()
                .map(SafeUserAttachment::from)
                .collect(),
        }
    }
}

impl From<QueuedPrompt> for SafeQueuedPrompt {
    fn from(prompt: QueuedPrompt) -> Self {
        Self {
            id: prompt.prompt_id,
            content: prompt.display_content,
            submitted_at: prompt.submitted_at,
            attachments: prompt
                .uploaded_attachments
                .into_iter()
                .map(SafeUserAttachment::from)
                .collect(),
        }
    }
}

impl From<UserAttachment> for SafeUserAttachment {
    fn from(attachment: UserAttachment) -> Self {
        Self {
            url: format!("/api/attachments/{}", attachment.attachment_id),
            id: attachment.attachment_id,
            name: attachment.file_name,
            mime: attachment.mime,
            kind: attachment.kind,
            size: attachment.size_bytes,
            width: attachment.width,
            height: attachment.height,
        }
    }
}

impl From<UsageSnapshot> for SafeUsageSnapshot {
    fn from(usage: UsageSnapshot) -> Self {
        Self {
            requests: usage.requests,
            prompt_tokens: usage.prompt_tokens,
            completion_tokens: usage.completion_tokens,
            total_tokens: usage.total_tokens,
            conversation_tokens: usage.conversation_tokens,
            cache_read_tokens: usage.cache_read_tokens,
            cache_write_tokens: usage.cache_write_tokens,
            reasoning_tokens: usage.reasoning_tokens,
            last_usage: usage.last_usage,
            last_conversation_usage: usage.last_conversation_usage,
        }
    }
}

pub(crate) fn redact_internal_assistant_text(value: &str) -> String {
    value
        .replace(crate::state::pending_placeholder(), "")
        .replace(crate::state::interrupted_text(), "")
}

pub(crate) fn normalize_answers(
    request: &QuestionRequest,
    mut answers: QuestionAnswers,
) -> std::result::Result<QuestionAnswers, String> {
    for answer in &mut answers {
        for value in answer {
            *value = value.trim().to_string();
            if value.chars().any(char::is_control) {
                return Err("answers cannot contain control characters".to_string());
            }
        }
    }
    question::validate_answers(request, &answers).map_err(|error| safe_error_message(&error))?;
    Ok(answers)
}

pub(crate) fn validate_content(content: String) -> std::result::Result<String, ApiError> {
    let content = content.trim().to_string();
    if content.is_empty() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "content cannot be empty",
        ));
    }
    if content.chars().count() > MAX_CONTENT_CHARS {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            format!("content cannot exceed {MAX_CONTENT_CHARS} characters"),
        ));
    }
    if content
        .chars()
        .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
    {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "content contains unsupported control characters",
        ));
    }
    Ok(content)
}

pub(crate) fn validate_short_field(
    value: String,
    field: &str,
    max_chars: usize,
) -> std::result::Result<String, ApiError> {
    let value = value.trim().to_string();
    if value.is_empty() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            format!("{field} cannot be empty"),
        ));
    }
    if value.chars().count() > max_chars || value.chars().any(char::is_control) {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            format!("{field} is invalid"),
        ));
    }
    Ok(value)
}

pub(crate) fn validate_model_selection(
    models: Vec<ActiveProviderModelConfig>,
) -> std::result::Result<Vec<ActiveProviderModelConfig>, ApiError> {
    if models.is_empty() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "at least one model endpoint must remain active",
        ));
    }
    if models.len() > 64 {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "at most 64 model endpoints can be active",
        ));
    }
    let mut seen = HashSet::with_capacity(models.len());
    let mut validated = Vec::with_capacity(models.len());
    for model in models {
        let provider_id = validate_short_field(model.provider_id, "provider_id", 200)?;
        let model = validate_short_field(model.model, "model", 500)?;
        if !seen.insert((provider_id.clone(), model.clone())) {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "duplicate provider/model selection",
            ));
        }
        validated.push(ActiveProviderModelConfig { provider_id, model });
    }
    Ok(validated)
}

pub(crate) fn validate_thinking_variant_updates(
    updates: Vec<ThinkingVariantUpdate>,
) -> std::result::Result<Vec<ThinkingVariantUpdate>, ApiError> {
    if updates.is_empty() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "at least one thinking variant update is required",
        ));
    }
    if updates.len() > MAX_THINKING_VARIANT_UPDATES {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            format!("at most {MAX_THINKING_VARIANT_UPDATES} thinking variants can be updated"),
        ));
    }

    let mut seen = HashSet::with_capacity(updates.len());
    let mut validated = Vec::with_capacity(updates.len());
    for update in updates {
        let provider_id = validate_short_field(update.provider_id, "provider_id", 200)?;
        let model = validate_short_field(update.model, "model", 500)?;
        let selected = update
            .selected
            .map(|selected| validate_short_field(selected, "selected", 200))
            .transpose()?;
        if !seen.insert((provider_id.clone(), model.clone())) {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "duplicate provider/model thinking variant update",
            ));
        }
        validated.push(ThinkingVariantUpdate {
            provider_id,
            model,
            selected,
        });
    }
    Ok(validated)
}

pub(crate) fn parse_mode(mode: &str) -> std::result::Result<AgentMode, ApiError> {
    match mode {
        "normal" => Ok(AgentMode::Normal),
        // 历史会话可能存过 plan：模式已移除，回落到普通模式而不是让会话打不开。
        "plan" => Ok(AgentMode::Normal),
        // 闲聊模式已删除:历史会话存过 "chat" 的回落普通模式,老会话照常打开。
        "chat" => Ok(AgentMode::Normal),
        "dev" => Ok(AgentMode::Dev),
        _ => Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "mode must be normal or dev",
        )),
    }
}

pub(crate) fn mode_name(mode: AgentMode) -> &'static str {
    match mode {
        AgentMode::Normal => "normal",
        AgentMode::Dev => "dev",
    }
}

pub(crate) fn is_local_webui_request(audience: PromptAudience, has_turn_profile: bool) -> bool {
    audience == PromptAudience::External && !has_turn_profile
}

pub(crate) fn real_tool_name(event_name: &str) -> &str {
    if event_name.starts_with("load_skill:") {
        "load_skill"
    } else if event_name.starts_with("load_tools:") {
        "load_tools"
    } else {
        event_name
    }
}

pub(crate) fn require_auth(
    headers: &HeaderMap,
    state: &DaemonState,
) -> std::result::Result<(), ApiError> {
    if state
        .auth
        .is_authenticated(cookie_value(headers, AUTH_COOKIE))
    {
        Ok(())
    } else {
        Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "authentication required",
        ))
    }
}

pub(crate) fn require_mutation(
    headers: &HeaderMap,
    state: &DaemonState,
) -> std::result::Result<(), ApiError> {
    require_auth(headers, state)?;
    if origin_is_allowed(headers) {
        Ok(())
    } else {
        Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "request origin is not allowed",
        ))
    }
}

pub(crate) fn cookie_value<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    for header in headers.get_all(COOKIE) {
        let Ok(header) = header.to_str() else {
            continue;
        };
        for pair in header.split(';') {
            let Some((key, value)) = pair.trim().split_once('=') else {
                continue;
            };
            if key.trim() == name {
                return Some(value.trim());
            }
        }
    }
    None
}

pub(crate) fn origin_is_allowed(headers: &HeaderMap) -> bool {
    let mut origins = headers.get_all(ORIGIN).iter();
    let Some(origin) = origins.next() else {
        return true;
    };
    if origins.next().is_some() {
        return false;
    }
    let Some(host) = headers.get(HOST).and_then(|host| host.to_str().ok()) else {
        return false;
    };
    let expected = format!("http://{host}");
    origin.to_str().is_ok_and(|origin| origin == expected)
}

pub(crate) fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let mut difference = left.len() ^ right.len();
    let length = left.len().max(right.len());
    for index in 0..length {
        let left = left.get(index).copied().unwrap_or(0);
        let right = right.get(index).copied().unwrap_or(0);
        difference |= usize::from(left ^ right);
    }
    difference == 0
}

pub(crate) fn random_token(bytes: usize) -> String {
    let mut buffer = vec![0u8; bytes];
    OsRng.fill_bytes(&mut buffer);
    URL_SAFE_NO_PAD.encode(buffer)
}

pub(crate) fn random_id(prefix: &str, bytes: usize) -> String {
    format!("{prefix}_{}", random_token(bytes))
}

pub(crate) fn safe_error_message(error: impl std::fmt::Display) -> String {
    let message = error
        .to_string()
        .chars()
        .filter(|character| !character.is_control() || *character == '\n' || *character == '\t')
        .take(1000)
        .collect::<String>();
    if message.trim().is_empty() {
        "operation failed".to_string()
    } else {
        message
    }
}

pub(crate) async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}
