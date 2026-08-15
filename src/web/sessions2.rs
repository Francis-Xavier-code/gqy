//! sessions2 — 自 src/web.rs 拆分。

use super::*;

pub(crate) fn active_persona_scope(state: &DaemonState) -> String {
    state.manager.lock().unwrap().config.active_persona_scope()
}

pub(crate) fn session_api_error(message: String) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, message)
}

pub(crate) fn require_local_web_session(
    state: &DaemonState,
    session_id: &str,
) -> std::result::Result<crate::state::SessionRecord, ApiError> {
    let record = state
        .state_store
        .session_record(session_id)
        .map_err(ApiError::internal)?;
    let is_platform = state
        .state_store
        .is_platform_session(session_id)
        .map_err(ApiError::internal)?;
    match record {
        // dev 会话(保留人格)对 WebUI 可见:侧栏分组列它,打开/改名/删除
        // 也得放行,否则点进去 404「会话不存在」(验收三轮)。
        Some(record)
            if !is_platform
                && record.kind == "user"
                && (record.persona == active_persona_scope(state)
                    || record.persona == crate::state::DEV_PERSONA) =>
        {
            Ok(record)
        }
        _ => Err(ApiError::new(StatusCode::NOT_FOUND, "session not found")),
    }
}

pub(crate) async fn list_sessions_http(
    State(state): State<DaemonState>,
    headers: HeaderMap,
) -> std::result::Result<Response, ApiError> {
    require_auth(&headers, &state)?;
    let current = state.state_store.session_id();
    let persona = active_persona_scope(&state);
    // 侧栏按模式分组:普通+dev 一起下发,mode 字段区分(问题七)。
    let sessions =
        sessions_with_dev(&state.state_store, &persona).map_err(ApiError::internal)?;
    let sessions = sessions
        .iter()
        .map(|overview| session_overview_json(overview, &current))
        .collect::<Vec<_>>();
    let data = json!({ "current": &*current, "sessions": sessions });
    Ok(Json(data).into_response())
}

