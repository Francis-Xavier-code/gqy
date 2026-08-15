//! repl_ui — 自 src/cli.rs 拆分。

use super::*;

pub(crate) fn submitted_echo_lines(mode: AgentMode, input: &str, cols: usize) -> Vec<String> {
    let max_text_width = cols.saturating_sub(3).max(1);
    let bar = submitted_echo_bar(mode);
    let mut output = Vec::new();
    output.push(bar.clone());
    for line in input.split('\n') {
        let mut chunks = wrap_visible_width(line, max_text_width);
        if chunks.is_empty() {
            chunks.push(String::new());
        }
        for chunk in chunks {
            output.push(format!("{bar} {}", colorize_repl_placeholders(&chunk)));
        }
    }
    output.push(bar);
    output
}

pub(crate) fn submitted_echo_bar(mode: AgentMode) -> String {
    match mode {
        AgentMode::Normal => "\x1b[1m\x1b[34m┃\x1b[0m".to_string(),
        // 与 footer 模式标签同为 tertiary(35 酒红),整条 dev 视觉一致。
        AgentMode::Dev => "\x1b[1m\x1b[35m┃\x1b[0m".to_string(),
    }
}

pub(crate) fn input_prompt_bar(mode: AgentMode) -> String {
    format!("{} ", submitted_echo_bar(mode))
}

pub(crate) fn repl_shortcut_hint_line(mode: AgentMode, cols: usize) -> String {
    let bar = input_prompt_bar(mode);
    let text = t(
        "Shift+Enter newline; Ctrl+J newline; Ctrl+V paste clipboard",
        "Shift+Enter 换行；Ctrl+J 换行；Ctrl+V 粘贴剪贴板",
    );
    let text_width = cols.saturating_sub(visible_width(&bar)).max(1);
    format!(
        "{bar}\x1b[2m{}\x1b[0m",
        truncate_visible_width(text, text_width)
    )
}

