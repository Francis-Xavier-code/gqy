//! sessions_a — 自 src/web.rs 拆分。

use super::*;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LoginRequest {
    password: String,
}

pub(crate) const QQ_GROUP_MANAGEMENT_PLUGIN_ID: &str = "qq_group_management";
pub(crate) const QQ_GROUP_MANAGEMENT_PLATFORM: &str = "onebot";

#[derive(Deserialize)]
pub(crate) struct QqGroupHistoryQuery {
    account_id: String,
    group_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct QqGroupHistoryClearRequest {
    account_id: String,
    group_id: String,
    kind: String,
}

pub(crate) fn qq_group_scope(
    account_id: &str,
    group_id: &str,
) -> std::result::Result<PlatformPluginScopeKey, ApiError> {
    if !valid_qq_id(account_id) || !valid_qq_id(group_id) {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "account_id and group_id must be numeric QQ ids",
        ));
    }
    Ok(PlatformPluginScopeKey {
        plugin_id: QQ_GROUP_MANAGEMENT_PLUGIN_ID.to_string(),
        platform: QQ_GROUP_MANAGEMENT_PLATFORM.to_string(),
        account_id: account_id.to_string(),
        conversation_kind: "group".to_string(),
        conversation_id: group_id.to_string(),
    })
}

pub(crate) fn valid_qq_id(value: &str) -> bool {
    (5..=12).contains(&value.len()) && value.bytes().all(|byte| byte.is_ascii_digit())
}

pub(crate) async fn qq_group_history_http(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Query(query): Query<QqGroupHistoryQuery>,
) -> std::result::Result<Response, ApiError> {
    require_auth(&headers, &state)?;
    let scope = qq_group_scope(&query.account_id, &query.group_id)?;
    let offenders = state
        .state_store
        .plugin_get_json::<Value>(&scope, "offender_history")
        .map_err(ApiError::internal)?
        .unwrap_or_else(|| json!({}));
    let kicks = state
        .state_store
        .plugin_get_json::<Value>(&scope, "kick_history")
        .map_err(ApiError::internal)?
        .unwrap_or_else(|| json!([]));
    let connected_accounts = state
        .platforms
        .onebot
        .lock()
        .unwrap()
        .connected_accounts()
        .into_iter()
        .map(|account| account.to_string())
        .collect::<Vec<_>>();
    Ok(Json(json!({
        "account_id": query.account_id,
        "group_id": query.group_id,
        "offenders": offenders.clone(),
        "kicks": kicks.clone(),
        "offender_history": offenders,
        "kick_history": kicks,
        "connected_accounts": connected_accounts,
    }))
    .into_response())
}

pub(crate) async fn qq_group_history_clear_http(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Json(request): Json<QqGroupHistoryClearRequest>,
) -> std::result::Result<Response, ApiError> {
    require_mutation(&headers, &state)?;
    let scope = qq_group_scope(&request.account_id, &request.group_id)?;
    let key = match request.kind.as_str() {
        "offenders" => "offender_history",
        "kicks" => "kick_history",
        _ => {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "kind must be offenders or kicks",
            ))
        }
    };
    state
        .state_store
        .plugin_delete_key(&scope, key)
        .map_err(ApiError::internal)?;
    Ok(Json(json!({ "ok": true })).into_response())
}

