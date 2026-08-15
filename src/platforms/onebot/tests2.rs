//! tests2 — 自 src/platforms/onebot.rs 外移。
#![cfg(test)]

use super::*;

fn group_trigger_matrix() {
    let at_only = OneBotConfig::default();
    let mut parsed = InboundMessage {
        text: "/cmd 查询".into(),
        ..Default::default()
    };
    assert!(group_trigger_text(&at_only, &parsed, None, 10_000).is_none());
    parsed.at_self = true;
    assert_eq!(
        group_trigger_text(&at_only, &parsed, None, 10_000).as_deref(),
        Some("/cmd 查询")
    );

    let prefix = config_with(|config| {
        config.group_chats.trigger_keywords = vec!["/cmd".into()];
    });
    parsed.at_self = false;
    assert_eq!(
        group_trigger_text(&prefix, &parsed, None, 10_000).as_deref(),
        Some("查询")
    );
    parsed.text = "无前缀".into();
    assert!(group_trigger_text(&prefix, &parsed, None, 10_000).is_none());

    // An empty keyword list never fires (avoids always-on).
    let empty_prefix = OneBotConfig::default();
    assert!(group_trigger_text(&empty_prefix, &parsed, None, 10_000).is_none());

    let either = config_with(|config| {
        config.group_chats.trigger_keywords = vec!["喵".into(), "喵喵".into()];
    });
    parsed.text = "喵喵：早上好".into();
    assert_eq!(
        group_trigger_text(&either, &parsed, None, 10_000).as_deref(),
        Some("早上好")
    );

    parsed.text = "继续说".into();
    let replied_message = PlatformMessageInfo {
        message_id: "previous".into(),
        sender_id: "10000".into(),
        sender_display_name: "GQY".into(),
        timestamp: 1,
        text: "previous reply".into(),
        reply_to_message_id: None,
        mentioned_user_ids: Vec::new(),
        mentioned_users: Vec::new(),
        media: Vec::new(),
        conversation_kind: Some(ConversationKind::Group),
        conversation_id: Some("9".to_string()),
    };
    assert_eq!(
        group_trigger_text(&at_only, &parsed, Some(&replied_message), 10_000).as_deref(),
        Some("继续说")
    );
}

#[tokio::test]
async fn internal_failures_are_silent_in_groups() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let state = test_web_state(temp.path(), 8300);
    let (handle, mut frames) = test_connection(None);
    let target = Target::Group { group_id: 42 };
    let context = Arc::new(PlatformTurnContext::new(
        unique_test_conversation(target),
        "7".to_string(),
        "seven".to_string(),
        false,
        crate::config::AppConfig::default(),
        paths.clone(),
        crate::state::StateStore::new(&paths).unwrap(),
        Arc::new(test_adapter(handle, target)),
        Arc::new(super::super::plugins::PlatformPluginRegistry::default()),
    ));

    let delivered = deliver_dispatch(
        &state,
        &context,
        TurnDispatch::Failed("provider secret".to_string()),
    )
    .await
    .unwrap();
    assert!(!delivered);
    assert!(frames.try_recv().is_err());
}

