//! core — 自 src/llm/openai_compatible.rs 拆分。

use super::*;

use super::{
    ChatMessage, ChatResult, ChatStreamChunk, ChatStreamKind, ResponsesContinuation, ToolCall,
    ToolCallFunction, ToolDefinition, Usage,
};
use crate::config::{AppConfig, ProviderConfig};
use crate::default_models::OPENCODE_ZEN_BASE_URL;
use crate::i18n::text as t;
use crate::models_cache::{self, ModelReasoningInfo, ReasoningSetting, ReasoningVariant};
use crate::paths::GQYPaths;
use anyhow::{bail, Context, Result};
use futures_util::{Stream, StreamExt};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Write};
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub(crate) static TOOL_CALL_COUNTER: AtomicU64 = AtomicU64::new(0);
pub(crate) static LLM_REQUEST_COUNTER: AtomicU64 = AtomicU64::new(0);
pub(crate) static LLM_SCHEDULER: LazyLock<Mutex<LlmScheduler>> =
    LazyLock::new(|| Mutex::new(LlmScheduler::default()));

pub(crate) const TRANSPORT_RETRY_DELAY: Duration = Duration::from_millis(250);
pub(crate) const MAX_SEND_ATTEMPTS: usize = 3;
/// Attempts a request gets before giving up, however few endpoints exist. With
/// several endpoints these are failovers; with one they are plain retries.
pub(crate) const MIN_ENDPOINT_ATTEMPTS: usize = 3;
#[cfg(not(test))]
pub(crate) const HTTP_STATUS_RETRY_INITIAL_DELAY: Duration = Duration::from_secs(2);
#[cfg(test)]
pub(crate) const HTTP_STATUS_RETRY_INITIAL_DELAY: Duration = Duration::from_millis(10);
#[cfg(not(test))]
pub(crate) const HTTP_STATUS_RETRY_MAX_DELAY: Duration = Duration::from_secs(120);
#[cfg(test)]
pub(crate) const HTTP_STATUS_RETRY_MAX_DELAY: Duration = Duration::from_millis(120);

pub(crate) const CHAT_RESERVED_BODY_KEYS: &[&str] = &[
    "model",
    "messages",
    "temperature",
    "stream",
    "stream_options",
    "tools",
    "chat_template_kwargs",
];
pub(crate) const RESPONSES_RESERVED_BODY_KEYS: &[&str] = &[
    "model",
    "input",
    "instructions",
    "previous_response_id",
    "stream",
    "tools",
    "reasoning",
    "temperature",
];
pub(crate) const ANTHROPIC_RESERVED_BODY_KEYS: &[&str] = &[
    "model",
    "system",
    "messages",
    "tools",
    "stream",
    "max_tokens",
    "temperature",
    "thinking",
];

pub(crate) fn sanitize_extra_body(
    extra: Option<Map<String, Value>>,
    reserved_keys: &[&str],
) -> Option<Map<String, Value>> {
    let mut extra = extra?;
    for key in reserved_keys {
        extra.remove(*key);
    }
    (!extra.is_empty()).then_some(extra)
}

pub(crate) fn merge_extra_body(
    base: Option<Map<String, Value>>,
    overlay: Option<Map<String, Value>>,
) -> Option<Map<String, Value>> {
    let mut base = base.unwrap_or_default();
    for (key, value) in overlay.unwrap_or_default() {
        match base.get_mut(&key) {
            Some(existing) => merge_json_value(existing, value),
            None => {
                base.insert(key, value);
            }
        }
    }
    (!base.is_empty()).then_some(base)
}

pub(crate) fn merge_json_value(base: &mut Value, overlay: Value) {
    if let (Some(base), Some(overlay)) = (base.as_object_mut(), overlay.as_object()) {
        for (key, value) in overlay {
            match base.get_mut(key) {
                Some(existing) => merge_json_value(existing, value.clone()),
                None => {
                    base.insert(key.clone(), value.clone());
                }
            }
        }
    } else {
        *base = overlay;
    }
}

