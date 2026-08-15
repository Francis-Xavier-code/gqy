//! widgets — 自 src/render/mod.rs 拆分。

#![allow(clippy::single_char_add_str)]
pub(crate) use super::*;

pub(crate) fn stream_needs_terminating_newline(
    mode: Option<ChatStreamKind>,
    reasoning_mode: ReasoningDisplayMode,
) -> bool {
    mode.is_some()
        && !(mode == Some(ChatStreamKind::Reasoning)
            && reasoning_mode == ReasoningDisplayMode::Summary)
}

#[derive(Default)]
pub(crate) struct SentMemeStreamFilter {
    pending: String,
    inside_tag: bool,
}

impl SentMemeStreamFilter {
    pub(crate) fn push(&mut self, text: &str) -> String {
        self.pending.push_str(text);
        let mut output = String::new();
        loop {
            if self.inside_tag {
                if let Some(end) = self.pending.find("</sent_meme>") {
                    let after = end + "</sent_meme>".len();
                    self.pending.drain(..after);
                    self.inside_tag = false;
                    continue;
                }
                self.pending.clear();
                return output;
            }

            let Some(start) = self.pending.find("<sent_meme>") else {
                let keep = longest_sent_meme_prefix_suffix(&self.pending);
                let emit_len = self.pending.len().saturating_sub(keep);
                output.push_str(&self.pending[..emit_len]);
                self.pending.drain(..emit_len);
                return output;
            };

            output.push_str(&self.pending[..start]);
            self.pending.drain(..start + "<sent_meme>".len());
            self.inside_tag = true;
        }
    }

    pub(crate) fn finish(&mut self) -> String {
        if self.inside_tag {
            self.pending.clear();
            self.inside_tag = false;
            return String::new();
        }
        std::mem::take(&mut self.pending)
    }
}

pub(crate) fn longest_sent_meme_prefix_suffix(text: &str) -> usize {
    pub(crate) const TAG: &str = "<sent_meme>";
    let max = TAG.len().saturating_sub(1).min(text.len());
    for len in (1..=max).rev() {
        if text.ends_with(&TAG[..len]) {
            return len;
        }
    }
    0
}

#[derive(Default)]
pub(crate) struct ToolStats {
    pub(crate) calls: usize,
    pub(crate) ok: usize,
    pub(crate) error: usize,
    pub(crate) subject: Option<String>,
    pub(crate) progress: Option<String>,
    pub(crate) final_progress: Option<String>,
    pub(crate) started_at: Option<std::time::Instant>,
    pub(crate) elapsed: Option<std::time::Duration>,
    /// The subagent handed itself off to the background. Its call returned at
    /// once, so the elapsed timer would only ever read `0s` — and worse, imply
    /// the work finished instantly. The job strip tracks it from here on.
    pub(crate) detached: bool,
    pub(crate) seq: usize,
}

impl ToolStats {
    pub(crate) fn elapsed(&self) -> Option<std::time::Duration> {
        self.elapsed
            .or_else(|| self.started_at.map(|started| started.elapsed()))
    }

    /// Every issued call has completed (ok or err) — nothing running.
    pub(crate) fn settled(&self) -> bool {
        self.calls > 0 && self.ok + self.error >= self.calls
    }
}

#[derive(Clone, Copy)]
pub(crate) enum SummaryStyle {
    Reasoning,
    Tool,
}

/// The still-line equivalent of a spinner style, for terminals that cannot
/// animate — so a phase keeps its identity (thinking vs tool) either way.
pub(crate) fn summary_style_for(style: SpinnerStyle) -> SummaryStyle {
    match style {
        SpinnerStyle::Scanner => SummaryStyle::Reasoning,
        SpinnerStyle::Braille => SummaryStyle::Tool,
    }
}

pub(crate) fn style_summary_text(text: &str, style: SummaryStyle) -> String {
    match style {
        SummaryStyle::Reasoning => format!("\x1b[38;5;10m{text}\x1b[0m"),
        SummaryStyle::Tool => format!("\x1b[2m{text}\x1b[0m"),
    }
}

pub(crate) fn write_activity_summary(
    writer: &mut impl Write,
    text: &str,
    style: SummaryStyle,
) -> Result<()> {
    writeln!(writer, "{}", style_summary_text(text, style))?;
    writeln!(writer)?;
    Ok(())
}

pub(crate) fn tool_status_text(name: &str, stats: &ToolStats, subagent: bool) -> String {
    let calls = stats.calls.max(stats.ok + stats.error).max(1);
    let running = stats.calls.saturating_sub(stats.ok + stats.error);
    let text = if calls == 1 && running > 0 {
        format!("{name}×1 {}", t("running", "运行中"))
    } else if calls == 1 && stats.error > 0 {
        format!("{name}×1 err")
    } else if calls == 1 && stats.ok > 0 {
        format!("{name}×1 ok")
    } else if running > 0 {
        let mut text = format!(
            "{name}×{calls} {}:{} ok:{}",
            t("running", "运行中"),
            running,
            stats.ok,
        );
        if stats.error > 0 {
            text.push_str(&format!(" err:{}", stats.error));
        }
        text
    } else if stats.error > 0 {
        format!("{name}×{calls} ok:{} err:{}", stats.ok, stats.error)
    } else {
        format!("{name}×{calls} ok:{}", stats.ok)
    };
    if subagent && !stats.detached {
        if let Some(elapsed) = stats.elapsed() {
            return format!("{text} · {}", format_elapsed(elapsed));
        }
    }
    text
}

pub(crate) fn tool_result_status(status: &str, elapsed: Option<std::time::Duration>) -> String {
    elapsed.map_or_else(
        || status.to_string(),
        |elapsed| format!("{status} · {}", format_elapsed(elapsed)),
    )
}

pub(crate) fn format_elapsed(elapsed: std::time::Duration) -> String {
    let seconds = elapsed.as_secs();
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3_600 {
        format!("{}m {:02}s", seconds / 60, seconds % 60)
    } else {
        format!("{}h {:02}m", seconds / 3_600, (seconds % 3_600) / 60)
    }
}