#[derive(Deserialize)]
pub(crate) struct CreateSessionRequest {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    switch: bool,
    /// "dev" 建 Build 模式会话(保留人格 dev);缺省=当前人格普通会话。
    #[serde(default)]
    mode: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct ResetConversationRequest {
    session_id: Option<String>,
}

pub(crate) async fn create_session_http(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Json(request): Json<CreateSessionRequest>,
) -> std::result::Result<Response, ApiError> {
    require_mutation(&headers, &state)?;
    let data = handle_session_command(
        &state,
        IpcCommand::CreateSession {
            name: request.name,
            switch: request.switch,
            kind: None,
            mode: request.mode,
        },
    )
    .await
    .map_err(session_api_error)?;
    Ok((StatusCode::CREATED, Json(data)).into_response())
}

#[derive(Deserialize)]
pub(crate) struct UpdateSessionRequest {
    #[serde(default)]
    name: Option<String>,
    /// `Some("")` unbinds the workspace; a non-empty value binds it.
    #[serde(default)]
    workspace: Option<String>,
}

pub(crate) async fn update_session_http(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Path(session_id): Path<String>,
    Json(request): Json<UpdateSessionRequest>,
) -> std::result::Result<Response, ApiError> {
    require_mutation(&headers, &state)?;
    require_local_web_session(&state, &session_id)?;
    let target = || ipc::SessionRef::Id {
        id: session_id.clone(),
    };
    if let Some(name) = request.name {
        handle_session_command(
            &state,
            IpcCommand::RenameSession {
                target: target(),
                name,
            },
        )
        .await
        .map_err(session_api_error)?;
    }
    if let Some(workspace) = request.workspace {
        let path = (!workspace.trim().is_empty()).then(|| std::path::PathBuf::from(workspace));
        handle_session_command(
            &state,
            IpcCommand::SetWorkspace {
                target: target(),
                path,
            },
        )
        .await
        .map_err(session_api_error)?;
    }
    Ok(Json(json!({})).into_response())
}

/// Read-only snapshot of one session's conversation for per-view browsing:
/// turns, queued follow-ups, and its currently running turns. Does not touch
/// the global current-session pointer.
pub(crate) async fn session_turns_http(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Path(session_id): Path<String>,
) -> std::result::Result<Response, ApiError> {
    require_auth(&headers, &state)?;
    require_local_web_session(&state, &session_id)?;
    let store = state.state_store.pinned(&session_id);
    let mut assets_by_turn = HashMap::<String, Vec<ImageAsset>>::new();
    for asset in store.load_image_assets().map_err(ApiError::internal)? {
        assets_by_turn
            .entry(asset.turn_id.clone())
            .or_default()
            .push(asset);
    }
    let mut artifacts_by_turn = HashMap::<String, Vec<ArtifactAsset>>::new();
    for artifact in store.load_artifact_assets().map_err(ApiError::internal)? {
        artifacts_by_turn
            .entry(artifact.turn_id.clone())
            .or_default()
            .push(artifact);
    }
    let turns: Vec<SafeTurn> = store
        .load_turns()
        .map_err(ApiError::internal)?
        .into_iter()
        .filter(|turn| !turn.is_summary)
        .map(|turn| {
            let assets = assets_by_turn.remove(&turn.turn_id).unwrap_or_default();
            let artifacts = artifacts_by_turn.remove(&turn.turn_id).unwrap_or_default();
            SafeTurn::from_turn(turn, assets, artifacts)
        })
        .collect();
    let running_target = store
        .running_turn_queue_target()
        .map_err(ApiError::internal)?;
    let queued_prompts: Vec<SafeQueuedPrompt> = match running_target.as_ref() {
        Some(target) => store
            .load_queued_prompts_for_target(target)
            .map_err(ApiError::internal)?,
        None => Vec::new(),
    }
    .into_iter()
    .map(SafeQueuedPrompt::from)
    .collect();
    let runs: Vec<Value> = state
        .manager
        .lock()
        .unwrap()
        .active_runs
        .iter()
        .filter(|(_, info)| &*info.session_id == session_id.as_str())
        .map(|(run_id, info)| {
            json!({
                "run_id": run_id,
                "session_id": &*info.session_id,
                "mode": mode_name(info.mode),
                "operation": info.operation.name(),
                "turn_id": info.operation.turn_id(),
                "input_id": info.operation.input_id(),
            })
        })
        .collect();
    let redo_candidate = if runs.is_empty() {
        store
            .redo_candidate()
            .map_err(ApiError::internal)?
            .map(SafeRedoCandidate::from)
    } else {
        None
    };
    let mut response = Json(json!({
        "session_id": session_id,
        "turns": turns,
        "queued_prompts": queued_prompts,
        "running_turn_id": running_target.as_ref().map(|target| target.turn_id.as_str()),
        "runs": runs,
        "redo_candidate": redo_candidate,
    }))
    .into_response();
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

pub(crate) async fn delete_session_http(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Path(session_id): Path<String>,
) -> std::result::Result<Response, ApiError> {
    require_mutation(&headers, &state)?;
    require_local_web_session(&state, &session_id)?;
    let data = handle_session_command(
        &state,
        IpcCommand::DeleteSession {
            target: ipc::SessionRef::Id { id: session_id },
        },
    )
    .await
    .map_err(session_api_error)?;
    Ok(Json(data).into_response())
}

pub(crate) fn resolve_local_session_ref(
    state: &DaemonState,
    target: &ipc::SessionRef,
) -> std::result::Result<crate::state::SessionRecord, String> {
    resolve_local_session_ref_with_kinds(state, target, &[crate::state::USER_SESSION_KIND])
}

/// Same, but for the two callers that must also reach one-shot `ask` sessions
/// (running their turn, then deleting them). `SessionRef::Name` still cannot
/// find those — the DB lookup filters to user sessions — so only the client
/// holding the freshly minted id can address one.
pub(crate) fn resolve_local_session_ref_with_kinds(
    state: &DaemonState,
    target: &ipc::SessionRef,
    kinds: &[&str],
) -> std::result::Result<crate::state::SessionRecord, String> {
    let store = &state.state_store;
    let persona = active_persona_scope(state);
    let record = match target {
        ipc::SessionRef::Current => store
            .session_record(&store.session_id())
            .map_err(|error| safe_error_message(&error))?,
        ipc::SessionRef::Id { id } => store
            .session_record(id)
            .map_err(|error| safe_error_message(&error))?,
        ipc::SessionRef::Name { name } => store
            .find_local_session_by_name(&persona, name)
            .map_err(|error| safe_error_message(&error))?,
    };
    let Some(record) = record else {
        return Err(t("session not found", "找不到该会话").to_string());
    };
    let is_platform = store
        .is_platform_session(&record.session_id)
        .map_err(|error| safe_error_message(&error))?;
    // 人格过滤只约束按名寻址与当前指针:显式 id 是不可猜测的能力凭据,
    // 且 dev 会话(保留人格 "dev")必须能被 dev REPL 按 id 操作——否则
    // 起回合/切换/指针全部 404(验收问题二:dev 首启即被踢回默认会话)。
    let persona_ok = record.persona == persona
        || record.persona == crate::state::DEV_PERSONA
        || matches!(target, ipc::SessionRef::Id { .. });
    if !persona_ok || !kinds.contains(&record.kind.as_str()) || is_platform {
        return Err(t("session not found", "找不到该会话").to_string());
    }
    Ok(record)
}

pub(crate) fn resolve_available_local_session_ref(
    state: &DaemonState,
    target: &ipc::SessionRef,
) -> std::result::Result<crate::state::SessionRecord, String> {
    resolve_local_session_ref(state, target)
}

/// Turn targets and deletions additionally accept one-shot `ask` sessions.
pub(crate) const TURN_TARGET_KINDS: &[&str] = &[
    crate::state::USER_SESSION_KIND,
    crate::state::ASK_SESSION_KIND,
];

/// Most recently updated other user session, or a fresh default session when
/// none is left.
pub(crate) fn fallback_session_id(state: &DaemonState, exclude: &str) -> std::result::Result<String, String> {
    let persona = active_persona_scope(state);
    let sessions = state
        .state_store
        .list_local_sessions(&persona)
        .map_err(|error| safe_error_message(&error))?;
    if let Some(overview) = sessions
        .iter()
        .find(|overview| overview.record.session_id != exclude)
    {
        return Ok(overview.record.session_id.clone());
    }
    let record = state
        .state_store
        .create_session(&persona, t("Terminal session", "终端集成会话"), "user", None)
        .map_err(|error| safe_error_message(&error))?;
    state.events.publish(
        "session.created",
        json!({ "session_id": record.session_id, "name": record.name }),
    );
    Ok(record.session_id)
}

pub(crate) async fn switch_session_via_actor(
    state: &DaemonState,
    session_id: String,
) -> std::result::Result<(), String> {
    reserve_admin_light(&state.manager).map_err(|error| error.message)?;
    let (reply, receiver) = oneshot::channel();
    if state
        .actor_tx
        .send(ActorCommand::SwitchSession {
            session_id,
            release_reservation: true,
            reply,
        })
        .is_err()
    {
        release_admin(&state.manager);
        return Err("GQY core worker is unavailable".to_string());
    }
    match receiver.await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(AdminFailure::Invalid(message) | AdminFailure::Internal(message))) => Err(message),
        Err(_) => {
            release_admin(&state.manager);
            Err("GQY core stopped while switching sessions".to_string())
        }
    }
}

pub(crate) async fn switch_session_via_actor_reserved(
    state: &DaemonState,
    session_id: String,
) -> std::result::Result<(), String> {
    let (reply, receiver) = oneshot::channel();
    if state
        .actor_tx
        .send(ActorCommand::SwitchSession {
            session_id,
            release_reservation: false,
            reply,
        })
        .is_err()
    {
        return Err("GQY core worker is unavailable".to_string());
    }
    match receiver.await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(AdminFailure::Invalid(message) | AdminFailure::Internal(message))) => Err(message),
        Err(_) => Err("GQY core stopped while switching sessions".to_string()),
    }
}

