//! research — 自 src/agent/mod.rs 拆分。

use super::*;

pub(crate) fn tool_output_succeeded(output: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(output)
        .ok()
        .and_then(|value| {
            value
                .get("success")
                .and_then(serde_json::Value::as_bool)
                .or_else(|| value.get("ok").and_then(serde_json::Value::as_bool))
        })
        .unwrap_or(true)
}

#[derive(Debug)]
pub(crate) struct AutoArtifactCandidate {
    call_id: String,
    tool_name: String,
    path: PathBuf,
}

pub(crate) fn artifact_delivery_requested(messages: &[ChatMessage]) -> bool {
    let text = messages
        .iter()
        .rev()
        .find(|message| message.role == "user")
        .and_then(chat_message_text)
        .unwrap_or_default()
        .to_lowercase();
    let zh_action = ["生成", "创建", "制作", "导出", "保存为", "写一", "写个"]
        .iter()
        .any(|word| text.contains(word));
    let zh_deliverable = [
        "报告",
        "文档",
        "文件",
        "网页",
        "页面",
        "表格",
        "清单",
        "markdown",
        "md",
        "html",
        "json",
        "csv",
        "pdf",
        "代码文件",
        "独立脚本",
        "示例程序",
    ]
    .iter()
    .any(|word| text.contains(word));
    let en_action = ["create", "generate", "write", "make", "export", "save"]
        .iter()
        .any(|word| text.split_whitespace().any(|part| part == *word));
    let en_deliverable = [
        "report",
        "document",
        "file",
        "webpage",
        "page",
        "table",
        "spreadsheet",
        "markdown",
        "html",
        "json",
        "csv",
        "pdf",
        "script",
        "standalone program",
    ]
    .iter()
    .any(|word| text.contains(word));
    (zh_action && zh_deliverable) || (en_action && en_deliverable)
}

