//! tools — 自 src/render/mod.rs 拆分。

use super::*;

pub(crate) fn render_code_block(lang: &str, lines: &[String]) -> String {
    let label = if lang.is_empty() {
        "code".to_string()
    } else {
        format!("code {lang}")
    };
    let header = format!("-- {label}");
    let footer = "--";
    let width = lines
        .iter()
        .map(|line| line.chars().count())
        .chain([header.chars().count(), footer.chars().count()])
        .max()
        .unwrap_or(footer.len())
        .max(24);
    let mut output = String::new();
    output.push_str(&render_code_block_frame(&header, width));
    output.push('\n');
    for line in lines {
        output.push_str(&render_code_block_line_with_width(lang, line, width));
        output.push('\n');
    }
    output.push_str(&render_code_block_frame(footer, width));
    output.push('\n');
    output
}

pub(crate) fn render_code_block_frame(text: &str, width: usize) -> String {
    if text == "--" {
        return format!("{CODE_BLOCK_FRAME_STYLE}{}{RESET}", "─".repeat(width));
    }
    let label = text.strip_prefix("-- ").unwrap_or(text);
    let prefix = format!("╭─ {label} ");
    format!(
        "{CODE_BLOCK_FRAME_STYLE}{prefix}{}{RESET}",
        "─".repeat(width.saturating_sub(prefix.chars().count()))
    )
}

pub(crate) fn render_code_block_line_with_width(lang: &str, line: &str, width: usize) -> String {
    let line_width = line.chars().count();
    let padding = " ".repeat(width.saturating_sub(line_width));
    let highlighted = highlight_code_line(lang, line);
    if highlighted.is_empty() {
        format!("{CODE_BLOCK_BG}{}{RESET}", " ".repeat(width.max(1)))
    } else {
        format!("{CODE_BLOCK_BG}{highlighted}{padding}{RESET}")
    }
}

pub(crate) fn code_keywords(lang: &str) -> &'static [&'static str] {
    match lang {
        "rs" | "rust" => &[
            "as", "async", "await", "break", "const", "continue", "crate", "else", "enum", "fn",
            "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref",
            "return", "self", "Self", "static", "struct", "trait", "type", "unsafe", "use",
            "where", "while",
        ],
        "py" | "python" => &[
            "and", "as", "async", "await", "break", "class", "continue", "def", "elif", "else",
            "except", "finally", "for", "from", "if", "import", "in", "is", "lambda", "not", "or",
            "pass", "raise", "return", "try", "while", "with", "yield",
        ],
        "js" | "ts" | "tsx" | "jsx" => &[
            "async", "await", "break", "case", "catch", "class", "const", "continue", "default",
            "else", "export", "extends", "finally", "for", "from", "function", "if", "import",
            "let", "new", "return", "switch", "throw", "try", "typeof", "var", "while",
        ],
        "sh" | "bash" | "zsh" | "fish" => &[
            "case", "do", "done", "elif", "else", "esac", "fi", "for", "function", "if", "in",
            "then", "while",
        ],
        "json" | "toml" | "yaml" | "yml" => &["true", "false", "null"],
        _ => &[],
    }
}

pub(crate) fn is_code_word_start(ch: char) -> bool {
    ch.is_ascii_alphabetic() || ch == '_'
}

pub(crate) fn is_code_word_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_'
}

pub(crate) fn next_non_space_is_open_paren(chars: &[char], mut index: usize) -> bool {
    while index < chars.len() && chars[index].is_whitespace() {
        index += 1;
    }
    chars.get(index) == Some(&'(')
}

pub(crate) fn is_table_separator(line: &str) -> bool {
    let trimmed = line.trim().trim_matches('|').trim();
    !trimmed.is_empty()
        && trimmed
            .chars()
            .all(|ch| matches!(ch, '-' | ':' | '|' | ' '))
        && trimmed.contains('-')
}

pub(crate) fn looks_like_table_row(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.starts_with('|') && trimmed.ends_with('|') && trimmed.matches('|').count() >= 2
}

pub(crate) fn is_horizontal_rule(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.len() >= 3 && trimmed.chars().all(|ch| ch == '-')
}

pub(crate) fn find_marker(chars: &[char], start: usize, marker: char) -> Option<usize> {
    (start..chars.len()).find(|index| chars[*index] == marker)
}

pub(crate) fn find_double_dollar(chars: &[char], start: usize) -> Option<usize> {
    (start..chars.len().saturating_sub(1))
        .find(|index| chars[*index] == '$' && chars[*index + 1] == '$')
}

