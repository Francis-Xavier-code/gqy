//! providers2 — 自 src/llm/openai_compatible/providers.rs 拆分。

pub(crate) use super::*;

impl AnthropicToolAccumulator {
    pub(crate) fn start(&mut self, index: usize, block: AnthropicStreamBlock) -> Option<String> {
        if index >= MAX_STREAM_TOOL_CALLS {
            return None;
        }
        while self.calls.len() <= index {
            self.calls.push(PartialToolCall::default());
        }
        let call = &mut self.calls[index];
        call.id = block.id.unwrap_or_else(|| format!("tool-{index}"));
        call.kind = "function".to_string();
        call.name = block.name.unwrap_or_default();
        (!call.name.is_empty()).then(|| call.name.clone())
    }

    pub(crate) fn append_arguments(&mut self, index: usize, text: String) {
        if index >= MAX_STREAM_TOOL_CALLS {
            return;
        }
        while self.calls.len() <= index {
            self.calls.push(PartialToolCall::default());
        }
        append_bounded(
            &mut self.calls[index].arguments,
            &text,
            MAX_STREAM_TOOL_ARGUMENT_BYTES,
        );
    }

    pub(crate) fn finish(self) -> Vec<ToolCall> {
        self.calls
            .into_iter()
            .filter(|call| !call.name.trim().is_empty())
            .map(|call| {
                let id = if call.id.is_empty() {
                    gen_tool_call_id()
                } else {
                    call.id
                };
                ToolCall {
                    id,
                    kind: if call.kind.is_empty() {
                        "function".to_string()
                    } else {
                        call.kind
                    },
                    function: ToolCallFunction {
                        name: call.name,
                        arguments: call.arguments,
                    },
                }
            })
            .collect()
    }
}

#[derive(Debug, Default)]
pub(crate) struct ResponsesToolAccumulator {
    calls: Vec<PartialResponsesToolCall>,
}

#[derive(Debug, Default)]
pub(crate) struct PartialResponsesToolCall {
    item_id: String,
    call: PartialToolCall,
}

impl ResponsesToolAccumulator {
    pub(crate) fn start(&mut self, item: ResponsesStreamItem) -> Option<String> {
        if item.kind != "function_call" || self.calls.len() >= MAX_STREAM_TOOL_CALLS {
            return None;
        }
        let item_id = item.id.unwrap_or_default();
        let name = item.name.unwrap_or_default();
        self.calls.push(PartialResponsesToolCall {
            call: PartialToolCall {
                id: item.call_id.unwrap_or_else(|| item_id.clone()),
                kind: "function".to_string(),
                name: name.clone(),
                arguments: bounded_stream_string(
                    item.arguments.unwrap_or_default(),
                    MAX_STREAM_TOOL_ARGUMENT_BYTES,
                ),
            },
            item_id,
        });
        (!name.is_empty()).then_some(name)
    }

    pub(crate) fn append_arguments(&mut self, item_id: Option<String>, delta: String) {
        if let Some(item_id) = item_id {
            if let Some(partial) = self.calls.iter_mut().find(|call| call.item_id == item_id) {
                append_bounded(
                    &mut partial.call.arguments,
                    &delta,
                    MAX_STREAM_TOOL_ARGUMENT_BYTES,
                );
                return;
            }
            return;
        }
        if let Some(partial) = self.calls.last_mut() {
            append_bounded(
                &mut partial.call.arguments,
                &delta,
                MAX_STREAM_TOOL_ARGUMENT_BYTES,
            );
        }
    }

