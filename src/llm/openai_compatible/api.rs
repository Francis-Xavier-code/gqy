//! api — 自 src/llm/openai_compatible.rs 拆分。

pub(crate) use super::*;

pub(crate) fn strip_tagged_sections(mut text: String, tag: &str) -> String {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let open_prefix = format!("<{tag}");
    loop {
        let Some(start) = text.find(&open_prefix) else {
            break;
        };
        let content_start = text[start..]
            .find('>')
            .map(|offset| start + offset + 1)
            .unwrap_or(start + open.len());
        let Some(relative_end) = text[content_start..].find(&close) else {
            text.replace_range(start.., "");
            break;
        };
        let end = content_start + relative_end + close.len();
        text.replace_range(start..end, "");
    }
    text
}
#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn handle_responses_sse_line<F>(
    line: &str,
    content: &mut String,
    content_emitted: &mut usize,
    reasoning: &mut String,
    reasoning_emitted: &mut usize,
    reasoning_part_active: &mut bool,
    usage: &mut Option<Usage>,
    content_started: &mut bool,
    output_text_delta_parts: &mut HashSet<(String, usize)>,
    refusal_delta_parts: &mut HashSet<(String, usize)>,
    response_id: &mut Option<String>,
    tool_calls: &mut ResponsesToolAccumulator,
    on_chunk: &mut F,
) -> Result<bool>
where
    F: FnMut(ChatStreamChunk) -> Result<()>,
{
    let Some(data) = line.strip_prefix("data:").map(str::trim) else {
        return Ok(false);
    };
    if data == "[DONE]" {
        flush_buffer(
            reasoning,
            reasoning_emitted,
            ChatStreamKind::Reasoning,
            on_chunk,
            true,
        )?;
        if *reasoning_part_active {
            on_chunk(ChatStreamChunk {
                kind: ChatStreamKind::ReasoningPartEnd,
                text: String::new(),
            })?;
            *reasoning_part_active = false;
        }
        flush_buffer(
            content,
            content_emitted,
            ChatStreamKind::Content,
            on_chunk,
            true,
        )?;
        return Ok(true);
    }
    let event: ResponsesStreamEvent = serde_json::from_str(data).with_context(|| {
        format!(
            "{}: {}",
            t(
                "invalid responses stream event",
                "无效的 Responses 流式事件"
            ),
            clean_plain_text(data.to_string())
        )
    })?;
    if let Some(id) = event
        .response
        .as_ref()
        .and_then(|response| response.id.as_deref())
        .filter(|id| !id.trim().is_empty())
    {
        *response_id = Some(id.to_string());
    }
    if event.kind.starts_with("response.reasoning")
        || matches!(
            event.kind.as_str(),
            "response.output_item.added" | "response.completed" | "response.incomplete"
        )
    {
        let item_kind = event.item.as_ref().map(|item| item.kind.as_str());
        let delta_chars = event.delta.as_deref().map(|delta| delta.chars().count());
        let reasoning_tokens = event
            .response
            .as_ref()
            .and_then(|response| response.usage.as_ref())
            .and_then(|usage| usage.output_tokens_details.as_ref())
            .and_then(|details| details.reasoning_tokens);
        tracing::debug!(
            event_type = %event.kind,
            item_kind = ?item_kind,
            delta_chars = ?delta_chars,
            reasoning_tokens = ?reasoning_tokens,
            "{}",
            t("Responses stream milestone", "Responses 流关键节点")
        );
    }
    let content_part_key = (
        event.item_id.clone().unwrap_or_default(),
        event.content_index.unwrap_or_default(),
    );
    match event.kind.as_str() {
        "response.output_text.delta"
        | "response.output_text.done"
        | "response.refusal.delta"
        | "response.refusal.done" => {
            let text = match event.kind.as_str() {
                "response.output_text.delta" => {
                    let text = event.delta.unwrap_or_default();
                    if !text.is_empty() {
                        output_text_delta_parts.insert(content_part_key.clone());
                    }
                    text
                }
                "response.output_text.done"
                    if !output_text_delta_parts.contains(&content_part_key) =>
                {
                    event.text.unwrap_or_default()
                }
                "response.output_text.done" => String::new(),
                "response.refusal.delta" => {
                    let text = event.delta.unwrap_or_default();
                    if !text.is_empty() {
                        refusal_delta_parts.insert(content_part_key.clone());
                    }
                    text
                }
                "response.refusal.done" if !refusal_delta_parts.contains(&content_part_key) => {
                    event.refusal.unwrap_or_default()
                }
                "response.refusal.done" => String::new(),
                _ => String::new(),
            };
            if text.is_empty() {
                return Ok(false);
            }
            if *reasoning_part_active {
                flush_buffer(
                    reasoning,
                    reasoning_emitted,
                    ChatStreamKind::Reasoning,
                    on_chunk,
                    true,
                )?;
                on_chunk(ChatStreamChunk {
                    kind: ChatStreamKind::ReasoningPartEnd,
                    text: String::new(),
                })?;
                *reasoning_part_active = false;
            }
            *content_started = true;
            push_buffered_chunk(
                content,
                content_emitted,
                ChatStreamKind::Content,
                text,
                on_chunk,
            )?;
        }
        "response.reasoning_text.delta"
        | "response.reasoning_summary.delta"
        | "response.reasoning_summary_text.delta" => {
            if let Some(text) = event.delta {
                if !*reasoning_part_active {
                    if !reasoning.is_empty() && !reasoning.ends_with("\n\n") {
                        reasoning.push_str("\n\n");
                    }
                    on_chunk(ChatStreamChunk {
                        kind: ChatStreamKind::ReasoningPartStart,
                        text: String::new(),
                    })?;
                    *reasoning_part_active = true;
                }
                push_buffered_chunk(
                    reasoning,
                    reasoning_emitted,
                    ChatStreamKind::Reasoning,
                    text,
                    on_chunk,
                )?;
            }
        }
        "response.reasoning_text.done"
        | "response.reasoning_summary.done"
        | "response.reasoning_summary_text.done" => {
            flush_buffer(
                reasoning,
                reasoning_emitted,
                ChatStreamKind::Reasoning,
                on_chunk,
                true,
            )?;
            if *reasoning_part_active {
                on_chunk(ChatStreamChunk {
                    kind: ChatStreamKind::ReasoningPartEnd,
                    text: String::new(),
                })?;
                *reasoning_part_active = false;
            }
            if !*content_started && !reasoning.trim().is_empty() {
                *content_started = true;
                on_chunk(ChatStreamChunk {
                    kind: ChatStreamKind::Content,
                    text: String::new(),
                })?;
            }
        }
        "response.output_item.added" => {
            if *reasoning_part_active {
                flush_buffer(
                    reasoning,
                    reasoning_emitted,
                    ChatStreamKind::Reasoning,
                    on_chunk,
                    true,
                )?;
                on_chunk(ChatStreamChunk {
                    kind: ChatStreamKind::ReasoningPartEnd,
                    text: String::new(),
                })?;
                *reasoning_part_active = false;
            }
            if let Some(item) = event.item {
                if let Some(name) = tool_calls.start(item) {
                    on_chunk(ChatStreamChunk {
                        kind: ChatStreamKind::ToolCall,
                        text: name,
                    })?;
                }
            }
        }
        "response.reasoning_summary_part.added" => {
            if *reasoning_part_active {
                flush_buffer(
                    reasoning,
                    reasoning_emitted,
                    ChatStreamKind::Reasoning,
                    on_chunk,
                    true,
                )?;
                on_chunk(ChatStreamChunk {
                    kind: ChatStreamKind::ReasoningPartEnd,
                    text: String::new(),
                })?;
            }
            if !reasoning.is_empty() && !reasoning.ends_with("\n\n") {
                reasoning.push_str("\n\n");
            }
            on_chunk(ChatStreamChunk {
                kind: ChatStreamKind::ReasoningPartStart,
                text: String::new(),
            })?;
            *reasoning_part_active = true;
        }
        "response.reasoning_summary_part.done" => {
            flush_buffer(
                reasoning,
                reasoning_emitted,
                ChatStreamKind::Reasoning,
                on_chunk,
                true,
            )?;
            if *reasoning_part_active {
                on_chunk(ChatStreamChunk {
                    kind: ChatStreamKind::ReasoningPartEnd,
                    text: String::new(),
                })?;
                *reasoning_part_active = false;
            }
        }
        "response.function_call_arguments.delta" => {
            if let Some(delta) = event.delta {
                tool_calls.append_arguments(event.item_id, delta);
            }
        }
        "response.function_call_arguments.done" => {
            tool_calls.finish_arguments(event.item_id, event.name, event.arguments);
        }
        "response.output_item.done" => {
            if let Some(item) = event.item {
                tool_calls.finish_item(item);
            }
        }
        "response.completed" => {
            if let Some(next_usage) = event.response.and_then(|response| response.usage) {
                let total_tokens = if next_usage.total_tokens > 0 {
                    next_usage.total_tokens
                } else {
                    next_usage
                        .input_tokens
                        .saturating_add(next_usage.output_tokens)
                };
                let input_details = next_usage.input_tokens_details.as_ref();
                let cache_read = input_details.and_then(|details| details.cached_tokens);
                let cache_write = input_details.and_then(|details| details.cache_write_tokens);
                let reasoning_tokens = next_usage
                    .output_tokens_details
                    .as_ref()
                    .and_then(|details| details.reasoning_tokens)
                    .unwrap_or(0);
                *usage = Some(Usage {
                    prompt_tokens: next_usage.input_tokens,
                    completion_tokens: next_usage.output_tokens,
                    total_tokens,
                    cache_read_tokens: cache_read.unwrap_or(0),
                    cache_write_tokens: cache_write.unwrap_or(0),
                    reasoning_tokens,
                    cache_reported: cache_read.is_some() || cache_write.is_some(),
                    ..Usage::default()
                });
            }
            flush_buffer(
                reasoning,
                reasoning_emitted,
                ChatStreamKind::Reasoning,
                on_chunk,
                true,
            )?;
            if *reasoning_part_active {
                on_chunk(ChatStreamChunk {
                    kind: ChatStreamKind::ReasoningPartEnd,
                    text: String::new(),
                })?;
                *reasoning_part_active = false;
            }
            flush_buffer(
                content,
                content_emitted,
                ChatStreamKind::Content,
                on_chunk,
                true,
            )?;
            return Ok(true);
        }
        "response.incomplete" => {
            let reason = event
                .response
                .as_ref()
                .and_then(|response| response.incomplete_details.as_ref())
                .and_then(|details| details.reason.as_deref())
                .unwrap_or("unknown");
            bail!("OpenAI Responses response was incomplete: {reason}");
        }
        "error" | "response.failed" => {
            bail!(
                "OpenAI Responses stream failed: {}",
                clean_plain_text(data.to_string())
            );
        }
        _ => {}
    }
    Ok(false)
}

