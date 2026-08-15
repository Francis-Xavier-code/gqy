//! tests3 — 自 src/platforms/onebot.rs 外移。
#![cfg(test)]

use super::tests::*;
use super::*;

#[tokio::test]
async fn tool_followup_reservation_requires_the_same_conversation_and_sender() {
    let temp = tempfile::tempdir().unwrap();
    let state = test_web_state(temp.path(), 0);
    let config = state.manager.lock().unwrap().config.clone();
    let target = Target::Group { group_id: 99 };
    let event = json!({
        "self_id": 10000,
        "user_id": 42,
        "message_type": "group",
        "group_id": 99,
        "sender": { "nickname": "Alice" }
    });
    let (connection, _frames) = test_connection(None);
    let context =
        Arc::new(platform_turn_context(&state, connection, target, &event, config, None).unwrap());
    let followup = PlatformFollowupRun::new(context);
    followup.ingress().tool_started("call_1");
    let session_id: Arc<str> = "qq-session".into();
    let (cancel, _cancel_rx) = watch::channel(false);
    state.manager.lock().unwrap().active_runs.insert(
        "run_1".to_string(),
        crate::web::RunInfo {
            session_id: session_id.clone(),
            mode: crate::agent::AgentMode::Normal,
            audience: crate::config::PromptAudience::External,
            cancel,
            turn_id: Some("turn_1".to_string()),
            queue_target: None,
            supersede: Arc::new(crate::agent::TurnSupersedeSignal::default()),
            platform_followup: Some(followup.clone()),
            operation: crate::web::RunOperation::Create,
            job_wake: false,
            turn_origin: crate::tools::workspace::TurnOrigin::Human,
            job_wake_label: None,
        },
    );

    assert!(
        reserve_tool_followup(&state, &session_id, &followup.conversation, "other-sender")
            .is_none()
    );
    let mut other_conversation = followup.conversation.clone();
    other_conversation.conversation_id = "100".to_string();
    assert!(reserve_tool_followup(&state, &session_id, &other_conversation, "42").is_none());
    assert!(reserve_tool_followup(&state, &session_id, &followup.conversation, "42").is_some());

    std::thread::sleep(Duration::from_millis(1));
    let newer = PlatformFollowupRun::new(followup.context.clone());
    newer.ingress().tool_started("call_2");
    let (newer_cancel, _newer_cancel_rx) = watch::channel(false);
    state.manager.lock().unwrap().active_runs.insert(
        "run_2".to_string(),
        crate::web::RunInfo {
            session_id: session_id.clone(),
            mode: crate::agent::AgentMode::Normal,
            audience: crate::config::PromptAudience::External,
            cancel: newer_cancel,
            turn_id: Some("turn_2".to_string()),
            queue_target: None,
            supersede: Arc::new(crate::agent::TurnSupersedeSignal::default()),
            platform_followup: Some(newer.clone()),
            operation: crate::web::RunOperation::Create,
            job_wake: false,
            turn_origin: crate::tools::workspace::TurnOrigin::Human,
            job_wake_label: None,
        },
    );
    assert_eq!(
        platform_update_target(&state, &session_id, &followup.conversation, "42")
            .unwrap()
            .0,
        "run_2"
    );

    followup.ingress().tool_finished("call_1");
    newer.ingress().tool_finished("call_2");
    assert!(reserve_tool_followup(&state, &session_id, &followup.conversation, "42").is_none());
}