    pub(crate) fn finish_item(&mut self, item: ResponsesStreamItem) {
        if item.kind != "function_call" {
            return;
        }
        let item_id = item.id.unwrap_or_default();
        let call_id = item.call_id.unwrap_or_default();
        let existing = self.calls.iter_mut().find(|partial| {
            (!item_id.is_empty() && partial.item_id == item_id)
                || (item_id.is_empty() && !call_id.is_empty() && partial.call.id == call_id)
        });
        if let Some(partial) = existing {
            if !call_id.is_empty() {
                partial.call.id = call_id;
            }
            if let Some(name) = item.name {
                partial.call.name = name;
            }
            if let Some(arguments) = item.arguments {
                partial.call.arguments =
                    bounded_stream_string(arguments, MAX_STREAM_TOOL_ARGUMENT_BYTES);
            }
        } else {
            let _ = self.start(ResponsesStreamItem {
                kind: "function_call".to_string(),
                id: (!item_id.is_empty()).then_some(item_id),
                call_id: (!call_id.is_empty()).then_some(call_id),
                name: item.name,
                arguments: item.arguments,
            });
        }
    }

    pub(crate) fn finish_arguments(
        &mut self,
        item_id: Option<String>,
        name: Option<String>,
        arguments: Option<String>,
    ) {
        let Some(item_id) = item_id else {
            return;
        };
        let Some(partial) = self.calls.iter_mut().find(|call| call.item_id == item_id) else {
            return;
        };
        if let Some(name) = name {
            partial.call.name = name;
        }
        if let Some(arguments) = arguments {
            partial.call.arguments =
                bounded_stream_string(arguments, MAX_STREAM_TOOL_ARGUMENT_BYTES);
        }
    }

    pub(crate) fn finish(self) -> Vec<ToolCall> {
        self.calls
            .into_iter()
            .map(|partial| partial.call)
            .filter(|call| !call.name.trim().is_empty())
            .map(|call| {
                let id = if call.id.is_empty() {
                    gen_tool_call_id()
                } else {
                    call.id
                };
                ToolCall {
                    id,
                    kind: call.kind,
                    function: ToolCallFunction {
                        name: call.name,
                        arguments: call.arguments,
                    },
                }
            })
            .collect()
    }
}

#[derive(Debug, Default)]
pub(crate) struct ToolCallAccumulator {
    pub(crate) calls: Vec<PartialToolCall>,
}

#[derive(Debug, Default)]
pub(crate) struct PartialToolCall {
    id: String,
    kind: String,
    name: String,
    arguments: String,
}

impl ToolCallAccumulator {
    pub(crate) fn push(&mut self, delta: ToolCallDelta) -> Option<String> {
        if delta.index >= MAX_STREAM_TOOL_CALLS {
            return None;
        }
        while self.calls.len() <= delta.index {
            self.calls.push(PartialToolCall::default());
        }
        let call = &mut self.calls[delta.index];
        let name_updated = delta.function.name.is_some();
        if let Some(id) = delta.id {
            call.id = id;
        }
        if let Some(kind) = delta.kind {
            call.kind = kind;
        }
        if let Some(name) = delta.function.name {
            // Some gateways resend the complete function name on every delta
            // instead of streaming fragments; blind appending would build
            // "use_tooluse_tool…". Treat an exact repeat (or a full-name replay
            // that extends the current prefix) as a replacement, and only
            // append genuine fragments.
            if call.name.is_empty() {
                append_bounded(&mut call.name, &name, 16 * 1024);
            } else if name == call.name {
                // full-name replay, ignore
            } else if name.starts_with(&call.name) {
                call.name.clear();
                append_bounded(&mut call.name, &name, 16 * 1024);
            } else {
                append_bounded(&mut call.name, &name, 16 * 1024);
            }
        }
        if let Some(arguments) = delta.function.arguments {
            append_bounded(
                &mut call.arguments,
                &arguments,
                MAX_STREAM_TOOL_ARGUMENT_BYTES,
            );
        }
        (name_updated && !call.name.is_empty()).then(|| call.name.clone())
    }

    pub(crate) fn finish(self) -> Vec<ToolCall> {
        self.calls
            .into_iter()
            .filter(|call| !call.name.trim().is_empty())
            .map(|call| {
                let id = if call.id.is_empty() {
                    gen_tool_call_id()
                } else {
                    call.id
                };
                ToolCall {
                    id,
                    kind: if call.kind.is_empty() {
                        "function".to_string()
                    } else {
                        call.kind
                    },
                    function: ToolCallFunction {
                        name: call.name,
                        arguments: call.arguments,
                    },
                }
            })
            .collect()
    }
}