pub(crate) fn wrap_visible_width(value: &str, max_width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut width = 0usize;
    for ch in value.chars() {
        let char_width = visible_width(&ch.to_string());
        if width > 0 && width.saturating_add(char_width) > max_width {
            lines.push(std::mem::take(&mut current));
            width = 0;
        }
        current.push(ch);
        width = width.saturating_add(char_width);
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

pub(crate) fn repl_wrapped_input_rows_for_cols(
    prefix: &str,
    lines: &[String],
    cols: usize,
) -> Vec<String> {
    let max_width = repl_content_width_for_cols(prefix, cols);
    let mut rows = Vec::new();
    for line in lines {
        let mut current = String::new();
        let mut width = 0usize;
        for ch in line.chars() {
            let char_width = visible_width(&ch.to_string());
            if width > 0 && width.saturating_add(char_width) > max_width {
                rows.push(std::mem::take(&mut current));
                width = 0;
            }
            current.push(ch);
            width = width.saturating_add(char_width);
        }
        rows.push(current);
        if width > 0 && width % max_width == 0 {
            rows.push(String::new());
        }
    }
    if rows.is_empty() {
        rows.push(String::new());
    }
    rows
}

pub(crate) fn repl_cursor_position_for_line_for_cols(
    prefix: &str,
    line: &str,
    cursor: usize,
    cols: usize,
) -> (u16, u16) {
    let cols = cols.max(1);
    let prefix_width = repl_prefix_width_for_cols(prefix, cols);
    let content_width = repl_content_width_for_cols(prefix, cols);
    let mut col = 0usize;
    let mut row = 0usize;
    for ch in line.chars().take(cursor) {
        let char_width = visible_width(&ch.to_string()).max(1);
        if col > 0 && col.saturating_add(char_width) > content_width {
            row = row.saturating_add(1);
            col = 0;
        }
        col = col.saturating_add(char_width);
        if col >= content_width {
            row = row.saturating_add(1);
            col = 0;
        }
    }
    (
        prefix_width.saturating_add(col).min(u16::MAX as usize) as u16,
        row.min(u16::MAX as usize) as u16,
    )
}

pub(crate) fn repl_history_is_clean(
    input: &str,
    history: &[String],
    history_clean_index: Option<usize>,
) -> bool {
    history_clean_index
        .and_then(|index| history.get(index))
        .map(|entry| entry == input)
        .unwrap_or(false)
}

pub(crate) fn repl_should_browse_history(
    input: &str,
    history: &[String],
    history_clean_index: Option<usize>,
) -> bool {
    input.is_empty() || repl_history_is_clean(input, history, history_clean_index)
}

pub(crate) fn repl_move_cursor_vertical(
    prefix: &str,
    input: &str,
    cursor: usize,
    direction: i32,
) -> usize {
    if input.is_empty() || direction == 0 {
        return cursor.min(input.chars().count());
    }
    repl_move_cursor_vertical_for_cols(prefix, input, cursor, direction, terminal_cols())
}

pub(crate) fn repl_move_cursor_vertical_for_cols(
    prefix: &str,
    input: &str,
    cursor: usize,
    direction: i32,
    cols: usize,
) -> usize {
    let positions = repl_cursor_layout_positions_for_cols(prefix, input, cols);
    let cursor = cursor.min(positions.len().saturating_sub(1));
    let (_, current_row, current_col) = positions[cursor];
    let last_row = positions.last().map(|(_, row, _)| *row).unwrap_or(0);
    let target_row = if direction < 0 {
        current_row.saturating_sub(1)
    } else {
        current_row.saturating_add(1).min(last_row)
    };
    if target_row == current_row {
        return cursor;
    }

    positions
        .iter()
        .filter(|(_, row, _)| *row == target_row)
        .min_by_key(|(index, _, col)| (col.abs_diff(current_col), usize::MAX - *index))
        .map(|(index, _, _)| *index)
        .unwrap_or(cursor)
}

pub(crate) fn repl_cursor_layout_positions_for_cols(
    prefix: &str,
    input: &str,
    cols: usize,
) -> Vec<(usize, usize, usize)> {
    let content_width = repl_content_width_for_cols(prefix, cols);
    let mut positions = Vec::with_capacity(input.chars().count() + 1);
    let mut row = 0usize;
    let mut col = 0usize;
    positions.push((0, row, col));
    for (index, ch) in input.chars().enumerate() {
        if ch == '\n' {
            row = row.saturating_add(1);
            col = 0;
            positions.push((index + 1, row, col));
            continue;
        }
        let char_width = visible_width(&ch.to_string()).max(1);
        if col > 0 && col.saturating_add(char_width) > content_width {
            row = row.saturating_add(1);
            col = 0;
        }
        col = col.saturating_add(char_width);
        if col >= content_width {
            row = row.saturating_add(1);
            col = 0;
        }
        positions.push((index + 1, row, col));
    }
    positions
}

pub(crate) fn repl_prompt_rows(prefix: &str, lines: &[String]) -> u16 {
    repl_prompt_rows_for_cols(prefix, lines, terminal_cols())
}

pub(crate) fn repl_cursor_position(prefix: &str, input: &str, cursor: usize) -> (u16, u16) {
    repl_cursor_position_for_cols(prefix, input, cursor, terminal_cols())
}

pub(crate) fn repl_line_rows_for_cols(prefix: &str, line: &str, cols: usize) -> u16 {
    let content_width = repl_content_width_for_cols(prefix, cols);
    let width = visible_width(line);
    (width / content_width + 1).min(u16::MAX as usize) as u16
}

pub(crate) fn repl_prefix_width_for_cols(prefix: &str, cols: usize) -> usize {
    visible_width(prefix).min(cols.max(1).saturating_sub(1))
}

pub(crate) fn repl_content_width_for_cols(prefix: &str, cols: usize) -> usize {
    cols.max(1)
        .saturating_sub(repl_prefix_width_for_cols(prefix, cols))
        .max(1)
}

pub(crate) fn repl_prompt_rows_for_cols(prefix: &str, lines: &[String], cols: usize) -> u16 {
    let cols = cols.max(1);
    let mut rows = 0usize;
    for line in lines {
        rows += repl_line_rows_for_cols(prefix, line, cols) as usize;
    }
    rows.max(1).min(u16::MAX as usize) as u16
}

pub(crate) fn repl_cursor_position_for_cols(
    prefix: &str,
    input: &str,
    cursor: usize,
    cols: usize,
) -> (u16, u16) {
    let cols = cols.max(1);
    let before_cursor = take_chars(input, cursor);
    let lines = repl_input_lines(&before_cursor);
    let last_index = lines.len().saturating_sub(1);
    let mut row_offset = 0usize;
    for (index, line) in lines.iter().enumerate() {
        if index == last_index {
            let (col, row) =
                repl_cursor_position_for_line_for_cols(prefix, line, line.chars().count(), cols);
            return (
                col,
                row_offset
                    .saturating_add(row as usize)
                    .min(u16::MAX as usize) as u16,
            );
        }
        row_offset += repl_line_rows_for_cols(prefix, line, cols) as usize;
    }
    (
        repl_prefix_width_for_cols(prefix, cols).min(u16::MAX as usize) as u16,
        0,
    )
}

pub(crate) fn insert_char_at_cursor(value: &mut String, cursor: &mut usize, ch: char) {
    let byte_index = byte_index_for_char(value, *cursor);
    value.insert(byte_index, ch);
    *cursor += 1;
}

pub(crate) fn insert_str_at_cursor(value: &mut String, cursor: &mut usize, text: &str) {
    let byte_index = byte_index_for_char(value, *cursor);
    value.insert_str(byte_index, text);
    *cursor += text.chars().count();
}

pub(crate) fn insert_newline_at_cursor(value: &mut String, cursor: &mut usize) {
    insert_char_at_cursor(value, cursor, '\n');
}

pub(crate) fn remove_char_before_cursor(value: &mut String, cursor: &mut usize) {
    let end = byte_index_for_char(value, *cursor);
    let start = byte_index_for_char(value, cursor.saturating_sub(1));
    value.replace_range(start..end, "");
    *cursor -= 1;
}

pub(crate) fn remove_word_before_cursor(value: &mut String, cursor: &mut usize) {
    if *cursor == 0 {
        return;
    }
    let chars = value.chars().collect::<Vec<_>>();
    let mut start = (*cursor).min(chars.len());
    while start > 0 && chars[start - 1].is_whitespace() {
        start -= 1;
    }
    while start > 0 && !chars[start - 1].is_whitespace() {
        start -= 1;
    }
    let byte_start = byte_index_for_char(value, start);
    let byte_end = byte_index_for_char(value, *cursor);
    value.replace_range(byte_start..byte_end, "");
    *cursor = start;
}

pub(crate) fn remove_char_at_cursor(value: &mut String, cursor: usize) {
    if cursor >= value.chars().count() {
        return;
    }
    let start = byte_index_for_char(value, cursor);
    let end = byte_index_for_char(value, cursor + 1);
    value.replace_range(start..end, "");
}

pub(crate) fn byte_index_for_char(value: &str, char_index: usize) -> usize {
    value
        .char_indices()
        .nth(char_index)
        .map(|(index, _)| index)
        .unwrap_or(value.len())
}

pub(crate) fn should_summarize_pasted_text(text: &str) -> bool {
    !text.is_empty()
        && (pasted_text_line_count(text) >= REPL_PASTE_PLACEHOLDER_MIN_LINES
            || text.chars().count() > REPL_PASTE_PLACEHOLDER_MIN_CHARS)
}

pub(crate) fn pasted_text_line_count(text: &str) -> usize {
    if text.is_empty() {
        0
    } else {
        text.chars().filter(|ch| *ch == '\n').count() + 1
    }
}

pub(crate) fn pasted_text_placeholder(index: usize, line_count: usize) -> String {
    if is_zh() {
        format!("[粘贴 {index}: ~{line_count} 行]")
    } else {
        format!("[Pasted {index}: ~{line_count} lines]")
    }
}

pub(crate) fn insert_pasted_text_at_cursor(
    input: &mut String,
    cursor: &mut usize,
    text: String,
    pasted_texts: &mut Vec<Option<PastedText>>,
) {
    let text = strip_terminal_control_sequences(&text);
    if should_summarize_pasted_text(&text) {
        let index = pasted_texts.len() + 1;
        let placeholder = pasted_text_placeholder(index, pasted_text_line_count(&text));
        insert_str_at_cursor(input, cursor, &placeholder);
        pasted_texts.push(Some(PastedText { text }));
    } else {
        insert_str_at_cursor(input, cursor, &text);
    }
}

pub(crate) fn find_repl_placeholders(input: &str) -> Vec<(usize, usize)> {
    let mut result = Vec::new();
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let prefix_len = if i + 7 <= chars.len()
            && chars[i..i + 7].iter().collect::<String>() == "[Image "
        {
            Some(7)
        } else if i + 8 <= chars.len() && chars[i..i + 8].iter().collect::<String>() == "[Pasted " {
            Some(8)
        } else if i + 4 <= chars.len() && chars[i..i + 4].iter().collect::<String>() == "[粘贴 " {
            Some(4)
        } else {
            None
        };

        if let Some(prefix_len) = prefix_len {
            let mut j = i + prefix_len;
            while j < chars.len() && chars[j].is_ascii_digit() {
                j += 1;
            }
            if j < chars.len() && chars[j] == ':' {
                j += 1;
                while j < chars.len() && chars[j] != ']' {
                    j += 1;
                }
                if j < chars.len() && chars[j] == ']' {
                    result.push((i, j + 1));
                    i = j + 1;
                    continue;
                }
            } else if j < chars.len() && chars[j] == ']' {
                result.push((i, j + 1));
                i = j + 1;
                continue;
            }
        }
        i += 1;
    }
    result
}

pub(crate) fn find_image_placeholders(input: &str) -> Vec<(usize, usize)> {
    find_repl_placeholders(input)
        .into_iter()
        .filter(|(start, end)| parse_image_placeholder_index(input, *start, *end).is_some())
        .collect()
}

pub(crate) fn find_pasted_text_placeholders(input: &str) -> Vec<(usize, usize, usize)> {
    find_repl_placeholders(input)
        .into_iter()
        .filter_map(|(start, end)| {
            parse_pasted_text_placeholder_index(input, start, end).map(|index| (start, end, index))
        })
        .collect()
}

pub(crate) fn placeholder_at_cursor(input: &str, cursor: usize) -> Option<(usize, usize)> {
    let placeholders = find_repl_placeholders(input);
    for (start, end) in &placeholders {
        if cursor > *start && cursor < *end {
            return Some((*start, *end));
        }
    }
    None
}

pub(crate) fn placeholder_before_cursor(input: &str, cursor: usize) -> Option<(usize, usize)> {
    let placeholders = find_repl_placeholders(input);
    for (start, end) in &placeholders {
        if *end == cursor {
            return Some((*start, *end));
        }
    }
    None
}

pub(crate) fn placeholder_before_or_at_cursor(
    input: &str,
    cursor: usize,
) -> Option<(usize, usize)> {
    placeholder_at_cursor(input, cursor).or_else(|| placeholder_before_cursor(input, cursor))
}

pub(crate) fn placeholder_after_cursor(input: &str, cursor: usize) -> Option<(usize, usize)> {
    let placeholders = find_repl_placeholders(input);
    for (start, end) in &placeholders {
        if *start == cursor {
            return Some((*start, *end));
        }
    }
    None
}

pub(crate) fn placeholder_after_or_at_cursor(input: &str, cursor: usize) -> Option<(usize, usize)> {
    placeholder_at_cursor(input, cursor).or_else(|| placeholder_after_cursor(input, cursor))
}

pub(crate) fn remove_range_chars(value: &mut String, char_start: usize, char_end: usize) {
    let byte_start = byte_index_for_char(value, char_start);
    let byte_end = byte_index_for_char(value, char_end);
    value.replace_range(byte_start..byte_end, "");
}

pub(crate) fn parse_image_placeholder_index(
    input: &str,
    char_start: usize,
    char_end: usize,
) -> Option<usize> {
    let chars: Vec<char> = input.chars().collect();
    let segment: String = chars[char_start..char_end].iter().collect();
    let after_prefix = segment.strip_prefix("[Image ")?;
    let num_str: String = after_prefix
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    num_str.parse::<usize>().ok()
}

pub(crate) fn parse_pasted_text_placeholder_index(
    input: &str,
    char_start: usize,
    char_end: usize,
) -> Option<usize> {
    let chars: Vec<char> = input.chars().collect();
    let segment: String = chars[char_start..char_end].iter().collect();
    let after_prefix = segment
        .strip_prefix("[Pasted ")
        .or_else(|| segment.strip_prefix("[粘贴 "))?;
    let num_str: String = after_prefix
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    num_str.parse::<usize>().ok()
}

pub(crate) fn clear_placeholder_payload(
    input: &str,
    start: usize,
    end: usize,
    pasted_images: &mut [Option<crate::clipboard::PastedImage>],
    pasted_texts: &mut [Option<PastedText>],
) {
    if let Some(n) = parse_image_placeholder_index(input, start, end) {
        if n > 0 && n <= pasted_images.len() {
            pasted_images[n - 1] = None;
        }
    }
    if let Some(n) = parse_pasted_text_placeholder_index(input, start, end) {
        if n > 0 && n <= pasted_texts.len() {
            pasted_texts[n - 1] = None;
        }
    }
}

pub(crate) fn expand_pasted_text_placeholders(
    input: &str,
    pasted_texts: &[Option<PastedText>],
) -> String {
    let placeholders = find_pasted_text_placeholders(input);
    if placeholders.is_empty() {
        return input.to_string();
    }

    let chars: Vec<char> = input.chars().collect();
    let mut expanded = String::new();
    let mut last_end = 0;
    for (start, end, index) in placeholders {
        expanded.extend(&chars[last_end..start]);
        if index > 0 {
            if let Some(Some(pasted_text)) = pasted_texts.get(index - 1) {
                expanded.push_str(&pasted_text.text);
            } else {
                expanded.extend(&chars[start..end]);
            }
        } else {
            expanded.extend(&chars[start..end]);
        }
        last_end = end;
    }
    expanded.extend(&chars[last_end..]);
    expanded
}

pub(crate) fn placeholder_text_near_cursor(
    input: &str,
    cursor: usize,
    pasted_texts: &[Option<PastedText>],
) -> Option<String> {
    let (start, end) = placeholder_at_cursor(input, cursor)
        .or_else(|| placeholder_before_cursor(input, cursor))
        .or_else(|| placeholder_after_cursor(input, cursor))?;
    let index = parse_pasted_text_placeholder_index(input, start, end)?;
    pasted_texts
        .get(index.checked_sub(1)?)
        .and_then(Option::as_ref)
        .map(|pasted_text| pasted_text.text.clone())
}

pub(crate) fn take_chars(value: &str, count: usize) -> String {
    value.chars().take(count).collect()
}

pub(crate) fn terminal_cols() -> usize {
    terminal::size()
        .map(|(cols, _)| cols.max(1) as usize)
        .unwrap_or(80)
}

pub(crate) fn repl_input_lines(input: &str) -> Vec<String> {
    let normalized = strip_terminal_control_sequences(input)
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    let mut lines = normalized
        .split('\n')
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

pub(crate) fn strip_terminal_control_sequences(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for next in chars.by_ref() {
                    if ('@'..='~').contains(&next) {
                        break;
                    }
                }
            } else {
                chars.next();
            }
            continue;
        }
        if is_disallowed_control_char(ch) {
            continue;
        }
        output.push(ch);
    }
    output
}

