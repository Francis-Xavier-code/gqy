//! actors — 自 src/web.rs 拆分。

use super::*;

pub(crate) async fn set_models(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Json(request): Json<SetModelsRequest>,
) -> std::result::Result<Json<ModelResponse>, ApiError> {
    require_mutation(&headers, &state)?;
    let models = validate_model_selection(request.models)?;
    reserve_admin_light(&state.manager)?;
    let (reply, receiver) = oneshot::channel();
    if state
        .actor_tx
        .send(ActorCommand::SetModels { models, reply })
        .is_err()
    {
        release_admin(&state.manager);
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "agent worker is unavailable",
        ));
    }
    match receiver.await {
        Ok(Ok(())) => {}
        Ok(Err(AdminFailure::Invalid(message))) => {
            return Err(ApiError::new(StatusCode::BAD_REQUEST, message));
        }
        Ok(Err(AdminFailure::Internal(message))) => {
            tracing::error!(
                error = %message,
                "{}",
                t("WebUI model update failed", "WebUI 模型更新失败")
            );
            return Err(ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                safe_error_message(&message),
            ));
        }
        Err(_) => {
            release_admin(&state.manager);
            return Err(ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "agent worker stopped before updating the model",
            ));
        }
    }
    let manager = state.manager.lock().unwrap();
    Ok(Json(ModelResponse {
        models: safe_models(&manager.config),
        display: web_display_config(&manager.config),
        context: manager.context,
    }))
}

