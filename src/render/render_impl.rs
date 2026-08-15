//! render_impl — 自 src/render/mod.rs 拆分。

pub(crate) use super::*;

impl StreamRenderer {
    pub fn new(
        reasoning_mode: ReasoningDisplayMode,
        tool_call_mode: ToolCallDisplayMode,
        plain: bool,
        readable_tool_names: bool,
        command_output_lines: usize,
    ) -> Self {
        Self {
            reasoning_mode,
            tool_call_mode,
            plain,
            mode: None,
            cursor_hidden: false,
            external_cursor_control: false,
            output: RenderOutput::Terminal,
            markdown: MarkdownStreamRenderer::new(),
            reasoning_text: String::new(),
            reasoning_tokens: 0,
            reasoning_title: None,
            reasoning_started_at: None,
            reasoning_elapsed: None,
            tool_stats: BTreeMap::new(),
            tool_seq: 0,
            readable_tool_names,
            command_output_lines,
            command_display: None,
            summary_line_active: false,
            summary_lines_active: 0,
            last_tool_summary: String::new(),
            live_summary: io::stdout().is_terminal(),
            wait_spinner: None,
            last_tick: None,
            preparing_question_started_at: None,
            tool_preparing: None,
            subagent_mode: None,
            sent_meme_filter: SentMemeStreamFilter::default(),
        }
    }

    pub fn use_external_cursor_control(&mut self) {
        self.external_cursor_control = true;
    }

    pub fn use_buffered_output(&mut self) {
        self.output = RenderOutput::Buffered(Vec::new());
    }

    pub fn take_output_frame(&mut self) -> Vec<u8> {
        match &mut self.output {
            RenderOutput::Terminal => Vec::new(),
            RenderOutput::Buffered(buffer) => std::mem::take(buffer),
        }
    }

    pub fn start_waiting(&mut self) -> Result<()> {
        if self.plain
            || self.wait_spinner.is_some()
            || self.command_display.is_some()
            || !WaitSpinner::supported()
        {
            return Ok(());
        }
        self.hide_cursor()?;
        let phase = self.waiting_phase_text();
        self.wait_spinner = Some(WaitSpinner::start(phase, SpinnerStyle::Scanner));
        self.last_tick = None;
        self.tick_spinner()?;
        Ok(())
    }

    pub fn start_reasoning_phase(&mut self, received_at: std::time::Instant) -> Result<()> {
        self.preparing_question_started_at = None;
        self.tool_preparing = None;
        if self.reasoning_mode == ReasoningDisplayMode::Summary {
            self.reasoning_started_at = Some(received_at);
            self.reasoning_elapsed = None;
            self.reasoning_title = None;
            self.reasoning_text.clear();
            self.reasoning_tokens = 0;
        }
        self.start_waiting()?;
        if self.wait_spinner.is_some() {
            self.set_waiting_phase(self.waiting_phase_text());
            self.last_tick = None;
            self.tick_spinner()?;
        }
        Ok(())
    }

    pub fn waiting_phase_text(&self) -> String {
        if let Some(started_at) = self.preparing_question_started_at {
            return format!(
                "{} · {}",
                t("~ Preparing question", "~ 准备问题"),
                format_reasoning_elapsed(started_at.elapsed())
            );
        }
        if let Some((phase, started_at)) = self.tool_preparing {
            return format!(
                "~ {phase} · {}",
                format_reasoning_elapsed(started_at.elapsed())
            );
        }
        match self.reasoning_mode {
            ReasoningDisplayMode::Summary => {
                if self.reasoning_title.is_some() || !self.reasoning_text.is_empty() {
                    self.reasoning_live_text()
                } else {
                    self.reasoning_elapsed_text()
                }
            }
            ReasoningDisplayMode::Full => String::new(),
            ReasoningDisplayMode::Hidden => t("thinking", "思考").to_string(),
        }
    }

    pub fn write_reasoning_title(&mut self, title: &str) -> Result<()> {
        if self.reasoning_mode != ReasoningDisplayMode::Summary || self.plain {
            return Ok(());
        }
        let title = redact_sensitive_inline(&sanitize_terminal_text(title));
        let title = clip_progress_line(&title, 80);
        if title.is_empty() {
            return Ok(());
        }
        self.reasoning_title = Some(title);
        self.ensure_waiting_phase(self.reasoning_live_text(), SpinnerStyle::Scanner)
    }

    pub fn start_reasoning_part(&mut self, received_at: std::time::Instant) -> Result<()> {
        if self.reasoning_mode != ReasoningDisplayMode::Summary {
            return Ok(());
        }
        self.end_active_stream_line()?;
        if self.reasoning_title.is_some() || !self.reasoning_text.is_empty() {
            self.freeze_reasoning_elapsed_at(received_at);
            self.finalize_reasoning_summary()?;
            self.reasoning_started_at = Some(received_at);
        } else if self.reasoning_started_at.is_none() {
            self.reasoning_started_at = Some(received_at);
        }
        self.reasoning_elapsed = None;
        self.reasoning_title = None;
        self.reasoning_text.clear();
        self.reasoning_tokens = 0;
        self.start_waiting()
    }

    pub fn finish_reasoning_part(&mut self, received_at: std::time::Instant) -> Result<()> {
        if self.reasoning_mode != ReasoningDisplayMode::Summary {
            return Ok(());
        }
        if self.reasoning_title.is_some() || !self.reasoning_text.is_empty() {
            self.freeze_reasoning_elapsed_at(received_at);
            self.finalize_reasoning_summary()?;
            self.reasoning_started_at = Some(received_at);
            self.reasoning_elapsed = None;
        }
        Ok(())
    }

