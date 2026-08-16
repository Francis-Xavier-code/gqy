//! jobs — 自 src/cli.rs 拆分。

// ponytail: 与 src/tools/jobs.rs（后台命令 job 工具）同名易混淆：本文件是 CLI 的 job 命令；
// 改名（如 jobs_cmd.rs）后删除本注释。
#![allow(clippy::too_many_arguments)]
pub(crate) use super::*;

type JobsOverviewSnapshot = (
    Vec<crate::tools::jobs::JobOverview>,
    Option<String>,
    Vec<(String, String, String)>,
);

pub(crate) async fn fetch_jobs_overview(paths: &GQYPaths) -> Result<JobsOverviewSnapshot> {
    let mut stream = ipc::connect(&paths.ipc_socket()).await?;
    ipc::send(&mut stream, &IpcRequest::new(IpcCommand::JobsOverview)).await?;
    match ipc::receive::<IpcFrame>(&mut stream).await? {
        Some(IpcFrame::AdminResult { state, data }) => {
            let wake_runs = data
                .get("wake_runs")
                .and_then(serde_json::Value::as_array)
                .map(|rows| {
                    rows.iter()
                        .filter_map(|row| {
                            Some((
                                row.get("run_id")?.as_str()?.to_string(),
                                row.get("session_id")?.as_str()?.to_string(),
                                row.get("label")
                                    .and_then(serde_json::Value::as_str)
                                    .unwrap_or_default()
                                    .to_string(),
                            ))
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            Ok((
                data.get("jobs")
                    .cloned()
                    .map(serde_json::from_value)
                    .transpose()
                    .unwrap_or_default()
                    .unwrap_or_default(),
                Some(state.session_id),
                wake_runs,
            ))
        }
        _ => Ok((Vec::new(), None, Vec::new())),
    }
}

/// 终端已死(PTY 对端关闭):POLLHUP/POLLERR/POLLNVAL 任一命中。
/// 不发 SIGHUP 的断开路径(tmux kill-pane、终端崩溃、SSH 掉线)只能靠它
/// 兜底——否则 crossterm 的 poll 对 EOF fd 永远立即就绪、read 又读不出
/// 事件,REPL 主循环全速空转,留下一个 98% CPU 的残留进程。
/// 挂断看门狗:独立线程每 500ms 裸 poll 探测 stdin 挂断,确认后给优雅
/// 退出路径 5 秒宽限——主线程若卡死在 crossterm 对 HUP fd 的任何内部
/// 自旋(事件 poll、CPR 应答等待,均为实测形态),由这里强制收尾,
/// 保证关终端后绝不留下吃 CPU 的残留进程。
pub(crate) fn spawn_hangup_watchdog() {
    pub(crate) static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        std::thread::spawn(|| loop {
            std::thread::sleep(Duration::from_millis(500));
            if terminal_hangup() {
                std::thread::sleep(Duration::from_secs(5));
                if terminal_hangup() {
                    std::process::exit(1);
                }
            }
        });
    });
}

pub(crate) fn terminal_hangup() -> bool {
    let mut pollfd = libc::pollfd {
        fd: libc::STDIN_FILENO,
        events: 0,
        revents: 0,
    };
    let ready = unsafe { libc::poll(&mut pollfd, 1, 0) };
    ready == 1 && (pollfd.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL)) != 0
}

pub(crate) enum LiveReplOutcome {
    Exit,
    Submit(
        AgentMode,
        String,
        Vec<Option<crate::clipboard::PastedImage>>,
    ),
    /// A daemon-initiated wake turn is running in this session; the caller
    /// should attach and render it live.
    FollowWake {
        run_id: String,
        label: String,
    },
    /// Ctrl+C on an empty line while this session has background work: stop
    /// the work and stay in the REPL. Pressing it again then exits.
    StopJobs,
}

pub(crate) fn read_live_repl_input(
    live: &mut LiveReplTail,
    paths: &GQYPaths,
    jobs_feed: &JobsFeed,
    wake_session: Option<&str>,
) -> Result<LiveReplOutcome> {
    let mut raw = if std::mem::take(&mut live.raw_mode_handoff) {
        LiveRawMode::adopt()
    } else {
        LiveRawMode::start()?
    };
    if !live.rendered {
        synchronized_terminal_update(CursorAfterUpdate::Shown, || live.resume())?;
    }
    let mut last_key_at = Instant::now();
    loop {
        // 等待权自持:PTY 死亡后 crossterm 的 poll 会在内部对 HUP fd
        // 无限自旋、永不返回(实测),所以不能把"等 80ms"交给它——用裸
        // poll 等待并率先识别挂断,有输入就绪时才让 crossterm 取事件。
        let mut pollfd = libc::pollfd {
            fd: libc::STDIN_FILENO,
            events: libc::POLLIN,
            revents: 0,
        };
        let ready = unsafe { libc::poll(&mut pollfd, 1, 80) };
        if ready == 1 && (pollfd.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL)) != 0 {
            return Ok(LiveReplOutcome::Exit);
        }
        // 就绪判定必须问 crossterm(它的内部缓冲对裸 poll 不可见):
        // 一次 read 会把 fd 里的字节全部吞进内部缓冲,只看 fd 会让
        // 积压按键滞留到下一次按键或超时才放行,打字手感直接变卡。
        let has_input = event::poll(Duration::ZERO)?;
        if !has_input {
            // Idle tick: structural changes redraw the whole tail; otherwise
            // only the strip repaints. While the user is actively typing the
            // animation pauses so the two repaint sources never interleave.
            for report in jobs_feed.take_reports() {
                synchronized_terminal_update(CursorAfterUpdate::Preserve, || {
                    live.show_background_report(&report)
                })?;
            }
            let typing = last_key_at.elapsed() < Duration::from_millis(350);
            if typing {
                continue;
            }
            if let Some(session) = wake_session {
                if let Some((run_id, label)) = jobs_feed.claim_wake_run(session) {
                    return Ok(LiveReplOutcome::FollowWake { run_id, label });
                }
            }
            let cumulative_changed = jobs_feed
                .cumulative()
                .is_some_and(|totals| live.footer.update_cumulative_tokens(totals));
            if live.set_jobs(jobs_feed.current()) || cumulative_changed {
                synchronized_terminal_update(CursorAfterUpdate::Preserve, || live.redraw())?;
            } else {
                live.tick_job_strip()?;
            }
            continue;
        }
        // 抽干本轮就绪的全部事件再回到等待,粘贴/快速输入不积压。
        while event::poll(Duration::ZERO)? {
            // read 前再验挂断:HUP 的 fd 会让 poll 报就绪却读不出事件,
            // 直接 read 就掉进 crossterm 的自旋。
            if terminal_hangup() {
                return Ok(LiveReplOutcome::Exit);
            }
            last_key_at = Instant::now();
            match live.editor.handle_event(event::read()?, paths, false)? {
                LiveEditorAction::None => {}
                LiveEditorAction::Redraw => {
                    synchronized_terminal_update(CursorAfterUpdate::Preserve, || live.redraw())?
                }
                LiveEditorAction::ClearScreen => {
                    synchronized_terminal_update(CursorAfterUpdate::Preserve, || {
                        live.clear_screen()
                    })?
                }
                LiveEditorAction::EmptySubmit => {
                    synchronized_terminal_update(CursorAfterUpdate::Preserve, || {
                        live.commit_empty_submission()
                    })?
                }
                LiveEditorAction::Submit(submission) => {
                    let mode = live.mode();
                    synchronized_terminal_update(CursorAfterUpdate::Hidden, || {
                        live.commit_submission(&submission)
                    })?;
                    raw.keep_cursor_hidden();
                    return Ok(LiveReplOutcome::Submit(
                        mode,
                        submission.content,
                        submission.images,
                    ));
                }
                // Ctrl+C rung 3: the draft was empty and no reply is running, but
                // this session still has background work — stop that before the
                // press is allowed to mean "quit". `live.jobs` holds only running
                // jobs of this session, refreshed on every idle tick. Ctrl+D
                // (`Exit`) always quits outright.
                LiveEditorAction::Interrupt if !live.jobs.is_empty() => {
                    return Ok(LiveReplOutcome::StopJobs);
                }
                LiveEditorAction::Interrupt | LiveEditorAction::Exit => {
                    synchronized_terminal_update(CursorAfterUpdate::Hidden, || live.suspend())?;
                    return Ok(LiveReplOutcome::Exit);
                }
            }
        }
    }
}