pub(crate) fn chat_message_text(message: &ChatMessage) -> Option<String> {
    match message.content.as_ref()? {
        ChatContent::Text(text) => Some(text.clone()),
        ChatContent::Parts(parts) => Some(
            parts
                .iter()
                .filter_map(|part| match part {
                    ChatContentPart::Text { text } => Some(text.as_str()),
                    ChatContentPart::ImageUrl { .. } => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
        ),
    }
}

pub(crate) fn artifact_candidate_paths(tool_name: &str, output: &str) -> Vec<PathBuf> {
    let Ok(payload) = serde_json::from_str::<Value>(output) else {
        return Vec::new();
    };
    let raw_paths = match tool_name {
        "write_file" if payload.get("created").and_then(Value::as_bool) == Some(true) => payload
            .get("path")
            .and_then(Value::as_str)
            .into_iter()
            .collect::<Vec<_>>(),
        "apply_patch" => payload
            .get("files")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|file| file.get("operation").and_then(Value::as_str) == Some("add"))
            .filter_map(|file| file.get("path").and_then(Value::as_str))
            .collect::<Vec<_>>(),
        _ => Vec::new(),
    };
    raw_paths
        .into_iter()
        .map(resolve_tool_output_path)
        .filter(|path| artifact_candidate_extension(path))
        .collect()
}

pub(crate) fn resolve_tool_output_path(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    let path = PathBuf::from(path);
    if path.is_absolute() {
        path
    } else {
        tools::workspace::effective_workdir().join(path)
    }
}

pub(crate) fn artifact_candidate_extension(path: &std::path::Path) -> bool {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    matches!(
        extension.as_str(),
        "md" | "markdown"
            | "html"
            | "htm"
            | "pdf"
            | "json"
            | "jsonl"
            | "csv"
            | "tsv"
            | "txt"
            | "log"
            | "css"
            | "js"
            | "jsx"
            | "ts"
            | "tsx"
            | "c"
            | "h"
            | "cpp"
            | "hpp"
            | "rs"
            | "py"
            | "sh"
            | "toml"
            | "yaml"
            | "yml"
            | "xml"
            | "sql"
    )
}

pub(crate) fn publish_auto_artifact_candidates<F>(
    candidates: &[AutoArtifactCandidate],
    on_event: &mut F,
) -> Result<()>
where
    F: FnMut(AgentEvent) -> Result<()>,
{
    let mut published = HashSet::new();
    for candidate in candidates {
        let key = candidate
            .path
            .canonicalize()
            .unwrap_or_else(|_| candidate.path.clone());
        if !published.insert(key) || !candidate.path.is_file() {
            continue;
        }
        on_event(AgentEvent::Artifact {
            call_id: candidate.call_id.clone(),
            name: candidate.tool_name.clone(),
            path: candidate.path.clone(),
            title: String::new(),
        })?;
    }
    Ok(())
}

#[derive(Default)]
pub(crate) struct UsageAccumulator {
    prompt_tokens: u64,
    completion_tokens: u64,
    total_tokens: u64,
    cache_read_tokens: u64,
    cache_write_tokens: u64,
    reasoning_tokens: u64,
    cache_reported: bool,
    has_usage: bool,
    pub(crate) estimated: bool,
}

impl UsageAccumulator {
    pub(crate) fn add_result(&mut self, result: &ChatResult, request_messages: &[ChatMessage]) {
        if let Some(usage) = &result.usage {
            self.add_usage(usage, false);
            return;
        }

        let prompt_tokens = overflow::estimate_messages_tokens(request_messages) as u64;
        let completion_tokens = estimate_result_tokens(result) as u64;
        self.add_usage(
            &Usage {
                prompt_tokens,
                completion_tokens,
                total_tokens: prompt_tokens.saturating_add(completion_tokens),
                ..Usage::default()
            },
            true,
        );
    }

    pub(crate) fn add_usage(&mut self, usage: &Usage, estimated: bool) {
        self.prompt_tokens = self.prompt_tokens.saturating_add(usage.prompt_tokens);
        self.completion_tokens = self
            .completion_tokens
            .saturating_add(usage.completion_tokens);
        let total = usage.effective_total_tokens();
        self.total_tokens = self.total_tokens.saturating_add(total);
        self.cache_read_tokens = self
            .cache_read_tokens
            .saturating_add(usage.cache_read_tokens);
        self.cache_write_tokens = self
            .cache_write_tokens
            .saturating_add(usage.cache_write_tokens);
        self.reasoning_tokens = self.reasoning_tokens.saturating_add(usage.reasoning_tokens);
        self.cache_reported |= usage.cache_reported;
        self.has_usage = true;
        self.estimated |= estimated;
    }

    pub(crate) fn usage(&self) -> Option<Usage> {
        self.has_usage.then_some(Usage {
            prompt_tokens: self.prompt_tokens,
            completion_tokens: self.completion_tokens,
            total_tokens: self.total_tokens,
            cache_read_tokens: self.cache_read_tokens,
            cache_write_tokens: self.cache_write_tokens,
            reasoning_tokens: self.reasoning_tokens,
            cache_reported: self.cache_reported,
            ..Usage::default()
        })
    }
}

pub(crate) fn queued_prompt_images(prompt: &QueuedPrompt) -> Result<Vec<Option<PastedImage>>> {
    prompt
        .attachments
        .iter()
        .map(|attachment| match attachment {
            QueuedPromptAttachment::Binary { mime, data_base64 } => {
                let data = base64::engine::general_purpose::STANDARD
                    .decode(data_base64)
                    .map_err(|error| anyhow::anyhow!("invalid queued image data: {error}"))?;
                Ok(Some(PastedImage::Binary(ClipboardImage::new(
                    mime.clone(),
                    data,
                ))))
            }
            QueuedPromptAttachment::Path { path } => Ok(Some(PastedImage::Path(path.clone()))),
        })
        .collect()
}

/// The fossilizable prefix of a transient tail: the contiguous run of
/// system-role text messages. Stops at the first non-system or non-text
/// message so redo checkpoints (which append loop messages) never leak
/// assistant/tool content into the fossil record.
/// Marks turn-context blocks that are standing advisories about recent state
/// (not about the current message): only these may be skipped when an
/// identical copy is already visible in a replayed fossil. Producers opt in
/// by using this prefix (reply_processor's long-reply conversion notice).
pub(crate) const STANDING_ADVISORY_PREFIX: &str = "[SystemInfo:";

/// True when `block`'s exact text already appears inside a user-role message
/// of the request being built (a fossilized turn tail replayed from an earlier
/// turn). Stops standing notices from re-fossilizing identical bytes every
/// turn; a block whose content changed no longer matches and is sent again.
pub(crate) fn turn_context_block_visible(messages: &[ChatMessage], block: &str) -> bool {
    messages.iter().any(|message| {
        message.role == "user"
            && matches!(
                message.content.as_ref(),
                Some(ChatContent::Text(text)) if text.contains(block)
            )
    })
}

/// Collects the associative-memory entry lines already visible in the request
/// being built. Fossilized blocks replay as `user` messages whose text starts
/// with the block tag, so matching on that prefix picks up exactly the earlier
/// injections (legacy `system` fossils are re-roled to `user` before this
/// point). Matching whole rendered lines means an updated memory — new content
/// or date — no longer matches and gets injected again.
pub(crate) fn visible_association_lines(messages: &[ChatMessage]) -> HashSet<&str> {
    let mut seen = HashSet::new();
    for message in messages {
        if message.role != "user" {
            continue;
        }
        let Some(ChatContent::Text(text)) = message.content.as_ref() else {
            continue;
        };
        if !text.starts_with("<associative-memory") {
            continue;
        }
        for line in text.lines() {
            if line.starts_with("- [") {
                seen.insert(line.trim_end());
            }
        }
    }
    seen
}

/// replay_start 之前最近一条非瞬态 user 消息=本轮真实用户输入的下标。
pub(crate) fn live_user_index(messages: &[ChatMessage], replay_start: usize) -> Option<usize> {
    let end = replay_start.min(messages.len());
    (0..end).rev().find(|&index| {
        let message = &messages[index];
        message.role == "user" && !message.transient_context
    })
}

/// 已拼装消息里最近一条以 `prefix` 开头的 user 侧文本(倒序首个)。
/// 不检查 transient 标志:回放化石反序列化后该标志会丢(serde skip),
/// 而以 `<runtime ` 开头的用户输入不存在——按内容前缀即可唯一识别。
pub(crate) fn last_fossil_with_prefix<'a>(
    messages: &'a [ChatMessage],
    prefix: &str,
) -> Option<&'a str> {
    messages.iter().rev().find_map(|message| {
        if message.role != "user" {
            return None;
        }
        match message.content.as_ref() {
            Some(ChatContent::Text(text)) if text.starts_with(prefix) => Some(text.as_str()),
            _ => None,
        }
    })
}

#[cfg(test)]
mod projection_tests {
    use super::*;

    #[test]
    pub(crate) fn runtime_projection_skips_byte_identical_fossil() {
        let stamp = "<runtime now=\"2026年08月16日 Sunday 00时\" cwd=\"/x\"/>";
        let mut messages = vec![
            ChatMessage::system("s"),
            ChatMessage::plain("user", "hi"),
            ChatMessage::turn_context(stamp.to_string()),
            ChatMessage::assistant("ok".to_string(), None),
        ];
        assert_eq!(last_fossil_with_prefix(&messages, "<runtime "), Some(stamp));
        // 变化才注入:相同→跳过,不同→追加。
        messages.push(ChatMessage::turn_context(
            "<runtime now=\"2026年08月16日 Sunday 01时\" cwd=\"/x\"/>".to_string(),
        ));
        assert_ne!(last_fossil_with_prefix(&messages, "<runtime "), Some(stamp));
    }
}