pub(crate) fn gen_tool_call_id() -> String {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let n = TOOL_CALL_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("call_{ts}_{n}")
}

pub(crate) fn gen_llm_request_id() -> String {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let n = LLM_REQUEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("llm_{ts}_{n}")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TransportFailureKind {
    Connect,
    Timeout,
    Other,
}

impl std::fmt::Display for TransportFailureKind {
    pub(crate) fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Connect => "connect",
            Self::Timeout => "timeout",
            Self::Other => "request",
        })
    }
}

pub(crate) fn retryable_transport_failure(kind: TransportFailureKind) -> bool {
    kind == TransportFailureKind::Connect
}

pub(crate) fn retryable_http_status(status: u16) -> bool {
    (500..=599).contains(&status)
}

pub(crate) fn http_status_retry_delay(attempt: usize) -> Duration {
    HTTP_STATUS_RETRY_INITIAL_DELAY
        .saturating_mul(1 << attempt.saturating_sub(1).min(6))
        .min(HTTP_STATUS_RETRY_MAX_DELAY)
}

#[derive(Debug)]
pub(crate) struct TransportFailure {
    stage: &'static str,
    kind: TransportFailureKind,
}

impl std::fmt::Display for TransportFailure {
    pub(crate) fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} transport failed ({})", self.stage, self.kind)
    }
}

impl std::error::Error for TransportFailure {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HttpFailureKind {
    Status,
    Authentication,
    RateLimit,
    EndpointUnavailable,
    EndpointIncompatible,
    InvalidRequest,
}

impl std::fmt::Display for HttpFailureKind {
    pub(crate) fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Status => "status",
            Self::Authentication => "authentication",
            Self::RateLimit => "rate_limit",
            Self::EndpointUnavailable => "endpoint_unavailable",
            Self::EndpointIncompatible => "endpoint_incompatible",
            Self::InvalidRequest => "invalid_request",
        })
    }
}

#[derive(Debug)]
pub(crate) struct HttpStatusFailure {
    status: u16,
    kind: HttpFailureKind,
}

impl HttpStatusFailure {
    pub(crate) fn classify(status: u16, body: &str) -> Self {
        let kind = match status {
            401 | 403 => HttpFailureKind::Authentication,
            429 => HttpFailureKind::RateLimit,
            408 | 500..=599 => HttpFailureKind::Status,
            _ => classify_provider_error_body(body).unwrap_or(HttpFailureKind::Status),
        };
        Self { status, kind }
    }
}

pub(crate) fn classify_provider_error_body(body: &str) -> Option<HttpFailureKind> {
    let structured = serde_json::from_str::<Value>(body).ok();
    let error = structured
        .as_ref()
        .and_then(|value| value.get("error"))
        .or(structured.as_ref());
    let mut signals = Vec::with_capacity(3);
    if let Some(error) = error {
        for field in ["code", "type", "status", "message"] {
            if let Some(value) = error.get(field).and_then(Value::as_str) {
                signals.push(normalize_error_signal(value));
            }
        }
    }
    if signals.is_empty() {
        signals.push(normalize_error_signal(body));
    }

    for signal in &signals {
        if contains_any(
            signal,
            &[
                "invalid_api_key",
                "incorrect_api_key",
                "authentication",
                "unauthorized",
                "forbidden",
                "permission_denied",
            ],
        ) {
            return Some(HttpFailureKind::Authentication);
        }
    }
    for signal in &signals {
        if contains_any(
            signal,
            &["rate_limit", "ratelimit", "quota", "too_many_requests"],
        ) {
            return Some(HttpFailureKind::RateLimit);
        }
    }
    for signal in &signals {
        if contains_any(
            signal,
            &[
                "model_not_found",
                "model_not_available",
                "model_unavailable",
                "unsupported_model",
                "deployment_not_found",
                "model_access_denied",
                "no_available_provider",
                "provider_unavailable",
                "upstream_request_failed",
                "service_unavailable",
                "overloaded",
            ],
        ) {
            return Some(HttpFailureKind::EndpointUnavailable);
        }
    }
    for signal in &signals {
        if contains_any(
            signal,
            &[
                "context_length",
                "context_window",
                "max_tokens",
                "unsupported_parameter",
                "unknown_parameter",
                "unsupported_feature",
                "not_supported",
            ],
        ) {
            return Some(HttpFailureKind::EndpointIncompatible);
        }
    }
    for signal in &signals {
        if contains_any(
            signal,
            &[
                "invalid_request",
                "invalid_argument",
                "malformed",
                "validation_error",
            ],
        ) {
            return Some(HttpFailureKind::InvalidRequest);
        }
    }
    None
}