pub(crate) fn is_disallowed_control_char(ch: char) -> bool {
    ch.is_control() && !matches!(ch, '\n' | '\t')
}

pub(crate) fn visible_width(value: &str) -> usize {
    let mut width = 0usize;
    let mut escape = false;
    for ch in value.chars() {
        if escape {
            if ch == 'm' {
                escape = false;
            }
            continue;
        }
        if ch == '\x1b' {
            escape = true;
        } else if (ch as u32) >= 0x2e80 {
            width += 2;
        } else {
            width += 1;
        }
    }
    width
}

pub(crate) fn colorize_repl_placeholders(line: &str) -> String {
    let placeholders = find_repl_placeholders(line);
    if placeholders.is_empty() {
        return line.to_string();
    }

    let chars: Vec<char> = line.chars().collect();
    let mut result = String::new();
    let mut last_end = 0;
    for (start, end) in placeholders {
        result.extend(&chars[last_end..start]);
        result.push_str("\x1b[35m");
        result.extend(&chars[start..end]);
        result.push_str("\x1b[0m");
        last_end = end;
    }
    result.extend(&chars[last_end..]);
    result
}

/// Identity of a REPL slash command, dispatched via `parse_repl_input`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReplSlashCommand {
    New,
    Session,
    Rename,
    Delete,
    Workspace,
    Models,
    Persona,
    Usage,
    Config,
    Variant,
    Undo,
    Pop,
    Compact,
    Reset,
    ResetMemory,
    Wipe,
    History,
    Clear,
    Help,
    Exit,
}

