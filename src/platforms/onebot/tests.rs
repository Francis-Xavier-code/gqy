//! tests — 自 src/platforms/onebot.rs 外移。
#![cfg(test)]

use super::tests3::*;
use super::*;

#[cfg(test)]
use crate::paths::GQYPaths;

/// issue #29:唤醒合成事件必须继承发起者身份,不能伪装成机器人自己。
#[test]
fn wake_sender_inherits_recorded_initiator() {
    let group = Target::Group { group_id: 777 };
    assert_eq!(wake_sender_user_id(Some("10086"), group, 999), 10086);
    let private = Target::Private { user_id: 555 };
    assert_eq!(wake_sender_user_id(Some("10086"), private, 999), 10086);
}

#[test]
fn wake_sender_falls_back_to_private_peer_then_self() {
    // 私聊无记录:对端就是这个私聊唯一的人类。
    let private = Target::Private { user_id: 555 };
    assert_eq!(wake_sender_user_id(None, private, 999), 555);
    assert_eq!(wake_sender_user_id(Some("not-a-number"), private, 999), 555);
    // 群聊无记录:保持 self_id,不凭空授予任何成员的权限。
    let group = Target::Group { group_id: 777 };
    assert_eq!(wake_sender_user_id(None, group, 999), 999);
}

pub(crate) fn test_paths(root: &std::path::Path) -> GQYPaths {
    GQYPaths {
        root_dir: root.to_path_buf(),
        config_dir: root.join("config"),
        config_file: root.join("config/config.jsonc"),
        skills_dir: root.join("config/skills"),
        data_dir: root.join("data"),
        cache_dir: root.join("cache"),
        state_dir: root.join("state"),
        pictures_dir: root.join("pictures"),
        fish_hook_file: root.join("fish"),
        bash_hook_file: root.join("bash"),
        zsh_hook_file: root.join("zsh"),
        scripts_dir: root.join("scripts"),
        system_scripts_dir: root.join("system-scripts"),
    }
}

pub(crate) fn test_web_state(root: &std::path::Path, web_port: u16) -> DaemonState {
    DaemonState::for_test(test_paths(root), web_port).unwrap()
}

pub(crate) fn config_with(mutate: impl FnOnce(&mut OneBotConfig)) -> OneBotConfig {
    let mut config = OneBotConfig::default();
    mutate(&mut config);
    config
}

pub(crate) fn friend_request_event(user_id: i64, flag: &str) -> Value {
    json!({
        "post_type": "request",
        "request_type": "friend",
        "self_id": 10000,
        "user_id": user_id,
        "flag": flag,
    })
}

pub(crate) struct BlockingObserverPlugin {
    pub(crate) observed: mpsc::UnboundedSender<String>,
    pub(crate) release_first: Arc<tokio::sync::Notify>,
}

pub(crate) struct BlockingJudgePlugin {
    pub(crate) entered: mpsc::UnboundedSender<String>,
    pub(crate) barrier: Arc<tokio::sync::Barrier>,
}

impl super::super::plugins::PlatformPlugin for BlockingJudgePlugin {
    fn descriptor(&self) -> super::super::plugins::PluginDescriptor {
        super::super::plugins::PluginDescriptor {
            id: "test_parallel_judge",
            priority: 1,
            default_enabled: true,
        }
    }

    fn decide_trigger<'a>(
        &'a self,
        _context: &'a PlatformTurnContext,
        event: &'a PlatformInboundEvent,
        decision: &'a mut TriggerDecision,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            self.entered.send(event.message_id.clone()).unwrap();
            self.barrier.wait().await;
            decision.should_reply = false;
            Ok(())
        })
    }
}

impl super::super::plugins::PlatformPlugin for BlockingObserverPlugin {
    fn descriptor(&self) -> super::super::plugins::PluginDescriptor {
        super::super::plugins::PluginDescriptor {
            id: "test_fifo_observer",
            priority: 1,
            default_enabled: true,
        }
    }

    fn observe_inbound<'a>(
        &'a self,
        _context: &'a PlatformTurnContext,
        event: &'a PlatformInboundEvent,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            self.observed.send(event.message_id.clone()).unwrap();
            if event.message_id == "1" {
                self.release_first.notified().await;
            }
            Ok(())
        })
    }
}