#[tokio::test]
async fn final_delivery_deduplicates_identical_image_content() {
    let temp = tempfile::tempdir().unwrap();
    let state = test_web_state(temp.path(), 8300);
    let store = state.state_store.clone();
    store
        .start_turn("image_turn", "show images", std::process::id())
        .unwrap();
    let duplicate_path = temp.path().join("duplicate.png");
    let distinct_path = temp.path().join("distinct.png");
    image::RgbaImage::from_pixel(2, 2, image::Rgba([255, 0, 0, 255]))
        .save(&duplicate_path)
        .unwrap();
    image::RgbaImage::from_pixel(2, 2, image::Rgba([0, 0, 255, 255]))
        .save(&distinct_path)
        .unwrap();
    let first = store
        .save_image_asset("image_turn", Some("tool_1"), &duplicate_path, "first")
        .unwrap();
    let duplicate = store
        .save_image_asset("image_turn", Some("tool_2"), &duplicate_path, "duplicate")
        .unwrap();
    let distinct = store
        .save_image_asset("image_turn", Some("tool_3"), &distinct_path, "distinct")
        .unwrap();
    store.complete_turn("image_turn", "done", None).unwrap();

    let (handle, mut frames) = test_connection(None);
    let target = Target::Private { user_id: 7 };
    let context = Arc::new(PlatformTurnContext::new(
        unique_test_conversation(target),
        "7".to_string(),
        "seven".to_string(),
        false,
        crate::config::AppConfig::default(),
        test_paths(temp.path()),
        store,
        Arc::new(test_adapter(handle.clone(), target)),
        Arc::new(super::super::plugins::PlatformPluginRegistry::default()),
    ));
    let dispatch = TurnDispatch::Completed(super::super::TurnOutcome {
        run_id: "run-test".to_string(),
        text: "reply".to_string(),
        provider_id: Some("provider-test".to_string()),
        model: Some("model-test".to_string()),
        image_assets: vec![first.asset_id, duplicate.asset_id, distinct.asset_id],
        suppressed_reply_ranges: Vec::new(),
        final_reply_already_sent: false,
    });
    let delivery_state = state.clone();
    let delivery_context = context.clone();
    let delivery = tokio::spawn(async move {
        deliver_dispatch(&delivery_state, &delivery_context, dispatch).await
    });

    let frame: Value = serde_json::from_str(&frames.recv().await.unwrap()).unwrap();
    let segments = frame["params"]["message"].as_array().unwrap();
    assert_eq!(
        segments
            .iter()
            .filter(|segment| segment["type"] == "image")
            .count(),
        2
    );
    route_api_response(
        &handle,
        json!({
            "status": "ok",
            "retcode": 0,
            "data": { "message_id": 70 },
            "echo": frame["echo"],
        }),
    );
    assert!(delivery.await.unwrap().unwrap());
}

#[tokio::test]
async fn final_delivery_skips_an_image_confirmed_by_a_tool_send() {
    let temp = tempfile::tempdir().unwrap();
    let state = test_web_state(temp.path(), 8300);
    let store = state.state_store.clone();
    store
        .start_turn("direct_image_turn", "draw", std::process::id())
        .unwrap();
    let image_path = temp.path().join("generated.png");
    image::RgbaImage::from_pixel(2, 2, image::Rgba([255, 0, 0, 255]))
        .save(&image_path)
        .unwrap();
    let asset = store
        .save_image_asset(
            "direct_image_turn",
            Some("generate_image"),
            &image_path,
            "generated",
        )
        .unwrap();
    store
        .complete_turn("direct_image_turn", "done", None)
        .unwrap();

    let (handle, mut frames) = test_connection(None);
    let target = Target::Private { user_id: 7 };
    let context = Arc::new(PlatformTurnContext::new(
        unique_test_conversation(target),
        "7".to_string(),
        "seven".to_string(),
        false,
        crate::config::AppConfig::default(),
        test_paths(temp.path()),
        store,
        Arc::new(test_adapter(handle.clone(), target)),
        Arc::new(super::super::plugins::PlatformPluginRegistry::default()),
    ));

    let direct_context = context.clone();
    let direct_path = image_path.clone();
    let direct_send = tokio::spawn(async move {
        direct_context
            .send(OutboundMessage::segments(
                OutboundOrigin::Tool,
                vec![OutboundSegment::ImagePath {
                    path: direct_path,
                    alt: "generated".to_string(),
                }],
            ))
            .await
    });
    let direct_frame: Value = serde_json::from_str(
        &tokio::time::timeout(Duration::from_secs(1), frames.recv())
            .await
            .expect("direct image send timed out")
            .expect("direct image frame channel closed"),
    )
    .unwrap();
    assert_eq!(
        direct_frame["params"]["message"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|segment| segment["type"] == "image")
            .count(),
        1
    );
    route_api_response(
        &handle,
        json!({
            "status": "ok",
            "retcode": 0,
            "data": { "message_id": 70 },
            "echo": direct_frame["echo"],
        }),
    );
    direct_send.await.unwrap().unwrap();

    let dispatch = TurnDispatch::Completed(super::super::TurnOutcome {
        run_id: "run-direct-image".to_string(),
        text: "画好了".to_string(),
        provider_id: Some("provider-test".to_string()),
        model: Some("model-test".to_string()),
        image_assets: vec![asset.asset_id],
        suppressed_reply_ranges: Vec::new(),
        final_reply_already_sent: false,
    });
    let delivery_state = state.clone();
    let delivery_context = context.clone();
    let delivery = tokio::spawn(async move {
        deliver_dispatch(&delivery_state, &delivery_context, dispatch).await
    });
    let final_frame: Value = serde_json::from_str(
        &tokio::time::timeout(Duration::from_secs(1), frames.recv())
            .await
            .expect("final text send timed out")
            .expect("final text frame channel closed"),
    )
    .unwrap();
    let final_segments = final_frame["params"]["message"].as_array().unwrap();
    assert!(final_segments
        .iter()
        .any(|segment| segment["data"]["text"] == "画好了"));
    assert!(!final_segments
        .iter()
        .any(|segment| segment["type"] == "image"));
    route_api_response(
        &handle,
        json!({
            "status": "ok",
            "retcode": 0,
            "data": { "message_id": 71 },
            "echo": final_frame["echo"],
        }),
    );
    assert!(delivery.await.unwrap().unwrap());
    assert!(frames.try_recv().is_err());
}