pub(crate) fn fossil_context_messages(tail: &[ChatMessage]) -> Vec<ChatMessage> {
    // Keyed on the explicit marker rather than the role: these blocks now ride
    // as `user` messages (see `ChatMessage::turn_context`), which is
    // indistinguishable by role from a real user turn.
    tail.iter()
        .take_while(|message| {
            message.transient_context
                && matches!(message.content.as_ref(), Some(ChatContent::Text(_)))
        })
        .cloned()
        .collect()
}

/// Fossils written before the role change are stored as `system`. Replaying
/// them verbatim would keep re-poisoning the prefix for the rest of the
/// session, so they are re-roled on the way out: one cold start at the upgrade
/// boundary, byte-stable forever after.
pub(crate) fn replay_fossil(message: &ChatMessage) -> ChatMessage {
    if message.role != "system" {
        return message.clone();
    }
    let mut message = message.clone();
    message.role = "user".to_string();
    message.transient_context = true;
    message
}

pub(crate) fn replace_request_mode_context(
    messages: &mut [ChatMessage],
    system_prompt: &str,
    mode: AgentMode,
    platform: bool,
) {
    if let Some(system) = messages.first_mut() {
        *system = ChatMessage::system(system_prompt);
    }
    // Role-agnostic on purpose: the live block is a `user` message now, while
    // fossils written before the change are still `system`.
    if let Some(runtime) = messages.iter_mut().rev().find(|message| {
        matches!(
            message.content.as_ref(),
            Some(ChatContent::Text(content)) if content.starts_with("<runtime now=")
        )
    }) {
        *runtime = ChatMessage::turn_context(runtime_context(mode, platform));
    }
}

pub(crate) fn continuation_system_prompt(system_prompt: &str, mode: AgentMode) -> String {
    let mode = match mode {
        AgentMode::Normal => "normal",
        AgentMode::Dev => "dev",
    };
    format!(
        "<mode-update active=\"{mode}\">This supersedes all earlier mode-specific instructions.</mode-update>\n\n{system_prompt}"
    )
}

pub(crate) fn estimate_result_tokens(result: &ChatResult) -> usize {
    let mut tokens = crate::token_estimate::estimate_tokens(&result.content);
    if let Some(reasoning) = &result.reasoning {
        tokens = tokens.saturating_add(crate::token_estimate::estimate_tokens(reasoning));
    }
    for call in &result.tool_calls {
        tokens = tokens.saturating_add(crate::token_estimate::estimate_tokens(&call.function.name));
        tokens = tokens.saturating_add(crate::token_estimate::estimate_tokens(
            &call.function.arguments,
        ));
    }
    tokens.max(1)
}

pub(crate) fn estimate_tool_definition_tokens(definitions: &[crate::llm::ToolDefinition]) -> usize {
    definitions
        .iter()
        .filter_map(|definition| serde_json::to_string(definition).ok())
        .map(|text| crate::token_estimate::estimate_tokens(&text))
        .sum()
}

/// Deterministic footprint extraction at tool-execution time: the only point
/// where tool arguments still exist (completed turns don't persist them).
/// Stub-mode lazy tools wrap real args in an `arguments` shell — unwrap it.
pub(crate) fn tool_call_footprint(
    name: &str,
    arguments: &str,
) -> Option<crate::state::ToolFootprint> {
    let mut args: serde_json::Value = serde_json::from_str(arguments).ok()?;
    if let Some(inner) = args.get("arguments") {
        if inner.is_object() {
            args = inner.clone();
        }
    }
    let mut footprint = crate::state::ToolFootprint::default();
    match name {
        "read_file" => {
            footprint
                .read
                .insert(args.get("path")?.as_str()?.trim().to_string());
        }
        "write_file" | "apply_patch" | "edit_string" => {
            footprint
                .modified
                .insert(args.get("path")?.as_str()?.trim().to_string());
        }
        "remember_fact" => {
            let content = args.get("content")?.as_str()?.trim();
            if content.is_empty() {
                return None;
            }
            let mut label: String = content.chars().take(80).collect();
            if content.chars().count() > 80 {
                label.push('…');
            }
            footprint.memories.insert(label);
        }
        _ => return None,
    }
    Some(footprint)
}

pub(crate) fn extract_persistable_tool_report(tool_name: &str, output: &str) -> Option<String> {
    let field = match tool_name {
        "create_artifact" | "apply_artifact_patch" | "present_artifact" => {
            return compact_artifact_tool_report(tool_name, output)
                .map(|report| wrap_previous_tool_report(tool_name, &report))
        }
        "load_tools" => {
            return compact_loaded_tools_report(output)
                .map(|report| wrap_previous_tool_report(tool_name, &report))
        }
        "show_meme" => return compact_sent_meme_report(output),
        "remember_fact" => {
            return compact_remembered_fact_report(output)
                .map(|report| wrap_previous_tool_report(tool_name, &report))
        }
        // 历史工具 id 兼容旧会话回放;新工具走下面两个主 id。
        "deep_research_linux_game_compatibility" | "deep_research_game_compatibility" => {
            "final_report"
        }
        "linux_input_method_diagnose" | "deep_diagnose" | "deep_research" => "final_answer",
        "task" => "result",
        _ => return None,
    };
    serde_json::from_str::<serde_json::Value>(output)
        .ok()
        .and_then(|value| {
            value
                .get(field)
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .map(str::to_string)
        })
        .map(|report| wrap_previous_tool_report(tool_name, &report))
        .filter(|report| !report.is_empty())
}