pub(crate) async fn qq_group_offender_delete_http(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Path(user_id): Path<String>,
    Query(query): Query<QqGroupHistoryQuery>,
) -> std::result::Result<Response, ApiError> {
    require_mutation(&headers, &state)?;
    if !valid_qq_id(&user_id) {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "user_id must be a numeric QQ id",
        ));
    }
    let scope = qq_group_scope(&query.account_id, &query.group_id)?;
    state
        .state_store
        .plugin_update_json::<HashMap<String, Value>, _>(&scope, "offender_history", |current| {
            let mut records = current.unwrap_or_default();
            records.remove(&user_id);
            Ok(if records.is_empty() {
                None
            } else {
                Some(records)
            })
        })
        .map_err(ApiError::internal)?;
    Ok(Json(json!({ "ok": true })).into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UpdateConfigRequest {
    config: Value,
    #[serde(default)]
    secrets: HashMap<String, SecretMutation>,
    prompts: PromptDocuments,
    #[serde(default)]
    reset_conversation: bool,
}

#[derive(Deserialize)]
#[serde(tag = "action", content = "value", rename_all = "snake_case")]
pub(crate) enum SecretMutation {
    Set(String),
    Clear,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PromptDocuments {
    #[serde(default)]
    personas: Vec<PromptDocument>,
    #[serde(default)]
    identities: Vec<PromptDocument>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PromptDocument {
    name: String,
    content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    avatar_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    board_image_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    board_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    board_subtitle: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    starter_prompts: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    original_name: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub(crate) struct PersonaMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    avatar_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    board_image_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    board_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    board_subtitle: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    starter_prompts: Option<Vec<String>>,
}

#[derive(Serialize)]
pub(crate) struct ConfigResponse {
    config: Value,
    secret_states: HashMap<String, bool>,
    prompts: PromptDocuments,
    models: Vec<SafeModel>,
    multimodal_models: Vec<SafeModel>,
    display: WebDisplayConfig,
    context: ContextSnapshot,
    persona: PersonaIdentity,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct PersonaIdentity {
    name: String,
    avatar_url: Option<String>,
    board_image_url: Option<String>,
    board_title: String,
    board_subtitle: String,
    starter_prompts: Vec<String>,
}

#[derive(Serialize)]
pub(crate) struct BootstrapResponse {
    version: &'static str,
    boot_id: String,
    latest_event_id: u64,
    active_run_id: Option<String>,
    running_turn_id: Option<String>,
    external_queue_available: bool,
    turns: Vec<SafeTurn>,
    queued_prompts: Vec<SafeQueuedPrompt>,
    models: Vec<SafeModel>,
    display: WebDisplayConfig,
    context: ContextSnapshot,
    usage: SafeUsageSnapshot,
    capabilities: Capabilities,
    sessions: Vec<Value>,
    current_session_id: String,
    /// Every turn currently running, across all sessions.
    runs: Vec<Value>,
    persona: PersonaIdentity,
    redo_candidate: Option<SafeRedoCandidate>,
}

#[derive(Serialize)]
pub(crate) struct Capabilities {
    multi_conversation: bool,
    attachments: bool,
    queue: bool,
    redo: bool,
}

#[derive(Clone, Serialize)]
pub(crate) struct WebDisplayConfig {
    reasoning: String,
    tool_calls: String,
    readable_tool_names: bool,
    command_output_lines: usize,
    mixed_model_endpoint_display: String,
    show_mixed_model_endpoint: bool,
}

#[derive(Clone, Serialize)]
pub(crate) struct SafeQueuedPrompt {
    id: String,
    content: String,
    submitted_at: String,
    attachments: Vec<SafeUserAttachment>,
}

#[derive(Serialize)]
pub(crate) struct SafeModel {
    provider_id: String,
    provider_name: String,
    model: String,
    active: bool,
}

#[derive(Serialize)]
pub(crate) struct SafeTurn {
    id: String,
    seq: i64,
    status: &'static str,
    active_context: bool,
    user_content: String,
    assistant_content: String,
    assistant_reasoning: Option<String>,
    provider_id: Option<String>,
    model: Option<String>,
    user_timestamp: String,
    assistant_timestamp: Option<String>,
    token_total: u64,
    token_prompt: u64,
    token_cache_read: u64,
    token_usage_estimated: bool,
    question_exchanges: Vec<crate::question::QuestionExchange>,
    followups: Vec<SafeFollowup>,
    assets: Vec<SafeImageAsset>,
    artifacts: Vec<SafeArtifactAsset>,
    attachments: Vec<SafeUserAttachment>,
    revision: i64,
}

#[derive(Serialize)]
pub(crate) struct SafeRedoCandidate {
    turn_id: String,
    revision: i64,
    input_id: String,
    input_kind: &'static str,
    content: String,
}

impl From<crate::state::RedoCandidate> for SafeRedoCandidate {
    pub(crate) fn from(candidate: crate::state::RedoCandidate) -> Self {
        Self {
            turn_id: candidate.turn_id,
            revision: candidate.revision,
            input_id: candidate.input_id,
            input_kind: match candidate.input_kind {
                crate::state::RedoInputKind::Initial => "initial",
                crate::state::RedoInputKind::Followup => "followup",
            },
            content: candidate.display_content,
        }
    }
}

#[derive(Serialize)]
pub(crate) struct SafeFollowup {
    id: String,
    content: String,
    submitted_at: String,
    preceding_assistant_content: Option<String>,
    preceding_assistant_reasoning: Option<String>,
    provider_id: Option<String>,
    model: Option<String>,
    attachments: Vec<SafeUserAttachment>,
}

#[derive(Clone, Serialize)]
pub(crate) struct SafeUserAttachment {
    id: String,
    url: String,
    name: String,
    mime: String,
    kind: String,
    size: u64,
    width: u32,
    height: u32,
}

#[derive(Serialize)]
pub(crate) struct SafeImageAsset {
    id: String,
    url: String,
    mime: String,
    width: u32,
    height: u32,
    alt: String,
    hide_caption: bool,
}

#[derive(Clone, Serialize)]
pub(crate) struct SafeArtifactAsset {
    id: String,
    url: String,
    name: String,
    mime: String,
    kind: String,
    type_label: String,
    size: u64,
    updated_at: String,
}

#[derive(Serialize)]
pub(crate) struct SafeUsageSnapshot {
    requests: u64,
    prompt_tokens: u64,
    completion_tokens: u64,
    total_tokens: u64,
    conversation_tokens: u64,
    cache_read_tokens: u64,
    cache_write_tokens: u64,
    reasoning_tokens: u64,
    last_usage: Option<Usage>,
    last_conversation_usage: Option<Usage>,
}

#[derive(Serialize)]
pub(crate) struct ModelResponse {
    models: Vec<SafeModel>,
    display: WebDisplayConfig,
    context: ContextSnapshot,
}

#[derive(Serialize)]
pub(crate) struct ThinkingVariantsResponse {
    options: Vec<ThinkingVariantOptions>,
}

pub async fn run(paths: GQYPaths, args: WebArgs) -> Result<()> {
    let password = resolve_web_password(&args)?;
    AppConfig::init_files(&paths)?;
    let config = AppConfig::load_or_default(&paths)?;
    tools::jobs::init(&paths);
    let state_store = StateStore::new(&paths)?;
    state_store.init_files()?;
    let persona = config.active_persona_scope();
    state_store.adopt_sessions_for_persona(&persona)?;
    ensure_local_current_session(&state_store, &persona)?;
    // Subagent audit sessions are kept for a week, cleaned at startup and
    // then daily while the daemon runs. One-shot `ask` sessions delete
    // themselves as their turn ends, so the hour-old survivors swept here are
    // strictly orphans from a client that died mid-turn.
    pub(crate) const SUBAGENT_AUDIT_RETENTION_DAYS: i64 = 7;
    pub(crate) const ASK_SESSION_RETENTION_HOURS: i64 = 1;
    let _ = state_store.delete_subagent_sessions_older_than(SUBAGENT_AUDIT_RETENTION_DAYS);
    let _ = state_store.delete_ask_sessions_older_than(ASK_SESSION_RETENTION_HOURS);
    {
        let store = state_store.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(24 * 60 * 60));
            interval.tick().await;
            loop {
                interval.tick().await;
                let _ = store.delete_subagent_sessions_older_than(SUBAGENT_AUDIT_RETENTION_DAYS);
                let _ = store.delete_ask_sessions_older_than(ASK_SESSION_RETENTION_HOURS);
            }
        });
    }
    let context = cold_context(&config, &state_store)?;

    // Default binds all interfaces so the WebUI is reachable from the LAN;
    // `--bind 127.0.0.1` restricts it to this machine. Access URLs matching
    // the effective bind are printed below.
    let bind_ip = args.bind.unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED));
    let listener = match tokio::net::TcpListener::bind(SocketAddr::new(bind_ip, args.port)).await {
        Ok(listener) => listener,
        Err(error)
            if args.port == ipc::DEFAULT_WEB_PORT
                && error.kind() == std::io::ErrorKind::AddrInUse =>
        {
            tracing::warn!(
                requested_port = args.port,
                "{}",
                t(
                    "GQY WebUI default port is occupied; selecting an ephemeral port",
                    "GQY WebUI 默认端口已被占用；将选择临时端口"
                )
            );
            tokio::net::TcpListener::bind(SocketAddr::new(bind_ip, 0))
                .await
                .context("binding GQY WebUI to an ephemeral fallback port")?
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("binding GQY WebUI to {bind_ip}:{}", args.port));
        }
    };
    let port = listener.local_addr()?.port();
    let boot_id: Arc<str> = random_id("boot", 18).into();
    let events = EventHub::new();
    let questions = QuestionBroker::new();
    let manager = Arc::new(Mutex::new(ManagerState {
        config: config.clone(),
        active_runs: HashMap::new(),
        admin_busy: false,
        context,
        persona_session_ids: HashMap::from([(
            config.active_persona_scope(),
            state_store.session_id().to_string(),
        )]),
    }));
    let turn_engine = TurnEngineState::default();
    let memory_organizer = MemoryOrganizer::spawn()?;
    let memory_organizer_handle = memory_organizer.handle();
    memory_organizer_handle.wake(config.clone(), paths.clone(), state_store.clone());
    let (actor_tx, actor_join) = spawn_actor(
        config,
        paths.clone(),
        state_store.clone(),
        manager.clone(),
        events.clone(),
        questions.clone(),
        turn_engine.clone(),
        Some(memory_organizer_handle),
    )?;
    let (shutdown_tx, mut shutdown_rx) = broadcast::channel(1);
    let state = DaemonState {
        auth: WebAuth::new(password.as_deref()),
        boot_id,
        web_port: port,
        web_public: !bind_ip.is_loopback(),
        web_bind: bind_ip,
        paths,
        manager,
        state_store,
        events,
        questions,
        actor_tx: actor_tx.clone(),
        shutdown_tx,
        turn_engine,
        platforms: PlatformRuntime::new()?,
    };
    let initial_qq = state.manager.lock().unwrap().config.platforms.qq.clone();
    state
        .platforms
        .qq_listener
        .prepare(&state, None, &initial_qq)
        .await?
        .commit();
    let (ipc_lease, ipc_task) = start_ipc_server(&state)?;
    install_background_job_hook(&state);
    let app = router(state.clone());
    let urls = ipc::web_access_urls_for(bind_ip, port);
    for url in &urls {
        println!("GQY WebUI: {url}");
    }
    std::io::stdout().flush().ok();

    let serve_result = {
        let server = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .into_future();
        tokio::pin!(server);
        tokio::select! {
            result = &mut server => result,
            _ = shutdown_signal() => Ok(()),
            _ = shutdown_rx.recv() => Ok(()),
        }
    };
    let _ = actor_tx.send(ActorCommand::Shutdown);
    tools::jobs::shutdown_all();
    state.platforms.qq_listener.shutdown(&state).await;
    ipc_task.abort();
    let _ = ipc_task.await;
    let actor_result = tokio::task::spawn_blocking(move || actor_join.join())
        .await
        .context("joining WebUI actor task")?
        .map_err(|_| anyhow::anyhow!("WebUI actor thread panicked"))?;
    memory_organizer.shutdown();
    drop(ipc_lease);
    serve_result.context("serving GQY WebUI")?;
    actor_result
}

