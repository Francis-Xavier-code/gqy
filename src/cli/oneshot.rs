//! oneshot — 自 src/cli.rs 拆分。

pub(crate) use super::*;

pub(crate) async fn append_stdin_if_piped(message: String) -> String {
    if io::stdin().is_terminal() {
        return message;
    }
    // The reader thread bounds itself with poll() deadlines instead of being
    // abandoned by an outer timeout: a thread stuck in a blocking read(0)
    // would make the tokio runtime hang forever on shutdown (the process
    // then never exits when stdin is a never-closing pipe).
    let read_result = tokio::task::spawn_blocking(|| -> std::io::Result<String> {
        use std::os::fd::AsRawFd;
        let stdin = std::io::stdin();
        let fd = stdin.as_raw_fd();
        let mut buf: Vec<u8> = Vec::new();
        let deadline =
            std::time::Instant::now() + std::time::Duration::from_secs(STDIN_TIMEOUT_SECS);
        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() || buf.len() >= STDIN_MAX_CHARS {
                break;
            }
            let mut pollfd = libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            };
            let timeout_ms = remaining.as_millis().min(i32::MAX as u128) as i32;
            let ready = unsafe { libc::poll(&mut pollfd, 1, timeout_ms) };
            if ready <= 0 {
                break;
            }
            let mut chunk = [0u8; 8192];
            let count = unsafe { libc::read(fd, chunk.as_mut_ptr().cast(), chunk.len()) };
            if count < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            if count == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..count as usize]);
        }
        buf.truncate(STDIN_MAX_CHARS);
        Ok(String::from_utf8_lossy(&buf).into_owned())
    })
    .await;

    let stdin_content = match read_result {
        Ok(Ok(content)) if !content.trim().is_empty() => content.trim().to_string(),
        _ => return message,
    };

    if message.is_empty() {
        stdin_content
    } else {
        format!("{message}\n\n---\n(stdin)\n{stdin_content}")
    }
}

/// Which session a one-shot CLI turn lands in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TurnSession {
    /// The terminal session — what shell-hook and `gqy new`/`session` drive.
    Current,
    /// An explicit `--session` target, resolved to a session id.
    Explicit(String),
    /// A throwaway session created for this turn and deleted right after, so a
    /// quick question never lands in a conversation the user cares about.
    Ephemeral,
}

/// Picks the session for `gqy ask` / a bare `gqy '<message>'`. Both default
/// to a throwaway session; `--session` and `--continue` opt back into a real
/// one (clap already rejects passing both).
pub(crate) async fn one_shot_session(
    paths: &GQYPaths,
    session_arg: Option<&str>,
    continue_session: bool,
) -> Result<TurnSession> {
    if let Some(arg) = session_arg {
        return Ok(TurnSession::Explicit(
            resolve_session_id_for_turn(paths, arg).await?,
        ));
    }
    if continue_session {
        return Ok(TurnSession::Current);
    }
    Ok(TurnSession::Ephemeral)
}

/// Named rather than left blank on purpose: a row that leaks past the sweep is
/// recognisable, and a non-empty name also skips the daemon's auto-title LLM
/// call (`maybe_auto_name_session`) for a session about to be deleted.
pub(crate) fn ephemeral_session_name() -> String {
    t("One-shot", "一次性对话").to_string()
}