pub(crate) fn find_emphasis_end(chars: &[char], start: usize, marker: char) -> Option<usize> {
    (start..chars.len()).find(|index| chars[*index] == marker && is_emphasis_end(chars, *index))
}

pub(crate) fn is_emphasis_start(chars: &[char], index: usize) -> bool {
    !chars
        .get(index.wrapping_sub(1))
        .is_some_and(|ch| is_word_char(*ch))
        && chars
            .get(index + 1)
            .is_some_and(|ch| !ch.is_whitespace() && *ch != '_')
}

pub(crate) fn is_emphasis_end(chars: &[char], index: usize) -> bool {
    chars
        .get(index.wrapping_sub(1))
        .is_some_and(|ch| !ch.is_whitespace() && *ch != '_')
        && !chars.get(index + 1).is_some_and(|ch| is_word_char(*ch))
}

pub(crate) fn is_word_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric()
}

pub(crate) fn find_double_marker(chars: &[char], start: usize, marker: char) -> Option<usize> {
    (start..chars.len().saturating_sub(1))
        .find(|index| chars[*index] == marker && chars[index + 1] == marker)
}

pub(crate) fn visible_width(text: &str) -> usize {
    let mut width = 0;
    let mut escape = false;
    for ch in text.chars() {
        if ch == '\x1b' {
            escape = true;
        } else if escape {
            if ch == 'm' {
                escape = false;
            }
        } else {
            width += char_display_width(ch);
        }
    }
    width
}

pub(crate) fn write_tool_payload(stdout: &mut impl Write, label: &str, payload: &str) -> Result<()> {
    let formatted = format_tool_payload(payload);
    writeln!(stdout, "\x1b[2m{label}:\x1b[0m")?;
    for line in formatted.lines() {
        writeln!(stdout, "\x1b[2m  {line}\x1b[0m")?;
    }
    Ok(())
}

pub(crate) fn write_patch_result(stdout: &mut impl Write, output: &str) -> Result<bool> {
    let Ok(value) = serde_json::from_str::<Value>(output.trim()) else {
        return Ok(false);
    };
    let path = value.get("path").and_then(Value::as_str).unwrap_or("file");
    let diff = value.get("diff").and_then(Value::as_str).unwrap_or("");
    if diff.trim().is_empty() {
        return Ok(false);
    }
    write!(stdout, "{}", render_patch_diff(path, diff))?;
    Ok(true)
}

pub(crate) fn render_patch_diff(path: &str, diff: &str) -> String {
    let mut output = String::new();
    // apply_patch 是唯一编辑器(增/改/删同一语义),标签按 diff 形态区分:
    // 纯 + 无上下文=新建,纯 - 无上下文=删除,其余=修改。
    let mut plus = false;
    let mut minus = false;
    let mut context = false;
    for line in diff.lines() {
        if line.starts_with("--- ") || line.starts_with("+++ ") || line.starts_with("@@") {
            continue;
        }
        match line.as_bytes().first() {
            Some(b'+') => plus = true,
            Some(b'-') => minus = true,
            Some(_) => context = true,
            None => {}
        }
    }
    let label = if plus && !minus && !context {
        t("Created", "已新建")
    } else if minus && !plus && !context {
        t("Deleted", "已删除")
    } else {
        t("Modified", "已修改")
    };
    output.push_str(&format!(
        "\x1b[2m{label}  \x1b[38;5;250m{path}\x1b[0m\n\n"
    ));

    let terminal_width = terminal::size()
        .map(|(width, _)| usize::from(width))
        .unwrap_or(100);

    let mut old_line = 0usize;
    let mut new_line = 0usize;
    for raw_line in diff.lines() {
        if raw_line.starts_with("--- ") || raw_line.starts_with("+++ ") {
            continue;
        }
        if raw_line.starts_with("@@") {
            if let Some((old_start, new_start)) = parse_diff_hunk_header(raw_line) {
                old_line = old_start;
                new_line = new_start;
            }
            if !output.ends_with("\n\n") {
                output.push('\n');
            }
            continue;
        }

        let (line_no, sign, body, style) = if let Some(body) = raw_line.strip_prefix('-') {
            let line_no = old_line;
            old_line += 1;
            (line_no, '-', body, PATCH_DELETE_STYLE)
        } else if let Some(body) = raw_line.strip_prefix('+') {
            let line_no = new_line;
            new_line += 1;
            (line_no, '+', body, PATCH_INSERT_STYLE)
        } else if let Some(body) = raw_line.strip_prefix(' ') {
            let line_no = new_line;
            old_line += 1;
            new_line += 1;
            (line_no, ' ', body, "\x1b[38;5;245m")
        } else {
            (new_line, ' ', raw_line, "\x1b[38;5;245m")
        };

        push_patch_diff_line(&mut output, line_no, sign, body, style, terminal_width);
    }
    output.push('\n');
    output
}