pub(crate) fn format_reasoning_elapsed(elapsed: std::time::Duration) -> String {
    if elapsed < std::time::Duration::from_millis(1) {
        "<1ms".to_string()
    } else if elapsed < std::time::Duration::from_secs(1) {
        format!("{}ms", elapsed.as_millis())
    } else if elapsed < std::time::Duration::from_secs(60) {
        format!("{:.1}s", elapsed.as_secs_f64())
    } else if elapsed < std::time::Duration::from_secs(3_600) {
        format!("{}m {:02}s", elapsed.as_secs() / 60, elapsed.as_secs() % 60)
    } else {
        format!(
            "{}h {:02}m",
            elapsed.as_secs() / 3_600,
            (elapsed.as_secs() % 3_600) / 60
        )
    }
}

pub(crate) fn is_silent_tool(name: &str) -> bool {
    matches!(name, "show_meme" | "ask_question")
}

pub(crate) fn is_subagent_tool(name: &str) -> bool {
    let name = tool_event_base_name(name);
    matches!(name, "deep_research" | "task")
}

pub(crate) fn tool_event_base_name(name: &str) -> &str {
    if name.starts_with("load_skill:") {
        "load_skill"
    } else if name.starts_with("load_tools:") {
        "load_tools"
    } else if name.starts_with("task:") {
        "task"
    } else {
        name
    }
}

pub(crate) fn inline_tool_subject(name: &str) -> bool {
    // 回收站的 subject 是条数,贴在标题上比单占一行更紧凑,
    // 而成功时整个块本来就只有这一行。
    matches!(tool_event_base_name(name), "load_tools" | "trash_path")
}

pub(crate) fn tool_subject(name: &str, arguments: &str) -> Option<String> {
    let args = serde_json::from_str::<Value>(arguments).ok()?;
    let name = tool_event_base_name(name);
    let value = match name {
        "task" => string_arg(&args, &["description"]),
        "web_search"
        | "search_web_images"
        | "search_meme"
        | "search_knowledge_base"
        | "search_evicted_context"
        | "recall_memories"
        | "recall_past_events"
        | "brew_search_packages"
        | "online_man_search"
        | "query_applegamingwiki" => string_arg(&args, &["query", "topic"]),
        "query_moegirl" => string_arg(&args, &["title", "query"]),
        "search_knowledge_base_by_name" => string_arg(&args, &["file_name_query"]),
        "read_file" => {
            let path = string_arg(&args, &["path"])?;
            Some(match read_page_label(&args) {
                Some(page) => format!("{path} ({page})"),
                None => path,
            })
        }
        "write_file" | "edit_file" | "edit_string" | "register_script" => {
            string_arg(&args, &["path"])
        }
        "trash_path" => {
            let paths = args.get("paths").and_then(Value::as_array)?;
            match paths.len() {
                0 => None,
                // 只删一个时报路径更有用;成堆删时路径无信息量,报条数。
                1 => paths[0].as_str().map(str::to_string),
                count => Some(format!("{count} {}", t("items", "项"))),
            }
        }
        "run_command" => {
            let command = string_arg(&args, &["command"])?;
            Some(
                if args.get("background").and_then(Value::as_bool) == Some(true) {
                    format!("[后台] {command}")
                } else {
                    command
                },
            )
        }
        "read_knowledge_base_file" | "edit_knowledge_base_file" | "remove_knowledge_base_file" => {
            string_arg(&args, &["file_name"])
        }
        "glob" | "grep" => {
            let pattern = string_arg(&args, &["pattern"]);
            let path = string_arg(&args, &["path"]);
            match (pattern, path) {
                (Some(pattern), Some(path)) if !path.trim().is_empty() => {
                    Some(format!("{pattern} · {path}"))
                }
                (pattern, _) => pattern,
            }
        }
        "web_fetch" => string_arg(&args, &["url"]).and_then(|url| safe_url_subject(&url)),
        "load_skill" => string_arg(&args, &["name"]),
        "create_skill" | "update_skill" | "delete_skill" => string_arg(&args, &["name"]),
        "publish_skill" => string_arg(&args, &["draft_id"]),
        "load_tools" => args.get("names").and_then(Value::as_array).map(|names| {
            names
                .iter()
                .filter_map(Value::as_str)
                .map(|name| {
                    let display = readable_tool_name(&format!("load_tools:{name}"));
                    display
                        .split_once('：')
                        .or_else(|| display.split_once(": "))
                        .map(|(_, target)| target.to_string())
                        .unwrap_or(display)
                })
                .collect::<Vec<_>>()
                .join(t(", ", "、"))
        }),
        "deep_research" => string_arg(&args, &["topic"]),
        "check_issue" => string_arg(&args, &["target", "area", "issue", "symptom"]),
        "get_weather" => string_arg(&args, &["location"])
            .or_else(|| Some(t("automatic location", "自动定位").to_string())),
        "get_exchange_rate" => {
            let base = string_arg(&args, &["base"])?;
            let target = string_arg(&args, &["target"])?;
            Some(format!(
                "{} → {}",
                base.to_uppercase(),
                target.to_uppercase()
            ))
        }
        "scientific_calculator" => string_arg(&args, &["expression", "operation"]),
        "set_alarm" => string_arg(&args, &["label", "time"]),
        "cancel_alarm" => string_arg(&args, &["id"]),
        "brew_get_package_info" | "review_brew_package" | "install_brew_package" => {
            string_arg(&args, &["package_name", "package"])
        }
        "online_man_get_page" => {
            let page = string_arg(&args, &["name"])?;
            let section = string_arg(&args, &["section"]);
            Some(section.map_or(page.clone(), |section| format!("{page}({section})")))
        }
        "vision_analyze" | "print_image" | "add_meme" => {
            string_arg(&args, &["image"]).map(|image| image_basename(&image))
        }
        "generate_image" => string_arg(&args, &["prompt"]),
        "upload_text_to_knowledge_base" => string_arg(&args, &["file_name", "title"]),
        "register_deep_research_topic_title" => string_arg(&args, &["topic_title"]),
        "register_deep_research_reference" => string_arg(&args, &["title"]),
        "remove_deep_research_reference" => string_arg(&args, &["ref"]),
        "unregister_script" => string_arg(&args, &["id"]),
        _ => None,
    }?;
    safe_inline_subject(&value)
}