#[test]
fn group_name_cache_is_ttl_bound_and_capacity_bound() {
    let mut cache = GroupNameCache::default();
    let start = Instant::now();
    cache.insert((1, 1), "first".to_string(), start);
    assert_eq!(
        cache.get((1, 1), start + Duration::from_secs(1)).as_deref(),
        Some("first")
    );
    assert!(cache.get((1, 1), start + GROUP_NAME_CACHE_TTL).is_none());

    for group_id in 0..=GROUP_NAME_CACHE_CAPACITY as i64 {
        cache.insert(
            (1, group_id),
            group_id.to_string(),
            start + Duration::from_secs(2),
        );
    }
    assert!(cache.entries.len() <= GROUP_NAME_CACHE_CAPACITY);
}

#[tokio::test]
async fn mentioned_member_name_is_resolved_and_cached() {
    let (handle, mut frames) = test_connection(None);
    let lookup = {
        let handle = handle.clone();
        tokio::spawn(async move {
            resolve_mentioned_users(
                &handle,
                91_001,
                Target::Group { group_id: 91_002 },
                &["91003".to_string()],
            )
            .await
        })
    };
    let frame: Value = serde_json::from_str(&frames.recv().await.unwrap()).unwrap();
    assert_eq!(frame["action"], "get_group_member_info");
    assert_eq!(frame["params"]["group_id"], 91_002);
    assert_eq!(frame["params"]["user_id"], "91003");
    route_api_response(
        &handle,
        json!({
            "status": "ok",
            "retcode": 0,
            "data": {
                "group_id": 91_002,
                "user_id": 91_003,
                "nickname": "fallback",
                "card": "yuyi"
            },
            "echo": frame["echo"]
        }),
    );
    let mentioned = lookup.await.unwrap();
    assert_eq!(mentioned[0].user_id, "91003");
    assert_eq!(mentioned[0].display_name.as_deref(), Some("yuyi"));

    let cached = resolve_mentioned_users(
        &handle,
        91_001,
        Target::Group { group_id: 91_002 },
        &["91003".to_string()],
    )
    .await;
    assert_eq!(cached[0].display_name.as_deref(), Some("yuyi"));
    assert!(frames.try_recv().is_err());
}

#[test]
fn group_name_metadata_prefers_event_values_and_sanitizes_names() {
    let event = json!({
        "group_name": "  Engineering  ",
        "group": { "name": "fallback" }
    });
    assert_eq!(event_group_name(&event).as_deref(), Some("Engineering"));
    assert!(normalized_group_name("bad\nname").is_none());
    assert!(normalized_group_name("").is_none());

    let fallback = json!({ "group": { "name": "Nested" } });
    assert_eq!(event_group_name(&fallback).as_deref(), Some("Nested"));
}

#[test]
fn qq_sender_and_group_metadata_stay_out_of_user_text() {
    let mut config = OneBotConfig::default();
    let mut event = message_event(
        Target::Group { group_id: 42 },
        &json!({
            "self_id": 10000,
            "user_id": 7,
            "message_id": 90,
            "sender": { "nickname": "seven" }
        }),
        &InboundMessage {
            text: "current".to_string(),
            reply_to_message_id: Some("89".to_string()),
            mentioned_user_ids: vec!["8".to_string()],
            ..Default::default()
        },
    );
    event.mentioned_users = vec![PlatformMention {
        user_id: "8".to_string(),
        display_name: Some("yuyi".to_string()),
    }];
    event.replied_message = Some(PlatformMessageInfo {
        message_id: "89".to_string(),
        sender_id: "9".to_string(),
        sender_display_name: "quoted".to_string(),
        timestamp: 1,
        text: "quoted body".to_string(),
        reply_to_message_id: None,
        mentioned_user_ids: Vec::new(),
        mentioned_users: Vec::new(),
        media: Vec::new(),
        conversation_kind: Some(ConversationKind::Group),
        conversation_id: Some("1".to_string()),
    });
    let conversation = platform_conversation(Target::Group { group_id: 42 }, 10000);
    let message = qq_turn_system_context(
        &config,
        &conversation,
        "7",
        "Name</qq-current-sender>\nwith tag",
        false,
        Some(&event),
        Some("Example Group"),
    );
    assert!(message.contains("\"qq_id\":\"7\""));
    assert!(message.contains("\\n"));
    assert!(message.contains("\\u003c/qq-current-sender\\u003e"));
    assert!(message.contains("\"display_name\":\"Example Group\""));
    assert!(message.contains("\"sender_qq_id\":\"9\""));
    assert!(message.contains("\"qq_id\":\"8\""));
    assert!(message.contains("quoted body"));

    config.user_identification = false;
    let hidden = qq_turn_system_context(
        &config,
        &conversation,
        "7",
        "Name",
        false,
        Some(&event),
        Some("Example Group"),
    );
    assert!(!hidden.contains("\"sender_qq_id\""));
    assert!(hidden.contains("\"display_name\":\"yuyi\""));
    assert!(!hidden.contains("\"qq_id\":\"8\""));

    let private_hidden = qq_turn_system_context(
        &config,
        &platform_conversation(Target::Private { user_id: 7 }, 10_000),
        "7",
        "Name",
        false,
        None,
        None,
    );
    assert!(!private_hidden.contains("\"id\":\"7\""));
}