pub(crate) fn push_patch_diff_line(
    output: &mut String,
    line_no: usize,
    sign: char,
    body: &str,
    style: &str,
    terminal_width: usize,
) {
    let first_prefix = format!("\x1b[38;5;102m{line_no:>5}\x1b[0m {style}{sign} │ ");
    let continuation_prefix = format!("\x1b[38;5;102m     \x1b[0m {style}  │ ");
    let prefix_width = visible_width(&first_prefix);
    let body_width = terminal_width.saturating_sub(prefix_width + 1).max(1);
    let wrapped = wrap_ansi_text(body, body_width);

    for (index, segment) in wrapped.iter().enumerate() {
        if index == 0 {
            output.push_str(&first_prefix);
        } else {
            output.push_str(&continuation_prefix);
        }
        output.push_str(segment);
        output.push_str("\x1b[0m\n");
    }
}

pub(crate) fn parse_diff_hunk_header(header: &str) -> Option<(usize, usize)> {
    let mut parts = header.split_whitespace();
    parts.next()?;
    let old_part = parts.next()?.trim_start_matches('-');
    let new_part = parts.next()?.trim_start_matches('+');
    Some((
        parse_diff_range_start(old_part)?,
        parse_diff_range_start(new_part)?,
    ))
}

pub(crate) fn parse_diff_range_start(value: &str) -> Option<usize> {
    value.split(',').next()?.parse().ok()
}

pub(crate) fn write_todo_table(stdout: &mut impl Write, output: &str) -> Result<bool> {
    let Ok(value) = serde_json::from_str::<Value>(output.trim()) else {
        return Ok(false);
    };
    let Some(todos) = value.get("todos").and_then(Value::as_array) else {
        return Ok(false);
    };

    if todos.is_empty() {
        let lines = vec![
            format!("| {} |", t("Todo List", "任务列表")),
            "|---|".to_string(),
            format!("| {} |", t("empty", "空")),
        ];
        write!(stdout, "{}", render_todo_table(&lines))?;
        return Ok(true);
    }

    let mut lines = vec![
        format!("| {} |", t("Todo List", "任务列表")),
        "|---|".to_string(),
    ];
    for item in todos {
        let status = item
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("pending");
        let content = item.get("content").and_then(Value::as_str).unwrap_or("");
        let cell = escape_table_cell(content);
        let cell = if status == "in_progress" {
            format!("{TERTIARY_STYLE}{cell}{RESET}")
        } else {
            cell
        };
        lines.push(format!("| {} {} |", todo_status_marker(status), cell));
    }
    write!(stdout, "{}", render_todo_table(&lines))?;
    Ok(true)
}

pub(crate) fn render_todo_table(lines: &[String]) -> String {
    render_table_with_header_style(lines, false)
}

pub(crate) fn todo_status_marker(status: &str) -> &'static str {
    match status {
        "completed" => "[✔]",
        "in_progress" => "[·]",
        "cancelled" => "[×]",
        _ => "[ ]",
    }
}

pub(crate) fn escape_table_cell(value: &str) -> String {
    value
        .replace('|', "\\|")
        .replace('\n', " ")
        .trim()
        .to_string()
}

pub(crate) fn write_command_block(stdout: &mut impl Write, arguments: &str) -> Result<()> {
    write_command_block_with_status(stdout, arguments, CommandStatus::Running)
}

pub(crate) fn write_command_block_with_status(
    stdout: &mut impl Write,
    arguments: &str,
    status: CommandStatus,
) -> Result<()> {
    let command = command_from_arguments(arguments);
    writeln!(stdout, "{}", command_heading_line(status))?;
    let terminal_width = terminal::size().map(|(w, _)| usize::from(w)).unwrap_or(120);
    let usable = terminal_width.saturating_sub(1).max(5);
    for line in render_command_preview(&command, usable, true, false, 0) {
        writeln!(stdout, "{line}")?;
    }
    Ok(())
}