/// 普通人格 + dev 保留人格的本地会话合并,按更新时间排。WebUI 侧栏与
/// `gqy session` 管理面共用:mode 字段(session_record_json)区分分组。
pub(crate) fn sessions_with_dev(
    store: &StateStore,
    persona: &str,
) -> anyhow::Result<Vec<crate::state::SessionOverview>> {
    let mut rows = store.list_local_sessions(persona)?;
    if persona != crate::state::DEV_PERSONA {
        rows.extend(store.list_local_sessions(crate::state::DEV_PERSONA)?);
    }
    rows.sort_by(|a, b| b.record.updated_at.cmp(&a.record.updated_at));
    Ok(rows)
}

pub(crate) fn session_record_json(record: &crate::state::SessionRecord) -> Value {
    json!({
        "session_id": record.session_id,
        "name": record.name,
        "kind": record.kind,
        "workspace": record.workspace,
        "created_at": record.created_at,
        "updated_at": record.updated_at,
        // 会话模式由人格推导(创建时定死),列表/选择器靠它标注类型。
        "mode": if record.persona == crate::state::DEV_PERSONA { "dev" } else { "normal" },
    })
}

pub(crate) fn session_overview_json(overview: &crate::state::SessionOverview, current: &str) -> Value {
    let mut value = session_record_json(&overview.record);
    value["turn_count"] = json!(overview.turn_count);
    value["last_user_content"] = json!(overview.last_user_content);
    value["is_current"] = json!(overview.record.session_id == current);
    value
}

/// Resolves an optional turn-target session id: validates existence and that
/// it is a user or one-shot session; `None` falls back to the global current
/// session.
/// 会话模式创建时定死:dev 人格(DEV_PERSONA)会话永远 Dev,其余永远
/// Normal——客户端传什么都不构成中途切换路径。
pub(crate) fn turn_mode_for_session(
    store: &StateStore,
    session_id: &str,
    requested: AgentMode,
) -> AgentMode {
    match store.session_record(session_id) {
        Ok(Some(record)) if record.persona == crate::state::DEV_PERSONA => AgentMode::Dev,
        _ => {
            if requested == AgentMode::Dev {
                tracing::debug!(%session_id, "client asked for dev mode on a non-dev session; forcing normal");
            }
            AgentMode::Normal
        }
    }
}