#[tokio::test]
async fn text_tool_followup_is_observed_and_queued_for_the_running_turn() {
    let temp = tempfile::tempdir().unwrap();
    let state = test_web_state(temp.path(), 0);
    let config = state.manager.lock().unwrap().config.clone();
    let target = Target::Group { group_id: 99 };
    let event = json!({
        "self_id": 10000,
        "user_id": 42,
        "message_id": 123,
        "message_type": "group",
        "group_id": 99,
        "message": "再检查一下",
        "sender": { "nickname": "Alice" }
    });
    let (connection, _frames) = test_connection(None);
    let parsed = InboundMessage {
        text: "再检查一下".to_string(),
        ..InboundMessage::default()
    };
    let inbound = message_event(target, &event, &parsed);
    let context = Arc::new(
        platform_turn_context(
            &state,
            connection.clone(),
            target,
            &event,
            config,
            Some(inbound.clone()),
        )
        .unwrap(),
    );
    let followup = PlatformFollowupRun::new(context.clone());
    let session_id = state.state_store.session_id();
    let turn_store = state.state_store.pinned_for_turn(&session_id);
    turn_store
        .start_turn("running_followup", "first", std::process::id())
        .unwrap();
    let (cancel, _cancel_rx) = watch::channel(false);
    state.manager.lock().unwrap().active_runs.insert(
        "run-followup".to_string(),
        crate::web::RunInfo {
            session_id: session_id.clone(),
            mode: crate::agent::AgentMode::Normal,
            audience: crate::config::PromptAudience::External,
            cancel,
            turn_id: Some("running_followup".to_string()),
            queue_target: Some(turn_store.queue_target("running_followup")),
            supersede: Arc::new(crate::agent::TurnSupersedeSignal::default()),
            platform_followup: Some(followup.clone()),
            operation: crate::web::RunOperation::Create,
            job_wake: false,
            turn_origin: crate::tools::workspace::TurnOrigin::Human,
            job_wake_label: None,
        },
    );

    enqueue_tool_followup(
        &state,
        &connection,
        target,
        &event,
        parsed,
        &inbound,
        &context,
        &followup,
        &session_id,
        "run-followup",
        "running_followup",
        TurnUpdateMode::Followup,
    )
    .await
    .unwrap();

    let queued = turn_store.load_queued_prompts().unwrap();
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].display_content, "再检查一下");
    assert!(queued[0].content.starts_with("再检查一下"));
    assert!(queued[0].content.contains("发送者 QQ=42; 消息 ID=123"));
}

#[tokio::test]
async fn qq_conversation_persona_drives_context_and_session_binding() {
    let temp = tempfile::tempdir().unwrap();
    let state = test_web_state(temp.path(), 8300);
    let mut config = state.manager.lock().unwrap().config.clone();
    std::fs::create_dir_all(config.prompts_dir_path(&state.paths)).unwrap();
    std::fs::write(
        config.persona_path(&state.paths, "Group.md"),
        "Group persona",
    )
    .unwrap();
    config
        .platforms
        .qq
        .conversations
        .push(crate::config::PlatformModelRoute {
            conversation: crate::config::PlatformConversationConfig {
                kind: PlatformConversationKind::Group,
                id: "99".to_string(),
            },
            persona: crate::config::PlatformPersonaOverride::Custom {
                name: "Group.md".to_string(),
            },
            text_models_inheritance: crate::config::PlatformModelPoolInheritance::Platform,
            text_models: None,
            multimodal_models_inheritance: crate::config::PlatformModelPoolInheritance::Platform,
            multimodal_models: None,
            extra_prompt: String::new(),
            session_limits: None,
        });
    let target = Target::Group { group_id: 99 };
    let event = json!({
        "self_id": 10000,
        "user_id": 42,
        "message_type": "group",
        "group_id": 99,
        "sender": { "nickname": "Alice" }
    });
    let (connection, _frames) = test_connection(None);

    let custom = platform_turn_context(
        &state,
        connection.clone(),
        target,
        &event,
        config.clone(),
        None,
    )
    .unwrap();
    assert_eq!(custom.config.prompt.active_persona, "Group.md");
    let custom_session = resolve_onebot_session(&state, &custom, target, &event).unwrap();
    assert_eq!(
        state
            .state_store
            .session_record(&custom_session)
            .unwrap()
            .unwrap()
            .persona,
        custom.config.active_persona_scope()
    );

    config.platforms.qq.conversations[0].persona = crate::config::PlatformPersonaOverride::GQY;
    let gqy = platform_turn_context(&state, connection, target, &event, config, None).unwrap();
    assert!(gqy.config.prompt.active_persona.is_empty());
    let gqy_session = resolve_onebot_session(&state, &gqy, target, &event).unwrap();
    assert_ne!(custom_session, gqy_session);
}