pub(crate) fn handle_anthropic_sse_data<F>(
    data: &str,
    state: &mut AnthropicStreamState,
    on_chunk: &mut F,
) -> Result<bool>
where
    F: FnMut(ChatStreamChunk) -> Result<()>,
{
    if data == "[DONE]" {
        flush_anthropic_state(state, on_chunk)?;
        return Ok(true);
    }
    let event: AnthropicStreamEvent = serde_json::from_str(data).with_context(|| {
        format!(
            "{}: {}",
            t(
                "invalid anthropic messages stream event",
                "无效的 Anthropic Messages 流式事件"
            ),
            clean_plain_text(data.to_string())
        )
    })?;
    match event.kind.as_str() {
        "message_start" => {
            if let Some(usage) = event.message.and_then(|message| message.usage) {
                merge_anthropic_usage(&mut state.usage, usage);
            }
        }
        "content_block_start" => {
            if let Some(block) = event.content_block {
                match block.kind.as_str() {
                    "tool_use" | "server_tool_use" => {
                        if let Some(index) = event.index {
                            if let Some(name) = state.tool_calls.start(index, block) {
                                on_chunk(ChatStreamChunk {
                                    kind: ChatStreamKind::ToolCall,
                                    text: name,
                                })?;
                            }
                        }
                    }
                    "text" => {
                        if state.reasoning_part_active {
                            flush_buffer(
                                &mut state.reasoning,
                                &mut state.reasoning_emitted,
                                ChatStreamKind::Reasoning,
                                on_chunk,
                                true,
                            )?;
                            on_chunk(ChatStreamChunk {
                                kind: ChatStreamKind::ReasoningPartEnd,
                                text: String::new(),
                            })?;
                            state.reasoning_part_active = false;
                        }
                        if let Some(text) = block.text {
                            push_buffered_chunk(
                                &mut state.content,
                                &mut state.content_emitted,
                                ChatStreamKind::Content,
                                text,
                                on_chunk,
                            )?;
                        }
                    }
                    "thinking" => {
                        if state.reasoning_part_active {
                            flush_buffer(
                                &mut state.reasoning,
                                &mut state.reasoning_emitted,
                                ChatStreamKind::Reasoning,
                                on_chunk,
                                true,
                            )?;
                            on_chunk(ChatStreamChunk {
                                kind: ChatStreamKind::ReasoningPartEnd,
                                text: String::new(),
                            })?;
                        }
                        if !state.reasoning.is_empty() && !state.reasoning.ends_with("\n\n") {
                            state.reasoning.push_str("\n\n");
                        }
                        on_chunk(ChatStreamChunk {
                            kind: ChatStreamKind::ReasoningPartStart,
                            text: String::new(),
                        })?;
                        state.reasoning_part_active = true;
                        if let Some(text) = block.thinking {
                            push_buffered_chunk(
                                &mut state.reasoning,
                                &mut state.reasoning_emitted,
                                ChatStreamKind::Reasoning,
                                text,
                                on_chunk,
                            )?;
                        }
                    }
                    _ => {}
                }
            }
        }
        "content_block_delta" => {
            if let Some(delta) = event.delta {
                match delta.kind.as_deref() {
                    Some("text_delta") => {
                        if state.reasoning_part_active {
                            flush_buffer(
                                &mut state.reasoning,
                                &mut state.reasoning_emitted,
                                ChatStreamKind::Reasoning,
                                on_chunk,
                                true,
                            )?;
                            on_chunk(ChatStreamChunk {
                                kind: ChatStreamKind::ReasoningPartEnd,
                                text: String::new(),
                            })?;
                            state.reasoning_part_active = false;
                        }
                        if let Some(text) = delta.text {
                            push_buffered_chunk(
                                &mut state.content,
                                &mut state.content_emitted,
                                ChatStreamKind::Content,
                                text,
                                on_chunk,
                            )?;
                        }
                    }
                    Some("thinking_delta") => {
                        if let Some(text) = delta.thinking {
                            if !state.reasoning_part_active {
                                if !state.reasoning.is_empty() && !state.reasoning.ends_with("\n\n")
                                {
                                    state.reasoning.push_str("\n\n");
                                }
                                on_chunk(ChatStreamChunk {
                                    kind: ChatStreamKind::ReasoningPartStart,
                                    text: String::new(),
                                })?;
                                state.reasoning_part_active = true;
                            }
                            push_buffered_chunk(
                                &mut state.reasoning,
                                &mut state.reasoning_emitted,
                                ChatStreamKind::Reasoning,
                                text,
                                on_chunk,
                            )?;
                        }
                    }
                    Some("input_json_delta") => {
                        if let (Some(index), Some(text)) = (event.index, delta.partial_json) {
                            state.tool_calls.append_arguments(index, text);
                        }
                    }
                    Some("signature_delta") => {
                        state.thinking_signature = delta.signature;
                    }
                    _ => {}
                }
            }
        }
        "content_block_stop" => {
            if state.reasoning_part_active {
                flush_buffer(
                    &mut state.reasoning,
                    &mut state.reasoning_emitted,
                    ChatStreamKind::Reasoning,
                    on_chunk,
                    true,
                )?;
                on_chunk(ChatStreamChunk {
                    kind: ChatStreamKind::ReasoningPartEnd,
                    text: String::new(),
                })?;
                state.reasoning_part_active = false;
            }
        }
        "message_delta" => {
            if let Some(usage) = event.usage {
                merge_anthropic_usage(&mut state.usage, usage);
            }
            flush_anthropic_state(state, on_chunk)?;
        }
        "message_stop" => {
            flush_anthropic_state(state, on_chunk)?;
            return Ok(true);
        }
        "error" => {
            let message = event
                .error
                .map(|error| match (error.kind, error.message) {
                    (Some(kind), Some(message)) => format!("{kind}: {message}"),
                    (Some(kind), None) => kind,
                    (None, Some(message)) => message,
                    (None, None) => "Anthropic Messages stream error".to_string(),
                })
                .unwrap_or_else(|| "Anthropic Messages stream error".to_string());
            bail!("{message}");
        }
        _ => {}
    }
    Ok(false)
}