    pub fn reset_reasoning_phase(&mut self, received_at: std::time::Instant) -> Result<()> {
        if self.reasoning_mode != ReasoningDisplayMode::Summary {
            return Ok(());
        }
        self.stop_waiting()?;
        if self.summary_line_active {
            self.clear_summary_lines()?;
        }
        self.reasoning_title = None;
        self.reasoning_text.clear();
        self.reasoning_tokens = 0;
        self.reasoning_started_at = Some(received_at);
        self.reasoning_elapsed = None;
        self.mode = None;
        self.start_waiting()
    }

    pub fn tick_spinner(&mut self) -> Result<()> {
        let now = std::time::Instant::now();
        let should_tick = self
            .last_tick
            .map(|last| now.duration_since(last) >= SPINNER_INTERVAL)
            .unwrap_or(true);
        if should_tick {
            let subagent_timer_active = self.has_running_subagent_timer();
            // Both sticky hints win over the tool/reasoning summaries below:
            // they describe what the turn is blocked on right now, and the
            // summaries would otherwise overwrite them on the very first tick
            // after they are set — before the spinner has drawn once.
            if (self.preparing_question_started_at.is_some() || self.tool_preparing.is_some())
                && self.wait_spinner.is_some()
            {
                self.set_waiting_phase(self.waiting_phase_text());
            } else if self.tool_call_mode == ToolCallDisplayMode::Summary
                && !self.tool_stats.is_empty()
                && self.wait_spinner.is_some()
            {
                let (header, sub) = self.tool_summary_live();
                self.set_tool_waiting_phase(&header, sub.as_deref());
            } else if self.reasoning_mode == ReasoningDisplayMode::Summary
                && self.reasoning_started_at.is_some()
                && self.wait_spinner.is_some()
            {
                self.set_waiting_phase(self.waiting_phase_text());
            }
            if let Some(display) = &mut self.command_display {
                debug_assert!(self.wait_spinner.is_none());
                display.tick(&mut self.output)?;
            } else if let Some(spinner) = &mut self.wait_spinner {
                spinner.tick(&mut self.output)?;
            }
            if self.wait_spinner.is_some()
                || self.command_display.is_some()
                || subagent_timer_active
            {
                self.last_tick = Some(now);
            }
        }
        Ok(())
    }

    pub fn write_chunk(&mut self, chunk: ChatStreamChunk) -> Result<()> {
        if chunk.kind == ChatStreamKind::ToolCall {
            if chunk.text == "ask_question" {
                self.start_preparing_question()?;
            }
            return Ok(());
        }
        if matches!(
            chunk.kind,
            ChatStreamKind::ReasoningPartStart
                | ChatStreamKind::ReasoningPartEnd
                | ChatStreamKind::ReasoningReset
        ) {
            return Ok(());
        }
        if !self.plain {
            self.hide_cursor()?;
        }
        let text = normalize_stream_text(&chunk.text);
        let text = if chunk.kind == ChatStreamKind::Content {
            self.sent_meme_filter.push(&text)
        } else {
            text
        };
        if text.is_empty() {
            return Ok(());
        }
        if self.plain && chunk.kind == ChatStreamKind::Reasoning {
            return Ok(());
        }
        if self.reasoning_mode == ReasoningDisplayMode::Hidden
            && chunk.kind == ChatStreamKind::Reasoning
        {
            return Ok(());
        }
        if self.reasoning_mode == ReasoningDisplayMode::Summary
            && chunk.kind == ChatStreamKind::Reasoning
        {
            self.finalize_tools_summary()?;
            self.record_reasoning_text(&text);
            self.mode = Some(ChatStreamKind::Reasoning);
            self.ensure_waiting_phase(self.reasoning_live_text(), SpinnerStyle::Scanner)?;
            return Ok(());
        }
        self.stop_waiting()?;
        if self.mode != Some(chunk.kind) {
            if chunk.kind == ChatStreamKind::Content {
                self.finalize_reasoning_summary()?;
                self.finalize_tools_summary()?;
            } else if chunk.kind == ChatStreamKind::Reasoning {
                self.finalize_tools_summary()?;
            }
            self.switch_mode(chunk.kind)?;
        }
        let stdout = &mut self.output;
        if chunk.kind == ChatStreamKind::Reasoning {
            write_full_reasoning_chunk(stdout, &text)?;
        } else if self.plain {
            write!(stdout, "{text}")?;
        } else {
            write!(stdout, "{}", self.markdown.push(&text))?;
        }
        stdout.flush()?;
        Ok(())
    }