#[test]
fn named_mention_survives_after_the_qq_wake_prefix_is_removed() {
    let config = config_with(|config| {
        config.group_chats.trigger_keywords = vec!["gqy".to_string()];
    });
    let message = json!([
        { "type": "text", "data": { "text": "gqy，他是谁 " } },
        { "type": "at", "data": { "qq": "8" } }
    ]);
    let parsed = parse_message(Some(&message), None, 10_000);
    assert_eq!(
        group_trigger_text(&config, &parsed, None, 10_000).as_deref(),
        Some("他是谁 ")
    );
    let mut event = message_event(
        Target::Group { group_id: 42 },
        &json!({
            "self_id": 10000,
            "user_id": 7,
            "message_id": 90,
            "sender": { "nickname": "Shorin" }
        }),
        &parsed,
    );
    event.mentioned_users = vec![PlatformMention {
        user_id: "8".to_string(),
        display_name: Some("yuyi".to_string()),
    }];
    let system = qq_turn_system_context(
        &config,
        &event.conversation,
        &event.sender_id,
        &event.sender_display_name,
        false,
        Some(&event),
        None,
    );
    assert!(system.contains("\"display_name\":\"yuyi\""));
    assert!(system.contains("\"qq_id\":\"8\""));
    assert!(!parsed.text.contains("yuyi"));
}

#[test]
fn trusted_qq_mapping_binds_identity_without_trusting_the_nickname() {
    let mut config = config_with(|config| {
        config.admin_users = vec![7];
    });
    let settings = RealContextPluginSettings {
        identity_mappings: vec![crate::config::RealContextIdentityMapping {
            nickname: "shorin".to_string(),
            user_id: 7,
        }],
        ..RealContextPluginSettings::default()
    };
    config.plugins.insert(
        REAL_CONTEXT_PLUGIN_ID.to_string(),
        crate::config::PlatformPluginInstanceConfig {
            enabled: Some(false),
            settings: serde_json::to_value(settings)
                .unwrap()
                .as_object()
                .unwrap()
                .clone(),
        },
    );
    let conversation = platform_conversation(Target::Private { user_id: 7 }, 10_000);
    let bound = qq_turn_system_context(
        &config,
        &conversation,
        "7",
        "completely different nickname",
        true,
        None,
        None,
    );
    assert!(bound.contains("\"canonical_identity\":\"shorin\""));
    assert!(bound.contains("\"is_admin\":true"));

    let impersonator = qq_turn_system_context(
        &config,
        &platform_conversation(Target::Private { user_id: 8 }, 10_000),
        "8",
        "shorin",
        false,
        None,
        None,
    );
    assert!(impersonator.contains("\"canonical_identity\":null"));
    assert!(impersonator.contains("\"protected_identity_conflict\":\"shorin\""));
    assert!(impersonator.contains("\"is_admin\":false"));

    let parsed = InboundMessage {
        text: "他是谁".to_string(),
        mentioned_user_ids: vec!["7".to_string()],
        ..InboundMessage::default()
    };
    let mut event = message_event(
        Target::Group { group_id: 42 },
        &json!({
            "self_id": 10000,
            "user_id": 8,
            "message_id": 91,
            "sender": { "nickname": "ordinary" }
        }),
        &parsed,
    );
    event.mentioned_users = vec![PlatformMention {
        user_id: "7".to_string(),
        display_name: Some("owner".to_string()),
    }];
    let ordinary_mention = qq_turn_system_context(
        &config,
        &event.conversation,
        &event.sender_id,
        &event.sender_display_name,
        false,
        Some(&event),
        None,
    );
    assert!(!ordinary_mention.contains("\"canonical_identity\":\"shorin\""));
}