#[tokio::test]
async fn image_only_final_delivery_accepts_an_already_delivered_image() {
    let temp = tempfile::tempdir().unwrap();
    let state = test_web_state(temp.path(), 8300);
    let store = state.state_store.clone();
    store
        .start_turn("direct_only_turn", "draw", std::process::id())
        .unwrap();
    let image_path = temp.path().join("generated.png");
    image::RgbaImage::from_pixel(2, 2, image::Rgba([255, 0, 0, 255]))
        .save(&image_path)
        .unwrap();
    let asset = store
        .save_image_asset(
            "direct_only_turn",
            Some("generate_image"),
            &image_path,
            "generated",
        )
        .unwrap();
    store
        .complete_turn("direct_only_turn", "done", None)
        .unwrap();

    let (handle, mut frames) = test_connection(None);
    let target = Target::Private { user_id: 7 };
    let context = Arc::new(PlatformTurnContext::new(
        unique_test_conversation(target),
        "7".to_string(),
        "seven".to_string(),
        false,
        crate::config::AppConfig::default(),
        test_paths(temp.path()),
        store,
        Arc::new(test_adapter(handle.clone(), target)),
        Arc::new(super::super::plugins::PlatformPluginRegistry::default()),
    ));

    let direct_context = context.clone();
    let direct_path = image_path.clone();
    let direct_send = tokio::spawn(async move {
        direct_context
            .send(OutboundMessage::segments(
                OutboundOrigin::Tool,
                vec![OutboundSegment::ImagePath {
                    path: direct_path,
                    alt: "generated".to_string(),
                }],
            ))
            .await
    });
    let direct_frame: Value = serde_json::from_str(
        &tokio::time::timeout(Duration::from_secs(1), frames.recv())
            .await
            .expect("direct image send timed out")
            .expect("direct image frame channel closed"),
    )
    .unwrap();
    route_api_response(
        &handle,
        json!({
            "status": "ok",
            "retcode": 0,
            "data": { "message_id": 72 },
            "echo": direct_frame["echo"],
        }),
    );
    direct_send.await.unwrap().unwrap();

    let delivered = deliver_dispatch(
        &state,
        &context,
        TurnDispatch::Completed(super::super::TurnOutcome {
            run_id: "run-direct-only".to_string(),
            text: String::new(),
            provider_id: Some("provider-test".to_string()),
            model: Some("model-test".to_string()),
            image_assets: vec![asset.asset_id.clone()],
            suppressed_reply_ranges: Vec::new(),
            final_reply_already_sent: false,
        }),
    )
    .await
    .unwrap();
    assert!(delivered);
    assert!(frames.try_recv().is_err());

    let unresolved = deliver_dispatch(
        &state,
        &context,
        TurnDispatch::Completed(super::super::TurnOutcome {
            run_id: "run-direct-with-missing".to_string(),
            text: String::new(),
            provider_id: Some("provider-test".to_string()),
            model: Some("model-test".to_string()),
            image_assets: vec![asset.asset_id, "missing-asset".to_string()],
            suppressed_reply_ranges: Vec::new(),
            final_reply_already_sent: false,
        }),
    )
    .await
    .unwrap();
    assert!(!unresolved);
    assert!(frames.try_recv().is_err());
}

