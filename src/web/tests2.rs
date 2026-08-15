//! tests2 — 自 src/web.rs 拆分。

#![cfg(test)]

use super::*;

#[test]
pub(crate) fn startup_repairs_a_platform_owned_current_session() {
    let temp = tempfile::tempdir().unwrap();
    let store = StateStore::new(&test_paths(temp.path())).unwrap();
    store.adopt_sessions_for_persona("gqy").unwrap();
    let qq_session = store
        .create_session("gqy", "QQ group 20000", "user", None)
        .unwrap();
    store
        .bind_platform_session(
            &PlatformSessionBindingKey {
                platform: "onebot".to_string(),
                account_id: "10000".to_string(),
                conversation_kind: "group".to_string(),
                conversation_id: "20000".to_string(),
                participant_id: None,
                persona: "gqy".to_string(),
            },
            &qq_session.session_id,
        )
        .unwrap();
    store.switch_session(&qq_session.session_id).unwrap();

    ensure_local_current_session(&store, "gqy").unwrap();

    let repaired = store.session_id();
    assert_ne!(&*repaired, qq_session.session_id);
    assert!(!store.is_platform_session(&repaired).unwrap());
    assert_eq!(
        store.session_record(&repaired).unwrap().unwrap().persona,
        "gqy"
    );
}

#[test]
pub(crate) fn actor_commands_keep_large_configuration_off_the_inline_queue_item() {
    assert!(std::mem::size_of::<ActorCommand>() <= 512);
}

#[test]
pub(crate) fn prompt_sidecar_reads_avatar_path_without_touching_prompt_content() {
    let temp = tempfile::tempdir().unwrap();
    let prompt = temp.path().join("Alice.md");
    std::fs::write(&prompt, "You are Alice.\n").unwrap();
    std::fs::write(
            temp.path().join("Alice.json"),
            r#"{"avatar_path":"avatars/alice.png","board_image_path":"persona-avatars/board.png","board_title":"欢迎","board_subtitle":"从这里开始","starter_prompts":["天气","问题"]}"#,
        )
        .unwrap();

    let documents = read_prompt_document_dir(temp.path(), true).unwrap();
    assert_eq!(documents.len(), 1);
    assert_eq!(documents[0].name, "Alice.md");
    assert_eq!(documents[0].content, "You are Alice.\n");
    assert_eq!(
        documents[0].avatar_path.as_deref(),
        Some("avatars/alice.png")
    );
    assert_eq!(
        documents[0].board_image_path.as_deref(),
        Some("persona-avatars/board.png")
    );
    assert_eq!(documents[0].board_title.as_deref(), Some("欢迎"));
    assert_eq!(documents[0].starter_prompts.as_ref().map(Vec::len), Some(2));
}

#[test]
pub(crate) fn malformed_prompt_sidecar_falls_back_to_no_avatar() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(temp.path().join("Alice.md"), "prompt").unwrap();
    std::fs::write(temp.path().join("Alice.json"), "not json").unwrap();

    let documents = read_prompt_document_dir(temp.path(), true).unwrap();
    assert_eq!(documents[0].avatar_path, None);
}

#[test]
pub(crate) fn persona_file_mutations_include_avatar_sidecar() {
    let temp = tempfile::tempdir().unwrap();
    let mut mutations = HashMap::new();
    let documents = vec![PromptDocument {
        name: "Alice.md".to_string(),
        content: "prompt".to_string(),
        avatar_path: Some("avatars/alice.png".to_string()),
        board_image_path: None,
        board_title: None,
        board_subtitle: None,
        starter_prompts: None,
        original_name: None,
    }];
    collect_prompt_file_mutations(
        &[],
        &documents,
        temp.path(),
        temp.path(),
        &mut mutations,
        true,
    );

    let metadata = mutations
        .get(&temp.path().join("Alice.json"))
        .and_then(Option::as_deref)
        .unwrap();
    let metadata: Value = serde_json::from_slice(metadata).unwrap();
    assert_eq!(metadata["avatar_path"], "avatars/alice.png");
}

#[test]
pub(crate) fn persona_identity_uses_default_and_custom_values() {
    let mut config = AppConfig::default();
    let prompts = PromptDocuments::default();
    let default = persona_identity(&config, &prompts);
    assert_eq!(default.name, "GQY");
    assert_eq!(default.avatar_url.as_deref(), Some("/assets/gqy-logo.png"));

    config.prompt.active_persona = "Alice.md".to_string();
    let prompts = PromptDocuments {
        personas: vec![PromptDocument {
            name: "Alice.md".to_string(),
            content: "prompt".to_string(),
            avatar_path: Some("avatars/alice.png".to_string()),
            board_image_path: None,
            board_title: None,
            board_subtitle: None,
            starter_prompts: None,
            original_name: None,
        }],
        identities: Vec::new(),
    };
    let custom = persona_identity(&config, &prompts);
    assert_eq!(custom.name, "Alice");
    assert_eq!(custom.avatar_url.as_deref(), Some("/api/persona/avatar"));
}