pub(crate) fn write_command_result_blocks(stdout: &mut impl Write, output: &str) -> Result<()> {
    let Some(result) = parse_command_result(output) else {
        return write_tool_payload(stdout, t("output", "输出"), &sanitize_terminal_text(output));
    };
    if !result.stdout.trim().is_empty() {
        write_fenced_block(stdout, t("output", "输出"), &result.stdout)?;
    }
    if !result.stderr.trim().is_empty() {
        let label = result
            .exit_code
            .map(|code| format!("err exit {code}"))
            .unwrap_or_else(|| "err".to_string());
        write_fenced_block(stdout, &label, &result.stderr)?;
    } else if !result.success {
        let label = result
            .exit_code
            .map(|code| format!("err exit {code}"))
            .unwrap_or_else(|| "err".to_string());
        write_fenced_block(
            stdout,
            &label,
            t(
                "command failed without stderr",
                "命令失败，但没有 stderr 输出",
            ),
        )?;
    }
    Ok(())
}

pub(crate) fn write_fenced_block(stdout: &mut impl Write, label: &str, text: &str) -> Result<()> {
    writeln!(stdout, "\x1b[2m,-- {label}\x1b[0m")?;
    let sanitized = sanitize_terminal_text(text);
    let style = if label.starts_with("err") {
        "\x1b[2m\x1b[31m"
    } else {
        "\x1b[2m"
    };
    for line in truncate_chars(sanitized.trim(), 2400).lines() {
        writeln!(stdout, "{style}{line}\x1b[0m")?;
    }
    writeln!(stdout, "\x1b[2m`--\x1b[0m")?;
    Ok(())
}

pub(crate) struct CommandResult {
    success: bool,
    exit_code: Option<i64>,
    stdout: String,
    stderr: String,
}

pub(crate) fn parse_command_result(output: &str) -> Option<CommandResult> {
    let value = serde_json::from_str::<Value>(output.trim()).ok()?;
    Some(CommandResult {
        success: value.get("success")?.as_bool()?,
        exit_code: value.get("exit_code").and_then(Value::as_i64),
        stdout: value
            .get("stdout")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        stderr: value
            .get("stderr")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
    })
}

pub(crate) fn format_tool_payload(payload: &str) -> String {
    let text = payload.trim();
    let formatted = serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .and_then(|value| serde_json::to_string_pretty(&value).ok())
        .unwrap_or_else(|| text.to_string());
    truncate_chars(&formatted, 2400)
}

pub(crate) fn truncate_chars(text: &str, max_chars: usize) -> String {
    let total = text.chars().count();
    if total <= max_chars {
        return text.to_string();
    }
    let omitted = total - max_chars;
    format!(
        "{}\n... {} {omitted} {} ...",
        text.chars().take(max_chars).collect::<String>(),
        t("truncated", "已截断"),
        t("chars", "字符")
    )
}

pub(crate) fn clip_progress_line(text: &str, max_chars: usize) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.chars().count() <= max_chars {
        text
    } else {
        format!(
            "{}...",
            text.chars()
                .take(max_chars.saturating_sub(3))
                .collect::<String>()
        )
    }
}

pub(crate) fn clip_progress_line_preserving_spaces(text: &str, max_chars: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max_chars {
        text.to_string()
    } else {
        format!(
            "{}...",
            text.chars()
                .take(max_chars.saturating_sub(3))
                .collect::<String>()
        )
    }
}

impl Drop for StreamRenderer {
    pub(crate) fn drop(&mut self) {
        let _ = self.stop_waiting();
        if let Some(mut display) = self.command_display.take() {
            let _ = display.clear(&mut self.output);
        }
        if self.summary_line_active {
            let _ = self.clear_summary_lines();
            eprintln!();
        }
        let _ = self.show_cursor();
        if !self.plain {
            let _ = execute!(self.output, ResetColor);
        }
    }
}

pub(crate) fn normalize_stream_text(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

pub(crate) fn write_full_reasoning_chunk(writer: &mut impl Write, text: &str) -> Result<()> {
    execute!(writer, SetForegroundColor(Color::Green))?;
    write!(writer, "{text}")?;
    Ok(())
}

pub(crate) fn print_reasoning(reasoning: &str) -> Result<()> {
    let mut stdout = io::stdout();
    execute!(stdout, SetForegroundColor(Color::Green))?;
    for line in reasoning.trim().lines() {
        writeln!(stdout, "  {line}")?;
    }
    execute!(stdout, ResetColor)?;
    if terminal::size().is_ok() {
        writeln!(stdout)?;
    }
    Ok(())
}