#[tokio::test]
async fn busy_model_capacity_waits_silently_without_merging_the_turn() {
    let temp = tempfile::tempdir().unwrap();
    let state = test_web_state(temp.path(), 8300);
    {
        let mut manager = state.manager.lock().unwrap();
        manager.config.platforms.qq.enabled = true;
        manager.config.platforms.qq.group_chats.allow_non_whitelist = true;
        manager
            .config
            .platforms
            .qq
            .group_chats
            .non_whitelist_rate_limit
            .max_messages = 0;
        manager.config.platforms.qq.group_chats.trigger_keywords = vec!["gqy".to_string()];
    }
    assert!(state
        .platforms
        .plugins
        .set(Ok(Arc::new(
            super::super::plugins::PlatformPluginRegistry::default()
        )))
        .is_ok());
    let all_turn_permits = state
        .platforms
        .turn_permits
        .clone()
        .acquire_many_owned(super::super::MAX_CONCURRENT_PLATFORM_TURNS as u32)
        .await
        .unwrap();
    let (handle, mut frames) = test_connection(None);
    let base = json!({
        "post_type": "message",
        "message_type": "group",
        "self_id": 10000,
        "user_id": 7,
        "group_id": 42,
        "message_id": 90,
        "group_name": "test group",
        "sender": { "nickname": "seven" },
    });

    let mut silent = base.clone();
    silent["message"] = json!([{ "type": "text", "data": { "text": "ordinary" } }]);
    handle_message(state.clone(), handle.clone(), silent, next_ingress_order()).await;
    assert!(frames.try_recv().is_err());

    let mut triggered = base;
    triggered["message"] = json!([{ "type": "text", "data": { "text": "gqy hello" } }]);
    let task = tokio::spawn(handle_message(
        state,
        handle,
        triggered,
        next_ingress_order(),
    ));
    assert!(
        tokio::time::timeout(Duration::from_millis(50), frames.recv())
            .await
            .is_err()
    );
    assert!(!task.is_finished());
    task.abort();
    let _ = task.await;
    drop(all_turn_permits);
}

#[tokio::test]
async fn same_conversation_messages_can_be_observed_in_parallel() {
    let temp = tempfile::tempdir().unwrap();
    let state = test_web_state(temp.path(), 8300);
    {
        let mut manager = state.manager.lock().unwrap();
        manager.config.platforms.qq.enabled = true;
        manager.config.platforms.qq.group_chats.allow_non_whitelist = true;
        manager
            .config
            .platforms
            .qq
            .group_chats
            .non_whitelist_rate_limit
            .max_messages = 0;
    }
    let (observed_tx, mut observed_rx) = mpsc::unbounded_channel();
    let release_first = Arc::new(tokio::sync::Notify::new());
    assert!(state
        .platforms
        .plugins
        .set(Ok(Arc::new(
            super::super::plugins::PlatformPluginRegistry::new(vec![Arc::new(
                BlockingObserverPlugin {
                    observed: observed_tx,
                    release_first: release_first.clone(),
                },
            )])
        )))
        .is_ok());
    let (handle, _frames) = test_connection(None);
    let event = |message_id: i64| {
        json!({
            "post_type": "message",
            "message_type": "group",
            "self_id": 10000,
            "user_id": 7,
            "group_id": 42,
            "group_name": "test group",
            "message_id": message_id,
            "message": [{ "type": "text", "data": { "text": "ordinary" } }],
            "sender": { "nickname": "seven" },
        })
    };

    let first = tokio::spawn(handle_message(
        state.clone(),
        handle.clone(),
        event(1),
        next_ingress_order(),
    ));
    assert_eq!(observed_rx.recv().await.as_deref(), Some("1"));

    let second = tokio::spawn(handle_message(
        state.clone(),
        handle,
        event(2),
        next_ingress_order(),
    ));
    assert_eq!(
        tokio::time::timeout(Duration::from_millis(50), observed_rx.recv())
            .await
            .unwrap()
            .as_deref(),
        Some("2")
    );

    release_first.notify_one();
    first.await.unwrap();
    second.await.unwrap();
}