#[test]
pub(crate) fn sanitize_session_title_cleans_llm_output() {
    assert_eq!(sanitize_session_title("「东京天气查询」"), "东京天气查询");
    assert_eq!(
        sanitize_session_title("\"Homebrew 更新公告\"\n第二行忽略"),
        "Homebrew 更新公告"
    );
    assert_eq!(sanitize_session_title("  标题。  "), "标题");
    assert_eq!(sanitize_session_title(""), "");
    // Overlong output clips to 20 chars.
    let long = "很长的标题".repeat(10);
    assert_eq!(sanitize_session_title(&long).chars().count(), 20);
}

pub(crate) fn manager_with_run(
    run_id: &str,
) -> (Arc<Mutex<ManagerState>>, tokio::sync::watch::Receiver<bool>) {
    let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
    let manager = Arc::new(Mutex::new(ManagerState {
        config: AppConfig::default(),
        active_runs: HashMap::from([(
            run_id.to_string(),
            RunInfo {
                session_id: "default".into(),
                mode: AgentMode::Normal,
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
        )]),
        admin_busy: false,
        context: ContextSnapshot {
            tokens: 0,
            window: None,
            cumulative_tokens: 0,
            cumulative_prompt_tokens: 0,
            cumulative_cache_read_tokens: 0,
        },
        persona_session_ids: HashMap::new(),
    }));
    (manager, cancel_rx)
}

#[test]
pub(crate) fn active_turn_queue_never_crosses_prompt_audiences() {
    let (manager, _cancel_rx) = manager_with_run("owner_run");
    let manager = manager.lock().unwrap();

    assert!(manager.session_runs_match_audience("default", PromptAudience::Owner));
    assert!(!manager.session_runs_match_audience("default", PromptAudience::External));
    assert!(!manager.session_runs_match_audience("missing", PromptAudience::Owner));
}

#[test]
pub(crate) fn light_admin_reservation_allows_running_turns_and_serializes_mutations() {
    let (manager, _cancel_rx) = manager_with_run("active_run");

    assert!(reserve_admin(&manager).is_err());
    assert!(reserve_admin_light(&manager).is_ok());
    assert!(reserve_admin_light(&manager).is_err());
    assert_eq!(manager.lock().unwrap().active_runs.len(), 1);

    release_admin(&manager);
    assert!(reserve_admin_light(&manager).is_ok());
    release_admin(&manager);
}

#[test]
pub(crate) fn turn_updates_are_routed_to_the_exact_run_and_turn() {
    let temp = tempfile::tempdir().unwrap();
    let state = DaemonState::for_test(test_paths(temp.path()), 8300).unwrap();
    let session_id = state.state_store.session_id();
    let first_store = state.state_store.pinned_for_turn(&session_id);
    let second_store = state.state_store.pinned_for_turn(&session_id);
    first_store
        .start_turn("turn-first", "first", std::process::id())
        .unwrap();
    second_store
        .start_turn("turn-second", "second", std::process::id())
        .unwrap();
    let mut manager = state.manager.lock().unwrap();
    for (run_id, turn_id, store) in [
        ("run-first", "turn-first", &first_store),
        ("run-second", "turn-second", &second_store),
    ] {
        let (cancel, _cancel_rx) = tokio::sync::watch::channel(false);
        manager.active_runs.insert(
            run_id.to_string(),
            RunInfo {
                session_id: session_id.clone(),
                mode: AgentMode::Normal,
                audience: PromptAudience::External,
                cancel,
                turn_id: Some(turn_id.to_string()),
                queue_target: Some(store.queue_target(turn_id)),
                supersede: Arc::new(crate::agent::TurnSupersedeSignal::default()),
                platform_followup: None,
                operation: RunOperation::Create,
                job_wake: false,
                turn_origin: crate::tools::workspace::TurnOrigin::Human,
                job_wake_label: None,
            },
        );
    }
    drop(manager);

    enqueue_turn_update(
        &state,
        TurnUpdateRequest {
            run_id: "run-first".to_string(),
            turn_id: "turn-first".to_string(),
            session_id: Some(session_id.clone()),
            audience: PromptAudience::External,
            content: "follow first".to_string(),
            display_content: "follow first".to_string(),
            attachments: Vec::new(),
            uploaded_attachment_ids: Vec::new(),
            mode: TurnUpdateMode::Followup,
        },
    )
    .unwrap();

    assert_eq!(first_store.load_queued_prompts().unwrap().len(), 1);
    assert!(second_store.load_queued_prompts().unwrap().is_empty());
    assert!(enqueue_turn_update(
        &state,
        TurnUpdateRequest {
            run_id: "run-first".to_string(),
            turn_id: "turn-second".to_string(),
            session_id: Some(session_id),
            audience: PromptAudience::External,
            content: "wrong target".to_string(),
            display_content: "wrong target".to_string(),
            attachments: Vec::new(),
            uploaded_attachment_ids: Vec::new(),
            mode: TurnUpdateMode::Followup,
        },
    )
    .is_err());
}

#[test]
pub(crate) fn dropped_ipc_turn_detaches_without_cancelling_the_run() {
    // dsh 语义:前端断线,回合继续——guard 掉落绝不发取消。
    let (manager, cancel_rx) = manager_with_run("run_test");
    drop(IpcRunGuard {
        manager,
        run_id: "run_test".to_string(),
        finished: false,
    });
    assert!(!*cancel_rx.borrow());
}

#[test]
pub(crate) fn assistant_sentinels_are_never_exposed() {
    assert_eq!(
        redact_internal_assistant_text(crate::state::pending_placeholder()),
        ""
    );
    assert_eq!(
        redact_internal_assistant_text(crate::state::interrupted_text()),
        ""
    );
    let combined = format!("before {} after", crate::state::interrupted_text());
    let redacted = redact_internal_assistant_text(&combined);
    assert_eq!(redacted, "before  after");
    assert!(!redacted.contains("system-reminder"));
}

#[test]
pub(crate) fn persisted_meme_assets_hide_their_descriptive_caption() {
    let asset = ImageAsset {
        asset_id: "img_test".to_string(),
        turn_id: "turn_test".to_string(),
        tool_id: Some("tool_test".to_string()),
        mime: "image/png".to_string(),
        width: 64,
        height: 64,
        alt: "猫猫 开心 & <得意>".to_string(),
        created_at: "2026-01-01T00:00:00Z".to_string(),
    };
    let reports = vec![
            "<sent_meme>发送了一个表情包：id=sha256:test；description=猫猫 开心 &amp; &lt;得意&gt;</sent_meme>"
                .to_string(),
        ];

    assert!(meme_asset_caption_hidden(&asset, &reports));
    assert!(!meme_asset_caption_hidden(
        &asset,
        &["normal tool output".to_string()]
    ));
}

#[test]
pub(crate) fn cookie_parser_matches_an_exact_cookie_name() {
    let mut headers = HeaderMap::new();
    headers.insert(
        COOKIE,
        HeaderValue::from_static("other=1; gqy_session=secret-token; suffix=2"),
    );
    assert_eq!(cookie_value(&headers, AUTH_COOKIE), Some("secret-token"));
    assert_eq!(cookie_value(&headers, "session"), None);
}

#[test]
pub(crate) fn origin_check_accepts_absent_or_current_host_origin() {
    let mut headers = HeaderMap::new();
    assert!(origin_is_allowed(&headers));
    headers.insert(HOST, HeaderValue::from_static("192.168.1.20:4096"));
    headers.insert(ORIGIN, HeaderValue::from_static("http://127.0.0.1:4096"));
    assert!(!origin_is_allowed(&headers));
    headers.insert(ORIGIN, HeaderValue::from_static("http://192.168.1.20:4096"));
    assert!(origin_is_allowed(&headers));
    headers.append(ORIGIN, HeaderValue::from_static("http://192.168.1.20:4096"));
    assert!(!origin_is_allowed(&headers));
}

#[test]
pub(crate) fn optional_password_auth_issues_server_side_sessions_and_limits_failures() {
    let disabled = WebAuth::new(None);
    assert!(disabled.is_authenticated(None));

    let auth = WebAuth::new(Some("correct horse"));
    let peer = IpAddr::V4(Ipv4Addr::LOCALHOST);
    assert!(!auth.is_authenticated(None));
    assert!(matches!(
        auth.login(peer, "wrong"),
        Err(LoginFailure::Invalid)
    ));
    let token = auth.login(peer, "correct horse").unwrap();
    assert!(auth.is_authenticated(Some(&token)));

    let limited = WebAuth::new(Some("secret"));
    for _ in 0..LOGIN_ATTEMPT_LIMIT {
        assert!(matches!(
            limited.login(peer, "wrong"),
            Err(LoginFailure::Invalid)
        ));
    }
    assert!(matches!(
        limited.login(peer, "secret"),
        Err(LoginFailure::RateLimited)
    ));
}

#[test]
pub(crate) fn model_selection_rejects_empty_and_duplicate_pools() {
    assert!(validate_model_selection(Vec::new()).is_err());
    let model = ActiveProviderModelConfig {
        provider_id: "provider".to_string(),
        model: "model".to_string(),
    };
    assert!(validate_model_selection(vec![model.clone()]).is_ok());
    assert!(validate_model_selection(vec![model.clone(), model]).is_err());
}

#[test]
pub(crate) fn thinking_variant_validation_distinguishes_model_default_and_named_default() {
    let updates = validate_thinking_variant_updates(vec![
        ThinkingVariantUpdate {
            provider_id: " provider ".to_string(),
            model: "model-one".to_string(),
            selected: None,
        },
        ThinkingVariantUpdate {
            provider_id: "provider".to_string(),
            model: "model-two".to_string(),
            selected: Some(" default ".to_string()),
        },
    ])
    .unwrap();
    assert_eq!(updates[0].provider_id, "provider");
    assert_eq!(updates[0].selected, None);
    assert_eq!(updates[1].selected.as_deref(), Some("default"));

    assert!(validate_thinking_variant_updates(vec![
        ThinkingVariantUpdate {
            provider_id: "provider".to_string(),
            model: "model".to_string(),
            selected: None,
        },
        ThinkingVariantUpdate {
            provider_id: " provider ".to_string(),
            model: " model ".to_string(),
            selected: Some("high".to_string()),
        },
    ])
    .is_err());
}

#[test]
pub(crate) fn thinking_variant_updates_validate_before_persisting_and_can_clear_a_selection() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let config = AppConfig::default();
    let choice = config
        .active_provider_model_choices()
        .into_iter()
        .next()
        .unwrap();
    let mut preferences = ThinkingVariantPreferences::load(&paths);
    preferences.set(
        &choice.provider_id,
        &choice.model,
        Some("previous-selection".to_string()),
    );
    preferences.save(&paths).unwrap();

    let mut agent = None;
    let invalid = ThinkingVariantUpdate {
        provider_id: choice.provider_id.clone(),
        model: choice.model.clone(),
        selected: Some("definitely-not-a-real-variant".to_string()),
    };
    assert!(matches!(
        apply_thinking_variant_updates(&mut agent, &config, &paths, &[invalid]),
        Err(AdminFailure::Invalid(_))
    ));
    assert_eq!(
        ThinkingVariantPreferences::load(&paths).selected(&choice.provider_id, &choice.model),
        Some("previous-selection")
    );

    let clear = ThinkingVariantUpdate {
        provider_id: choice.provider_id.clone(),
        model: choice.model.clone(),
        selected: None,
    };
    apply_thinking_variant_updates(&mut agent, &config, &paths, &[clear]).unwrap();
    assert_eq!(
        ThinkingVariantPreferences::load(&paths).selected(&choice.provider_id, &choice.model),
        None
    );
}