#[test]
fn generated_mentions_are_ordered_deduplicated_and_separated() {
    let mut segments = vec![text_segment("正文")];
    prepend_response_target(
        &mut segments,
        &ResponseTarget {
            message_id: String::new(),
            user_id: "123".to_string(),
            quote: false,
            mention: true,
            explicit_mention_user_ids: vec![
                "123".to_string(),
                "456".to_string(),
                "456".to_string(),
            ],
        },
    );
    assert_eq!(segments[0]["type"], "at");
    assert_eq!(segments[0]["data"]["qq"], "123");
    assert_eq!(segments[1]["type"], "text");
    assert_eq!(segments[1]["data"]["text"], " ");
    assert_eq!(segments[2]["type"], "at");
    assert_eq!(segments[2]["data"]["qq"], "456");
    assert_eq!(segments[3]["type"], "text");
    assert_eq!(segments[3]["data"]["text"], " ");
    assert_eq!(segments[4]["data"]["text"], "正文");
}

#[tokio::test]
async fn listener_rebind_is_transactional_and_reuses_the_web_port() {
    let temp = tempfile::tempdir().unwrap();
    let web_listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let web_port = web_listener.local_addr().unwrap().port();
    let state = test_web_state(temp.path(), web_port);
    let listener = state.platforms.qq_listener.clone();

    let shared = config_with(|config| {
        config.enabled = true;
        config.reverse_ws_port = web_port;
    });
    listener
        .prepare(&state, None, &shared)
        .await
        .unwrap()
        .commit();
    {
        let inner = listener.inner.lock().unwrap();
        assert_eq!(inner.active_port, Some(web_port));
        assert!(inner.task.is_none());
    }

    let available = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let dedicated_port = available.local_addr().unwrap().port();
    drop(available);
    let dedicated = config_with(|config| {
        config.enabled = true;
        config.reverse_ws_port = dedicated_port;
    });
    listener
        .prepare(&state, Some(&shared), &dedicated)
        .await
        .unwrap()
        .commit();
    {
        let inner = listener.inner.lock().unwrap();
        assert_eq!(inner.active_port, Some(dedicated_port));
        assert!(inner.task.is_some());
    }

    let occupied = tokio::net::TcpListener::bind(("0.0.0.0", 0)).await.unwrap();
    let occupied_port = occupied.local_addr().unwrap().port();
    let conflict = config_with(|config| {
        config.enabled = true;
        config.reverse_ws_port = occupied_port;
    });
    assert!(listener
        .prepare(&state, Some(&dedicated), &conflict)
        .await
        .is_err());
    {
        let inner = listener.inner.lock().unwrap();
        assert_eq!(inner.active_port, Some(dedicated_port));
        assert!(inner.task.is_some());
    }

    let disabled = OneBotConfig::default();
    listener
        .prepare(&state, Some(&dedicated), &disabled)
        .await
        .unwrap()
        .commit();
    let inner = listener.inner.lock().unwrap();
    assert_eq!(inner.active_port, None);
    assert!(inner.task.is_none());
}

#[tokio::test]
async fn default_qq_port_follows_the_web_fallback_port() {
    let temp = tempfile::tempdir().unwrap();
    let web_listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let web_port = web_listener.local_addr().unwrap().port();
    assert_ne!(web_port, crate::ipc::DEFAULT_WEB_PORT);
    let state = test_web_state(temp.path(), web_port);
    let listener = state.platforms.qq_listener.clone();
    let config = config_with(|config| config.enabled = true);

    assert_eq!(effective_reverse_ws_port(&state, &config), Some(web_port));
    listener
        .prepare(&state, None, &config)
        .await
        .unwrap()
        .commit();

    let inner = listener.inner.lock().unwrap();
    assert_eq!(inner.active_port, Some(web_port));
    assert!(inner.task.is_none());
}