pub(crate) fn flush_anthropic_state<F>(
    state: &mut AnthropicStreamState,
    on_chunk: &mut F,
) -> Result<()>
where
    F: FnMut(ChatStreamChunk) -> Result<()>,
{
    flush_buffer(
        &state.reasoning,
        &mut state.reasoning_emitted,
        ChatStreamKind::Reasoning,
        on_chunk,
        true,
    )?;
    if state.reasoning_part_active {
        on_chunk(ChatStreamChunk {
            kind: ChatStreamKind::ReasoningPartEnd,
            text: String::new(),
        })?;
        state.reasoning_part_active = false;
    }
    flush_buffer(
        &state.content,
        &mut state.content_emitted,
        ChatStreamKind::Content,
        on_chunk,
        true,
    )
}

pub(crate) fn merge_anthropic_usage(current: &mut Option<Usage>, usage: AnthropicUsage) {
    let previous = current.take().unwrap_or_default();
    let cache_read = usage
        .cache_read_input_tokens
        .unwrap_or(previous.cache_read_tokens);
    let cache_write = usage
        .cache_creation_input_tokens
        .unwrap_or(previous.cache_write_tokens);
    // Anthropic's `input_tokens` excludes both cache reads and cache writes;
    // normalize to the cross-provider invariant `prompt = uncached + read + write`
    // so context accounting does not collapse once cache_control is in play.
    let prompt_tokens = if usage.input_tokens > 0 || cache_read > 0 || cache_write > 0 {
        usage
            .input_tokens
            .saturating_add(cache_read)
            .saturating_add(cache_write)
    } else {
        previous.prompt_tokens
    };
    let completion_tokens = if usage.output_tokens > 0 {
        usage.output_tokens
    } else {
        previous.completion_tokens
    };
    *current = Some(Usage {
        prompt_tokens,
        completion_tokens,
        total_tokens: prompt_tokens.saturating_add(completion_tokens),
        cache_read_tokens: cache_read,
        cache_write_tokens: cache_write,
        cache_reported: previous.cache_reported
            || usage.cache_read_input_tokens.is_some()
            || usage.cache_creation_input_tokens.is_some(),
        reasoning_tokens: previous.reasoning_tokens,
        ..Usage::default()
    });
}