pub(crate) fn normalize_error_signal(value: &str) -> String {
    let mut normalized = String::with_capacity(value.len());
    let mut separator = false;
    let bytes = value.as_bytes();
    for (index, byte) in bytes.iter().copied().enumerate() {
        if byte.is_ascii_alphanumeric() {
            let previous = index
                .checked_sub(1)
                .and_then(|index| bytes.get(index))
                .copied();
            let next = bytes.get(index + 1).copied();
            let camel_case_boundary = byte.is_ascii_uppercase()
                && previous.is_some_and(|previous| {
                    previous.is_ascii_lowercase()
                        || previous.is_ascii_digit()
                        || (previous.is_ascii_uppercase()
                            && next.is_some_and(|next_byte| next_byte.is_ascii_lowercase()))
                });
            if camel_case_boundary && !separator && !normalized.is_empty() {
                normalized.push('_');
            }
            normalized.push((byte as char).to_ascii_lowercase());
            separator = false;
        } else if !separator && !normalized.is_empty() {
            normalized.push('_');
            separator = true;
        }
    }
    if normalized.ends_with('_') {
        normalized.pop();
    }
    normalized
}

pub(crate) fn contains_any(value: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| value.contains(needle))
}

impl std::fmt::Display for HttpStatusFailure {
    pub(crate) fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "upstream returned HTTP {}", self.status)
    }
}

impl std::error::Error for HttpStatusFailure {}