pub(crate) fn resolve_turn_session(
    state: &DaemonState,
    session_id: Option<String>,
) -> std::result::Result<Arc<str>, String> {
    match session_id {
        None => Ok(state.state_store.session_id()),
        Some(session_id) => {
            let record = resolve_local_session_ref_with_kinds(
                state,
                &ipc::SessionRef::Id { id: session_id },
                TURN_TARGET_KINDS,
            )?;
            Ok(record.session_id.into())
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn handle_ipc_turn(
    state: &DaemonState,
    stream: &mut tokio::net::UnixStream,
    content: String,
    mode: String,
    images: Vec<Option<ImageAttachment>>,
    cwd: Option<std::path::PathBuf>,
    session_id: Option<String>,
    origin_tty: Option<crate::ipc::OriginTty>,
) -> Result<()> {
    let content = match validate_content(content) {
        Ok(content) => content,
        Err(error) => {
            ipc::send(stream, &IpcFrame::error(error.message)).await?;
            return Ok(());
        }
    };
    let mode = match parse_mode(&mode) {
        Ok(mode) => mode,
        Err(error) => {
            ipc::send(stream, &IpcFrame::error(error.message)).await?;
            return Ok(());
        }
    };
    // Turns run in parallel — several may be active at once, including in
    // the same session (placeholder semantics). The only rejection is a
    // transient admin mutation window.
    let run_id = random_id("run", 18);
    let session_id = match resolve_turn_session(state, session_id) {
        Ok(session_id) => session_id,
        // (mode 在会话解析后按会话记录强制,见下。)
        Err(message) => {
            ipc::send(stream, &IpcFrame::error(message)).await?;
            return Ok(());
        }
    };
    // 会话模式创建时定死:以会话记录为准强制,客户端传参只是遗留字段。
    let mode = turn_mode_for_session(&state.state_store, &session_id, mode);
    let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
    let busy = {
        let mut manager = state.manager.lock().unwrap();
        if manager.admin_busy {
            true
        } else {
            manager.active_runs.insert(
                run_id.clone(),
                RunInfo {
                    session_id: session_id.clone(),
                    mode,
                    audience: PromptAudience::Owner,
                    cancel: cancel_tx,
                    turn_id: None,
                    queue_target: None,
                    supersede: Arc::new(crate::agent::TurnSupersedeSignal::default()),
                    platform_followup: None,
                    operation: RunOperation::Create,
                    job_wake: false,
                    turn_origin: crate::tools::workspace::TurnOrigin::Human,
                job_wake_label: None,
                },
            );
            false
        }
    };
    if busy {
        ipc::send(
            stream,
            &IpcFrame::coded_error(ipc::ErrorCode::Busy, ipc::ADMIN_BUSY_MESSAGE),
        )
        .await?;
        return Ok(());
    }

    let after = state.events.latest_id();
    let mut subscription = state.events.subscribe_after(after);
    if state
        .actor_tx
        .send(ActorCommand::StartTurn {
            run_id: run_id.clone(),
            session_id,
            display_content: content.clone(),
            content,
            attachment_run_id: None,
            mode,
            images,
            cwd,
            origin_tty,
            audience: PromptAudience::Owner,
            profile: None,
            cancel: cancel_rx,
            turn_origin: Box::new(crate::tools::workspace::TurnOrigin::Human),
        })
        .is_err()
    {
        finish_run(&state.manager, &run_id, None);
        ipc::send(stream, &IpcFrame::error("GQY core worker is unavailable")).await?;
        return Ok(());
    }
    let mut run_guard = IpcRunGuard {
        manager: state.manager.clone(),
        run_id: run_id.clone(),
        finished: false,
    };
    ipc::send(
        stream,
        &IpcFrame::Accepted {
            run_id: run_id.clone(),
            turn_id: None,
        },
    )
    .await?;

    let mut last_id = after;
    loop {
        let record = if let Some(record) = subscription.pending.pop_front() {
            record
        } else {
            match subscription.receiver.recv().await {
                Ok(record) => record,
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    subscription.pending = state.events.replay_after(last_id);
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        };
        if record.kind == "resync_required" {
            ipc::send(
                stream,
                &IpcFrame::error("GQY core event history was exhausted; the turn was cancelled"),
            )
            .await?;
            break;
        }
        last_id = record.id;
        let Ok(data) = serde_json::from_str::<Value>(&record.data) else {
            continue;
        };
        if data.get("run_id").and_then(Value::as_str) != Some(run_id.as_str()) {
            continue;
        }
        let terminal = matches!(
            record.kind.as_str(),
            "run.completed" | "run.failed" | "run.cancelled"
        );
        ipc::send(
            stream,
            &IpcFrame::Event {
                id: record.id,
                kind: record.kind,
                data,
            },
        )
        .await?;
        if terminal {
            run_guard.finish();
            break;
        }
    }
    Ok(())
}


/// Attach a client to an already-running turn (background-command wake):
/// forwards its event frames until terminal, without owning the run.
pub(crate) async fn follow_run(
    state: &DaemonState,
    stream: &mut tokio::net::UnixStream,
    run_id: String,
) -> Result<()> {
    let mut subscription = state.events.subscribe_after(state.events.latest_id());
    let run_state = {
        let manager = state.manager.lock().unwrap();
        manager
            .active_runs
            .get(&run_id)
            .map(|info| info.turn_id.clone())
    };
    let Some(turn_id) = run_state else {
        ipc::send(stream, &IpcFrame::error("run is not active")).await?;
        return Ok(());
    };
    ipc::send(
        stream,
        &IpcFrame::Accepted {
            run_id: run_id.clone(),
            turn_id,
        },
    )
    .await?;
    let mut last_id = 0u64;
    loop {
        let record = if let Some(record) = subscription.pending.pop_front() {
            record
        } else {
            match subscription.receiver.recv().await {
                Ok(record) => record,
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    subscription.pending = state.events.replay_after(last_id);
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        };
        if record.kind == "resync_required" {
            ipc::send(
                stream,
                &IpcFrame::error("GQY core event history was exhausted"),
            )
            .await?;
            break;
        }
        last_id = record.id;
        let Ok(data) = serde_json::from_str::<Value>(&record.data) else {
            continue;
        };
        if data.get("run_id").and_then(Value::as_str) != Some(run_id.as_str()) {
            // The run may have finished before we saw a frame; stop when it
            // is no longer active and nothing more will arrive for it.
            if !state.manager.lock().unwrap().active_runs.contains_key(&run_id) {
                break;
            }
            continue;
        }
        let terminal = matches!(
            record.kind.as_str(),
            "run.completed" | "run.failed" | "run.cancelled"
        );
        ipc::send(
            stream,
            &IpcFrame::Event {
                id: record.id,
                kind: record.kind,
                data,
            },
        )
        .await?;
        if terminal {
            break;
        }
    }
    Ok(())
}

pub(crate) fn router(state: DaemonState) -> Router {
    Router::new()
        .route("/", get(index_asset))
        .route("/styles.css", get(styles_asset))
        .route("/theme.css", get(theme_css))
        .route("/app.js", get(app_asset))
        .route("/vendor/katex/katex.min.js", get(katex_js_asset))
        .route("/vendor/katex/katex.min.css", get(katex_css_asset))
        .route("/vendor/katex/fonts/{font}", get(katex_font_asset))
        .route("/api/media", get(media_stream))
        .route("/assets/gqy-logo.png", get(logo_asset))
        .route("/assets/gqywallpaper.png", get(wallpaper_asset))
        .route("/api/health", get(health))
        .route("/api/auth/login", post(auth_login))
        .route("/api/bootstrap", get(bootstrap))
        .route("/api/persona/avatar", get(persona_avatar))
        .route(
            "/api/persona/assets",
            post(upload_persona_asset).layer(DefaultBodyLimit::max(PERSONA_ASSET_LIMIT)),
        )
        .route("/api/config", get(get_config).put(update_config))
        .route(
            "/api/qq-group-management/history",
            get(qq_group_history_http),
        )
        .route(
            "/api/qq-group-management/history/clear",
            post(qq_group_history_clear_http),
        )
        .route(
            "/api/qq-group-management/offenders/{user_id}",
            delete(qq_group_offender_delete_http),
        )
        .route("/api/events", get(events))
        .route("/api/assets/{asset_id}", get(image_asset))
        .route("/api/artifacts/{asset_id}", get(artifact_asset))
        .route(
            "/api/attachments",
            post(upload_user_attachment).layer(DefaultBodyLimit::max(ATTACHMENT_BODY_LIMIT)),
        )
        .route(
            "/api/attachments/{attachment_id}",
            get(user_attachment).delete(delete_user_attachment),
        )
        .route(
            "/api/platform-assets/{token}",
            get(platforms::platform_asset),
        )
        .route(
            "/api/sessions",
            get(list_sessions_http).post(create_session_http),
        )
        .route(
            "/api/sessions/{session_id}",
            patch(update_session_http).delete(delete_session_http),
        )
        .route("/api/sessions/{session_id}/turns", get(session_turns_http))
        .route(
            "/api/sessions/{session_id}/models",
            get(get_session_models_http).put(set_session_models_http),
        )
        .route(
            "/api/sessions/{session_id}/turns/{turn_id}/redo",
            post(redo_turn),
        )
        .route("/api/turns", post(create_turn))
        .route("/api/queue", post(queue_prompt))
        .route(
            "/api/runs/{run_id}/turns/{turn_id}/queue/{prompt_id}",
            delete(remove_queue_prompt),
        )
        .route("/api/runs/{run_id}/cancel", post(cancel_run))
        .route("/api/questions/{question_id}", delete(close_question))
        .route("/api/questions/{question_id}/answer", post(answer_question))
        .route("/api/models/active", put(set_models))
        .route(
            "/api/models/thinking-variants",
            get(get_thinking_variants).put(set_thinking_variants),
        )
        .route("/api/conversation/reset", post(reset_conversation))
        .route("/api/jobs", get(list_jobs_http))
        .route("/api/usage/stats", get(usage_stats_web))
        .route("/api/usage/details", get(usage_details_web))
        .route("/api/jobs/{job_id}", delete(stop_job_http))
        // OneBot v11 reverse-WS endpoint: NapCat connects here as a WS
        // client. Gated by platforms.qq config, not web auth.
        .route("/ws", get(platforms::onebot::onebot_ws_on_web_port))
        // Backward-compatible endpoint used by earlier GQY releases.
        .route(
            "/onebot/v11/ws",
            get(platforms::onebot::onebot_ws_on_web_port),
        )
        .layer(DefaultBodyLimit::max(JSON_BODY_LIMIT))
        .with_state(state)
}

/// Strong validator shared by all build-embedded assets: the BUILD_ID
/// changes on any frontend edit (build.rs rerun triggers), so a 304 can
/// never pin a stale file.
pub(crate) fn build_etag() -> &'static HeaderValue {
    pub(crate) static ETAG_VALUE: std::sync::LazyLock<HeaderValue> = std::sync::LazyLock::new(|| {
        HeaderValue::from_str(concat!("\"", env!("GQY_BUILD_ID"), "\""))
            .expect("build id forms a valid header value")
    });
    &ETAG_VALUE
}

pub(crate) fn embedded_asset(
    headers: &HeaderMap,
    content: &'static [u8],
    content_type: &'static str,
) -> Response {
    if headers
        .get(axum::http::header::IF_NONE_MATCH)
        .is_some_and(|value| value == build_etag())
    {
        let mut response = StatusCode::NOT_MODIFIED.into_response();
        response
            .headers_mut()
            .insert(axum::http::header::ETAG, build_etag().clone());
        return response;
    }
    let mut response = finish_asset_response(content.into_response(), content_type);
    response
        .headers_mut()
        .insert(axum::http::header::ETAG, build_etag().clone());
    response
}

pub(crate) async fn index_asset(headers: HeaderMap) -> Response {
    // Version the asset references so browsers and intermediaries can never
    // serve a stale app.js/styles.css after an upgrade.
    pub(crate) static VERSIONED_INDEX: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
        INDEX_HTML
            .replace("href=\"/styles.css\"", concat!("href=\"/styles.css?v=", env!("GQY_BUILD_ID"), "\""))
            .replace("src=\"/app.js\"", concat!("src=\"/app.js?v=", env!("GQY_BUILD_ID"), "\""))
            .replace(
                "href=\"/vendor/katex/katex.min.css\"",
                concat!("href=\"/vendor/katex/katex.min.css?v=", env!("GQY_BUILD_ID"), "\""),
            )
            .replace(
                "src=\"/vendor/katex/katex.min.js\"",
                concat!("src=\"/vendor/katex/katex.min.js?v=", env!("GQY_BUILD_ID"), "\""),
            )
    });
    embedded_asset(&headers, VERSIONED_INDEX.as_bytes(), "text/html; charset=utf-8")
}

pub(crate) async fn styles_asset(headers: HeaderMap) -> Response {
    embedded_asset(&headers, STYLES_CSS.as_bytes(), "text/css; charset=utf-8")
}

/// Optional MD3 token override generated by matugen from the wallpaper.
/// Read from disk on every request (the file is tiny and regenerated at any
/// time); 404 when absent so the WebUI falls back to the built-in palette.
pub(crate) async fn theme_css(State(state): State<DaemonState>) -> Response {
    let path = state.paths.config_dir.join("webui-theme.css");
    match tokio::fs::read(&path).await {
        Ok(bytes) => finish_asset_response(bytes.into_response(), "text/css; charset=utf-8"),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

pub(crate) async fn app_asset(headers: HeaderMap) -> Response {
    embedded_asset(&headers, APP_JS.as_bytes(), "application/javascript; charset=utf-8")
}

pub(crate) async fn logo_asset(headers: HeaderMap) -> Response {
    embedded_asset(&headers, GQY_LOGO, "image/png")
}

#[derive(Deserialize)]
pub(crate) struct MediaQuery {
    path: String,
}

/// 视频扩展名 → MIME。清单外一律拒绝:这个端点只做媒体流,
/// 不做通用文件下载器(尽管登录态本就有 read_file 同级能力)。
pub(crate) fn media_mime(path: &std::path::Path) -> Option<&'static str> {
    match path
        .extension()
        .and_then(|ext| ext.to_str())?
        .to_ascii_lowercase()
        .as_str()
    {
        "mp4" | "m4v" => Some("video/mp4"),
        "webm" => Some("video/webm"),
        "mov" => Some("video/quicktime"),
        "mkv" => Some("video/x-matroska"),
        "ogv" | "ogg" => Some("video/ogg"),
        "mp3" => Some("audio/mpeg"),
        "flac" => Some("audio/flac"),
        "wav" => Some("audio/wav"),
        "m4a" => Some("audio/mp4"),
        "opus" => Some("audio/ogg"),
        _ => None,
    }
}

/// 解析 `Range: bytes=start-end`(单段)。返回 (start, inclusive_end)。
pub(crate) fn parse_byte_range(value: &str, total: u64) -> Option<(u64, u64)> {
    let spec = value.trim().strip_prefix("bytes=")?.split(',').next()?.trim();
    let (start, end) = spec.split_once('-')?;
    if start.is_empty() {
        // 后缀形式 bytes=-N:最后 N 字节
        let suffix: u64 = end.parse().ok()?;
        if suffix == 0 || total == 0 {
            return None;
        }
        return Some((total.saturating_sub(suffix), total - 1));
    }
    let start: u64 = start.parse().ok()?;
    let end: u64 = if end.is_empty() { total.saturating_sub(1) } else { end.parse().ok()? };
    (start <= end && start < total).then(|| (start, end.min(total.saturating_sub(1))))
}

/// 本地媒体流:登录态可播放本机音视频文件,带 HTTP Range(拖进度条)。
pub(crate) async fn media_stream(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Query(query): Query<MediaQuery>,
) -> std::result::Result<Response, ApiError> {
    require_auth(&headers, &state)?;
    let raw = if let Some(rest) = query.path.strip_prefix("~/") {
        let home = std::env::var_os("HOME")
            .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "media not found"))?;
        std::path::Path::new(&home).join(rest)
    } else {
        std::path::PathBuf::from(&query.path)
    };
    let path = tokio::fs::canonicalize(&raw)
        .await
        .map_err(|_| ApiError::new(StatusCode::NOT_FOUND, "media not found"))?;
    let mime = media_mime(&path)
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "unsupported media type"))?;
    let metadata = tokio::fs::metadata(&path)
        .await
        .map_err(|_| ApiError::new(StatusCode::NOT_FOUND, "media not found"))?;
    if !metadata.is_file() {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "media not found"));
    }
    let total = metadata.len();
    let range = headers
        .get(axum::http::header::RANGE)
        .and_then(|value| value.to_str().ok())
        .map(|value| parse_byte_range(value, total));
    let mut file = tokio::fs::File::open(&path)
        .await
        .map_err(|_| ApiError::new(StatusCode::NOT_FOUND, "media not found"))?;

    let (status, start, end) = match range {
        None => (StatusCode::OK, 0, total.saturating_sub(1)),
        Some(Some((start, end))) => (StatusCode::PARTIAL_CONTENT, start, end),
        Some(None) => {
            let mut response = StatusCode::RANGE_NOT_SATISFIABLE.into_response();
            response.headers_mut().insert(
                axum::http::header::CONTENT_RANGE,
                HeaderValue::from_str(&format!("bytes */{total}")).unwrap(),
            );
            return Ok(response);
        }
    };
    let length = if total == 0 { 0 } else { end - start + 1 };
    use tokio::io::AsyncSeekExt;
    file.seek(std::io::SeekFrom::Start(start))
        .await
        .map_err(ApiError::internal)?;
    let stream = tokio_util::io::ReaderStream::new(tokio::io::AsyncReadExt::take(file, length));
    let mut response = Response::new(axum::body::Body::from_stream(stream));
    *response.status_mut() = status;
    let response_headers = response.headers_mut();
    response_headers.insert(CONTENT_TYPE, HeaderValue::from_static(mime));
    response_headers.insert(CONTENT_LENGTH, HeaderValue::from(length));
    response_headers.insert(
        axum::http::header::ACCEPT_RANGES,
        HeaderValue::from_static("bytes"),
    );
    if status == StatusCode::PARTIAL_CONTENT {
        response_headers.insert(
            axum::http::header::CONTENT_RANGE,
            HeaderValue::from_str(&format!("bytes {start}-{end}/{total}")).unwrap(),
        );
    }
    response_headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