#[derive(Default)]
pub(crate) struct Utf8LineBuffer {
    buffer: Vec<u8>,
    received_bytes: usize,
}

impl Utf8LineBuffer {
    pub(crate) fn push(&mut self, bytes: &[u8]) -> Result<Vec<String>> {
        if self.received_bytes.saturating_add(bytes.len()) > MAX_STREAM_RESPONSE_BYTES {
            bail!("streaming response exceeded {MAX_STREAM_RESPONSE_BYTES} bytes");
        }
        if self.buffer.len().saturating_add(bytes.len()) > MAX_STREAM_LINE_BYTES {
            bail!("streaming response line exceeded {MAX_STREAM_LINE_BYTES} bytes");
        }
        self.received_bytes += bytes.len();
        self.buffer.extend_from_slice(bytes);
        let mut lines = Vec::new();
        while let Some(index) = self.buffer.iter().position(|byte| *byte == b'\n') {
            let mut line = self.buffer.drain(..=index).collect::<Vec<_>>();
            if line.last() == Some(&b'\n') {
                line.pop();
            }
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            lines.push(
                std::str::from_utf8(&line)
                    .context("invalid utf-8 in streaming response")?
                    .to_string(),
            );
        }
        Ok(lines)
    }

    pub(crate) fn finish(mut self) -> Result<Vec<String>> {
        if self.buffer.iter().all(|byte| byte.is_ascii_whitespace()) {
            return Ok(Vec::new());
        }
        if self.buffer.last() == Some(&b'\r') {
            self.buffer.pop();
        }
        Ok(vec![std::str::from_utf8(&self.buffer)
            .context("invalid utf-8 in streaming response")?
            .to_string()])
    }
}

#[derive(Default)]
pub(crate) struct SseDataBuffer {
    lines: Utf8LineBuffer,
    data_lines: Vec<String>,
}

impl SseDataBuffer {
    pub(crate) fn push(&mut self, bytes: &[u8]) -> Result<Vec<String>> {
        let mut events = Vec::new();
        for line in self.lines.push(bytes)? {
            if let Some(event) = self.push_line(&line) {
                events.push(event);
            }
        }
        Ok(events)
    }

    pub(crate) fn finish(mut self) -> Result<Vec<String>> {
        let mut events = Vec::new();
        for line in std::mem::take(&mut self.lines).finish()? {
            if let Some(event) = self.push_line(&line) {
                events.push(event);
            }
        }
        if !self.data_lines.is_empty() {
            events.push(self.data_lines.join("\n"));
        }
        Ok(events)
    }

    pub(crate) fn push_line(&mut self, line: &str) -> Option<String> {
        if line.is_empty() {
            if self.data_lines.is_empty() {
                return None;
            }
            return Some(std::mem::take(&mut self.data_lines).join("\n"));
        }
        if let Some(data) = line.strip_prefix("data:") {
            self.data_lines.push(data.trim_start().to_string());
        }
        None
    }
}

pub(crate) fn clean_response_content(content: String) -> (String, Option<String>) {
    split_tagged_reasoning(clean_plain_text(content))
}

pub(crate) fn is_empty_error(value: &Value) -> bool {
    match value {
        Value::String(text) => text.trim().is_empty(),
        Value::Object(fields) => fields.is_empty(),
        Value::Null => true,
        _ => false,
    }
}

pub(crate) fn provider_error_text(value: &Value) -> String {
    value
        .get("message")
        .and_then(Value::as_str)
        .or_else(|| {
            value
                .get("error")
                .and_then(|error| error.get("message"))
                .and_then(Value::as_str)
        })
        .map(|message| clean_plain_text(message.to_string()))
        .unwrap_or_else(|| clean_plain_text(value.to_string()))
}