pub(crate) fn delta_reasoning_text(delta: &ChatChoiceMessage) -> Option<String> {
    delta
        .reasoning_content
        .clone()
        .or_else(|| delta.reasoning.clone())
        .or_else(|| delta.thinking.clone())
        .or_else(|| delta.thinking_content.clone())
        .or_else(|| delta.reasoning_text.clone())
        .or_else(|| reasoning_details_text(delta.reasoning_details.as_ref()))
}

pub(crate) fn reasoning_details_text(value: Option<&serde_json::Value>) -> Option<String> {
    let value = value?;
    if let Some(text) = value.as_str() {
        return Some(text.to_string());
    }
    if let Some(array) = value.as_array() {
        let text = array
            .iter()
            .filter_map(|item| {
                item.get("text")
                    .or_else(|| item.get("content"))
                    .and_then(serde_json::Value::as_str)
            })
            .collect::<Vec<_>>()
            .join("");
        return (!text.is_empty()).then_some(text);
    }
    value
        .get("text")
        .or_else(|| value.get("content"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
}

pub(crate) fn push_buffered_chunk<F>(
    target: &mut String,
    emitted: &mut usize,
    kind: ChatStreamKind,
    text: String,
    on_chunk: &mut F,
) -> Result<()>
where
    F: FnMut(ChatStreamChunk) -> Result<()>,
{
    if text.is_empty() {
        return Ok(());
    }
    target.push_str(&text);
    flush_buffer(target, emitted, kind, on_chunk, false)
}

pub(crate) fn flush_buffer<F>(
    target: &str,
    emitted: &mut usize,
    kind: ChatStreamKind,
    on_chunk: &mut F,
    final_flush: bool,
) -> Result<()>
where
    F: FnMut(ChatStreamChunk) -> Result<()>,
{
    while *emitted < target.len() {
        let remaining = &target[*emitted..];
        if starts_hidden_prefix(remaining) {
            if let Some(end) = hidden_end_after(target, *emitted) {
                *emitted = end;
                continue;
            }
            if final_flush {
                *emitted = target.len();
            }
            return Ok(());
        }
        let hidden_start = hidden_start_after(target, *emitted);
        let mut safe_end = hidden_start.unwrap_or(target.len());
        if hidden_start.is_none() && !final_flush {
            safe_end =
                safe_end.saturating_sub(partial_hidden_suffix_len(&target[*emitted..safe_end]));
        }
        if safe_end <= *emitted {
            return Ok(());
        }
        let text = target[*emitted..safe_end].to_string();
        *emitted = safe_end;
        if !text.is_empty() {
            on_chunk(ChatStreamChunk { kind, text })?;
        }
    }
    Ok(())
}
#[allow(clippy::too_many_arguments)]

#[allow(clippy::too_many_arguments)]
pub(crate) fn finalize_responses_stream_result(
    content: String,
    reasoning: String,
    usage: Option<Usage>,
    tool_calls: Vec<ToolCall>,
    dsml_enabled: bool,
    response_id: Option<String>,
    store_disabled: bool,
    continuation_unsupported: bool,
) -> Result<ChatResult> {
    let mut result = finalize_stream_result(content, reasoning, usage, tool_calls, dsml_enabled)?;
    if result.tool_calls.is_empty() {
        return Ok(result);
    }
    // 该端点续传已被记为不可用(能力记录/自愈置位):不设 continuation,
    // 工具轮走无状态全量回放(lower_responses_messages 重放
    // function_call/function_call_output 是完整配对的)。
    if continuation_unsupported {
        return Ok(result);
    }
    if store_disabled {
        bail!(
            "OpenAI Responses returned tool calls, but store=false prevents stateful continuation"
        );
    }
    let response_id = response_id
        .filter(|id| !id.trim().is_empty())
        .context("OpenAI Responses returned tool calls without a response ID")?;
    result.responses_continuation = Some(Box::new(ResponsesContinuation {
        response_id,
        endpoint_id: String::new(),
    }));
    Ok(result)
}

pub(crate) fn finalize_stream_result(
    content: String,
    reasoning: String,
    usage: Option<Usage>,
    tool_calls: Vec<ToolCall>,
    dsml_enabled: bool,
) -> Result<ChatResult> {
    let usage = usage.map(|mut usage| {
        usage.normalize_cache_fields();
        if usage.cache_reported {
            // v7 Release 1 observability: one absolute-value line per request,
            // à la Reasonix ("in N (M cached / K new)"). Percentages mislead
            // when a turn adds lots of fresh content, so none are shown.
            tracing::info!(
                prompt_tokens = usage.prompt_tokens,
                cache_read = usage.cache_read_tokens,
                cache_write = usage.cache_write_tokens,
                fresh = usage.uncached_prompt_tokens(),
                "prompt cache accounting"
            );
        }
        usage
    });
    let content = clean_plain_text(content);
    let (content, mut dsml_tool_calls) = if dsml_enabled {
        extract_dsml_tool_calls(content)
    } else {
        (content, Vec::new())
    };
    let content = if dsml_enabled {
        strip_orphaned_dsml_tags(content)
    } else {
        content
    };
    let reasoning = clean_plain_text(reasoning);
    let (reasoning, reasoning_dsml_tool_calls) = if dsml_enabled {
        extract_dsml_tool_calls(reasoning)
    } else {
        (reasoning, Vec::new())
    };
    let reasoning = if dsml_enabled {
        strip_orphaned_dsml_tags(reasoning)
    } else {
        reasoning
    };
    dsml_tool_calls.extend(reasoning_dsml_tool_calls);
    let (content, tag_reasoning) = clean_response_content(content);
    let reasoning = if reasoning.trim().is_empty() {
        tag_reasoning
    } else {
        Some(reasoning)
    };
    let tool_calls = if dsml_tool_calls.is_empty() {
        tool_calls
    } else {
        dsml_tool_calls
    };
    if content.trim().is_empty()
        && !reasoning
            .as_ref()
            .is_some_and(|text| !text.trim().is_empty())
        && tool_calls.is_empty()
    {
        bail!(
            "{}",
            t(
                "chat completions stream response was empty",
                "聊天流式响应为空",
            )
        );
    }
    Ok(ChatResult {
        content,
        reasoning: reasoning.filter(|text| !text.trim().is_empty()),
        usage,
        usage_estimated: false,
        tool_calls,
        provider_id: None,
        model: None,
        finish_reason: None,
        thinking_signature: None,
        last_request_usage: None,
        responses_continuation: None,
    })
}

pub(crate) fn dsml_enabled_for(provider: &ProviderConfig) -> bool {
    let base_url = provider.base_url.to_ascii_lowercase();
    let model = provider.default_model.to_ascii_lowercase();
    base_url.contains("taotoken.net") && model.starts_with("glm")
}

pub(crate) const DSML_ANY_PREFIX: &str = "<｜｜DSML";
pub(crate) const DSML_PREFIX: &str = "<｜｜DSML｜｜tool_calls";
pub(crate) const DSML_END: &str = "</｜｜DSML｜｜tool_calls>";
pub(crate) const SYSTEM_REMINDER_PREFIX: &str = "<system-reminder";
pub(crate) const SYSTEM_REMINDER_UNDERSCORE_PREFIX: &str = "<system_reminder";

pub(crate) fn hidden_start_after(target: &str, offset: usize) -> Option<usize> {
    [
        target[offset..].find(DSML_ANY_PREFIX),
        target[offset..].find(SYSTEM_REMINDER_PREFIX),
        target[offset..].find(SYSTEM_REMINDER_UNDERSCORE_PREFIX),
    ]
    .into_iter()
    .flatten()
    .map(|index| offset + index)
    .min()
}

pub(crate) fn starts_hidden_prefix(value: &str) -> bool {
    DSML_ANY_PREFIX.starts_with(value)
        || SYSTEM_REMINDER_PREFIX.starts_with(value)
        || SYSTEM_REMINDER_UNDERSCORE_PREFIX.starts_with(value)
        || value.starts_with(DSML_ANY_PREFIX)
        || value.starts_with(SYSTEM_REMINDER_PREFIX)
        || value.starts_with(SYSTEM_REMINDER_UNDERSCORE_PREFIX)
}

pub(crate) fn partial_hidden_suffix_len(value: &str) -> usize {
    let max_len = value.len().min(
        DSML_ANY_PREFIX
            .len()
            .max(SYSTEM_REMINDER_PREFIX.len())
            .max(SYSTEM_REMINDER_UNDERSCORE_PREFIX.len()),
    );
    for len in (1..=max_len).rev() {
        if !value.is_char_boundary(value.len() - len) {
            continue;
        }
        let suffix = &value[value.len() - len..];
        if DSML_ANY_PREFIX.starts_with(suffix)
            || SYSTEM_REMINDER_PREFIX.starts_with(suffix)
            || SYSTEM_REMINDER_UNDERSCORE_PREFIX.starts_with(suffix)
        {
            return len;
        }
    }
    0
}

pub(crate) fn hidden_end_after(target: &str, offset: usize) -> Option<usize> {
    let remaining = &target[offset..];
    if remaining.starts_with(DSML_ANY_PREFIX) {
        return remaining
            .find(DSML_END)
            .map(|index| offset + index + DSML_END.len());
    }
    for tag in ["system-reminder", "system_reminder"] {
        let open_prefix = format!("<{tag}");
        if remaining.starts_with(&open_prefix) {
            let close = format!("</{tag}>");
            return remaining
                .find(&close)
                .map(|index| offset + index + close.len());
        }
    }
    None
}

pub(crate) fn extract_dsml_tool_calls(mut content: String) -> (String, Vec<ToolCall>) {
    let mut calls = Vec::new();
    let mut index = 0usize;
    while let Some(start) = content.find(DSML_PREFIX) {
        let tag_end = content[start..]
            .find('>')
            .map(|offset| start + offset + 1)
            .unwrap_or(start + DSML_PREFIX.len());
        let body_start = tag_end;
        let Some(relative_end) = content[body_start..].find(DSML_END) else {
            content.replace_range(start.., "");
            break;
        };
        let end = body_start + relative_end;
        let block = content[body_start..end].to_string();
        calls.extend(parse_dsml_block(&block, &mut index));
        content.replace_range(start..end + DSML_END.len(), "");
    }
    (content.trim().to_string(), calls)
}

pub(crate) fn strip_orphaned_dsml_tags(mut content: String) -> String {
    content = content.replace(DSML_END, "");
    content = content.replace(DSML_PREFIX, "");
    content = content.replace("</｜｜DSML｜｜invoke>", "");
    content = content.replace("<｜｜DSML｜｜invoke", "");
    content = content.replace("</｜｜DSML｜｜parameter>", "");
    content = content.replace("<｜｜DSML｜｜parameter", "");
    content.trim().to_string()
}

pub(crate) fn parse_dsml_block(block: &str, index: &mut usize) -> Vec<ToolCall> {
    let mut calls = Vec::new();
    let mut rest = block;
    while let Some(start) = rest.find("<｜｜DSML｜｜invoke") {
        rest = &rest[start..];
        let Some(tag_end) = rest.find('>') else {
            break;
        };
        let tag = &rest[..tag_end];
        let Some(name) = attr_value(tag, "name") else {
            rest = &rest[tag_end..];
            continue;
        };
        let body_start = tag_end + 1;
        let Some(relative_end) = rest[body_start..].find("</｜｜DSML｜｜invoke>") else {
            break;
        };
        let body = &rest[body_start..body_start + relative_end];
        let arguments = parse_dsml_arguments(body);
        *index += 1;
        calls.push(ToolCall {
            id: format!("dsml-tool-call-{index}"),
            kind: "function".to_string(),
            function: ToolCallFunction {
                name,
                arguments: arguments.to_string(),
            },
        });
        rest = &rest[body_start + relative_end + "</｜｜DSML｜｜invoke>".len()..];
    }
    calls
}

pub(crate) fn parse_dsml_arguments(body: &str) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    let mut rest = body;
    while let Some(start) = rest.find("<｜｜DSML｜｜parameter") {
        rest = &rest[start..];
        let Some(tag_end) = rest.find('>') else {
            break;
        };
        let tag = &rest[..tag_end];
        let Some(name) = attr_value(tag, "name") else {
            rest = &rest[tag_end..];
            continue;
        };
        let value_start = tag_end + 1;
        let Some(relative_end) = rest[value_start..].find("</｜｜DSML｜｜parameter>") else {
            break;
        };
        let raw_value = rest[value_start..value_start + relative_end].trim();
        map.insert(name, parse_dsml_value(raw_value));
        rest = &rest[value_start + relative_end + "</｜｜DSML｜｜parameter>".len()..];
    }
    serde_json::Value::Object(map)
}

pub(crate) fn parse_dsml_value(value: &str) -> serde_json::Value {
    let trimmed = value.trim();
    if let Ok(value) = serde_json::from_str(trimmed) {
        return value;
    }
    if let Ok(value) = trimmed.parse::<i64>() {
        return serde_json::Value::Number(value.into());
    }
    serde_json::Value::String(trimmed.trim_matches('"').to_string())
}

pub(crate) fn attr_value(tag: &str, name: &str) -> Option<String> {
    let pattern = format!("{name}=\"");
    let start = tag.find(&pattern)? + pattern.len();
    let end = tag[start..].find('"')?;
    Some(tag[start..start + end].to_string())
}

pub(crate) fn clean_plain_text(mut text: String) -> String {
    for tag in ["system-reminder", "system_reminder"] {
        text = strip_tagged_sections(text, tag);
    }
    text = text.replace("<system-reminder>", "");
    text = text.replace("</system-reminder>", "");
    text = text.replace("<system_reminder>", "");
    text = text.replace("</system_reminder>", "");
    text
}