/// Old WebUI versions could make a platform-owned conversation the global
/// current session. Repair that pointer before constructing the local agent
/// so QQ history can never become the WebUI/CLI startup conversation.
pub(crate) fn ensure_local_current_session(state_store: &StateStore, persona: &str) -> Result<()> {
    let current_session_id = state_store.session_id();
    if is_available_local_session(state_store, &current_session_id, persona)? {
        return Ok(());
    }

    let target_session_id = match state_store.list_local_sessions(persona)?.into_iter().next() {
        Some(overview) => overview.record.session_id,
        None => {
            state_store
                .create_session(persona, "", "user", None)?
                .session_id
        }
    };
    state_store.switch_session(&target_session_id)
}

pub(crate) fn is_available_local_session(
    state_store: &StateStore,
    session_id: &str,
    persona: &str,
) -> Result<bool> {
    let usable = state_store
        .session_record(session_id)?
        .is_some_and(|record| record.persona == persona && record.kind == "user");
    Ok(usable && !state_store.is_platform_session(session_id)?)
}

pub(crate) struct IpcRunGuard {
    pub(crate) manager: Arc<Mutex<ManagerState>>,
    pub(crate) run_id: String,
    pub(crate) finished: bool,
}

impl IpcRunGuard {
    pub(crate) fn finish(&mut self) {
        self.finished = true;
    }
}