    pub fn write_tool_call(&mut self, name: &str, arguments: &str) -> Result<()> {
        // The arguments finished arriving, so the "still receiving" hint has
        // done its job and hands the spinner back to the tool summary.
        self.tool_preparing = None;
        if self.plain {
            return Ok(());
        }
        if name == "ask_question" {
            return self.start_preparing_question();
        }
        self.release_transient_output()?;
        if is_silent_tool(name) {
            return Ok(());
        }
        if name == "run_command" {
            let mut display = CommandLiveDisplay::new(
                arguments,
                self.command_output_lines,
                self.tool_call_mode != ToolCallDisplayMode::Hidden,
                self.tool_call_mode == ToolCallDisplayMode::Full,
            );
            if self.live_summary {
                display.tick(&mut self.output)?;
                self.last_tick = None;
            }
            self.command_display = Some(display);
            return Ok(());
        }
        if is_subagent_tool(name) && self.tool_call_mode != ToolCallDisplayMode::Hidden {
            let stats = self.tool_stats_entry(name);
            stats.started_at = Some(std::time::Instant::now());
            stats.elapsed = None;
        }
        if self.tool_call_mode == ToolCallDisplayMode::Full {
            let display_name = self.display_tool_name(name);
            let stdout = &mut self.output;
            writeln!(stdout, "{} {}", t("tool", "工具"), display_name)?;
            write_tool_payload(stdout, t("args", "参数"), arguments)?;
            stdout.flush()?;
        } else if self.tool_call_mode == ToolCallDisplayMode::Summary {
            let stats = self.tool_stats_entry(name);
            stats.calls += 1;
            stats.subject = tool_subject(name, arguments);
            self.ensure_tool_waiting_phase()?;
        }
        Ok(())
    }

    pub fn write_tool_preparing(&mut self, name: &str) -> Result<()> {
        if self.plain {
            return Ok(());
        }
        let Some(phase) = crate::tools::preparing_phase(name) else {
            return Ok(());
        };
        self.release_transient_output()?;
        // Set before the spinner exists: `ensure_waiting_phase` ticks
        // immediately, and that tick re-derives the phase from renderer state.
        // Without the sticky field the tool summary or the reasoning timer wins
        // there and the hint never reaches the screen.
        self.tool_preparing = Some((phase, std::time::Instant::now()));
        // Braille + the dim tool palette: this is a tool starting up, not the
        // model thinking, and the scanner/green pair reads as the latter.
        self.ensure_waiting_phase(self.waiting_phase_text(), SpinnerStyle::Braille)
    }

    pub fn write_tool_result(&mut self, name: &str, ok: bool, output: &str) -> Result<()> {
        if self.plain {
            return Ok(());
        }
        if is_silent_tool(name) && ok {
            return Ok(());
        }
        self.stop_waiting()?;
        self.end_subagent_stream_line()?;
        let status = if ok { "ok" } else { "err" };
        let elapsed = self.finish_subagent_timer(name);
        if name == "run_command" {
            if let Some(mut display) = self.command_display.take() {
                display.set_result(ok);
                let include_output = self.tool_call_mode == ToolCallDisplayMode::Summary
                    || (self.tool_call_mode == ToolCallDisplayMode::Full && !ok);
                if self.live_summary {
                    display.commit(&mut self.output, include_output)?;
                } else {
                    display.write_static(&mut self.output, include_output)?;
                }
                self.last_tick = None;
            }
            if self.tool_call_mode == ToolCallDisplayMode::Full {
                let stdout = &mut self.output;
                write_command_result_blocks(stdout, output)?;
                stdout.flush()?;
            }
            return Ok(());
        }
        if matches!(name, "todowrite" | "todoupdate") && ok {
            self.release_transient_output()?;
            let stdout = &mut self.output;
            if write_todo_table(stdout, output)? {
                stdout.flush()?;
                if self.tool_call_mode == ToolCallDisplayMode::Summary {
                    let stats = self.tool_stats_entry(name);
                    stats.ok += 1;
                    stats.progress = None;
                    self.tool_stats.clear();
                    self.last_tool_summary.clear();
                }
                return Ok(());
            }
        }
        if self.tool_call_mode == ToolCallDisplayMode::Full {
            self.release_transient_output()?;
            let display_name = self.display_tool_name(name);
            let stdout = &mut self.output;
            writeln!(
                stdout,
                "{} {} {}",
                t("result", "结果"),
                display_name,
                tool_result_status(status, elapsed)
            )?;
            write_tool_payload(stdout, t("output", "输出"), output)?;
            stdout.flush()?;
            self.tool_stats.remove(name);
        } else if self.tool_call_mode == ToolCallDisplayMode::Summary {
            let stats = self.tool_stats_entry(name);
            if ok {
                stats.ok += 1;
            } else {
                stats.error += 1;
            }
            stats.progress = None;
            if self.tool_stats.values().any(|stats| !stats.settled()) {
                // Siblings still running (parallel subagents): freeze this
                // tool's block in the live area; commit only when the whole
                // batch settles.
                self.update_tool_summary_display()?;
            } else {
                self.finalize_tools_summary()?;
            }
        }
        Ok(())
    }

    pub fn write_command_output(
        &mut self,
        name: &str,
        stream: CommandOutputStream,
        chunk: &[u8],
    ) -> Result<()> {
        if self.plain || name != "run_command" {
            return Ok(());
        }
        if let Some(display) = &mut self.command_display {
            display.push(stream, chunk);
        }
        Ok(())
    }