#[tokio::test]
async fn reset_command_uses_configured_admins_and_clears_the_bound_session() {
    let temp = tempfile::tempdir().unwrap();
    let (state, actor_join) =
        DaemonState::for_test_with_actor(test_paths(temp.path()), 8300).unwrap();
    let target = Target::Group { group_id: 99 };
    let event = json!({
        "self_id": 10000,
        "user_id": 42,
        "message_type": "group",
        "group_id": 99,
        "message_id": 7,
        "message": [{ "type": "text", "data": { "text": "/reset extra" } }],
        "sender": { "nickname": "Alice", "role": "owner" }
    });
    state.manager.lock().unwrap().config.platforms.qq.enabled = true;
    let (connection, mut frames) = test_connection(None);
    let persona = state.manager.lock().unwrap().config.active_persona_scope();
    let sessions_before = state.state_store.list_sessions(&persona).unwrap().len();

    // QQ group roles never grant GQY command administration.
    let denied = tokio::spawn(handle_message(
        state.clone(),
        connection.clone(),
        event.clone(),
        next_ingress_order(),
    ));
    denied.await.unwrap();
    assert!(frames.try_recv().is_err());
    assert_eq!(
        state.state_store.list_sessions(&persona).unwrap().len(),
        sessions_before
    );

    state
        .manager
        .lock()
        .unwrap()
        .config
        .platforms
        .qq
        .admin_users
        .push(42);
    let context = platform_turn_context(
        &state,
        connection.clone(),
        target,
        &event,
        state.manager.lock().unwrap().config.clone(),
        None,
    )
    .unwrap();
    assert!(context.is_admin);
    let session_id = resolve_onebot_session(&state, &context, target, &event).unwrap();
    let store = state.state_store.pinned(&session_id);
    store
        .start_turn("qq_history", "hello", std::process::id())
        .unwrap();
    store.complete_turn("qq_history", "world", None).unwrap();

    let mut raw_reset_event = event.clone();
    raw_reset_event["message"] = json!("[CQ:reply,id=6]/reset");
    let reset = tokio::spawn(handle_message(
        state.clone(),
        connection.clone(),
        raw_reset_event,
        next_ingress_order(),
    ));
    let reset_frame: Value = serde_json::from_str(&frames.recv().await.unwrap()).unwrap();
    assert_eq!(reset_frame["action"], "send_group_msg");
    route_api_response(
        &connection,
        json!({
            "status": "ok",
            "retcode": 0,
            "data": { "message_id": 71 },
            "echo": reset_frame["echo"],
        }),
    );
    reset.await.unwrap();
    assert!(store.load_turns().unwrap().is_empty());
    assert!(temp
        .path()
        .join("data/platforms/onebot/message_history/history.sqlite3")
        .is_file());
    assert_eq!(
        resolve_onebot_session(&state, &context, target, &event).unwrap(),
        session_id
    );
    assert!(!state.manager.lock().unwrap().admin_busy);

    state
        .actor_tx
        .send(crate::web::ActorCommand::Shutdown)
        .unwrap();
    actor_join.join().unwrap().unwrap();
}

#[tokio::test]
async fn wipe_clears_every_local_session_of_the_active_persona() {
    let temp = tempfile::tempdir().unwrap();
    let (state, actor_join) =
        DaemonState::for_test_with_actor(test_paths(temp.path()), 8300).unwrap();
    let mut config = state.manager.lock().unwrap().config.clone();
    config.platforms.qq.admin_users.push(42);
    let persona = config.active_persona_scope();
    state
        .state_store
        .adopt_sessions_for_persona(&persona)
        .unwrap();
    let active = state
        .state_store
        .create_session(&persona, "active", "user", None)
        .unwrap();
    let second = state
        .state_store
        .create_session(&persona, "second", "user", None)
        .unwrap();
    for (session_id, turn_id) in [
        (&active.session_id, "active-before-reset-all"),
        (&second.session_id, "second-before-reset-all"),
    ] {
        let store = state.state_store.pinned(session_id);
        store
            .start_turn(turn_id, "before", std::process::id())
            .unwrap();
        store.complete_turn(turn_id, "after", None).unwrap();
    }

    let generated_skill = config
        .active_persona_skills_dir(&state.paths)
        .join("generated-test");
    std::fs::create_dir_all(&generated_skill).unwrap();
    std::fs::write(
        generated_skill.join("SKILL.md"),
        "---\ngenerated_by: gqy\n---\n",
    )
    .unwrap();

    let target = Target::Private { user_id: 42 };
    let event = json!({
        "self_id": 10000,
        "user_id": 42,
        "message_type": "private",
        "message_id": 8,
        "message": [{ "type": "text", "data": { "text": "/reset all" } }],
        "sender": { "nickname": "Alice" }
    });
    let (connection, _frames) = test_connection(None);
    let context = platform_turn_context(&state, connection, target, &event, config, None).unwrap();
    let response = execute_builtin_command(
        &state,
        &context,
        target,
        &event,
        commands::ParsedPlatformCommand::Wipe { confirmed: false },
    )
    .await
    .expect("an unconfirmed wipe answers with what it would erase");
    let asked = format!("{:?}", response.body);
    assert!(asked.contains("confirm"), "{asked}");
    // Nothing may be gone yet: the word `confirm` is the only dialog box a
    // chat platform gets.
    assert!(!state
        .state_store
        .pinned(&active.session_id)
        .load_turns()
        .unwrap()
        .is_empty());

    let response = execute_builtin_command(
        &state,
        &context,
        target,
        &event,
        commands::ParsedPlatformCommand::Wipe { confirmed: true },
    )
    .await
    .expect("a confirmed wipe returns a response");

    assert!(matches!(response.body, OutboundBody::Segments(_)));
    assert!(state
        .state_store
        .pinned(&active.session_id)
        .load_turns()
        .unwrap()
        .is_empty());
    // 归档豁免已随功能移除:/reset all 现在清掉本人格全部本地会话。
    assert!(state
        .state_store
        .pinned(&second.session_id)
        .load_turns()
        .unwrap()
        .is_empty());
    assert!(!generated_skill.exists());
    assert!(!state.manager.lock().unwrap().admin_busy);

    state
        .actor_tx
        .send(crate::web::ActorCommand::Shutdown)
        .unwrap();
    actor_join.join().unwrap().unwrap();
}