pub(crate) struct ReplCommandSpec {
    name: &'static str,
    command: ReplSlashCommand,
    /// Argument hint rendered in /help, e.g. "[count]"; empty when the
    /// command takes no arguments (enforced at dispatch).
    arg_hint: &'static str,
    help_en: &'static str,
    help_zh: &'static str,
}

/// Single source of truth for REPL slash commands: drives Tab completion,
/// prefix resolution, /help output, and dispatch in the REPL loop.
pub(crate) const REPL_COMMAND_TABLE: &[ReplCommandSpec] = &[
    ReplCommandSpec {
        name: "/new",
        command: ReplSlashCommand::New,
        arg_hint: "[name]",
        help_en: "create a new session and switch to it",
        help_zh: "创建新会话并切换过去",
    },
    ReplCommandSpec {
        name: "/session",
        command: ReplSlashCommand::Session,
        arg_hint: "[name|index]",
        help_en: "list sessions, or switch to one (Ctrl+D deletes in the picker)",
        help_zh: "列出会话，或切换到指定会话（菜单内 Ctrl+D 删除）",
    },
    ReplCommandSpec {
        name: "/rename",
        command: ReplSlashCommand::Rename,
        arg_hint: "<name>",
        help_en: "rename the current session",
        help_zh: "重命名当前会话",
    },
    ReplCommandSpec {
        name: "/delete",
        command: ReplSlashCommand::Delete,
        arg_hint: "[name|index]",
        help_en: "delete a session (current by default)",
        help_zh: "删除会话（默认当前会话）",
    },
    ReplCommandSpec {
        name: "/workspace",
        command: ReplSlashCommand::Workspace,
        arg_hint: "[path|clear]",
        help_en: "show, bind, or unbind the session workspace",
        help_zh: "查看、绑定或解绑会话工作目录",
    },
    ReplCommandSpec {
        name: "/models",
        command: ReplSlashCommand::Models,
        arg_hint: "[index|provider/model|default]",
        help_en: "switch this session's model",
        help_zh: "切换当前会话使用的模型",
    },
    ReplCommandSpec {
        name: "/persona",
        command: ReplSlashCommand::Persona,
        arg_hint: "[name]",
        help_en: "switch the active persona",
        help_zh: "切换当前人格",
    },
    ReplCommandSpec {
        name: "/usage",
        command: ReplSlashCommand::Usage,
        arg_hint: "",
        help_en: "show token usage details",
        help_zh: "显示 Token 用量详情",
    },
    ReplCommandSpec {
        name: "/config",
        command: ReplSlashCommand::Config,
        arg_hint: "",
        help_en: "open configuration UI",
        help_zh: "打开配置界面",
    },
    ReplCommandSpec {
        name: "/variant",
        command: ReplSlashCommand::Variant,
        arg_hint: "[name]",
        help_en: "view or switch thinking level",
        help_zh: "查看或切换思考档位",
    },
    ReplCommandSpec {
        name: "/undo",
        command: ReplSlashCommand::Undo,
        arg_hint: "",
        help_en: "undo the last turn or context compaction",
        help_zh: "撤销上一轮或上下文压缩",
    },
    ReplCommandSpec {
        name: "/pop",
        command: ReplSlashCommand::Pop,
        arg_hint: "[count]",
        help_en: "pop selected turns or the oldest count from active context",
        help_zh: "从当前上下文弹出所选轮次或最旧的指定轮数",
    },
    ReplCommandSpec {
        name: "/compact",
        command: ReplSlashCommand::Compact,
        arg_hint: "",
        help_en: "compact current conversation context now",
        help_zh: "立即压缩当前会话上下文",
    },
    ReplCommandSpec {
        name: "/reset",
        command: ReplSlashCommand::Reset,
        arg_hint: "",
        help_en: "start this conversation over",
        help_zh: "重新开始当前会话",
    },
    ReplCommandSpec {
        name: "/reset-memory",
        command: ReplSlashCommand::ResetMemory,
        arg_hint: "",
        help_en: "erase this mode's long-term memory",
        help_zh: "清空当前模式的长期记忆",
    },
    ReplCommandSpec {
        name: "/wipe",
        command: ReplSlashCommand::Wipe,
        arg_hint: "",
        help_en: "erase memory, every conversation, group contexts and generated skills",
        help_zh: "抹掉记忆、所有会话内容、群聊上下文和自动技能",
    },
    ReplCommandSpec {
        name: "/history",
        command: ReplSlashCommand::History,
        arg_hint: "",
        help_en: "show recent conversation history",
        help_zh: "显示最近的会话历史",
    },
    ReplCommandSpec {
        name: "/clear",
        command: ReplSlashCommand::Clear,
        arg_hint: "",
        help_en: "clear the screen",
        help_zh: "清屏",
    },
    ReplCommandSpec {
        name: "/help",
        command: ReplSlashCommand::Help,
        arg_hint: "",
        help_en: "show this help",
        help_zh: "显示此帮助",
    },
    ReplCommandSpec {
        name: "/exit",
        command: ReplSlashCommand::Exit,
        arg_hint: "",
        help_en: "leave REPL",
        help_zh: "退出 REPL",
    },
];