pub(crate) async fn katex_js_asset(headers: HeaderMap) -> Response {
    embedded_asset(&headers, KATEX_JS.as_bytes(), "text/javascript; charset=utf-8")
}

pub(crate) async fn katex_css_asset(headers: HeaderMap) -> Response {
    embedded_asset(&headers, KATEX_CSS.as_bytes(), "text/css; charset=utf-8")
}

pub(crate) async fn katex_font_asset(headers: HeaderMap, Path(font): Path<String>) -> Response {
    match KATEX_FONTS.iter().find(|(name, _)| *name == font) {
        Some((_, bytes)) => embedded_asset(&headers, bytes, "font/woff2"),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

pub(crate) async fn wallpaper_asset(headers: HeaderMap) -> Response {
    embedded_asset(&headers, GQY_WALLPAPER, "image/png")
}

pub(crate) async fn persona_avatar(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Query(query): Query<std::collections::HashMap<String, String>>,
) -> std::result::Result<Response, ApiError> {
    require_auth(&headers, &state)?;
    let (config, prompts) = {
        let manager = state.manager.lock().unwrap();
        let prompts =
            read_prompt_documents(&manager.config, &state.paths).map_err(ApiError::internal)?;
        (manager.config.clone(), prompts)
    };
    let path = if let Some(path) = query.get("path").filter(|p| !p.is_empty()) {
        managed_persona_asset_path(&state.paths, path).ok_or_else(|| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid managed persona asset path",
            )
        })?
    } else if query.contains_key("board") {
        active_persona_board_path(&config, &prompts, &state.paths)
            .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "persona board image not found"))?
    } else if let Some(path) = active_persona_avatar_path(&config, &prompts, &state.paths) {
        path
    } else {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "persona avatar not found",
        ));
    };
    if path.starts_with(state.paths.persona_avatars_dir()) {
        validate_managed_persona_asset_file(&state.paths, &path)
            .map_err(|_| ApiError::new(StatusCode::NOT_FOUND, "persona avatar not found"))?;
    }
    let bytes = tokio::fs::read(&path)
        .await
        .map_err(|_| ApiError::new(StatusCode::NOT_FOUND, "persona avatar not found"))?;
    if bytes.len() > 8 * 1024 * 1024 {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "persona avatar is too large",
        ));
    }
    let format = image::guess_format(&bytes)
        .map_err(|_| ApiError::new(StatusCode::NOT_FOUND, "persona avatar is not an image"))?;
    let mime = match format {
        image::ImageFormat::Png => "image/png",
        image::ImageFormat::Jpeg => "image/jpeg",
        image::ImageFormat::Gif => "image/gif",
        image::ImageFormat::WebP => "image/webp",
        image::ImageFormat::Bmp => "image/bmp",
        _ => {
            return Err(ApiError::new(
                StatusCode::NOT_FOUND,
                "persona avatar format is unsupported",
            ))
        }
    };
    let mut response = bytes.into_response();
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static(mime));
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    response
        .headers_mut()
        .insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    Ok(response)
}