#[test]
pub(crate) fn config_response_never_serializes_secret_values() {
    let mut config = AppConfig::default();
    config.providers[0].api_key = Some("provider-secret".to_string());
    config.plugins.web.tavily_api_keys = vec!["tavily-secret".to_string()];
    config.plugins.exchange_rate.api_key = "exchange-secret".to_string();
    config.plugins.image_generation.api_keys = vec!["image-secret".to_string()];
    config.plugins.api_quota.deepseek.api_key = "deepseek-secret".to_string();
    config.plugins.api_quota.openrouter.api_key = "openrouter-secret".to_string();
    let paths = tempfile::tempdir().unwrap();
    let paths = GQYPaths {
        root_dir: paths.path().to_path_buf(),
        config_dir: paths.path().join("config"),
        config_file: paths.path().join("config/config.jsonc"),
        skills_dir: paths.path().join("config/skills"),
        data_dir: paths.path().join("data"),
        cache_dir: paths.path().join("cache"),
        state_dir: paths.path().join("state"),
        pictures_dir: paths.path().join("pictures"),
        fish_hook_file: paths.path().join("fish"),
        bash_hook_file: paths.path().join("bash"),
        zsh_hook_file: paths.path().join("zsh"),
        scripts_dir: paths.path().join("scripts"),
        system_scripts_dir: paths.path().join("system-scripts"),
    };
    let response = config_response(
        &config,
        ContextSnapshot {
            tokens: 0,
            window: None,
            cumulative_tokens: 0,
            cumulative_prompt_tokens: 0,
            cumulative_cache_read_tokens: 0,
        },
        &paths,
    )
    .unwrap();
    let serialized = serde_json::to_string(&response).unwrap();
    assert!(!serialized.contains("provider-secret"));
    assert!(!serialized.contains("tavily-secret"));
    assert!(!serialized.contains("exchange-secret"));
    assert!(!serialized.contains("image-secret"));
    assert!(!serialized.contains("deepseek-secret"));
    assert!(!serialized.contains("openrouter-secret"));
    assert_eq!(response.secret_states["providers.0.api_key"], true);
    assert_eq!(response.secret_states["plugins.web.tavily_api_keys"], true);
    assert_eq!(
        response.secret_states["plugins.api_quota.deepseek.accounts.0.api_key"],
        true
    );
    assert_eq!(
        response.secret_states["plugins.api_quota.openrouter.accounts.0.api_key"],
        true
    );
    assert!(response.config.get("memory").is_some());
}