/// Page label for a read_file call: `L<start>-<end>` when the range is
/// bounded, `L<start>+` for an open tail. `None` for a plain full read so
/// the common case stays a bare path.
pub(crate) fn read_page_label(args: &Value) -> Option<String> {
    let offset = args.get("offset").and_then(Value::as_u64);
    let limit = args.get("limit").and_then(Value::as_u64);
    let start = offset.unwrap_or(1).max(1);
    match (offset, limit) {
        (None, None) => None,
        (_, Some(limit)) => Some(format!(
            "L{start}-{}",
            start.saturating_add(limit.saturating_sub(1))
        )),
        (Some(_), None) => Some(format!("L{start}+")),
    }
}

pub(crate) fn string_arg(args: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| args.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

pub(crate) fn safe_inline_subject(value: &str) -> Option<String> {
    let value = truncate_inline_input(&sanitize_terminal_text(value), 256);
    let value = clip_progress_line(&value, 256);
    let value = redact_sensitive_inline(&value);
    let value = clip_progress_line(&value, 80);
    (!value.is_empty()).then_some(value)
}

pub(crate) fn truncate_inline_input(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

pub(crate) fn redact_sensitive_inline(value: &str) -> String {
    pub(crate) const KEYS: &[&str] = &[
        "secret_access_key",
        "secret-access-key",
        "access_key_id",
        "access-key-id",
        "api_key",
        "api-key",
        "apikey",
        "token",
        "password",
        "passwd",
        "secret",
        "authorization",
        "cookie",
        "credential",
        "private_key",
        "private-key",
    ];
    let mut output = value.to_string();
    for key in KEYS {
        let mut from = 0usize;
        loop {
            let lower = output.to_ascii_lowercase();
            let Some(relative) = lower[from..].find(key) else {
                break;
            };
            let key_start = from + relative;
            let key_end = key_start + key.len();
            let boundary_ok =
                key_start == 0 || !lower.as_bytes()[key_start - 1].is_ascii_alphanumeric();
            let mut separator = key_end;
            if matches!(lower.as_bytes().get(separator), Some(b'\'' | b'"')) {
                separator += 1;
            }
            let mut had_space = false;
            while lower.as_bytes().get(separator) == Some(&b' ') {
                had_space = true;
                separator += 1;
            }
            let flag_prefix = &lower[..key_start];
            let single_dash_flag = flag_prefix.ends_with('-')
                && (key_start == 1 || lower.as_bytes()[key_start - 2].is_ascii_whitespace());
            let flag_space = had_space && (flag_prefix.ends_with("--") || single_dash_flag);
            let space_delimited = had_space
                && (matches!(*key, "authorization" | "password" | "passwd") || flag_space);
            if !boundary_ok
                || (!space_delimited
                    && !matches!(lower.as_bytes().get(separator), Some(b'=' | b':')))
            {
                from = key_end;
                continue;
            }
            let mut value_start = separator + usize::from(!space_delimited);
            while lower.as_bytes().get(value_start) == Some(&b' ') {
                value_start += 1;
            }
            let quote = lower
                .as_bytes()
                .get(value_start)
                .copied()
                .filter(|value| matches!(value, b'\'' | b'"'));
            value_start += usize::from(quote.is_some());
            let value_end = quote
                .and_then(|quote| {
                    lower.as_bytes()[value_start..]
                        .iter()
                        .position(|value| *value == quote)
                        .map(|end| value_start + end)
                })
                .or_else(|| {
                    flag_space.then(|| {
                        lower.as_bytes()[value_start..]
                            .iter()
                            .position(|byte| byte.is_ascii_whitespace())
                            .map(|end| value_start + end)
                            .unwrap_or(output.len())
                    })
                })
                .or_else(|| {
                    lower[value_start..]
                        .find(['&', ',', ';'])
                        .map(|end| value_start + end)
                })
                .unwrap_or(output.len());
            output.replace_range(value_start..value_end, "[redacted]");
            from = value_start + "[redacted]".len();
        }
    }
    redact_bearer_token(output)
}

pub(crate) fn redact_bearer_token(mut output: String) -> String {
    let mut from = 0usize;
    loop {
        let lower = output.to_ascii_lowercase();
        let Some(relative) = lower[from..].find("bearer") else {
            break;
        };
        let start = from + relative;
        let end = start + "bearer".len();
        let boundary_ok = start == 0 || !lower.as_bytes()[start - 1].is_ascii_alphanumeric();
        let mut value_start = end;
        while lower.as_bytes().get(value_start) == Some(&b' ') {
            value_start += 1;
        }
        if !boundary_ok || value_start == end || value_start == output.len() {
            from = end;
            continue;
        }
        let value_end = lower.as_bytes()[value_start..]
            .iter()
            .position(|byte| byte.is_ascii_whitespace() || matches!(*byte, b',' | b';' | b'&'))
            .map(|relative| value_start + relative)
            .unwrap_or(output.len());
        output.replace_range(value_start..value_end, "[redacted]");
        from = value_start + "[redacted]".len();
    }
    output
}

pub(crate) fn safe_url_subject(value: &str) -> Option<String> {
    let mut url = reqwest::Url::parse(value).ok()?;
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_query(None);
    url.set_fragment(None);
    Some(url.to_string())
}

pub(crate) fn image_basename(value: &str) -> String {
    if let Some(url) = safe_url_subject(value) {
        return url;
    }
    std::path::Path::new(value)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(value)
        .to_string()
}

pub(crate) fn readable_tool_name(name: &str) -> String {
    crate::tools::readable_tool_name(name)
}

pub(crate) struct MarkdownStreamRenderer {
    buffer: String,
    line_renderer: MarkdownLineRenderer,
}

impl MarkdownStreamRenderer {
    pub(crate) fn new() -> Self {
        Self {
            buffer: String::new(),
            line_renderer: MarkdownLineRenderer::new(),
        }
    }

    pub(crate) fn push(&mut self, delta: &str) -> String {
        self.buffer.push_str(delta);
        let mut output = String::new();
        while let Some(index) = self.buffer.find('\n') {
            let line = self.buffer[..index].to_string();
            self.buffer = self.buffer[index + 1..].to_string();
            output.push_str(&self.line_renderer.render_line(&line));
        }
        output
    }

    pub(crate) fn flush(&mut self) -> String {
        let mut output = String::new();
        if !self.buffer.is_empty() {
            let line = std::mem::take(&mut self.buffer);
            output.push_str(&self.line_renderer.render_line(&line));
        }
        output.push_str(&self.line_renderer.flush());
        output
    }
}

pub(crate) struct MarkdownLineRenderer {
    in_code_block: bool,
    in_math_block: bool,
    code_lang: String,
    code_buffer: Vec<String>,
    table_buffer: Vec<String>,
    active_table: Option<ActiveTable>,
    math_buffer: Vec<String>,
    /// 当前块级公式的闭合定界符("$$" 或 "\\]")。
    math_closer: &'static str,
}

pub(crate) struct ActiveTable {
    widths: Vec<usize>,
    alignments: Vec<TableAlign>,
}

impl MarkdownLineRenderer {
    pub(crate) fn new() -> Self {
        Self {
            in_code_block: false,
            in_math_block: false,
            code_lang: String::new(),
            code_buffer: Vec::new(),
            table_buffer: Vec::new(),
            active_table: None,
            math_buffer: Vec::new(),
            math_closer: "$$",
        }
    }

    pub(crate) fn render_line(&mut self, line: &str) -> String {
        if line.trim_start().starts_with("```") {
            if self.in_code_block {
                self.in_code_block = false;
                let code = render_code_block(&self.code_lang, &self.code_buffer);
                self.code_lang.clear();
                self.code_buffer.clear();
                return code;
            }
            let pending = self.flush();
            self.in_code_block = true;
            self.code_lang = line
                .trim_start()
                .trim_start_matches('`')
                .split_whitespace()
                .next()
                .unwrap_or_default()
                .to_string();
            self.code_buffer.clear();
            return pending;
        }
        if self.in_code_block {
            self.code_buffer.push(line.to_string());
            return String::new();
        }
        if self.in_math_block {
            let trimmed = line.trim();
            if trimmed == self.math_closer || trimmed.ends_with(self.math_closer) {
                if trimmed != self.math_closer {
                    self.math_buffer
                        .push(trimmed[..trimmed.len() - self.math_closer.len()].to_string());
                }
                self.in_math_block = false;
                let tex = std::mem::take(&mut self.math_buffer).join("\n");
                return render_display_math(&tex, self.math_closer);
            }
            self.math_buffer.push(line.to_string());
            return String::new();
        }
        {
            let trimmed = line.trim();
            let opener = if trimmed.starts_with("$$") {
                Some(("$$", "$$"))
            } else if trimmed.starts_with("\\[") {
                Some(("\\[", "\\]"))
            } else {
                None
            };
            if let Some((open, close)) = opener {
                let pending = self.flush();
                let inner = &trimmed[open.len()..];
                // 单行闭合:$$E=mc^2$$ / \[x\]
                if let Some(tex) = inner.strip_suffix(close) {
                    if !tex.trim().is_empty() {
                        return format!("{pending}{}", render_display_math(tex, close));
                    }
                }
                self.in_math_block = true;
                self.math_closer = close;
                self.math_buffer.clear();
                if !inner.trim().is_empty() {
                    self.math_buffer.push(inner.to_string());
                }
                return pending;
            }
        }
        if let Some(table) = &self.active_table {
            if looks_like_table_row(line) {
                let row = parse_table_row(line);
                let mut output = middle_table_border(&table.widths);
                output.push_str(&render_table_row(
                    &row,
                    &table.widths,
                    &table.alignments,
                    false,
                ));
                return output;
            }
            let mut output = bottom_table_border(&table.widths);
            self.active_table = None;
            output.push_str(&self.render_line(line));
            return output;
        }
        if looks_like_table_row(line) {
            self.table_buffer.push(line.to_string());
            if self.table_buffer.len() < 3 {
                return String::new();
            }
            let second = self.table_buffer.get(1).cloned().unwrap_or_default();
            if is_table_separator(&second) {
                let header =
                    parse_table_row(self.table_buffer.first().map(String::as_str).unwrap_or(""));
                let alignments = parse_table_alignments(&second);
                let first_row =
                    parse_table_row(self.table_buffer.get(2).map(String::as_str).unwrap_or(""));
                let widths = table_widths_for_rows(&[header.clone(), first_row.clone()]);
                self.table_buffer.clear();
                self.active_table = Some(ActiveTable {
                    widths: widths.clone(),
                    alignments: alignments.clone(),
                });
                let mut output = top_table_border(&widths);
                output.push_str(&render_table_row(&header, &widths, &alignments, true));
                output.push_str(&middle_table_border(&widths));
                output.push_str(&render_table_row(&first_row, &widths, &alignments, false));
                return output;
            }
            return self.flush();
        }
        let mut output = self.flush();
        output.push_str(&render_markdown_line(line));
        output.push('\n');
        output
    }

    pub(crate) fn flush(&mut self) -> String {
        if self.in_math_block {
            // 流结束仍未闭合:按原样回放,不吞内容。
            self.in_math_block = false;
            let opener = if self.math_closer == "$$" {
                "$$"
            } else {
                "\\["
            };
            let mut output = format!("\x1b[36m{opener}\x1b[0m\n");
            for line in std::mem::take(&mut self.math_buffer) {
                output.push_str(&format!("\x1b[36m{line}\x1b[0m\n"));
            }
            return output;
        }
        if self.in_code_block {
            self.in_code_block = false;
            let output = render_code_block(&self.code_lang, &self.code_buffer);
            self.code_lang.clear();
            self.code_buffer.clear();
            return output;
        }
        if let Some(table) = self.active_table.take() {
            return bottom_table_border(&table.widths);
        }
        if self.table_buffer.is_empty() {
            return String::new();
        }
        let lines = std::mem::take(&mut self.table_buffer);
        if lines.len() >= 2 && is_table_separator(lines.get(1).map(String::as_str).unwrap_or("")) {
            render_table(&lines)
        } else {
            let mut output = String::new();
            for line in lines {
                output.push_str(&render_markdown_line(&line));
                output.push('\n');
            }
            output
        }
    }
}

/// 块级公式:kitty 家族终端走图形协议(高清,复用 print_image 管线),
/// 其余终端半块画;渲染失败原样回放(青色+定界符)。
pub(crate) fn render_display_math(tex: &str, closer: &str) -> String {
    let max_cols = terminal::size()
        .map(|(cols, _)| cols as usize)
        .unwrap_or(100)
        .saturating_sub(6)
        .clamp(24, 110);
    if math::kitty_graphics_supported() {
        if let Some(kitty) = math::render_math_kitty(tex, max_cols) {
            // 占位行自带换行,逐行加两格缩进(图形转义段无换行,不受影响);
            // 首尾补空行,与正文拉开呼吸感。
            let mut output = String::from("\n");
            for line in kitty.sequence.split_inclusive('\n') {
                output.push_str("  ");
                output.push_str(line);
            }
            output.push('\n');
            return output;
        }
    }
    if let Some(art) = math::render_math(tex, math::MathMode::Block, 9, max_cols) {
        let mut output = String::from("\n");
        for line in art.lines {
            output.push_str("  ");
            output.push_str(&line);
            output.push('\n');
        }
        output.push('\n');
        return output;
    }
    let opener = if closer == "$$" { "$$" } else { "\\[" };
    let closing = if closer == "$$" { "$$" } else { "\\]" };
    let mut output = format!("\x1b[36m{opener}\x1b[0m\n");
    for line in tex.lines() {
        output.push_str(&format!("\x1b[36m{line}\x1b[0m\n"));
    }
    output.push_str(&format!("\x1b[36m{closing}\x1b[0m\n"));
    output
}

pub(crate) fn render_markdown_line(line: &str) -> String {
    let trimmed = line.trim_start();
    let indent = &line[..line.len() - trimmed.len()];
    if let Some(header) = render_header(trimmed) {
        return header;
    }
    if let Some((depth, rest)) = parse_blockquote(trimmed) {
        let bars = "\x1b[32m| \x1b[0m".repeat(depth);
        return format!("{indent}{bars}\x1b[32m{}\x1b[0m", render_inline(rest));
    }
    if let Some(rest) = trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix("* "))
        .or_else(|| trimmed.strip_prefix("+ "))
    {
        return format!("{indent}{TERTIARY_STYLE}-{RESET} {}", render_inline(rest));
    }
    let digits = trimmed.chars().take_while(|ch| ch.is_ascii_digit()).count();
    if digits > 0
        && trimmed.as_bytes().get(digits) == Some(&b'.')
        && trimmed.as_bytes().get(digits + 1) == Some(&b' ')
    {
        let marker = &trimmed[..=digits];
        let rest = &trimmed[digits + 2..];
        return format!(
            "{indent}{TERTIARY_STYLE}{marker}{RESET} {}",
            render_inline(rest)
        );
    }
    if is_horizontal_rule(trimmed) {
        return horizontal_rule();
    }
    render_inline(line)
}

pub(crate) fn parse_blockquote(line: &str) -> Option<(usize, &str)> {
    let mut depth = 0;
    let mut rest = line;
    while let Some(stripped) = rest.strip_prefix('>') {
        depth += 1;
        rest = stripped.strip_prefix(' ').unwrap_or(stripped);
    }
    (depth > 0).then_some((depth, rest))
}

pub(crate) fn render_header(line: &str) -> Option<String> {
    let level = line.chars().take_while(|ch| *ch == '#').count();
    if level == 0 || level > 6 || line.as_bytes().get(level) != Some(&b' ') {
        return None;
    }
    let prefix = "#".repeat(level);
    Some(format!(
        "{HEADER_STYLE}{prefix} {}{RESET}",
        render_inline(&line[level + 1..])
    ))
}

pub(crate) fn render_inline(text: &str) -> String {
    let mut output = String::new();
    let chars = text.chars().collect::<Vec<_>>();
    let mut index = 0;
    while index < chars.len() {
        // 行内公式 $…$ / $$…$$:Unicode 转写(xₙ₊₁、√π、α∈(0,1))。
        // 单 $ 启发式同 WebUI:内容非空、两端非空格、右侧不接数字(放过价格)。
        if chars[index] == '$' {
            let double = chars.get(index + 1) == Some(&'$');
            let open = if double { index + 2 } else { index + 1 };
            let close = if double {
                find_double_dollar(&chars, open)
            } else {
                find_marker(&chars, open, '$')
            };
            if let Some(end) = close {
                let tex: String = chars[open..end].iter().collect();
                let accept = !tex.trim().is_empty()
                    && (double
                        || (!tex.starts_with(' ')
                            && !tex.ends_with(' ')
                            && !chars.get(end + 1).is_some_and(|next| next.is_ascii_digit())));
                if accept {
                    output.push_str(PRIMARY_STYLE);
                    output.push_str(&math::unicode_math(&tex));
                    output.push_str(RESET);
                    index = end + if double { 2 } else { 1 };
                    continue;
                }
            }
        }
        if chars[index] == '\\' && chars.get(index + 1) == Some(&'(') {
            let mut probe = index + 2;
            let mut closing = None;
            while probe + 1 < chars.len() {
                if chars[probe] == '\\' && chars[probe + 1] == ')' {
                    closing = Some(probe);
                    break;
                }
                probe += 1;
            }
            if let Some(end) = closing {
                let tex: String = chars[index + 2..end].iter().collect();
                output.push_str(PRIMARY_STYLE);
                output.push_str(&math::unicode_math(&tex));
                output.push_str(RESET);
                index = end + 2;
                continue;
            }
        }
        if index + 1 < chars.len() && chars[index] == '!' && chars[index + 1] == '[' {
            if let Some(label_end) = find_marker(&chars, index + 2, ']') {
                if chars.get(label_end + 1) == Some(&'(') {
                    if let Some(url_end) = find_marker(&chars, label_end + 2, ')') {
                        let alt = chars[index + 2..label_end].iter().collect::<String>();
                        output.push_str(IMAGE_STYLE);
                        output.push_str("[image");
                        if !alt.is_empty() {
                            output.push_str(": ");
                            output.push_str(&alt);
                        }
                        output.push_str("]");
                        output.push_str(RESET);
                        output.push('(');
                        output.push_str(&render_url(
                            &chars[label_end + 2..url_end].iter().collect::<String>(),
                        ));
                        output.push(')');
                        index = url_end + 1;
                        continue;
                    }
                }
            }
        }
        if chars[index] == '`' {
            if let Some(end) = find_marker(&chars, index + 1, '`') {
                output.push_str(INLINE_CODE_STYLE);
                output.extend(chars[index + 1..end].iter());
                output.push_str(RESET);
                index = end + 1;
                continue;
            }
        }
        if index + 1 < chars.len() && chars[index] == '~' && chars[index + 1] == '~' {
            if let Some(end) = find_double_marker(&chars, index + 2, '~') {
                output.push_str(STRIKE_STYLE);
                output.extend(chars[index + 2..end].iter());
                output.push_str(RESET);
                index = end + 2;
                continue;
            }
        }
        if index + 1 < chars.len() && chars[index] == '*' && chars[index + 1] == '*' {
            if let Some(end) = find_double_marker(&chars, index + 2, '*') {
                output.push_str(BOLD_STYLE);
                output.extend(chars[index + 2..end].iter());
                output.push_str(RESET);
                index = end + 2;
                continue;
            }
        }
        if chars[index] == '*' {
            if let Some(end) = find_marker(&chars, index + 1, '*') {
                output.push_str(ITALIC_STYLE);
                output.extend(chars[index + 1..end].iter());
                output.push_str(RESET);
                index = end + 1;
                continue;
            }
        }
        if chars[index] == '_' {
            if is_emphasis_start(&chars, index) {
                if let Some(end) = find_emphasis_end(&chars, index + 1, '_') {
                    output.push_str(ITALIC_STYLE);
                    output.extend(chars[index + 1..end].iter());
                    output.push_str(RESET);
                    index = end + 1;
                    continue;
                }
            }
        }
        if chars[index] == '[' {
            if let Some(label_end) = find_marker(&chars, index + 1, ']') {
                if chars.get(label_end + 1) == Some(&'(') {
                    if let Some(url_end) = find_marker(&chars, label_end + 2, ')') {
                        output.push_str(LINK_LABEL_STYLE);
                        output.extend(chars[index + 1..label_end].iter());
                        output.push_str(RESET);
                        output.push(' ');
                        output.push_str(&render_url_wrapped(
                            &chars[label_end + 2..url_end].iter().collect::<String>(),
                        ));
                        index = url_end + 1;
                        continue;
                    }
                }
            }
        }
        if chars[index] == '<' {
            if let Some(end) = find_marker(&chars, index + 1, '>') {
                let value = chars[index + 1..end].iter().collect::<String>();
                if value.starts_with("http://") || value.starts_with("https://") {
                    output.push_str("\x1b[4m");
                    output.push_str(&render_url_wrapped(&value));
                    output.push_str(RESET);
                    index = end + 1;
                    continue;
                }
                if let Some(rendered) = render_html_tag(&value) {
                    output.push_str(&rendered);
                    index = end + 1;
                    continue;
                }
            }
        }
        output.push(chars[index]);
        index += 1;
    }
    output
}

pub(crate) const RESET: &str = "\x1b[0m";
pub(crate) const PRIMARY_STYLE: &str = "\x1b[38;5;189m";
pub(crate) const SECONDARY_STYLE: &str = "\x1b[36m";
pub(crate) const TERTIARY_STYLE: &str = "\x1b[35m";
pub(crate) const HEADER_STYLE: &str = "\x1b[1m\x1b[35m";
pub(crate) const INLINE_CODE_STYLE: &str = SECONDARY_STYLE;
pub(crate) const LINK_LABEL_STYLE: &str = "\x1b[38;5;117m";
pub(crate) const URL_STYLE: &str = "\x1b[2m\x1b[38;5;75m";
pub(crate) const IMAGE_STYLE: &str = "\x1b[38;5;183m";

pub(crate) const BOLD_STYLE: &str = "\x1b[1m\x1b[34m";
pub(crate) const ITALIC_STYLE: &str = "\x1b[3m\x1b[38;5;250m";
pub(crate) const STRIKE_STYLE: &str = "\x1b[9m";
pub(crate) const CODE_BLOCK_BG: &str = "";
pub(crate) const CODE_BLOCK_FRAME_STYLE: &str = SECONDARY_STYLE;
pub(crate) const CODE_TOKEN_RESET: &str = "\x1b[0m";
pub(crate) const CODE_KEYWORD_STYLE: &str = "\x1b[38;2;196;167;231m";
pub(crate) const CODE_FUNCTION_STYLE: &str = "\x1b[38;2;156;207;216m";
pub(crate) const CODE_STRING_STYLE: &str = "\x1b[38;2;166;214;160m";
pub(crate) const CODE_NUMBER_STYLE: &str = "\x1b[38;2;246;193;119m";
pub(crate) const CODE_COMMENT_STYLE: &str = "\x1b[32m";
pub(crate) const PATCH_DELETE_STYLE: &str = "\x1b[48;2;60;41;53m\x1b[38;5;210m";
pub(crate) const PATCH_INSERT_STYLE: &str = "\x1b[48;2;32;52;67m\x1b[38;5;157m";

pub(crate) fn render_url(url: &str) -> String {
    format!("{URL_STYLE}{url}{RESET}")
}

pub(crate) fn render_url_wrapped(url: &str) -> String {
    format!("<{}>", render_url(url))
}

pub(crate) fn render_html_tag(tag: &str) -> Option<String> {
    match tag.trim().to_ascii_lowercase().as_str() {
        "u" => Some("\x1b[4m".to_string()),
        "/u" => Some("\x1b[0m".to_string()),
        "sub" => Some("\x1b[2m".to_string()),
        "/sub" => Some("\x1b[0m".to_string()),
        "sup" => Some("\x1b[1m".to_string()),
        "/sup" => Some("\x1b[0m".to_string()),
        "br" | "br/" | "br /" => Some("\n".to_string()),
        _ => None,
    }
}

pub(crate) fn horizontal_rule() -> String {
    let width = terminal::size()
        .map(|(width, _)| usize::from(width) / 3)
        .unwrap_or(24)
        .clamp(16, 40);
    format!("\x1b[2m{}\x1b[0m", "─".repeat(width))
}

pub(crate) fn render_table(lines: &[String]) -> String {
    render_table_with_header_style(lines, true)
}

pub(crate) fn render_table_with_header_style(lines: &[String], bold_header: bool) -> String {
    let alignments = lines
        .get(1)
        .filter(|line| is_table_separator(line))
        .map(|line| parse_table_alignments(line))
        .unwrap_or_default();
    let rows = lines
        .iter()
        .filter(|line| !is_table_separator(line))
        .map(|line| {
            line.trim()
                .trim_matches('|')
                .split('|')
                .map(|cell| render_inline(cell.trim()))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let widths = table_widths_for_rows(&rows);
    let mut output = String::new();
    output.push_str(&top_table_border(&widths));
    for (row_index, row) in rows.iter().enumerate() {
        output.push_str(&render_table_row(
            row,
            &widths,
            &alignments,
            bold_header && row_index == 0,
        ));
        if row_index + 1 < rows.len() {
            output.push_str(&middle_table_border(&widths));
        }
    }
    output.push_str(&bottom_table_border(&widths));
    output
}

pub(crate) fn parse_table_row(line: &str) -> Vec<String> {
    line.trim()
        .trim_matches('|')
        .split('|')
        .map(|cell| render_table_cell(cell.trim()))
        .collect()
}

/// 表格单元格:整格为一条公式($…$ / \(…\) 完整包裹)时走二维转写,
/// 分式排成真正的上下结构(多行格);其余走常规行内渲染。
pub(crate) fn render_table_cell(cell: &str) -> String {
    let tex = cell
        .strip_prefix('$')
        .and_then(|rest| rest.strip_suffix('$'))
        .filter(|inner| !inner.is_empty() && !inner.contains('$'))
        .or_else(|| {
            cell.strip_prefix("\\(")
                .and_then(|rest| rest.strip_suffix("\\)"))
        });
    if let Some(tex) = tex {
        if !tex.trim().is_empty() {
            let lines = math::unicode_math_lines(tex);
            let styled = lines
                .iter()
                .map(|line| format!("{PRIMARY_STYLE}{line}{RESET}"))
                .collect::<Vec<_>>();
            return styled.join("\n");
        }
    }
    render_inline(cell)
}

pub(crate) fn table_widths_for_rows(rows: &[Vec<String>]) -> Vec<usize> {
    let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
    let mut widths = vec![0usize; cols];
    for row in rows {
        for (index, cell) in row.iter().enumerate() {
            // 多行格(二维公式)取最宽一行。
            let cell_width = cell.split('\n').map(visible_width).max().unwrap_or(0);
            widths[index] = widths[index].max(cell_width);
        }
    }
    let readable_min = readable_table_min_width(cols);
    for width in &mut widths {
        *width = (*width).max(readable_min);
    }
    bounded_table_widths(widths)
}

pub(crate) fn readable_table_min_width(cols: usize) -> usize {
    match cols {
        0 => 0,
        1 => 16,
        2 => 14,
        3 | 4 => 10,
        _ => 8,
    }
}

pub(crate) fn render_table_row(
    row: &[String],
    widths: &[usize],
    alignments: &[TableAlign],
    header: bool,
) -> String {
    let wrapped = widths
        .iter()
        .enumerate()
        .map(|(index, width)| {
            let cell = row.get(index).map(String::as_str).unwrap_or("");
            cell.split('\n')
                .flat_map(|part| wrap_ansi_text(part, *width))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let row_height = wrapped.iter().map(Vec::len).max().unwrap_or(1);
    let mut output = String::new();
    for line_index in 0..row_height {
        push_table_vertical(&mut output);
        for (index, width) in widths.iter().enumerate() {
            let cell = wrapped
                .get(index)
                .and_then(|lines| lines.get(line_index))
                .map(String::as_str)
                .unwrap_or("");
            let cell = if header && !cell.is_empty() {
                format!("{BOLD_STYLE}{cell}{RESET}")
            } else {
                cell.to_string()
            };
            output.push(' ');
            output.push_str(&aligned_cell(
                &cell,
                *width,
                alignments.get(index).copied().unwrap_or(TableAlign::Left),
            ));
            output.push(' ');
            push_table_vertical(&mut output);
        }
        output.push('\n');
    }
    output
}

pub(crate) fn top_table_border(widths: &[usize]) -> String {
    table_border(widths, '┌', '┬', '┐')
}

pub(crate) fn middle_table_border(widths: &[usize]) -> String {
    table_border(widths, '├', '┼', '┤')
}

pub(crate) fn bottom_table_border(widths: &[usize]) -> String {
    table_border(widths, '└', '┴', '┘')
}

pub(crate) fn bounded_table_widths(mut widths: Vec<usize>) -> Vec<usize> {
    if widths.is_empty() {
        return widths;
    }
    let terminal_width = terminal::size()
        .map(|(width, _)| usize::from(width))
        .unwrap_or(100)
        .saturating_sub(1)
        .max(20);
    let border_overhead = widths.len().saturating_mul(3).saturating_add(1);
    let available = terminal_width
        .saturating_sub(border_overhead)
        .max(widths.len());
    while widths.iter().sum::<usize>() > available {
        let Some((index, width)) = widths
            .iter()
            .enumerate()
            .max_by_key(|(_, width)| **width)
            .map(|(index, width)| (index, *width))
        else {
            break;
        };
        if width <= 1 {
            break;
        }
        widths[index] -= 1;
    }
    widths
}

pub(crate) fn wrap_ansi_text(text: &str, width: usize) -> Vec<String> {
    if text.is_empty() {
        return vec![String::new()];
    }
    let width = width.max(1);
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut current_width = 0usize;
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            current.push(ch);
            for next in chars.by_ref() {
                current.push(next);
                if next == 'm' {
                    break;
                }
            }
            continue;
        }
        let ch_width = char_display_width(ch);
        if current_width > 0 && current_width + ch_width > width {
            lines.push(current);
            current = String::new();
            current_width = 0;
        }
        current.push(ch);
        current_width += ch_width;
    }
    lines.push(current);
    lines
}

pub(crate) fn char_display_width(ch: char) -> usize {
    if ch.is_ascii() {
        1
    } else if (ch as u32) >= 0x2e80 {
        2
    } else {
        1
    }
}

#[derive(Clone, Copy)]
pub(crate) enum TableAlign {
    Left,
    Center,
    Right,
}

pub(crate) fn parse_table_alignments(line: &str) -> Vec<TableAlign> {
    line.trim()
        .trim_matches('|')
        .split('|')
        .map(|cell| {
            let cell = cell.trim();
            match (cell.starts_with(':'), cell.ends_with(':')) {
                (true, true) => TableAlign::Center,
                (false, true) => TableAlign::Right,
                _ => TableAlign::Left,
            }
        })
        .collect()
}

pub(crate) fn aligned_cell(cell: &str, width: usize, align: TableAlign) -> String {
    let padding = width.saturating_sub(visible_width(cell));
    match align {
        TableAlign::Left => format!("{cell}{}", " ".repeat(padding)),
        TableAlign::Right => format!("{}{cell}", " ".repeat(padding)),
        TableAlign::Center => {
            let left = padding / 2;
            let right = padding - left;
            format!("{}{cell}{}", " ".repeat(left), " ".repeat(right))
        }
    }
}

pub(crate) fn table_border(widths: &[usize], left: char, mid: char, right: char) -> String {
    let mut output = String::new();
    output.push_str("\x1b[2m");
    output.push(left);
    for (index, width) in widths.iter().enumerate() {
        output.push_str(&"─".repeat(width + 2));
        output.push(if index + 1 == widths.len() {
            right
        } else {
            mid
        });
    }
    output.push_str("\x1b[0m\n");
    output
}

pub(crate) fn push_table_vertical(output: &mut String) {
    output.push_str("\x1b[2m│\x1b[0m");
}

pub(crate) fn highlight_code_line(lang: &str, line: &str) -> String {
    let lang = lang.trim().to_ascii_lowercase();
    if lang.is_empty() {
        return line.to_string();
    }
    let comment_marker = match lang.as_str() {
        "py" | "python" | "sh" | "bash" | "zsh" | "fish" | "toml" | "yaml" | "yml" => Some('#'),
        "rs" | "rust" | "js" | "ts" | "tsx" | "jsx" | "c" | "cpp" | "java" | "go" => None,
        _ => None,
    };
    let mut output = String::new();
    let chars = line.chars().collect::<Vec<_>>();
    let mut index = 0;
    while index < chars.len() {
        if let Some(marker) = comment_marker {
            if chars[index] == marker {
                output.push_str(CODE_COMMENT_STYLE);
                output.extend(chars[index..].iter());
                output.push_str(CODE_TOKEN_RESET);
                return output;
            }
        }
        if index + 1 < chars.len() && chars[index] == '/' && chars[index + 1] == '/' {
            output.push_str(CODE_COMMENT_STYLE);
            output.extend(chars[index..].iter());
            output.push_str(CODE_TOKEN_RESET);
            return output;
        }
        if chars[index] == '"'
            || chars[index] == '\''
            || (chars[index] == '`'
                && matches!(lang.as_str(), "js" | "ts" | "tsx" | "jsx" | "sh" | "bash"))
        {
            let quote = chars[index];
            let start = index;
            index += 1;
            let mut escaped = false;
            while index < chars.len() {
                if escaped {
                    escaped = false;
                } else if chars[index] == '\\' {
                    escaped = true;
                } else if chars[index] == quote {
                    index += 1;
                    break;
                }
                index += 1;
            }
            output.push_str(CODE_STRING_STYLE);
            output.extend(chars[start..index].iter());
            output.push_str(CODE_TOKEN_RESET);
            continue;
        }
        if chars[index].is_ascii_digit() {
            let start = index;
            index += 1;
            while index < chars.len()
                && (chars[index].is_ascii_alphanumeric() || matches!(chars[index], '_' | '.'))
            {
                index += 1;
            }
            output.push_str(CODE_NUMBER_STYLE);
            output.extend(chars[start..index].iter());
            output.push_str(CODE_TOKEN_RESET);
            continue;
        }
        if is_code_word_start(chars[index]) {
            let start = index;
            index += 1;
            while index < chars.len() && is_code_word_char(chars[index]) {
                index += 1;
            }
            let token = chars[start..index].iter().collect::<String>();
            let style = if code_keywords(&lang).contains(&token.as_str()) {
                Some(CODE_KEYWORD_STYLE)
            } else if matches!(
                token.as_str(),
                "true" | "false" | "null" | "None" | "Some" | "Ok" | "Err"
            ) {
                Some(CODE_NUMBER_STYLE)
            } else if next_non_space_is_open_paren(&chars, index) {
                Some(CODE_FUNCTION_STYLE)
            } else {
                None
            };
            if let Some(style) = style {
                output.push_str(style);
                output.push_str(&token);
                output.push_str(CODE_TOKEN_RESET);
            } else {
                output.push_str(PRIMARY_STYLE);
                output.push_str(&token);
                output.push_str(CODE_TOKEN_RESET);
            }
            continue;
        }
        output.push(chars[index]);
        index += 1;
    }
    output
}