    pub fn write_tool_progress(&mut self, name: &str, message: &str) -> Result<()> {
        if let Some(phase) = message.strip_prefix("__tool_phase__") {
            if self.plain {
                let stdout = &mut self.output;
                writeln!(stdout, "{phase}")?;
                stdout.flush()?;
            } else if self.wait_spinner.is_some() {
                self.set_waiting_phase(phase.to_string());
                self.tick_spinner()?;
            } else {
                self.render_summary_line(phase, SummaryStyle::Tool)?;
            }
            return Ok(());
        }
        if let Some(json) = message.strip_prefix("__patch_preview__") {
            self.release_transient_output()?;
            let stdout = &mut self.output;
            if write_patch_result(stdout, json)? {
                stdout.flush()?;
            }
            return Ok(());
        }
        if self.plain {
            return Ok(());
        }
        if message == "__external_output__" {
            self.prepare_for_external_output()?;
            return Ok(());
        }
        if let Some(text) = message.strip_prefix("__subagent_detach__") {
            if self.tool_call_mode == ToolCallDisplayMode::Full {
                self.release_transient_output()?;
                let display_name = self.display_tool_name(name);
                let stdout = &mut self.output;
                writeln!(stdout, "{} {}: {text}", t("progress", "进度"), display_name)?;
                stdout.flush()?;
            } else if self.tool_call_mode == ToolCallDisplayMode::Summary {
                // Lands as the block's `↳` subject line, not the final `✓`
                // stats line — detach is a fact about the call, not a result.
                let stats = self.tool_stats_entry(name);
                stats.subject = Some(text.to_string());
                stats.detached = true;
                self.update_tool_summary_display()?;
            }
            return Ok(());
        }
        if let Some(text) = message.strip_prefix(crate::tools::TOOL_SUMMARY_PREFIX) {
            if self.tool_call_mode == ToolCallDisplayMode::Full {
                self.release_transient_output()?;
                let stdout = &mut self.output;
                for line in text.lines() {
                    writeln!(stdout, "{line}")?;
                }
                stdout.flush()?;
            } else if self.tool_call_mode == ToolCallDisplayMode::Summary {
                self.tool_stats_entry(name).final_progress = Some(text.to_string());
                self.update_tool_summary_display()?;
            }
            return Ok(());
        }
        if let Some(text) = message.strip_prefix("__subagent_stats__") {
            if self.tool_call_mode == ToolCallDisplayMode::Full {
                self.release_transient_output()?;
                let display_name = self.display_tool_name(name);
                let stdout = &mut self.output;
                writeln!(stdout, "{} {}: {text}", t("progress", "进度"), display_name)?;
                stdout.flush()?;
            } else if self.tool_call_mode == ToolCallDisplayMode::Summary {
                self.tool_stats_entry(name).final_progress = Some(text.to_string());
                self.update_tool_summary_display()?;
            }
            return Ok(());
        }
        if let Some(text) = message.strip_prefix("__subagent_reasoning__") {
            let text = normalize_stream_text(text);
            if self.tool_call_mode == ToolCallDisplayMode::Full {
                if self.subagent_mode != Some(ChatStreamKind::Reasoning) {
                    self.stop_waiting()?;
                    self.clear_summary_lines()?;
                    self.end_active_stream_line()?;
                    let stdout = &mut self.output;
                    writeln!(stdout)?;
                    stdout.flush()?;
                }
                let stdout = &mut self.output;
                write_full_reasoning_chunk(stdout, &text)?;
                stdout.flush()?;
                self.subagent_mode = Some(ChatStreamKind::Reasoning);
            }
            return Ok(());
        }
        if let Some(json) = message.strip_prefix("__subtool_call__") {
            if let Ok(value) = serde_json::from_str::<Value>(json) {
                let tool_name = value
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                if self.tool_call_mode == ToolCallDisplayMode::Full {
                    let args = value.get("args").and_then(Value::as_str).unwrap_or("");
                    self.release_transient_output()?;
                    let display_name = self.display_tool_name(tool_name);
                    let stdout = &mut self.output;
                    if tool_name == "run_command" {
                        write_command_block(stdout, args)?;
                    } else {
                        writeln!(stdout, "{} {}", t("tool", "工具"), display_name)?;
                        write_tool_payload(stdout, t("args", "参数"), args)?;
                    }
                    stdout.flush()?;
                }
            }
            return Ok(());
        }
        if let Some(json) = message.strip_prefix("__subtool_result__") {
            if let Ok(value) = serde_json::from_str::<Value>(json) {
                let tool_name = value
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                let ok = value.get("ok").and_then(Value::as_bool).unwrap_or(true);
                if self.tool_call_mode == ToolCallDisplayMode::Full {
                    let args = value.get("args").and_then(Value::as_str).unwrap_or("");
                    let output = value.get("output").and_then(Value::as_str).unwrap_or("");
                    let status = if ok { "ok" } else { "err" };
                    self.release_transient_output()?;
                    let display_name = self.display_tool_name(tool_name);
                    let stdout = &mut self.output;
                    if tool_name == "run_command" {
                        write_command_block_with_status(
                            stdout,
                            args,
                            if ok {
                                CommandStatus::Ok
                            } else {
                                CommandStatus::Error
                            },
                        )?;
                        write_command_result_blocks(stdout, output)?;
                        write_command_block_gap(stdout, true)?;
                    } else {
                        writeln!(stdout, "{} {} {status}", t("result", "结果"), display_name)?;
                        write_tool_payload(stdout, t("output", "输出"), output)?;
                    }
                    stdout.flush()?;
                }
            }
            return Ok(());
        }
        if is_silent_tool(name) {
            return Ok(());
        }
        if self.tool_call_mode == ToolCallDisplayMode::Full {
            self.release_transient_output()?;
            let display_name = self.display_tool_name(name);
            let stdout = &mut self.output;
            writeln!(
                stdout,
                "{} {}: {message}",
                t("progress", "进度"),
                display_name
            )?;
            stdout.flush()?;
        } else if self.tool_call_mode == ToolCallDisplayMode::Summary {
            self.tool_stats_entry(name).progress = Some(message.to_string());
            self.update_tool_summary_display()?;
        }
        Ok(())
    }

