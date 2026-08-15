//! live — 自 src/cli.rs 拆分。

use super::*;

pub(crate) struct LiveReplTail {
    editor: LiveReplEditor,
    queued: Vec<QueuedPrompt>,
    pending_chunks: Vec<ChatStreamChunk>,
    footer: ReplFooterStatus,
    /// 回合中途逐请求刷新计量时的基线(回合开始前的 footer 快照)。
    /// 每次 RoundUsage 事件都从基线重新叠加,避免累计值重复相加;
    /// 任何权威更新(set_footer)都会清掉它。
    round_base_footer: Option<Box<ReplFooterStatus>>,
    jobs: Vec<crate::tools::jobs::JobOverview>,
    job_spinner: usize,
    output_cursor: (u16, u16),
    tail_start: u16,
    tail_rows: u16,
    input_cursor: (u16, u16),
    rendered: bool,
    external_output_active: bool,
    raw_mode_handoff: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LiveTailPlacement {
    output_row: u16,
    tail_start: u16,
    overflow: u16,
    anchored: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TerminalFrameLayout {
    cursor: (u16, u16),
    occupied_bottom: Option<u16>,
}

pub(crate) struct TerminalFrameTracker {
    columns: usize,
    bottom_margin: Option<usize>,
    cursor_col: usize,
    cursor_row: usize,
    saved_cursor: (usize, usize, bool),
    pending_wrap: bool,
    pending_text: String,
    occupied_bottom: Option<usize>,
}

impl TerminalFrameTracker {
    pub(crate) fn new(start: (u16, u16), columns: u16, bottom_margin: Option<u16>) -> Self {
        let columns = usize::from(columns.max(1));
        let cursor_col = usize::from(start.0).min(columns.saturating_sub(1));
        let cursor_row = usize::from(start.1);
        Self {
            columns,
            bottom_margin: bottom_margin.map(usize::from),
            cursor_col,
            cursor_row,
            saved_cursor: (cursor_col, cursor_row, false),
            pending_wrap: false,
            pending_text: String::new(),
            occupied_bottom: None,
        }
    }

    pub(crate) fn finish(mut self) -> TerminalFrameLayout {
        self.flush_text();
        TerminalFrameLayout {
            cursor: (
                self.cursor_col.min(u16::MAX as usize) as u16,
                self.cursor_row.min(u16::MAX as usize) as u16,
            ),
            occupied_bottom: self
                .occupied_bottom
                .map(|row| row.min(u16::MAX as usize) as u16),
        }
    }

    pub(crate) fn flush_text(&mut self) {
        if self.pending_text.is_empty() {
            return;
        }
        let text = std::mem::take(&mut self.pending_text);
        for grapheme in text.graphemes(true) {
            self.print_width(UnicodeWidthStr::width(grapheme));
        }
    }

    pub(crate) fn print_width(&mut self, width: usize) {
        if width == 0 {
            return;
        }
        if self.pending_wrap || self.cursor_col.saturating_add(width) > self.columns {
            self.cursor_col = 0;
            self.index();
            self.pending_wrap = false;
        }
        self.occupied_bottom = Some(
            self.occupied_bottom
                .map_or(self.cursor_row, |row| row.max(self.cursor_row)),
        );
        let next_col = self.cursor_col.saturating_add(width);
        if next_col >= self.columns {
            self.cursor_col = self.columns.saturating_sub(1);
            self.pending_wrap = true;
        } else {
            self.cursor_col = next_col;
        }
    }

    pub(crate) fn index(&mut self) {
        if self
            .bottom_margin
            .is_some_and(|bottom| self.cursor_row >= bottom)
        {
            return;
        }
        self.cursor_row = self.cursor_row.saturating_add(1);
    }

    pub(crate) fn move_down(&mut self, count: usize) {
        self.pending_wrap = false;
        self.cursor_row = self.cursor_row.saturating_add(count);
        if let Some(bottom) = self.bottom_margin {
            self.cursor_row = self.cursor_row.min(bottom);
        }
    }

    pub(crate) fn move_up(&mut self, count: usize) {
        self.pending_wrap = false;
        self.cursor_row = self.cursor_row.saturating_sub(count);
    }

    pub(crate) fn move_right(&mut self, count: usize) {
        self.pending_wrap = false;
        self.cursor_col = self
            .cursor_col
            .saturating_add(count)
            .min(self.columns.saturating_sub(1));
    }

    pub(crate) fn move_left(&mut self, count: usize) {
        self.pending_wrap = false;
        self.cursor_col = self.cursor_col.saturating_sub(count);
    }

    pub(crate) fn set_row(&mut self, row: usize) {
        self.pending_wrap = false;
        self.cursor_row = row;
        if let Some(bottom) = self.bottom_margin {
            self.cursor_row = self.cursor_row.min(bottom);
        }
    }

    pub(crate) fn set_col(&mut self, col: usize) {
        self.pending_wrap = false;
        self.cursor_col = col.min(self.columns.saturating_sub(1));
    }

    pub(crate) fn param(params: &VteParams, index: usize, default: usize) -> usize {
        params
            .iter()
            .nth(index)
            .and_then(|param| param.first())
            .copied()
            .map(usize::from)
            .filter(|value| *value != 0)
            .unwrap_or(default)
    }
}

impl VtePerform for TerminalFrameTracker {
    pub(crate) fn print(&mut self, character: char) {
        self.pending_text.push(character);
    }

    pub(crate) fn execute(&mut self, byte: u8) {
        self.flush_text();
        match byte {
            b'\n' => {
                self.cursor_col = 0;
                self.pending_wrap = false;
                self.index();
            }
            b'\r' => self.set_col(0),
            0x08 => self.move_left(1),
            b'\t' => {
                let next = (self.cursor_col / 8 + 1) * 8;
                self.set_col(next);
            }
            0x0b | 0x0c => {
                self.pending_wrap = false;
                self.index();
            }
            _ => {}
        }
    }

    pub(crate) fn csi_dispatch(
        &mut self,
        params: &VteParams,
        _intermediates: &[u8],
        ignore: bool,
        action: char,
    ) {
        self.flush_text();
        if ignore {
            return;
        }
        let count = Self::param(params, 0, 1);
        match action {
            'A' => self.move_up(count),
            'B' | 'e' => self.move_down(count),
            'C' | 'a' => self.move_right(count),
            'D' => self.move_left(count),
            'E' => {
                self.move_down(count);
                self.set_col(0);
            }
            'F' => {
                self.move_up(count);
                self.set_col(0);
            }
            'G' | '`' => self.set_col(count.saturating_sub(1)),
            'H' | 'f' => {
                self.set_row(Self::param(params, 0, 1).saturating_sub(1));
                self.set_col(Self::param(params, 1, 1).saturating_sub(1));
            }
            'd' => self.set_row(count.saturating_sub(1)),
            's' => {
                self.saved_cursor = (self.cursor_col, self.cursor_row, self.pending_wrap);
            }
            'u' => {
                (self.cursor_col, self.cursor_row, self.pending_wrap) = self.saved_cursor;
            }
            _ => {}
        }
    }

    pub(crate) fn esc_dispatch(&mut self, _intermediates: &[u8], ignore: bool, byte: u8) {
        self.flush_text();
        if ignore {
            return;
        }
        match byte {
            b'7' => self.saved_cursor = (self.cursor_col, self.cursor_row, self.pending_wrap),
            b'8' => {
                (self.cursor_col, self.cursor_row, self.pending_wrap) = self.saved_cursor;
            }
            b'D' => {
                self.pending_wrap = false;
                self.index();
            }
            b'E' => {
                self.cursor_col = 0;
                self.pending_wrap = false;
                self.index();
            }
            b'M' => self.move_up(1),
            _ => {}
        }
    }
}

pub(crate) fn terminal_frame_layout(
    frame: &[u8],
    start: (u16, u16),
    columns: u16,
    bottom_margin: Option<u16>,
) -> TerminalFrameLayout {
    let mut parser = VteParser::new();
    let mut tracker = TerminalFrameTracker::new(start, columns, bottom_margin);
    parser.advance(&mut tracker, frame);
    tracker.finish()
}

pub(crate) fn live_frame_output_bottom(frame_margin: u16, layout: TerminalFrameLayout) -> Option<u16> {
    let ends_on_free_line = layout.cursor.0 == 0
        && layout
            .occupied_bottom
            .is_none_or(|bottom| layout.cursor.1 > bottom);
    if ends_on_free_line {
        Some(frame_margin)
    } else {
        frame_margin.checked_sub(1)
    }
}

#[derive(Clone, Copy)]
pub(crate) enum CursorAfterUpdate {
    Preserve,
    Shown,
    Hidden,
}

pub(crate) fn synchronized_terminal_update<T>(
    cursor_after: CursorAfterUpdate,
    update: impl FnOnce() -> Result<T>,
) -> Result<T> {
    let mut stdout = io::stdout();
    match cursor_after {
        CursorAfterUpdate::Preserve => execute!(stdout, BeginSynchronizedUpdate)?,
        CursorAfterUpdate::Shown | CursorAfterUpdate::Hidden => {
            execute!(stdout, Hide, BeginSynchronizedUpdate)?
        }
    }
    let result = update();
    let end = match cursor_after {
        CursorAfterUpdate::Shown => execute!(stdout, EndSynchronizedUpdate, Show),
        CursorAfterUpdate::Preserve | CursorAfterUpdate::Hidden => {
            execute!(stdout, EndSynchronizedUpdate)
        }
    };
    match result {
        Ok(value) => {
            end?;
            Ok(value)
        }
        Err(error) => {
            let _ = end;
            Err(error)
        }
    }
}

/// Places the tail below the output. `was_anchored` says the tail was already
/// pinned to the bottom: without it a tail that *shrinks* (a background job
/// strip or a queue bubble going away) would spring back up to the output
/// cursor, leaving blank rows under the input box until later output pushed it
/// down again — the input visibly bouncing.
pub(crate) fn live_tail_placement(
    output_col: u16,
    output_row: u16,
    total_rows: u16,
    terminal_rows: u16,
    was_anchored: bool,
) -> LiveTailPlacement {
    let terminal_rows = terminal_rows.max(1);
    let last_row = terminal_rows.saturating_sub(2);
    let natural_start = output_row.saturating_add(u16::from(output_col > 0));
    let natural_end = natural_start.saturating_add(total_rows.saturating_sub(1));
    let overflow = natural_end.saturating_sub(last_row);
    let output_row = output_row.saturating_sub(overflow);
    let natural_start = output_row.saturating_add(u16::from(output_col > 0));
    let anchored = was_anchored || overflow > 0 || natural_end == last_row;
    let anchored_start = last_row.saturating_add(1).saturating_sub(total_rows);
    let tail_start = if anchored {
        natural_start.max(anchored_start)
    } else {
        natural_start
    };
    // `output_row` is deliberately left where the output actually ended, even
    // when the tail re-anchors below it. It is the contract between the
    // renderer's byte frames and the terminal — the wait spinner erases itself
    // by moving relative to that cursor — so nudging it down to hug the tail
    // leaves orphaned spinner frames in the scrollback.
    LiveTailPlacement {
        output_row,
        tail_start,
        overflow,
        anchored,
    }
}

/// Where a streaming output frame should leave the tail.
///
/// Normally the tail follows the output cursor. A tail already pinned to the
/// bottom stays pinned instead: output fills the rows above it (the frame sets
/// a scroll region so it cannot reach the tail). Letting it slide back up is
/// what made the input box bounce — the rows a finished job strip freed would
/// be reclaimed on the very next frame, then handed back a line of output
/// later.
pub(crate) fn live_tail_next_start(current_start: u16, desired_tail: u16, max_tail: u16) -> u16 {
    if current_start >= max_tail {
        max_tail
    } else {
        desired_tail.min(max_tail)
    }
}

pub(crate) fn max_live_tail_start(terminal_rows: u16, tail_rows: u16) -> u16 {
    terminal_rows
        .max(1)
        .saturating_sub(1)
        .saturating_sub(tail_rows)
}

impl LiveReplTail {
    pub(crate) fn new(
        mode: AgentMode,
        history: Vec<String>,
        queued: Vec<QueuedPrompt>,
        footer: ReplFooterStatus,
    ) -> Result<Self> {
        Ok(Self {
            editor: LiveReplEditor::new(mode, history),
            queued,
            pending_chunks: Vec::new(),
            footer,
            round_base_footer: None,
            jobs: Vec::new(),
            job_spinner: 0,
            output_cursor: cursor_position_or((0, 0)),
            tail_start: 0,
            tail_rows: 0,
            input_cursor: (0, 0),
            rendered: false,
            external_output_active: false,
            raw_mode_handoff: false,
        })
    }

    pub(crate) fn mode(&self) -> AgentMode {
        self.editor.mode
    }

    pub(crate) fn set_footer(&mut self, footer: ReplFooterStatus) {
        self.footer = footer;
        self.round_base_footer = None;
    }

    /// 回合内一次模型请求结束:用基线+回合累计刷新计量并立即重绘。
    /// `context_tokens` 取该请求 prompt+completion,即当前上下文占用的
    /// 最新实测;回合结束后外层会用权威数字覆盖(set_footer 清基线)。
    pub(crate) fn refresh_round_usage(&mut self, context_tokens: u64, turn: TurnTokens) -> Result<()> {
        let base = self
            .round_base_footer
            .get_or_insert_with(|| Box::new(self.footer.clone()));
        let mut display = (**base).clone();
        display.apply_round_usage(context_tokens, turn);
        self.footer = display;
        if self.rendered && !self.external_output_active {
            synchronized_terminal_update(CursorAfterUpdate::Shown, || self.redraw())?;
        }
        Ok(())
    }

    /// Replaces the footer and redraws the live editor immediately when it is
    /// already on screen. Without the redraw, token/context updates remain
    /// invisible until the next input event causes the editor to render.
    /// Update the background-command strip; returns true when a redraw is
    /// needed (content changed, or spinners/timers must advance).
    pub(crate) fn set_jobs(&mut self, jobs: Vec<crate::tools::jobs::JobOverview>) -> bool {
        let changed = self.jobs.len() != jobs.len()
            || self
                .jobs
                .iter()
                .zip(jobs.iter())
                .any(|(a, b)| a.job_id != b.job_id || a.status != b.status);
        self.jobs = jobs;
        changed
    }

    /// Lightweight spinner/timer repaint of the job strip only — no full
    /// tail redraw, so it can run at animation frequency without flicker.
    pub(crate) fn tick_job_strip(&mut self) -> Result<()> {
        if !self.rendered || self.jobs.is_empty() {
            return Ok(());
        }
        self.job_spinner = self.job_spinner.wrapping_add(1);
        let (cols, _) = terminal::size().unwrap_or((80, 24));
        let lines = background_job_lines(&self.jobs, self.job_spinner, usize::from(cols));
        let rows = lines.len().min(u16::MAX as usize) as u16;
        if rows > self.tail_rows {
            return Ok(());
        }
        let start = self.tail_start.saturating_add(self.tail_rows).saturating_sub(rows);
        let input_cursor = self.input_cursor;
        // Lines are padded to the full terminal width, so plain overwrites
        // suffice — no Clear, no intermediate blank state. The synchronized
        // block keeps the cursor hop invisible over slow links (SSH).
        synchronized_terminal_update(CursorAfterUpdate::Preserve, || {
            let mut stdout = io::stdout();
            let mut row = start;
            for line in &lines {
                queue!(stdout, MoveTo(0, row), Print(line))?;
                row = row.saturating_add(1);
            }
            queue!(stdout, MoveTo(input_cursor.0, input_cursor.1))?;
            stdout.flush()?;
            Ok(())
        })
    }

    pub(crate) fn refresh_footer(&mut self, footer: ReplFooterStatus) -> Result<()> {
        self.set_footer(footer);
        if self.rendered {
            synchronized_terminal_update(CursorAfterUpdate::Shown, || self.redraw())?;
        }
        Ok(())
    }

    pub(crate) fn suspend(&mut self) -> Result<()> {
        if !self.rendered {
            return Ok(());
        }
        let mut stdout = io::stdout();
        let (_, terminal_rows) = terminal::size().unwrap_or((80, 24));
        for offset in 0..self.tail_rows {
            let row = self.tail_start.saturating_add(offset);
            if row >= terminal_rows {
                break;
            }
            queue!(stdout, MoveTo(0, row), Clear(ClearType::CurrentLine))?;
        }
        queue!(stdout, MoveTo(self.output_cursor.0, self.output_cursor.1))?;
        stdout.flush()?;
        self.rendered = false;
        Ok(())
    }

    pub(crate) fn resume(&mut self) -> Result<()> {
        self.resume_at(cursor_position_or(self.output_cursor))
    }

    pub(crate) fn resume_at(&mut self, (output_col, output_row): (u16, u16)) -> Result<()> {
        let (cols, terminal_rows) = terminal::size().unwrap_or((80, 24));
        let terminal_rows = terminal_rows.max(1);
        let editor_rows = repl_input_rendered_rows(
            &self.editor.input,
            self.editor.is_pasted,
            false,
            usize::from(cols),
        );
        let mut queue_lines =
            queued_prompt_lines(&self.queued, self.editor.mode, usize::from(cols));
        let queue_gap = u16::from(!queue_lines.is_empty());
        let max_queue_rows = terminal_rows.saturating_sub(editor_rows).saturating_sub(3) as usize;
        if queue_lines.len() > max_queue_rows {
            let omitted = queue_lines.len() - max_queue_rows.saturating_sub(1);
            let mut clipped = vec![format!(
                "\x1b[2m… {}\x1b[0m",
                if is_zh() {
                    format!("已隐藏 {omitted} 行排队内容")
                } else {
                    format!("{omitted} queued lines hidden")
                }
            )];
            let keep = max_queue_rows.saturating_sub(1);
            clipped.extend(queue_lines.split_off(queue_lines.len().saturating_sub(keep)));
            queue_lines = clipped;
        }
        let job_lines = background_job_lines(&self.jobs, self.job_spinner, usize::from(cols));
        let job_rows = job_lines.len().min(u16::MAX as usize) as u16;
        let total_rows = 1u16
            .saturating_add(queue_lines.len().min(u16::MAX as usize) as u16)
            .saturating_add(queue_gap)
            .saturating_add(editor_rows)
            .saturating_add(job_rows);
        // Derived from what is on screen rather than stored: the tail was
        // pinned to the bottom exactly when its bottom edge sat on the last
        // usable row. `suspend()` leaves both values untouched, so they are
        // still the previous frame's truth here, and a terminal resize simply
        // falls back to natural placement.
        let was_anchored = self.tail_rows > 0
            && self.tail_start.saturating_add(self.tail_rows) == terminal_rows.saturating_sub(1);
        let placement = live_tail_placement(
            output_col,
            output_row,
            total_rows,
            terminal_rows,
            was_anchored,
        );
        if placement.overflow > 0 {
            let mut stdout = io::stdout();
            queue!(stdout, MoveTo(0, terminal_rows.saturating_sub(1)))?;
            for _ in 0..placement.overflow {
                queue!(stdout, Print("\n"))?;
            }
            stdout.flush()?;
        }
        let output_row = placement.output_row;
        let tail_start = placement.tail_start;

        let mut stdout = io::stdout();
        queue!(stdout, MoveTo(0, tail_start), Clear(ClearType::CurrentLine))?;
        let mut row = tail_start.saturating_add(1);
        for line in &queue_lines {
            queue!(
                stdout,
                MoveTo(0, row),
                Clear(ClearType::CurrentLine),
                Print(line)
            )?;
            row = row.saturating_add(1);
        }
        if !queue_lines.is_empty() {
            queue!(stdout, MoveTo(0, row), Clear(ClearType::CurrentLine))?;
            row = row.saturating_add(1);
        }
        stdout.flush()?;

        let mut input_row = row;
        let mut rendered_rows = 0u16;
        render_repl_input_with_footer(
            &mut stdout,
            &mut input_row,
            &mut rendered_rows,
            self.editor.mode,
            &self.editor.input,
            self.editor.cursor,
            self.editor.is_pasted,
            &self.footer,
            false,
        )?;
        // The editor is back on screen: the cursor must be visible no
        // matter which path hid it (e.g. a question prompt suspended the
        // editor with the cursor hidden and then exited early). This is
        // the single convergence point for every editor redraw, so an
        // unconditional Show here prevents a permanently invisible cursor.
        self.input_cursor = cursor_position_or(self.input_cursor);
        if !job_lines.is_empty() {
            let mut stdout = io::stdout();
            let mut job_row = input_row.saturating_add(rendered_rows);
            for line in &job_lines {
                queue!(
                    stdout,
                    MoveTo(0, job_row),
                    Clear(ClearType::CurrentLine),
                    Print(line)
                )?;
                job_row = job_row.saturating_add(1);
            }
            queue!(stdout, MoveTo(self.input_cursor.0, self.input_cursor.1))?;
            stdout.flush()?;
        }
        execute!(io::stdout(), crossterm::cursor::Show)?;
        self.output_cursor = (output_col, output_row);
        self.tail_start = tail_start;
        self.tail_rows = total_rows;
        self.rendered = true;
        Ok(())
    }

    pub(crate) fn apply_output_frame(&mut self, frame: &[u8]) -> Result<()> {
        if frame.is_empty() {
            return Ok(());
        }
        if !self.rendered {
            io::stdout().write_all(frame)?;
            io::stdout().flush()?;
            self.output_cursor = cursor_position_or(self.output_cursor);
            return Ok(());
        }

        let (columns, terminal_rows) = terminal::size().unwrap_or((80, 24));
        let terminal_rows = terminal_rows.max(1);
        let unbounded = terminal_frame_layout(frame, self.output_cursor, columns, None);
        let natural_tail = unbounded
            .cursor
            .1
            .saturating_add(u16::from(unbounded.cursor.0 > 0));
        let occupied_tail = unbounded
            .occupied_bottom
            .map(|row| row.saturating_add(1))
            .unwrap_or(0);
        let desired_tail = natural_tail.max(occupied_tail);
        let max_tail = max_live_tail_start(terminal_rows, self.tail_rows);
        let next_tail = live_tail_next_start(self.tail_start, desired_tail, max_tail);
        let shift = i32::from(next_tail) - i32::from(self.tail_start);
        let frame_margin = if shift < 0 {
            self.tail_start
        } else {
            next_tail
        };
        let output_bottom = live_frame_output_bottom(frame_margin, unbounded);
        let leading_scroll = output_bottom
            .map(|bottom| self.output_cursor.1.saturating_sub(bottom))
            .unwrap_or(0);
        let frame_start = if let Some(bottom) = output_bottom.filter(|_| leading_scroll > 0) {
            (0, bottom)
        } else {
            self.output_cursor
        };
        let bounded = terminal_frame_layout(frame, frame_start, columns, output_bottom);

        let mut transaction = Vec::with_capacity(frame.len().saturating_add(96));
        if shift > 0 {
            queue!(
                transaction,
                MoveTo(0, self.tail_start.saturating_add(1)),
                Print(format!("\x1b[{shift}L"))
            )?;
        }
        if let Some(bottom) = output_bottom {
            queue!(
                transaction,
                Print(format!("\x1b[1;{}r", bottom.saturating_add(1)))
            )?;
        }
        if let Some(bottom) = output_bottom.filter(|_| leading_scroll > 0) {
            queue!(transaction, MoveTo(0, bottom))?;
            for _ in 0..leading_scroll {
                queue!(transaction, Print("\n"))?;
            }
        }
        queue!(transaction, MoveTo(frame_start.0, frame_start.1))?;
        transaction.extend_from_slice(frame);
        queue!(transaction, Print("\x1b[r"))?;
        if shift < 0 {
            queue!(
                transaction,
                MoveTo(0, next_tail.saturating_add(1)),
                Print(format!("\x1b[{}M", -shift))
            )?;
        }
        let input_row = (i32::from(self.input_cursor.1) + shift)
            .clamp(0, i32::from(terminal_rows.saturating_sub(1))) as u16;
        queue!(transaction, MoveTo(self.input_cursor.0, input_row))?;
        let mut stdout = io::stdout();
        stdout.write_all(&transaction)?;
        stdout.flush()?;

        self.output_cursor = bounded.cursor;
        self.tail_start = next_tail;
        self.input_cursor.1 = input_row;
        Ok(())
    }

    pub(crate) fn apply_renderer_frame(&mut self, renderer: &mut render::StreamRenderer) -> Result<()> {
        let frame = renderer.take_output_frame();
        self.apply_output_frame(&frame)
    }

    pub(crate) fn redraw(&mut self) -> Result<()> {
        let output_cursor = self.output_cursor;
        self.suspend()?;
        self.resume_at(output_cursor)
    }

    pub(crate) fn clear_screen(&mut self) -> Result<()> {
        self.suspend()?;
        let mut stdout = io::stdout();
        execute!(stdout, Clear(ClearType::All), MoveTo(0, 0))?;
        self.output_cursor = (0, 0);
        self.tail_start = 0;
        self.tail_rows = 0;
        self.resume_at((0, 0))
    }

    pub(crate) fn enqueue(&mut self, prompt: QueuedPrompt) -> Result<()> {
        let output_cursor = self.output_cursor;
        self.suspend()?;
        self.append_queued(prompt);
        self.resume_at(output_cursor)
    }

    pub(crate) fn append_queued(&mut self, prompt: QueuedPrompt) {
        self.queued.push(prompt);
        self.queued.sort_by_key(|prompt| prompt.seq);
    }

    pub(crate) fn queue_stream_chunk(&mut self, chunk: ChatStreamChunk) {
        if let Some(pending) = self
            .pending_chunks
            .last_mut()
            .filter(|pending| pending.kind == chunk.kind)
        {
            pending.text.push_str(&chunk.text);
        } else {
            self.pending_chunks.push(chunk);
        }
    }

    pub(crate) fn flush_pending_chunks(&mut self, renderer: &mut render::StreamRenderer) -> Result<()> {
        for chunk in std::mem::take(&mut self.pending_chunks) {
            renderer.write_chunk(chunk)?;
        }
        Ok(())
    }

    pub(crate) fn discard_pending_chunks(&mut self) {
        self.pending_chunks.clear();
    }

    pub(crate) fn tick_spinner(&mut self, renderer: &mut render::StreamRenderer) -> Result<()> {
        self.flush_pending_chunks(renderer)?;
        renderer.tick_spinner()?;
        self.apply_renderer_frame(renderer)
    }

    pub(crate) fn commit_submission(&mut self, submission: &LiveSubmission) -> Result<()> {
        self.suspend()?;
        write_committed_user_messages(
            &[(submission.display_content.as_str(), self.editor.mode)],
            true,
        )?;
        self.output_cursor = cursor_position_or(self.output_cursor);
        Ok(())
    }

    pub(crate) fn commit_empty_submission(&mut self) -> Result<()> {
        let mode = self.editor.mode;
        self.editor.clear();
        self.suspend()?;
        write_committed_user_messages(&[("", mode)], true)?;
        let output_cursor = cursor_position_or(self.output_cursor);
        self.output_cursor = output_cursor;
        self.resume_at(output_cursor)
    }

    /// Print a background-command wake reply into the scrollback while the
    /// REPL idles: dim header, then the assistant's report.
    pub(crate) fn show_background_report(&mut self, report: &BackgroundReport) -> Result<()> {
        self.suspend()?;
        let mut stdout = io::stdout();
        queue!(
            stdout,
            Print(format!(
                "\x1b[2m⚙ {}\x1b[0m\r\n\r\n",
                job_wake_headline(&report.headline)
            ))
        )?;
        for line in report.reply.lines() {
            queue!(stdout, Print(format!("{}\r\n", render::render_markdown_line(line))))?;
        }
        queue!(stdout, Print("\r\n"))?;
        stdout.flush()?;
        self.output_cursor = cursor_position_or(self.output_cursor);
        let output_cursor = self.output_cursor;
        self.resume_at(output_cursor)
    }

    /// Remove queued bubbles without committing them as sent messages —
    /// the daemon dropped these prompts (explicit cancel), they were never
    /// answered and never entered the conversation.
    pub(crate) fn drop_queued(&mut self, prompt_ids: &[String]) -> Result<()> {
        let ids = prompt_ids.iter().collect::<std::collections::HashSet<_>>();
        if !self.queued.iter().any(|prompt| ids.contains(&prompt.prompt_id)) {
            return Ok(());
        }
        let output_cursor = self.output_cursor;
        self.suspend()?;
        self.queued
            .retain(|prompt| !ids.contains(&prompt.prompt_id));
        self.resume_at(output_cursor)
    }

    pub(crate) fn consume_queued(&mut self, prompt_ids: &[String], mode: AgentMode) -> Result<()> {
        self.suspend()?;
        let ids = prompt_ids.iter().collect::<std::collections::HashSet<_>>();
        let consumed = self
            .queued
            .iter()
            .filter(|prompt| ids.contains(&prompt.prompt_id))
            .map(|prompt| (prompt.display_content.as_str(), mode))
            .collect::<Vec<_>>();
        write_committed_user_messages(&consumed, true)?;
        self.queued
            .retain(|prompt| !ids.contains(&prompt.prompt_id));
        let output_cursor = cursor_position_or(self.output_cursor);
        self.output_cursor = output_cursor;
        self.resume_at(output_cursor)
    }

    pub(crate) fn reload_queue(&mut self, state: &StateStore) -> Result<()> {
        let output_cursor = self.output_cursor;
        self.suspend()?;
        self.queued = state.load_queued_prompts()?;
        self.resume_at(output_cursor)
    }
}

pub(crate) fn repl_input_rendered_rows(
    input: &str,
    is_pasted: bool,
    show_shortcut_hint: bool,
    cols: usize,
) -> u16 {
    let suggestions = repl_command_suggestions(input);
    let lines = repl_input_lines(input);
    let display_lines =
        repl_visible_input_lines("  ", &lines, REPL_MAX_VISIBLE_INPUT_ROWS, is_pasted);
    let input_rows = repl_wrapped_input_rows_for_cols("  ", &display_lines, cols)
        .len()
        .max(1)
        .min(u16::MAX as usize) as u16;
    input_rows.saturating_add(if show_shortcut_hint && suggestions.is_empty() {
        4
    } else {
        3
    })
}

pub(crate) const JOB_SPINNER_FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

pub(crate) fn format_job_duration(seconds: u64) -> String {
    if seconds >= 3600 {
        format!("{}h {:02}m", seconds / 3600, (seconds % 3600) / 60)
    } else if seconds >= 60 {
        format!("{}m {:02}s", seconds / 60, seconds % 60)
    } else {
        format!("{seconds}s")
    }
}

/// Status strip under the footer: a leading blank line, then one line per
/// background command with a blank line between entries. Timers are
/// right-aligned to the terminal width.
pub(crate) fn background_job_lines(
    jobs: &[crate::tools::jobs::JobOverview],
    spinner_phase: usize,
    cols: usize,
) -> Vec<String> {
    if jobs.is_empty() {
        return Vec::new();
    }
    let kind_label = |job: &crate::tools::jobs::JobOverview| {
        if job.kind == "subagent" {
            crate::i18n::text("agent", "子代理")
        } else {
            crate::i18n::text("cmd", "命令")
        }
    };
    // Pad kinds to one column so mixed command/subagent rows keep their ids
    // and titles vertically aligned.
    let kind_col = jobs
        .iter()
        .map(|job| visible_width(kind_label(job)))
        .max()
        .unwrap_or(0);
    let mut lines = vec![String::new()];
    for job in jobs.iter() {
        let marker = JOB_SPINNER_FRAMES[spinner_phase % JOB_SPINNER_FRAMES.len()];
        let kind_word = kind_label(job);
        let kind_pad = " ".repeat(kind_col.saturating_sub(visible_width(kind_word)));
        let mut left = format!("{marker} {kind_word}{kind_pad} {} · {}", job.job_id, job.title);
        let timer = format_job_duration(job.runtime_seconds);
        let timer_width = visible_width(&timer);
        // Never exceed the terminal width: a wrapped strip line would shift
        // the whole tail and flicker.
        let max_left = cols.saturating_sub(timer_width).saturating_sub(2);
        while visible_width(&left) > max_left && !left.is_empty() {
            left.pop();
        }
        let left_width = visible_width(&left);
        let pad = cols
            .saturating_sub(left_width)
            .saturating_sub(timer_width)
            .max(1);
        lines.push(format!(
            "\x1b[2m{left}{}{timer}\x1b[0m",
            " ".repeat(pad)
        ));
    }
    lines
}

pub(crate) fn queued_prompt_lines(prompts: &[QueuedPrompt], mode: AgentMode, cols: usize) -> Vec<String> {
    let mut lines = Vec::new();
    for (index, prompt) in prompts.iter().enumerate() {
        if index > 0 {
            lines.push(String::new());
        }
        lines.extend(submitted_echo_lines(mode, &prompt.display_content, cols));
        lines.push(format!(
            "{} {}",
            submitted_echo_bar(mode),
            primary_footer_text(t("Queued", "排队中"))
        ));
    }
    lines
}

pub(crate) fn write_committed_user_messages(messages: &[(&str, AgentMode)], leading_gap: bool) -> Result<()> {
    if messages.is_empty() {
        return Ok(());
    }
    let mut stdout = io::stdout();
    let col = cursor_col_or(0);
    if col > 0 {
        writeln!(stdout)?;
    }
    let cols = terminal_cols();
    write!(
        stdout,
        "{}",
        committed_user_messages_text(messages, leading_gap, cols)
    )?;
    stdout.flush()?;
    Ok(())
}

pub(crate) fn committed_user_messages_text(
    messages: &[(&str, AgentMode)],
    leading_gap: bool,
    cols: usize,
) -> String {
    let mut output = String::new();
    if leading_gap {
        output.push('\n');
    }
    for (index, (content, mode)) in messages.iter().enumerate() {
        if index > 0 {
            output.push('\n');
        }
        for line in submitted_echo_lines(*mode, content, cols) {
            output.push_str(&line);
            output.push('\n');
        }
    }
    output.push('\n');
    output
}

/// Strips the bracketed prefix off a background-job wake headline, leaving
/// `子代理完成 82bea3 · 标题`. The older `[后台命令完成] ` spelling still shows
/// up in sessions recorded before the rename.
pub(crate) fn job_wake_headline(headline: &str) -> &str {
    headline
        .strip_prefix("[后台任务完成] ")
        .or_else(|| headline.strip_prefix("[后台命令完成] "))
        .unwrap_or(headline)
}

/// Fires a desktop notification unless the REPL window has focus.
///
/// `focused` is `None` when there is no live tail — a one-shot `gqy ask` has
/// no window to be away from, so it stays quiet.
pub(crate) fn notify_if_unfocused(config: &AppConfig, focused: Option<bool>, title: &str, body: &str) {
    if !config.notifications.enabled || focused != Some(false) {
        return;
    }
    crate::notify::notify(title, &crate::notify::clip_body(body, 120));
}

/// Redraws finished turns of a session as one ANSI frame.
///
/// Feeds the stored transcript back through the same `StreamRenderer` a live
/// turn uses, so tool blocks and prose come out identical — and re-wrapped for
/// the terminal's *current* width, which a saved byte transcript could not do.
/// Turns older than the transcript column fall back to prompt + final reply.
pub(crate) fn session_replay_frame(
    replays: &[crate::state::TurnReplay],
    mode: AgentMode,
    config: &AppConfig,
    cols: usize,
) -> Result<Vec<u8>> {
    use crate::state::ReplayEntry;
    let mut frame = Vec::new();
    for replay in replays {
        if replay.is_job_wake {
            // A background job woke the session; live rendering shows a dim
            // `⚙` notice, never a user bubble. Mirror that.
            frame.extend_from_slice(
                format!(
                    "\n\x1b[2m⚙ {}\x1b[0m\n\n",
                    job_wake_headline(&replay.display_content)
                )
                .as_bytes(),
            );
        } else if !replay.display_content.trim().is_empty() {
            frame.extend_from_slice(
                committed_user_messages_text(&[(&replay.display_content, mode)], true, cols)
                    .as_bytes(),
            );
        }
        let mut renderer = render::StreamRenderer::new(
            render::ReasoningDisplayMode::Hidden,
            render::ToolCallDisplayMode::from_config(&config.display.tool_calls),
            false,
            config.display.readable_tool_names,
            config.display.command_output_lines,
        );
        renderer.use_external_cursor_control();
        renderer.use_buffered_output();
        if replay.entries.is_empty() {
            renderer.write_chunk(ChatStreamChunk {
                kind: crate::llm::ChatStreamKind::Content,
                text: replay.assistant_content.clone(),
            })?;
        } else {
            for entry in &replay.entries {
                match entry {
                    ReplayEntry::Text { text } => renderer.write_chunk(ChatStreamChunk {
                        kind: crate::llm::ChatStreamKind::Content,
                        text: text.clone(),
                    })?,
                    ReplayEntry::ToolCall { name, arguments } => {
                        renderer.write_tool_call(name, arguments)?
                    }
                    ReplayEntry::ToolResult { name, ok, output } => {
                        renderer.write_tool_result(name, *ok, output)?
                    }
                }
            }
        }
        renderer.finish()?;
        frame.extend_from_slice(&renderer.take_output_frame());
    }
    Ok(frame)
}

pub(crate) fn queued_prompt_attachments(
    images: &[Option<crate::clipboard::PastedImage>],
) -> Vec<QueuedPromptAttachment> {
    images
        .iter()
        .filter_map(|image| match image {
            Some(crate::clipboard::PastedImage::Binary(image)) => {
                Some(QueuedPromptAttachment::Binary {
                    mime: image.mime.clone(),
                    data_base64: base64::engine::general_purpose::STANDARD.encode(&image.data),
                })
            }
            Some(crate::clipboard::PastedImage::Path(path)) => {
                Some(QueuedPromptAttachment::Path { path: path.clone() })
            }
            None => None,
        })
        .collect()
}

pub(crate) fn persist_queued_submission(
    state: &StateStore,
    submission: &LiveSubmission,
) -> Result<QueuedPrompt> {
    let prompt_id = format!(
        "queued_{}_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_millis())
            .unwrap_or(0),
        rand::random::<u16>()
    );
    state.enqueue_prompt(
        &prompt_id,
        &submission.content,
        &submission.display_content,
        &queued_prompt_attachments(&submission.images),
    )
}

/// Queues a submission for the turn currently running in the daemon, using
/// the cross-process queue target so the daemon consumes it mid-turn.
pub(crate) async fn persist_remote_queued_submission(
    paths: &GQYPaths,
    run_id: &str,
    turn_id: &str,
    submission: &LiveSubmission,
) -> Result<QueuedPrompt> {
    let mut stream = ipc::connect(&paths.ipc_socket()).await?;
    ipc::send(
        &mut stream,
        &IpcRequest::new(IpcCommand::QueueTurnUpdate {
            run_id: run_id.to_string(),
            turn_id: turn_id.to_string(),
            content: submission.content.clone(),
            display_content: submission.display_content.clone(),
            images: ipc_images(&submission.images),
            supersede: false,
        }),
    )
    .await?;
    match ipc::receive::<IpcFrame>(&mut stream).await? {
        Some(IpcFrame::TurnUpdateAccepted {
            prompt_id,
            seq,
            submitted_at,
            ..
        }) => Ok(QueuedPrompt {
            prompt_id,
            seq,
            content: submission.content.clone(),
            display_content: submission.display_content.clone(),
            attachments: queued_prompt_attachments(&submission.images),
            uploaded_attachments: Vec::new(),
            submitted_at,
        }),
        Some(IpcFrame::Error { message, .. }) => bail!("{message}"),
        Some(_) => bail!("GQY core returned an invalid queue response"),
        None => bail!("GQY core closed the queue connection"),
    }
}

pub(crate) struct LiveRawMode {
    show_cursor_on_drop: bool,
    restore_terminal_on_drop: bool,
    keyboard_enhancement: KeyboardEnhancementState,
}

pub(crate) struct ReplCursorRestore;

impl Drop for ReplCursorRestore {
    pub(crate) fn drop(&mut self) {
        // 1. 会话级兜底：恢复括号粘贴与光标
        // 2. 再关闭 raw mode；键盘增强由 LiveRawMode / 局部输入作用域负责 Pop
        let _ = execute!(io::stdout(), DisableBracketedPaste, DisableFocusChange, Show);
        let _ = terminal::disable_raw_mode();
    }
}

impl LiveRawMode {
    /// 进入 live REPL 的 raw 输入模式，并尽量启用键盘增强协议。
    ///
    /// 参数: 无
    ///
    /// 返回:
    /// - 成功时返回会在 Drop 时恢复终端的守卫对象
    pub(crate) fn start() -> Result<Self> {
        enable_live_raw_mode()?;
        let mut stdout = io::stdout();
        if let Err(error) = execute!(stdout, EnableBracketedPaste) {
            let _ = terminal::disable_raw_mode();
            return Err(error.into());
        }
        // Focus reporting is advisory: terminals that ignore it simply never
        // send the events, and the editor stays on its "focused" default.
        let _ = execute!(stdout, EnableFocusChange);
        Ok(Self {
            show_cursor_on_drop: true,
            restore_terminal_on_drop: true,
            keyboard_enhancement: KeyboardEnhancementState::enable(&mut stdout),
        })
    }

    /// 接管上一段 live 输入已启用的终端模式，避免重复 Push 键盘增强。
    ///
    /// 参数: 无
    ///
    /// 返回:
    /// - 会在最终 Drop 时恢复终端的守卫对象
    pub(crate) fn adopt() -> Self {
        Self {
            show_cursor_on_drop: true,
            restore_terminal_on_drop: true,
            keyboard_enhancement: KeyboardEnhancementState::assume_active(),
        }
    }

    pub(crate) fn keep_cursor_hidden(&mut self) {
        self.show_cursor_on_drop = false;
    }

    pub(crate) fn handoff(&mut self) {
        self.restore_terminal_on_drop = false;
        // handoff 后由下一段 LiveRawMode::adopt 继续持有键盘增强状态
        self.keyboard_enhancement = KeyboardEnhancementState::default();
    }
}

pub(crate) fn enable_live_raw_mode() -> Result<()> {
    terminal::enable_raw_mode()?;
    spawn_hangup_watchdog();
    if let Err(error) = restore_live_output_processing() {
        let _ = terminal::disable_raw_mode();
        return Err(error);
    }
    Ok(())
}

#[cfg(unix)]
pub(crate) fn restore_live_output_processing() -> Result<()> {
    let mut attributes = std::mem::MaybeUninit::<libc::termios>::uninit();
    // Raw input is required for key events, but renderer output still relies on newline translation.
    unsafe {
        if libc::tcgetattr(libc::STDOUT_FILENO, attributes.as_mut_ptr()) != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let mut attributes = attributes.assume_init();
        attributes.c_oflag |= libc::OPOST | libc::ONLCR;
        if libc::tcsetattr(libc::STDOUT_FILENO, libc::TCSANOW, &attributes) != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    Ok(())
}

#[cfg(not(unix))]
pub(crate) fn restore_live_output_processing() -> Result<()> {
    Ok(())
}

impl Drop for LiveRawMode {
    pub(crate) fn drop(&mut self) {
        if !self.restore_terminal_on_drop {
            return;
        }
        let mut stdout = io::stdout();
        if self.show_cursor_on_drop {
            let _ = execute!(stdout, DisableBracketedPaste, DisableFocusChange, Show);
        } else {
            let _ = execute!(stdout, DisableBracketedPaste, DisableFocusChange);
        }
        // 1. 先 Pop 键盘增强协议
        // 2. 再退出 raw mode
        self.keyboard_enhancement.disable(&mut stdout);
        let _ = terminal::disable_raw_mode();
    }
}

/// Shared feed state between the remote REPL and its IPC poll thread.
#[derive(Default)]
pub(crate) struct SharedJobsFeed {
    /// The owning REPL's current session — strip snapshots are filtered to
    /// it (daemon "current session" can drift from the REPL's after /new).
    repl_session: std::sync::Mutex<Option<String>>,
    jobs: std::sync::Mutex<Vec<crate::tools::jobs::JobOverview>>,
    /// Rendered wake-turn reports waiting to be printed into the scrollback.
    reports: std::sync::Mutex<Vec<BackgroundReport>>,
    /// Latest session Σ read straight from the store. Background subagents
    /// bill to the session that launched them, but they finish long after the
    /// turn that spawned them published its totals — without this the footer
    /// sat on a stale Σ until the user happened to send another prompt.
    cumulative: std::sync::Mutex<Option<TurnTokens>>,
    /// Active daemon-initiated wake runs: (run_id, session_id, label).
    wake_runs: std::sync::Mutex<Vec<(String, String, String)>>,
    /// Wake runs already attached to (never re-follow), and turn ids that
    /// were rendered live (their DB report must not print again).
    followed_runs: std::sync::Mutex<std::collections::HashSet<String>>,
    rendered_turns: std::sync::Mutex<std::collections::HashSet<String>>,
}

#[derive(Clone)]
pub(crate) struct BackgroundReport {
    turn_id: String,
    headline: String,
    reply: String,
}

/// Session isolation for the strip: keep only `session`'s jobs (sessionless
/// jobs stay visible as a legacy fallback; `None` session shows everything).
pub(crate) fn retain_session_jobs(jobs: &mut Vec<crate::tools::jobs::JobOverview>, session: Option<&str>) {
    if let Some(session) = session {
        jobs.retain(|job| {
            job.session_id.is_none() || job.session_id.as_deref() == Some(session)
        });
    }
}

/// Source of background-command snapshots for the idle status strip.
pub(crate) enum JobsFeed {
    /// Remote REPL: snapshots pushed by the IPC poll thread.
    Shared(std::sync::Arc<SharedJobsFeed>),
    /// Direct REPL: read the in-process registry.
    Local,
}

impl JobsFeed {
    pub(crate) fn current(&self) -> Vec<crate::tools::jobs::JobOverview> {
        match self {
            JobsFeed::Shared(shared) => shared.jobs.lock().unwrap().clone(),
            JobsFeed::Local => crate::tools::jobs::overview(),
        }
    }

    /// The store's current Σ for the REPL's session, or `None` when this feed
    /// has no store behind it.
    pub(crate) fn cumulative(&self) -> Option<TurnTokens> {
        match self {
            JobsFeed::Shared(shared) => *shared.cumulative.lock().unwrap(),
            JobsFeed::Local => None,
        }
    }

    pub(crate) fn take_reports(&self) -> Vec<BackgroundReport> {
        match self {
            JobsFeed::Shared(shared) => {
                let mut reports = shared.reports.lock().unwrap();
                let rendered = shared.rendered_turns.lock().unwrap();
                let taken = reports
                    .drain(..)
                    .filter(|report| !rendered.contains(&report.turn_id))
                    .collect();
                taken
            }
            JobsFeed::Local => Vec::new(),
        }
    }

    /// Next wake run in `session` that has not been followed yet; marks it
    /// followed so the caller attaches exactly once.
    pub(crate) fn claim_wake_run(&self, session: &str) -> Option<(String, String)> {
        let JobsFeed::Shared(shared) = self else {
            return None;
        };
        let wake_runs = shared.wake_runs.lock().unwrap();
        let mut followed = shared.followed_runs.lock().unwrap();
        for (run_id, run_session, label) in wake_runs.iter() {
            if run_session == session && !followed.contains(run_id) {
                followed.insert(run_id.clone());
                return Some((run_id.clone(), label.clone()));
            }
        }
        None
    }
}

/// Poll the daemon for background commands while the remote REPL idles:
/// 1s when commands are live, 3s when quiet — a unix-socket roundtrip
/// costs microseconds either way.
pub(crate) fn spawn_jobs_poll_thread(paths: GQYPaths) -> std::sync::Arc<SharedJobsFeed> {
    let shared = std::sync::Arc::new(SharedJobsFeed::default());
    let feed = shared.clone();
    std::thread::spawn(move || {
        let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        else {
            return;
        };
        // Track per-session watermarks so wake replies print exactly once,
        // and never replay history from before this REPL started.
        let mut seen: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
        // The store open can lose a race against daemon writes (SQLITE_BUSY);
        // retry every cycle instead of deciding at startup forever.
        let mut store: Option<StateStore> = None;
        loop {
            if store.is_none() {
                store = StateStore::new(&paths).ok();
            }
            let (jobs, session_id, wake_runs) = runtime
                .block_on(async {
                    tokio::time::timeout(
                        std::time::Duration::from_millis(500),
                        fetch_jobs_overview(&paths),
                    )
                    .await
                    .unwrap_or_else(|_| Ok((Vec::new(), None, Vec::new())))
                })
                .unwrap_or_default();
            let mut jobs = jobs;
            let repl_session = { feed.repl_session.lock().unwrap().clone() };
            retain_session_jobs(&mut jobs, repl_session.as_deref());
            *feed.jobs.lock().unwrap() = jobs;
            *feed.wake_runs.lock().unwrap() = wake_runs;
            if let (Some(store), Some(session)) = (store.as_ref(), repl_session.as_deref()) {
                if let Ok(totals) = store.pinned(session).session_cumulative_token_totals() {
                    *feed.cumulative.lock().unwrap() = Some(totals);
                }
            }
            if let (Some(store), Some(session_id)) = (store.as_ref(), session_id) {
                let watermark = match seen.entry(session_id.clone()) {
                    std::collections::hash_map::Entry::Occupied(entry) => *entry.get(),
                    std::collections::hash_map::Entry::Vacant(entry) => {
                        let latest = store.latest_turn_seq(&session_id).unwrap_or(0);
                        *entry.insert(latest)
                    }
                };
                if let Ok(rows) = store.background_report_replies_after(&session_id, watermark) {
                    for (seq, turn_id, display, reply) in rows {
                        seen.insert(session_id.clone(), seq);
                        if feed.rendered_turns.lock().unwrap().contains(&turn_id) {
                            continue;
                        }
                        feed.reports.lock().unwrap().push(BackgroundReport {
                            turn_id,
                            headline: display,
                            reply,
                        });
                    }
                }
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
    });
    shared
}

