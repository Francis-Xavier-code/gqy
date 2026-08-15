//! core — 自 src/render/mod.rs 拆分。

use super::*;

pub(crate) mod math;
pub(crate) mod wait_spinner;

use crate::i18n::text as t;
use crate::llm::{ChatResult, ChatStreamChunk, ChatStreamKind, Usage};
use crate::render::wait_spinner::{braille_frame, SpinnerStyle, WaitSpinner, SPINNER_INTERVAL};
use crate::tools::CommandOutputStream;
use anyhow::Result;
use crossterm::cursor::{Hide, MoveToColumn, MoveUp, Show};
use crossterm::style::{Color, ResetColor, SetForegroundColor};
use crossterm::terminal::{Clear, ClearType};
use crossterm::{execute, terminal};
use serde_json::Value;
use std::collections::{BTreeMap, VecDeque};
use std::io::{self, IsTerminal, Write};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

pub(crate) fn rendered_physical_rows(widths: &[usize], terminal_width: usize) -> u16 {
    let columns = terminal_width.max(1);
    widths
        .iter()
        .map(|width| (*width).max(1).div_ceil(columns))
        .sum::<usize>()
        .min(u16::MAX as usize) as u16
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReasoningDisplayMode {
    Hidden,
    Summary,
    Full,
}

impl ReasoningDisplayMode {
    pub fn from_config(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "hidden" => Self::Hidden,
            "full" => Self::Full,
            _ => Self::Summary,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToolCallDisplayMode {
    Hidden,
    Summary,
    Full,
}

impl ToolCallDisplayMode {
    pub fn from_config(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "hidden" => Self::Hidden,
            "full" => Self::Full,
            _ => Self::Summary,
        }
    }
}

#[derive(Clone)]
pub(crate) struct CommandLogLine {
    stream: CommandOutputStream,
    text: String,
    sequence: u64,
}

#[derive(Default)]
pub(crate) struct CommandStreamState {
    utf8_pending: Vec<u8>,
    current: String,
    control: TerminalControlState,
    last_update: u64,
    current_sequence: Option<u64>,
    pending_cr: bool,
}

#[derive(Clone, serde::Serialize)]
pub(crate) struct CommandOutputPreviewLine {
    stream: &'static str,
    text: String,
}

#[derive(Clone, serde::Serialize)]
pub(crate) struct CommandOutputPreview {
    lines: Vec<CommandOutputPreviewLine>,
    omitted: bool,
}

pub(crate) struct CommandOutputTail {
    max_output_rows: usize,
    stdout: CommandStreamState,
    stderr: CommandStreamState,
    completed: VecDeque<CommandLogLine>,
    omitted_lines: bool,
    sequence: u64,
}

impl CommandOutputTail {
    pub(crate) fn new(max_output_rows: usize) -> Self {
        Self {
            max_output_rows,
            stdout: CommandStreamState::default(),
            stderr: CommandStreamState::default(),
            completed: VecDeque::new(),
            omitted_lines: false,
            sequence: 0,
        }
    }

    pub(crate) fn push(&mut self, stream: CommandOutputStream, chunk: &[u8]) {
        self.sequence = self.sequence.wrapping_add(1);
        let completed = match stream {
            CommandOutputStream::Stdout => self.stdout.push(chunk, self.sequence),
            CommandOutputStream::Stderr => self.stderr.push(chunk, self.sequence),
        };
        self.completed.extend(completed.into_iter().map(|mut line| {
            line.stream = stream;
            line
        }));
        let keep = self.max_output_rows.saturating_mul(4).max(100);
        while self.completed.len() > keep {
            self.completed.pop_front();
            self.omitted_lines = true;
        }
    }

    pub(crate) fn finalize(&mut self) {
        self.stdout.finalize_pending(self.sequence);
        self.stderr.finalize_pending(self.sequence);
    }

    pub(crate) fn preview(&self) -> CommandOutputPreview {
        if self.max_output_rows == 0 {
            return CommandOutputPreview {
                lines: Vec::new(),
                omitted: false,
            };
        }
        let logical = self.logical_lines();
        let omitted = self.omitted_lines || logical.len() > self.max_output_rows;
        let start = logical.len().saturating_sub(self.max_output_rows);
        let lines = logical[start..]
            .iter()
            .map(|line| CommandOutputPreviewLine {
                stream: match line.stream {
                    CommandOutputStream::Stdout => "stdout",
                    CommandOutputStream::Stderr => "stderr",
                },
                text: line.text.clone(),
            })
            .collect();
        CommandOutputPreview { lines, omitted }
    }

    pub(crate) fn logical_lines(&self) -> Vec<CommandLogLine> {
        let mut logical = self.completed.iter().cloned().collect::<Vec<_>>();
        let mut pending = [
            (CommandOutputStream::Stdout, &self.stdout),
            (CommandOutputStream::Stderr, &self.stderr),
        ];
        pending.sort_by_key(|(_, state)| state.last_update);
        for (stream, state) in pending {
            if !state.current.is_empty() {
                logical.push(CommandLogLine {
                    stream,
                    text: state.current.clone(),
                    sequence: state.current_sequence.unwrap_or(state.last_update),
                });
            }
        }
        logical.sort_by_key(|line| line.sequence);
        logical
    }
}

#[derive(Clone, Copy, Default)]
pub(crate) enum TerminalControlState {
    #[default]
    Text,
    Escape,
    EscapeIntermediate,
    Csi,
    Osc,
    OscEscape,
}

impl CommandStreamState {
    pub(crate) fn push(&mut self, chunk: &[u8], sequence: u64) -> Vec<CommandLogLine> {
        self.last_update = sequence;
        let decoded = decode_utf8_chunk(&mut self.utf8_pending, chunk);
        let mut completed = Vec::new();
        for ch in decoded.chars() {
            let Some(ch) = sanitize_terminal_char(&mut self.control, ch) else {
                continue;
            };
            if self.pending_cr {
                self.pending_cr = false;
                if ch == '\n' {
                    completed.push(CommandLogLine {
                        stream: CommandOutputStream::Stdout,
                        text: std::mem::take(&mut self.current),
                        sequence: self.current_sequence.take().unwrap_or(sequence),
                    });
                    continue;
                }
                self.current.clear();
                self.current_sequence = None;
            }
            match ch {
                '\n' => completed.push(CommandLogLine {
                    stream: CommandOutputStream::Stdout,
                    text: std::mem::take(&mut self.current),
                    sequence: self.current_sequence.take().unwrap_or(sequence),
                }),
                '\r' => self.pending_cr = true,
                '\t' => {
                    self.current_sequence.get_or_insert(sequence);
                    self.current.push_str("    ");
                }
                _ => {
                    self.current_sequence.get_or_insert(sequence);
                    self.current.push(ch);
                }
            }
        }
        pub(crate) const MAX_LIVE_LINE_CHARS: usize = 20_000;
        if self.current.chars().count() > MAX_LIVE_LINE_CHARS {
            self.current = self
                .current
                .chars()
                .rev()
                .take(MAX_LIVE_LINE_CHARS)
                .collect::<String>()
                .chars()
                .rev()
                .collect();
        }
        completed
    }

    pub(crate) fn finalize_pending(&mut self, sequence: u64) {
        if !self.utf8_pending.is_empty() {
            self.utf8_pending.clear();
            self.current_sequence.get_or_insert(sequence);
            self.current.push('\u{fffd}');
        }
        self.pending_cr = false;
        self.control = TerminalControlState::Text;
    }
}

pub(crate) struct CommandLiveDisplay {
    command: String,
    status: CommandStatus,
    max_output_rows: usize,
    show_output: bool,
    show_full_command: bool,
    output: CommandOutputTail,
    frame: usize,
    rendered_line_widths: Vec<usize>,
}

impl CommandLiveDisplay {
    pub(crate) fn new(
        arguments: &str,
        max_output_rows: usize,
        show_output: bool,
        show_full_command: bool,
    ) -> Self {
        Self {
            command: command_from_arguments(arguments),
            status: CommandStatus::Running,
            max_output_rows,
            show_output,
            show_full_command,
            output: CommandOutputTail::new(max_output_rows),
            frame: 0,
            rendered_line_widths: Vec::new(),
        }
    }

    pub(crate) fn set_result(&mut self, ok: bool) {
        self.status = if ok {
            CommandStatus::Ok
        } else {
            CommandStatus::Error
        };
    }

    pub(crate) fn push(&mut self, stream: CommandOutputStream, chunk: &[u8]) {
        self.output.push(stream, chunk);
    }

    pub(crate) fn tick(&mut self, writer: &mut impl Write) -> Result<()> {
        self.redraw(writer, true)?;
        self.frame = self.frame.wrapping_add(1);
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn tick_changes_layout_at_width(&self, width: usize) -> bool {
        let next_widths = self
            .rendered_lines(width, true)
            .iter()
            .map(|line| command_ansi_width(line))
            .collect::<Vec<_>>();
        rendered_physical_rows(&self.rendered_line_widths, width)
            != rendered_physical_rows(&next_widths, width)
    }

    pub(crate) fn redraw(&mut self, writer: &mut impl Write, spinning: bool) -> Result<()> {
        let width = command_terminal_width();
        let lines = self.rendered_lines(width, spinning);
        self.clear(writer)?;
        for (index, line) in lines.iter().enumerate() {
            execute!(writer, MoveToColumn(0), Clear(ClearType::CurrentLine))?;
            write!(writer, "{line}")?;
            if index + 1 < lines.len() {
                writeln!(writer)?;
            }
        }
        writer.flush()?;
        self.rendered_line_widths = lines.iter().map(|line| command_ansi_width(line)).collect();
        Ok(())
    }

    pub(crate) fn commit(&mut self, writer: &mut impl Write, include_output: bool) -> Result<()> {
        self.output.finalize();
        let show_output = self.show_output;
        self.show_output = include_output && show_output;
        self.redraw(writer, false)?;
        self.show_output = show_output;
        if !self.rendered_line_widths.is_empty() {
            write_command_block_gap(writer, false)?;
            writer.flush()?;
            self.rendered_line_widths.clear();
        }
        Ok(())
    }

    pub(crate) fn write_static(&mut self, writer: &mut impl Write, include_output: bool) -> Result<()> {
        self.output.finalize();
        let show_output = self.show_output;
        self.show_output = include_output && show_output;
        let lines = self.rendered_lines(command_terminal_width(), false);
        self.show_output = show_output;
        for line in lines {
            writeln!(writer, "{line}")?;
        }
        write_command_block_gap(writer, true)?;
        writer.flush()?;
        Ok(())
    }

    pub(crate) fn clear(&mut self, writer: &mut impl Write) -> Result<()> {
        if self.rendered_line_widths.is_empty() {
            return Ok(());
        }
        let rendered_rows =
            rendered_physical_rows(&self.rendered_line_widths, command_terminal_width());
        if rendered_rows > 1 {
            execute!(writer, MoveUp(rendered_rows - 1))?;
        }
        for index in 0..rendered_rows {
            execute!(writer, MoveToColumn(0), Clear(ClearType::CurrentLine))?;
            if index + 1 < rendered_rows {
                writeln!(writer)?;
            }
        }
        if rendered_rows > 1 {
            execute!(writer, MoveUp(rendered_rows - 1))?;
        }
        execute!(writer, MoveToColumn(0))?;
        writer.flush()?;
        self.rendered_line_widths.clear();
        Ok(())
    }

    pub(crate) fn rendered_lines(&self, width: usize, spinning: bool) -> Vec<String> {
        let usable = width.saturating_sub(1).max(5);
        let body_width = usable.saturating_sub(4).max(1);
        let command_lines = render_command_preview(
            &self.command,
            usable,
            self.show_full_command,
            spinning,
            self.frame,
        );
        let mut output = Vec::with_capacity(command_lines.len() + self.max_output_rows + 1);
        output.push(command_heading_line(self.status));
        output.extend(command_lines);
        if self.show_output && self.max_output_rows > 0 {
            output.extend(self.rendered_log_lines(body_width));
        }
        output
    }

    pub(crate) fn rendered_log_lines(&self, body_width: usize) -> Vec<String> {
        let logical = self.output.logical_lines();
        let mut rows = Vec::new();
        for line in logical {
            for text in wrap_plain_text(&line.text, body_width) {
                rows.push(CommandLogLine {
                    stream: line.stream,
                    text,
                    sequence: line.sequence,
                });
            }
        }
        let omitted = self.output.omitted_lines || rows.len() > self.max_output_rows;
        let keep = if omitted && self.max_output_rows > 1 {
            self.max_output_rows - 1
        } else {
            self.max_output_rows
        };
        let start = rows.len().saturating_sub(keep);
        let mut output = Vec::with_capacity(self.max_output_rows);
        if omitted && self.max_output_rows > 1 {
            output.push(format!(
                "\x1b[2m  ⋮ {}\x1b[0m",
                t("earlier output omitted", "已省略较早输出")
            ));
        }
        output.extend(rows[start..].iter().map(|line| {
            let style = match line.stream {
                CommandOutputStream::Stdout => "\x1b[2m",
                CommandOutputStream::Stderr => "\x1b[2m\x1b[31m",
            };
            format!("\x1b[2m  │\x1b[0m {style}{}\x1b[0m", line.text)
        }));
        output
    }
}

pub(crate) fn write_command_block_gap(writer: &mut impl Write, line_terminated: bool) -> Result<()> {
    if !line_terminated {
        writeln!(writer)?;
    }
    writeln!(writer)?;
    Ok(())
}

#[derive(Clone, Copy)]
pub(crate) enum CommandStatus {
    Running,
    Ok,
    Error,
}

pub(crate) fn command_heading_line(status: CommandStatus) -> String {
    let status = match status {
        CommandStatus::Running => t("running", "运行中"),
        CommandStatus::Ok => "ok",
        CommandStatus::Error => "err",
    };
    format!(
        "\x1b[2m$ {}×1 {status}\x1b[0m",
        t("run command", "运行命令")
    )
}

pub(crate) fn command_terminal_width() -> usize {
    terminal::size()
        .map(|(width, _)| usize::from(width))
        .unwrap_or(120)
}

pub(crate) fn command_from_arguments(arguments: &str) -> String {
    let parsed = serde_json::from_str::<Value>(arguments).ok();
    let command = parsed
        .as_ref()
        .and_then(|value| value.get("command"))
        .and_then(Value::as_str)
        .unwrap_or(arguments);
    sanitize_terminal_text(command).trim().to_string()
}

pub(crate) const COMMAND_PREVIEW_HEAD_LINES: usize = 2;
pub(crate) const COMMAND_PREVIEW_TAIL_LINES: usize = 4;

#[derive(Clone, Copy)]
pub(crate) enum CommandPreviewPrefix {
    First,
    Middle,
    Last,
    SoftWrap,
    LastSoftWrap,
}

pub(crate) fn render_command_preview(
    command: &str,
    width: usize,
    full: bool,
    spinning: bool,
    frame: usize,
) -> Vec<String> {
    let total_lines = command.split('\n').count();
    let compact_lines = COMMAND_PREVIEW_HEAD_LINES + COMMAND_PREVIEW_TAIL_LINES;
    let omitted_lines = if !full && total_lines > compact_lines {
        Some(total_lines - compact_lines)
    } else {
        None
    };
    let logical_lines = if omitted_lines.is_some() {
        command
            .split('\n')
            .take(COMMAND_PREVIEW_HEAD_LINES)
            .chain(
                command
                    .split('\n')
                    .skip(total_lines - COMMAND_PREVIEW_TAIL_LINES),
            )
            .collect::<Vec<_>>()
    } else {
        command.split('\n').collect::<Vec<_>>()
    };
    // Soft-wrap rows have two extra indentation columns after the tree marker.
    let content_width = width.saturating_sub(6).max(1);
    let mut rows = Vec::new();
    for (index, logical_line) in logical_lines.iter().enumerate() {
        if index == COMMAND_PREVIEW_HEAD_LINES {
            if let Some(omitted) = omitted_lines {
                let message = format!(
                    "{} {omitted} {}",
                    t("omitted", "已省略中间"),
                    t("middle lines", "行")
                );
                rows.extend(
                    wrap_plain_text(&message, content_width)
                        .into_iter()
                        .enumerate()
                        .map(|(wrapped_index, text)| {
                            let prefix = if wrapped_index == 0 {
                                "  ⋮ "
                            } else {
                                "  │   "
                            };
                            format!("\x1b[2m{prefix}{text}\x1b[0m")
                        }),
                );
            }
        }
        let wrapped = wrap_plain_text(logical_line, content_width);
        for (wrapped_index, text) in wrapped.iter().enumerate() {
            let first_logical_line = index == 0;
            let last_logical_line = index + 1 == logical_lines.len();
            let last_wrapped_row = wrapped_index + 1 == wrapped.len();
            let prefix = if first_logical_line && wrapped_index == 0 {
                CommandPreviewPrefix::First
            } else if last_logical_line && last_wrapped_row {
                if wrapped_index == 0 {
                    CommandPreviewPrefix::Last
                } else {
                    CommandPreviewPrefix::LastSoftWrap
                }
            } else if wrapped_index > 0 {
                CommandPreviewPrefix::SoftWrap
            } else {
                CommandPreviewPrefix::Middle
            };
            rows.push(format_command_preview_line(prefix, text, spinning, frame));
        }
    }
    rows
}

pub(crate) fn format_command_preview_line(
    prefix: CommandPreviewPrefix,
    text: &str,
    spinning: bool,
    frame: usize,
) -> String {
    let prefix = match prefix {
        CommandPreviewPrefix::First if spinning => format!(
            "\x1b[2m\x1b[36m{}\x1b[0m \x1b[2m↳\x1b[0m ",
            braille_frame(frame)
        ),
        CommandPreviewPrefix::First => "  \x1b[2m↳\x1b[0m ".to_string(),
        CommandPreviewPrefix::Middle => "  \x1b[2m│\x1b[0m ".to_string(),
        CommandPreviewPrefix::Last => "  \x1b[2m└\x1b[0m ".to_string(),
        CommandPreviewPrefix::SoftWrap => "  \x1b[2m│\x1b[0m   ".to_string(),
        CommandPreviewPrefix::LastSoftWrap => "  \x1b[2m└\x1b[0m   ".to_string(),
    };
    format!("{prefix}\x1b[33m{text}\x1b[0m")
}

pub(crate) fn wrap_plain_text(text: &str, width: usize) -> Vec<String> {
    if text.is_empty() {
        return vec![String::new()];
    }
    let width = width.max(1);
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut current_width = 0usize;
    for grapheme in text.graphemes(true) {
        let grapheme_width = UnicodeWidthStr::width(grapheme);
        if current_width > 0 && current_width + grapheme_width > width {
            lines.push(std::mem::take(&mut current));
            current_width = 0;
        }
        current.push_str(grapheme);
        current_width += grapheme_width;
    }
    lines.push(current);
    lines
}

pub(crate) fn clip_to_display_width(text: &str, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }
    if UnicodeWidthStr::width(text) <= max_width {
        return text.to_string();
    }
    let ellipsis = "…";
    let ellipsis_width = UnicodeWidthStr::width(ellipsis);
    if max_width <= ellipsis_width {
        return ellipsis.to_string();
    }
    let content_width = max_width - ellipsis_width;
    let mut output = String::new();
    let mut width = 0usize;
    for grapheme in text.graphemes(true) {
        let grapheme_width = UnicodeWidthStr::width(grapheme);
        if width + grapheme_width > content_width {
            break;
        }
        output.push_str(grapheme);
        width += grapheme_width;
    }
    output.push_str(ellipsis);
    output
}

pub(crate) fn transient_summary_lines(text: &str, terminal_width: usize) -> Vec<String> {
    let max_width = terminal_width.saturating_sub(1).max(1);
    let mut lines = text
        .lines()
        .map(|line| clip_to_display_width(line, max_width))
        .collect::<Vec<_>>();
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

pub(crate) fn command_ansi_width(text: &str) -> usize {
    let mut plain = String::new();
    let mut state = TerminalControlState::Text;
    for ch in text.chars() {
        if let Some(ch) = sanitize_terminal_char(&mut state, ch) {
            plain.push(ch);
        }
    }
    UnicodeWidthStr::width(plain.as_str())
}

pub(crate) fn sanitize_terminal_text(text: &str) -> String {
    let mut state = CommandStreamState::default();
    let completed = state.push(text.as_bytes(), 0);
    state.finalize_pending(0);
    let mut lines = completed
        .into_iter()
        .map(|line| line.text)
        .collect::<Vec<_>>();
    if !state.current.is_empty() {
        lines.push(state.current);
    }
    lines.join("\n")
}

pub(crate) fn decode_utf8_chunk(pending: &mut Vec<u8>, chunk: &[u8]) -> String {
    pending.extend_from_slice(chunk);
    let bytes = std::mem::take(pending);
    let mut output = String::new();
    let mut offset = 0;
    while offset < bytes.len() {
        match std::str::from_utf8(&bytes[offset..]) {
            Ok(text) => {
                output.push_str(text);
                break;
            }
            Err(error) => {
                let valid_end = offset + error.valid_up_to();
                output.push_str(std::str::from_utf8(&bytes[offset..valid_end]).unwrap_or_default());
                match error.error_len() {
                    Some(length) => {
                        output.push('\u{fffd}');
                        offset = valid_end + length;
                    }
                    None => {
                        pending.extend_from_slice(&bytes[valid_end..]);
                        break;
                    }
                }
            }
        }
    }
    output
}

pub(crate) fn sanitize_terminal_char(state: &mut TerminalControlState, ch: char) -> Option<char> {
    match *state {
        TerminalControlState::Text => {
            if ch == '\x1b' {
                *state = TerminalControlState::Escape;
                None
            } else if ch.is_control() && !matches!(ch, '\n' | '\r' | '\t') {
                None
            } else {
                Some(ch)
            }
        }
        TerminalControlState::Escape => {
            *state = match ch {
                '[' => TerminalControlState::Csi,
                ']' | 'P' | 'X' | '^' | '_' => TerminalControlState::Osc,
                ' '..='/' => TerminalControlState::EscapeIntermediate,
                _ => TerminalControlState::Text,
            };
            None
        }
        TerminalControlState::EscapeIntermediate => {
            if ('0'..='~').contains(&ch) {
                *state = TerminalControlState::Text;
            }
            None
        }
        TerminalControlState::Csi => {
            if ('@'..='~').contains(&ch) {
                *state = TerminalControlState::Text;
            }
            None
        }
        TerminalControlState::Osc => {
            if ch == '\x07' {
                *state = TerminalControlState::Text;
            } else if ch == '\x1b' {
                *state = TerminalControlState::OscEscape;
            }
            None
        }
        TerminalControlState::OscEscape => {
            *state = if ch == '\\' {
                TerminalControlState::Text
            } else {
                TerminalControlState::Osc
            };
            None
        }
    }
}

pub fn print_assistant_response(response: &ChatResult, show_reasoning: bool) -> Result<()> {
    if show_reasoning {
        if let Some(reasoning) = response
            .reasoning
            .as_deref()
            .filter(|text| !text.trim().is_empty())
        {
            print_reasoning(reasoning)?;
        }
    }
    print_markdown(&response.content);
    Ok(())
}

pub fn print_markdown(markdown: &str) {
    let skin = termimad::MadSkin::default();
    println!("{}", skin.term_text(markdown.trim_end()));
}

/// Everything the token meters show. Grouped into one struct because the two
/// cache rates each need a numerator *and* a denominator, and threading eight
/// loose `u64`s through four call layers was already past readable.
#[derive(Clone, Copy, Debug, Default)]
pub struct TokenMeter {
    pub turn_tokens: u64,
    /// Denominator of the turn cache rate. A cache hit is an input-side
    /// property — output tokens only enter the prompt on the *next* turn — so
    /// the rate is read/prompt, never read/total, which is what every provider
    /// reports too (DeepSeek splits the prompt into hit+miss; OpenAI's
    /// `cached_tokens` is a subset of `prompt_tokens`; Anthropic names all
    /// three fields `*_input_tokens`).
    pub turn_prompt_tokens: u64,
    pub turn_cached_tokens: u64,
    pub session_tokens: u64,
    pub context_window: Option<usize>,
    /// Σ: session-lifetime total. `None` hides it on narrow terminals.
    pub cumulative_tokens: Option<u64>,
    pub cumulative_prompt_tokens: u64,
    pub cumulative_cached_tokens: u64,
}

/// `None` when there is nothing honest to report: a provider that never said
/// anything about caching must not be rendered as a flat 0%.
pub(crate) fn cache_percent(cached: u64, prompt: u64) -> Option<u64> {
    (cached > 0 && prompt > 0)
        .then(|| ((cached as f64 / prompt as f64) * 100.0).round().min(100.0) as u64)
}

pub(crate) fn cache_suffix(cached: u64, prompt: u64) -> String {
    cache_percent(cached, prompt)
        .map(|percent| format!("(C{percent}%)"))
        .unwrap_or_default()
}

pub fn print_token_usage(meter: &TokenMeter, estimated: bool) -> Result<()> {
    let output = token_usage_output(meter, estimated);
    let mut stdout = io::stdout();
    write!(stdout, "{output}")?;
    stdout.flush()?;
    Ok(())
}

pub(crate) fn token_usage_output(meter: &TokenMeter, estimated: bool) -> String {
    let prefix = if estimated {
        t("Estimated ", "估算")
    } else {
        ""
    };
    let line = format!("{prefix}Token: {}", format_token_usage_inline(meter));
    format!("\x1b[2m{line}\x1b[0m\n\n")
}

pub(crate) fn format_token_usage_inline(meter: &TokenMeter) -> String {
    format_token_usage_inline_opts(meter, true)
}

pub(crate) fn format_token_usage_inline_opts(meter: &TokenMeter, show_percent: bool) -> String {
    let context_window = meter.context_window.map(|value| value as u64);
    let context = context_window
        .map(format_compact_count)
        .unwrap_or_else(|| "?".to_string());
    let usage_ratio = if let Some(context_window) = context_window.filter(|value| *value > 0) {
        format!(
            "{:.1}%",
            meter.session_tokens as f64 / context_window as f64 * 100.0
        )
    } else {
        "?".to_string()
    };

    let mut session = if show_percent {
        format!(
            "{}/{}({usage_ratio})",
            format_compact_count(meter.session_tokens),
            context,
        )
    } else {
        format!("{}/{}", format_compact_count(meter.session_tokens), context)
    };
    if let Some(cumulative_tokens) = meter.cumulative_tokens {
        session.push_str(&format!(
            " · Σ{}{}",
            format_compact_count(cumulative_tokens),
            cache_suffix(
                meter.cumulative_cached_tokens,
                meter.cumulative_prompt_tokens
            ),
        ));
    }
    if meter.turn_tokens == 0 {
        session
    } else {
        format!(
            "{}{} · {session}",
            format_compact_count(meter.turn_tokens),
            cache_suffix(meter.turn_cached_tokens, meter.turn_prompt_tokens),
        )
    }
}

pub fn usage_total(usage: &Usage) -> u64 {
    usage.effective_total_tokens()
}

pub(crate) fn format_compact_count(value: u64) -> String {
    pub(crate) const K: f64 = 1_000.0;
    pub(crate) const M: f64 = 1_000_000.0;
    if value >= 1_000_000 {
        format_compact_unit(value as f64 / M, "M")
    } else if value >= 1_000 {
        format_compact_unit(value as f64 / K, "k")
    } else {
        value.to_string()
    }
}

pub(crate) fn format_compact_unit(value: f64, suffix: &str) -> String {
    if (value.fract() - 0.0).abs() < f64::EPSILON {
        format!("{value:.0}{suffix}")
    } else {
        format!("{value:.1}{suffix}")
    }
}

pub(crate) enum RenderOutput {
    Terminal,
    Buffered(Vec<u8>),
}

impl Write for RenderOutput {
    pub(crate) fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        match self {
            Self::Terminal => io::stdout().write(bytes),
            Self::Buffered(buffer) => buffer.write(bytes),
        }
    }

    pub(crate) fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Terminal => io::stdout().flush(),
            Self::Buffered(_) => Ok(()),
        }
    }
}

