//! sessions_b — 自 src/web.rs 拆分。

pub(crate) use super::*;

pub(crate) async fn handle_ipc_connection(
    state: DaemonState,
    mut stream: tokio::net::UnixStream,
) -> Result<()> {
    let Some(request) = tokio::time::timeout(
        Duration::from_secs(5),
        ipc::receive::<IpcRequest>(&mut stream),
    )
    .await
    .context("timed out waiting for a GQY IPC request")??
    else {
        return Ok(());
    };
    if request.version != ipc::PROTOCOL_VERSION
        && !matches!(&request.command, IpcCommand::Ping | IpcCommand::Shutdown)
    {
        ipc::send(
            &mut stream,
            &IpcFrame::error(format!(
                "unsupported IPC protocol version {}; expected {}",
                request.version,
                ipc::PROTOCOL_VERSION
            )),
        )
        .await?;
        return Ok(());
    }

    match request.command {
        IpcCommand::Ping => {
            ipc::send(
                &mut stream,
                &IpcFrame::Ready {
                    pid: std::process::id(),
                    web_port: state.web_port,
                    web_public: state.web_public,
                    web_bind: Some(state.web_bind),
                    build_id: ipc::BUILD_ID.to_string(),
                },
            )
            .await?;
        }
        IpcCommand::Shutdown => {
            ipc::send(&mut stream, &IpcFrame::Ack).await?;
            let _ = state.shutdown_tx.send(());
        }
        IpcCommand::JobsOverview => {
            let wake_runs = {
                let manager = state.manager.lock().unwrap();
                manager
                    .active_runs
                    .iter()
                    .filter(|(_, info)| info.job_wake)
                    .map(|(run_id, info)| {
                        json!({
                            "run_id": run_id,
                            "session_id": &*info.session_id,
                            "label": info.job_wake_label,
                        })
                    })
                    .collect::<Vec<_>>()
            };
            ipc::send(
                &mut stream,
                &IpcFrame::AdminResult {
                    state: session_state(&state.manager, &state.state_store)?,
                    data: json!({ "jobs": tools::jobs::overview(), "wake_runs": wake_runs }),
                },
            )
            .await?;
        }
        IpcCommand::FollowRun { run_id } => {
            follow_run(&state, &mut stream, run_id).await?;
        }
        IpcCommand::StopSessionJobs { session_id } => {
            let stopped = tools::jobs::stop_session_jobs(&session_id).await;
            state
                .events
                .publish("job.acknowledged", json!({ "session_id": session_id }));
            ipc::send(
                &mut stream,
                &IpcFrame::AdminResult {
                    state: session_state(&state.manager, &state.state_store)?,
                    data: json!({ "stopped": stopped }),
                },
            )
            .await?;
        }
        IpcCommand::GetStatus => {
            let qq_enabled = state.manager.lock().unwrap().config.platforms.qq.enabled;
            let qq_port = state.platforms.qq_listener.active_port();
            let connected_accounts = state.platforms.onebot.lock().unwrap().connected_accounts();
            ipc::send(
                &mut stream,
                &IpcFrame::AdminResult {
                    state: session_state(&state.manager, &state.state_store)?,
                    data: json!({
                        "runtime": {
                            "turn_engine": state.turn_engine.label(),
                        },
                        "platforms": {
                            "qq": {
                                "enabled": qq_enabled,
                                "listen_port": qq_port,
                                "connected_accounts": connected_accounts,
                            }
                        }
                    }),
                },
            )
            .await?;
        }
        IpcCommand::GetReplSession { mode } => {
            let dev = mode.as_deref() == Some("dev");
            let persona = if dev {
                crate::state::DEV_PERSONA.to_string()
            } else {
                active_persona_scope(&state)
            };
            let store = &state.state_store;
            // A stale pointer (session deleted or archived elsewhere) must not
            // strand the REPL: fall back to the terminal session and heal the
            // pointer so the next start is a plain read. Dev 无「终端会话」
            // 可退,指针缺失时自举一个新的 dev 会话。
            let session_id = match store.repl_session(&persona).ok().flatten() {
                Some(session_id) => session_id,
                None if dev => {
                    store
                        .create_session(
                            crate::state::DEV_PERSONA,
                            "",
                            crate::state::USER_SESSION_KIND,
                            None,
                        )
                        .map_err(|error| anyhow::anyhow!(safe_error_message(&error)))?
                        .session_id
                }
                None => store.session_id().to_string(),
            };
            let target = ipc::SessionRef::Id { id: session_id };
            let session_id = match resolve_available_local_session_ref(&state, &target) {
                Ok(record) => record.session_id,
                Err(_) if dev => {
                    store
                        .create_session(
                            crate::state::DEV_PERSONA,
                            "",
                            crate::state::USER_SESSION_KIND,
                            None,
                        )
                        .map_err(|error| anyhow::anyhow!(safe_error_message(&error)))?
                        .session_id
                }
                Err(_) => store.session_id().to_string(),
            };
            let _ = store.set_repl_session(&persona, &session_id);
            ipc::send(
                &mut stream,
                &IpcFrame::AdminResult {
                    state: session_state_for(&state, &session_id)?,
                    data: json!({}),
                },
            )
            .await?;
        }
        IpcCommand::GetSessionState { target } => {
            let record = match resolve_available_local_session_ref(&state, &target) {
                Ok(record) => record,
                Err(message) => {
                    ipc::send(&mut stream, &IpcFrame::error(message)).await?;
                    return Ok(());
                }
            };
            ipc::send(
                &mut stream,
                &IpcFrame::AdminResult {
                    state: session_state_for(&state, &record.session_id)?,
                    data: json!({}),
                },
            )
            .await?;
        }
        IpcCommand::ReloadConfig => {
            let current_config = state.manager.lock().unwrap().config.clone();
            let next_config = match AppConfig::load_or_default(&state.paths) {
                Ok(config) => config,
                Err(error) => {
                    ipc::send(
                        &mut stream,
                        &IpcFrame::error(format!(
                            "invalid configuration: {}",
                            safe_error_message(error)
                        )),
                    )
                    .await?;
                    return Ok(());
                }
            };
            let prompts = match read_prompt_documents(&next_config, &state.paths) {
                Ok(prompts) => prompts,
                Err(error) => {
                    ipc::send(&mut stream, &IpcFrame::error(safe_error_message(error))).await?;
                    return Ok(());
                }
            };
            let qq_listener = match state
                .platforms
                .qq_listener
                .prepare(
                    &state,
                    Some(&current_config.platforms.qq),
                    &next_config.platforms.qq,
                )
                .await
            {
                Ok(listener) => listener,
                Err(error) => {
                    ipc::send(
                        &mut stream,
                        &IpcFrame::error(format!(
                            "Tencent QQ listener configuration failed: {}",
                            safe_error_message(error)
                        )),
                    )
                    .await?;
                    return Ok(());
                }
            };
            // Light reservation: reloading is allowed while turns run. Running
            // turns keep the config snapshot they started with; new turns pick
            // up the reloaded config. Persona layout changes interrupt running
            // turns inside the ApplyConfig handler instead of failing here.
            if let Err(error) = reserve_admin_light(&state.manager) {
                ipc::send(
                    &mut stream,
                    &IpcFrame::coded_error(ipc::ErrorCode::Busy, error.message),
                )
                .await?;
                return Ok(());
            }
            let (reply, receiver) = oneshot::channel();
            if state
                .actor_tx
                .send(ActorCommand::ApplyConfig {
                    config: Box::new(next_config),
                    prompts,
                    reset_conversation: false,
                    reply,
                })
                .is_err()
            {
                release_admin(&state.manager);
                ipc::send(
                    &mut stream,
                    &IpcFrame::error("GQY core worker is unavailable"),
                )
                .await?;
                return Ok(());
            }
            match receiver.await {
                Ok(Ok(())) => {
                    qq_listener.commit();
                    match session_state(&state.manager, &state.state_store) {
                        Ok(session) => {
                            ipc::send(
                                &mut stream,
                                &IpcFrame::AdminResult {
                                    state: session,
                                    data: json!({}),
                                },
                            )
                            .await?
                        }
                        Err(error) => {
                            ipc::send(&mut stream, &IpcFrame::error(safe_error_message(error)))
                                .await?
                        }
                    }
                }
                Ok(Err(AdminFailure::Invalid(message) | AdminFailure::Internal(message))) => {
                    ipc::send(&mut stream, &IpcFrame::error(message)).await?
                }
                Err(_) => {
                    release_admin(&state.manager);
                    ipc::send(
                        &mut stream,
                        &IpcFrame::error("GQY core stopped while reloading configuration"),
                    )
                    .await?
                }
            }
        }
        IpcCommand::ResetConversation { target } => {
            let target_record = match resolve_available_local_session_ref(&state, &target) {
                Ok(record) => record,
                Err(message) => {
                    ipc::send(&mut stream, &IpcFrame::error(message)).await?;
                    return Ok(());
                }
            };
            let session_id: Arc<str> = target_record.session_id.into();
            reserve_admin_for_session(&state.manager, &session_id)
                .map_err(|error| anyhow::anyhow!(error.message))?;
            let (reply, receiver) = oneshot::channel();
            if state
                .actor_tx
                .send(ActorCommand::ResetConversation {
                    session_id: session_id.clone(),
                    reply,
                })
                .is_err()
            {
                release_admin(&state.manager);
                anyhow::bail!("GQY core worker is unavailable");
            }
            match receiver.await {
                Ok(Ok(())) => {
                    ipc::send(
                        &mut stream,
                        &IpcFrame::AdminResult {
                            state: session_state_for(&state, &session_id)?,
                            data: json!({}),
                        },
                    )
                    .await?
                }
                Ok(Err(AdminFailure::Invalid(message) | AdminFailure::Internal(message))) => {
                    ipc::send(&mut stream, &IpcFrame::error(message)).await?
                }
                Err(_) => {
                    release_admin(&state.manager);
                    anyhow::bail!("GQY core stopped while resetting the conversation");
                }
            }
        }
        IpcCommand::WipePersona => {
            let config = state.manager.lock().unwrap().config.clone();
            let current = state.state_store.session_id().to_string();
            match reset_platform_persona_state(&state, &config).await {
                Ok(sessions) => {
                    ipc::send(
                        &mut stream,
                        &IpcFrame::AdminResult {
                            state: session_state_for(&state, &current)?,
                            data: json!({ "sessions": sessions }),
                        },
                    )
                    .await?;
                }
                Err(PlatformPersonaResetError::Busy) => {
                    ipc::send(
                        &mut stream,
                        &IpcFrame::coded_error(ipc::ErrorCode::Busy, ipc::ADMIN_BUSY_MESSAGE),
                    )
                    .await?;
                }
                Err(PlatformPersonaResetError::Unavailable) => {
                    anyhow::bail!("GQY core worker is unavailable");
                }
                Err(PlatformPersonaResetError::Internal(message)) => {
                    ipc::send(&mut stream, &IpcFrame::error(message)).await?;
                }
            }
        }
        IpcCommand::Undo { target } => {
            let record = match resolve_available_local_session_ref(&state, &target) {
                Ok(record) => record,
                Err(message) => {
                    ipc::send(&mut stream, &IpcFrame::error(message)).await?;
                    return Ok(());
                }
            };
            let session_id: Arc<str> = record.session_id.into();
            reserve_admin_for_session(&state.manager, &session_id)
                .map_err(|error| anyhow::anyhow!(error.message))?;
            let (reply, receiver) = oneshot::channel();
            if state
                .actor_tx
                .send(ActorCommand::Undo {
                    session_id: session_id.clone(),
                    reply,
                })
                .is_err()
            {
                release_admin(&state.manager);
                anyhow::bail!("GQY core worker is unavailable");
            }
            match receiver.await {
                Ok(Ok(data)) => {
                    ipc::send(
                        &mut stream,
                        &IpcFrame::AdminResult {
                            state: session_state_for(&state, &session_id)?,
                            data,
                        },
                    )
                    .await?
                }
                Ok(Err(AdminFailure::Invalid(message) | AdminFailure::Internal(message))) => {
                    ipc::send(&mut stream, &IpcFrame::error(message)).await?
                }
                Err(_) => {
                    release_admin(&state.manager);
                    anyhow::bail!("GQY core stopped while undoing the conversation");
                }
            }
        }
        IpcCommand::Pop { target, turn_ids } => {
            let record = match resolve_available_local_session_ref(&state, &target) {
                Ok(record) => record,
                Err(message) => {
                    ipc::send(&mut stream, &IpcFrame::error(message)).await?;
                    return Ok(());
                }
            };
            let session_id: Arc<str> = record.session_id.into();
            reserve_admin_for_session(&state.manager, &session_id)
                .map_err(|error| anyhow::anyhow!(error.message))?;
            let (reply, receiver) = oneshot::channel();
            if state
                .actor_tx
                .send(ActorCommand::Pop {
                    session_id: session_id.clone(),
                    turn_ids,
                    reply,
                })
                .is_err()
            {
                release_admin(&state.manager);
                anyhow::bail!("GQY core worker is unavailable");
            }
            match receiver.await {
                Ok(Ok(data)) => {
                    ipc::send(
                        &mut stream,
                        &IpcFrame::AdminResult {
                            state: session_state_for(&state, &session_id)?,
                            data,
                        },
                    )
                    .await?
                }
                Ok(Err(AdminFailure::Invalid(message) | AdminFailure::Internal(message))) => {
                    ipc::send(&mut stream, &IpcFrame::error(message)).await?
                }
                Err(_) => {
                    release_admin(&state.manager);
                    anyhow::bail!("GQY core stopped while popping the conversation");
                }
            }
        }
        IpcCommand::Compact { target } => {
            let record = match resolve_available_local_session_ref(&state, &target) {
                Ok(record) => record,
                Err(message) => {
                    ipc::send(&mut stream, &IpcFrame::error(message)).await?;
                    return Ok(());
                }
            };
            let session_id: Arc<str> = record.session_id.into();
            reserve_admin_for_session(&state.manager, &session_id)
                .map_err(|error| anyhow::anyhow!(error.message))?;
            let (reply, receiver) = oneshot::channel();
            if state
                .actor_tx
                .send(ActorCommand::Compact {
                    session_id: session_id.clone(),
                    reply,
                })
                .is_err()
            {
                release_admin(&state.manager);
                anyhow::bail!("GQY core worker is unavailable");
            }
            match receiver.await {
                Ok(Ok(data)) => {
                    ipc::send(
                        &mut stream,
                        &IpcFrame::AdminResult {
                            state: session_state_for(&state, &session_id)?,
                            data,
                        },
                    )
                    .await?
                }
                Ok(Err(AdminFailure::Invalid(message) | AdminFailure::Internal(message))) => {
                    ipc::send(&mut stream, &IpcFrame::error(message)).await?
                }
                Err(_) => {
                    release_admin(&state.manager);
                    anyhow::bail!("GQY core stopped while compacting the conversation");
                }
            }
        }
        IpcCommand::StartTurn {
            content,
            mode,
            images,
            cwd,
            session_id,
            origin_tty,
        } => {
            handle_ipc_turn(
                &state,
                &mut stream,
                content,
                mode,
                images,
                cwd,
                session_id,
                origin_tty,
            )
            .await?;
        }
        IpcCommand::QueueTurnUpdate {
            run_id,
            turn_id,
            content,
            display_content,
            images,
            supersede,
        } => {
            let attachments = images
                .into_iter()
                .flatten()
                .map(|image| match image {
                    ImageAttachment::Binary { mime, data } => {
                        crate::state::QueuedPromptAttachment::Binary {
                            mime,
                            data_base64: base64::engine::general_purpose::STANDARD.encode(data),
                        }
                    }
                    ImageAttachment::Path { path } => {
                        crate::state::QueuedPromptAttachment::Path { path }
                    }
                })
                .collect();
            match enqueue_turn_update(
                &state,
                TurnUpdateRequest {
                    run_id,
                    turn_id,
                    session_id: None,
                    audience: PromptAudience::Owner,
                    content,
                    display_content,
                    attachments,
                    uploaded_attachment_ids: Vec::new(),
                    mode: if supersede {
                        TurnUpdateMode::Supersede
                    } else {
                        TurnUpdateMode::Followup
                    },
                },
            ) {
                Ok(receipt) => {
                    ipc::send(
                        &mut stream,
                        &IpcFrame::TurnUpdateAccepted {
                            run_id: receipt.run_id,
                            turn_id: receipt.turn_id,
                            prompt_id: receipt.prompt.prompt_id,
                            seq: receipt.prompt.seq,
                            submitted_at: receipt.prompt.submitted_at,
                        },
                    )
                    .await?;
                }
                Err(error) => {
                    ipc::send(&mut stream, &IpcFrame::error(error.to_string())).await?;
                }
            }
        }
        IpcCommand::Cancel { run_id } => {
            let cancelled = {
                let manager = state.manager.lock().unwrap();
                manager.active_runs.get(&run_id).map(|run| {
                    run.request_cancel();
                })
            };
            if cancelled.is_some() {
                ipc::send(&mut stream, &IpcFrame::Ack).await?;
            } else {
                ipc::send(&mut stream, &IpcFrame::error("active run not found")).await?;
            }
        }
        IpcCommand::CloseQuestion { question_id } => {
            let _ = state.questions.close(&question_id, |run_id| {
                state.events.publish(
                    "question.closed",
                    json!({
                        "run_id": run_id,
                        "question_id": question_id,
                    }),
                );
            });
            ipc::send(&mut stream, &IpcFrame::Ack).await?
        }
        IpcCommand::AnswerQuestion {
            question_id,
            answers,
        } => match state
            .questions
            .answer(&question_id, answers, |run_id, answers| {
                state.events.publish(
                    "question.answered",
                    json!({
                        "run_id": run_id,
                        "question_id": question_id,
                        "answers": answers,
                    }),
                );
            }) {
            Ok(()) => ipc::send(&mut stream, &IpcFrame::Ack).await?,
            Err(error) => {
                let message = match error {
                    AnswerFailure::NotFound => "pending question not found".to_string(),
                    AnswerFailure::Invalid(message) => message,
                    AnswerFailure::Gone => "pending question is no longer active".to_string(),
                };
                ipc::send(&mut stream, &IpcFrame::error(message)).await?;
            }
        },
        session_command => match handle_session_command(&state, session_command).await {
            Ok(data) => {
                ipc::send(
                    &mut stream,
                    &IpcFrame::AdminResult {
                        state: session_state(&state.manager, &state.state_store)?,
                        data,
                    },
                )
                .await?
            }
            Err(message) => ipc::send(&mut stream, &IpcFrame::error(message)).await?,
        },
    }
    Ok(())
}