#[test]
pub(crate) fn omitted_provider_secret_does_not_follow_array_position_after_rename() {
    let mut current = AppConfig::default();
    current.providers[0].id = "first".to_string();
    current.providers[0].api_key = Some("first-secret".to_string());
    let mut candidate = current.clone();
    candidate.providers[0].id = "renamed".to_string();
    candidate.providers[0].api_key = None;
    restore_config_secrets(&mut candidate, &current, &HashMap::new()).unwrap();
    assert_eq!(candidate.providers[0].api_key, None);
}

#[test]
pub(crate) fn explicit_secret_clear_removes_a_provider_key() {
    let mut current = AppConfig::default();
    current.providers[0].api_key = Some("secret".to_string());
    let mut candidate = current.clone();
    candidate.providers[0].api_key = None;
    let mutations = HashMap::from([("providers.0.api_key".to_string(), SecretMutation::Clear)]);
    restore_config_secrets(&mut candidate, &current, &mutations).unwrap();
    assert_eq!(candidate.providers[0].api_key, None);
}

#[test]
pub(crate) fn api_quota_secrets_are_preserved_set_and_cleared() {
    let mut current = AppConfig::default();
    current.plugins.api_quota.deepseek.api_key = "deepseek-old".to_string();
    current.plugins.api_quota.openrouter.api_key = "openrouter-old".to_string();
    let mut candidate = current.clone();
    candidate.plugins.api_quota.deepseek.accounts = vec![crate::config::ApiQuotaAccountConfig {
        id: "account-1".to_string(),
        name: "默认账号".to_string(),
        api_key: String::new(),
    }];
    candidate.plugins.api_quota.openrouter.accounts = vec![crate::config::ApiQuotaAccountConfig {
        id: "account-1".to_string(),
        name: "默认账号".to_string(),
        api_key: String::new(),
    }];
    candidate.plugins.api_quota.deepseek.api_key.clear();
    candidate.plugins.api_quota.openrouter.api_key.clear();

    restore_config_secrets(&mut candidate, &current, &HashMap::new()).unwrap();
    assert_eq!(
        candidate.plugins.api_quota.deepseek.accounts[0].api_key,
        "deepseek-old"
    );
    assert_eq!(
        candidate.plugins.api_quota.openrouter.accounts[0].api_key,
        "openrouter-old"
    );

    let mutations = HashMap::from([
        (
            "plugins.api_quota.deepseek.accounts.0.api_key".to_string(),
            SecretMutation::Set("deepseek-new".to_string()),
        ),
        (
            "plugins.api_quota.openrouter.accounts.0.api_key".to_string(),
            SecretMutation::Clear,
        ),
    ]);
    restore_config_secrets(&mut candidate, &current, &mutations).unwrap();
    assert_eq!(
        candidate.plugins.api_quota.deepseek.accounts[0].api_key,
        "deepseek-new"
    );
    assert!(candidate.plugins.api_quota.openrouter.accounts[0]
        .api_key
        .is_empty());
}