#[test]
fn rate_limit_notices_are_silent_in_private_chats_only() {
    assert!(!sends_rate_limit_notice(Target::Private { user_id: 7 }));
    assert!(sends_rate_limit_notice(Target::Group { group_id: 42 }));
}

#[tokio::test]
async fn stop_command_cancels_the_session_and_preserves_completed_history() {
    let temp = tempfile::tempdir().unwrap();
    let state = test_web_state(temp.path(), 8300);
    let target = Target::Private { user_id: 42 };
    let event = json!({
        "self_id": 10000,
        "user_id": 42,
        "message_type": "private",
        "message_id": 8,
        "message": [{ "type": "text", "data": { "text": "/stop" } }],
        "sender": { "nickname": "Alice" }
    });
    let (connection, _frames) = test_connection(None);
    let mut config = state.manager.lock().unwrap().config.clone();
    config.platforms.qq.admin_users.push(42);
    let context = platform_turn_context(&state, connection, target, &event, config, None).unwrap();
    let session_id = resolve_onebot_session(&state, &context, target, &event).unwrap();
    let store = state.state_store.pinned(&session_id);
    store
        .start_turn("completed_before_stop", "hello", std::process::id())
        .unwrap();
    store
        .complete_turn("completed_before_stop", "world", None)
        .unwrap();
    let (cancel, cancel_rx) = watch::channel(false);
    state.manager.lock().unwrap().active_runs.insert(
        "active_stop_test".to_string(),
        crate::web::RunInfo {
            session_id: session_id.clone(),
            mode: crate::agent::AgentMode::Normal,
            audience: crate::config::PromptAudience::External,
            cancel,
            turn_id: None,
            queue_target: None,
            supersede: Arc::new(crate::agent::TurnSupersedeSignal::default()),
            platform_followup: None,
            operation: crate::web::RunOperation::Create,
            job_wake: false,
            turn_origin: crate::tools::workspace::TurnOrigin::Human,
            job_wake_label: None,
        },
    );

    let response = execute_builtin_command(
        &state,
        &context,
        target,
        &event,
        commands::ParsedPlatformCommand::Stop {
            has_arguments: false,
        },
    )
    .await;

    assert!(*cancel_rx.borrow());
    assert_eq!(store.load_turns().unwrap().len(), 1);
    let OutboundBody::Segments(segments) = response.expect("stop returns a response").body else {
        panic!("stop response must be a normal message");
    };
    assert!(matches!(
        segments.as_slice(),
        [OutboundSegment::Text(text)]
            if text.contains("已打断 1 个运行中的任务") || text.contains("Interrupted 1 running task")
    ));
    state
        .manager
        .lock()
        .unwrap()
        .active_runs
        .remove("active_stop_test");
}

