//! tasks — 自 src/agent/mod.rs 拆分。

pub(crate) use super::*;

pub(crate) fn compact_one_line(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(crate) fn truncate_chars(value: &str, max_chars: usize) -> String {
    let mut output = String::new();
    for (index, ch) in value.chars().enumerate() {
        if index >= max_chars {
            output.push('…');
            return output;
        }
        output.push(ch);
    }
    output
}

pub(crate) fn xml_text_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

pub(crate) fn loaded_tools_from_messages(messages: &[ChatMessage]) -> BTreeSet<String> {
    let mut loaded = BTreeSet::new();
    for message in messages {
        let Some(ChatContent::Text(text)) = message.content.as_ref() else {
            continue;
        };
        collect_loaded_tools_from_text(text, &mut loaded);
    }
    loaded
}

pub(crate) fn collect_loaded_tools_from_text(text: &str, loaded: &mut BTreeSet<String>) {
    let mut rest = text;
    let start_tag = "<previous_tool_report name=\"load_tools\">";
    let end_tag = "</previous_tool_report>";
    while let Some(start) = rest.find(start_tag) {
        let body_start = start + start_tag.len();
        let Some(end) = rest[body_start..].find(end_tag) else {
            break;
        };
        let body = &rest[body_start..body_start + end];
        if let Ok(value) = serde_json::from_str::<Value>(body.trim()) {
            if let Some(names) = value.get("loaded_tools").and_then(Value::as_array) {
                for name in names.iter().filter_map(Value::as_str) {
                    if !name.trim().is_empty() {
                        loaded.insert(name.trim().to_string());
                    }
                }
            }
        }
        rest = &rest[body_start + end + end_tag.len()..];
    }
}

pub(crate) fn tool_event_name(name: &str, arguments: &str) -> String {
    let Ok(args) = serde_json::from_str::<Value>(arguments) else {
        return name.to_string();
    };
    match name {
        "load_skill" => args
            .get("name")
            .and_then(Value::as_str)
            .map(|skill| format!("load_skill:{skill}"))
            .unwrap_or_else(|| name.to_string()),
        "load_tools" => args
            .get("names")
            .and_then(Value::as_array)
            .map(|names| {
                names
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .filter(|tools| !tools.is_empty())
            .map(|tools| format!("load_tools:{tools}"))
            .unwrap_or_else(|| name.to_string()),
        // Each subagent gets a distinct event name so concurrent task calls
        // render as separate status lines instead of one aggregated counter.
        "task" => args
            .get("description")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|description| !description.is_empty())
            .map(|description| {
                let truncated: String = description.chars().take(32).collect();
                format!("task:{truncated}")
            })
            .unwrap_or_else(|| name.to_string()),
        _ => name.to_string(),
    }
}

pub(crate) fn clipboard_binary_image_from_tool_result(
    tool_name: &str,
    output: &str,
) -> Option<ClipboardImage> {
    if tool_name != "read_clipboard" {
        return None;
    }
    let value = serde_json::from_str::<Value>(output).ok()?;
    if value.get("ok").and_then(Value::as_bool) != Some(true) {
        return None;
    }
    if value.get("kind").and_then(Value::as_str) != Some("clipboard") {
        return None;
    }
    if value.get("content_type").and_then(Value::as_str) != Some("image") {
        return None;
    }
    if value.get("source").and_then(Value::as_str) != Some("clipboard_binary") {
        return None;
    }
    let path = value.get("path").and_then(Value::as_str)?;
    let mime = value
        .get("mime")
        .and_then(Value::as_str)
        .unwrap_or("image/png")
        .to_string();
    let data = std::fs::read(path).ok()?;
    Some(ClipboardImage::new(mime, data))
}

pub(crate) fn resolve_pasted_image_paths(
    images: &[Option<PastedImage>],
    paths: &GQYPaths,
    image_platform: Option<&str>,
) -> Vec<Option<String>> {
    images
        .iter()
        .enumerate()
        .map(|(i, image)| match image {
            Some(PastedImage::Binary(img)) => image_platform
                .map(|platform| {
                    img.write_cache_file(
                        &paths.cache_dir,
                        &PathBuf::from("platform_images").join(platform),
                    )
                })
                .unwrap_or_else(|| img.write_temp_file(&paths.cache_dir, i + 1))
                .ok()
                .map(|path| path.display().to_string()),
            Some(PastedImage::Path(path)) => Some(path.clone()),
            None => None,
        })
        .collect()
}

pub(crate) fn rewrite_image_placeholders_with_paths(
    input: &str,
    paths: &[Option<String>],
) -> String {
    let mut output = String::new();
    let mut rest = input;
    while let Some(start) = rest.find("[Image ") {
        output.push_str(&rest[..start]);
        let after_start = &rest[start..];
        let Some(end) = after_start.find(']') else {
            output.push_str(after_start);
            return output;
        };
        let placeholder = &after_start[..=end];
        if let Some(index) = image_placeholder_index(placeholder) {
            if let Some(Some(path)) = paths.get(index - 1) {
                output.push_str(&format!("[Image {index}: {path}]"));
            } else {
                output.push_str(placeholder);
            }
        } else {
            output.push_str(placeholder);
        }
        rest = &after_start[end + 1..];
    }
    output.push_str(rest);
    output
}

pub(crate) fn image_placeholder_index(placeholder: &str) -> Option<usize> {
    let inner = placeholder
        .strip_prefix("[Image ")?
        .strip_suffix(']')?
        .trim_start();
    let num: String = inner.chars().take_while(|c| c.is_ascii_digit()).collect();
    let index = num.parse::<usize>().ok()?;
    (index > 0).then_some(index)
}

pub(crate) fn vision_analysis_progress(tick: usize) -> String {
    let dots = match tick % 3 {
        1 => ".",
        2 => "..",
        _ => "...",
    };
    if crate::i18n::is_zh() {
        format!("视觉分析{dots}")
    } else {
        format!("Vision analysis{dots}")
    }
}

pub(crate) fn with_mode_reminder(system_prompt: String, mode: AgentMode) -> String {
    let mut prompt = system_prompt;
    if let Some(reminder) = mode.reminder() {
        prompt.push_str("\n\n");
        prompt.push_str(reminder);
    }
    prompt
}

pub(crate) fn with_runtime_system_context(mut system_prompt: String, context: &[String]) -> String {
    for item in context
        .iter()
        .map(String::as_str)
        .filter(|item| !item.is_empty())
    {
        system_prompt.push_str("\n\n");
        system_prompt.push_str(item);
    }
    system_prompt
}

/// Appends the static host block to the stable prefix.
///
/// It belongs here rather than in the per-turn `<runtime …/>` tail: the tail is
/// fossilized into `turns.context_messages` and replayed byte-for-byte by every
/// later turn, so a process-constant put there is re-sent once per turn and
/// piles up in the request; in the system prompt it is paid once and then
/// served from the provider's prefix cache.
///
/// Only owner sessions get it. A QQ reply has no use for kernel versions, and
/// skipping the append outright — rather than adding an empty block — keeps
/// those sessions' system prompt byte-identical to what the provider already
#[allow(clippy::empty_line_after_doc_comments)]
/// has cached, so the platform side sees no cold start at all.

/// 模式选提示词源:Dev=一行可编辑开发提示词(无人格全家、无用户身份,
/// 极简原则);Normal=人格提示词(按 audience 附用户档案)。
pub(crate) fn mode_system_prompt(
    config: &AppConfig,
    paths: &GQYPaths,
    mode: AgentMode,
    audience: PromptAudience,
) -> Result<String> {
    match mode {
        AgentMode::Dev => config.dev_system_prompt(paths),
        AgentMode::Normal => config.system_prompt_for(paths, audience),
    }
}

pub(crate) fn with_host_environment(
    mut system_prompt: String,
    audience: PromptAudience,
    paths: &GQYPaths,
    mode: AgentMode,
) -> String {
    if audience != PromptAudience::Owner {
        return system_prompt;
    }
    system_prompt.push_str("\n\n");
    system_prompt.push_str(&crate::host_info::host_environment_block(&paths.root_dir));
    // 渲染能力说明(仅 owner 会话):终端与 WebUI 都支持 LaTeX。
    // 不放人格提示词里——QQ 等平台的排版能力不同,不该看到这段。
    // dev 也不带:极简原则,编码任务用不上排版说明(验收 08-16 解剖)。
    if mode != AgentMode::Dev {
        system_prompt.push_str(
            "\n\n输出数学公式时使用 LaTeX:重要公式用块级定界符(`$$…$$` 或 `\\[…\\]`,独立成段)会渲染成排版图;行内用 `$…$` 或 `\\(…\\)`,会转写为 Unicode 数学文本;表格单元格内的公式同样支持,分式会排成上下结构。不要用裸 Unicode 或 ASCII 手拼公式。",
        );
    }
    system_prompt
}

pub(crate) fn active_text_pool_supports_vision(config: &AppConfig) -> bool {
    let choices = config.active_provider_model_choices();
    !choices.is_empty()
        && choices.iter().all(|choice| {
            config.model_supports_any_input(&choice.provider_id, &choice.model, &["image"])
        })
}

pub(crate) fn should_use_active_text_pool_for_images(config: &AppConfig) -> bool {
    config.plugins.vision.prefer_current_multimodal_model
        && active_text_pool_supports_vision(config)
}

#[derive(Default)]
pub(crate) struct ReasoningTitleFilter {
    pending: String,
    decided: bool,
    trim_body_prefix: bool,
}

impl ReasoningTitleFilter {
    pub(crate) fn push(&mut self, text: &str) -> (Option<String>, Option<String>) {
        if self.decided {
            let text = if self.trim_body_prefix {
                let text = text.trim_start_matches(['\r', '\n']);
                if text.is_empty() {
                    return (None, None);
                }
                self.trim_body_prefix = false;
                text
            } else {
                text
            };
            return (None, (!text.is_empty()).then(|| text.to_string()));
        }
        self.pending.push_str(text);
        let trimmed = self.pending.trim_start();
        if "**".starts_with(trimmed) {
            return (None, None);
        }
        if let Some(body) = trimmed.strip_prefix("**") {
            let Some(close) = body.find("**") else {
                if trimmed.chars().count() <= 160 {
                    return (None, None);
                }
                return self.release_without_title();
            };
            let title = clean_reasoning_title(&body[..close]);
            let suffix = &body[close + 2..];
            if only_line_breaks(suffix) {
                return self.finish_decision(title, String::new());
            }
            if !suffix.starts_with("\n\n") && !suffix.starts_with("\r\n\r\n") {
                return self.release_without_title();
            }
            let rest = suffix.trim_start_matches(['\r', '\n']).to_string();
            return self.finish_decision(title, rest);
        }
        if possible_markdown_heading_prefix(trimmed) {
            return (None, None);
        }
        if let Some(title_start) = markdown_heading_content_start(trimmed) {
            let Some(end) = trimmed.find('\n') else {
                if trimmed.chars().count() <= 160 {
                    return (None, None);
                }
                return self.release_without_title();
            };
            let suffix = &trimmed[end + 1..];
            if only_line_breaks(suffix) {
                return (None, None);
            }
            let title = clean_reasoning_title(&trimmed[title_start..end]);
            let rest = suffix.trim_start_matches(['\r', '\n']).to_string();
            return self.finish_decision(title, rest);
        }
        self.release_without_title()
    }

    pub(crate) fn finish_decision(
        &mut self,
        title: String,
        rest: String,
    ) -> (Option<String>, Option<String>) {
        self.pending.clear();
        self.decided = true;
        self.trim_body_prefix = rest.is_empty();
        (
            (!title.is_empty()).then_some(title),
            (!rest.is_empty()).then_some(rest),
        )
    }

    pub(crate) fn release_without_title(&mut self) -> (Option<String>, Option<String>) {
        self.decided = true;
        (None, Some(std::mem::take(&mut self.pending)))
    }

    pub(crate) fn finish(&mut self) -> (Option<String>, Option<String>) {
        if self.pending.is_empty() {
            return (None, None);
        }
        self.decided = true;
        let pending = std::mem::take(&mut self.pending);
        let trimmed = pending.trim_start();
        if let Some(body) = trimmed.strip_prefix("**") {
            if let Some(close) = body.find("**") {
                let suffix = &body[close + 2..];
                if suffix.is_empty()
                    || ((suffix.starts_with("\n\n") || suffix.starts_with("\r\n\r\n"))
                        && only_line_breaks(suffix))
                {
                    let title = clean_reasoning_title(&body[..close]);
                    return ((!title.is_empty()).then_some(title), None);
                }
            }
        }
        if let Some(title_start) = markdown_heading_content_start(trimmed) {
            let title = clean_reasoning_title(&trimmed[title_start..]);
            return ((!title.is_empty()).then_some(title), None);
        }
        (None, Some(trimmed.to_string()))
    }
}

pub(crate) fn possible_markdown_heading_prefix(text: &str) -> bool {
    !text.is_empty() && text.len() <= 6 && text.bytes().all(|byte| byte == b'#')
}

pub(crate) fn only_line_breaks(text: &str) -> bool {
    text.bytes().all(|byte| matches!(byte, b'\r' | b'\n'))
}

pub(crate) fn markdown_heading_content_start(text: &str) -> Option<usize> {
    let hashes = text.bytes().take_while(|byte| *byte == b'#').count();
    if !(1..=6).contains(&hashes) {
        return None;
    }
    let rest = text.get(hashes..)?;
    let whitespace = rest
        .bytes()
        .take_while(|byte| matches!(*byte, b' ' | b'\t'))
        .count();
    (whitespace > 0).then_some(hashes + whitespace)
}

pub(crate) fn clean_reasoning_title(value: &str) -> String {
    let value = compact_one_line(value);
    let value = value.trim_matches(['*', '#', ' ', '\t', '.', '。', '!', '！', '?', '？']);
    truncate_chars(value, 80)
}

pub(crate) fn emit_filtered_chunk_at<F>(
    chunk: ChatStreamChunk,
    received_at: Instant,
    filter: &mut ReasoningTitleFilter,
    on_event: &mut F,
) -> Result<()>
where
    F: FnMut(AgentEvent) -> Result<()>,
{
    match chunk.kind {
        ChatStreamKind::ReasoningPartStart => {
            *filter = ReasoningTitleFilter::default();
            on_event(AgentEvent::ReasoningPartStart { received_at })?;
        }
        ChatStreamKind::ReasoningReset => {
            *filter = ReasoningTitleFilter::default();
            on_event(AgentEvent::ReasoningReset { received_at })?;
        }
        ChatStreamKind::ReasoningPartEnd => {
            let (title, text) = filter.finish();
            if let Some(title) = title {
                on_event(AgentEvent::ReasoningTitle(title))?;
            }
            if let Some(text) = text {
                on_event(AgentEvent::Chunk(ChatStreamChunk {
                    kind: ChatStreamKind::Reasoning,
                    text,
                }))?;
            }
            on_event(AgentEvent::ReasoningPartEnd { received_at })?;
        }
        ChatStreamKind::ToolCall => {
            // The chunk carries only the tool name, emitted the moment it is
            // decoded — the arguments are still streaming behind it. That is
            // exactly the window a long patch or file write spends looking
            // frozen, so the hint goes up here rather than at ToolCall.
            if crate::tools::preparing_phase(&chunk.text).is_some() {
                on_event(AgentEvent::ToolPreparing {
                    name: chunk.text.clone(),
                })?;
            }
            on_event(AgentEvent::Chunk(chunk))?;
        }
        ChatStreamKind::Reasoning => {
            let (title, text) = filter.push(&chunk.text);
            if let Some(title) = title {
                on_event(AgentEvent::ReasoningTitle(title))?;
            }
            if let Some(text) = text {
                on_event(AgentEvent::Chunk(ChatStreamChunk {
                    kind: ChatStreamKind::Reasoning,
                    text,
                }))?;
            }
        }
        _ => on_event(AgentEvent::Chunk(chunk))?,
    }
    Ok(())
}

pub(crate) fn emit_model_chunk_at<F>(
    chunk: ChatStreamChunk,
    received_at: Instant,
    filter: &mut ReasoningTitleFilter,
    on_event: &mut F,
) -> Result<()>
where
    F: FnMut(AgentEvent) -> Result<()>,
{
    if chunk.kind == ChatStreamKind::Reasoning {
        on_event(AgentEvent::RawReasoning(chunk.clone()))?;
    }
    emit_filtered_chunk_at(chunk, received_at, filter, on_event)
}

#[cfg(test)]
pub(crate) fn emit_filtered_chunk<F>(
    chunk: ChatStreamChunk,
    filter: &mut ReasoningTitleFilter,
    on_event: &mut F,
) -> Result<()>
where
    F: FnMut(AgentEvent) -> Result<()>,
{
    emit_filtered_chunk_at(chunk, Instant::now(), filter, on_event)
}

#[cfg(test)]
pub(crate) fn parse_reasoning_title(reasoning: &str) -> (Option<String>, String) {
    parse_reasoning_title_chunks([reasoning])
}

#[cfg(test)]
pub(crate) fn parse_reasoning_title_chunks<'a>(
    chunks: impl IntoIterator<Item = &'a str>,
) -> (Option<String>, String) {
    let mut filter = ReasoningTitleFilter::default();
    let mut title = None;
    let mut output = String::new();
    for chunk in chunks {
        let (chunk_title, text) = filter.push(chunk);
        title = title.or(chunk_title);
        if let Some(text) = text {
            output.push_str(&text);
        }
    }
    let (finished_title, pending) = filter.finish();
    let title = title.or(finished_title);
    if let Some(pending) = pending {
        output.push_str(&pending);
    }
    (title, output)
}

/// The transient runtime stamp that rides the turn tail.
///
/// `platform` strips everything a chat message cannot use. A QQ turn has no
/// working directory, no shell and no terminal — those attributes were pure
/// scaffolding there, and they were re-sent at full price on every single
/// turn (285 chars against a ~45-char timestamp).
/// 距最近一次防失忆提醒化石过去了多少个可见轮;None=历史里没有提醒。
pub(crate) fn turns_since_reminder_fossil(
    state: &crate::state::StateStore,
    current_turn_id: &str,
) -> Result<Option<usize>> {
    let turns = state.load_visible_turns_excluding(current_turn_id)?;
    let mut since = None;
    for turn in &turns {
        if turn.is_summary || turn.status == crate::state::TurnStatus::Running {
            continue;
        }
        let has_reminder = turn.context_messages.iter().any(|fossil| {
            matches!(
                fossil.content.as_ref(),
                Some(ChatContent::Text(text)) if text.starts_with("<persona-reminder>")
            )
        });
        if has_reminder {
            since = Some(0);
        } else if let Some(count) = since.as_mut() {
            *count += 1;
        }
    }
    Ok(since)
}

pub(crate) fn runtime_context(mode: AgentMode, platform: bool) -> String {
    if platform {
        return format!(
            "<runtime now=\"{}\"/>",
            Local::now().format("%Y年%m月%d日 %A %H:%M")
        );
    }
    let cwd = crate::tools::workspace::effective_workdir()
        .display()
        .to_string();
    let _ = mode;
    let runtime = terminal_runtime_context();
    // 小时级而非分钟级:同一小时内整块字节不变,配合投影跳注入
    // (缓存调研 08-16);要精确时间,终端面有 date。
    format!(
        "<runtime now=\"{}\" cwd=\"{}\" note=\"cwd is workspace context only; do not infer assistant identity from paths or project names\" {runtime}/>",
        Local::now().format("%Y年%m月%d日 %A %H时"),
        xml_attr_escape(&cwd),
    )
}

pub(crate) fn terminal_runtime_context() -> String {
    let stdin_tty = std::io::stdin().is_terminal();
    let stdout_tty = std::io::stdout().is_terminal();
    let stderr_tty = std::io::stderr().is_terminal();
    let environment = if stdin_tty || stdout_tty || stderr_tty {
        if crate::i18n::agent_is_zh() {
            "终端会话"
        } else {
            "terminal session"
        }
    } else if crate::i18n::agent_is_zh() {
        "非交互或管道环境"
    } else {
        "non-interactive or piped environment"
    };
    let shell = std::env::var("SHELL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "unknown".to_string());
    let mut terminal_parts = Vec::new();
    for key in ["TERM_PROGRAM", "TERM", "COLORTERM"] {
        if let Ok(value) = std::env::var(key) {
            if !value.trim().is_empty() {
                terminal_parts.push(format!("{key}={value}"));
            }
        }
    }
    let terminal = if terminal_parts.is_empty() {
        "unknown".to_string()
    } else {
        terminal_parts.join(", ")
    };
    format!(
        "env=\"{}\" shell=\"{}\" terminal=\"{}\"",
        xml_attr_escape(environment),
        xml_attr_escape(&shell),
        xml_attr_escape(&terminal)
    )
}

pub(crate) fn clean_user_visible_text(input: &str) -> String {
    let mut output = input.to_string();
    for tag in ["system-reminder", "system_reminder"] {
        output = strip_tagged_sections(output, tag);
    }
    output
}

pub(crate) fn strip_tagged_sections(mut text: String, tag: &str) -> String {
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    while let Some(start) = text.find(&open) {
        let Some(relative_end) = text[start..].find(&close) else {
            text.replace_range(start.., "");
            break;
        };
        let end = start + relative_end + close.len();
        text.replace_range(start..end, "");
    }
    text
}