#[test]
pub(crate) fn api_quota_account_ids_prevent_deleted_key_reuse() {
    let mut current = AppConfig::default();
    current.plugins.api_quota.deepseek.accounts[0] = crate::config::ApiQuotaAccountConfig {
        id: "old-id".to_string(),
        name: "账号 2".to_string(),
        api_key: "old-secret".to_string(),
    };
    let mut candidate = current.clone();
    candidate.plugins.api_quota.deepseek.accounts[0] = crate::config::ApiQuotaAccountConfig {
        id: "new-id".to_string(),
        name: "账号 2".to_string(),
        api_key: String::new(),
    };

    restore_config_secrets(&mut candidate, &current, &HashMap::new()).unwrap();
    assert!(candidate.plugins.api_quota.deepseek.accounts[0]
        .api_key
        .is_empty());

    candidate.plugins.api_quota.deepseek.accounts[0].id = "old-id".to_string();
    candidate.plugins.api_quota.deepseek.accounts[0].name = "重命名账号".to_string();
    restore_config_secrets(&mut candidate, &current, &HashMap::new()).unwrap();
    assert_eq!(
        candidate.plugins.api_quota.deepseek.accounts[0].api_key,
        "old-secret"
    );
}

#[test]
pub(crate) fn stale_event_cursor_receives_resync_marker() {
    let events = EventHub::new();
    for index in 0..=EVENT_CAPACITY {
        events.publish("test", json!({ "index": index }));
    }
    let replay = events.replay_after(0);
    assert_eq!(replay.len(), 1);
    assert_eq!(replay[0].kind, "resync_required");
    assert_eq!(replay[0].id, events.latest_id());
    let next = events.publish("after-resync", json!({}));
    assert!(next > replay[0].id);
}