#[test]
fn sanitizes_file_names() {
    assert_eq!(sanitize_file_name("../../etc/passwd"), "passwd");
    assert_eq!(sanitize_file_name("C:\\evil\\x.exe"), "x.exe");
    assert_eq!(sanitize_file_name(".."), "file");
    assert_eq!(sanitize_file_name("  "), "file");
    assert_eq!(sanitize_file_name("报告 v2.pdf"), "报告 v2.pdf");
}

#[tokio::test]
async fn concurrent_inbound_files_with_the_same_name_do_not_overwrite() {
    let temp = tempfile::tempdir().unwrap();
    let first = save_platform_file(temp.path(), "report.txt", b"first");
    let second = save_platform_file(temp.path(), "report.txt", b"second");
    let (first, second) = tokio::join!(first, second);
    let first = first.unwrap();
    let second = second.unwrap();

    assert_ne!(first, second);
    let mut contents = vec![
        tokio::fs::read(first).await.unwrap(),
        tokio::fs::read(second).await.unwrap(),
    ];
    contents.sort();
    assert_eq!(contents, vec![b"first".to_vec(), b"second".to_vec()]);
}

#[tokio::test]
async fn inbound_file_store_enforces_a_total_capacity() {
    let temp = tempfile::tempdir().unwrap();
    save_platform_file(temp.path(), "existing.bin", b"12345678")
        .await
        .unwrap();

    assert!(
        ensure_platform_file_capacity(temp.path(), 2, 10, 10, Duration::from_secs(60),)
            .await
            .is_ok()
    );
    assert!(
        ensure_platform_file_capacity(temp.path(), 3, 10, 10, Duration::from_secs(60),)
            .await
            .is_err()
    );
}

#[test]
fn outbound_frames_have_the_onebot_shape() {
    let frame: Value = serde_json::from_str(&api_frame(
        "send_private_msg",
        json!({ "user_id": 42, "message": [text_segment("hi")] }),
        "test",
    ))
    .unwrap();
    assert_eq!(frame["action"], "send_private_msg");
    assert_eq!(frame["params"]["user_id"], 42);
    assert_eq!(frame["params"]["message"][0]["type"], "text");
    assert_eq!(frame["params"]["message"][0]["data"]["text"], "hi");
    assert!(frame["echo"].as_str().is_some());

    let frame: Value = serde_json::from_str(&api_frame(
        "send_group_msg",
        json!({ "group_id": 7, "message": [text_segment("x")] }),
        "test",
    ))
    .unwrap();
    assert_eq!(frame["action"], "send_group_msg");
    assert_eq!(frame["params"]["group_id"], 7);
}

#[test]
fn token_check_accepts_bearer_and_rejects_wrong() {
    let mut headers = HeaderMap::new();
    assert!(token_matches(&headers, ""));
    assert!(!token_matches(&headers, "secret"));
    headers.insert(AUTHORIZATION, "Bearer secret".parse().unwrap());
    assert!(token_matches(&headers, "secret"));
    assert!(!token_matches(&headers, "other"));
    headers.insert(AUTHORIZATION, "Token secret".parse().unwrap());
    assert!(token_matches(&headers, "secret"));
    headers.insert(AUTHORIZATION, "secret".parse().unwrap());
    assert!(token_matches(&headers, "secret"));
}

#[test]
fn empty_token_only_authorizes_loopback_connections() {
    let headers = HeaderMap::new();
    assert!(connection_authorized(
        &headers,
        "",
        "127.0.0.1:1234".parse().unwrap()
    ));
    assert!(connection_authorized(
        &headers,
        "",
        "[::1]:1234".parse().unwrap()
    ));
    assert!(!connection_authorized(
        &headers,
        "",
        "192.168.1.5:1234".parse().unwrap()
    ));
}

pub(crate) fn test_connection(
    asset_base_url: Option<String>,
) -> (ConnectionHandle, mpsc::UnboundedReceiver<String>) {
    let (out_tx, out_rx) = mpsc::unbounded_channel();
    let (shutdown, _shutdown_rx) = watch::channel(false);
    (
        ConnectionHandle {
            out_tx,
            pending: Arc::new(Mutex::new(HashMap::new())),
            bot_name: Arc::new(Mutex::new(None)),
            asset_base_url,
            assets: super::super::assets::AssetLeaseStore::new(),
            shutdown,
        },
        out_rx,
    )
}

