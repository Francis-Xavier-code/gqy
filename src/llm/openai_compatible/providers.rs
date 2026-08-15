//! providers — 自 src/llm/openai_compatible.rs 拆分。

use super::*;

pub(crate) fn stream_chunk_commits_attempt(
    chunk: &ChatStreamChunk,
    reasoning_visibility: ReasoningVisibility,
) -> bool {
    (chunk.kind == ChatStreamKind::ReasoningPartEnd
        && reasoning_visibility != ReasoningVisibility::Hidden)
        || chunk.kind == ChatStreamKind::ToolCall
        || (chunk.kind == ChatStreamKind::Content && !chunk.text.is_empty())
        || (reasoning_visibility == ReasoningVisibility::Full
            && chunk.kind == ChatStreamKind::Reasoning
            && !chunk.text.is_empty())
}

pub(crate) fn reasoning_visibility(config: &AppConfig) -> ReasoningVisibility {
    match config
        .display
        .reasoning
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "hidden" => ReasoningVisibility::Hidden,
        "full" => ReasoningVisibility::Full,
        _ => ReasoningVisibility::Summary,
    }
}

pub(crate) fn reasoning_summary_is_detailed(config: &AppConfig) -> bool {
    config.display.reasoning.trim().eq_ignore_ascii_case("full")
}

pub(crate) fn provider_looks_anthropic(provider: &ProviderConfig) -> bool {
    let id = provider.id.to_ascii_lowercase();
    let display_name = provider.display_name.to_ascii_lowercase();
    let base_url = provider.base_url.to_ascii_lowercase();
    id == "anthropic"
        || id == "claude"
        || id.contains("anthropic")
        || display_name.contains("anthropic")
        || base_url.contains("api.anthropic.com")
        || base_url.contains("anthropic.com/v1")
}

pub(crate) fn provider_looks_claude_related(provider: &ProviderConfig) -> bool {
    let id = provider.id.to_ascii_lowercase();
    let display_name = provider.display_name.to_ascii_lowercase();
    let base_url = provider.base_url.to_ascii_lowercase();
    let model = provider.default_model.to_ascii_lowercase();
    provider_looks_anthropic(provider)
        || id.contains("claude")
        || display_name.contains("claude")
        || model.starts_with("claude")
        || base_url.contains("claude")
}

pub(crate) fn claude_protocol_hint(provider: &ProviderConfig) -> &'static str {
    let protocol = provider.protocol.trim();
    if (protocol.is_empty()
        || protocol.eq_ignore_ascii_case("auto")
        || protocol.eq_ignore_ascii_case("openai-chat"))
        && provider_looks_claude_related(provider)
        && !provider_looks_anthropic(provider)
    {
        return "\nHint: if this provider is the official Anthropic Claude API, set provider protocol to anthropic and base_url to https://api.anthropic.com/v1. If it is an OpenAI-compatible Claude proxy, keep openai-chat/auto.";
    }
    ""
}

pub(crate) fn anthropic_thinking_config() -> Value {
    json!({ "type": "adaptive", "display": "summarized" })
}

pub(crate) fn anthropic_thinking_unsupported(status: u16, body: &str) -> bool {
    if status != 400 && status != 422 {
        return false;
    }
    let body = body.to_ascii_lowercase();
    body.contains("thinking")
        && (body.contains("unsupported")
            || body.contains("not supported")
            || body.contains("unknown")
            || body.contains("invalid")
            || body.contains("unrecognized"))
}

pub(crate) fn responses_unsupported(status: u16, body: &str) -> bool {
    if status == 404 || status == 405 {
        return true;
    }
    if status != 400 {
        return false;
    }
    let body = body.to_ascii_lowercase();
    body.contains("unsupported")
        || body.contains("not supported")
        || body.contains("unknown parameter")
        || body.contains("invalid endpoint")
        || body.contains("not found")
}