#[test]
fn parses_segment_arrays_with_mixed_content() {
    let message = json!([
        { "type": "at", "data": { "qq": "10001" } },
        { "type": "text", "data": { "text": " 你好" } },
        { "type": "image", "data": { "file": "x.jpg", "url": "https://img.example/x.jpg" } },
        { "type": "image", "data": { "file": "base64://aGk=" } },
        { "type": "file", "data": { "file_id": "f1", "file_name": "报告.pdf" } },
        { "type": "reply", "data": { "id": "5" } },
    ]);
    let parsed = parse_message(Some(&message), None, 10001);
    assert!(parsed.at_self);
    assert_eq!(parsed.text, " 你好");
    assert_eq!(parsed.images.len(), 2);
    assert!(matches!(&parsed.images[0], MediaRef::Url(url) if url == "https://img.example/x.jpg"));
    assert!(matches!(&parsed.images[1], MediaRef::Bytes(bytes) if bytes == b"hi"));
    assert_eq!(parsed.files.len(), 1);
    assert_eq!(parsed.files[0].name, "报告.pdf");
    assert_eq!(parsed.files[0].file_id.as_deref(), Some("f1"));
    assert_eq!(parsed.reply_to_message_id.as_deref(), Some("5"));
    assert_eq!(parsed.mentioned_user_ids, vec!["10001"]);
    assert_eq!(parsed.media.len(), 3);
    assert_eq!(parsed.media[0].kind, PlatformMediaKind::Image);
    assert_eq!(parsed.media[2].kind, PlatformMediaKind::File);
    let inbound = message_event(
        Target::Group { group_id: 42 },
        &json!({
            "self_id": 10001,
            "user_id": 7,
            "message_id": 90
        }),
        &parsed,
    );
    assert!(inbound.mentioned_bot);

    // Someone else being @-ed does not wake the bot.
    let other = json!([{ "type": "at", "data": { "qq": "999" } }]);
    assert!(!parse_message(Some(&other), None, 10001).at_self);
}

#[test]
fn ingress_history_event_uses_bound_account_and_supports_private_messages() {
    let frame = json!({
        "post_type": "message",
        "message_type": "private",
        "user_id": 42,
        "message_id": 90,
        "time": 123,
        "sender": { "nickname": "Alice" },
        "message": [
            { "type": "text", "data": { "text": "hello" } },
            { "type": "image", "data": { "file": "photo.jpg" } }
        ]
    });

    let inbound = ingress_message_event(&frame, 10001, 7, None).unwrap();
    assert_eq!(inbound.conversation.account_id, "10001");
    assert_eq!(inbound.conversation.kind, ConversationKind::Private);
    assert_eq!(inbound.conversation.conversation_id, "42");
    assert_eq!(inbound.ingress_order, Some(7));
    assert_eq!(inbound.text, "hello");
    assert_eq!(inbound.media.len(), 1);

    let bot_echo = json!({
        "post_type": "message",
        "message_type": "private",
        "user_id": 10001,
        "message_id": 91,
        "message": "echo"
    });
    assert!(ingress_message_event(&bot_echo, 10001, 8, None).is_none());
}

#[test]
fn cq_string_images_use_the_same_model_input_parser_as_segment_arrays() {
    let message = json!(
        "说明[CQ:image,file=https://img.example/a.png,url=https://img.example/a&#44;b.png][CQ:image,file=base64://aGk=]"
    );
    let parsed = parse_message(Some(&message), None, 10001);

    assert_eq!(parsed.text, "说明");
    assert_eq!(parsed.images.len(), 2);
    assert!(matches!(
        &parsed.images[0],
        MediaRef::Url(url) if url == "https://img.example/a,b.png"
    ));
    assert!(matches!(&parsed.images[1], MediaRef::Bytes(bytes) if bytes == b"hi"));
    assert_eq!(parsed.media.len(), 2);
    assert!(parsed
        .media
        .iter()
        .all(|media| media.kind == PlatformMediaKind::Image));
    let mention = json!("[CQ:at,qq=10001]你好");
    let parsed = parse_message(Some(&mention), None, 10001);
    assert!(parsed.at_self);
    let inbound = message_event(
        Target::Group { group_id: 42 },
        &json!({ "self_id": 10001, "user_id": 7, "message_id": 91 }),
        &parsed,
    );
    assert!(inbound.mentioned_bot);
}

#[test]
fn ordered_history_image_sources_preserve_duplicate_positions() {
    let message = json!([
        { "type": "image", "data": { "file": "base64://AQID" } },
        { "type": "image", "data": { "file": "base64://AQID" } }
    ]);

    let sources = ordered_message_image_sources(Some(&message), None);
    assert_eq!(sources.len(), 2);
    assert!(matches!(
        &sources[0],
        OrderedMessageImageSource::Media(MediaRef::Bytes(bytes)) if bytes == &[1, 2, 3]
    ));
    assert!(matches!(
        &sources[1],
        OrderedMessageImageSource::Media(MediaRef::Bytes(bytes)) if bytes == &[1, 2, 3]
    ));
}