pub(crate) fn repl_command_spec(command: ReplSlashCommand) -> &'static ReplCommandSpec {
    REPL_COMMAND_TABLE
        .iter()
        .find(|spec| spec.command == command)
        .expect("every ReplSlashCommand has a table entry")
}

/// Parsed REPL input: plain chat, a resolved slash command with its argument
/// string, or an unknown/ambiguous slash command.
pub(crate) enum ReplInput<'a> {
    Chat,
    Slash(ReplSlashCommand, &'a str),
    UnknownSlash(&'a str),
}

pub(crate) fn parse_repl_input(input: &str) -> ReplInput<'_> {
    if !input.starts_with('/') {
        return ReplInput::Chat;
    }
    let (name, args) = split_repl_command(input);
    let lowered = name.to_ascii_lowercase();
    if let Some(spec) = REPL_COMMAND_TABLE.iter().find(|spec| spec.name == lowered) {
        return ReplInput::Slash(spec.command, args);
    }
    let mut matches = REPL_COMMAND_TABLE
        .iter()
        .filter(|spec| spec.name.starts_with(&lowered));
    match (matches.next(), matches.next()) {
        (Some(spec), None) => ReplInput::Slash(spec.command, args),
        _ => ReplInput::UnknownSlash(name),
    }
}