pub(crate) async fn upload_persona_asset(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    body: Bytes,
) -> std::result::Result<Json<Value>, ApiError> {
    require_mutation(&headers, &state)?;
    if body.is_empty() {
        return Err(ApiError::new(StatusCode::BAD_REQUEST, "image is empty"));
    }
    if body.len() > PERSONA_ASSET_LIMIT {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "persona image is too large",
        ));
    }
    let format = image::guess_format(&body)
        .map_err(|_| ApiError::new(StatusCode::BAD_REQUEST, "unsupported image format"))?;
    let extension = match format {
        image::ImageFormat::Png => "png",
        image::ImageFormat::Jpeg => "jpg",
        image::ImageFormat::Gif => "gif",
        image::ImageFormat::WebP => "webp",
        image::ImageFormat::Bmp => "bmp",
        _ => {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "unsupported image format",
            ))
        }
    };
    let hash = format!("{:x}", Sha256::digest(&body));
    let relative = format!("persona-avatars/{hash}.{extension}");
    let directory = state.paths.persona_avatars_dir();
    let destination = directory.join(format!("{hash}.{extension}"));
    tokio::fs::create_dir_all(&directory)
        .await
        .map_err(ApiError::internal)?;
    let directory_metadata = tokio::fs::symlink_metadata(&directory)
        .await
        .map_err(ApiError::internal)?;
    if directory_metadata.file_type().is_symlink() || !directory_metadata.is_dir() {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "persona asset directory is unsafe",
        ));
    }
    store_persona_asset(&directory, &destination, &hash, &body).await?;
    let config = state.manager.lock().unwrap().config.clone();
    if let Ok(prompts) = read_prompt_documents(&config, &state.paths) {
        cleanup_persona_assets(&state.paths, &prompts, &prompts);
    }
    Ok(Json(json!({
        "path": relative,
        "preview_url": format!("/api/persona/avatar?path={relative}"),
    })))
}