pub(crate) fn format_error_chain(error: &(dyn std::error::Error + 'static)) -> String {
    let mut message = error.to_string();
    let mut source = error.source();
    while let Some(error) = source {
        message.push_str(": ");
        message.push_str(&error.to_string());
        source = error.source();
    }
    message
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProviderProtocol {
    Auto,
    OpenAiChat,
    OpenAiResponses,
    Anthropic,
}

impl ProviderProtocol {
    pub(crate) fn from_provider(provider: &ProviderConfig) -> Result<Self> {
        match provider.protocol.trim().to_ascii_lowercase().as_str() {
            "" | "auto" => Ok(Self::Auto),
            "openai-chat" => Ok(Self::OpenAiChat),
            "openai-responses" => Ok(Self::OpenAiResponses),
            "anthropic" | "anthropic-messages" | "claude" | "claude-messages" => {
                Ok(Self::Anthropic)
            }
            protocol => bail!("unsupported provider protocol: {protocol}"),
        }
    }
}

pub(crate) fn effective_protocol(provider: &ProviderConfig, model: &str) -> Result<ProviderProtocol> {
    match ProviderProtocol::from_provider(provider)? {
        ProviderProtocol::Auto if provider_looks_anthropic(provider) => {
            Ok(ProviderProtocol::Anthropic)
        }
        ProviderProtocol::Auto if uses_openai_responses(model) => {
            Ok(ProviderProtocol::OpenAiResponses)
        }
        ProviderProtocol::Auto => Ok(ProviderProtocol::OpenAiChat),
        protocol => Ok(protocol),
    }
}

pub(crate) fn uses_openai_responses(model: &str) -> bool {
    let model = model.to_ascii_lowercase();
    model.starts_with("gpt-5")
        || model.starts_with("o1")
        || model.starts_with("o3")
        || model.starts_with("o4")
}

pub(crate) fn is_openrouter_provider(provider: &ProviderConfig) -> bool {
    provider.id.eq_ignore_ascii_case("openrouter")
        || provider
            .base_url
            .to_ascii_lowercase()
            .contains("openrouter.ai")
}

pub(crate) fn uses_enable_thinking(provider: &ProviderConfig, info: &ModelReasoningInfo) -> bool {
    info.provider_npm.as_deref() == Some("@ai-sdk/alibaba")
        || provider.id.to_ascii_lowercase().contains("alibaba")
        || provider
            .base_url
            .to_ascii_lowercase()
            .contains("dashscope.aliyuncs.com")
}

pub(crate) fn anthropic_reasoning_budget(max_tokens: u32, requested: u64) -> Option<u64> {
    (max_tokens > 1024 && requested < u64::from(max_tokens)).then_some(requested)
}

pub(crate) fn supported_reasoning_variants(provider: &ProviderConfig, model: &str) -> Vec<ReasoningVariant> {
    let Some(info) = models_cache::reasoning_info(&provider.id, model) else {
        return Vec::new();
    };
    info.variants
        .iter()
        .filter(|variant| reasoning_variant_supported(provider, model, &info, variant))
        .cloned()
        .collect()
}

pub(crate) fn reasoning_variant_supported(
    provider: &ProviderConfig,
    model: &str,
    info: &ModelReasoningInfo,
    variant: &ReasoningVariant,
) -> bool {
    let Ok(protocol) = effective_protocol(provider, model) else {
        return false;
    };
    reasoning_variant_supported_for_protocol(provider, info, variant, protocol)
}

pub(crate) fn reasoning_variant_supported_for_protocol(
    provider: &ProviderConfig,
    info: &ModelReasoningInfo,
    variant: &ReasoningVariant,
    protocol: ProviderProtocol,
) -> bool {
    match protocol {
        ProviderProtocol::OpenAiResponses => matches!(
            variant.setting,
            ReasoningSetting::Effort(_) | ReasoningSetting::Toggle(_) | ReasoningSetting::Disabled
        ),
        ProviderProtocol::Anthropic => match variant.setting {
            ReasoningSetting::BudgetTokens(budget) => {
                anthropic_reasoning_budget(provider.anthropic_max_tokens, budget).is_some()
            }
            _ => true,
        },
        ProviderProtocol::OpenAiChat | ProviderProtocol::Auto => {
            let npm = info.provider_npm.as_deref().unwrap_or_default();
            if is_openrouter_provider(provider) || npm == "@openrouter/ai-sdk-provider" {
                matches!(
                    variant.setting,
                    ReasoningSetting::Effort(_) | ReasoningSetting::BudgetTokens(_)
                )
            } else if matches!(variant.setting, ReasoningSetting::Effort(_)) {
                true
            } else if uses_enable_thinking(provider, info) {
                matches!(variant.setting, ReasoningSetting::Toggle(_))
            } else {
                false
            }
        }
    }
}

pub(crate) fn thinking_variant_key(provider_id: &str, model: &str) -> String {
    format!("{provider_id}\t{model}")
}

pub(crate) fn rename_thinking_variant_entries<T>(
    entries: &mut HashMap<String, T>,
    old_id: &str,
    new_id: &str,
) {
    let prefix = format!("{old_id}\t");
    let renamed = entries
        .keys()
        .filter_map(|key| {
            key.strip_prefix(&prefix)
                .map(|model| (key.clone(), thinking_variant_key(new_id, model)))
        })
        .collect::<Vec<_>>();
    for (old_key, new_key) in renamed {
        if let Some(value) = entries.remove(&old_key) {
            entries.insert(new_key, value);
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct ThinkingVariantPreferences {
    #[serde(default)]
    selected: HashMap<String, String>,
    #[serde(skip)]
    changes: HashMap<String, Option<String>>,
    #[serde(skip)]
    provider_renames: Vec<(String, String)>,
}

pub(crate) fn thinking_variant_preferences_file(paths: &GQYPaths) -> PathBuf {
    paths.state_dir.join("thinking-variants.json")
}

pub(crate) fn lock_thinking_variant_preferences(paths: &GQYPaths) -> Result<File> {
    let lock_path = paths.state_dir.join("thinking-variants.lock");
    let lock = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(&lock_path)
        .with_context(|| {
            format!(
                "failed to open thinking variant lock: {}",
                lock_path.display()
            )
        })?;
    let result = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) };
    if result != 0 {
        return Err(std::io::Error::last_os_error()).with_context(|| {
            format!(
                "failed to lock thinking variant state: {}",
                lock_path.display()
            )
        });
    }
    Ok(lock)
}

pub(crate) fn load_thinking_variant_preferences(paths: &GQYPaths) -> ThinkingVariantPreferences {
    ThinkingVariantPreferences::load(paths)
}

impl ThinkingVariantPreferences {
    pub(crate) fn load(paths: &GQYPaths) -> Self {
        Self::load_for_update(paths).unwrap_or_default()
    }

    pub(crate) fn load_for_update(paths: &GQYPaths) -> Result<Self> {
        let path = thinking_variant_preferences_file(paths);
        match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).with_context(|| {
                format!("failed to parse thinking variant state: {}", path.display())
            }),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error).with_context(|| {
                format!("failed to read thinking variant state: {}", path.display())
            }),
        }
    }

    pub(crate) fn selected(&self, provider_id: &str, model: &str) -> Option<&str> {
        self.selected
            .get(&thinking_variant_key(provider_id, model))
            .map(String::as_str)
    }

    pub(crate) fn set(&mut self, provider_id: &str, model: &str, selected: Option<String>) {
        let key = thinking_variant_key(provider_id, model);
        let selected = selected.filter(|value| !value.trim().is_empty());
        if self.selected.get(&key).map(String::as_str) == selected.as_deref() {
            return;
        }
        if let Some(selected) = &selected {
            self.selected.insert(key.clone(), selected.clone());
        } else {
            self.selected.remove(&key);
        }
        self.changes.insert(key, selected);
    }

    pub(crate) fn rename_provider(&mut self, old_id: &str, new_id: &str) {
        if old_id == new_id {
            return;
        }
        rename_thinking_variant_entries(&mut self.selected, old_id, new_id);
        rename_thinking_variant_entries(&mut self.changes, old_id, new_id);
        self.provider_renames
            .push((old_id.to_string(), new_id.to_string()));
    }

    /// True when `save` would write anything to disk.
    pub(crate) fn is_dirty(&self) -> bool {
        !self.changes.is_empty() || !self.provider_renames.is_empty()
    }

    pub(crate) fn save(&self, paths: &GQYPaths) -> Result<()> {
        if self.changes.is_empty() && self.provider_renames.is_empty() {
            return Ok(());
        }

        let path = thinking_variant_preferences_file(paths);
        let parent = path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("thinking variant state path has no parent"))?;
        std::fs::create_dir_all(parent)?;
        let _lock = lock_thinking_variant_preferences(paths)?;
        let mut persisted = Self::load_for_update(paths)?;
        for (old_id, new_id) in &self.provider_renames {
            rename_thinking_variant_entries(&mut persisted.selected, old_id, new_id);
        }
        for (key, selected) in &self.changes {
            if let Some(selected) = selected {
                persisted.selected.insert(key.clone(), selected.clone());
            } else {
                persisted.selected.remove(key);
            }
        }
        let mut temp = tempfile::NamedTempFile::new_in(parent)?;
        temp.write_all(serde_json::to_string_pretty(&persisted)?.as_bytes())?;
        temp.persist(path).map_err(|error| error.error)?;
        Ok(())
    }
}