pub(crate) fn test_adapter(handle: ConnectionHandle, target: Target) -> OneBotAdapter {
    let mut registry = ConnectionRegistry::default();
    registry.register(10000, handle.clone());
    OneBotAdapter {
        conn: handle,
        registry: Arc::new(Mutex::new(registry)),
        http: reqwest::Client::new(),
        self_id: 10000,
        target,
        max_reply_chars: 0,
    }
}

#[test]
fn late_identity_binding_cannot_replace_a_newer_connection() {
    let (older, _older_frames) = test_connection(None);
    let (newer, _newer_frames) = test_connection(None);
    let mut registry = ConnectionRegistry::default();
    let older_generation = registry.register(0, older.clone());
    let newer_generation = registry.register(0, newer.clone());

    assert!(registry.bind(10000, newer_generation, newer));
    assert!(!registry.bind(10000, older_generation, older));
    assert!(registry.is_current(10000, newer_generation));
    assert!(!registry.is_current(10000, older_generation));
}

#[tokio::test]
async fn api_calls_wait_for_the_matching_echo() {
    let (handle, mut frames) = test_connection(None);
    let caller = {
        let handle = handle.clone();
        tokio::spawn(async move { handle.call_api("get_login_info", json!({})).await })
    };
    let frame: Value = serde_json::from_str(&frames.recv().await.unwrap()).unwrap();
    assert_eq!(frame["action"], "get_login_info");
    let echo = frame["echo"].as_str().unwrap().to_string();

    // An unrelated response must not resolve this request.
    route_api_response(
        &handle,
        json!({ "status": "ok", "retcode": 0, "data": null, "echo": "other" }),
    );
    assert!(!caller.is_finished());
    route_api_response(
        &handle,
        json!({
            "status": "ok",
            "retcode": 0,
            "data": { "nickname": "GQY" },
            "echo": echo,
        }),
    );
    let data = caller.await.unwrap().unwrap();
    assert_eq!(data["nickname"], "GQY");
    assert!(handle.pending.lock().unwrap().is_empty());
}

#[test]
fn api_error_detail_drops_raw_protocol_bytes() {
    // Verbatim shape of a failed kick: NapCat splices the target's
    // protobuf-encoded UID into the wording.
    let raw = "kick member failed: \u{8}\u{0}\u{12}\u{18}u_GnsZB8HSJVKfjWNjMqYqbA";
    let cleaned = sanitize_api_detail(raw);
    assert_eq!(cleaned, "kick member failed: u_GnsZB8HSJVKfjWNjMqYqbA");
    assert!(!cleaned.chars().any(char::is_control));

    assert_eq!(sanitize_api_detail("  spaced  "), "spaced");
    let long = "x".repeat(500);
    let clipped = sanitize_api_detail(&long);
    assert!(clipped.ends_with('…'));
    assert_eq!(clipped.chars().count(), 201);
}

#[tokio::test]
async fn api_errors_preserve_napcat_status_retcode_and_wording() {
    let (handle, mut frames) = test_connection(None);
    let caller = {
        let handle = handle.clone();
        tokio::spawn(async move {
            handle
                .call_api("delete_msg", json!({ "message_id": 1 }))
                .await
        })
    };
    let frame: Value = serde_json::from_str(&frames.recv().await.unwrap()).unwrap();
    route_api_response(
        &handle,
        json!({
            "status": "failed",
            "retcode": "1200",
            "wording": "消息已超过撤回时限",
            "echo": frame["echo"],
        }),
    );
    let error = caller.await.unwrap().unwrap_err().to_string();
    assert!(error.contains("status=failed"));
    assert!(error.contains("retcode=1200"));
    assert!(error.contains("消息已超过撤回时限"));
}