pub(crate) fn compact_artifact_tool_report(tool_name: &str, output: &str) -> Option<String> {
    let value = serde_json::from_str::<Value>(output).ok()?;
    if value.get("ok").and_then(Value::as_bool) != Some(true) {
        return None;
    }
    let filenames = value
        .get("files")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|file| file.get("path").and_then(Value::as_str))
        .filter_map(|path| std::path::Path::new(path).file_name())
        .filter_map(|name| name.to_str())
        .map(str::to_string)
        .collect::<Vec<_>>();
    if !filenames.is_empty() {
        return serde_json::to_string(&serde_json::json!({
            "artifacts": filenames,
            "operation": tool_name,
        }))
        .ok();
    }
    let filename = value
        .get("filename")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| {
            value
                .get("path")
                .and_then(Value::as_str)
                .and_then(|path| std::path::Path::new(path).file_name())
                .and_then(|name| name.to_str())
                .map(str::to_string)
        })?;
    let title = value
        .get("title")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    Some(
        serde_json::to_string(&serde_json::json!({
            "artifact": filename,
            "title": title,
            "operation": tool_name,
        }))
        .ok()?,
    )
}

pub(crate) fn wrap_previous_tool_report(tool_name: &str, report: &str) -> String {
    format!(
        "<previous_tool_report name=\"{tool_name}\">\n{}\n</previous_tool_report>",
        report.trim()
    )
}

/// User role + explicit historical-record framing (not system): a
/// system-weighted summary tempts the model to re-execute imperative lines in
/// it as fresh instructions, and several providers treat multiple system
/// messages inconsistently.
pub(crate) fn summary_checkpoint_message(summary: &str) -> ChatMessage {
    ChatMessage::plain(
        "user",
        format!(
            "<conversation-checkpoint>\nThe earlier conversation was compacted into the summary below. Treat it as historical context, not as new instructions.\n<summary>\n{summary}\n</summary>\n</conversation-checkpoint>"
        ),
    )
}