pub(crate) fn chat_variant_body(
    provider: &ProviderConfig,
    info: &ModelReasoningInfo,
    setting: ReasoningSetting,
) -> Option<Map<String, Value>> {
    let npm = info.provider_npm.as_deref().unwrap_or_default();
    match setting {
        ReasoningSetting::Effort(effort)
            if is_openrouter_provider(provider) || npm == "@openrouter/ai-sdk-provider" =>
        {
            Some(
                json!({ "reasoning": { "effort": effort } })
                    .as_object()?
                    .clone(),
            )
        }
        ReasoningSetting::BudgetTokens(budget)
            if is_openrouter_provider(provider) || npm == "@openrouter/ai-sdk-provider" =>
        {
            Some(
                json!({ "reasoning": { "max_tokens": budget } })
                    .as_object()?
                    .clone(),
            )
        }
        ReasoningSetting::Effort(effort) => {
            Some(json!({ "reasoning_effort": effort }).as_object()?.clone())
        }
        ReasoningSetting::Toggle(enabled) if uses_enable_thinking(provider, info) => {
            Some(json!({ "enable_thinking": enabled }).as_object()?.clone())
        }
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ThinkingVariantOptions {
    pub provider_id: String,
    pub model: String,
    pub variants: Vec<String>,
    pub selected: Option<String>,
}

pub(crate) fn thinking_variant_options_for_model(
    provider: &ProviderConfig,
    model: &str,
    selected: Option<&str>,
) -> ThinkingVariantOptions {
    let variants = supported_reasoning_variants(provider, model)
        .into_iter()
        .map(|variant| variant.id)
        .collect::<Vec<_>>();
    let selected = selected
        .filter(|selected| variants.iter().any(|variant| variant == *selected))
        .map(str::to_string);
    ThinkingVariantOptions {
        provider_id: provider.id.clone(),
        model: model.to_string(),
        variants,
        selected,
    }
}

/// Responses 续传健康位(任务#16 自愈)。跨 clone 共享:压缩器等辅助克隆
/// 与主客户端看到同一份;置位即进程内立即生效,并持久化到
/// provider-capabilities.json 供后续会话读取。多供应商混池时按主
/// provider 记录(续传本就钉在单端点上,混池仅有过度抑制的轻微风险)。
#[derive(Clone)]
pub(crate) struct ResponsesContinuationHealth {
    unsupported: Arc<std::sync::atomic::AtomicBool>,
    store: std::path::PathBuf,
    base_url: String,
    provider_id: String,
}

impl ResponsesContinuationHealth {
    pub(crate) fn for_provider(paths: &GQYPaths, provider: &ProviderConfig) -> Self {
        let store = crate::llm::provider_capabilities::store_path(&paths.cache_dir);
        let unsupported = crate::llm::provider_capabilities::continuation_unsupported(
            &store,
            &provider.base_url,
        );
        Self {
            unsupported: Arc::new(std::sync::atomic::AtomicBool::new(unsupported)),
            store,
            base_url: provider.base_url.clone(),
            provider_id: provider.id.clone(),
        }
    }

    /// 测试用:无持久化、乐观放行。
    pub(crate) fn detached() -> Self {
        Self {
            unsupported: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            store: std::path::PathBuf::new(),
            base_url: String::new(),
            provider_id: String::new(),
        }
    }
}

#[derive(Clone)]
pub struct OpenAiCompatibleClient {
    client: Client,
    provider: ProviderConfig,
    api_key: String,
    endpoints: Arc<Vec<LlmEndpoint>>,
    thinking_variants: HashMap<String, String>,
    reasoning_visibility: ReasoningVisibility,
    /// True when partial output never reaches a person mid-request — platform
    /// turns buffer a round and post it as one message. Nothing is committed
    /// until the round ends, so a dropped stream can be retried invisibly.
    buffered_delivery: bool,
    detailed_reasoning_summary: bool,
    request_timeouts: Option<RequestTimeouts>,
    /// Per-clone completion cap. Auxiliary callers (compaction summaries)
    /// clone the client and set this so a runaway summary cannot eat the
    /// window; None leaves the provider default untouched.
    max_tokens_override: Option<u32>,
    continuation_health: ResponsesContinuationHealth,
    /// Scope tag for the per-request cache accounting log ("chat", "qq-judge",
    /// "compact", …). Auxiliary callers override it via `with_request_scope`
    /// so cache stats separate the main conversation from side channels.
    request_scope: &'static str,
}

#[derive(Clone, Copy)]
pub(crate) struct RequestTimeouts {
    response_header: Duration,
    stream_idle: Duration,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ReasoningVisibility {
    Hidden,
    Summary,
    Full,
}

#[derive(Clone)]
pub(crate) struct LlmEndpoint {
    client: Client,
    provider: ProviderConfig,
    api_key: String,
    key_index: usize,
}

impl LlmEndpoint {
    pub(crate) fn id(&self) -> String {
        endpoint_id(
            &self.provider.id,
            &self.provider.default_model,
            self.key_index,
        )
    }
}

#[derive(Default)]
pub(crate) struct LlmScheduler {
    cursor: usize,
    cooldowns: HashMap<String, Instant>,
}

impl LlmScheduler {
    pub(crate) fn ordered_indices(&mut self, endpoints: &[LlmEndpoint]) -> Vec<usize> {
        let available = endpoints
            .iter()
            .enumerate()
            .filter_map(|(index, endpoint)| self.is_ready(&endpoint.id()).then_some(index))
            .collect::<Vec<_>>();
        if available.is_empty() {
            return Vec::new();
        }
        let start = self.cursor % available.len();
        self.cursor = self.cursor.wrapping_add(1);
        rotate_from(available, start)
    }

    pub(crate) fn is_ready(&mut self, id: &str) -> bool {
        match self.cooldowns.get(id).copied() {
            Some(until) if until > Instant::now() => false,
            Some(_) => {
                self.cooldowns.remove(id);
                true
            }
            None => true,
        }
    }

    /// The endpoint whose cooldown lifts first, for the single probe sent when
    /// every endpoint is cooling down. `None` sorts ahead of any deadline, so
    /// an endpoint with no cooldown recorded wins outright.
    pub(crate) fn soonest_ready_index(&self, endpoints: &[LlmEndpoint]) -> Option<usize> {
        endpoints
            .iter()
            .enumerate()
            .min_by_key(|(_, endpoint)| self.cooldowns.get(&endpoint.id()).copied())
            .map(|(index, _)| index)
    }

    pub(crate) fn mark_success(&mut self, id: &str) {
        self.cooldowns.remove(id);
    }

    pub(crate) fn mark_failure(&mut self, id: String, duration: Duration) {
        self.cooldowns.insert(id, Instant::now() + duration);
    }
}

pub(crate) fn rotate_from<T>(mut items: Vec<T>, start: usize) -> Vec<T> {
    items.rotate_left(start);
    items
}

pub(crate) fn endpoint_id(provider_id: &str, model: &str, key_index: usize) -> String {
    format!("{provider_id}\t{model}\t{key_index}")
}

pub(crate) fn ordered_endpoint_indices(endpoints: &[LlmEndpoint]) -> Vec<usize> {
    LLM_SCHEDULER
        .lock()
        .map(|mut scheduler| scheduler.ordered_indices(endpoints))
        .unwrap_or_else(|_| (0..endpoints.len()).collect())
}

pub(crate) fn soonest_ready_endpoint_index(endpoints: &[LlmEndpoint]) -> Option<usize> {
    LLM_SCHEDULER
        .lock()
        .ok()
        .and_then(|scheduler| scheduler.soonest_ready_index(endpoints))
        .or_else(|| (!endpoints.is_empty()).then_some(0))
}

pub(crate) fn mark_endpoint_success(endpoint: &LlmEndpoint) {
    if let Ok(mut scheduler) = LLM_SCHEDULER.lock() {
        scheduler.mark_success(&endpoint.id());
    }
}

pub(crate) fn mark_endpoint_failure(endpoint: &LlmEndpoint, error: &anyhow::Error) -> Option<Duration> {
    let duration = cooldown_for_error(error)?;
    let mut scheduler = LLM_SCHEDULER.lock().ok()?;
    scheduler.mark_failure(endpoint.id(), duration);
    Some(duration)
}

pub(crate) fn cooldown_for_status(status: u16) -> Option<Duration> {
    match status {
        401 | 403 | 429 => Some(Duration::from_secs(600)),
        408 | 500..=599 => Some(Duration::from_secs(120)),
        _ => None,
    }
}

pub(crate) fn cooldown_for_error(error: &anyhow::Error) -> Option<Duration> {
    if let Some(failure) = error.downcast_ref::<HttpStatusFailure>() {
        return match failure.kind {
            HttpFailureKind::Authentication | HttpFailureKind::RateLimit => {
                Some(Duration::from_secs(600))
            }
            HttpFailureKind::EndpointUnavailable => Some(Duration::from_secs(120)),
            HttpFailureKind::EndpointIncompatible | HttpFailureKind::InvalidRequest => None,
            HttpFailureKind::Status => cooldown_for_status(failure.status),
        };
    }
    if error.downcast_ref::<TransportFailure>().is_some() {
        return Some(Duration::from_secs(120));
    }
    error
        .downcast_ref::<reqwest::Error>()
        .filter(|error| error.is_connect() || error.is_timeout())
        .map(|_| Duration::from_secs(120))
}

pub(crate) fn endpoint_failover_allowed(error: &anyhow::Error) -> bool {
    !error
        .downcast_ref::<HttpStatusFailure>()
        .is_some_and(|failure| failure.kind == HttpFailureKind::InvalidRequest)
}

/// Whether the *same* endpoint may be tried again inside one request. A 429 or
/// a rejected key is a verdict on that provider/model/key, not a moment in
/// time: the retries `MIN_ENDPOINT_ATTEMPTS` pads in would fire back-to-back
/// with no backoff and spend more of a quota that already said no — which on a
/// shared free tier is what exhausted it. Failover to a *different* endpoint is
/// unaffected; that is `endpoint_failover_allowed`'s job.
pub(crate) fn same_endpoint_retry_allowed(error: &anyhow::Error) -> bool {
    !error
        .downcast_ref::<HttpStatusFailure>()
        .is_some_and(|failure| {
            matches!(
                failure.kind,
                HttpFailureKind::Authentication | HttpFailureKind::RateLimit
            )
        })
}

pub(crate) fn endpoint_client(provider: &ProviderConfig) -> Result<Client> {
    // Auxiliary callers (judge/affection/organizer) rebuild their client per
    // call; without this cache every judge run pays fresh TLS setup and loses
    // connection reuse. Keyed by every input the builder consumes, so a config
    // edit that changes the timeout naturally mints a new client; the map is
    // bounded by the number of distinct providers. `reqwest::Client` is an Arc
    // handle — clones share one pool.
    pub(crate) static CLIENTS: std::sync::OnceLock<std::sync::Mutex<HashMap<(String, u64), Client>>> =
        std::sync::OnceLock::new();
    let timeout = provider.timeout_seconds.clamp(5, 30);
    let key = (provider.id.clone(), timeout);
    let mut cache = CLIENTS
        .get_or_init(|| std::sync::Mutex::new(HashMap::new()))
        .lock()
        .unwrap();
    if let Some(client) = cache.get(&key) {
        return Ok(client.clone());
    }
    let client = Client::builder()
        .connect_timeout(Duration::from_secs(timeout))
        .build()
        .with_context(|| format!("building HTTP client for provider {}", provider.id))?;
    cache.insert(key, client.clone());
    Ok(client)
}

pub(crate) fn llm_endpoints(config: &AppConfig, paths: &GQYPaths) -> Result<Vec<LlmEndpoint>> {
    let mut endpoints = Vec::new();
    let mut errors = Vec::new();
    for choice in config.active_provider_model_choices() {
        let mut provider = config.provider(Some(&choice.provider_id))?.clone();
        provider.default_model = choice.model;
        let client = endpoint_client(&provider)?;
        match provider.resolved_api_keys(paths) {
            Ok(keys) => {
                for key in keys {
                    endpoints.push(LlmEndpoint {
                        client: client.clone(),
                        provider: provider.clone(),
                        api_key: key.value,
                        key_index: key.index,
                    });
                }
            }
            Err(err) => errors.push(format!(
                "{} / {}: {err}",
                provider.id, provider.default_model
            )),
        }
    }
    if endpoints.is_empty() {
        bail!(
            "no active provider/model endpoint is configured:\n- {}",
            errors.join("\n- ")
        )
    }
    Ok(endpoints)
}