    pub fn update_tool_summary_display(&mut self) -> Result<()> {
        self.end_subagent_stream_line()?;
        if self.wait_spinner.is_some() {
            let (header, sub) = self.tool_summary_live();
            self.set_tool_waiting_phase(&header, sub.as_deref());
        } else {
            self.end_active_stream_line()?;
            self.finalize_reasoning_summary()?;
            self.ensure_tool_waiting_phase()?;
        }
        Ok(())
    }

    pub fn prepare_for_external_output(&mut self) -> Result<()> {
        self.preparing_question_started_at = None;
        self.tool_preparing = None;
        self.release_transient_output()?;
        self.finalize_tools_summary()?;
        self.show_cursor()?;
        Ok(())
    }

    pub fn write_system_message(&mut self, message: &str) -> Result<()> {
        self.prepare_for_external_output()?;
        let stdout = &mut self.output;
        execute!(stdout, SetForegroundColor(Color::DarkGrey), MoveToColumn(0))?;
        writeln!(stdout, "{message}")?;
        execute!(stdout, ResetColor)?;
        stdout.flush()?;
        Ok(())
    }

    pub fn write_compact_chunk(&mut self, chunk: &ChatStreamChunk) -> Result<()> {
        if chunk.kind != ChatStreamKind::Content {
            return Ok(());
        }
        self.prepare_for_external_output()?;
        let stdout = &mut self.output;
        execute!(stdout, SetForegroundColor(Color::DarkGrey))?;
        write!(stdout, "{}", chunk.text)?;
        execute!(stdout, ResetColor)?;
        stdout.flush()?;
        Ok(())
    }

    pub fn finish_compact(&mut self) -> Result<()> {
        let stdout = &mut self.output;
        execute!(stdout, ResetColor)?;
        writeln!(stdout)?;
        stdout.flush()?;
        Ok(())
    }

    pub fn finish(&mut self) -> Result<()> {
        self.preparing_question_started_at = None;
        self.tool_preparing = None;
        self.stop_waiting()?;
        if let Some(mut display) = self.command_display.take() {
            display.commit(
                &mut self.output,
                self.tool_call_mode == ToolCallDisplayMode::Summary,
            )?;
        }
        self.end_subagent_stream_line()?;
        if self.mode == Some(ChatStreamKind::Content) && !self.plain {
            let stdout = &mut self.output;
            let pending = self.sent_meme_filter.finish();
            if !pending.is_empty() {
                write!(stdout, "{}", self.markdown.push(&pending))?;
            }
            write!(stdout, "{}", self.markdown.flush())?;
            stdout.flush()?;
        }
        if self.mode == Some(ChatStreamKind::Reasoning) {
            execute!(self.output, ResetColor)?;
        }
        if stream_needs_terminating_newline(self.mode, self.reasoning_mode) {
            writeln!(self.output)?;
        }
        self.finalize_reasoning_summary()?;
        self.finalize_tools_summary()?;
        if self.summary_line_active {
            self.clear_summary_lines()?;
        }
        self.mode = None;
        self.show_cursor()?;
        Ok(())
    }

    pub fn switch_mode(&mut self, mode: ChatStreamKind) -> Result<()> {
        let stdout = &mut self.output;
        match mode {
            ChatStreamKind::Reasoning => {
                if self.mode.is_some() {
                    writeln!(stdout)?;
                }
            }
            ChatStreamKind::Content => {
                if self.mode == Some(ChatStreamKind::Reasoning) {
                    execute!(stdout, ResetColor)?;
                    writeln!(stdout)?;
                    writeln!(stdout)?;
                }
            }
            ChatStreamKind::ToolCall => return Ok(()),
            ChatStreamKind::ReasoningPartStart | ChatStreamKind::ReasoningPartEnd => return Ok(()),
            ChatStreamKind::ReasoningReset => return Ok(()),
        }
        stdout.flush()?;
        self.mode = Some(mode);
        Ok(())
    }

    pub fn end_active_stream_line(&mut self) -> Result<()> {
        if self.reasoning_mode == ReasoningDisplayMode::Summary
            && self.mode == Some(ChatStreamKind::Reasoning)
        {
            self.mode = None;
            return Ok(());
        }
        let was_reasoning = self.mode == Some(ChatStreamKind::Reasoning);
        if was_reasoning {
            execute!(self.output, ResetColor)?;
        } else if self.mode == Some(ChatStreamKind::Content) && !self.plain {
            let stdout = &mut self.output;
            write!(stdout, "{}", self.markdown.flush())?;
            stdout.flush()?;
        }
        if self.mode.is_some() {
            writeln!(self.output)?;
            if was_reasoning {
                writeln!(self.output)?;
            }
            self.mode = None;
        }
        Ok(())
    }

    pub fn finalize_reasoning_summary(&mut self) -> Result<()> {
        if self.reasoning_mode == ReasoningDisplayMode::Summary
            && (self.reasoning_title.is_some() || !self.reasoning_text.is_empty())
        {
            self.stop_waiting()?;
            let summary = self.reasoning_summary_text();
            if self.summary_line_active {
                self.clear_summary_lines()?;
                self.summary_line_active = false;
                self.summary_lines_active = 0;
            }
            let stdout = &mut self.output;
            write_activity_summary(stdout, &summary, SummaryStyle::Reasoning)?;
            stdout.flush()?;
            self.reasoning_text.clear();
            self.reasoning_tokens = 0;
            self.reasoning_title = None;
            self.reasoning_started_at = None;
            self.reasoning_elapsed = None;
            self.mode = None;
        }
        Ok(())
    }