impl Drop for IpcRunGuard {
    pub(crate) fn drop(&mut self) {
        // dsh 语义(验收):回合归 daemon 所有,前端断线只是观众离席——
        // 不取消。曾经这里在客户端断开时砍掉 run,REPL 一关回合就死;
        // 现在 run 由 actor 跑到终态,finish_run 在完成路径里自行清理,
        // 断线客户端留下的只是一个没人看的事件流。guard 保留为挂点
        // (显式取消仍走 IpcCommand::Cancel)。
        let _ = self.finished;
    }
}

pub(crate) fn start_ipc_server(
    state: &DaemonState,
) -> Result<(crate::ipc::WebCoreLease, TokioJoinHandle<()>)> {
    let lease = ipc::acquire_web_core(&state.paths)
        .context("another GQY core is already running or starting")?;
    let socket_path = state.paths.ipc_socket();
    let listener = tokio::net::UnixListener::bind(&socket_path)
        .with_context(|| format!("binding GQY IPC socket at {}", socket_path.display()))?;
    std::fs::set_permissions(&socket_path, std::fs::Permissions::from_mode(0o600))?;

    let server_state = state.clone();
    let permits = Arc::new(Semaphore::new(32));
    let task = tokio::spawn(async move {
        loop {
            let (stream, _) = match listener.accept().await {
                Ok(connection) => connection,
                Err(error) => {
                    tracing::warn!(
                        error = %error,
                        "{}",
                        t("GQY IPC listener stopped", "GQY IPC 监听器已停止")
                    );
                    break;
                }
            };
            let permit = match permits.clone().acquire_owned().await {
                Ok(permit) => permit,
                Err(_) => break,
            };
            let connection_state = server_state.clone();
            tokio::spawn(async move {
                let _permit = permit;
                if let Err(error) = handle_ipc_connection(connection_state, stream).await {
                    tracing::debug!(
                        error = %error,
                        "{}",
                        t(
                            "GQY IPC connection closed with an error",
                            "GQY IPC 连接因错误关闭"
                        )
                    );
                }
            });
        }
    });
    Ok((lease, task))
}