#[tokio::test]
async fn same_conversation_judgements_reuse_parallel_turn_admission() {
    let temp = tempfile::tempdir().unwrap();
    let state = test_web_state(temp.path(), 8300);
    {
        let mut manager = state.manager.lock().unwrap();
        manager.config.platforms.qq.enabled = true;
        manager.config.platforms.qq.group_chats.allow_non_whitelist = true;
        manager.config.platforms.qq.session_limits = crate::config::PlatformSessionLimits {
            running: 2,
            queued: 2,
        };
        manager
            .config
            .platforms
            .qq
            .group_chats
            .non_whitelist_rate_limit
            .max_messages = 0;
    }
    let (entered_tx, mut entered_rx) = mpsc::unbounded_channel();
    let barrier = Arc::new(tokio::sync::Barrier::new(3));
    assert!(state
        .platforms
        .plugins
        .set(Ok(Arc::new(
            super::super::plugins::PlatformPluginRegistry::new(vec![Arc::new(
                BlockingJudgePlugin {
                    entered: entered_tx,
                    barrier: barrier.clone(),
                },
            )])
        )))
        .is_ok());
    let (handle, _frames) = test_connection(None);
    let event = |message_id: i64, user_id: i64| {
        json!({
            "post_type": "message",
            "message_type": "group",
            "self_id": 10000,
            "user_id": user_id,
            "group_id": 42,
            "group_name": "test group",
            "message_id": message_id,
            "message": [{ "type": "text", "data": { "text": "ordinary" } }],
            "sender": { "nickname": user_id.to_string() },
        })
    };

    let first = tokio::spawn(handle_message(
        state.clone(),
        handle.clone(),
        event(1, 7),
        next_ingress_order(),
    ));
    let second = tokio::spawn(handle_message(
        state.clone(),
        handle,
        event(2, 8),
        next_ingress_order(),
    ));
    let entered = tokio::time::timeout(Duration::from_secs(1), async {
        let mut ids = vec![
            entered_rx.recv().await.unwrap(),
            entered_rx.recv().await.unwrap(),
        ];
        ids.sort();
        ids
    })
    .await
    .expect("both judgements should enter under the shared running=2 limit");
    assert_eq!(entered, ["1", "2"]);
    barrier.wait().await;
    first.await.unwrap();
    second.await.unwrap();
    assert!(state
        .platforms
        .session_turn_locks
        .lock()
        .unwrap()
        .is_empty());
}

#[test]
fn admission_matrix_uses_private_and_group_conversation_buckets() {
    let mut config = OneBotConfig::default();
    config.admin_users.push(1);
    config.private_chats.whitelist.push(2);
    config.group_chats.whitelist.push(10);

    let admin = admission_for(&config, Target::Group { group_id: 99 }, 100, 1);
    assert!(admin.allowed);
    assert!(admin.rate_key.is_none());
    assert!(admin.use_non_whitelist_text_models);

    let private_admin = admission_for(&config, Target::Private { user_id: 1 }, 100, 1);
    assert!(private_admin.allowed);
    assert!(!private_admin.use_non_whitelist_text_models);

    let private_whitelist = admission_for(&config, Target::Private { user_id: 2 }, 100, 2);
    assert!(private_whitelist.allowed);
    assert!(private_whitelist.rate_key.is_none());
    assert!(!private_whitelist.use_non_whitelist_text_models);

    let private_guest = admission_for(&config, Target::Private { user_id: 3 }, 100, 3);
    assert!(private_guest.allowed);
    assert_eq!(private_guest.rate_limit.max_messages, 2);
    assert_eq!(private_guest.rate_limit.window_seconds, 600);
    assert_eq!(private_guest.rate_key.as_deref(), Some("qq:100:private:3"));
    assert!(private_guest.use_non_whitelist_text_models);

    let group_whitelist = admission_for(&config, Target::Group { group_id: 10 }, 100, 2);
    assert!(group_whitelist.allowed);
    assert_eq!(group_whitelist.rate_limit.max_messages, 30);
    assert_eq!(group_whitelist.rate_limit.window_seconds, 60);
    assert!(group_whitelist.rate_key.is_none());
    assert!(!group_whitelist.use_non_whitelist_text_models);

    let group_guest = admission_for(&config, Target::Group { group_id: 11 }, 100, 3);
    assert!(group_guest.allowed);
    assert_eq!(group_guest.rate_limit.max_messages, 2);
    assert_eq!(group_guest.rate_limit.window_seconds, 600);
    assert_eq!(group_guest.rate_key.as_deref(), Some("qq:100:group:11"));
    assert!(group_guest.use_non_whitelist_text_models);

    let privileged_group_guest = admission_for(&config, Target::Group { group_id: 11 }, 100, 2);
    assert!(privileged_group_guest.allowed);
    assert!(privileged_group_guest.rate_key.is_none());
    assert!(privileged_group_guest.use_non_whitelist_text_models);

    config.private_chats.allow_non_whitelist = false;
    config.group_chats.allow_non_whitelist = false;
    assert!(!admission_for(&config, Target::Private { user_id: 3 }, 100, 3).allowed);
    assert!(!admission_for(&config, Target::Group { group_id: 11 }, 100, 3).allowed);
    let privileged_disallowed_group =
        admission_for(&config, Target::Group { group_id: 11 }, 100, 2);
    assert!(!privileged_disallowed_group.allowed);
    assert!(privileged_disallowed_group.rate_key.is_none());
    assert!(privileged_disallowed_group.use_non_whitelist_text_models);
}