pub(crate) async fn create_ephemeral_session(paths: &GQYPaths) -> Result<String> {
    let (_, data) = session_admin(
        paths,
        IpcCommand::CreateSession {
            name: Some(ephemeral_session_name()),
            switch: false,
            kind: Some(crate::state::ASK_SESSION_KIND.to_string()),
            mode: None,
        },
    )
    .await?;
    data.get("session")
        .and_then(|session| session.get("session_id"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| anyhow::anyhow!("GQY core returned an invalid response"))
}

/// Tears a throwaway session down. Background jobs go first so nothing is left
/// pointing at a session that is about to disappear. Best effort: a daemon
/// that has gone away leaves a row the startup sweep collects.
pub(crate) async fn discard_ephemeral_session(paths: &GQYPaths, session_id: &str) {
    let _ = send_ipc_admin(
        paths,
        IpcCommand::StopSessionJobs {
            session_id: session_id.to_string(),
        },
    )
    .await;
    let _ = send_ipc_admin(
        paths,
        IpcCommand::DeleteSession {
            target: crate::ipc::SessionRef::Id {
                id: session_id.to_string(),
            },
        },
    )
    .await;
}

/// Deletes the throwaway session however the direct-mode turn unwinds — error,
/// cancelled question, or early return.
pub(crate) struct EphemeralSessionGuard {
    state: StateStore,
    session_id: String,
}

impl Drop for EphemeralSessionGuard {
    fn drop(&mut self) {
        let _ = self.state.delete_session(&self.session_id);
    }
}

pub(crate) async fn run_chat_with_options(
    paths: &GQYPaths,
    message: String,
    show_reasoning: Option<bool>,
    plain: bool,
    mode: AgentMode,
    session: TurnSession,
) -> Result<()> {
    let message = append_stdin_if_piped(message).await;
    if message.is_empty() {
        return run_repl(paths, mode).await;
    }
    if !direct_mode_requested() {
        let session_override = match &session {
            TurnSession::Current => None,
            TurnSession::Explicit(session_id) => Some(session_id.clone()),
            TurnSession::Ephemeral => Some(create_ephemeral_session(paths).await?),
        };
        // Not `?`-through: the throwaway session has to be torn down on the
        // failure path too, otherwise a cancelled turn leaves it behind.
        let outcome = try_run_remote_chat(
            paths,
            None,
            &message,
            show_reasoning,
            plain,
            mode,
            &[],
            session_override.clone(),
            None,
        )
        .await;
        if session == TurnSession::Ephemeral {
            if let Some(session_id) = &session_override {
                discard_ephemeral_session(paths, session_id).await;
            }
        }
        match outcome {
            Ok(Some(_)) => return Ok(()),
            Ok(None) => {}
            Err(err) => return Err(err),
        }
    }
    let _core_lease = ipc::acquire_direct_core(paths)?;
    initialize_models_cache(paths);
    AppConfig::init_files(paths)?;
    let config = AppConfig::load_or_default(paths)?;
    let state = StateStore::new(paths)?;
    state.init_files()?;
    // Direct mode has no daemon to mint the throwaway session, so it makes its
    // own and pins the turn to it.
    let (state, _ephemeral_guard) = if session == TurnSession::Ephemeral {
        let record = state.create_session(
            &config.active_persona_scope(),
            &ephemeral_session_name(),
            crate::state::ASK_SESSION_KIND,
            None,
        )?;
        let guard = EphemeralSessionGuard {
            state: state.clone(),
            session_id: record.session_id.clone(),
        };
        (state.pinned(&record.session_id), Some(guard))
    } else {
        (state, None)
    };
    let memory_organizer = MemoryOrganizer::spawn()?;
    let memory_organizer_handle = memory_organizer.handle();
    memory_organizer_handle.wake(config.clone(), paths.clone(), state.clone());
    let client = OpenAiCompatibleClient::from_config(&config, paths)?;
    let registry =
        build_tool_registry(&config, paths, mode, crate::question_tui::available(plain))?;
    let reasoning_mode = if show_reasoning == Some(false) {
        render::ReasoningDisplayMode::Hidden
    } else {
        render::ReasoningDisplayMode::from_config(&config.display.reasoning)
    };
    let tool_call_mode = if plain {
        render::ToolCallDisplayMode::Hidden
    } else {
        render::ToolCallDisplayMode::from_config(&config.display.tool_calls)
    };
    let readable_tool_names = config.display.readable_tool_names;
    let command_output_lines = config.display.command_output_lines;
    let show_token_usage = config.display.show_token_usage && !plain;
    let show_mixed_model_endpoint = show_mixed_model_endpoint(&config, false);
    let display_config = config.clone();
    let mut agent = Agent::new(config, paths, state.clone(), client, registry, mode)?;
    agent.set_memory_organizer(memory_organizer_handle);
    agent.prepare_for_turn()?;
    let mut renderer = render::StreamRenderer::new(
        reasoning_mode,
        tool_call_mode,
        plain,
        readable_tool_names,
        command_output_lines,
    );
    renderer.start_waiting()?;
    let result = agent
        .chat_stream(&message, |event| handle_agent_event(&mut renderer, event))
        .await;
    renderer.finish()?;
    let result = match result {
        Ok(result) => result,
        Err(err) if crate::question::is_question_cancelled(&err) => return Ok(()),
        Err(err) => return Err(err),
    };
    print_mixed_model_endpoint(show_mixed_model_endpoint, &result, None);
    let mut cumulative_tokens = TurnTokens::from_usage(result.usage.as_ref());
    let context_tokens = agent.effective_context_tokens()?;
    print_chat_token_usage(
        &result,
        show_token_usage,
        context_tokens,
        result_context_window(&display_config, &result).or(agent.context_window()),
        cumulative_tokens,
    )?;
    let overflow_result = handle_post_turn_overflow(
        &agent,
        &mut renderer,
        context_tokens,
        show_token_usage,
        Some(&mut cumulative_tokens),
    )
    .await?;
    let updated_context_tokens = agent.effective_context_tokens()?;
    if overflow_result.is_none() && updated_context_tokens != context_tokens {
        print_chat_token_usage(
            &result,
            show_token_usage,
            updated_context_tokens,
            result_context_window(&display_config, &result).or(agent.context_window()),
            cumulative_tokens,
        )?;
    }
    Ok(())
}

pub(crate) struct RemoteTurnSummary {
    pub(crate) result: ChatResult,
    pub(crate) context_tokens: u64,
    pub(crate) context_window: Option<usize>,
    pub(crate) cumulative_tokens: TurnTokens,
}

/// Marker error for a remote turn interrupted by the user (Ctrl+C) or a
/// cancel from another client. The REPL catches it and returns to the prompt
/// instead of exiting; one-shot mode surfaces it as a normal error message.
#[derive(Debug)]
pub(crate) struct RemoteTurnCancelled;

impl std::fmt::Display for RemoteTurnCancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(t("cancelled", "已取消"))
    }
}