/// Attach to a daemon-initiated wake turn and render it live: streaming
/// content, reasoning, and tool activity, exactly like a user-started turn.
/// ESC detaches (the turn keeps running; the DB report is suppressed because
/// the turn was already rendered here). Typed submissions queue into the
/// wake turn as follow-ups.
pub(crate) async fn follow_wake_run(
    paths: &GQYPaths,
    live: &mut LiveReplTail,
    run_id: &str,
    label: &str,
    jobs_feed: &JobsFeed,
    jobs_shared: &std::sync::Arc<SharedJobsFeed>,
) -> Result<()> {
    let config = AppConfig::load_or_default(paths)?;
    let mut stream = ipc::connect(&paths.ipc_socket()).await?;
    ipc::send(
        &mut stream,
        &IpcRequest::new(IpcCommand::FollowRun {
            run_id: run_id.to_string(),
        }),
    )
    .await?;
    let mut turn_id: Option<String> = match ipc::receive::<IpcFrame>(&mut stream).await? {
        Some(IpcFrame::Accepted { turn_id, .. }) => turn_id,
        // Run already finished — the DB report path will print it instead.
        _ => return Ok(()),
    };

    let mut renderer = render::StreamRenderer::new(
        render::ReasoningDisplayMode::from_config(&config.display.reasoning),
        render::ToolCallDisplayMode::from_config(&config.display.tool_calls),
        false,
        config.display.readable_tool_names,
        config.display.command_output_lines,
    );
    renderer.use_external_cursor_control();
    renderer.use_buffered_output();
    live.external_output_active = false;
    // Print the header straight into the scrollback (not the live frame):
    // it must survive the streaming render that follows.
    {
        live.suspend()?;
        let mut stdout = io::stdout();
        let header = if label.is_empty() {
            crate::i18n::text("⚙ background task finished", "⚙ 后台任务完成").to_string()
        } else {
            format!("⚙ {label}")
        };
        queue!(stdout, Print(format!("\x1b[2m{header}\x1b[0m\r\n\r\n")))?;
        stdout.flush()?;
        live.output_cursor = cursor_position_or(live.output_cursor);
        let output_cursor = live.output_cursor;
        live.resume_at(output_cursor)?;
    }
    renderer.start_waiting()?;
    live.apply_renderer_frame(&mut renderer)?;
    let mut raw = LiveRawMode::start()?;

    let mut spinner_tick = tokio::time::interval(Duration::from_millis(33));
    spinner_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    spinner_tick.tick().await;
    let mut input_tick = tokio::time::interval(Duration::from_millis(16));
    input_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    input_tick.tick().await;
    let mut follow_strip_tick: u32 = 0;

    'outer: loop {
        let recv = ipc::receive::<IpcFrame>(&mut stream);
        tokio::pin!(recv);
        let frame = loop {
            tokio::select! {
                biased;
                _ = input_tick.tick() => {
                    if terminal_hangup() {
                        let _ = send_ipc_command(paths, IpcCommand::Cancel { run_id: run_id.to_string() }).await;
                        std::process::exit(0);
                    }
                    if !event::poll(Duration::ZERO)? {
                        continue;
                    }
                    match live.editor.handle_event(event::read()?, paths, true)? {
                        LiveEditorAction::None => {}
                        LiveEditorAction::Redraw if !live.external_output_active => {
                            synchronized_terminal_update(CursorAfterUpdate::Preserve, || {
                                live.redraw()
                            })?
                        }
                        LiveEditorAction::Redraw | LiveEditorAction::ClearScreen => {}
                        LiveEditorAction::EmptySubmit => {}
                        LiveEditorAction::Submit(submission) => {
                            let Some(target_turn) = turn_id.as_deref() else {
                                continue;
                            };
                            if let Ok(prompt) = persist_remote_queued_submission(
                                paths,
                                run_id,
                                target_turn,
                                &submission,
                            )
                            .await
                            {
                                live.editor.record_history(&submission.content);
                                synchronized_terminal_update(
                                    CursorAfterUpdate::Preserve,
                                    || live.enqueue(prompt),
                                )?;
                            }
                        }
                        LiveEditorAction::Interrupt | LiveEditorAction::Exit => {
                            // Detach only: the wake turn keeps running.
                            break 'outer;
                        }
                    }
                }
                frame = &mut recv => break frame?,
                _ = spinner_tick.tick() => {
                    // SpinnerTick 经 live 路径冲刷 chunk 缓冲，流式输出靠它。
                    handle_live_agent_event(live, &mut renderer, AgentEvent::SpinnerTick)?;
                    // 状态条是 live tail 的一部分，附着期间同样要持续刷新。
                    follow_strip_tick = follow_strip_tick.wrapping_add(1);
                    if follow_strip_tick % 8 == 0 && !live.external_output_active {
                        if live.set_jobs(jobs_feed.current()) {
                            synchronized_terminal_update(CursorAfterUpdate::Preserve, || {
                                live.redraw()
                            })?;
                        } else {
                            live.tick_job_strip()?;
                        }
                    }
                }
            }
        };
        let Some(IpcFrame::Event { kind, data, .. }) = frame else {
            break;
        };
        match kind.as_str() {
            "turn.started" => {
                turn_id = Some(ipc_text(&data, "turn_id").to_string());
            }
            "assistant.delta" => handle_live_agent_event(
                live,
                &mut renderer,
                AgentEvent::Chunk(ChatStreamChunk {
                    kind: crate::llm::ChatStreamKind::Content,
                    text: ipc_text(&data, "delta").to_string(),
                }),
            )?,
            "reasoning.delta" => handle_live_agent_event(
                live,
                &mut renderer,
                AgentEvent::Chunk(ChatStreamChunk {
                    kind: crate::llm::ChatStreamKind::Reasoning,
                    text: ipc_text(&data, "delta").to_string(),
                }),
            )?,
            "reasoning.start" => handle_live_agent_event(
                live,
                &mut renderer,
                AgentEvent::ReasoningStart {
                    received_at: Instant::now(),
                },
            )?,
            "reasoning.reset" => handle_live_agent_event(
                live,
                &mut renderer,
                AgentEvent::ReasoningReset {
                    received_at: Instant::now(),
                },
            )?,
            "reasoning.part_start" => handle_live_agent_event(
                live,
                &mut renderer,
                AgentEvent::ReasoningPartStart {
                    received_at: Instant::now(),
                },
            )?,
            "reasoning.part_end" => handle_live_agent_event(
                live,
                &mut renderer,
                AgentEvent::ReasoningPartEnd {
                    received_at: Instant::now(),
                },
            )?,
            "reasoning.title" => handle_live_agent_event(
                live,
                &mut renderer,
                AgentEvent::ReasoningTitle(ipc_text(&data, "title").to_string()),
            )?,
            "tool.preparing" => handle_live_agent_event(
                live,
                &mut renderer,
                AgentEvent::ToolPreparing {
                    name: ipc_text(&data, "name").to_string(),
                },
            )?,
            "tool.started" => handle_live_agent_event(
                live,
                &mut renderer,
                AgentEvent::ToolCall {
                    call_id: ipc_text(&data, "tool_id").to_string(),
                    name: ipc_text(&data, "name").to_string(),
                    arguments: ipc_text(&data, "arguments").to_string(),
                },
            )?,
            "tool.progress" => handle_live_agent_event(
                live,
                &mut renderer,
                AgentEvent::ToolProgress {
                    call_id: ipc_text(&data, "tool_id").to_string(),
                    name: ipc_text(&data, "name").to_string(),
                    message: ipc_text(&data, "message").to_string(),
                },
            )?,
            "tool.output" => handle_live_agent_event(
                live,
                &mut renderer,
                AgentEvent::CommandOutput {
                    call_id: ipc_text(&data, "tool_id").to_string(),
                    name: ipc_text(&data, "name").to_string(),
                    stream: if ipc_text(&data, "stream") == "stderr" {
                        tools::CommandOutputStream::Stderr
                    } else {
                        tools::CommandOutputStream::Stdout
                    },
                    chunk: ipc_text(&data, "output").as_bytes().to_vec(),
                },
            )?,
            "tool.finished" => handle_live_agent_event(
                live,
                &mut renderer,
                AgentEvent::ToolResult {
                    call_id: ipc_text(&data, "tool_id").to_string(),
                    name: ipc_text(&data, "name").to_string(),
                    ok: data
                        .get("ok")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false),
                    output: ipc_text(&data, "output").to_string(),
                },
            )?,
            "queue.consumed" => {
                let prompt_ids: Vec<String> = data
                    .get("prompt_ids")
                    .and_then(serde_json::Value::as_array)
                    .map(|values| {
                        values
                            .iter()
                            .filter_map(|value| value.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                let consumed_mode = match ipc_text(&data, "mode") {
                    "dev" => AgentMode::Dev,
                    _ => AgentMode::Normal,
                };
                renderer.prepare_for_external_output()?;
                live.apply_renderer_frame(&mut renderer)?;
                synchronized_terminal_update(CursorAfterUpdate::Preserve, || {
                    live.suspend()?;
                    live.consume_queued(&prompt_ids, consumed_mode)
                })?;
            }
            "generation.superseded" => handle_live_agent_event(
                live,
                &mut renderer,
                AgentEvent::ReasoningReset {
                    received_at: Instant::now(),
                },
            )?,
            "run.completed" | "run.failed" | "run.cancelled" => {
                break;
            }
            _ => {}
        }
    }

    // Flush chunks still buffered when the terminal frame arrived — the
    // final content burst lands right before run.completed.
    live.flush_pending_chunks(&mut renderer)?;
    renderer.finish()?;
    live.apply_renderer_frame(&mut renderer)?;
    raw.handoff();
    live.raw_mode_handoff = true;
    // Suppress the duplicate DB report for a turn that was rendered live.
    if let Some(turn_id) = turn_id {
        jobs_shared.rendered_turns.lock().unwrap().insert(turn_id);
    }
    Ok(())
}

pub(crate) fn handle_live_agent_event(
    live: &mut LiveReplTail,
    renderer: &mut render::StreamRenderer,
    event: AgentEvent,
) -> Result<()> {
    let event = match event {
        AgentEvent::Chunk(chunk) => {
            live.queue_stream_chunk(chunk);
            return Ok(());
        }
        AgentEvent::RoundUsage { round, turn, .. } => {
            // 一次模型请求刚结束:立即刷新 footer 计量,不等整个回合。
            // prompt+completion 即该请求结束时的上下文实际占用。
            let context_tokens = round.prompt_tokens.saturating_add(round.completion_tokens);
            return live.refresh_round_usage(context_tokens, turn);
        }
        event => event,
    };
    if live.external_output_active && matches!(&event, AgentEvent::SpinnerTick) {
        return Ok(());
    }
    if matches!(&event, AgentEvent::SpinnerTick) {
        return live.tick_spinner(renderer);
    }
    match event {
        AgentEvent::PrepareForExternalOutput { ready } => {
            let result = (|| {
                live.flush_pending_chunks(renderer)?;
                renderer.prepare_for_external_output()?;
                live.apply_renderer_frame(renderer)?;
                synchronized_terminal_update(CursorAfterUpdate::Hidden, || live.suspend())?;
                live.external_output_active = true;
                Ok(())
            })();
            if result.is_ok() {
                let _ = ready.send(true);
            }
            result
        }
        AgentEvent::QueuedPromptsConsumed {
            prompt_ids, mode, ..
        } => {
            live.flush_pending_chunks(renderer)?;
            renderer.prepare_for_external_output()?;
            live.apply_renderer_frame(renderer)?;
            synchronized_terminal_update(CursorAfterUpdate::Preserve, || {
                live.suspend()?;
                live.consume_queued(&prompt_ids, mode)
            })
        }
        event => {
            let finishes_external_output =
                live.external_output_active && matches!(&event, AgentEvent::ToolResult { .. });
            if live.external_output_active && !finishes_external_output {
                handle_agent_event(renderer, event)?;
                return live.apply_renderer_frame(renderer);
            }
            let question = matches!(&event, AgentEvent::AskQuestion { .. });
            if question {
                live.flush_pending_chunks(renderer)?;
                renderer.prepare_for_external_output()?;
                live.apply_renderer_frame(renderer)?;
                synchronized_terminal_update(CursorAfterUpdate::Hidden, || live.suspend())?;
                handle_agent_event(renderer, event)?;
                // 问题面板只关闭 raw mode 与括号粘贴，键盘增强仍由外层 LiveRawMode 持有
                enable_live_raw_mode()?;
                execute!(io::stdout(), EnableBracketedPaste)?;
                synchronized_terminal_update(CursorAfterUpdate::Shown, || live.resume())?;
                return live.apply_renderer_frame(renderer);
            }
            live.flush_pending_chunks(renderer)?;
            handle_agent_event(renderer, event)?;
            live.apply_renderer_frame(renderer)?;
            if finishes_external_output {
                live.external_output_active = false;
                synchronized_terminal_update(CursorAfterUpdate::Shown, || live.resume())?;
            }
            Ok(())
        }
    }
}

pub(crate) async fn run_live_agent_turn(
    live: &mut LiveReplTail,
    paths: &GQYPaths,
    state: &StateStore,
    agent: &mut Agent,
    input: LiveAgentInput<'_>,
    control: &AgentTurnControl,
    renderer: &mut render::StreamRenderer,
) -> Result<Option<crate::llm::ChatResult>> {
    renderer.use_external_cursor_control();
    renderer.use_buffered_output();
    let mut raw = if std::mem::take(&mut live.raw_mode_handoff) {
        LiveRawMode::adopt()
    } else {
        LiveRawMode::start()?
    };
    live.external_output_active = false;
    if !live.rendered {
        live.resume_at(live.output_cursor)?;
    }
    renderer.start_waiting()?;
    live.apply_renderer_frame(renderer)?;

    let result = {
        let live_cell = std::cell::RefCell::new(&mut *live);
        let renderer_cell = std::cell::RefCell::new(&mut *renderer);
        let chat = agent.chat_stream_with_control(input.content, input.images, control, |event| {
            handle_live_agent_event(
                &mut live_cell.borrow_mut(),
                &mut renderer_cell.borrow_mut(),
                event,
            )
        });
        tokio::pin!(chat);
        let mut input_tick = tokio::time::interval(Duration::from_millis(16));
        input_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        input_tick.tick().await;
        loop {
            tokio::select! {
                biased;
                _ = input_tick.tick() => {
                    if terminal_hangup() {
                        std::process::exit(0);
                    }
                    if !event::poll(Duration::ZERO)? {
                        continue;
                    }
                    let event = event::read()?;
                    let mut live = live_cell.borrow_mut();
                    if matches!(
                        &event,
                        Event::Key(KeyEvent {
                            code: KeyCode::Enter,
                            kind,
                            ..
                        }) if *kind != KeyEventKind::Release
                    ) && live.editor.input.trim_start().starts_with('/')
                    {
                        if live.external_output_active {
                            continue;
                        }
                        let mut renderer = renderer_cell.borrow_mut();
                        live.flush_pending_chunks(&mut renderer)?;
                        renderer.write_system_message(t(
                            "REPL commands are available after the current reply finishes",
                            "当前回复结束后才能执行 REPL 命令",
                        ))?;
                        // write_system_message tears down the wait spinner;
                        // restart it so progress keeps rendering.
                        renderer.start_waiting()?;
                        live.apply_renderer_frame(&mut renderer)?;
                        continue;
                    }
                    let mode_before = live.mode();
                    match live.editor.handle_event(event, paths, true)? {
                        LiveEditorAction::None => {}
                        LiveEditorAction::Redraw if !live.external_output_active => {
                            synchronized_terminal_update(CursorAfterUpdate::Preserve, || {
                                live.redraw()
                            })?
                        }
                        LiveEditorAction::ClearScreen if !live.external_output_active => {
                            synchronized_terminal_update(CursorAfterUpdate::Preserve, || {
                                live.clear_screen()
                            })?
                        }
                        LiveEditorAction::Redraw | LiveEditorAction::ClearScreen => {}
                        LiveEditorAction::EmptySubmit => {}
                        LiveEditorAction::Submit(submission) => {
                            let prompt = persist_queued_submission(state, &submission)?;
                            live.editor.record_history(&submission.content);
                            if live.external_output_active {
                                live.append_queued(prompt);
                            } else {
                                synchronized_terminal_update(CursorAfterUpdate::Preserve, || {
                                    live.enqueue(prompt)
                                })?;
                            }
                        }
                        LiveEditorAction::Interrupt | LiveEditorAction::Exit => break Ok(None),
                    }
                    if live.mode() != mode_before {
                        control.set_mode(live.mode());
                    }
                },
                result = &mut chat => break result.map(Some),
            }
        }
    };

    if matches!(&result, Ok(None)) {
        live.discard_pending_chunks();
    }
    live.external_output_active = false;
    live.flush_pending_chunks(renderer)?;
    renderer.finish()?;
    live.apply_renderer_frame(renderer)?;
    raw.handoff();
    live.raw_mode_handoff = true;
    result
}

pub(crate) fn read_repl_input(
    paths: &GQYPaths,
    mode: AgentMode,
    prefill: Option<String>,
    history: &[String],
    footer: &ReplFooterStatus,
    show_shortcut_hint: bool,
) -> Result<
    Option<(
        AgentMode,
        String,
        Vec<Option<crate::clipboard::PastedImage>>,
    )>,
> {
    let mut stdout = io::stdout();
    let mut input = strip_terminal_control_sequences(&prefill.unwrap_or_default());
    let mut cursor = input.chars().count();
    let mut history_index = history.len();
    let mut history_clean_index: Option<usize> = None;
    let plain_prefix = "  ";
    let cursor_col = cursor_col_or(0);
    if cursor_col != 0 {
        writeln!(stdout)?;
        stdout.flush()?;
    }
    terminal::enable_raw_mode()?;
    spawn_hangup_watchdog();
    execute!(stdout, EnableBracketedPaste)?;
    let mut keyboard_enhancement = KeyboardEnhancementState::enable(&mut stdout);
    let mut input_row = cursor_row_or(0);
    let mut rendered_rows = 0u16;
    let mut is_pasted = false;
    let mut pasted_images: Vec<Option<crate::clipboard::PastedImage>> = Vec::new();
    let mut pasted_texts: Vec<Option<PastedText>> = Vec::new();
    // 1. 局部退出时统一恢复终端协议
    // 2. 避免多处 return 漏 Pop 键盘增强
    let restore_terminal = |stdout: &mut io::Stdout,
                            keyboard_enhancement: &mut KeyboardEnhancementState|
     -> Result<()> {
        execute!(stdout, DisableBracketedPaste)?;
        keyboard_enhancement.disable(stdout);
        terminal::disable_raw_mode()?;
        Ok(())
    };
    let render_repl_input = |stdout: &mut io::Stdout,
                             input_row: &mut u16,
                             rendered_rows: &mut u16,
                             mode: AgentMode,
                             input: &str,
                             cursor: usize,
                             is_pasted: bool| {
        render_repl_input_with_footer(
            stdout,
            input_row,
            rendered_rows,
            mode,
            input,
            cursor,
            is_pasted,
            footer,
            show_shortcut_hint,
        )
    };
    render_repl_input(
        &mut stdout,
        &mut input_row,
        &mut rendered_rows,
        mode,
        &input,
        cursor,
        is_pasted,
    )?;
    loop {
        match event::read()? {
            Event::Paste(text) => {
                insert_pasted_text_at_cursor(&mut input, &mut cursor, text, &mut pasted_texts);
                history_clean_index = None;
                is_pasted = true;
                render_repl_input(
                    &mut stdout,
                    &mut input_row,
                    &mut rendered_rows,
                    mode,
                    &input,
                    cursor,
                    is_pasted,
                )?;
            }
            Event::Key(KeyEvent {
                code, modifiers, ..
            }) => match code {
                KeyCode::Tab => {
                    if input.starts_with('/') {
                        if let Some(completed) = complete_repl_command(&input) {
                            input = completed.to_string();
                            cursor = input.chars().count();
                            history_clean_index = None;
                        }
                    } else {
                        // 会话模式创建时定死:Tab 切换已随闲聊模式一并删除。
                    }
                    is_pasted = false;
                    render_repl_input(
                        &mut stdout,
                        &mut input_row,
                        &mut rendered_rows,
                        mode,
                        &input,
                        cursor,
                        is_pasted,
                    )?;
                }
                KeyCode::Esc => {
                    input.clear();
                    cursor = 0;
                    history_clean_index = None;
                    is_pasted = false;
                    pasted_images.clear();
                    pasted_texts.clear();
                    render_repl_input(
                        &mut stdout,
                        &mut input_row,
                        &mut rendered_rows,
                        mode,
                        &input,
                        cursor,
                        is_pasted,
                    )?;
                }
                KeyCode::Left => {
                    if let Some((start, _)) = placeholder_at_cursor(&input, cursor) {
                        cursor = start;
                    } else {
                        cursor = cursor.saturating_sub(1);
                    }
                    render_repl_input(
                        &mut stdout,
                        &mut input_row,
                        &mut rendered_rows,
                        mode,
                        &input,
                        cursor,
                        is_pasted,
                    )?;
                }
                KeyCode::Right => {
                    if let Some((_, end)) = placeholder_at_cursor(&input, cursor) {
                        cursor = end;
                    } else {
                        cursor = (cursor + 1).min(input.chars().count());
                    }
                    render_repl_input(
                        &mut stdout,
                        &mut input_row,
                        &mut rendered_rows,
                        mode,
                        &input,
                        cursor,
                        is_pasted,
                    )?;
                }
                KeyCode::Home => {
                    cursor = 0;
                    render_repl_input(
                        &mut stdout,
                        &mut input_row,
                        &mut rendered_rows,
                        mode,
                        &input,
                        cursor,
                        is_pasted,
                    )?;
                }
                KeyCode::End => {
                    cursor = input.chars().count();
                    render_repl_input(
                        &mut stdout,
                        &mut input_row,
                        &mut rendered_rows,
                        mode,
                        &input,
                        cursor,
                        is_pasted,
                    )?;
                }
                KeyCode::Up => {
                    if !history.is_empty()
                        && repl_should_browse_history(&input, history, history_clean_index)
                    {
                        if input.is_empty() {
                            history_index = history.len();
                        }
                        history_index = history_index.saturating_sub(1);
                        input = history.get(history_index).cloned().unwrap_or_default();
                        cursor = input.chars().count();
                        history_clean_index = Some(history_index);
                        is_pasted = false;
                        pasted_images.clear();
                        pasted_texts.clear();
                    } else {
                        cursor = repl_move_cursor_vertical(plain_prefix, &input, cursor, -1);
                    }
                    render_repl_input(
                        &mut stdout,
                        &mut input_row,
                        &mut rendered_rows,
                        mode,
                        &input,
                        cursor,
                        is_pasted,
                    )?;
                }
                KeyCode::Down => {
                    if repl_history_is_clean(&input, history, history_clean_index) {
                        if history_index + 1 < history.len() {
                            history_index += 1;
                            input = history.get(history_index).cloned().unwrap_or_default();
                            cursor = input.chars().count();
                            history_clean_index = Some(history_index);
                        } else {
                            history_index = history.len();
                            input.clear();
                            cursor = 0;
                            history_clean_index = None;
                        }
                        is_pasted = false;
                        pasted_images.clear();
                        pasted_texts.clear();
                    } else {
                        cursor = repl_move_cursor_vertical(plain_prefix, &input, cursor, 1);
                    }
                    render_repl_input(
                        &mut stdout,
                        &mut input_row,
                        &mut rendered_rows,
                        mode,
                        &input,
                        cursor,
                        is_pasted,
                    )?;
                }
                KeyCode::Enter if modifiers.contains(KeyModifiers::SHIFT) => {
                    // Shift+Enter 与 Ctrl+J 相同：在光标处插入换行，不提交
                    insert_newline_at_cursor(&mut input, &mut cursor);
                    history_clean_index = None;
                    is_pasted = false;
                    render_repl_input(
                        &mut stdout,
                        &mut input_row,
                        &mut rendered_rows,
                        mode,
                        &input,
                        cursor,
                        is_pasted,
                    )?;
                }
                KeyCode::Enter => {
                    let submitted_echo = strip_terminal_control_sequences(&input);
                    input = expand_pasted_text_placeholders(&submitted_echo, &pasted_texts);
                    replace_repl_input_with_user_echo(
                        &mut stdout,
                        input_row,
                        rendered_rows,
                        mode,
                        &submitted_echo,
                    )?;
                    restore_terminal(&mut stdout, &mut keyboard_enhancement)?;
                    return Ok(Some((mode, input, pasted_images)));
                }
                KeyCode::Char('j') if modifiers.contains(KeyModifiers::CONTROL) => {
                    insert_newline_at_cursor(&mut input, &mut cursor);
                    history_clean_index = None;
                    is_pasted = false;
                    render_repl_input(
                        &mut stdout,
                        &mut input_row,
                        &mut rendered_rows,
                        mode,
                        &input,
                        cursor,
                        is_pasted,
                    )?;
                }
                KeyCode::Char('c')
                    if modifiers.contains(KeyModifiers::CONTROL)
                        && !modifiers.contains(KeyModifiers::SHIFT) =>
                {
                    if !input.is_empty() {
                        input.clear();
                        cursor = 0;
                        history_clean_index = None;
                        is_pasted = false;
                        pasted_images.clear();
                        pasted_texts.clear();
                        render_repl_input(
                            &mut stdout,
                            &mut input_row,
                            &mut rendered_rows,
                            mode,
                            &input,
                            cursor,
                            is_pasted,
                        )?;
                        continue;
                    }
                    move_after_repl_input(&mut stdout, input_row, rendered_rows)?;
                    restore_terminal(&mut stdout, &mut keyboard_enhancement)?;
                    return Ok(None);
                }
                KeyCode::Char('d')
                    if modifiers.contains(KeyModifiers::CONTROL) && input.is_empty() =>
                {
                    move_after_repl_input(&mut stdout, input_row, rendered_rows)?;
                    restore_terminal(&mut stdout, &mut keyboard_enhancement)?;
                    return Ok(None);
                }
                KeyCode::Char('l') if modifiers.contains(KeyModifiers::CONTROL) => {
                    queue!(stdout, Clear(ClearType::All), MoveTo(0, 0))?;
                    stdout.flush()?;
                    input_row = 0;
                    rendered_rows = 0;
                    render_repl_input(
                        &mut stdout,
                        &mut input_row,
                        &mut rendered_rows,
                        mode,
                        &input,
                        cursor,
                        is_pasted,
                    )?;
                }
                KeyCode::Char('w') if modifiers.contains(KeyModifiers::CONTROL) => {
                    if let Some((start, end)) = placeholder_before_or_at_cursor(&input, cursor) {
                        clear_placeholder_payload(
                            &input,
                            start,
                            end,
                            &mut pasted_images,
                            &mut pasted_texts,
                        );
                        remove_range_chars(&mut input, start, end);
                        cursor = start;
                    } else {
                        remove_word_before_cursor(&mut input, &mut cursor);
                    }
                    history_clean_index = None;
                    is_pasted = false;
                    render_repl_input(
                        &mut stdout,
                        &mut input_row,
                        &mut rendered_rows,
                        mode,
                        &input,
                        cursor,
                        is_pasted,
                    )?;
                }
                KeyCode::Backspace => {
                    if cursor > 0 {
                        if let Some((start, end)) = placeholder_before_or_at_cursor(&input, cursor)
                        {
                            clear_placeholder_payload(
                                &input,
                                start,
                                end,
                                &mut pasted_images,
                                &mut pasted_texts,
                            );
                            remove_range_chars(&mut input, start, end);
                            cursor = start;
                        } else {
                            remove_char_before_cursor(&mut input, &mut cursor);
                        }
                        history_clean_index = None;
                    }
                    is_pasted = false;
                    render_repl_input(
                        &mut stdout,
                        &mut input_row,
                        &mut rendered_rows,
                        mode,
                        &input,
                        cursor,
                        is_pasted,
                    )?;
                }
                KeyCode::Delete => {
                    if let Some((start, end)) = placeholder_after_or_at_cursor(&input, cursor) {
                        clear_placeholder_payload(
                            &input,
                            start,
                            end,
                            &mut pasted_images,
                            &mut pasted_texts,
                        );
                        remove_range_chars(&mut input, start, end);
                    } else {
                        remove_char_at_cursor(&mut input, cursor);
                    }
                    history_clean_index = None;
                    is_pasted = false;
                    render_repl_input(
                        &mut stdout,
                        &mut input_row,
                        &mut rendered_rows,
                        mode,
                        &input,
                        cursor,
                        is_pasted,
                    )?;
                }
                KeyCode::Char('c' | 'C')
                    if modifiers.contains(KeyModifiers::CONTROL)
                        && modifiers.contains(KeyModifiers::SHIFT) =>
                {
                    if let Some(selected) =
                        placeholder_text_near_cursor(&input, cursor, &pasted_texts)
                    {
                        let _ = crate::clipboard::write_clipboard_text(&selected)?;
                    }
                }
                KeyCode::Char('v') if modifiers.contains(KeyModifiers::CONTROL) => {
                    match crate::clipboard::read_clipboard() {
                        Ok(crate::clipboard::ClipboardContent::Image(img)) => {
                            let index = pasted_images.len() + 1;
                            let placeholder = match img.write_temp_file(&paths.cache_dir, index) {
                                Ok(path) => {
                                    let filename = path
                                        .file_name()
                                        .and_then(|n| n.to_str())
                                        .unwrap_or("image");
                                    format!("[Image {}: {}]", index, filename)
                                }
                                Err(_) => format!("[Image {}]", index),
                            };
                            insert_str_at_cursor(&mut input, &mut cursor, &placeholder);
                            history_clean_index = None;
                            pasted_images.push(Some(crate::clipboard::PastedImage::Binary(img)));
                            is_pasted = false;
                            render_repl_input(
                                &mut stdout,
                                &mut input_row,
                                &mut rendered_rows,
                                mode,
                                &input,
                                cursor,
                                is_pasted,
                            )?;
                        }
                        Ok(crate::clipboard::ClipboardContent::ImagePath(path)) => {
                            let index = pasted_images.len() + 1;
                            let filename = std::path::Path::new(&path)
                                .file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or("image");
                            let placeholder = format!("[Image {}: {}]", index, filename);
                            insert_str_at_cursor(&mut input, &mut cursor, &placeholder);
                            history_clean_index = None;
                            pasted_images.push(Some(crate::clipboard::PastedImage::Path(path)));
                            is_pasted = false;
                            render_repl_input(
                                &mut stdout,
                                &mut input_row,
                                &mut rendered_rows,
                                mode,
                                &input,
                                cursor,
                                is_pasted,
                            )?;
                        }
                        Ok(crate::clipboard::ClipboardContent::TextPath(path)) => {
                            insert_str_at_cursor(&mut input, &mut cursor, &path);
                            history_clean_index = None;
                            is_pasted = false;
                            render_repl_input(
                                &mut stdout,
                                &mut input_row,
                                &mut rendered_rows,
                                mode,
                                &input,
                                cursor,
                                is_pasted,
                            )?;
                        }
                        _ => {
                            if let Ok(Some(text)) = crate::clipboard::read_clipboard_text() {
                                insert_pasted_text_at_cursor(
                                    &mut input,
                                    &mut cursor,
                                    text,
                                    &mut pasted_texts,
                                );
                                history_clean_index = None;
                                is_pasted = true;
                                render_repl_input(
                                    &mut stdout,
                                    &mut input_row,
                                    &mut rendered_rows,
                                    mode,
                                    &input,
                                    cursor,
                                    is_pasted,
                                )?;
                            }
                        }
                    }
                }
                KeyCode::Char(ch) if !modifiers.contains(KeyModifiers::CONTROL) => {
                    if !is_disallowed_control_char(ch) {
                        if let Some((_, end)) = placeholder_at_cursor(&input, cursor) {
                            cursor = end;
                        }
                        insert_char_at_cursor(&mut input, &mut cursor, ch);
                        history_clean_index = None;
                    }
                    is_pasted = false;
                    render_repl_input(
                        &mut stdout,
                        &mut input_row,
                        &mut rendered_rows,
                        mode,
                        &input,
                        cursor,
                        is_pasted,
                    )?;
                }
                _ => {}
            },
            _ => {}
        }
    }
}

pub(crate) fn render_repl_input_with_footer(
    stdout: &mut io::Stdout,
    input_row: &mut u16,
    rendered_rows: &mut u16,
    mode: AgentMode,
    input: &str,
    cursor: usize,
    is_pasted: bool,
    footer: &ReplFooterStatus,
    show_shortcut_hint: bool,
) -> Result<()> {
    let suggestions = repl_command_suggestions(input);
    let lines = repl_input_lines(input);
    let prompt_prefix = input_prompt_bar(mode);
    let plain_prefix = "  ";
    let cols = terminal_cols();
    let display_lines = repl_visible_input_lines(
        &plain_prefix,
        &lines,
        REPL_MAX_VISIBLE_INPUT_ROWS,
        is_pasted,
    );
    let display_rows = repl_wrapped_input_rows_for_cols(&plain_prefix, &display_lines, cols);
    let display_rows: Vec<String> = display_rows
        .iter()
        .map(|line| colorize_repl_placeholders(line))
        .collect();
    let input_rows = display_rows.len().max(1).min(u16::MAX as usize) as u16;
    let show_hint = show_shortcut_hint && suggestions.is_empty();
    let current_rows = input_rows.saturating_add(if show_hint { 4 } else { 3 });
    let rows_to_clear = (*rendered_rows).max(current_rows).max(1);
    ensure_repl_space(stdout, input_row, rows_to_clear)?;
    for row_offset in 0..rows_to_clear {
        queue!(
            stdout,
            MoveTo(0, (*input_row).saturating_add(row_offset)),
            Clear(ClearType::CurrentLine)
        )?;
    }
    let mut row_offset = 0u16;
    queue!(stdout, MoveTo(0, *input_row), Print(&prompt_prefix))?;
    row_offset = row_offset.saturating_add(1);
    for line in &display_rows {
        let row = (*input_row).saturating_add(row_offset);
        queue!(stdout, MoveTo(0, row))?;
        queue!(stdout, Print(&prompt_prefix), Print(line))?;
        row_offset = row_offset.saturating_add(1);
    }
    queue!(
        stdout,
        MoveTo(0, (*input_row).saturating_add(row_offset)),
        Print(&prompt_prefix)
    )?;
    row_offset = row_offset.saturating_add(1);
    if !suggestions.is_empty() {
        let suggestion_width = cols.saturating_sub(visible_width(&prompt_prefix)).max(1);
        queue!(
            stdout,
            MoveTo(0, (*input_row).saturating_add(row_offset)),
            Print(&prompt_prefix),
            Print(format!(
                "\x1b[2m{}\x1b[0m",
                repl_command_suggestions_line(&suggestions, suggestion_width)
            ))
        )?;
    } else {
        queue!(
            stdout,
            MoveTo(0, (*input_row).saturating_add(row_offset)),
            Print(repl_footer_line(mode, footer, cols))
        )?;
        if show_hint {
            row_offset = row_offset.saturating_add(1);
            queue!(
                stdout,
                MoveTo(0, (*input_row).saturating_add(row_offset)),
                Print(repl_shortcut_hint_line(mode, cols))
            )?;
        }
    }
    let (cursor_col, cursor_row_offset) = if display_lines.len() == lines.len() {
        repl_cursor_position(&plain_prefix, input, cursor)
    } else {
        let last_line = display_lines.last().map(String::as_str).unwrap_or_default();
        let (col, _) = repl_cursor_position_for_line_for_cols(
            &plain_prefix,
            last_line,
            last_line.chars().count(),
            terminal_cols(),
        );
        (
            col,
            repl_prompt_rows(&plain_prefix, &display_lines).saturating_sub(1),
        )
    };
    queue!(
        stdout,
        MoveTo(
            cursor_col,
            (*input_row)
                .saturating_add(1)
                .saturating_add(cursor_row_offset)
        )
    )?;
    stdout.flush()?;
    *rendered_rows = current_rows;
    Ok(())
}

pub(crate) fn repl_visible_input_lines(
    prefix: &str,
    lines: &[String],
    max_rows: u16,
    is_pasted: bool,
) -> Vec<String> {
    let total_rows = repl_prompt_rows(prefix, lines);
    if total_rows <= max_rows || lines.len() <= 2 || !is_pasted {
        return lines.to_vec();
    }

    let omitted_lines = lines.len().saturating_sub(2);
    let omitted = if is_zh() {
        format!("... 已隐藏 {omitted_lines} 行粘贴内容 ...")
    } else {
        format!("... {omitted_lines} pasted lines hidden ...")
    };
    vec![lines[0].clone(), omitted, lines[lines.len() - 1].clone()]
}

pub(crate) fn ensure_repl_space(
    stdout: &mut io::Stdout,
    input_row: &mut u16,
    needed_rows: u16,
) -> Result<()> {
    let (_, term_rows) = terminal::size().unwrap_or((80, 24));
    let term_rows = term_rows.max(1);
    if (*input_row).saturating_add(needed_rows) < term_rows {
        return Ok(());
    }
    let overflow = (*input_row)
        .saturating_add(needed_rows)
        .saturating_sub(term_rows.saturating_sub(1));
    queue!(stdout, MoveTo(0, term_rows.saturating_sub(1)))?;
    for _ in 0..overflow {
        queue!(stdout, Print("\n"))?;
    }
    *input_row = (*input_row).saturating_sub(overflow);
    Ok(())
}

pub(crate) fn move_after_repl_input(
    stdout: &mut io::Stdout,
    input_row: u16,
    rendered_rows: u16,
) -> Result<()> {
    queue!(
        stdout,
        MoveTo(0, input_row.saturating_add(rendered_rows.max(1)))
    )?;
    stdout.flush()?;
    Ok(())
}

pub(crate) fn replace_repl_input_with_user_echo(
    stdout: &mut io::Stdout,
    input_row: u16,
    rendered_rows: u16,
    mode: AgentMode,
    input: &str,
) -> Result<()> {
    let cols = terminal_cols();
    let echo_lines = submitted_echo_lines(mode, input.trim_end(), cols);
    let echo_rows = echo_lines.len().min(u16::MAX as usize) as u16;
    let rows_to_clear = rendered_rows.max(echo_rows).max(1);
    for row_offset in 0..rows_to_clear {
        queue!(
            stdout,
            MoveTo(0, input_row.saturating_add(row_offset)),
            Clear(ClearType::CurrentLine)
        )?;
    }
    for (offset, line) in echo_lines.iter().enumerate() {
        queue!(
            stdout,
            MoveTo(
                0,
                input_row.saturating_add(offset.min(u16::MAX as usize) as u16)
            ),
            Print(line)
        )?;
    }
    queue!(
        stdout,
        MoveTo(0, input_row.saturating_add(echo_rows).saturating_add(1))
    )?;
    stdout.flush()?;
    Ok(())
}