#[test]
fn admission_materializes_the_effective_text_model_pool() {
    let mut base = crate::config::AppConfig::default();
    let provider_id = base.providers[0].id.clone();
    let pool = |model: &str| {
        vec![crate::config::ActiveProviderModelConfig {
            provider_id: provider_id.clone(),
            model: model.to_string(),
        }]
    };
    base.active_provider_models = Some(pool("global"));
    base.platforms.qq.text_models = Some(pool("platform"));
    base.platforms.qq.non_whitelist_text_models = Some(pool("non-whitelist"));
    base.platforms.qq.admin_users.push(1);
    base.platforms.qq.private_chats.whitelist.push(2);
    base.platforms.qq.group_chats.whitelist.push(10);

    for (target, user_id, expected) in [
        (Target::Private { user_id: 1 }, 1, "platform"),
        (Target::Private { user_id: 2 }, 2, "platform"),
        (Target::Private { user_id: 3 }, 3, "non-whitelist"),
        (Target::Group { group_id: 10 }, 3, "platform"),
        (Target::Group { group_id: 11 }, 1, "non-whitelist"),
    ] {
        let mut config = base.clone();
        let admission = admission_for(&config.platforms.qq, target, 100, user_id);
        apply_admission_text_model_pool(&mut config, target, &admission);
        assert_eq!(
            config.active_provider_models.as_ref().unwrap()[0].model,
            expected
        );
    }
}

#[test]
fn dynamic_access_grants_feed_the_same_admission_matrix_for_every_bot() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let state = StateStore::new(&paths).unwrap();
    let actor = crate::state::PlatformAccessActor {
        platform: "onebot".to_string(),
        account_id: "100".to_string(),
        user_id: "42".to_string(),
        conversation_kind: "private".to_string(),
        conversation_id: "42".to_string(),
        message_id: "message-1".to_string(),
    };
    for (permission, target_id) in [
        (
            crate::platforms::access_control::AccessPermission::Administrator,
            "1",
        ),
        (
            crate::platforms::access_control::AccessPermission::PrivateWhitelist,
            "2",
        ),
        (
            crate::platforms::access_control::AccessPermission::GroupWhitelist,
            "10",
        ),
    ] {
        state
            .add_platform_access_grant(
                &crate::platforms::access_control::global_grant_key(
                    permission,
                    target_id.to_string(),
                ),
                &actor,
            )
            .unwrap();
    }
    let mut config = OneBotConfig::default();
    config.private_chats.allow_non_whitelist = false;
    config.group_chats.allow_non_whitelist = false;

    let admin = admission_for_with_state(&config, &state, Target::Group { group_id: 99 }, 999, 1);
    assert!(admin.allowed);
    assert!(admin.rate_key.is_none());
    assert!(admin.use_non_whitelist_text_models);

    let private_admin =
        admission_for_with_state(&config, &state, Target::Private { user_id: 1 }, 999, 1);
    assert!(private_admin.allowed);
    assert!(!private_admin.use_non_whitelist_text_models);

    let private_whitelist =
        admission_for_with_state(&config, &state, Target::Private { user_id: 2 }, 999, 2);
    assert!(private_whitelist.allowed);
    assert!(private_whitelist.rate_key.is_none());
    assert!(!private_whitelist.use_non_whitelist_text_models);

    let group_whitelist =
        admission_for_with_state(&config, &state, Target::Group { group_id: 10 }, 999, 3);
    assert!(group_whitelist.allowed);
    assert_eq!(
        group_whitelist.rate_limit,
        config.group_chats.whitelist_rate_limit
    );
    assert_eq!(group_whitelist.rate_key.as_deref(), Some("qq:999:group:10"));
    assert!(!group_whitelist.use_non_whitelist_text_models);
}