#[test]
fn image_reference_budget_deduplicates_and_caps_total_inline_bytes() {
    let mut images = Vec::new();
    assert!(push_image_ref_with_limits(
        &mut images,
        MediaRef::Bytes(vec![1, 2, 3]),
        4,
        5,
    ));
    assert!(!push_image_ref_with_limits(
        &mut images,
        MediaRef::Bytes(vec![1, 2, 3]),
        4,
        5,
    ));
    assert!(!push_image_ref_with_limits(
        &mut images,
        MediaRef::Bytes(vec![4, 5, 6]),
        4,
        5,
    ));
    assert!(push_image_ref_with_limits(
        &mut images,
        MediaRef::Url("https://img.example/a.png".to_string()),
        4,
        5,
    ));
    assert!(!push_image_ref_with_limits(
        &mut images,
        MediaRef::Url("https://img.example/a.png".to_string()),
        4,
        5,
    ));
    assert_eq!(images.len(), 2);
}

#[tokio::test]
async fn prepared_images_become_binary_attachments_and_deduplicate_content() {
    let temp = tempfile::tempdir().unwrap();
    let state = test_web_state(temp.path(), 8300);
    let png = vec![0x89, b'P', b'N', b'G', 1];
    let prepared = prepare_inbound_images(
        &state,
        vec![
            MediaRef::Bytes(png.clone()),
            MediaRef::Bytes(png),
            MediaRef::Bytes(vec![0xFF, 0xD8, 0xFF, 2]),
        ],
    )
    .await
    .unwrap();

    assert_eq!(prepared.attempted, 3);
    assert_eq!(prepared.attachments.len(), 2);
    assert_eq!(prepared.duplicates, 1);
    assert_eq!(prepared.failed, 0);
    assert_eq!(prepared.total_bytes, 9);
    assert!(matches!(
        &prepared.attachments[0],
        Some(ImageAttachment::Binary { mime, data })
            if mime == "image/png" && data.starts_with(&[0x89, b'P', b'N', b'G'])
    ));
    assert!(matches!(
        &prepared.attachments[1],
        Some(ImageAttachment::Binary { mime, .. }) if mime == "image/jpeg"
    ));
}

#[test]
fn recall_notices_become_structured_inbound_events() {
    let event = json!({
        "post_type": "notice",
        "notice_type": "group_recall",
        "self_id": 10000,
        "group_id": 42,
        "user_id": 7,
        "operator_id": 8,
        "message_id": 99,
        "time": 123,
    });
    assert!(is_message_recall(&event));
    let recalled = recall_event(Target::Group { group_id: 42 }, &event, 7);
    assert_eq!(recalled.kind, PlatformInboundEventKind::MessageRecall);
    assert_eq!(recalled.conversation.account_id, "10000");
    assert_eq!(recalled.conversation.conversation_id, "42");
    assert_eq!(recalled.message_id, "99");
    assert_eq!(recalled.sender_id, "7");
    assert_eq!(recalled.operator_id.as_deref(), Some("8"));
    assert_eq!(recalled.timestamp, 123);

    assert!(!is_message_recall(&json!({
        "post_type": "notice",
        "notice_type": "group_increase"
    })));
}

#[test]
fn falls_back_to_raw_string_messages() {
    let message = json!("纯文本消息");
    let parsed = parse_message(Some(&message), None, 1);
    assert_eq!(parsed.text, "纯文本消息");

    let raw = json!("raw 兜底");
    let parsed = parse_message(None, Some(&raw), 1);
    assert_eq!(parsed.text, "raw 兜底");

    let reply_command = json!("[CQ:reply,id=5][CQ:at,qq=10001] /reset");
    let parsed = parse_message(Some(&reply_command), None, 10001);
    assert!(parsed.at_self);
    assert_eq!(parsed.text, " /reset");
    assert_eq!(parsed.reply_to_message_id.as_deref(), Some("5"));
    assert_eq!(parsed.mentioned_user_ids, vec!["10001"]);
    assert_eq!(
        commands::parse(&crate::config::PlatformsConfig::default(), &parsed.text),
        Some(commands::ParsedPlatformCommand::Reset {
            scope: Some(commands::ResetScope::Current)
        })
    );

    let escaped_literal = json!("&#91;CQ:reply,id=5&#93;/reset");
    let parsed = parse_message(Some(&escaped_literal), None, 1);
    assert_eq!(parsed.text, "[CQ:reply,id=5]/reset");
}