impl std::error::Error for RemoteTurnCancelled {}

/// 前端退出但回合继续:daemon 拥有回合,REPL 只是观众离席(验收:
/// dsh 语义,前端退出任务照跑)。
#[derive(Debug)]
pub(crate) struct RemoteTurnDetached;

impl std::fmt::Display for RemoteTurnDetached {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(t("detached", "已脱离"))
    }
}

impl std::error::Error for RemoteTurnDetached {}

pub(crate) fn is_remote_turn_detached(error: &anyhow::Error) -> bool {
    error.downcast_ref::<RemoteTurnDetached>().is_some()
}

pub(crate) fn is_remote_turn_cancelled(error: &anyhow::Error) -> bool {
    error.downcast_ref::<RemoteTurnCancelled>().is_some()
}

#[allow(clippy::too_many_arguments)]
/// 触发终端指纹。shellhook/单次 CLI 的 stdin 常被管道占用(--stdin 喂正文),
/// 所以按 stderr→stdout→stdin 找第一个 tty;父进程就是触发它的 shell。后台任务
/// 完成后 daemon 凭这份指纹校验「shell 还活着、仍在这个 tty、空闲在提示符」,
/// 才把跟进回复写回终端。检测不到(纯管道/重定向/cron)就不带。
pub(crate) fn detect_origin_tty() -> Option<crate::ipc::OriginTty> {
    let fd = [2, 1, 0]
        .into_iter()
        .find(|&fd| unsafe { libc::isatty(fd) } == 1)?;
    let path = std::fs::read_link(format!("/proc/self/fd/{fd}")).ok()?;
    if !path.starts_with("/dev/") {
        return None;
    }
    Some(crate::ipc::OriginTty {
        path,
        shell_pid: std::os::unix::process::parent_id(),
    })
}