pub(crate) fn split_tagged_reasoning(content: String) -> (String, Option<String>) {
    match split_tag_pair(content, "think").or_else(|content| split_tag_pair(content, "thinking")) {
        Ok(result) => result,
        Err(content) => (content, None),
    }
}

pub(crate) fn split_tag_pair(
    content: String,
    tag: &str,
) -> std::result::Result<(String, Option<String>), String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let Some(start) = content.find(&open) else {
        return Err(content);
    };
    let reasoning_start = start + open.len();
    let Some(relative_end) = content[reasoning_start..].find(&close) else {
        return Ok((content, None));
    };
    let end = reasoning_start + relative_end;
    let reasoning = content[reasoning_start..end].trim().to_string();
    let mut visible = String::new();
    visible.push_str(content[..start].trim_end());
    visible.push_str(content[end + close.len()..].trim_start());
    Ok((
        visible.trim().to_string(),
        (!reasoning.is_empty()).then_some(reasoning),
    ))
}

#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn handle_sse_line<F>(
    line: &str,
    content: &mut String,
    content_emitted: &mut usize,
    reasoning: &mut String,
    reasoning_emitted: &mut usize,
    reasoning_part_active: &mut bool,
    finish_reason: &mut Option<String>,
    usage: &mut Option<Usage>,
    tool_calls: &mut ToolCallAccumulator,
    on_chunk: &mut F,
) -> Result<Option<bool>>
where
    F: FnMut(ChatStreamChunk) -> Result<()>,
{
    let Some(data) = line.strip_prefix("data:").map(str::trim) else {
        return Ok(None);
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
        tracing::debug!(
            finish_reason = finish_reason.as_deref(),
            content_chars = content.chars().count(),
            reasoning_chars = reasoning.chars().count(),
            tool_call_count = tool_calls.calls.len(),
            "{}",
            t(
                "Chat completions stream received DONE",
                "聊天补全流已收到 DONE"
            )
        );
        return Ok(Some(true));
    }
    let response: ChatStreamResponse = serde_json::from_str(data).with_context(|| {
        format!(
            "{}: {}",
            t(
                "invalid chat completions stream response",
                "无效的聊天流式响应",
            ),
            clean_plain_text(data.to_string())
        )
    })?;
    // An empty `error` is not one: some gateways send `{"error":""}` alongside
    // the terminal usage event, and failing the turn over it would turn a
    // normal completion into a spurious error.
    if let Some(error) = response.error.filter(|error| !is_empty_error(error)) {
        bail!(
            "{}: {}",
            t(
                "chat completions stream returned an error",
                "聊天流式响应返回错误"
            ),
            provider_error_text(&error)
        );
    }
    if let Some(next_usage) = response.usage {
        *usage = Some(next_usage);
    }
    for choice in response.choices {
        // An empty string is "absent", not an end signal: some gateways send
        // `"finish_reason": ""` on ordinary chunks.
        if let Some(next_finish_reason) = choice.finish_reason.filter(|reason| !reason.is_empty()) {
            tracing::debug!(
                finish_reason = %next_finish_reason,
                "{}",
                t(
                    "Chat completions stream finish reason received",
                    "已收到聊天补全流结束原因"
                )
            );
            *finish_reason = Some(next_finish_reason);
        }
        let delta = choice.delta;
        if let Some(text) = delta_reasoning_text(&delta) {
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
        if let Some(text) = delta.content {
            if !text.is_empty() {
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
                push_buffered_chunk(
                    content,
                    content_emitted,
                    ChatStreamKind::Content,
                    text,
                    on_chunk,
                )?;
            }
        }
        for tool_call in delta.tool_calls {
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
            if let Some(name) = tool_calls.push(tool_call) {
                on_chunk(ChatStreamChunk {
                    kind: ChatStreamKind::ToolCall,
                    text: name,
                })?;
            }
        }
    }
    Ok(Some(false))
}