    pub fn end_subagent_stream_line(&mut self) -> Result<()> {
        let was_reasoning = self.subagent_mode == Some(ChatStreamKind::Reasoning);
        if was_reasoning {
            execute!(self.output, ResetColor)?;
        }
        if self.subagent_mode.is_some() {
            writeln!(self.output)?;
            if was_reasoning {
                writeln!(self.output)?;
            }
            self.subagent_mode = None;
        }
        Ok(())
    }

    pub fn finalize_tools_summary(&mut self) -> Result<()> {
        if self.tool_call_mode == ToolCallDisplayMode::Summary && !self.tool_stats.is_empty() {
            self.stop_waiting()?;
            execute!(self.output, ResetColor)?;
            let summary = self.tool_summary_text();
            if self.summary_line_active {
                self.clear_summary_lines()?;
                self.summary_line_active = false;
                self.summary_lines_active = 0;
            }
            let stdout = &mut self.output;
            write_activity_summary(stdout, &summary, SummaryStyle::Tool)?;
            stdout.flush()?;
            self.tool_stats.clear();
            self.last_tool_summary.clear();
        }
        Ok(())
    }

    pub fn render_summary_line(&mut self, text: &str, style: SummaryStyle) -> Result<()> {
        self.stop_waiting()?;
        if !self.live_summary {
            return Ok(());
        }
        self.clear_summary_lines()?;
        let stdout = &mut self.output;
        let lines = transient_summary_lines(text, command_terminal_width());
        for (index, line) in lines.iter().enumerate() {
            if index > 0 {
                writeln!(stdout)?;
            }
            execute!(stdout, MoveToColumn(0))?;
            write!(stdout, "{}\x1b[K", style_summary_text(line, style))?;
        }
        stdout.flush()?;
        self.summary_line_active = true;
        self.summary_lines_active = lines.len().max(1) as u16;
        Ok(())
    }

    pub fn clear_summary_lines(&mut self) -> Result<()> {
        if !self.summary_line_active {
            return Ok(());
        }
        let stdout = &mut self.output;
        let lines = self.summary_lines_active.max(1);
        for index in 0..lines {
            if index > 0 {
                execute!(stdout, crossterm::cursor::MoveUp(1))?;
            }
            execute!(stdout, MoveToColumn(0), Clear(ClearType::CurrentLine))?;
        }
        stdout.flush()?;
        self.summary_line_active = false;
        self.summary_lines_active = 0;
        Ok(())
    }

    pub fn reasoning_summary_text(&self) -> String {
        let elapsed = self.reasoning_elapsed_text();
        format!("{} · {elapsed}", self.reasoning_live_metrics_text())
    }

    pub fn reasoning_live_text(&self) -> String {
        if self.reasoning_started_at.is_none() {
            return match &self.reasoning_title {
                Some(title) if crate::i18n::is_zh() => {
                    format!("{}：{title}", t("thinking", "思考"))
                }
                Some(title) => format!("{}: {title}", t("thinking", "思考")),
                None => t("thinking", "思考").to_string(),
            };
        }
        let elapsed = self.reasoning_elapsed_text();
        format!("{} · {elapsed}", self.reasoning_live_metrics_text())
    }

    pub fn reasoning_elapsed_text(&self) -> String {
        self.reasoning_elapsed
            .or_else(|| self.reasoning_started_at.map(|started| started.elapsed()))
            .map(format_reasoning_elapsed)
            .unwrap_or_else(|| "0ms".to_string())
    }

    pub fn freeze_reasoning_elapsed_at(&mut self, received_at: std::time::Instant) {
        self.reasoning_elapsed = self
            .reasoning_started_at
            .map(|started_at| received_at.saturating_duration_since(started_at));
    }

    pub fn reasoning_live_metrics_text(&self) -> String {
        let phase = match &self.reasoning_title {
            Some(title) if crate::i18n::is_zh() => {
                format!("{}：{title}", t("thinking", "思考"))
            }
            Some(title) => format!("{}: {title}", t("thinking", "思考")),
            None => t("thinking", "思考").to_string(),
        };
        if self.reasoning_tokens == 0 {
            return phase;
        }
        format!(
            "{phase} · {} {}",
            self.reasoning_tokens,
            t("tokens", "词元")
        )
    }

    pub fn record_reasoning_text(&mut self, text: &str) {
        self.reasoning_started_at
            .get_or_insert_with(std::time::Instant::now);
        self.reasoning_text.push_str(text);
        // Incremental: recounting the whole accumulated text on every chunk is
        // O(n²) over the stream and the value only feeds the spinner label.
        // Per-chunk sums drift <1% from a full recount (BPE merges across
        // chunk boundaries) — fine for a display estimate.
        self.reasoning_tokens += crate::token_estimate::estimate_tokens(text);
    }

    /// Gets or creates a tool's stats entry, stamping first-seen order so
    /// parallel blocks render in launch order rather than name order.
    pub fn tool_stats_entry(&mut self, name: &str) -> &mut ToolStats {
        self.tool_seq += 1;
        let seq = self.tool_seq;
        self.tool_stats
            .entry(name.to_string())
            .or_insert_with(|| ToolStats {
                seq,
                ..ToolStats::default()
            })
    }

    /// Tools in first-seen order (stable for direct test inserts with seq 0).
    pub fn ordered_tool_stats(&self) -> Vec<(&String, &ToolStats)> {
        let mut entries: Vec<_> = self.tool_stats.iter().collect();
        entries.sort_by_key(|(_, stats)| stats.seq);
        entries
    }