/// Regression: a picture is megabytes of base64 JSON that NapCat has to
/// receive, decode and upload to QQ. Any budget short of the backstop made
/// GQY treat a delivered image as failed and post the text fallback on top
/// of it. Size scaling was not enough — the old `div_ceil` step handed a
/// 0.99 MiB payload the same 30s as a 64 KiB one.
#[test]
fn attachment_sends_wait_for_napcat_instead_of_a_size_budget() {
    let text_only = vec![text_segment("hello")];
    assert_eq!(send_timeout_for(&text_only), API_CALL_TIMEOUT);

    let small_image = vec![image_segment(&vec![0u8; 64 * 1024])];
    assert_eq!(send_timeout_for(&small_image), MAX_SEND_TIMEOUT);

    // The old boundary case: just under a megabyte used to share the
    // smallest budget with a thumbnail.
    let boundary_image = vec![image_segment(&vec![0u8; 700 * 1024])];
    assert_eq!(send_timeout_for(&boundary_image), MAX_SEND_TIMEOUT);

    let huge_image = vec![image_segment(&vec![0u8; 19 * 1024 * 1024])];
    assert_eq!(send_timeout_for(&huge_image), MAX_SEND_TIMEOUT);

    // Mixed frames follow the attachment, not the text.
    let mixed = vec![text_segment("看图"), image_segment(&vec![0u8; 4096])];
    assert_eq!(send_timeout_for(&mixed), MAX_SEND_TIMEOUT);
}

#[tokio::test]
async fn delete_message_sends_one_numeric_request_and_does_not_retry_failure() {
    let (handle, mut frames) = test_connection(None);
    let adapter = test_adapter(handle.clone(), Target::Group { group_id: 7 });
    let caller = tokio::spawn(async move { adapter.delete_message("442989412").await });

    let request: Value = serde_json::from_str(&frames.recv().await.unwrap()).unwrap();
    assert_eq!(request["action"], "delete_msg");
    assert_eq!(request["params"]["message_id"], 442989412);
    route_api_response(
        &handle,
        json!({
            "status": "failed",
            "retcode": 1200,
            "wording": "decode failed",
            "echo": request["echo"],
        }),
    );
    let error = caller.await.unwrap().unwrap_err().to_string();
    assert!(error.contains("retcode=1200"));
    assert!(error.contains("decode failed"));
    assert!(frames.try_recv().is_err());
}

#[test]
fn private_message_info_uses_target_peer_and_sender_fallbacks() {
    let sent = parse_message_info(
        &json!({
            "message_type": "private",
            "message_id": 1,
            "target_id": 20000,
            "sender": { "user_id": 10000, "nickname": "GQY" },
            "message": [{ "type": "text", "data": { "text": "hello" } }],
        }),
        10000,
    )
    .unwrap();
    assert_eq!(sent.conversation_kind, Some(ConversationKind::Private));
    assert_eq!(sent.conversation_id.as_deref(), Some("20000"));
    assert_eq!(sent.sender_id, "10000");

    let received = parse_message_info(
        &json!({
            "message_type": "private",
            "message_id": "2",
            "sender": { "user_id": "20000", "nickname": "user" },
            "message": [],
        }),
        10000,
    )
    .unwrap();
    assert_eq!(received.conversation_id.as_deref(), Some("20000"));
}

#[tokio::test]
async fn group_name_resolution_prefers_events_and_caches_api_fallbacks() {
    let (handle, mut frames) = test_connection(None);
    let event_name = json!({ "group_name": "From event" });
    assert_eq!(
        resolve_group_name(&handle, 71, 7101, &event_name)
            .await
            .as_deref(),
        Some("From event")
    );
    assert!(frames.try_recv().is_err());

    let no_name = json!({});
    let lookup = {
        let handle = handle.clone();
        let event = no_name.clone();
        tokio::spawn(async move { resolve_group_name(&handle, 71, 7102, &event).await })
    };
    let frame: Value = serde_json::from_str(&frames.recv().await.unwrap()).unwrap();
    assert_eq!(frame["action"], "get_group_info");
    assert_eq!(frame["params"]["group_id"], 7102);
    route_api_response(
        &handle,
        json!({
            "status": "ok",
            "retcode": 0,
            "data": { "group_id": 7102, "group_name": "From API" },
            "echo": frame["echo"],
        }),
    );
    assert_eq!(lookup.await.unwrap().as_deref(), Some("From API"));

    assert_eq!(
        resolve_group_name(&handle, 71, 7102, &no_name)
            .await
            .as_deref(),
        Some("From API")
    );
    assert!(frames.try_recv().is_err());
}

#[tokio::test]
async fn api_call_fails_immediately_when_the_writer_is_closed() {
    let (handle, frames) = test_connection(None);
    drop(frames);
    let started = tokio::time::Instant::now();

    assert!(handle.call_api("get_status", json!({})).await.is_err());
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(handle.pending.lock().unwrap().is_empty());
}