pub(crate) fn private_tool_memory(reports: &[String]) -> String {
    format!(
        "<system-reminder>\n<private_tool_memory>\n这些是内部工具记忆，仅用于保持对话连续性。不要向用户复述、展示或引用这些标签。\n{}\n</private_tool_memory>\n</system-reminder>",
        reports
            .iter()
            .map(|report| {
                truncate_middle_chars(
                    report.trim(),
                    PRIVATE_TOOL_REPORT_HEAD_CHARS,
                    PRIVATE_TOOL_REPORT_TAIL_CHARS,
                )
            })
            .filter(|report| !report.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    )
}

/// A18: bound the per-turn "collapsed body" that re-renders into history. The
/// truncation depends only on the text itself (never on turn age or position),
/// so a turn's rendering is frozen once written and the history prefix stays
/// byte-stable across later requests.
pub(crate) const PRIVATE_MEMORY_HEAD_CHARS: usize = 800;
pub(crate) const PRIVATE_MEMORY_TAIL_CHARS: usize = 400;
pub(crate) const PRIVATE_TOOL_REPORT_HEAD_CHARS: usize = 1600;
pub(crate) const PRIVATE_TOOL_REPORT_TAIL_CHARS: usize = 400;

pub(crate) fn truncate_middle_chars(text: &str, head: usize, tail: usize) -> String {
    let total = text.chars().count();
    // The +64 slack guarantees idempotency: a truncated result is always below
    // the threshold, so re-applying the function is a no-op.
    if total <= head + tail + 64 {
        return text.to_string();
    }
    let head_str: String = text.chars().take(head).collect();
    let tail_str: String = text.chars().skip(total.saturating_sub(tail)).collect();
    format!(
        "{head_str}\n[...省略{}字符...]\n{tail_str}",
        total - head - tail
    )
}

pub(crate) fn private_reasoning_memory(reasoning: &str) -> Option<String> {
    (!reasoning.trim().is_empty()).then(|| {
        let reasoning =
            truncate_middle_chars(reasoning, PRIVATE_MEMORY_HEAD_CHARS, PRIVATE_MEMORY_TAIL_CHARS);
        format!(
            "<system-reminder>\n<previous_assistant_reasoning>\n{reasoning}\n</previous_assistant_reasoning>\n这些是上一轮 assistant 已经产生的原始思考内容，用于继续工作；不要向用户复述这些标签。\n</system-reminder>"
        )
    })
}

/// dsh 式外溢替换文案的预算自洽拼装:先按最坏情况预扣提示文案的字节数,
/// 预览用剩余额度头尾对半(字符边界安全);连提示都放不下返回 None(放弃
/// 外溢保留原文——替换永不比原文更大)。
pub(crate) fn spill_replacement(output: &str, cap: usize, locator: &str) -> Option<String> {
    pub(crate) fn notice(omitted: usize, locator: &str) -> String {
        format!(
            "\n\n(已省略 {omitted} 字节。完整结果已存至: {locator} ——可用 read_file 配 offset/limit 分段读取,或用 run_command 里的 rg 检索。)"
        )
    }
    pub(crate) fn cut_at_boundary(text: &str, mut at: usize) -> usize {
        while at > 0 && !text.is_char_boundary(at) {
            at -= 1;
        }
        at
    }
    // 预扣:提示文案按最坏情况(省略数取全长的位数上界) + 头尾之间的 \n…\n 分隔符。
    let reserve = notice(output.len(), locator).len() + "\n…\n".len();
    if reserve >= cap {
        return None;
    }
    let budget = cap - reserve;
    let head_end = cut_at_boundary(output, budget / 2);
    let mut tail_start = output.len().saturating_sub(budget - budget / 2);
    while tail_start < output.len() && !output.is_char_boundary(tail_start) {
        tail_start += 1;
    }
    if tail_start <= head_end {
        return None;
    }
    let omitted = tail_start - head_end;
    Some(format!(
        "{}\n…\n{}{}",
        &output[..head_end],
        &output[tail_start..],
        notice(omitted, locator)
    ))
}

/// 完成时从本回合的实况消息尾段推导结构化工具流。以 messages 为唯一真相
/// (dsh "reconstructable requests":模型可见 ⟺ 可持久重建):assistant 带
/// tool_calls 即开一轮,其后按 call id 认领 role:"tool" 输出。被 length 截断
/// 拒执行的调用照录——它们的错误文案同样是模型看到的字节。任何悬空调用
/// (无输出)补占位,回放绝不发"无应答的 tool_calls"(provider 会 400)。
pub(crate) fn derive_tool_flow(
    messages: &[ChatMessage],
    live_start: usize,
) -> Vec<crate::state::ToolFlowRound> {
    let mut rounds: Vec<crate::state::ToolFlowRound> = Vec::new();
    for message in &messages[live_start.min(messages.len())..] {
        if message.role == "assistant" {
            if let Some(calls) = message
                .tool_calls
                .as_ref()
                .filter(|calls| !calls.is_empty())
            {
                rounds.push(crate::state::ToolFlowRound {
                    assistant_content: chat_message_text(message).unwrap_or_default(),
                    assistant_reasoning: message
                        .reasoning_content
                        .clone()
                        .filter(|reasoning| !reasoning.is_empty()),
                    calls: calls
                        .iter()
                        .map(|call| crate::state::ToolFlowCall {
                            id: call.id.clone(),
                            name: call.function.name.clone(),
                            arguments: call.function.arguments.clone(),
                            output: String::new(),
                        })
                        .collect(),
                });
            }
        } else if message.role == "tool" {
            if let (Some(call_id), Some(round)) = (message.tool_call_id.as_ref(), rounds.last_mut())
            {
                if let Some(call) = round
                    .calls
                    .iter_mut()
                    .find(|call| &call.id == call_id && call.output.is_empty())
                {
                    call.output = chat_message_text(message).unwrap_or_default();
                }
            }
        }
    }
    for round in &mut rounds {
        for call in &mut round.calls {
            if call.output.is_empty() {
                call.output = "(执行结果不可用)".to_string();
            }
        }
    }
    rounds
}

pub(crate) fn push_assistant_context_messages(
    messages: &mut Vec<ChatMessage>,
    content: &str,
    reasoning: Option<&str>,
    force_assistant_message: bool,
) {
    push_assistant_message_with_reasoning(
        messages,
        content.to_string(),
        reasoning,
        None,
        None,
        force_assistant_message,
    );
}

pub(crate) fn push_assistant_message_with_reasoning(
    messages: &mut Vec<ChatMessage>,
    content: String,
    reasoning: Option<&str>,
    thinking_signature: Option<&str>,
    tool_calls: Option<Vec<ToolCall>>,
    force_assistant_message: bool,
) {
    let has_tool_calls = tool_calls.as_ref().is_some_and(|calls| !calls.is_empty());
    if has_tool_calls {
        // A17: DeepSeek thinking mode requires the `reasoning_content` KEY on
        // assistant tool_calls turns of the live tool loop (an empty string is
        // accepted, a missing key is a 400). Carry it on the assistant message
        // itself; the provider adapter strips it for endpoints that do not
        // understand the field and rebuilds the Anthropic thinking block from
        // the signature where present.
        let mut message = ChatMessage::assistant(content, tool_calls);
        message.reasoning_content = Some(reasoning.unwrap_or_default().to_string());
        message.thinking_signature = thinking_signature.map(str::to_string);
        messages.push(message);
        return;
    }
    // 跨轮思考回放退役(验收 08-16):正常完成轮的正式回复已承载结论,
    // 思维链副本纯属冗余——官方语义 reasoning 是轮内产物(普通轮回传被
    // API 忽略),dsh 同款丢弃。中断恢复不走这里:journal 专道
    // (interrupted_turn_replay_messages)仍原样重放中断前的思考。
    let _ = reasoning;
    if force_assistant_message || !content.trim().is_empty() {
        messages.push(ChatMessage::assistant(content, None));
    }
}

pub(crate) fn turn_context_tokens(turn: &crate::state::Turn) -> usize {
    let mut messages = vec![ChatMessage::plain("user", &turn.user_content)];
    // Fossilized transient tail is replayed with the turn, so count it.
    messages.extend(turn.context_messages.iter().cloned());
    // 与 push_history_turn 同步:有结构化 flow 的回合问答对不再回放。
    let replay_exchanges: &[crate::question::QuestionExchange] = if turn.tool_flow.is_empty() {
        &turn.question_exchanges
    } else {
        &[]
    };
    for exchange in replay_exchanges {
        messages.push(ChatMessage::plain(
            "assistant",
            crate::question::assistant_exchange_text(exchange),
        ));
        messages.push(ChatMessage::plain(
            "user",
            crate::question::user_exchange_text(exchange),
        ));
    }
    for followup in &turn.followups {
        push_assistant_context_messages(
            &mut messages,
            followup
                .preceding_assistant_content
                .as_deref()
                .unwrap_or_default(),
            followup.preceding_assistant_reasoning.as_deref(),
            false,
        );
        messages.push(ChatMessage::plain("user", &followup.content));
    }
    push_assistant_context_messages(
        &mut messages,
        &turn.assistant_content,
        turn.assistant_reasoning.as_deref(),
        true,
    );
    if !turn.tool_reports.is_empty() {
        messages.push(ChatMessage::turn_context(private_tool_memory(
            &turn.tool_reports,
        )));
    }
    overflow::estimate_messages_tokens(&messages)
}

pub(crate) fn followup_assistant_replay_content(
    followup: &crate::state::TurnFollowup,
) -> Option<&str> {
    followup
        .preceding_assistant_content
        .as_deref()
        .filter(|content| !content.trim().is_empty())
        .or_else(|| {
            followup
                .preceding_assistant_reasoning
                .as_deref()
                .filter(|reasoning| !reasoning.trim().is_empty())
        })
}

pub(crate) fn interrupted_turn_replay_messages(
    agent: &Agent,
    turn: &crate::state::Turn,
) -> Vec<ChatMessage> {
    let mut messages = Vec::new();
    messages.push(ChatMessage::turn_context(
        "<interrupted-turn-recovery>上一轮回复已中断。以下内容是中断前已经持久化的模型输出和工具进度；不要重新执行已经完成的工具，基于这些内容继续处理当前用户请求。</interrupted-turn-recovery>",
    ));

    // A redo revision only journals the new branch. Preserve the already
    // committed clarification/follow-up prefix from the turn row before
    // replaying the new branch's events.
    let replayed_prompt_ids = turn
        .journal_events
        .iter()
        .filter(|event| event.kind == "queued_prompts_consumed")
        .flat_map(|event| {
            event
                .text_payload
                .as_deref()
                .and_then(|payload| serde_json::from_str::<Vec<String>>(payload).ok())
                .unwrap_or_default()
        })
        .collect::<HashSet<_>>();
    if turn.revision > 0 {
        let prefix_question_count = turn
            .journal_events
            .iter()
            .find(|event| event.kind == "redo_prefix_question_count")
            .and_then(|event| event.text_payload.as_deref())
            .and_then(|count| count.parse::<usize>().ok())
            .unwrap_or_else(|| {
                let branch_answers = turn
                    .journal_events
                    .iter()
                    .filter(|event| {
                        event.kind == "tool_result"
                            && event.name.as_deref() == Some("ask_question")
                            && event
                                .text_payload
                                .as_deref()
                                .and_then(|payload| serde_json::from_str::<Value>(payload).ok())
                                .and_then(|payload| {
                                    payload
                                        .get("status")
                                        .and_then(Value::as_str)
                                        .map(|status| status == "answered")
                                })
                                .unwrap_or(false)
                    })
                    .count();
                turn.question_exchanges.len().saturating_sub(branch_answers)
            });
        for exchange in turn.question_exchanges.iter().take(prefix_question_count) {
            messages.push(ChatMessage::plain(
                "assistant",
                crate::question::assistant_exchange_text(exchange),
            ));
            messages.push(ChatMessage::plain(
                "user",
                crate::question::user_exchange_text(exchange),
            ));
        }
        for followup in &turn.followups {
            if replayed_prompt_ids.contains(&followup.prompt_id) {
                continue;
            }
            push_assistant_context_messages(
                &mut messages,
                followup
                    .preceding_assistant_content
                    .as_deref()
                    .unwrap_or_default(),
                followup.preceding_assistant_reasoning.as_deref(),
                false,
            );
            messages.push(agent.followup_user_message(followup));
        }
    }

    let mut assistant_text = String::new();
    let mut assistant_reasoning = String::new();
    let mut pending_calls = Vec::<ToolCall>::new();
    let mut open_calls = Vec::<ToolCall>::new();
    let mut progress = HashMap::<String, String>::new();
    let mut command_tail = HashMap::<String, Vec<u8>>::new();

    for event in &turn.journal_events {
        match event.kind.as_str() {
            "assistant_content" => {
                if let Some(text) = &event.text_payload {
                    assistant_text.push_str(text);
                }
            }
            "assistant_reasoning" => {
                if let Some(text) = &event.text_payload {
                    assistant_reasoning.push_str(text);
                }
            }
            "reasoning_reset" => assistant_reasoning.clear(),
            "tool_call" => {
                let Some(call_id) = event.call_id.clone() else {
                    continue;
                };
                let Some(name) = event.name.as_deref() else {
                    continue;
                };
                pending_calls.push(ToolCall {
                    id: call_id,
                    kind: "function".to_string(),
                    function: ToolCallFunction {
                        name: replay_tool_function_name(name),
                        arguments: event.text_payload.clone().unwrap_or_default(),
                    },
                });
            }
            "tool_result" => {
                open_calls.extend(flush_interrupted_assistant(
                    &mut messages,
                    &mut assistant_reasoning,
                    &mut assistant_text,
                    &mut pending_calls,
                ));
                if let Some(call_id) = &event.call_id {
                    let output = event.text_payload.as_deref().unwrap_or_default();
                    messages.push(ChatMessage::tool(call_id, truncate_chars(output, 48_000)));
                    open_calls.retain(|call| call.id != *call_id);
                    progress.remove(call_id);
                    command_tail.remove(call_id);
                }
            }
            "tool_progress" => {
                if let Some(call_id) = &event.call_id {
                    progress.insert(
                        call_id.clone(),
                        truncate_chars(event.text_payload.as_deref().unwrap_or_default(), 4_000),
                    );
                }
            }
            "command_stdout" | "command_stderr" => {
                if let Some(call_id) = &event.call_id {
                    let tail = command_tail.entry(call_id.clone()).or_default();
                    if let Some(bytes) = &event.blob_payload {
                        tail.extend_from_slice(bytes);
                        pub(crate) const MAX_COMMAND_TAIL: usize = 8 * 1024;
                        if tail.len() > MAX_COMMAND_TAIL {
                            let start = tail.len() - MAX_COMMAND_TAIL;
                            tail.drain(..start);
                        }
                    }
                }
            }
            "queued_prompts_consumed" => {
                open_calls.extend(flush_interrupted_assistant(
                    &mut messages,
                    &mut assistant_reasoning,
                    &mut assistant_text,
                    &mut pending_calls,
                ));
                append_interrupted_tool_results(
                    &mut messages,
                    &mut open_calls,
                    &mut progress,
                    &mut command_tail,
                );
                let prompt_ids = event
                    .text_payload
                    .as_deref()
                    .and_then(|payload| serde_json::from_str::<Vec<String>>(payload).ok())
                    .unwrap_or_default();
                for prompt_id in prompt_ids {
                    if let Some(followup) = turn
                        .followups
                        .iter()
                        .find(|followup| followup.prompt_id == prompt_id)
                    {
                        messages.push(agent.followup_user_message(followup));
                    }
                }
            }
            _ => {}
        }
    }

    open_calls.extend(flush_interrupted_assistant(
        &mut messages,
        &mut assistant_reasoning,
        &mut assistant_text,
        &mut pending_calls,
    ));
    append_interrupted_tool_results(
        &mut messages,
        &mut open_calls,
        &mut progress,
        &mut command_tail,
    );
    messages
}

pub(crate) fn flush_interrupted_assistant(
    messages: &mut Vec<ChatMessage>,
    assistant_reasoning: &mut String,
    assistant_text: &mut String,
    pending_calls: &mut Vec<ToolCall>,
) -> Vec<ToolCall> {
    if assistant_reasoning.trim().is_empty()
        && assistant_text.trim().is_empty()
        && pending_calls.is_empty()
    {
        return Vec::new();
    }
    if !assistant_reasoning.trim().is_empty() {
        if let Some(reasoning) = private_reasoning_memory(assistant_reasoning) {
            messages.push(ChatMessage::turn_context(reasoning));
        }
    }
    assistant_reasoning.clear();
    let text = std::mem::take(assistant_text);
    let calls = std::mem::take(pending_calls);
    let replay_calls = (!calls.is_empty()).then(|| calls.clone());
    messages.push(ChatMessage::assistant(text, replay_calls));
    calls
}

pub(crate) fn append_interrupted_tool_results(
    messages: &mut Vec<ChatMessage>,
    open_calls: &mut Vec<ToolCall>,
    progress: &mut HashMap<String, String>,
    command_tail: &mut HashMap<String, Vec<u8>>,
) {
    for call in std::mem::take(open_calls) {
        let mut output =
            "tool execution was interrupted before a final result was persisted".to_string();
        if let Some(message) = progress.remove(&call.id) {
            output.push_str("\nlast progress: ");
            output.push_str(&message);
        }
        if let Some(bytes) = command_tail.remove(&call.id) {
            let tail = String::from_utf8_lossy(&bytes);
            if !tail.trim().is_empty() {
                output.push_str("\nlast command output:\n");
                output.push_str(&truncate_chars(&tail, 8_000));
            }
        }
        messages.push(ChatMessage::tool(call.id, output));
    }
}

pub(crate) fn replay_tool_function_name(name: &str) -> String {
    match name.split_once(':').map(|(prefix, _)| prefix) {
        Some("load_skill") | Some("load_tools") | Some("task") => {
            name.split(':').next().unwrap_or(name).to_string()
        }
        _ => name.to_string(),
    }
}

pub(crate) fn redo_checkpoint_payload(
    messages: &[ChatMessage],
    replay_start: usize,
    base_tool_reports: &[String],
    pending_tool_reports: &[(String, String)],
    tool_rounds: usize,
    question_rounds: usize,
) -> TurnRedoCheckpointPayload {
    let mut prefix_tool_reports = Vec::with_capacity(
        base_tool_reports
            .len()
            .saturating_add(pending_tool_reports.len()),
    );
    prefix_tool_reports.extend(base_tool_reports.iter().cloned());
    prefix_tool_reports.extend(
        pending_tool_reports
            .iter()
            .map(|(_, report)| report.clone()),
    );
    TurnRedoCheckpointPayload {
        replay_messages: messages.get(replay_start..).unwrap_or_default().to_vec(),
        prefix_tool_reports,
        tool_rounds,
        question_rounds,
        loaded_items: Vec::new(),
        prefix_question_count: 0,
        prefix_image_asset_ids: Vec::new(),
        prefix_artifact_asset_ids: Vec::new(),
    }
}

pub(crate) fn evicted_turn_entries(
    turns: &[crate::state::Turn],
) -> (Vec<crate::state::StoredConversationEntry>, Vec<EvictedTurn>) {
    let mut entries = Vec::new();
    let mut evicted = Vec::new();
    for turn in turns {
        entries.push(crate::state::StoredConversationEntry {
            timestamp: turn.user_timestamp.clone(),
            role: "user".to_string(),
            content: turn.user_content.clone(),
            reasoning: None,
        });
        evicted.push(EvictedTurn {
            source_id: format!("{}:user", turn.turn_id),
            timestamp: turn.user_timestamp.clone(),
            role: "user".to_string(),
            content: turn.user_content.clone(),
            ..EvictedTurn::default()
        });

        for (index, exchange) in turn.question_exchanges.iter().enumerate() {
            let timestamp = exchange.answered_at.clone();
            let assistant_content = crate::question::assistant_exchange_text(exchange);
            entries.push(crate::state::StoredConversationEntry {
                timestamp: timestamp.clone(),
                role: "assistant_clarification".to_string(),
                content: assistant_content.clone(),
                reasoning: None,
            });
            evicted.push(EvictedTurn {
                source_id: format!("{}:question:{index}", turn.turn_id),
                timestamp: timestamp.clone(),
                role: "assistant".to_string(),
                content: assistant_content,
                ..EvictedTurn::default()
            });
            let user_content = crate::question::user_exchange_text(exchange);
            entries.push(crate::state::StoredConversationEntry {
                timestamp: timestamp.clone(),
                role: "user_clarification".to_string(),
                content: user_content.clone(),
                reasoning: None,
            });
            evicted.push(EvictedTurn {
                source_id: format!("{}:answer:{index}", turn.turn_id),
                timestamp,
                role: "user".to_string(),
                content: user_content,
                ..EvictedTurn::default()
            });
        }

        for followup in &turn.followups {
            if followup_assistant_replay_content(followup).is_some() {
                let content = followup
                    .preceding_assistant_content
                    .clone()
                    .unwrap_or_default();
                entries.push(crate::state::StoredConversationEntry {
                    timestamp: followup.submitted_at.clone(),
                    role: "assistant".to_string(),
                    content: content.clone(),
                    reasoning: followup.preceding_assistant_reasoning.clone(),
                });
                evicted.push(EvictedTurn {
                    source_id: format!("{}:before:{}", turn.turn_id, followup.prompt_id),
                    timestamp: followup.submitted_at.clone(),
                    role: "assistant".to_string(),
                    content,
                    ..EvictedTurn::default()
                });
            }
            entries.push(crate::state::StoredConversationEntry {
                timestamp: followup.submitted_at.clone(),
                role: "user".to_string(),
                content: followup.content.clone(),
                reasoning: None,
            });
            evicted.push(EvictedTurn {
                source_id: format!("{}:followup:{}", turn.turn_id, followup.prompt_id),
                timestamp: followup.submitted_at.clone(),
                role: "user".to_string(),
                content: followup.content.clone(),
                ..EvictedTurn::default()
            });
        }

        let timestamp = turn.assistant_timestamp.clone().unwrap_or_default();
        entries.push(crate::state::StoredConversationEntry {
            timestamp: timestamp.clone(),
            role: "assistant".to_string(),
            content: turn.assistant_content.clone(),
            reasoning: turn.assistant_reasoning.clone(),
        });
        evicted.push(EvictedTurn {
            source_id: format!("{}:assistant", turn.turn_id),
            timestamp: timestamp.clone(),
            role: "assistant".to_string(),
            content: turn.assistant_content.clone(),
            ..EvictedTurn::default()
        });

        for (index, report) in turn.tool_reports.iter().enumerate() {
            entries.push(crate::state::StoredConversationEntry {
                timestamp: timestamp.clone(),
                role: "assistant".to_string(),
                content: report.clone(),
                reasoning: None,
            });
            evicted.push(EvictedTurn {
                source_id: format!("{}:tool:{index}", turn.turn_id),
                timestamp: timestamp.clone(),
                role: "assistant".to_string(),
                content: report.clone(),
                ..EvictedTurn::default()
            });
        }
    }
    (entries, evicted)
}

pub(crate) fn archive_and_delete_visible_turns(
    state: &StateStore,
    memory: &MemoryStore,
    turns: &[crate::state::Turn],
) -> Result<Vec<crate::state::StoredConversationEntry>> {
    archive_and_delete_visible_turns_checked(state, memory, turns, None)
}

pub(crate) fn archive_and_delete_visible_turns_checked(
    state: &StateStore,
    memory: &MemoryStore,
    turns: &[crate::state::Turn],
    expected_loaded_tools: Option<&[(String, Option<String>)]>,
) -> Result<Vec<crate::state::StoredConversationEntry>> {
    let (entries, mut evicted) = evicted_turn_entries(turns);
    memory.apply_evicted_ownership(&mut evicted);
    let turn_ids = turns
        .iter()
        .map(|turn| turn.turn_id.clone())
        .collect::<Vec<_>>();
    if let Some(archive_db) = memory.prepare_evicted_context_db()? {
        state.archive_and_delete_visible_turns(
            &archive_db,
            &evicted,
            &turn_ids,
            expected_loaded_tools,
        )?;
    } else if expected_loaded_tools.is_some() {
        state.delete_visible_turns_checked(&turn_ids, expected_loaded_tools)?;
    } else {
        state.delete_visible_turns(&turn_ids)?;
    }
    Ok(entries)
}

pub(crate) fn compact_remembered_fact_report(output: &str) -> Option<String> {
    let value = serde_json::from_str::<Value>(output).ok()?;
    if value.get("ok").and_then(Value::as_bool) != Some(true) {
        return None;
    }
    let content = value.get("content").and_then(Value::as_str)?.trim();
    if content.is_empty() {
        return None;
    }
    let mut report = serde_json::json!({
        "remembered_fact": {
            "content": content,
        }
    });
    if let Some(id) = value.get("id").and_then(Value::as_i64) {
        report["remembered_fact"]["id"] = serde_json::json!(id);
    }
    if let Some(source) = value.get("source").and_then(Value::as_str) {
        let source = source.trim();
        if !source.is_empty() {
            report["remembered_fact"]["source"] = serde_json::json!(source);
        }
    }
    serde_json::to_string(&report).ok()
}

pub(crate) fn compact_loaded_tools_report(output: &str) -> Option<String> {
    let value = serde_json::from_str::<Value>(output).ok()?;
    let names = value
        .get("loaded_tools")
        .and_then(Value::as_array)?
        .iter()
        .filter_map(|item| {
            item.as_str()
                .or_else(|| item.get("name").and_then(Value::as_str))
        })
        .map(str::to_string)
        .collect::<Vec<_>>();
    if names.is_empty() {
        return None;
    }
    serde_json::to_string(&serde_json::json!({ "loaded_tools": names })).ok()
}

#[derive(Default)]
pub(crate) struct LoadedItems {
    pub(crate) targets: Vec<String>,
    pub(crate) tools: Vec<String>,
}

pub(crate) fn loaded_items_from_output(output: &str) -> LoadedItems {
    let Ok(value) = serde_json::from_str::<Value>(output) else {
        return LoadedItems::default();
    };
    let targets = value
        .get("loaded_targets")
        .and_then(Value::as_array)
        .map(|items| string_array_items(items))
        .unwrap_or_default();
    let tools = value
        .get("loaded_tools")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    item.as_str()
                        .or_else(|| item.get("name").and_then(Value::as_str))
                })
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    LoadedItems { targets, tools }
}

pub(crate) fn string_array_items(items: &[Value]) -> Vec<String> {
    items
        .iter()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .collect()
}

pub(crate) fn compact_sent_meme_report(output: &str) -> Option<String> {
    pub(crate) const MAX_DESCRIPTION_CHARS: usize = 120;

    let value = serde_json::from_str::<Value>(output).ok()?;
    if value.get("success").and_then(Value::as_bool) != Some(true) {
        return None;
    }
    let id = value.get("id").and_then(Value::as_str)?.trim();
    if id.is_empty() {
        return None;
    }
    let description = value
        .get("description")
        .and_then(Value::as_str)
        .map(compact_one_line)
        .filter(|description| !description.is_empty())
        .map(|description| truncate_chars(&description, MAX_DESCRIPTION_CHARS));
    let id = xml_text_escape(id);
    match description {
        Some(description) => Some(format!(
            "<sent_meme>发送了一个表情包：id={}；description={}</sent_meme>",
            id,
            xml_text_escape(&description)
        )),
        None => Some(format!("<sent_meme>发送了一个表情包：id={id}</sent_meme>")),
    }
}