#[test]
pub(crate) fn replay_after_cursor_is_ordered_and_exclusive() {
    let events = EventHub::new();
    events.publish("one", json!({}));
    events.publish("two", json!({}));
    events.publish("three", json!({}));
    let replay = events.replay_after(1);
    assert_eq!(
        replay.iter().map(|record| record.id).collect::<Vec<_>>(),
        vec![2, 3]
    );
}

#[test]
pub(crate) fn future_event_cursor_requests_resync_after_server_restart() {
    let events = EventHub::new();
    let replay = events.replay_after(42);
    assert_eq!(replay.len(), 1);
    assert_eq!(replay[0].kind, "resync_required");
}

#[test]
pub(crate) fn answer_validation_trims_values_and_rejects_control_characters() {
    let request = sample_question();
    assert_eq!(
        normalize_answers(&request, vec![vec!["  All  ".to_string()]]).unwrap(),
        vec![vec!["All".to_string()]]
    );
    assert!(normalize_answers(&request, vec![vec!["bad\nanswer".to_string()]]).is_err());
}

#[test]
pub(crate) fn invalid_answer_keeps_question_pending() {
    let broker = QuestionBroker::new();
    let (responder, mut response) = oneshot::channel();
    let question_id = broker.insert("run_test", sample_question(), responder);
    let invalid = broker.answer(&question_id, vec![Vec::new()], |_, _| {
        panic!("invalid answer must not be published")
    });
    assert!(matches!(invalid, Err(AnswerFailure::Invalid(_))));
    assert!(broker.pending.lock().unwrap().contains_key(&question_id));

    broker
        .answer(
            &question_id,
            vec![vec![" All ".to_string()]],
            |run_id, answers| {
                assert_eq!(run_id, "run_test");
                assert_eq!(answers, &vec![vec!["All".to_string()]]);
            },
        )
        .unwrap();
    assert!(matches!(
        response.try_recv().unwrap(),
        QuestionResponse::Answered(answers) if answers == vec![vec!["All".to_string()]]
    ));
}

#[test]
pub(crate) fn closed_question_responder_does_not_publish_an_answer() {
    let broker = QuestionBroker::new();
    let (responder, response) = oneshot::channel();
    drop(response);
    let question_id = broker.insert("run_test", sample_question(), responder);
    let mut published = false;
    let result = broker.answer(&question_id, vec![vec!["All".to_string()]], |_, _| {
        published = true
    });
    assert!(matches!(result, Err(AnswerFailure::Gone)));
    assert!(!published);
}

#[test]
pub(crate) fn closing_question_resumes_run_without_answers() {
    let broker = QuestionBroker::new();
    let (responder, mut response) = oneshot::channel();
    let question_id = broker.insert("run_test", sample_question(), responder);
    let mut resumed_run = None;

    broker
        .close(&question_id, |run_id| {
            assert!(response.try_recv().is_err());
            resumed_run = Some(run_id.to_string())
        })
        .unwrap();

    assert_eq!(resumed_run.as_deref(), Some("run_test"));
    assert!(matches!(
        response.try_recv().unwrap(),
        QuestionResponse::Closed
    ));
    assert!(!broker.pending.lock().unwrap().contains_key(&question_id));
}

#[test]
pub(crate) fn closed_question_receiver_does_not_publish_close_event() {
    let broker = QuestionBroker::new();
    let (responder, response) = oneshot::channel();
    drop(response);
    let question_id = broker.insert("run_test", sample_question(), responder);
    let mut published = false;

    let result = broker.close(&question_id, |_| published = true);

    assert!(matches!(result, Err(AnswerFailure::Gone)));
    assert!(!published);
}

pub(crate) fn sample_question() -> QuestionRequest {
    QuestionRequest {
        questions: vec![QuestionPrompt {
            header: "Scope".to_string(),
            question: "Which scope?".to_string(),
            options: vec![QuestionOption {
                label: "All".to_string(),
                description: String::new(),
            }],
            multiple: false,
            custom: true,
        }],
    }
}