    pub fn tool_summary_text(&self) -> String {
        self.ordered_tool_stats()
            .into_iter()
            .map(|(name, stats)| self.tool_block_lines(name, stats, false).join("\n"))
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    /// Builds one tool's display block: a `~`-prefixed status header plus
    /// its own subject/progress lines. In `live` mode a still-running tool's
    /// header carries [`wait_spinner::BLOCK_MARKER`] so the spinner animates
    /// it, and a settled tool freezes into its final `✓` stats in place. The
    /// committed variant (`live == false`) prefers `final_progress` with `✓`.
    pub fn tool_block_lines(&self, name: &str, stats: &ToolStats, live: bool) -> Vec<String> {
        let display = self.display_tool_name(name);
        let mut header = tool_status_text(&display, stats, is_subagent_tool(name));
        if inline_tool_subject(name) {
            if let Some(subject) = &stats.subject {
                header.push_str(" · ");
                header.push_str(subject);
            }
        }
        let header = self.tool_summary_with_prefix(header);
        // In live mode a running block's detail lines are indented to sit
        // under its spinner glyph; a settled block drops the glyph and sits
        // flush, matching the committed layout.
        let running_live = live && !stats.settled();
        // Detail lines always sit two columns in, matching command blocks
        // (`$ …` / `  ↳ cmd` / `  │ output`) and avoiding the leftward jump
        // a block used to make when it settled.
        let detail_indent = "  ";
        let mut lines = Vec::new();
        if running_live {
            lines.push(format!("{}{header}", wait_spinner::BLOCK_MARKER));
        } else {
            lines.push(header);
        }
        if !inline_tool_subject(name) {
            if let Some(subject) = &stats.subject {
                // Subagent headers already carry the description — don't
                // repeat it as a subject line.
                if !lines[0].contains(subject.as_str()) {
                    lines.push(format!("{detail_indent}↳ {subject}"));
                }
            }
        }
        let (progress_text, is_final) = if live {
            if stats.settled() {
                (stats.final_progress.as_ref(), true)
            } else {
                (stats.progress.as_ref(), false)
            }
        } else if stats.final_progress.is_some() {
            (stats.final_progress.as_ref(), true)
        } else {
            (stats.progress.as_ref(), false)
        };
        let progress_prefix = if is_final { "✓" } else { "↳" };
        if let Some(message) = progress_text {
            for line in message.lines().filter(|line| !line.trim().is_empty()) {
                let line = if is_final {
                    clip_progress_line_preserving_spaces(line, 120)
                } else {
                    clip_progress_line(line, 120)
                };
                // 自带记号的行原样保留:失败清单是 `✗ …`,再套一个 `✓` 就成了
                // 「✓ ✗ 权限不足」。
                if line.starts_with('✗') {
                    lines.push(format!("{detail_indent}{line}"));
                } else {
                    lines.push(format!("{detail_indent}{progress_prefix} {line}"));
                }
            }
        }
        lines
    }

    pub fn tool_summary_header(&self) -> String {
        let parts = self
            .ordered_tool_stats()
            .into_iter()
            .map(|(name, stats)| {
                let display = self.display_tool_name(name);
                let mut header = tool_status_text(&display, stats, is_subagent_tool(name));
                if inline_tool_subject(name) {
                    if let Some(subject) = &stats.subject {
                        header.push_str(" · ");
                        header.push_str(subject);
                    }
                }
                header
            })
            .collect::<Vec<_>>()
            .join(", ");
        self.tool_summary_with_prefix(parts)
    }

    /// Live status for the wait spinner. A single tool keeps the classic
    /// one-line phase + progress sub-block. Multiple tools (e.g. parallel
    /// subagents) switch the spinner into block mode: the phase line is
    /// empty and every tool renders as its own block — running blocks carry
    /// their own animated glyph, settled blocks freeze into their final
    /// stats, and blocks are separated by blank lines:
    ///
    /// ```text
    /// ⠋ ~ 子代理·任务A×1 运行中 · 3s
    ///   ↳ 任务A进度
    ///
    ///   ~ 子代理·任务B×1 ok · 2s
    ///   ✓ 工具调用 1 次
    /// ```
    pub fn tool_summary_live(&self) -> (String, Option<String>) {
        if self.tool_stats.len() <= 1 {
            return (self.tool_summary_header(), self.tool_summary_progress());
        }
        let blocks = self
            .ordered_tool_stats()
            .into_iter()
            .map(|(name, stats)| self.tool_block_lines(name, stats, true).join("\n"))
            .collect::<Vec<_>>()
            .join("\n\n");
        (String::new(), Some(blocks))
    }

    pub fn tool_summary_with_prefix(&self, parts: String) -> String {
        if self.tool_stats.len() == 1
            && self
                .tool_stats
                .keys()
                .next()
                .is_some_and(|name| name == "run_command")
        {
            format!("$ {parts}")
        } else {
            format!("~ {parts}")
        }
    }

    pub fn tool_summary_progress(&self) -> Option<String> {
        for (name, stats) in self.ordered_tool_stats() {
            let mut lines = Vec::new();
            if !inline_tool_subject(name) {
                if let Some(subject) = &stats.subject {
                    // Skip subjects already shown in the header (subagent
                    // descriptions are part of the display name).
                    if !self.display_tool_name(name).contains(subject.as_str()) {
                        lines.push(format!("  ↳ {subject}"));
                    }
                }
            }
            if let Some(message) = &stats.progress {
                let progress = message
                    .lines()
                    .filter(|line| !line.trim().is_empty())
                    .map(|line| format!("  ↳ {}", clip_progress_line(line, 120)))
                    .collect::<Vec<_>>()
                    .join("\n");
                if !progress.is_empty() {
                    lines.push(progress);
                }
            }
            if !lines.is_empty() {
                return Some(lines.join("\n"));
            }
        }
        None
    }

    pub fn has_running_subagent_timer(&self) -> bool {
        self.tool_stats
            .iter()
            .any(|(name, stats)| is_subagent_tool(name) && stats.started_at.is_some())
    }

    pub fn finish_subagent_timer(&mut self, name: &str) -> Option<std::time::Duration> {
        if !is_subagent_tool(name) {
            return None;
        }
        let stats = self.tool_stats.get_mut(name)?;
        let elapsed = stats.started_at.take()?.elapsed();
        stats.elapsed = Some(elapsed);
        Some(elapsed)
    }

    pub fn display_tool_name<'a>(&self, name: &'a str) -> String {
        // Subagents keep their per-call description so parallel task calls
        // show as separate lines: "子代理·<描述>".
        if let Some(description) = name.strip_prefix("task:") {
            let base = if self.readable_tool_names {
                readable_tool_name("task")
            } else {
                "task".to_string()
            };
            return format!("{base}·{description}");
        }
        let name = tool_event_base_name(name);
        if self.readable_tool_names {
            readable_tool_name(name)
        } else {
            name.to_string()
        }
    }