/// Handles the session-management IPC commands. Returns the `AdminResult`
/// payload on success or a user-facing error message.
pub(crate) async fn handle_session_command(
    state: &DaemonState,
    command: IpcCommand,
) -> std::result::Result<Value, String> {
    let store = &state.state_store;
    let persona = active_persona_scope(state);
    match command {
        IpcCommand::SetRequestLogging { enabled } => {
            // 此刻可能还没构造过任何 LLM 客户端,目录未必已安装——就地
            // 安装,免得 current_file 返回 None、监控端拿兜底路径扑空。
            crate::llm::request_log::install_dir(state.paths.logs_dir());
            crate::llm::request_log::set_enabled(enabled);
            Ok(json!({
                "enabled": enabled,
                "file": crate::llm::request_log::current_file()
                    .map(|path| path.display().to_string()),
            }))
        }
        IpcCommand::ResetMemory { mode } => {
            // dev 记忆挂保留人格名下,与 Agent 构造同一把 dev_scoped 钥匙;
            // 生成号在 reset_all 里自增,进行中的回合据此识别陈旧句柄。
            let config = state.manager.lock().unwrap().config.clone();
            let config = if mode.as_deref() == Some("dev") {
                config.dev_scoped()
            } else {
                config
            };
            let memory = crate::memory::MemoryStore::new(&config, &state.paths);
            memory
                .reset_all(false)
                .map_err(|error| safe_error_message(&error))?;
            Ok(json!({}))
        }
        IpcCommand::ListSessions { mode } => {
            // dev 列表以 dev REPL 指针为"当前":全局指针指向普通会话,
            // 用它高亮永远落空。"all" 是管理面(gqy session):普通+dev
            // 合并按更新时间排,别的人格仍不可见。
            let dev = mode.as_deref() == Some("dev");
            let all = mode.as_deref() == Some("all");
            let current = if dev {
                store
                    .repl_session(crate::state::DEV_PERSONA)
                    .ok()
                    .flatten()
                    .unwrap_or_default()
                    .into()
            } else {
                store.session_id()
            };
            let sessions = if all {
                sessions_with_dev(store, &persona).map_err(|error| safe_error_message(&error))?
            } else {
                let scope = if dev {
                    crate::state::DEV_PERSONA.to_string()
                } else {
                    persona.clone()
                };
                store
                    .list_local_sessions(&scope)
                    .map_err(|error| safe_error_message(&error))?
            };
            let sessions: Vec<Value> = sessions
                .iter()
                .map(|overview| session_overview_json(overview, &current))
                .collect();
            Ok(json!({ "current": &*current, "sessions": sessions }))
        }
        IpcCommand::CreateSession {
            name,
            switch,
            kind,
            mode,
        } => {
            // Whitelisted: `ask` is the only non-user kind a client may mint,
            // and it is deliberately unswitchable — subagent audit sessions and
            // anything else stay daemon-internal.
            let kind = match kind.as_deref() {
                None | Some(crate::state::USER_SESSION_KIND) => crate::state::USER_SESSION_KIND,
                Some(crate::state::ASK_SESSION_KIND) if !switch => crate::state::ASK_SESSION_KIND,
                Some(_) => {
                    return Err(t("unsupported session kind", "不支持的会话类型").to_string())
                }
            };
            // No explicit name: leave it empty; the session is auto-named
            // from the first prompt when its first turn completes.
            let name = name.map(|name| name.trim().to_string()).unwrap_or_default();
            // dev 会话建到保留人格名下,模式由 persona 推导(见 DEV_PERSONA)。
            let session_persona = if mode.as_deref() == Some("dev") {
                crate::state::DEV_PERSONA
            } else {
                persona.as_str()
            };
            let record = store
                .create_session(session_persona, &name, kind, None)
                .map_err(|error| safe_error_message(&error))?;
            if kind == crate::state::USER_SESSION_KIND {
                state.events.publish(
                    "session.created",
                    json!({ "session_id": record.session_id, "name": record.name }),
                );
            }
            if switch {
                switch_session_via_actor(state, record.session_id.clone()).await?;
            }
            Ok(json!({ "session": session_record_json(&record) }))
        }
        IpcCommand::ToolCall {
            session,
            name,
            arguments,
            origin,
            depth,
        } => {
            if depth >= crate::tools::workspace::MAX_BRIDGE_DEPTH {
                return Err(format!(
                    "tool bridge recursion limit reached (depth {depth})"
                ));
            }
            let session_id = match session {
                Some(session) => {
                    resolve_local_session_ref(state, &ipc::SessionRef::Id { id: session })?
                        .session_id
                }
                None => store.session_id().to_string(),
            };
            let record = store
                .session_record(&session_id)
                .map_err(|error| safe_error_message(&error))?
                .ok_or_else(|| "session not found".to_string())?;
            let mode = turn_mode_for_session(store, &session_id, AgentMode::Normal);
            // 与回合同源的 registry(guard/超时齐备);会话工作区与来源
            // 一并作用域化,内层工具看到的世界和回合内一致。
            let config = { state.manager.lock().unwrap().config.clone() };
            let registry = crate::cli::build_tool_registry(&config, &state.paths, mode, false)
                .map_err(|error| safe_error_message(&error))?;
            let turn_origin: crate::tools::workspace::TurnOrigin = origin
                .as_deref()
                .and_then(|raw| serde_json::from_str(raw).ok())
                .unwrap_or(crate::tools::workspace::TurnOrigin::Human);
            let workspace = record
                .workspace
                .clone()
                .map(std::path::PathBuf::from)
                .filter(|path| path.is_dir())
                .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| ".".into()));
            let session_arc: Arc<str> = session_id.clone().into();
            let output = crate::tools::workspace::with_workspace(
                workspace,
                crate::tools::workspace::with_session(
                    session_arc,
                    crate::tools::workspace::with_turn_origin(
                        turn_origin,
                        crate::tools::workspace::with_bridge_depth(depth + 1, async {
                            registry.call(&name, &arguments).await
                        }),
                    ),
                ),
            )
            .await
            .map_err(|error| format!("tool error: {error:#}"))?;
            Ok(json!({ "output": output }))
        }
        IpcCommand::SetReplSession { target } => {
            let record = resolve_available_local_session_ref(state, &target)?;
            store
                .set_repl_session(&record.persona, &record.session_id)
                .map_err(|error| safe_error_message(&error))?;
            Ok(json!({ "session": session_record_json(&record) }))
        }
        IpcCommand::RenameSession { target, name } => {
            let record = resolve_local_session_ref(state, &target)?;
            if record.session_id == crate::state::DEFAULT_SESSION_ID {
                return Err(t(
                    "the terminal-integration session cannot be renamed",
                    "终端集成会话不可重命名",
                )
                .to_string());
            }
            let name = name.trim();
            if name.is_empty() {
                return Err(t("session name cannot be empty", "会话名称不能为空").to_string());
            }
            store
                .rename_session(&record.session_id, name)
                .map_err(|error| safe_error_message(&error))?;
            state.events.publish(
                "session.renamed",
                json!({ "session_id": record.session_id, "name": name }),
            );
            Ok(json!({}))
        }
        IpcCommand::DeleteSession { target } => {
            // Accepts `ask` too: a one-shot turn deletes its own session here.
            let record = resolve_local_session_ref_with_kinds(state, &target, TURN_TARGET_KINDS)?;
            // 终端集成会话是 CLI/shellhook 的固定入口,永远只有这一个;
            // 清空用 /reset,删除免谈(验收:WebUI 不许改默认会话)。
            if record.session_id == crate::state::DEFAULT_SESSION_ID {
                return Err(t(
                    "the terminal-integration session cannot be deleted",
                    "终端集成会话不可删除",
                )
                .to_string());
            }
            reserve_admin_for_session(&state.manager, &record.session_id)
                .map_err(|error| error.message)?;
            if &*store.session_id() == record.session_id.as_str() {
                let fallback = match fallback_session_id(state, &record.session_id) {
                    Ok(fallback) => fallback,
                    Err(error) => {
                        release_admin(&state.manager);
                        return Err(error);
                    }
                };
                if let Err(error) = switch_session_via_actor_reserved(state, fallback).await {
                    release_admin(&state.manager);
                    return Err(error);
                }
            }
            let result = store
                .delete_session(&record.session_id)
                .map_err(|error| safe_error_message(&error));
            release_admin(&state.manager);
            result?;
            state.events.publish(
                "session.deleted",
                json!({ "session_id": record.session_id }),
            );
            Ok(json!({}))
        }
        IpcCommand::SetWorkspace { target, path } => {
            let record = resolve_local_session_ref(state, &target)?;
            let workspace = match path {
                Some(path) => {
                    if !path.is_dir() {
                        return Err(format!(
                            "{}: {}",
                            t("workspace is not a directory", "workspace 不是目录"),
                            path.display()
                        ));
                    }
                    Some(path.to_string_lossy().into_owned())
                }
                None => None,
            };
            store
                .set_session_workspace(&record.session_id, workspace.as_deref())
                .map_err(|error| safe_error_message(&error))?;
            state.events.publish(
                "session.updated",
                json!({ "session_id": record.session_id, "workspace": workspace }),
            );
            Ok(json!({}))
        }
        IpcCommand::SetSessionModels { target, models } => {
            let record = resolve_local_session_ref(state, &target)?;
            let models = (!models.is_empty()).then_some(models);
            if let Some(models) = &models {
                let choices = {
                    let manager = state.manager.lock().unwrap();
                    manager.config.text_provider_model_choices()
                };
                for model in models {
                    if !choices.iter().any(|choice| {
                        choice.provider_id == model.provider_id && choice.model == model.model
                    }) {
                        return Err(format!(
                            "{}{}/{}",
                            t("unknown model: ", "未知模型："),
                            model.provider_id,
                            model.model
                        ));
                    }
                }
            }
            store
                .set_session_model_override(&record.session_id, models.as_deref())
                .map_err(|error| safe_error_message(&error))?;
            state.events.publish(
                "session.updated",
                json!({
                    "session_id": record.session_id,
                    "model_override": models,
                }),
            );
            Ok(json!({ "session_id": record.session_id }))
        }
        _ => Err("unsupported session command".to_string()),
    }
}