#[test]
pub(crate) fn content_limit_counts_characters() {
    assert!(validate_content("x".repeat(MAX_CONTENT_CHARS)).is_ok());
    let error = validate_content("界".repeat(MAX_CONTENT_CHARS + 1)).unwrap_err();
    assert_eq!(error.status, StatusCode::PAYLOAD_TOO_LARGE);
}
#[test]
pub(crate) fn web_persona_rename_updates_qq_routes_and_deletion_is_rejected() {
    let mut config = AppConfig::default();
    config
        .platforms
        .qq
        .conversations
        .push(crate::config::PlatformModelRoute {
            conversation: crate::config::PlatformConversationConfig {
                kind: crate::config::PlatformConversationKind::Group,
                id: "42".to_string(),
            },
            persona: crate::config::PlatformPersonaOverride::Custom {
                name: "Old.md".to_string(),
            },
            text_models_inheritance: crate::config::PlatformModelPoolInheritance::Platform,
            text_models: None,
            multimodal_models_inheritance: crate::config::PlatformModelPoolInheritance::Platform,
            multimodal_models: None,
            extra_prompt: String::new(),
            session_limits: None,
        });
    let renamed: PromptDocuments = serde_json::from_value(json!({
        "personas": [{
            "name": "New.md",
            "content": "persona",
            "original_name": "Old.md"
        }],
        "identities": []
    }))
    .unwrap();

    reconcile_qq_persona_references(&mut config, &renamed);
    assert_eq!(
        config.platforms.qq.conversations[0].persona.custom_name(),
        Some("New.md")
    );
    assert!(validate_prompt_documents(&config, &renamed).is_ok());
    assert!(validate_prompt_documents(&config, &PromptDocuments::default()).is_err());
}

#[test]
pub(crate) fn web_persona_renames_use_the_original_reference_snapshot() {
    let route = |id: &str, persona: &str| crate::config::PlatformModelRoute {
        conversation: crate::config::PlatformConversationConfig {
            kind: crate::config::PlatformConversationKind::Group,
            id: id.to_string(),
        },
        persona: crate::config::PlatformPersonaOverride::Custom {
            name: persona.to_string(),
        },
        text_models_inheritance: crate::config::PlatformModelPoolInheritance::Platform,
        text_models: None,
        multimodal_models_inheritance: crate::config::PlatformModelPoolInheritance::Platform,
        multimodal_models: None,
        extra_prompt: String::new(),
        session_limits: None,
    };
    let mut config = AppConfig::default();
    config.platforms.qq.conversations = vec![route("1", "A.md"), route("2", "B.md")];
    let prompts: PromptDocuments = serde_json::from_value(json!({
        "personas": [
            {"name": "B.md", "content": "A", "original_name": "A.md"},
            {"name": "C.md", "content": "B", "original_name": "B.md"}
        ],
        "identities": []
    }))
    .unwrap();

    reconcile_qq_persona_references(&mut config, &prompts);

    assert_eq!(
        config.platforms.qq.conversations[0].persona.custom_name(),
        Some("B.md")
    );
    assert_eq!(
        config.platforms.qq.conversations[1].persona.custom_name(),
        Some("C.md")
    );
}

#[test]
pub(crate) fn web_rejects_persona_names_with_colliding_persistent_scopes() {
    let prompts: PromptDocuments = serde_json::from_value(json!({
        "personas": [
            {"name": "A B.md", "content": "first"},
            {"name": "A@B.md", "content": "second"}
        ],
        "identities": []
    }))
    .unwrap();

    assert!(validate_prompt_documents(&AppConfig::default(), &prompts).is_err());
}

#[test]
pub(crate) fn web_persona_scope_batch_migration_supports_swaps() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let store = StateStore::new(&paths).unwrap();
    let first = store.create_session("a", "first", "user", None).unwrap();
    let second = store.create_session("b", "second", "user", None).unwrap();

    migrate_persona_db_scopes(
        &store,
        &[
            ("a".to_string(), "b".to_string()),
            ("b".to_string(), "a".to_string()),
        ],
    )
    .unwrap();

    assert_eq!(
        store
            .session_record(&first.session_id)
            .unwrap()
            .unwrap()
            .persona,
        "b"
    );
    assert_eq!(
        store
            .session_record(&second.session_id)
            .unwrap()
            .unwrap()
            .persona,
        "a"
    );
}