pub(crate) fn repl_commands() -> Vec<&'static str> {
    REPL_COMMAND_TABLE.iter().map(|spec| spec.name).collect()
}

pub(crate) fn repl_command_suggestions(input: &str) -> Vec<&'static str> {
    if !input.starts_with('/') {
        return Vec::new();
    }
    repl_commands()
        .into_iter()
        .filter(|command| command.starts_with(input))
        .collect()
}

pub(crate) fn complete_repl_command(input: &str) -> Option<&'static str> {
    let suggestions = repl_command_suggestions(input);
    if suggestions.len() == 1 {
        suggestions.first().copied()
    } else {
        None
    }
}

pub(crate) fn resolve_repl_command<'a>(input: &'a str) -> &'a str {
    if input.starts_with('/') {
        if let Some(command) = complete_repl_command(input) {
            return command;
        }
    }
    input
}

pub(crate) fn repl_command_suggestions_line(suggestions: &[&str], max_width: usize) -> String {
    let line = if suggestions.len() == 1 {
        suggestions[0].to_string()
    } else {
        suggestions.join("  ")
    };
    truncate_visible_width(&line, max_width)
}

pub(crate) fn truncate_visible_width(value: &str, max_width: usize) -> String {
    if visible_width(value) <= max_width {
        return value.to_string();
    }
    if max_width <= 3 {
        return ".".repeat(max_width);
    }
    let mut output = String::new();
    let mut width = 0usize;
    let ellipsis_width = visible_width("...");
    let budget = max_width.saturating_sub(ellipsis_width);
    for ch in value.chars() {
        let ch_width = visible_width(&ch.to_string());
        if width.saturating_add(ch_width) > budget {
            break;
        }
        output.push(ch);
        width = width.saturating_add(ch_width);
    }
    output.push_str("...");
    output
}
