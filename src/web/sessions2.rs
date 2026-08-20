//! sessions2 — 自 src/web.rs 拆分。

// ponytail: 与 sessions_a.rs/sessions_b.rs 同属一次拆分但后缀风格混用（a/b vs 2）；
// 统一后缀（如全改 sessions2/3/4）后删除本注释。
#![allow(clippy::too_many_arguments)]
pub(crate) use super::*;

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
    let sessions = sessions_with_dev(&state.state_store, &persona).map_err(ApiError::internal)?;
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
    pub(crate) session_id: Option<String>,
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
pub(crate) fn fallback_session_id(
    state: &DaemonState,
    exclude: &str,
) -> std::result::Result<String, String> {
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
        .create_session(
            &persona,
            t("Terminal session", "终端集成会话"),
            "user",
            None,
        )
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

pub(crate) fn session_overview_json(
    overview: &crate::state::SessionOverview,
    current: &str,
) -> Value {
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
            if !state
                .manager
                .lock()
                .unwrap()
                .active_runs
                .contains_key(&run_id)
            {
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