#[test]
fn friend_request_access_uses_admins_private_whitelist_and_dynamic_grants() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let state = StateStore::new(&paths).unwrap();
    let actor = crate::state::PlatformAccessActor {
        platform: "onebot".to_string(),
        account_id: "100".to_string(),
        user_id: "42".to_string(),
        conversation_kind: "private".to_string(),
        conversation_id: "42".to_string(),
        message_id: "message-1".to_string(),
    };
    for (permission, target_id) in [
        (
            crate::platforms::access_control::AccessPermission::Administrator,
            "3",
        ),
        (
            crate::platforms::access_control::AccessPermission::PrivateWhitelist,
            "4",
        ),
    ] {
        state
            .add_platform_access_grant(
                &crate::platforms::access_control::global_grant_key(permission, target_id),
                &actor,
            )
            .unwrap();
    }
    let mut config = OneBotConfig::default();
    config.admin_users.push(1);
    config.private_chats.whitelist.push(2);

    assert!(friend_request_allowed(&config, &state, 999, 1));
    assert!(friend_request_allowed(&config, &state, 999, 2));
    assert!(friend_request_allowed(&config, &state, 100, 3));
    assert!(friend_request_allowed(&config, &state, 100, 4));
    assert!(!friend_request_allowed(&config, &state, 100, 5));

    config
        .private_chats
        .friend_requests_require_private_whitelist = false;
    assert!(friend_request_allowed(&config, &state, 100, 5));
}

#[tokio::test]
async fn friend_request_handler_accepts_allowed_requests_and_leaves_others_pending() {
    let temp = tempfile::tempdir().unwrap();
    let state = test_web_state(temp.path(), 8300);
    {
        let mut manager = state.manager.lock().unwrap();
        manager.config.platforms.qq.enabled = true;
        manager.config.platforms.qq.private_chats.whitelist.push(42);
    }
    let (handle, mut frames) = test_connection(None);

    let task = tokio::spawn(handle_friend_add_request(
        state.clone(),
        handle.clone(),
        friend_request_event(42, "flag-42"),
    ));
    let request: Value = serde_json::from_str(&frames.recv().await.unwrap()).unwrap();
    assert_eq!(request["action"], "set_friend_add_request");
    assert_eq!(request["params"]["flag"], "flag-42");
    assert_eq!(request["params"]["approve"], true);
    route_api_response(
        &handle,
        json!({
            "status": "ok",
            "retcode": 0,
            "data": null,
            "echo": request["echo"],
        }),
    );
    task.await.unwrap();
    assert!(frames.try_recv().is_err());

    handle_friend_add_request(
        state.clone(),
        handle.clone(),
        friend_request_event(43, "flag-43"),
    )
    .await;
    assert!(frames.try_recv().is_err());

    state
        .manager
        .lock()
        .unwrap()
        .config
        .platforms
        .qq
        .private_chats
        .friend_requests_require_private_whitelist = false;
    let task = tokio::spawn(handle_friend_add_request(
        state,
        handle.clone(),
        friend_request_event(44, "flag-44"),
    ));
    let request: Value = serde_json::from_str(&frames.recv().await.unwrap()).unwrap();
    assert_eq!(request["action"], "set_friend_add_request");
    assert_eq!(request["params"]["flag"], "flag-44");
    assert_eq!(request["params"]["approve"], true);
    route_api_response(
        &handle,
        json!({
            "status": "ok",
            "retcode": 0,
            "data": null,
            "echo": request["echo"],
        }),
    );
    task.await.unwrap();
    assert!(frames.try_recv().is_err());
}