    pub fn hide_cursor(&mut self) -> Result<()> {
        if self.external_cursor_control {
            return Ok(());
        }
        if !self.cursor_hidden && !self.plain && self.wait_spinner.is_none() {
            execute!(self.output, Hide)?;
            self.cursor_hidden = true;
        }
        Ok(())
    }

    pub fn show_cursor(&mut self) -> Result<()> {
        if self.external_cursor_control {
            return Ok(());
        }
        if self.cursor_hidden && !self.plain {
            execute!(self.output, Show)?;
            self.cursor_hidden = false;
        }
        Ok(())
    }

    pub fn set_waiting_phase(&mut self, phase: String) {
        if let Some(spinner) = &mut self.wait_spinner {
            spinner.set_phase(phase);
        }
    }

    pub fn ensure_waiting_phase(&mut self, phase: String, style: SpinnerStyle) -> Result<()> {
        if self.command_display.is_some() {
            return Ok(());
        }
        if self.plain || !WaitSpinner::supported() {
            if self.summary_line_active {
                self.clear_summary_lines()?;
            }
            self.render_summary_line(&phase, summary_style_for(style))?;
            return Ok(());
        }
        if self.wait_spinner.is_none() {
            self.wait_spinner = Some(WaitSpinner::start(phase, style));
            self.last_tick = None;
            self.tick_spinner()?;
        } else {
            self.set_waiting_phase(phase);
        }
        Ok(())
    }

    pub fn ensure_tool_waiting_phase(&mut self) -> Result<()> {
        debug_assert!(self.command_display.is_none());
        let (header, sub) = self.tool_summary_live();
        if self.plain || !self.live_summary {
            let summary = match &sub {
                Some(s) if header.is_empty() => s.clone(),
                Some(s) => format!("{header}\n{s}"),
                None => header,
            };
            let summary = summary.replace(wait_spinner::BLOCK_MARKER, "");
            if self.summary_line_active {
                self.clear_summary_lines()?;
            }
            self.last_tool_summary = summary.clone();
            return self.render_summary_line(&summary, SummaryStyle::Tool);
        }
        if self.summary_line_active {
            self.clear_summary_lines()?;
        }
        if self.wait_spinner.is_none() {
            self.hide_cursor()?;
            self.wait_spinner = Some(WaitSpinner::start(header, SpinnerStyle::Braille));
            self.last_tick = None;
        } else {
            self.set_waiting_phase(header);
        }
        if let Some(spinner) = &mut self.wait_spinner {
            spinner.set_sub_phase(sub);
        }
        self.tick_spinner()
    }

    pub fn start_preparing_question(&mut self) -> Result<()> {
        if self.plain || self.preparing_question_started_at.is_some() {
            return Ok(());
        }
        self.release_transient_output()?;
        self.preparing_question_started_at = Some(std::time::Instant::now());
        if !WaitSpinner::supported() {
            return Ok(());
        }
        self.hide_cursor()?;
        self.wait_spinner = Some(WaitSpinner::start(
            self.waiting_phase_text(),
            SpinnerStyle::Braille,
        ));
        self.last_tick = None;
        self.tick_spinner()
    }

    pub fn set_tool_waiting_phase(&mut self, header: &str, sub: Option<&str>) {
        if let Some(spinner) = &mut self.wait_spinner {
            spinner.set_phase(header.to_string());
            spinner.set_sub_phase(sub.map(|s| s.to_string()));
        }
    }

    pub fn stop_waiting(&mut self) -> Result<()> {
        if let Some(mut spinner) = self.wait_spinner.take() {
            spinner.stop(&mut self.output)?;
        }
        self.last_tick = None;
        Ok(())
    }

    pub fn release_transient_output(&mut self) -> Result<()> {
        self.stop_waiting()?;
        if let Some(mut display) = self.command_display.take() {
            display.commit(
                &mut self.output,
                self.tool_call_mode == ToolCallDisplayMode::Summary,
            )?;
        }
        self.end_subagent_stream_line()?;
        self.end_active_stream_line()?;
        self.finalize_reasoning_summary()?;
        self.clear_summary_lines()
    }
}