pub(crate) async fn store_persona_asset(
    directory: &FilePath,
    destination: &FilePath,
    expected_hash: &str,
    body: &[u8],
) -> std::result::Result<(), ApiError> {
    let replace_corrupt = match tokio::fs::symlink_metadata(destination).await {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            match verify_persona_asset_hash(destination, expected_hash).await {
                Ok(()) => return Ok(()),
                Err(error) if error.status == StatusCode::CONFLICT => true,
                Err(error) => return Err(error),
            }
        }
        Ok(_) => {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "persona asset destination is unsafe",
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => return Err(ApiError::internal(error)),
    };

    let temporary = directory.join(format!(
        ".upload-{}-{:016x}",
        std::process::id(),
        rand::random::<u64>()
    ));
    let mut file = tokio::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)
        .await
        .map_err(ApiError::internal)?;
    let write_result = async {
        file.write_all(body).await?;
        file.sync_all().await?;
        if replace_corrupt {
            tokio::fs::rename(&temporary, destination).await
        } else {
            tokio::fs::hard_link(&temporary, destination).await
        }
    }
    .await;
    match write_result {
        Ok(()) => {
            let _ = tokio::fs::remove_file(&temporary).await;
            let directory = tokio::fs::File::open(directory)
                .await
                .map_err(ApiError::internal)?;
            directory.sync_all().await.map_err(ApiError::internal)?;
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let _ = tokio::fs::remove_file(&temporary).await;
            verify_persona_asset_hash(destination, expected_hash).await
        }
        Err(error) => {
            let _ = tokio::fs::remove_file(&temporary).await;
            Err(ApiError::internal(error))
        }
    }
}

pub(crate) async fn verify_persona_asset_hash(
    path: &FilePath,
    expected_hash: &str,
) -> std::result::Result<(), ApiError> {
    let bytes = tokio::fs::read(path).await.map_err(ApiError::internal)?;
    if bytes.len() > PERSONA_ASSET_LIMIT || format!("{:x}", Sha256::digest(&bytes)) != expected_hash
    {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "persona asset cache entry is corrupted",
        ));
    }
    Ok(())
}

pub(crate) fn text_asset(content: &'static str, content_type: &'static str) -> Response {
    asset_response(content.as_bytes(), content_type)
}

pub(crate) fn binary_asset(content: &'static [u8], content_type: &'static str) -> Response {
    asset_response(content, content_type)
}

pub(crate) fn asset_response(content: &'static [u8], content_type: &'static str) -> Response {
    finish_asset_response(content.into_response(), content_type)
}

pub(crate) fn finish_asset_response(mut response: Response, content_type: &'static str) -> Response {
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static(content_type));
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    response
        .headers_mut()
        .insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    response.headers_mut().insert(
        CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'self'; img-src 'self'; media-src 'self' https: http:; style-src 'self'; script-src 'self'; connect-src 'self'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'",
        ),
    );
    response
        .headers_mut()
        .insert(REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    response
}

pub(crate) async fn auth_login(
    State(state): State<DaemonState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(request): Json<LoginRequest>,
) -> std::result::Result<Response, ApiError> {
    if !origin_is_allowed(&headers) {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "request origin is not allowed",
        ));
    }
    if !state.auth.required() {
        return Ok(StatusCode::NO_CONTENT.into_response());
    }
    if request.password.chars().count() > 1_024 {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "password is too long",
        ));
    }
    let session = match state.auth.login(peer.ip(), &request.password) {
        Ok(session) => session,
        Err(LoginFailure::Invalid) => {
            return Err(ApiError::new(StatusCode::UNAUTHORIZED, "invalid password"));
        }
        Err(LoginFailure::RateLimited) => {
            let mut response = ApiError::new(
                StatusCode::TOO_MANY_REQUESTS,
                "too many login attempts; try again shortly",
            )
            .into_response();
            response
                .headers_mut()
                .insert(RETRY_AFTER, HeaderValue::from_static("60"));
            return Ok(response);
        }
    };
    let cookie =
        format!("{AUTH_COOKIE}={session}; HttpOnly; SameSite=Strict; Path=/; Max-Age=86400");
    let mut response = StatusCode::NO_CONTENT.into_response();
    response.headers_mut().insert(
        SET_COOKIE,
        HeaderValue::from_str(&cookie).map_err(ApiError::internal)?,
    );
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