pub(crate) fn stream_options_unsupported(status: u16, body: &str) -> bool {
    if status != 400 && status != 422 {
        return false;
    }
    let body = body.to_ascii_lowercase();
    body.contains("stream_options")
        && (body.contains("unsupported")
            || body.contains("not supported")
            || body.contains("unknown")
            || body.contains("unrecognized")
            || body.contains("invalid")
            || body.contains("extra"))
}

pub(crate) fn non_stream_quota_fallback_candidate(status: u16, body: &str) -> bool {
    status == 429 && body.to_ascii_lowercase().contains("insufficient_quota")
}

pub(crate) fn zen_upstream_failed(provider: &ProviderConfig, status: u16, body: &str) -> bool {
    status == 400
        && provider.base_url.trim_end_matches('/') == OPENCODE_ZEN_BASE_URL
        && body
            .to_ascii_lowercase()
            .contains("upstream request failed")
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    temperature: f32,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream_options: Option<ChatStreamOptions>,
    /// Only set by cache-keepalive pings; normal chat leaves the provider
    /// default in place.
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<ToolDefinition>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    chat_template_kwargs: Option<ChatTemplateKwargs>,
    #[serde(flatten)]
    #[serde(skip_serializing_if = "Option::is_none")]
    extra_body: Option<Map<String, Value>>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ChatStreamOptions {
    include_usage: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct ResponsesRequest {
    model: String,
    input: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    instructions: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    previous_response_id: Option<String>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning: Option<ResponsesReasoning>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(flatten)]
    #[serde(skip_serializing_if = "Option::is_none")]
    extra_body: Option<Map<String, Value>>,
}

#[derive(Debug, Serialize)]
pub(crate) struct ResponsesReasoning {
    #[serde(skip_serializing_if = "Option::is_none")]
    effort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    summary: Option<String>,
}

pub(crate) fn default_responses_reasoning(summary: &str) -> ResponsesReasoning {
    ResponsesReasoning {
        effort: Some("medium".to_string()),
        summary: Some(summary.to_string()),
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct AnthropicRequest {
    model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    messages: Vec<AnthropicMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<AnthropicTool>>,
    stream: bool,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking: Option<Value>,
    #[serde(flatten)]
    #[serde(skip_serializing_if = "Option::is_none")]
    extra_body: Option<Map<String, Value>>,
}

#[derive(Debug, Serialize)]
pub(crate) struct AnthropicMessage {
    role: String,
    content: Vec<AnthropicContentBlock>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type")]
pub(crate) enum AnthropicContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image")]
    Image { source: AnthropicImageSource },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        content: String,
    },
    /// Extended-thinking block replayed on assistant tool_use turns. Anthropic
    /// 400s a thinking-enabled tool loop whose assistant turns omit the block.
    #[serde(rename = "thinking")]
    Thinking { thinking: String, signature: String },
}

#[derive(Debug, Serialize)]
#[serde(tag = "type")]
pub(crate) enum AnthropicImageSource {
    #[serde(rename = "base64")]
    Base64 { media_type: String, data: String },
    #[serde(rename = "url")]
    Url { url: String },
}

#[derive(Debug, Serialize)]
pub(crate) struct AnthropicTool {
    name: String,
    description: String,
    input_schema: Value,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ChatTemplateKwargs {
    enable_thinking: bool,
}

/// DeepSeek thinking mode 400s an assistant tool_calls turn whose
/// `reasoning_content` KEY is absent from the request JSON, while many other
/// OpenAI-compatible gateways reject the unknown field outright. Send the key
/// only to providers known to understand it and strip it everywhere else, so
/// the transport copy stays byte-identical to the pre-A17 shape on unrelated
/// endpoints (prompt-cache prefix preserved).
pub(crate) fn provider_accepts_reasoning_content(provider: &ProviderConfig) -> bool {
    let haystack = format!(
        "{} {} {}",
        provider.id.to_ascii_lowercase(),
        provider.base_url.to_ascii_lowercase(),
        provider.default_model.to_ascii_lowercase()
    );
    ["deepseek", "glm-", "zhipu", "bigmodel", "kimi", "moonshot"]
        .iter()
        .any(|needle| haystack.contains(needle))
}

pub(crate) fn prepare_chat_messages_for_provider(
    provider: &ProviderConfig,
    mut messages: Vec<ChatMessage>,
) -> Vec<ChatMessage> {
    if !provider_accepts_reasoning_content(provider) {
        for message in &mut messages {
            message.reasoning_content = None;
        }
    }
    messages
}

pub(crate) fn taotoken_glm_chat_template_kwargs(provider: &ProviderConfig) -> Option<ChatTemplateKwargs> {
    let base_url = provider.base_url.to_ascii_lowercase();
    let model = provider.default_model.to_ascii_lowercase();
    if base_url.contains("taotoken.net") && model.starts_with("glm") {
        Some(ChatTemplateKwargs {
            enable_thinking: true,
        })
    } else {
        None
    }
}

pub(crate) fn lower_responses_messages(messages: Vec<ChatMessage>) -> Vec<Value> {
    messages
        .into_iter()
        .flat_map(|message| match message.role.as_str() {
            "system" => vec![json!({"role": "system", "content": chat_content_text(message.content)})],
            "user" => vec![json!({"role": "user", "content": lower_responses_user_content(message.content)})],
            "assistant" => lower_responses_assistant_message(message),
            "tool" => vec![json!({"type": "function_call_output", "call_id": message.tool_call_id.unwrap_or_default(), "output": chat_content_text(message.content)})],
            role => vec![json!({"role": role, "content": chat_content_text(message.content)})],
        })
        .collect()
}

pub(crate) fn lower_responses_assistant_message(message: ChatMessage) -> Vec<Value> {
    let mut items = Vec::new();
    let text = chat_content_text(message.content);
    if !text.trim().is_empty() {
        items.push(json!({"role": "assistant", "content": text}));
    }
    if let Some(tool_calls) = message.tool_calls {
        items.extend(tool_calls.into_iter().map(|call| {
            json!({
                "type": "function_call",
                "call_id": call.id,
                "name": call.function.name,
                "arguments": call.function.arguments,
            })
        }));
    }
    items
}

pub(crate) fn lower_responses_user_content(content: Option<super::ChatContent>) -> Vec<Value> {
    match content {
        Some(super::ChatContent::Parts(parts)) => parts
            .into_iter()
            .map(|part| match part {
                super::ChatContentPart::Text { text } => {
                    json!({"type": "input_text", "text": text})
                }
                super::ChatContentPart::ImageUrl { image_url } => {
                    json!({"type": "input_image", "image_url": image_url.url})
                }
            })
            .collect(),
        Some(super::ChatContent::Text(text)) => vec![json!({"type": "input_text", "text": text})],
        None => vec![json!({"type": "input_text", "text": ""})],
    }
}

pub(crate) fn chat_content_text(content: Option<super::ChatContent>) -> String {
    match content {
        Some(super::ChatContent::Text(text)) => text,
        Some(super::ChatContent::Parts(parts)) => parts
            .into_iter()
            .filter_map(|part| match part {
                super::ChatContentPart::Text { text } => Some(text),
                super::ChatContentPart::ImageUrl { .. } => None,
            })
            .collect::<Vec<_>>()
            .join(""),
        None => String::new(),
    }
}

pub(crate) fn lower_responses_tools(tools: Vec<ToolDefinition>) -> Vec<Value> {
    tools
        .into_iter()
        .map(|tool| {
            json!({
                "type": "function",
                "name": tool.function.name,
                "description": tool.function.description,
                "parameters": openai_tool_input_schema(tool.function.parameters),
                "strict": false,
            })
        })
        .collect()
}

pub(crate) fn lower_anthropic_system(messages: &[ChatMessage]) -> Option<String> {
    messages
        .iter()
        .take_while(|message| message.role == "system")
        .map(|message| chat_content_text_ref(message.content.as_ref()))
        .filter(|text| !text.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
        .into_non_empty()
}

pub(crate) fn lower_anthropic_messages(messages: Vec<ChatMessage>) -> Vec<AnthropicMessage> {
    let mut output = Vec::new();
    let mut skipped_initial_system = true;
    for message in messages {
        if skipped_initial_system && message.role == "system" {
            continue;
        }
        skipped_initial_system = false;
        match message.role.as_str() {
            "user" => output.push(AnthropicMessage {
                role: "user".to_string(),
                content: lower_anthropic_user_content(message.content),
            }),
            "assistant" => output.push(AnthropicMessage {
                role: "assistant".to_string(),
                content: lower_anthropic_assistant_content(message),
            }),
            "tool" => output.push(AnthropicMessage {
                role: "user".to_string(),
                content: vec![AnthropicContentBlock::ToolResult {
                    tool_use_id: message.tool_call_id.unwrap_or_default(),
                    content: chat_content_text(message.content),
                }],
            }),
            "system" => output.push(AnthropicMessage {
                role: "user".to_string(),
                content: vec![AnthropicContentBlock::Text {
                    text: wrap_system_update(chat_content_text(message.content)),
                }],
            }),
            _ => output.push(AnthropicMessage {
                role: "user".to_string(),
                content: vec![AnthropicContentBlock::Text {
                    text: chat_content_text(message.content),
                }],
            }),
        }
    }
    output
}

pub(crate) fn lower_anthropic_user_content(content: Option<super::ChatContent>) -> Vec<AnthropicContentBlock> {
    match content {
        Some(super::ChatContent::Parts(parts)) => parts
            .into_iter()
            .filter_map(|part| match part {
                super::ChatContentPart::Text { text } => Some(AnthropicContentBlock::Text { text }),
                super::ChatContentPart::ImageUrl { image_url } => {
                    lower_anthropic_image_url(&image_url.url)
                }
            })
            .collect(),
        Some(super::ChatContent::Text(text)) => vec![AnthropicContentBlock::Text { text }],
        None => vec![AnthropicContentBlock::Text {
            text: String::new(),
        }],
    }
}

pub(crate) fn lower_anthropic_image_url(url: &str) -> Option<AnthropicContentBlock> {
    if url.starts_with("http://") || url.starts_with("https://") {
        return Some(AnthropicContentBlock::Image {
            source: AnthropicImageSource::Url {
                url: url.to_string(),
            },
        });
    }
    let data = url.strip_prefix("data:")?;
    let (media_type, base64) = data.split_once(";base64,")?;
    Some(AnthropicContentBlock::Image {
        source: AnthropicImageSource::Base64 {
            media_type: media_type.to_string(),
            data: base64.to_string(),
        },
    })
}

pub(crate) fn lower_anthropic_assistant_content(message: ChatMessage) -> Vec<AnthropicContentBlock> {
    let mut content = Vec::new();
    let has_tool_calls = message
        .tool_calls
        .as_ref()
        .is_some_and(|calls| !calls.is_empty());
    if has_tool_calls {
        if let (Some(signature), Some(thinking)) = (
            message.thinking_signature.as_ref(),
            message.reasoning_content.as_ref(),
        ) {
            if !thinking.trim().is_empty() && !signature.trim().is_empty() {
                content.push(AnthropicContentBlock::Thinking {
                    thinking: thinking.clone(),
                    signature: signature.clone(),
                });
            }
        }
    }
    let text = chat_content_text(message.content);
    if !text.trim().is_empty() {
        content.push(AnthropicContentBlock::Text { text });
    }
    if let Some(tool_calls) = message.tool_calls {
        content.extend(
            tool_calls
                .into_iter()
                .map(|call| AnthropicContentBlock::ToolUse {
                    id: call.id,
                    name: call.function.name,
                    input: serde_json::from_str(&call.function.arguments)
                        .unwrap_or_else(|_| json!({})),
                }),
        );
    }
    if content.is_empty() {
        content.push(AnthropicContentBlock::Text {
            text: String::new(),
        });
    }
    content
}

pub(crate) fn lower_anthropic_tools(tools: Vec<ToolDefinition>) -> Vec<AnthropicTool> {
    tools
        .into_iter()
        .map(|tool| AnthropicTool {
            name: tool.function.name,
            description: tool.function.description,
            input_schema: tool.function.parameters,
        })
        .collect()
}

pub(crate) fn wrap_system_update(text: String) -> String {
    format!(
        "<system-update>\n{}\n</system-update>",
        text.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    )
}

pub(crate) trait IntoNonEmpty {
    pub(crate) fn into_non_empty(self) -> Option<String>;
}

impl IntoNonEmpty for String {
    pub(crate) fn into_non_empty(self) -> Option<String> {
        (!self.trim().is_empty()).then_some(self)
    }
}

pub(crate) fn chat_content_text_ref(content: Option<&super::ChatContent>) -> String {
    match content {
        Some(super::ChatContent::Text(text)) => text.clone(),
        Some(super::ChatContent::Parts(parts)) => parts
            .iter()
            .filter_map(|part| match part {
                super::ChatContentPart::Text { text } => Some(text.clone()),
                super::ChatContentPart::ImageUrl { .. } => None,
            })
            .collect::<Vec<_>>()
            .join(""),
        None => String::new(),
    }
}

pub(crate) fn openai_tool_input_schema(schema: Value) -> Value {
    let flattened = flatten_top_level_any_of(schema);
    let normalized = remove_null_any_of(flattened);
    if normalized.is_object() {
        normalized
    } else {
        json!({"type": "object"})
    }
}

pub(crate) fn flatten_top_level_any_of(schema: Value) -> Value {
    let Some(object) = schema.as_object() else {
        return json!({"type": "object"});
    };
    let Some(variants) = object.get("anyOf").and_then(Value::as_array) else {
        let mut cloned = object.clone();
        cloned.insert("type".to_string(), Value::String("object".to_string()));
        return Value::Object(cloned);
    };
    let mut properties = serde_json::Map::new();
    for variant in variants.iter().filter_map(Value::as_object) {
        if let Some(variant_properties) = variant.get("properties").and_then(Value::as_object) {
            for (key, value) in variant_properties {
                properties.insert(key.clone(), value.clone());
            }
        }
    }
    let mut flattened = object
        .iter()
        .filter(|(key, _)| key.as_str() != "anyOf")
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect::<serde_json::Map<_, _>>();
    flattened.insert("type".to_string(), Value::String("object".to_string()));
    flattened.insert("properties".to_string(), Value::Object(properties));
    flattened.insert("additionalProperties".to_string(), Value::Bool(false));
    Value::Object(flattened)
}

pub(crate) fn remove_null_any_of(value: Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.into_iter().map(remove_null_any_of).collect()),
        Value::Object(mut object) => {
            let any_of = object.remove("anyOf");
            let mut object = object
                .into_iter()
                .map(|(key, value)| (key, remove_null_any_of(value)))
                .collect::<serde_json::Map<_, _>>();
            let Some(Value::Array(variants)) = any_of else {
                return Value::Object(object);
            };
            let variants = variants
                .into_iter()
                .filter(|variant| variant.get("type").and_then(Value::as_str) != Some("null"))
                .map(remove_null_any_of)
                .collect::<Vec<_>>();
            if variants.len() == 1 {
                if let Some(variant_object) =
                    variants.first().and_then(|item| item.as_object().cloned())
                {
                    object.extend(variant_object);
                    return Value::Object(object);
                }
            }
            object.insert("anyOf".to_string(), Value::Array(variants));
            Value::Object(object)
        }
        value => value,
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct ChatStreamResponse {
    #[serde(default, deserialize_with = "null_as_default")]
    choices: Vec<ChatStreamChoice>,
    #[serde(default, deserialize_with = "null_as_default")]
    usage: Option<Usage>,
    #[serde(default, deserialize_with = "null_as_default")]
    error: Option<Value>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ChatCompletionResponse {
    #[serde(default, deserialize_with = "null_as_default")]
    choices: Vec<ChatCompletionChoice>,
    #[serde(default, deserialize_with = "null_as_default")]
    usage: Option<Usage>,
    #[serde(default, deserialize_with = "null_as_default")]
    error: Option<Value>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ChatCompletionChoice {
    #[serde(default, deserialize_with = "null_as_default")]
    finish_reason: Option<String>,
    #[serde(default)]
    message: ChatChoiceMessage,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ChatStreamChoice {
    #[serde(default, deserialize_with = "null_as_default")]
    finish_reason: Option<String>,
    #[serde(default)]
    delta: ChatChoiceMessage,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct ChatChoiceMessage {
    #[serde(default, deserialize_with = "null_as_default")]
    content: Option<String>,
    #[serde(default, deserialize_with = "null_as_default")]
    reasoning_content: Option<String>,
    #[serde(default, deserialize_with = "null_as_default")]
    reasoning: Option<String>,
    #[serde(default, deserialize_with = "null_as_default")]
    thinking: Option<String>,
    #[serde(default, deserialize_with = "null_as_default")]
    thinking_content: Option<String>,
    #[serde(default, deserialize_with = "null_as_default")]
    reasoning_text: Option<String>,
    #[serde(default, deserialize_with = "null_as_default")]
    reasoning_details: Option<serde_json::Value>,
    #[serde(default, deserialize_with = "null_as_default")]
    tool_calls: Vec<ToolCallDelta>,
}

pub(crate) fn null_as_default<'de, D, T>(deserializer: D) -> std::result::Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct ToolCallDelta {
    #[serde(default)]
    index: usize,
    #[serde(default, deserialize_with = "null_as_default")]
    id: Option<String>,
    #[serde(rename = "type", default, deserialize_with = "null_as_default")]
    kind: Option<String>,
    #[serde(default)]
    function: ToolCallFunctionDelta,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct ToolCallFunctionDelta {
    #[serde(default, deserialize_with = "null_as_default")]
    name: Option<String>,
    #[serde(default, deserialize_with = "null_as_default")]
    arguments: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ResponsesStreamEvent {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default, deserialize_with = "null_as_default")]
    delta: Option<String>,
    #[serde(default, deserialize_with = "null_as_default")]
    arguments: Option<String>,
    #[serde(default, deserialize_with = "null_as_default")]
    name: Option<String>,
    #[serde(default, deserialize_with = "null_as_default")]
    refusal: Option<String>,
    #[serde(default, deserialize_with = "null_as_default")]
    text: Option<String>,
    #[serde(default, deserialize_with = "null_as_default")]
    content_index: Option<usize>,
    #[serde(default, deserialize_with = "null_as_default")]
    item_id: Option<String>,
    #[serde(default, deserialize_with = "null_as_default")]
    item: Option<ResponsesStreamItem>,
    #[serde(default, deserialize_with = "null_as_default")]
    response: Option<ResponsesStreamResponse>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ResponsesStreamItem {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default, deserialize_with = "null_as_default")]
    id: Option<String>,
    #[serde(default, deserialize_with = "null_as_default")]
    call_id: Option<String>,
    #[serde(default, deserialize_with = "null_as_default")]
    name: Option<String>,
    #[serde(default, deserialize_with = "null_as_default")]
    arguments: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ResponsesStreamResponse {
    #[serde(default, deserialize_with = "null_as_default")]
    id: Option<String>,
    #[serde(default, deserialize_with = "null_as_default")]
    usage: Option<ResponsesUsage>,
    #[serde(default, deserialize_with = "null_as_default")]
    incomplete_details: Option<ResponsesIncompleteDetails>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ResponsesIncompleteDetails {
    #[serde(default, deserialize_with = "null_as_default")]
    reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ResponsesUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    total_tokens: u64,
    #[serde(default)]
    input_tokens_details: Option<ResponsesInputTokenDetails>,
    #[serde(default)]
    output_tokens_details: Option<ResponsesOutputTokenDetails>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ResponsesInputTokenDetails {
    #[serde(default)]
    cached_tokens: Option<u64>,
    #[serde(default)]
    cache_write_tokens: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ResponsesOutputTokenDetails {
    #[serde(default)]
    reasoning_tokens: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct AnthropicStreamEvent {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default, deserialize_with = "null_as_default")]
    index: Option<usize>,
    #[serde(default, deserialize_with = "null_as_default")]
    message: Option<AnthropicStreamMessage>,
    #[serde(default, deserialize_with = "null_as_default")]
    content_block: Option<AnthropicStreamBlock>,
    #[serde(default, deserialize_with = "null_as_default")]
    delta: Option<AnthropicStreamDelta>,
    #[serde(default, deserialize_with = "null_as_default")]
    usage: Option<AnthropicUsage>,
    #[serde(default, deserialize_with = "null_as_default")]
    error: Option<AnthropicStreamError>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct AnthropicStreamMessage {
    #[serde(default, deserialize_with = "null_as_default")]
    usage: Option<AnthropicUsage>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct AnthropicStreamBlock {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default, deserialize_with = "null_as_default")]
    id: Option<String>,
    #[serde(default, deserialize_with = "null_as_default")]
    name: Option<String>,
    #[serde(default, deserialize_with = "null_as_default")]
    text: Option<String>,
    #[serde(default, deserialize_with = "null_as_default")]
    thinking: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct AnthropicStreamDelta {
    #[serde(rename = "type", default, deserialize_with = "null_as_default")]
    kind: Option<String>,
    #[serde(default, deserialize_with = "null_as_default")]
    text: Option<String>,
    #[serde(default, deserialize_with = "null_as_default")]
    thinking: Option<String>,
    #[serde(default, deserialize_with = "null_as_default")]
    partial_json: Option<String>,
    #[serde(default, deserialize_with = "null_as_default")]
    signature: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct AnthropicUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: Option<u64>,
    #[serde(default)]
    cache_creation_input_tokens: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct AnthropicStreamError {
    #[serde(rename = "type", default, deserialize_with = "null_as_default")]
    kind: Option<String>,
    #[serde(default, deserialize_with = "null_as_default")]
    message: Option<String>,
}

#[derive(Default)]
pub(crate) struct AnthropicStreamState {
    content: String,
    content_emitted: usize,
    reasoning: String,
    reasoning_emitted: usize,
    reasoning_part_active: bool,
    thinking_signature: Option<String>,
    usage: Option<Usage>,
    tool_calls: AnthropicToolAccumulator,
}

/// Upper bound on streamed tool calls per response. Indices come from the
/// upstream stream verbatim; without a cap a single malformed chunk (e.g.
/// index 2^30) makes the accumulator allocate gigabytes. Chunks addressing
/// an index beyond the cap are dropped.
pub(crate) const MAX_STREAM_TOOL_CALLS: usize = 128;
pub(crate) const MAX_STREAM_TOOL_ARGUMENT_BYTES: usize = 4 * 1024 * 1024;
pub(crate) const MAX_STREAM_LINE_BYTES: usize = 4 * 1024 * 1024;
pub(crate) const MAX_STREAM_RESPONSE_BYTES: usize = 64 * 1024 * 1024;

pub(crate) fn append_bounded(target: &mut String, text: &str, limit: usize) {
    let remaining = limit.saturating_sub(target.len());
    if remaining == 0 {
        return;
    }
    let mut end = text.len().min(remaining);
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    target.push_str(&text[..end]);
}

pub(crate) fn bounded_stream_string(mut value: String, limit: usize) -> String {
    if value.len() <= limit {
        return value;
    }
    let mut end = limit;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value.truncate(end);
    value
}

#[derive(Debug, Default)]
pub(crate) struct AnthropicToolAccumulator {
    calls: Vec<PartialToolCall>,
}

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
    calls: Vec<PartialToolCall>,
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