#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_arguments)]
pub(crate) async fn try_run_remote_chat(
    paths: &GQYPaths,
    mut live: Option<&mut LiveReplTail>,
    message: &str,
    show_reasoning: Option<bool>,
    plain: bool,
    mode: AgentMode,
    images: &[Option<crate::clipboard::PastedImage>],
    session_override: Option<String>,
    jobs_feed: Option<&JobsFeed>,
) -> Result<Option<RemoteTurnSummary>> {
    let refreshed_paths = if direct_mode_requested() {
        None
    } else {
        // ensure_daemon also restarts a daemon left over from an older build.
        // Re-resolve paths because that shutdown may complete legacy layout migration.
        ipc::ensure_daemon(paths, None).await?;
        Some(GQYPaths::new()?)
    };
    let paths = refreshed_paths.as_ref().unwrap_or(paths);
    let mut stream = if direct_mode_requested() {
        match ipc::connect(&paths.ipc_socket()).await {
            Ok(stream) => stream,
            Err(_) => return Ok(None),
        }
    } else {
        ipc::connect(&paths.ipc_socket()).await?
    };
    // Turns run in parallel daemon-side: a running turn in this session does
    // not block a new one (the old multi-process placeholder semantics).
    let state_probe = StateStore::new(paths)?;
    let state_probe = session_override
        .as_deref()
        .map(|session_id| state_probe.pinned(session_id))
        .unwrap_or(state_probe);
    ipc::send(
        &mut stream,
        &IpcRequest::new(IpcCommand::StartTurn {
            content: message.to_string(),
            mode: ipc_mode_name(mode).to_string(),
            images: ipc_images(images),
            cwd: std::env::current_dir().ok(),
            session_id: session_override,
            // REPL 常驻连接,后台任务有自己的 FollowWake 通道;只有阅后即焚的
            // 单次/shellhook 触发才需要记下终端供 daemon 回写。
            origin_tty: if live.is_none() {
                detect_origin_tty()
            } else {
                None
            },
        }),
    )
    .await?;
    let Some(first) = ipc::receive::<IpcFrame>(&mut stream).await? else {
        bail!("GQY core closed the connection before accepting the turn");
    };
    let run_id = match first {
        IpcFrame::Accepted { run_id, .. } => run_id,
        IpcFrame::Error { message, .. } => bail!("{message}"),
        _ => bail!("GQY core returned an invalid response"),
    };
    let mut turn_id: Option<String> = None;

    let config = AppConfig::load_or_default(paths)?;
    let reasoning_mode = if show_reasoning == Some(false) {
        render::ReasoningDisplayMode::Hidden
    } else {
        render::ReasoningDisplayMode::from_config(&config.display.reasoning)
    };
    let tool_call_mode = if plain {
        render::ToolCallDisplayMode::Hidden
    } else {
        render::ToolCallDisplayMode::from_config(&config.display.tool_calls)
    };
    let mut renderer = render::StreamRenderer::new(
        reasoning_mode,
        tool_call_mode,
        plain,
        config.display.readable_tool_names,
        config.display.command_output_lines,
    );
    let queue_state = Some(state_probe);
    if let Some(live) = live.as_deref_mut() {
        renderer.use_external_cursor_control();
        renderer.use_buffered_output();
        live.external_output_active = false;
        if !live.rendered {
            live.resume_at(live.output_cursor)?;
        }
    }
    // Keep the terminal in raw mode during the turn so the editor stays
    // interactive: typed input is queued for the running turn, mirroring the
    // direct REPL's input pump.
    let mut raw = match live.as_deref_mut() {
        Some(live) => Some(if std::mem::take(&mut live.raw_mode_handoff) {
            LiveRawMode::adopt()
        } else {
            LiveRawMode::start()?
        }),
        None => None,
    };
    renderer.start_waiting()?;
    if let Some(live) = live.as_deref_mut() {
        live.apply_renderer_frame(&mut renderer)?;
    }
    let mut content = String::new();
    let mut reasoning = String::new();
    let mut spinner_tick = tokio::time::interval(Duration::from_millis(33));
    let mut job_strip_tick: u32 = 0;
    spinner_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    spinner_tick.tick().await;
    let mut input_tick = tokio::time::interval(Duration::from_millis(16));
    input_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    input_tick.tick().await;
    let completion = loop {
        // The receive future must survive across select iterations: dropping
        // it after it consumed the 4-byte length prefix (but before the
        // payload arrived) would desynchronize the frame stream.
        let recv = ipc::receive::<IpcFrame>(&mut stream);
        tokio::pin!(recv);
        let frame = loop {
            tokio::select! {
                biased;
                _ = input_tick.tick(), if live.is_some() => {
                    if terminal_hangup() {
                        // 终端没了但回合是 daemon 的:观众离席,戏照演。
                        std::process::exit(0);
                    }
                    if !event::poll(Duration::ZERO)? {
                        continue;
                    }
                    let event = event::read()?;
                    let Some(live_tail) = live.as_deref_mut() else {
                        continue;
                    };
                    if matches!(
                        &event,
                        Event::Key(KeyEvent {
                            code: KeyCode::Enter,
                            kind,
                            ..
                        }) if *kind != KeyEventKind::Release
                    ) && live_tail.editor.input.trim_start().starts_with('/')
                    {
                        if live_tail.external_output_active {
                            continue;
                        }
                        renderer.write_system_message(t(
                            "REPL commands are available after the current reply finishes",
                            "当前回复结束后才能执行 REPL 命令",
                        ))?;
                        // write_system_message tears down the wait spinner;
                        // restart it so progress keeps rendering.
                        renderer.start_waiting()?;
                        live_tail.apply_renderer_frame(&mut renderer)?;
                        continue;
                    }
                    match live_tail.editor.handle_event(event, paths, true)? {
                        LiveEditorAction::None => {}
                        LiveEditorAction::Redraw if !live_tail.external_output_active => {
                            synchronized_terminal_update(CursorAfterUpdate::Preserve, || {
                                live_tail.redraw()
                            })?
                        }
                        LiveEditorAction::ClearScreen if !live_tail.external_output_active => {
                            synchronized_terminal_update(CursorAfterUpdate::Preserve, || {
                                live_tail.clear_screen()
                            })?
                        }
                        LiveEditorAction::Redraw | LiveEditorAction::ClearScreen => {}
                        LiveEditorAction::EmptySubmit => {}
                        LiveEditorAction::Submit(submission) => {
                            let Some(target_turn_id) = turn_id.as_deref() else {
                                live_tail.editor.input = submission.display_content.clone();
                                live_tail.editor.cursor = live_tail.editor.input.chars().count();
                                renderer.write_system_message(t(
                                    "the reply is still starting; try sending the follow-up again",
                                    "当前回复仍在启动，请稍后重新发送追加消息",
                                ))?;
                                live_tail.apply_renderer_frame(&mut renderer)?;
                                continue;
                            };
                            match persist_remote_queued_submission(
                                paths,
                                &run_id,
                                target_turn_id,
                                &submission,
                            ).await {
                                Ok(prompt) => {
                                    live_tail.editor.record_history(&submission.content);
                                    if live_tail.external_output_active {
                                        live_tail.append_queued(prompt);
                                    } else {
                                        synchronized_terminal_update(
                                            CursorAfterUpdate::Preserve,
                                            || live_tail.enqueue(prompt),
                                        )?;
                                    }
                                }
                                Err(_) => {
                                    live_tail.editor.input =
                                        submission.display_content.clone();
                                    live_tail.editor.cursor =
                                        live_tail.editor.input.chars().count();
                                    renderer.write_system_message(t(
                                        "could not queue the message; the reply may have just finished",
                                        "无法排队消息；当前回复可能刚刚结束",
                                    ))?;
                                    live_tail.apply_renderer_frame(&mut renderer)?;
                                }
                            }
                        }
                        LiveEditorAction::Interrupt => {
                            let _ = send_ipc_command(
                                paths,
                                IpcCommand::Cancel { run_id: run_id.clone() },
                            )
                            .await;
                        }
                        LiveEditorAction::Exit => {
                            renderer.finish()?;
                            if let Some(live) = live.as_deref_mut() {
                                live.apply_renderer_frame(&mut renderer)?;
                            }
                            return Err(anyhow::Error::new(RemoteTurnDetached));
                        }
                    }
                },
                frame = &mut recv => break frame?,
                _ = spinner_tick.tick() => {
                    renderer.tick_spinner()?;
                    if let Some(live) = live.as_deref_mut() {
                        live.apply_renderer_frame(&mut renderer)?;
                        // The job strip is part of the live tail, so it keeps
                        // rendering during streaming; throttle to ~every 8th
                        // spinner frame.
#[allow(clippy::manual_is_multiple_of)]
                        if let Some(feed) = jobs_feed {
                            job_strip_tick = job_strip_tick.wrapping_add(1);
                            if job_strip_tick % 8 == 0 && !live.external_output_active {
                                if live.set_jobs(feed.current()) {
                                    synchronized_terminal_update(
                                        CursorAfterUpdate::Preserve,
                                        || live.redraw(),
                                    )?;
                                } else {
                                    live.tick_job_strip()?;
                                }
                            }
                        }
                    }
                }
                _ = tokio::signal::ctrl_c() => {
                    let _ = send_ipc_command(
                        paths,
                        IpcCommand::Cancel { run_id: run_id.clone() },
                    ).await;
                    renderer.finish()?;
                    if let Some(live) = live.as_deref_mut() {
                        live.apply_renderer_frame(&mut renderer)?;
                    }
                    return Err(anyhow::Error::new(RemoteTurnCancelled));
                }
            }
        };
        let Some(frame) = frame else {
            renderer.finish()?;
            if let Some(live) = live.as_deref_mut() {
                live.apply_renderer_frame(&mut renderer)?;
            }
            bail!("GQY core disconnected during the turn");
        };
        let IpcFrame::Event { kind, data, .. } = frame else {
            if let IpcFrame::Error { message, .. } = frame {
                renderer.finish()?;
                if let Some(live) = live.as_deref_mut() {
                    live.apply_renderer_frame(&mut renderer)?;
                }
                bail!("{message}");
            }
            continue;
        };
        match kind.as_str() {
            "turn.started" => {
                let id = ipc_text(&data, "turn_id");
                if !id.is_empty() {
                    turn_id = Some(id.to_string());
                }
            }
            "assistant.delta" => {
                let delta = ipc_text(&data, "delta");
                content.push_str(delta);
                handle_agent_event(
                    &mut renderer,
                    AgentEvent::Chunk(ChatStreamChunk {
                        kind: crate::llm::ChatStreamKind::Content,
                        text: delta.to_string(),
                    }),
                )?;
            }
            "reasoning.delta" => {
                let delta = ipc_text(&data, "delta");
                reasoning.push_str(delta);
                handle_agent_event(
                    &mut renderer,
                    AgentEvent::Chunk(ChatStreamChunk {
                        kind: crate::llm::ChatStreamKind::Reasoning,
                        text: delta.to_string(),
                    }),
                )?;
            }
            "reasoning.start" => handle_agent_event(
                &mut renderer,
                AgentEvent::ReasoningStart {
                    received_at: Instant::now(),
                },
            )?,
            "reasoning.reset" => {
                reasoning.clear();
                handle_agent_event(
                    &mut renderer,
                    AgentEvent::ReasoningReset {
                        received_at: Instant::now(),
                    },
                )?;
            }
            "reasoning.part_start" => handle_agent_event(
                &mut renderer,
                AgentEvent::ReasoningPartStart {
                    received_at: Instant::now(),
                },
            )?,
            "reasoning.part_end" => handle_agent_event(
                &mut renderer,
                AgentEvent::ReasoningPartEnd {
                    received_at: Instant::now(),
                },
            )?,
            "reasoning.title" => handle_agent_event(
                &mut renderer,
                AgentEvent::ReasoningTitle(ipc_text(&data, "title").to_string()),
            )?,
            "tool.preparing" => handle_agent_event(
                &mut renderer,
                AgentEvent::ToolPreparing {
                    name: ipc_text(&data, "name").to_string(),
                },
            )?,
            "tool.started" => handle_agent_event(
                &mut renderer,
                AgentEvent::ToolCall {
                    call_id: ipc_text(&data, "tool_id").to_string(),
                    name: ipc_text(&data, "name").to_string(),
                    arguments: ipc_text(&data, "arguments").to_string(),
                },
            )?,
            "tool.progress" => handle_agent_event(
                &mut renderer,
                AgentEvent::ToolProgress {
                    call_id: ipc_text(&data, "tool_id").to_string(),
                    name: ipc_text(&data, "name").to_string(),
                    message: ipc_text(&data, "message").to_string(),
                },
            )?,
            "tool.output" => handle_agent_event(
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
            "tool.finished" => {
                handle_agent_event(
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
                )?;
                if let Some(live) = live.as_deref_mut() {
                    if live.external_output_active {
                        live.external_output_active = false;
                        live.output_cursor = cursor_position_or(live.output_cursor);
                        live.resume_at(live.output_cursor)?;
                        live.apply_renderer_frame(&mut renderer)?;
                    }
                }
            }
            "tool.image" => {
                renderer.prepare_for_external_output()?;
                if let Some(live) = live.as_deref_mut() {
                    live.apply_renderer_frame(&mut renderer)?;
                    synchronized_terminal_update(CursorAfterUpdate::Hidden, || live.suspend())?;
                    live.external_output_active = true;
                }
                let state = queue_state
                    .as_ref()
                    .expect("queue state exists for a remote turn");
                let size = (ipc_text(&data, "name") == "show_meme")
                    .then(|| tools::memes::configured_meme_size(&config.plugins.memes))
                    .flatten();
                if let Err(error) = render_remote_tool_image(state, &data, size).await {
                    renderer.write_system_message(&format!(
                        "{}: {error}",
                        t("Could not display tool image", "工具图片显示失败")
                    ))?;
                }
            }
            "question.requested" => {
                renderer.prepare_for_external_output()?;
                if let Some(live) = live.as_deref_mut() {
                    live.apply_renderer_frame(&mut renderer)?;
                    synchronized_terminal_update(CursorAfterUpdate::Hidden, || live.suspend())?;
                }
                let request = crate::question::QuestionRequest {
                    questions: serde_json::from_value(
                        data.get("questions").cloned().unwrap_or_default(),
                    )?,
                };
                notify_if_unfocused(
                    &config,
                    live.as_deref().map(|live| live.editor.focused),
                    t("GQY is waiting on you", "GQY 在等你回答"),
                    request
                        .questions
                        .first()
                        .map(|prompt| prompt.question.as_str())
                        .unwrap_or_default(),
                );
                // A panel that cannot be shown is not a reason to abort the
                // turn: fall through to the same path a closed panel takes, so
                // the daemon gets an answer instead of the run dying on an
                // error the user cannot act on. The direct-mode handler has
                // always done this; this branch used to propagate instead.
                let asked = crate::question_tui::ask(&request).unwrap_or_else(|err| {
                    crate::question::QuestionResponse::Unavailable(err.to_string())
                });
                match asked {
                    crate::question::QuestionResponse::Answered(answers) => {
                        send_ipc_command(
                            paths,
                            IpcCommand::AnswerQuestion {
                                question_id: ipc_text(&data, "question_id").to_string(),
                                answers,
                            },
                        )
                        .await?;
                        renderer.start_waiting()?;
                    }
                    // Nobody could be shown the panel — no tty, or it failed to
                    // open. That is not the user calling the turn off, so the
                    // question is resolved and the turn carries on; the tool
                    // that asked finds out that nobody answered and can say so.
                    crate::question::QuestionResponse::Unavailable(_) => {
                        let _ = send_ipc_command(
                            paths,
                            IpcCommand::CloseQuestion {
                                question_id: ipc_text(&data, "question_id").to_string(),
                            },
                        )
                        .await;
                    }
                    // The terminal question UI maps its close gestures to
                    // Cancelled; that one really is "stop this turn".
                    crate::question::QuestionResponse::Closed
                    | crate::question::QuestionResponse::Cancelled => {
                        let _ = send_ipc_command(
                            paths,
                            IpcCommand::Cancel {
                                run_id: run_id.clone(),
                            },
                        )
                        .await;
                    }
                }
                if let Some(live) = live.as_deref_mut() {
                    live.external_output_active = false;
                    live.output_cursor = cursor_position_or(live.output_cursor);
                    live.resume_at(live.output_cursor)?;
                }
            }
            "queue.consumed" => {
                if let Some(live) = live.as_deref_mut() {
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
            }
            "queue.removed" => {
                if let Some(live) = live.as_deref_mut() {
                    if let Some(prompt_id) =
                        data.get("prompt_id").and_then(serde_json::Value::as_str)
                    {
                        synchronized_terminal_update(CursorAfterUpdate::Preserve, || {
                            live.drop_queued(&[prompt_id.to_string()])
                        })?;
                    }
                }
            }
            "generation.superseded" => {
                content.clear();
                reasoning.clear();
                handle_agent_event(
                    &mut renderer,
                    AgentEvent::ReasoningReset {
                        received_at: Instant::now(),
                    },
                )?;
            }
            "context.compact_start" => handle_agent_event(&mut renderer, AgentEvent::CompactStart)?,
            "context.compact_delta" => handle_agent_event(
                &mut renderer,
                AgentEvent::CompactChunk(ChatStreamChunk {
                    kind: crate::llm::ChatStreamKind::Content,
                    text: ipc_text(&data, "delta").to_string(),
                }),
            )?,
            "context.compact_end" => handle_agent_event(&mut renderer, AgentEvent::CompactEnd)?,
            "context.pop_start" => handle_agent_event(&mut renderer, AgentEvent::PopStart)?,
            "context.pop_end" => handle_agent_event(&mut renderer, AgentEvent::PopEnd)?,
            "context.notice" => handle_agent_event(
                &mut renderer,
                AgentEvent::Notice {
                    text: ipc_text(&data, "text").to_string(),
                },
            )?,
            "run.completed" => break data,
            "run.failed" => {
                renderer.finish()?;
                if let Some(live) = live.as_deref_mut() {
                    live.apply_renderer_frame(&mut renderer)?;
                }
                bail!("{}", ipc_text(&data, "message"));
            }
            "run.cancelled" => {
                renderer.finish()?;
                if let Some(live) = live.as_deref_mut() {
                    live.apply_renderer_frame(&mut renderer)?;
                }
                return Err(anyhow::Error::new(RemoteTurnCancelled));
            }
            _ => {}
        }
        if let Some(live) = live.as_deref_mut() {
            live.apply_renderer_frame(&mut renderer)?;
        }
    };
    renderer.finish()?;
    let focused = live.as_deref().map(|live| live.editor.focused);
    if let Some(live) = live {
        live.apply_renderer_frame(&mut renderer)?;
        if let Some(raw) = raw.as_mut() {
            raw.handoff();
            live.raw_mode_handoff = true;
        }
    }

    let result = ChatResult {
        content,
        reasoning: (!reasoning.is_empty()).then_some(reasoning),
        usage: completion
            .get("usage")
            .cloned()
            .filter(|value| !value.is_null())
            .map(serde_json::from_value::<Usage>)
            .transpose()?,
        usage_estimated: completion
            .get("usage_estimated")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        tool_calls: Vec::new(),
        provider_id: completion
            .get("provider_id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        model: completion
            .get("model")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        finish_reason: None,
        thinking_signature: None,
        last_request_usage: None,
        responses_continuation: None,
    };
    if config.notifications.on_turn_complete {
        notify_if_unfocused(
            &config,
            focused,
            t("GQY finished replying", "GQY 回复完成"),
            &result.content,
        );
    }
    print_mixed_model_endpoint(show_mixed_model_endpoint(&config, false), &result, None);
    let context_tokens = completion
        .get("context_tokens")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or_default();
    let context_window = completion
        .get("context_window")
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| usize::try_from(value).ok());
    let completion_u64 = |key: &str| {
        completion
            .get(key)
            .and_then(serde_json::Value::as_u64)
            .unwrap_or_default()
    };
    let cumulative_tokens = TurnTokens {
        total: completion_u64("cumulative_tokens"),
        prompt: completion_u64("cumulative_prompt_tokens"),
        cache_read: completion_u64("cumulative_cache_read_tokens"),
    };
    print_chat_token_usage(
        &result,
        config.display.show_token_usage && !plain,
        context_tokens,
        context_window,
        TurnTokens::from_usage(result.usage.as_ref()),
    )?;
    Ok(Some(RemoteTurnSummary {
        result,
        context_tokens,
        context_window,
        cumulative_tokens,
    }))
}