pub(crate) fn resolve_web_password(args: &WebArgs) -> Result<Option<String>> {
    let password = if let Some(path) = &args.password_file {
        let contents = std::fs::read_to_string(path)
            .with_context(|| format!("reading WebUI password file: {}", path.display()))?;
        Some(contents.trim_end_matches(['\r', '\n']).to_string())
    } else {
        match &args.password {
            Some(password) if !password.is_empty() => Some(password.clone()),
            Some(_) if io::stdin().is_terminal() => {
                Some(rpassword::prompt_password("WebUI password: ")?)
            }
            Some(_) => {
                anyhow::bail!("-p requires an interactive terminal or an explicit password value")
            }
            None => None,
        }
    };
    if let Some(password) = &password {
        if password.is_empty() {
            anyhow::bail!("WebUI password cannot be empty");
        }
        if password.chars().count() > 1_024 {
            anyhow::bail!("WebUI password cannot exceed 1,024 characters");
        }
    }
    Ok(password)
}

pub(crate) async fn health() -> Json<Value> {
    Json(json!({
        "status": "ready",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

pub(crate) async fn bootstrap(
    State(state): State<DaemonState>,
    headers: HeaderMap,
) -> std::result::Result<Response, ApiError> {
    require_auth(&headers, &state)?;
    let metadata_config = state.manager.lock().unwrap().config.clone();
    crate::models_cache::ensure_active_metadata(&state.paths, &metadata_config);
    state
        .state_store
        .recover_stale_turns()
        .map_err(ApiError::internal)?;
    let current_session = state.state_store.session_id();
    let (config, active_run_id, runs, context) = {
        let manager = state.manager.lock().unwrap();
        let runs: Vec<Value> = manager
            .active_runs
            .iter()
            .map(|(run_id, info)| {
                json!({
                    "run_id": run_id,
                    "session_id": &*info.session_id,
                    "mode": mode_name(info.mode),
                    "operation": info.operation.name(),
                    "turn_id": info.operation.turn_id(),
                    "input_id": info.operation.input_id(),
                })
            })
            .collect();
        (
            manager.config.clone(),
            manager.run_in_session(&current_session).cloned(),
            runs,
            manager.context,
        )
    };
    let running_target = state
        .state_store
        .running_turn_queue_target()
        .map_err(ApiError::internal)?;
    let external_target = active_run_id
        .is_none()
        .then_some(running_target.as_ref())
        .flatten();
    let mut assets_by_turn = HashMap::<String, Vec<ImageAsset>>::new();
    for asset in state
        .state_store
        .load_image_assets()
        .map_err(ApiError::internal)?
    {
        assets_by_turn
            .entry(asset.turn_id.clone())
            .or_default()
            .push(asset);
    }
    let mut artifacts_by_turn = HashMap::<String, Vec<ArtifactAsset>>::new();
    for artifact in state
        .state_store
        .load_artifact_assets()
        .map_err(ApiError::internal)?
    {
        artifacts_by_turn
            .entry(artifact.turn_id.clone())
            .or_default()
            .push(artifact);
    }
    let turns = state
        .state_store
        .load_turns()
        .map_err(ApiError::internal)?
        .into_iter()
        .filter(|turn| !turn.is_summary)
        .map(|turn| {
            let assets = assets_by_turn.remove(&turn.turn_id).unwrap_or_default();
            let artifacts = artifacts_by_turn.remove(&turn.turn_id).unwrap_or_default();
            SafeTurn::from_turn(turn, assets, artifacts)
        })
        .collect();
    let usage = state
        .state_store
        .usage_snapshot()
        .map_err(ApiError::internal)?
        .into();
    let queued_prompts = match external_target {
        Some(target) => state
            .state_store
            .load_queued_prompts_for_target(target)
            .map_err(ApiError::internal)?,
        None => state
            .state_store
            .load_queued_prompts()
            .map_err(ApiError::internal)?,
    }
    .into_iter()
    .map(SafeQueuedPrompt::from)
    .collect();
    let running_turn_id = running_target.as_ref().map(|target| target.turn_id.clone());
    let external_queue_available = external_target
        .is_some_and(|target| target.queue_session_id.is_some() && target.owner_pid.is_some());
    let current_session_id = state.state_store.session_id().to_string();
    let sessions = sessions_with_dev(&state.state_store, &config.active_persona_scope())
        .map_err(ApiError::internal)?
        .iter()
        .map(|overview| session_overview_json(overview, &current_session_id))
        .collect();
    let persona = persona_identity(
        &config,
        &read_prompt_documents(&config, &state.paths).map_err(ApiError::internal)?,
    );
    let redo_candidate = if active_run_id.is_none() {
        state
            .state_store
            .redo_candidate()
            .map_err(ApiError::internal)?
            .map(SafeRedoCandidate::from)
    } else {
        None
    };
    let mut response = Json(BootstrapResponse {
        version: env!("CARGO_PKG_VERSION"),
        boot_id: state.boot_id.to_string(),
        latest_event_id: state.events.latest_id(),
        active_run_id,
        running_turn_id,
        external_queue_available,
        turns,
        queued_prompts,
        models: safe_models(&config),
        display: web_display_config(&config),
        context,
        usage,
        capabilities: Capabilities {
            multi_conversation: true,
            attachments: true,
            queue: true,
            redo: true,
        },
        sessions,
        current_session_id,
        runs,
        persona,
        redo_candidate,
    })
    .into_response();
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