#[test]
fn inbound_parser_caps_media_segment_counts() {
    let message = Value::Array(
        (0..8)
            .flat_map(|index| {
                [
                    json!({
                        "type": "image",
                        "data": { "url": format!("https://img.example/{index}.png") }
                    }),
                    json!({
                        "type": "file",
                        "data": { "file_id": format!("f{index}"), "file_name": "x.txt" }
                    }),
                ]
            })
            .collect(),
    );
    let parsed = parse_message(Some(&message), None, 1);
    assert_eq!(parsed.images.len(), MAX_INBOUND_IMAGES);
    assert_eq!(parsed.files.len(), MAX_INBOUND_FILES);
}

#[test]
fn inbound_parser_rejects_oversized_text_and_segment_arrays_early() {
    let oversized = json!([{
        "type": "text",
        "data": { "text": "界".repeat(MAX_INBOUND_TEXT_CHARS + 1) }
    }]);
    let parsed = parse_message(Some(&oversized), None, 1);
    assert!(parsed.rejected_reason.is_some());
    assert_eq!(parsed.text.chars().count(), MAX_INBOUND_TEXT_CHARS);

    let too_many = Value::Array(
        (0..=MAX_INBOUND_SEGMENTS)
            .map(|_| json!({ "type": "text", "data": { "text": "x" } }))
            .collect(),
    );
    let parsed = parse_message(Some(&too_many), None, 1);
    assert_eq!(
        parsed.rejected_reason,
        Some("message has too many OneBot segments")
    );
}

#[test]
fn inbound_mentions_are_bounded_and_non_numeric_targets_are_ignored() {
    let mut segments = (1..=MAX_INBOUND_MENTIONS + 8)
        .map(|id| json!({ "type": "at", "data": { "qq": id.to_string() } }))
        .collect::<Vec<_>>();
    segments.push(json!({ "type": "at", "data": { "qq": "all" } }));
    let parsed = parse_message(Some(&Value::Array(segments)), None, 99_999);
    assert_eq!(parsed.mentioned_user_ids.len(), MAX_INBOUND_MENTIONS);
    assert!(parsed
        .mentioned_user_ids
        .iter()
        .all(|id| id.bytes().all(|byte| byte.is_ascii_digit())));
}

#[test]
fn image_only_turns_receive_nonempty_model_instructions() {
    for count in [1, 2, 4] {
        let prompt = image_only_prompt(count);
        assert!(!prompt.trim().is_empty());
        assert!(prompt.contains(&count.to_string()));
    }
}

#[test]
fn confirmed_direct_send_only_suppresses_later_assistant_text() {
    let outcome = crate::platforms::TurnOutcome {
        run_id: "run-test".to_string(),
        text: "首条消息的回答\n工具发送后的重复确认".to_string(),
        provider_id: None,
        model: None,
        image_assets: Vec::new(),
        suppressed_reply_ranges: vec![(
            "首条消息的回答".len(),
            "首条消息的回答\n工具发送后的重复确认".len(),
        )],
        final_reply_already_sent: true,
    };
    assert_eq!(final_reply_text(&outcome), "首条消息的回答");

    let unsuppressed = crate::platforms::TurnOutcome {
        suppressed_reply_ranges: Vec::new(),
        final_reply_already_sent: false,
        ..outcome
    };
    assert_eq!(
        final_reply_text(&unsuppressed),
        "首条消息的回答\n工具发送后的重复确认"
    );
}

#[test]
fn direct_send_suppression_preserves_text_outside_the_suppressed_range() {
    let prefix = "首条回答";
    let duplicate = "工具确认";
    let later = "后续回答";
    let text = format!("{prefix}{duplicate}{later}");
    let outcome = crate::platforms::TurnOutcome {
        run_id: "run-test".to_string(),
        text,
        provider_id: None,
        model: None,
        image_assets: Vec::new(),
        suppressed_reply_ranges: vec![(prefix.len(), prefix.len() + duplicate.len())],
        final_reply_already_sent: false,
    };
    assert_eq!(final_reply_text(&outcome), format!("{prefix}{later}"));
}