/// comm 字段可以合法地包含空格和右括号(如进程改名成 "a) b"),解析必须
/// 锚定在最后一个 ')' 之后,否则字段错位会把别的数字当成 tpgid。
#[test]
pub(crate) fn stat_parse_survives_hostile_comm() {
    // 正常 fish:pgrp==tpgid(停在提示符)
    let stat = "1234 (fish) S 1000 1234 1234 34816 1234 4194304 1 0 0 0";
    assert_eq!(parse_stat_pgrp_tpgid(stat), Some((1234, 1234)));
    // comm 里嵌了 ") S 9 9 9 9":只有从最后一个 ')' 起切才对
    let stat = "1234 (a) S 9 9 9 9 (b) R 1000 1234 1234 34816 5678 4194304";
    assert_eq!(parse_stat_pgrp_tpgid(stat), Some((1234, 5678)));
    // 前台在跑别的程序:pgrp != tpgid
    let stat = "1234 (zsh) S 1000 1234 1234 34816 9999 4194304";
    assert_eq!(parse_stat_pgrp_tpgid(stat), Some((1234, 9999)));
    assert_eq!(parse_stat_pgrp_tpgid("no paren here"), None);
}

/// 真 PTY 全链路:python pty.fork 造出「会话首进程挂在 pts 上且是前台」的
/// 假 shell(exec sleep),验证 ① 在提示符判定为真 ② 写回的字节真从 master
/// 端读出来 ③ 进程死后判定翻假。覆盖 /proc 探测和 tty 写入两段真实内核路径。
#[test]
pub(crate) fn origin_tty_gates_and_writeback_against_real_pty() {
    let script = r#"
import os, pty, signal, sys
pid, master = pty.fork()
if pid == 0:
    os.execvp("sleep", ["sleep", "60"])
# 子进程是会话首进程,ctty=slave,前台进程组=自己 —— 正是 shell 停在提示符的形状。
# slave 路径从 /proc/child/fd/0 反查,不依赖 ptsname。
slave = os.readlink(f"/proc/{pid}/fd/0")
print(pid, slave, flush=True)
sys.stdin.readline()  # 等 Rust 侧写完
data = b""
try:
    while b"GQY-E2E-END" not in data:
        data += os.read(master, 4096)
except OSError:
    pass
print("DATA:" + data.hex(), flush=True)
os.kill(pid, signal.SIGKILL)
os.waitpid(pid, 0)
print("GONE", flush=True)
sys.stdin.readline()  # 等 Rust 侧完成死后判定
"#;
    use std::io::{BufRead, BufReader, Write};
    let Ok(mut child) = std::process::Command::new("python3")
        .arg("-c")
        .arg(script)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
    else {
        eprintln!("python3 unavailable; skipping pty gate test");
        return;
    };
    let mut stdin = child.stdin.take().unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let head = lines.next().unwrap().unwrap();
    let (pid, slave) = head.split_once(' ').unwrap();
    let origin = crate::ipc::OriginTty {
        path: std::path::PathBuf::from(slave),
        shell_pid: pid.parse().unwrap(),
    };

    assert!(
        origin_shell_at_prompt(&origin),
        "pty.fork 出的会话首进程应判定为「在提示符」"
    );
    // 走生产写线程:Write 分片 + Finish(flush + SIGWINCH),与流式回写同路。
    {
        use std::os::unix::fs::OpenOptionsExt;
        let tty = std::fs::OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NOCTTY)
            .open(&origin.path)
            .unwrap();
        let (ops_tx, ops_rx) = std::sync::mpsc::channel::<TtyWriteOp>();
        let shell_pid = origin.shell_pid;
        let writer = std::thread::spawn(move || origin_tty_writer(tty, shell_pid, ops_rx));
        ops_tx
            .send(TtyWriteOp::Write(
                "\x1b[1m✦ GQY 后台任务跟进\x1b[0m\r\n".to_string(),
            ))
            .unwrap();
        let mut body = String::new();
        push_rendered_line(
            "**粗体** 与 `代码` GQY-E2E-END",
            WriteLineStyle::Content,
            &mut body,
        );
        ops_tx.send(TtyWriteOp::Write(body)).unwrap();
        ops_tx.send(TtyWriteOp::Finish).unwrap();
        writer.join().unwrap();
    }
    stdin.write_all(b"written\n").unwrap();

    let data_line = loop {
        let line = lines.next().unwrap().unwrap();
        if let Some(rest) = line.strip_prefix("DATA:") {
            break rest.to_string();
        }
    };
    let bytes = (0..data_line.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&data_line[i..i + 2], 16).unwrap())
        .collect::<Vec<u8>>();
    let text = String::from_utf8_lossy(&bytes);
    assert!(
        text.contains("GQY 后台任务跟进"),
        "master 端应读到标题,实际: {text:?}"
    );
    assert!(text.contains("GQY-E2E-END"), "正文应完整到达");
    assert!(text.contains("\u{1b}["), "应带 SGR 样式");

    let gone = lines.next().unwrap().unwrap();
    assert_eq!(gone, "GONE");
    assert!(!origin_shell_at_prompt(&origin), "进程死后必须判定为不可写");
    stdin.write_all(b"done\n").unwrap();
    let _ = child.wait();
}