pub(crate) async fn reset_conversation(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Json(request): Json<ResetConversationRequest>,
) -> std::result::Result<StatusCode, ApiError> {
    require_mutation(&headers, &state)?;
    let session_id = request
        .session_id
        .unwrap_or_else(|| state.state_store.session_id().to_string());
    require_local_web_session(&state, &session_id)?;
    let store = state.state_store.pinned(&session_id);
    if store.has_running_turns().map_err(ApiError::internal)? {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "a conversation turn is already running",
        ));
    }
    reserve_admin_for_session(&state.manager, &session_id)?;
    let (reply, receiver) = oneshot::channel();
    if state
        .actor_tx
        .send(ActorCommand::ResetConversation {
            session_id: session_id.into(),
            reply,
        })
        .is_err()
    {
        release_admin(&state.manager);
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "agent worker is unavailable",
        ));
    }
    match receiver.await {
        Ok(Ok(())) => Ok(StatusCode::NO_CONTENT),
        Ok(Err(AdminFailure::Invalid(message))) => {
            Err(ApiError::new(StatusCode::CONFLICT, message))
        }
        Ok(Err(AdminFailure::Internal(message))) => {
            tracing::error!(
                error = %message,
                "{}",
                t("WebUI conversation reset failed", "WebUI 对话重置失败")
            );
            Err(ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                safe_error_message(&message),
            ))
        }
        Err(_) => {
            release_admin(&state.manager);
            Err(ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "agent worker stopped before resetting the conversation",
            ))
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn spawn_actor(
    config: AppConfig,
    paths: GQYPaths,
    state_store: StateStore,
    manager: Arc<Mutex<ManagerState>>,
    events: EventHub,
    questions: QuestionBroker,
    turn_engine: TurnEngineState,
    memory_organizer: Option<MemoryOrganizerHandle>,
) -> Result<(mpsc::UnboundedSender<ActorCommand>, JoinHandle<Result<()>>)> {
    let (sender, receiver) = mpsc::unbounded_channel();
    let join = std::thread::Builder::new()
        .name("gqy-daemon-core".to_string())
        // tiktoken 词元计数器首次初始化会走 fancy_regex/regex_automata 的深递归
        // 编译，debug 构建栈帧大，默认 2MB 线程栈会溢出（release 勉强够用）
        .stack_size(16 * 1024 * 1024)
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .context("building daemon core runtime")?;
            // Turns are spawned as local tasks so several can run
            // concurrently on this thread (they are IO-bound); LocalSet
            // avoids imposing Send on the agent futures.
            let local = tokio::task::LocalSet::new();
            runtime.block_on(local.run_until(actor_loop(
                config,
                paths,
                state_store,
                manager,
                events,
                questions,
                turn_engine,
                memory_organizer,
                receiver,
            )));
            Ok(())
        })
        .context("starting daemon core thread")?;
    Ok((sender, join))
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn actor_loop(
    mut config: AppConfig,
    paths: GQYPaths,
    state_store: StateStore,
    manager: Arc<Mutex<ManagerState>>,
    events: EventHub,
    questions: QuestionBroker,
    turn_engine: TurnEngineState,
    memory_organizer: Option<MemoryOrganizerHandle>,
    mut receiver: mpsc::UnboundedReceiver<ActorCommand>,
) {
    let mut agent: Option<Agent> = None;
    let resource_cache = Arc::new(Mutex::new(TurnResourceCache::default()));
    while let Some(command) = receiver.recv().await {
        match command {
            ActorCommand::StartTurn {
                run_id,
                session_id,
                content,
                display_content,
                attachment_run_id,
                mode,
                images,
                cwd,
                origin_tty,
                audience,
                profile,
                cancel,
                turn_origin,
            } => {
                // Stale-turn recovery is owner-pid safe. Prompt maintenance is
                // performed after per-turn platform overrides are applied.
                let _ = state_store.recover_stale_turns();
                // 会话模式定死的最终防线:无论谁构造的 StartTurn(ipc/唤醒/
                // goal 驱动器),都按会话记录重derive 一次。
                let mode = turn_mode_for_session(&state_store, &session_id, mode);
                let store = state_store.pinned_for_turn(&session_id);
                // Per-turn workspace: a workspace bound to the session wins,
                // otherwise the calling client's cwd, otherwise the daemon
                // process cwd. The resolved path scopes the whole turn task.
                let workspace = store
                    .session_record(&session_id)
                    .ok()
                    .flatten()
                    .and_then(|record| record.workspace.map(std::path::PathBuf::from))
                    .filter(|path| path.is_dir())
                    .or_else(|| cwd.filter(|path| path.is_dir()))
                    .or_else(|| std::env::current_dir().ok())
                    .unwrap_or_else(|| std::path::PathBuf::from("."));
                // 平台回合的真实发起者。后台任务 spawn 时从 task-local 捕获,
                // 完成唤醒凭它还原身份(issue #29)。
                let platform_sender = profile
                    .as_ref()
                    .and_then(|profile| profile.platform.as_ref())
                    .map(|platform| platform.sender_id.clone());
                let task = run_turn_task(
                    config.clone(),
                    paths.clone(),
                    store,
                    state_store.clone(),
                    manager.clone(),
                    events.clone(),
                    questions.clone(),
                    run_id,
                    session_id.clone(),
                    TurnTaskInput::Create {
                        content,
                        display_content,
                        attachment_run_id,
                        images,
                    },
                    mode,
                    audience,
                    profile,
                    cancel,
                    resource_cache.clone(),
                    turn_engine.clone(),
                    memory_organizer.clone(),
                );
                tokio::task::spawn_local(crate::tools::workspace::with_workspace(
                    workspace,
                    crate::tools::workspace::with_session(
                        session_id,
                        crate::tools::workspace::with_origin_tty(
                            origin_tty,
                            crate::tools::workspace::with_platform_sender(
                                platform_sender,
                                crate::tools::workspace::with_turn_origin(*turn_origin, task),
                            ),
                        ),
                    ),
                ));
            }
            ActorCommand::RedoTurn {
                run_id,
                session_id,
                candidate,
                prompts,
                mode,
                cancel,
            } => {
                let _ = state_store.recover_stale_turns();
                let store = state_store.pinned_for_turn(&session_id);
                let workspace = store
                    .session_record(&session_id)
                    .ok()
                    .flatten()
                    .and_then(|record| record.workspace.map(std::path::PathBuf::from))
                    .filter(|path| path.is_dir())
                    .or_else(|| std::env::current_dir().ok())
                    .unwrap_or_else(|| std::path::PathBuf::from("."));
                let task = run_turn_task(
                    config.clone(),
                    paths.clone(),
                    store,
                    state_store.clone(),
                    manager.clone(),
                    events.clone(),
                    questions.clone(),
                    run_id,
                    session_id.clone(),
                    TurnTaskInput::Redo { candidate, prompts },
                    mode,
                    PromptAudience::External,
                    None,
                    cancel,
                    resource_cache.clone(),
                    turn_engine.clone(),
                    memory_organizer.clone(),
                );
                tokio::task::spawn_local(crate::tools::workspace::with_workspace(
                    workspace,
                    crate::tools::workspace::with_session(session_id, task),
                ));
            }
            ActorCommand::SetModels { models, reply } => {
                let result = rebuild_for_models(
                    &mut agent,
                    &mut config,
                    &paths,
                    &state_store,
                    &manager,
                    &models,
                );
                if result.is_ok() {
                    resource_cache.lock().unwrap().clear();
                    turn_engine.set(if agent.is_some() {
                        TurnEngineState::READY
                    } else {
                        TurnEngineState::COLD
                    });
                }
                release_admin(&manager);
                let _ = reply.send(result);
            }
            ActorCommand::SetThinkingVariants { updates, reply } => {
                let result = apply_thinking_variant_updates(&mut agent, &config, &paths, &updates);
                if result.is_ok() {
                    resource_cache.lock().unwrap().clear();
                }
                release_admin(&manager);
                let _ = reply.send(result);
            }
            ActorCommand::ApplyConfig {
                config: next_config,
                prompts,
                reset_conversation,
                reply,
            } => {
                // Persona layout changes migrate or delete session state that
                // running turns may be standing on, so those interrupt every
                // running turn before applying ("save after interrupting").
                // All other changes hot-apply: running turns keep the config
                // snapshot they cloned at start and later turns use the new
                // configuration.
                if config_change_requires_interrupt(&config, &next_config, &paths, &prompts) {
                    for info in manager.lock().unwrap().active_runs.values() {
                        info.request_cancel();
                    }
                    for _ in 0..100 {
                        if manager.lock().unwrap().active_runs.is_empty() {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                }
                let result = rebuild_for_config(
                    &mut agent,
                    &mut config,
                    &paths,
                    &state_store,
                    &manager,
                    &events,
                    *next_config,
                    &prompts,
                    reset_conversation,
                );
                if result.is_ok() {
                    resource_cache.lock().unwrap().clear();
                    turn_engine.set(if agent.is_some() {
                        TurnEngineState::READY
                    } else {
                        TurnEngineState::COLD
                    });
                    if let Some(handle) = memory_organizer.as_ref() {
                        handle.wake(config.clone(), paths.clone(), state_store.clone());
                    }
                }
                release_admin(&manager);
                let _ = reply.send(result);
            }
            ActorCommand::ResetConversation { session_id, reply } => {
                let result = reset_actor_conversation(
                    &mut agent,
                    &config,
                    &paths,
                    &state_store,
                    &manager,
                    &events,
                    &session_id,
                );
                release_admin(&manager);
                let _ = reply.send(result);
            }
            ActorCommand::ResetPersonaState {
                config: reset_config,
                reply,
            } => {
                let result = reset_actor_persona_state(
                    &mut agent,
                    &config,
                    &reset_config,
                    &paths,
                    &state_store,
                    &manager,
                    &events,
                );
                if result.is_ok() {
                    resource_cache.lock().unwrap().clear();
                }
                release_admin(&manager);
                let _ = reply.send(result);
            }
            ActorCommand::ClearSessionContent { session_id, reply } => {
                let result = clear_actor_session_content(
                    &mut agent,
                    &config,
                    &state_store,
                    &manager,
                    &session_id,
                );
                release_admin(&manager);
                let _ = reply.send(result);
            }
            ActorCommand::SwitchSession {
                session_id,
                release_reservation,
                reply,
            } => {
                let result = switch_actor_session(
                    agent.as_ref(),
                    &config,
                    &state_store,
                    &manager,
                    &events,
                    &session_id,
                );
                if release_reservation {
                    release_admin(&manager);
                }
                let _ = reply.send(result);
            }
            ActorCommand::Shutdown => {
                // Cancel every running turn, then drain briefly so they can
                // persist their interrupted state before the runtime drops.
                for info in manager.lock().unwrap().active_runs.values() {
                    info.request_cancel();
                }
                for _ in 0..100 {
                    if manager.lock().unwrap().active_runs.is_empty() {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                break;
            }
            ActorCommand::Undo { session_id, reply } => {
                let result = (|| -> std::result::Result<Value, AdminFailure> {
                    let store = state_store.pinned(&session_id);
                    let (removed, prompt) = store
                        .undo_last_turn()
                        .map_err(|error| AdminFailure::Internal(safe_error_message(&error)))?;
                    if &*state_store.session_id() == &*session_id {
                        manager.lock().unwrap().context =
                            actor_context(&agent, &config, &state_store).map_err(|error| {
                                AdminFailure::Internal(safe_error_message(&error))
                            })?;
                    }
                    Ok(json!({ "removed": removed, "prompt": prompt }))
                })();
                release_admin(&manager);
                let _ = reply.send(result);
            }
            ActorCommand::Pop {
                session_id,
                turn_ids,
                reply,
            } => {
                let result = (|| -> std::result::Result<Value, AdminFailure> {
                    if turn_ids.is_empty() {
                        return Ok(json!({ "turns": 0, "archived": false }));
                    }
                    let store = state_store.pinned(&session_id);
                    let turns = store
                        .oldest_evictable_visible_turns(usize::MAX)
                        .map_err(|error| AdminFailure::Internal(safe_error_message(&error)))?;
                    let selected = turns
                        .into_iter()
                        .filter(|turn| turn_ids.iter().any(|id| id == &turn.turn_id))
                        .collect::<Vec<_>>();
                    if selected.len() != turn_ids.len() {
                        return Err(AdminFailure::Invalid(
                            "one or more conversation turns are no longer available".to_string(),
                        ));
                    }
                    let memory = MemoryStore::new(&config, &paths);
                    let memory_config = config.memory_config();
                    archive_and_delete_visible_turns(&store, &memory, &selected)
                        .map_err(|error| AdminFailure::Internal(safe_error_message(&error)))?;
                    if &*state_store.session_id() == &*session_id {
                        manager.lock().unwrap().context =
                            actor_context(&agent, &config, &state_store).map_err(|error| {
                                AdminFailure::Internal(safe_error_message(&error))
                            })?;
                    }
                    let data = json!({
                        "turns": selected.len(),
                        "archived": memory_config.enabled && memory_config.evicted_context_enabled
                    });
                    let mut event_data = data.clone();
                    event_data["session_id"] = json!(&*session_id);
                    events.publish("conversation.pop", event_data);
                    Ok(data)
                })();
                release_admin(&manager);
                let _ = reply.send(result);
            }
            ActorCommand::Compact { session_id, reply } => {
                let result = async {
                    let updates_default = &*state_store.session_id() == &*session_id;
                    let compact = if updates_default {
                        let agent = ensure_actor_agent(
                            &mut agent,
                            &config,
                            &paths,
                            &state_store,
                            &turn_engine,
                        )?;
                        let compact = agent
                            .compact_now(|_| Ok(()))
                            .await
                            .map_err(|error| AdminFailure::Internal(safe_error_message(&error)))?;
                        manager.lock().unwrap().context = current_context(agent)
                            .map_err(|error| AdminFailure::Internal(safe_error_message(&error)))?;
                        compact
                    } else {
                        let store = state_store.pinned(&session_id);
                        let target_agent = build_actor_agent(&config, &paths, &store)
                            .map_err(|error| AdminFailure::Internal(safe_error_message(&error)))?;
                        target_agent
                            .compact_now(|_| Ok(()))
                            .await
                            .map_err(|error| AdminFailure::Internal(safe_error_message(&error)))?
                    };
                    Ok::<Value, AdminFailure>(json!({
                        "compacted": compact.is_some(),
                        "usage": compact.as_ref().and_then(|result| result.usage.clone()),
                        "usage_estimated": compact
                            .as_ref()
                            .map(|result| result.usage_estimated)
                            .unwrap_or(false)
                    }))
                }
                .await;
                release_admin(&manager);
                let _ = reply.send(result);
            }
        }
    }
}

pub(crate) fn trim_process_memory() {}

pub(crate) struct AttachmentRunGuard {
    store: StateStore,
    run_id: Option<String>,
}

pub(crate) enum TurnTaskInput {
    Create {
        content: String,
        display_content: String,
        attachment_run_id: Option<String>,
        images: Vec<Option<ImageAttachment>>,
    },
    Redo {
        candidate: crate::state::RedoCandidate,
        prompts: Vec<RedoWebPrompt>,
    },
}

pub(crate) fn into_pasted_images(
    images: Vec<Option<ImageAttachment>>,
) -> Vec<Option<crate::clipboard::PastedImage>> {
    images
        .into_iter()
        .map(|image| {
            image.map(|image| match image {
                ImageAttachment::Binary { mime, data } => crate::clipboard::PastedImage::Binary(
                    crate::clipboard::ClipboardImage::new(mime, data),
                ),
                ImageAttachment::Path { path } => crate::clipboard::PastedImage::Path(path),
            })
        })
        .collect()
}

impl AttachmentRunGuard {
    pub(crate) fn new(store: StateStore, run_id: Option<String>) -> Self {
        Self { store, run_id }
    }
}

impl Drop for AttachmentRunGuard {
    pub(crate) fn drop(&mut self) {
        if let Some(run_id) = self.run_id.as_deref() {
            let _ = self.store.release_user_attachments_for_run(run_id);
        }
    }
}

/// Executes one turn as a self-contained task. Multiple turn tasks run
/// concurrently on the actor's LocalSet — each with its own Agent, a
/// StateStore pinned to the turn's session, and an independent cancel signal.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_turn_task(
    mut config: AppConfig,
    paths: GQYPaths,
    store: StateStore,
    base_store: StateStore,
    manager: Arc<Mutex<ManagerState>>,
    events: EventHub,
    questions: QuestionBroker,
    run_id: String,
    session_id: Arc<str>,
    input: TurnTaskInput,
    mode: AgentMode,
    audience: PromptAudience,
    profile: Option<platforms::TurnProfile>,
    mut cancel: tokio::sync::watch::Receiver<bool>,
    resource_cache: Arc<Mutex<TurnResourceCache>>,
    turn_engine: TurnEngineState,
    memory_organizer: Option<MemoryOrganizerHandle>,
) {
    let attachment_run_id = match &input {
        TurnTaskInput::Create {
            attachment_run_id, ..
        } => attachment_run_id.clone(),
        TurnTaskInput::Redo { .. } => None,
    };
    let _attachment_guard = AttachmentRunGuard::new(base_store.clone(), attachment_run_id.clone());
    if let Some(profile) = &profile {
        if let Some(active_persona) = &profile.active_persona {
            config.prompt.active_persona.clone_from(active_persona);
        }
        if let Some(models) = &profile.text_models {
            config.active_provider_models = Some(models.clone());
        }
        // Groups drop whole turns instead of summarising: a compaction would
        // fold the structured group log into prose and every
        // `回复引用: msg=…` in the surviving turns would point at nothing.
        if let Some(group_context) = &profile.group_context {
            if !group_context.on_overflow.trim().is_empty() {
                config.context.on_overflow = group_context.on_overflow.trim().to_string();
            }
            if group_context.trim_batch_ratio > 0.0 {
                config.context.trim_batch_ratio = group_context.trim_batch_ratio;
            }
        }
        if let Some(models) = &profile.multimodal_models {
            config.active_multimodal_provider_models = Some(models.clone());
            // A conversation-specific multimodal pool is an explicit
            // override of the global vision plugin's single-model choice.
            config.plugins.vision.vision_provider_id.clear();
            config.plugins.vision.vision_model.clear();
        }
    }
    // Local sessions (REPL/WebUI/shell hook) may pin their own model pool.
    // Platform turns were already routed through the platform pools above.
    if profile
        .as_ref()
        .is_none_or(|profile| profile.text_models.is_none())
    {
        match base_store.session_model_override(&session_id) {
            Ok(Some(models)) => config.active_provider_models = Some(models),
            Ok(None) => {}
            Err(error) => tracing::warn!(
                error = %error,
                session_id = &*session_id,
                "{}",
                t(
                    "loading the session model override failed",
                    "读取会话模型覆盖失败"
                )
            ),
        }
    }
    let manager = &manager;
    let events = &events;
    let questions = &questions;
    let run_id = run_id.as_str();
    let operation = match &input {
        TurnTaskInput::Create { .. } => "create",
        TurnTaskInput::Redo { .. } => "redo",
    };
    events.publish(
        "run.started",
        json!({
            "run_id": run_id,
            "session_id": &*session_id,
            "mode": mode_name(mode),
            "operation": operation,
        }),
    );
    let title_seed: String = match &input {
        TurnTaskInput::Create { content, .. } => content.chars().take(80).collect(),
        TurnTaskInput::Redo { candidate, .. } => {
            candidate.display_content.chars().take(80).collect()
        }
    };
    let warming = !turn_engine.is_ready();
    if warming {
        turn_engine.set(TurnEngineState::INITIALIZING);
    }
    let setup = (|| -> Result<(Agent, AgentTurnControl)> {
        let platform_context = profile
            .as_ref()
            .and_then(|profile| profile.platform.as_deref());
        let local_webui = is_local_webui_request(audience, profile.is_some());
        let resources = resource_cache
            .lock()
            .map_err(|_| anyhow::anyhow!("turn resource cache is poisoned"))?
            .get_or_build(&config, &paths)?;
        let restricted = platform_context.is_some_and(|context| !context.host_tools_allowed());
        let mut normal_tools = if restricted {
            resources.restricted_tools.clone()
        } else {
            resources.normal_tools.clone()
        };
        let mut dev_tools = if restricted {
            resources.restricted_tools.clone()
        } else {
            resources.dev_tools.clone()
        };
        if !restricted {
            if let Some(context) = platform_context {
                tools::rescope_platform_memory_tools(
                    &mut normal_tools,
                    &config,
                    &paths,
                    context,
                    false,
                );
            }
        }
        if local_webui && config.tools.enabled {
            tools::register_webui_artifact_tools(&mut normal_tools, &paths, &session_id);
        }
        if profile
            .as_ref()
            .is_some_and(|profile| !profile.memory_write_enabled)
        {
            normal_tools.unregister("remember_fact");
            dev_tools.unregister("remember_fact");
        }
        if platform_context.is_none() && config.tools.enabled {
            tools::register_ask_question(&mut normal_tools);
            tools::register_ask_question(&mut dev_tools);
        }
        if config.tools.enabled {
            if let Some(context) = profile
                .as_ref()
                .and_then(|profile| profile.platform.clone())
            {
                platforms::register_platform_tools(&mut normal_tools, context.clone());
                platforms::register_platform_tools(&mut dev_tools, context);
            }
        }
        let active_tools = match mode {
            AgentMode::Normal => normal_tools.clone(),
            AgentMode::Dev => dev_tools.clone(),
        };
        let mut agent = Agent::new_for_audience(
            config.clone(),
            &paths,
            store.clone(),
            // A platform turn buffers a whole round and posts it as one
            // message, so a stream that dies mid-round showed the group
            // nothing and can be retried on another endpoint — or the same
            // one — without anybody seeing a false start.
            resources
                .client
                .clone()
                .with_buffered_delivery(platform_context.is_some()),
            active_tools,
            mode,
            audience,
        )?;
        let mut runtime_system_context = profile
            .as_ref()
            .map(|profile| profile.system_context.clone())
            .unwrap_or_default();
        let mut turn_system_context = profile
            .as_ref()
            .map(|profile| profile.turn_system_context.clone())
            .unwrap_or_default();
        if local_webui && mode == AgentMode::Normal {
            let manifest = tools::webui_artifact_manifest(&paths, &session_id)
                .unwrap_or_else(|_| "（Artifact 清单暂时不可用）".to_string());
            // v7 Phase 2.1: the manifest changes whenever artifacts change, so
            // it rides the turn tail; only the static policy stays in the
            // system prompt.
            turn_system_context.push(format!(
                "<artifact-workspace>\n{manifest}\n使用 read_artifact 和 apply_artifact_patch 按文件名操作已有 Artifact；不要用 glob 搜索托管目录，也不要猜测 ~/.gqy 路径。\n</artifact-workspace>"
            ));
            runtime_system_context.push(
                "<artifact-policy>\n\
                你正在 GQY WebUI 中工作，并且拥有 Artifact 展示工具。\n\
                - 当用户明确要求报告、文档、网页、表格、数据文件、独立代码文件或其他可下载成品时，必须创建或展示 Artifact。\n\
                - 对由你直接编写的文本交付物，优先调用 create_artifact；filename 必须带正确扩展名。\n\
                - 对命令或其他工具已经生成的文件，调用 present_artifact。\n\
                - 更新已有 Artifact 时先使用 read_artifact，再使用 apply_artifact_patch 做局部修改；补丁路径只写 Artifact 文件名。除非用户明确要求完全重写，否则不要用 create_artifact 覆盖全文。\n\
                - 内容完成并自检后再发布。普通项目源码修改、配置修改、测试夹具和简短回答不要发布为 Artifact。\n\
                - Artifact 是回答的一部分；发布成功后再用简短文字告知用户。\n\
                </artifact-policy>"
                    .to_string(),
            );
        }
        if !runtime_system_context.is_empty() {
            agent.set_runtime_system_context(runtime_system_context)?;
        }
        if !turn_system_context.is_empty() {
            agent.set_turn_system_context(turn_system_context);
        }
        if let Some(profile) = &profile {
            agent.set_memory_writes_enabled(profile.memory_write_enabled);
            agent.set_memory_content(profile.memory_content.clone());
            agent.set_session_history_suppressed(profile.suppress_session_history);
            if let Some(namespace) = profile.image_cache_namespace.as_deref() {
                agent.set_image_platform(
                    namespace,
                    profile.image_source_label.as_deref().unwrap_or(namespace),
                );
            }
            if let Some(context) = profile.platform.as_deref() {
                let principal = context.principal().stable_key();
                agent.set_memory_request_context(
                    if context.is_admin {
                        MemoryAccess::Privileged
                    } else {
                        MemoryAccess::principal(principal.clone())
                    },
                    Some(principal),
                    context.sender_display_name.clone(),
                );
                agent.set_memory_origin(MemoryOrigin {
                    kind: "platform".to_string(),
                    platform: context.conversation.platform.clone(),
                    account_id: context.conversation.account_id.clone(),
                    conversation_kind: context.conversation.kind.as_str().to_string(),
                    conversation_id: context.conversation.conversation_id.clone(),
                    sender_id: context.sender_id.clone(),
                    sender_display_name: context.sender_display_name.clone(),
                    session_id: session_id.to_string(),
                    message_id: context
                        .inbound_event()
                        .map(|event| event.message_id.clone())
                        .unwrap_or_default(),
                });
            }
            if let Some(context) = profile.platform.clone() {
                agent.set_platform_context_images(context, profile.context_images.clone());
            }
        }
        if let Some(organizer) = memory_organizer.clone() {
            agent.set_memory_organizer(organizer);
        }
        agent.prepare_for_turn()?;
        let mut control = AgentTurnControl::new(mode, normal_tools, dev_tools);
        if let Some(signal) = manager
            .lock()
            .unwrap()
            .active_runs
            .get(run_id)
            .map(|run| run.supersede.clone())
        {
            control.set_supersede_signal(signal);
        }
        if let Some(ingress) = profile
            .as_ref()
            .and_then(|profile| profile.followup.as_ref())
            .map(|followup| followup.ingress())
        {
            control.set_queue_ingress(ingress);
        }
        Ok((agent, control))
    })();
    let (mut agent, control) = match setup {
        Ok(setup) => {
            turn_engine.set(TurnEngineState::READY);
            setup
        }
        Err(error) => {
            if warming {
                turn_engine.set(TurnEngineState::FAILED);
            }
            questions.cancel_run(run_id);
            finish_run(manager, run_id, None);
            let message = safe_error_message(&error);
            tracing::error!(
                run_id,
                error = %error,
                "{}",
                t("WebUI agent run setup failed", "WebUI 智能体运行初始化失败")
            );
            events.publish(
                "run.failed",
                json!({ "run_id": run_id, "session_id": &*session_id, "message": message }),
            );
            return;
        }
    };
    if let TurnTaskInput::Create {
        display_content, ..
    } = &input
    {
        agent.set_turn_persistence(display_content.clone(), attachment_run_id);
    }
    // The daemon-wide context snapshot tracks the *current* session; a turn
    // for another session must not overwrite it.
    let updates_context = || *base_store.session_id() == *session_id;
    let agent = &mut agent;
    let (redo_input_id, redo_display_content) = match &input {
        TurnTaskInput::Redo { candidate, prompts } => (
            Some(candidate.input_id.clone()),
            prompts.last().map(|prompt| prompt.display_content.clone()),
        ),
        TurnTaskInput::Create { .. } => (None, None),
    };

    let mapper = Arc::new(Mutex::new(RunEventMapper::new(
        run_id.to_string(),
        events.clone(),
        questions.clone(),
        store.clone(),
        manager.clone(),
        profile
            .as_ref()
            .and_then(|profile| profile.followup.as_ref())
            .map(|followup| followup.ingress()),
        operation,
        redo_input_id,
        redo_display_content,
        config.display.command_output_lines,
    )));
    let chat_outcome = match input {
        TurnTaskInput::Create {
            content, images, ..
        } => {
            let callback_mapper = mapper.clone();
            let images = into_pasted_images(images);
            let chat = agent.chat_stream_with_control(&content, &images, &control, move |event| {
                callback_mapper.lock().unwrap().handle(event);
                Ok(())
            });
            tokio::pin!(chat);
            loop {
                tokio::select! {
                    biased;
                    result = &mut chat => break TurnOutcome::Finished(result),
                    changed = cancel.changed() => {
                        if changed.is_err() || *cancel.borrow() {
                            questions.cancel_run(run_id);
                            break TurnOutcome::Cancelled;
                        }
                    }
                }
            }
        }
        TurnTaskInput::Redo { candidate, prompts } => {
            let callback_mapper = mapper.clone();
            let prompts = prompts
                .into_iter()
                .map(|prompt| crate::agent::RedoPromptInput {
                    prompt_id: prompt.prompt_id,
                    content: prompt.content,
                    display_content: prompt.display_content,
                    images: into_pasted_images(prompt.images),
                })
                .collect();
            let chat =
                agent.redo_stream_with_control(&candidate, prompts, &control, move |event| {
                    callback_mapper.lock().unwrap().handle(event);
                    Ok(())
                });
            tokio::pin!(chat);
            loop {
                tokio::select! {
                    biased;
                    result = &mut chat => break TurnOutcome::Finished(result),
                    changed = cancel.changed() => {
                        if changed.is_err() || *cancel.borrow() {
                            questions.cancel_run(run_id);
                            break TurnOutcome::Cancelled;
                        }
                    }
                }
            }
        }
    };

    let result = match chat_outcome {
        TurnOutcome::Cancelled => {
            drop_cancelled_queue(&store, events, run_id, &session_id);
            finish_cancelled_run(
                manager,
                events,
                agent,
                run_id,
                &session_id,
                updates_context(),
            );
            finish_turn_task(&config, &paths, &store, &title_seed, events, false);
            return;
        }
        TurnOutcome::Finished(Err(error)) if question::is_question_cancelled(&error) => {
            questions.cancel_run(run_id);
            drop_cancelled_queue(&store, events, run_id, &session_id);
            finish_cancelled_run(
                manager,
                events,
                agent,
                run_id,
                &session_id,
                updates_context(),
            );
            finish_turn_task(&config, &paths, &store, &title_seed, events, false);
            return;
        }
        TurnOutcome::Finished(Err(error)) => {
            finish_failed_run(
                manager,
                events,
                questions,
                agent,
                run_id,
                &session_id,
                updates_context(),
                &error,
            );
            finish_turn_task(&config, &paths, &store, &title_seed, events, false);
            return;
        }
        TurnOutcome::Finished(Ok(result)) => result,
    };

    questions.cancel_run(run_id);
    let context_tokens = match agent.effective_context_tokens() {
        Ok(tokens) => tokens,
        Err(error) => {
            finish_completed_with_context_error(
                manager,
                events,
                agent,
                run_id,
                &session_id,
                updates_context(),
                &result,
                &error,
            );
            finish_turn_task(&config, &paths, &store, &title_seed, events, true);
            return;
        }
    };
    let overflow_outcome = {
        let callback_mapper = mapper;
        let overflow = agent.handle_overflow_after_turn(context_tokens, move |event| {
            callback_mapper.lock().unwrap().handle(event);
            Ok(())
        });
        tokio::pin!(overflow);
        loop {
            tokio::select! {
                biased;
                result = &mut overflow => break OverflowOutcome::Finished(result),
                changed = cancel.changed() => {
                    if changed.is_err() || *cancel.borrow() {
                        break OverflowOutcome::Cancelled;
                    }
                }
            }
        }
    };
    match overflow_outcome {
        OverflowOutcome::Cancelled => {
            drop_cancelled_queue(&store, events, run_id, &session_id);
            let context =
                current_context(agent).unwrap_or_else(|_| manager.lock().unwrap().context);
            finish_run(manager, run_id, updates_context().then_some(context));
            publish_completed(events, run_id, &session_id, &result, context);
            finish_turn_task(&config, &paths, &store, &title_seed, events, true);
            return;
        }
        OverflowOutcome::Finished(Err(error)) => {
            finish_completed_with_context_error(
                manager,
                events,
                agent,
                run_id,
                &session_id,
                updates_context(),
                &result,
                &error,
            );
            finish_turn_task(&config, &paths, &store, &title_seed, events, true);
            return;
        }
        OverflowOutcome::Finished(Ok(_)) => {}
    }
    let context = match current_context(agent) {
        Ok(context) => context,
        Err(error) => {
            finish_completed_with_context_error(
                manager,
                events,
                agent,
                run_id,
                &session_id,
                updates_context(),
                &result,
                &error,
            );
            finish_turn_task(&config, &paths, &store, &title_seed, events, true);
            return;
        }
    };
    finish_run(manager, run_id, updates_context().then_some(context));
    publish_completed(events, run_id, &session_id, &result, context);
    finish_turn_task(&config, &paths, &store, &title_seed, events, true);
}

/// Shared per-turn cleanup: auto-naming, activity timestamp, queue-identity
/// cleanup, and allocator trimming. `store` is the turn's pinned store, so
/// session-scoped operations hit the turn's own session.
pub(crate) fn finish_turn_task(
    config: &AppConfig,
    paths: &GQYPaths,
    store: &StateStore,
    title_seed: &str,
    events: &EventHub,
    completed: bool,
) {
    if completed {
        if let Some(fallback) = maybe_auto_name_session(store, events, title_seed) {
            spawn_session_title_refinement(config, paths, store, events, fallback, title_seed);
        }
        let _ = store.touch_session(&store.session_id());
    }
    let _ = store.discard_queued_prompts();
    trim_process_memory();
}

/// Best-effort AI pass over the truncated default session name: ask the
/// main model pool for a concise title and apply it only if the
/// auto-generated name is still in place (a user rename wins). Runs
/// detached on the actor's LocalSet — never blocks the turn.
pub(crate) fn spawn_session_title_refinement(
    config: &AppConfig,
    paths: &GQYPaths,
    store: &StateStore,
    events: &EventHub,
    fallback: String,
    seed: &str,
) {
    let Ok(client) = OpenAiCompatibleClient::from_config(config, paths) else {
        return;
    };
    let store = store.clone();
    let events = events.clone();
    let seed = seed.to_string();
    tokio::task::spawn_local(async move {
        let session_id = store.session_id();
        let prompt = format!(
            "为下面这条用户消息生成一个简洁的会话标题。要求：不超过 16 个字，             概括主题，只输出标题本身，不要引号、句号或任何解释。

用户消息：{seed}"
        );
        let result = client
            .chat_stream(
                vec![
                    crate::llm::ChatMessage::system("你是会话标题生成器，只输出标题本身。"),
                    crate::llm::ChatMessage::plain("user", prompt),
                ],
                Vec::new(),
                |_| Ok(()),
            )
            .await;
        let Ok(result) = result else { return };
        let title = sanitize_session_title(&result.content);
        if title.is_empty() {
            return;
        }
        let Ok(Some(record)) = store.session_record(&session_id) else {
            return;
        };
        if record.name != fallback {
            return;
        }
        if store.rename_session(&record.session_id, &title).is_ok() {
            events.publish(
                "session.renamed",
                json!({ "session_id": record.session_id, "name": title }),
            );
        }
        if let Some(usage) = result.usage.as_ref() {
            let meta = crate::state::UsageMeta {
                source: "agent",
                provider: result.provider_id.as_deref(),
                model: result.model.as_deref(),
            };
            let _ = store.add_auxiliary_usage(usage, meta);
        }
    });
}

/// Cleans an LLM-generated title down to a single short line: first line
/// only, surrounding quotes/punctuation stripped, clipped to 20 chars.
pub(crate) fn sanitize_session_title(raw: &str) -> String {
    let cleaned = raw
        .trim()
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .trim_matches(|c: char| {
            matches!(
                c,
                '"' | '\''
                    | '“'
                    | '”'
                    | '‘'
                    | '’'
                    | '「'
                    | '」'
                    | '《'
                    | '》'
                    | '。'
                    | '.'
                    | '，'
                    | ','
            )
        })
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    cleaned.chars().take(20).collect()
}

pub(crate) enum TurnOutcome {
    Finished(Result<ChatResult>),
    Cancelled,
}

pub(crate) enum OverflowOutcome {
    Finished(Result<Option<ChatResult>>),
    Cancelled,
}

pub(crate) fn active_thinking_variant_options(
    config: &AppConfig,
    paths: &GQYPaths,
) -> Result<Vec<ThinkingVariantOptions>> {
    crate::models_cache::ensure_active_metadata(paths, config);
    let preferences = ThinkingVariantPreferences::load(paths);
    config
        .active_provider_model_choices()
        .into_iter()
        .map(|choice| {
            let provider = config.provider(Some(&choice.provider_id))?;
            Ok(thinking_variant_options_for_model(
                provider,
                &choice.model,
                preferences.selected(&choice.provider_id, &choice.model),
            ))
        })
        .collect()
}

pub(crate) fn apply_thinking_variant_updates(
    agent: &mut Option<Agent>,
    config: &AppConfig,
    paths: &GQYPaths,
    updates: &[ThinkingVariantUpdate],
) -> std::result::Result<(), AdminFailure> {
    let options = active_thinking_variant_options(config, paths)
        .map_err(|error| AdminFailure::Internal(safe_error_message(error)))?;
    for update in updates {
        let option = options
            .iter()
            .find(|option| option.provider_id == update.provider_id && option.model == update.model)
            .ok_or_else(|| {
                AdminFailure::Invalid(format!(
                    "inactive model: {} / {}",
                    update.provider_id, update.model
                ))
            })?;
        if let Some(selected) = &update.selected {
            if !option.variants.iter().any(|variant| variant == selected) {
                return Err(AdminFailure::Invalid(format!(
                    "thinking variant is unavailable for {} / {}: {}",
                    update.provider_id, update.model, selected
                )));
            }
        }
    }

    let selections = updates
        .iter()
        .map(|update| {
            (
                update.provider_id.clone(),
                update.model.clone(),
                update.selected.clone(),
            )
        })
        .collect::<Vec<_>>();
    let next_client = agent
        .as_ref()
        .map(|current| {
            let mut client = current.cloned_client();
            client
                .set_thinking_variants(&selections)
                .map_err(|error| AdminFailure::Invalid(safe_error_message(error)))?;
            Ok(client)
        })
        .transpose()?;

    let mut preferences = ThinkingVariantPreferences::load(paths);
    for update in updates {
        preferences.set(&update.provider_id, &update.model, update.selected.clone());
    }
    preferences
        .save(paths)
        .map_err(|error| AdminFailure::Internal(safe_error_message(error)))?;

    if let (Some(agent), Some(client)) = (agent.as_mut(), next_client) {
        agent.replace_client(client);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn rebuild_for_models(
    agent: &mut Option<Agent>,
    config: &mut AppConfig,
    paths: &GQYPaths,
    state_store: &StateStore,
    manager: &Arc<Mutex<ManagerState>>,
    models: &[ActiveProviderModelConfig],
) -> std::result::Result<(), AdminFailure> {
    let mut next_config = config.clone();
    next_config
        .set_active_provider_models(models)
        .map_err(|error| AdminFailure::Invalid(safe_error_message(&error)))?;
    if next_config.active_provider_models == config.active_provider_models {
        return Ok(());
    }
    let next_agent = if agent.is_some() {
        crate::models_cache::ensure_active_metadata(paths, &next_config);
        let client = OpenAiCompatibleClient::from_config(&next_config, paths)
            .map_err(|error| AdminFailure::Internal(safe_error_message(&error)))?;
        let registry = build_tool_registry(&next_config, paths, AgentMode::Normal, true)
            .map_err(|error| AdminFailure::Internal(safe_error_message(&error)))?;
        Some(
            Agent::new(
                next_config.clone(),
                paths,
                state_store.clone(),
                client,
                registry,
                AgentMode::Normal,
            )
            .map_err(|error| AdminFailure::Internal(safe_error_message(&error)))?,
        )
    } else {
        None
    };
    let context = next_agent
        .as_ref()
        .map_or_else(|| cold_context(&next_config, state_store), current_context)
        .map_err(|error| AdminFailure::Internal(safe_error_message(&error)))?;
    next_config
        .save(paths)
        .map_err(|error| AdminFailure::Internal(safe_error_message(&error)))?;
    *agent = next_agent;
    *config = next_config.clone();
    let mut manager = manager.lock().unwrap();
    manager.config = next_config;
    manager.context = context;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn session_for_persona(
    state_store: &StateStore,
    manager: &Arc<Mutex<ManagerState>>,
    persona: &str,
) -> Result<String> {
    if let Some(session_id) = state_store.persona_current_session(persona)? {
        if is_available_local_session(state_store, &session_id, persona)? {
            return Ok(session_id);
        }
    }
    let remembered = manager
        .lock()
        .unwrap()
        .persona_session_ids
        .get(persona)
        .cloned();
    if let Some(session_id) = remembered {
        if is_available_local_session(state_store, &session_id, persona)? {
            return Ok(session_id);
        }
    }
    if let Some(overview) = state_store
        .list_local_sessions(persona)?
        .into_iter()
        .next()
    {
        return Ok(overview.record.session_id);
    }
    Ok(state_store
        .create_session(persona, "", "user", None)?
        .session_id)
}