pub struct StreamRenderer {
    reasoning_mode: ReasoningDisplayMode,
    tool_call_mode: ToolCallDisplayMode,
    plain: bool,
    mode: Option<ChatStreamKind>,
    cursor_hidden: bool,
    external_cursor_control: bool,
    output: RenderOutput,
    markdown: MarkdownStreamRenderer,
    reasoning_text: String,
    reasoning_tokens: usize,
    reasoning_title: Option<String>,
    reasoning_started_at: Option<std::time::Instant>,
    reasoning_elapsed: Option<std::time::Duration>,
    tool_stats: BTreeMap<String, ToolStats>,
    tool_seq: usize,
    readable_tool_names: bool,
    command_output_lines: usize,
    command_display: Option<CommandLiveDisplay>,
    summary_line_active: bool,
    summary_lines_active: u16,
    last_tool_summary: String,
    live_summary: bool,
    wait_spinner: Option<WaitSpinner>,
    last_tick: Option<std::time::Instant>,
    preparing_question_started_at: Option<std::time::Instant>,
    /// Phase text and start time for the "still receiving arguments" hint.
    /// Sticky like `preparing_question_started_at` and for the same reason:
    /// `tick_spinner` re-derives the phase from renderer state on every tick,
    /// so a phase merely pushed into the spinner is overwritten before it can
    /// be drawn.
    tool_preparing: Option<(&'static str, std::time::Instant)>,
    subagent_mode: Option<ChatStreamKind>,
    sent_meme_filter: SentMemeStreamFilter,
}

