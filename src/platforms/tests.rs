//! tests — 自 src/platforms/mod.rs 外移。
#![cfg(test)]

use super::*;

use crate::paths::GQYPaths;
use futures_util::future::BoxFuture;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

/// Regression: an auto-attached reply image delivered in one turn must
/// stay suppressed for the recovery turn that follows an interrupted
/// send — that replay is what duplicated pictures in QQ groups.
#[test]
fn delivered_images_stay_deduplicated_across_turns_per_conversation() {
    let first = blake3::hash(b"generated-picture");
    let second = blake3::hash(b"another-picture");
    let scope = "onebot:1:group:duplicate-image-regression";
    let other_scope = "onebot:1:group:unrelated";

    assert!(recent_conversation_images(scope).is_empty());
    record_recent_conversation_images(scope, &[first]);
    assert_eq!(recent_conversation_images(scope), vec![first]);
    // Other conversations are unaffected.
    assert!(recent_conversation_images(other_scope).is_empty());

    // Re-recording keeps one entry per digest.
    record_recent_conversation_images(scope, &[first, second]);
    let mut seen = recent_conversation_images(scope);
    seen.sort_by_key(|digest| digest.as_bytes().to_vec());
    let mut expected = vec![first, second];
    expected.sort_by_key(|digest| digest.as_bytes().to_vec());
    assert_eq!(seen, expected);
}

struct SuppressingToolPlugin;

impl plugins::PlatformPlugin for SuppressingToolPlugin {
    fn descriptor(&self) -> plugins::PluginDescriptor {
        plugins::PluginDescriptor {
            id: "test_suppress",
            priority: 1,
            default_enabled: true,
        }
    }

    fn before_send<'a>(
        &'a self,
        _context: &'a PlatformTurnContext,
        message: OutboundMessage,
    ) -> BoxFuture<'a, Result<plugins::PreparedSend>> {
        Box::pin(async move {
            Ok(plugins::PreparedSend {
                primary: message.clone(),
                after_success: Vec::new(),
                fallback: Some(message),
                suppress_final_reply: true,
                suppress_prior_reply: false,
            })
        })
    }
}

pub(crate) struct CountingAdapter {
    pub(crate) calls: AtomicUsize,
    pub(crate) fail_first: bool,
    pub(crate) messages: Mutex<Vec<OutboundMessage>>,
    pub(crate) group_members: Vec<PlatformGroupMember>,
}

struct PartialFailureAdapter {
    calls: AtomicUsize,
    digest: blake3::Hash,
    response_target_delivered: bool,
}

impl PlatformAdapter for CountingAdapter {
    fn send<'a>(&'a self, message: OutboundMessage) -> BoxFuture<'a, Result<SendReceipt>> {
        Box::pin(async move {
            let image_digests = match &message.body {
                OutboundBody::Segments(segments) => segments
                    .iter()
                    .filter_map(|segment| match segment {
                        OutboundSegment::ImageBytes { data, .. } => Some(blake3::hash(data)),
                        _ => None,
                    })
                    .collect(),
                OutboundBody::Forward(_) => Vec::new(),
            };
            self.messages.lock().unwrap().push(message);
            let call = self.calls.fetch_add(1, AtomicOrdering::Relaxed);
            if self.fail_first && call == 0 {
                anyhow::bail!("injected primary failure");
            }
            Ok(SendReceipt {
                delivered_parts: 1,
                image_digests,
                ..SendReceipt::default()
            })
        })
    }

    fn bot_display_name<'a>(&'a self) -> BoxFuture<'a, Result<String>> {
        Box::pin(async { Ok("GQY".to_string()) })
    }

    fn group_members<'a>(&'a self) -> BoxFuture<'a, Result<Vec<PlatformGroupMember>>> {
        let members = self.group_members.clone();
        Box::pin(async move { Ok(members) })
    }
}

impl PlatformAdapter for PartialFailureAdapter {
    fn send<'a>(&'a self, _message: OutboundMessage) -> BoxFuture<'a, Result<SendReceipt>> {
        Box::pin(async move {
            self.calls.fetch_add(1, AtomicOrdering::Relaxed);
            Err(anyhow::Error::new(PartialSendError::new(
                anyhow::anyhow!("injected failure after partial delivery"),
                SendReceipt {
                    delivered_parts: 1,
                    image_digests: vec![self.digest],
                    response_target_delivered: self.response_target_delivered,
                    ..SendReceipt::default()
                },
            )))
        })
    }

    fn bot_display_name<'a>(&'a self) -> BoxFuture<'a, Result<String>> {
        Box::pin(async { Ok("GQY".to_string()) })
    }
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
        fish_hook_file: root.join("fish/gqy.fish"),
        bash_hook_file: root.join("shell/bash-hook.sh"),
        zsh_hook_file: root.join("shell/zsh-hook.zsh"),
        scripts_dir: root.join("config/scripts"),
        system_scripts_dir: PathBuf::new(),
    }
}

pub(crate) fn test_group_members() -> Vec<PlatformGroupMember> {
    ["20000", "30000", "40000", "50000"]
        .into_iter()
        .map(|user_id| PlatformGroupMember {
            group_id: "20000".to_string(),
            user_id: user_id.to_string(),
            nickname: format!("member-{user_id}"),
            card: String::new(),
            role: "member".to_string(),
            title: String::new(),
            joined_at: 0,
            last_active_at: 0,
        })
        .collect()
}

#[test]
fn parenthetical_only_filter_ignores_mentions_but_preserves_real_content() {
    let filtered = OutboundMessage::segments(
        OutboundOrigin::FinalReply,
        vec![
            OutboundSegment::Mention("123".to_string()),
            OutboundSegment::Text("  （这个消息与我无关）  ".to_string()),
        ],
    );
    assert!(message_is_parenthetical_only(&filtered));

    let nested = OutboundMessage::text(OutboundOrigin::FinalReply, "（外层（说明））");
    assert!(message_is_parenthetical_only(&nested));
    let two = OutboundMessage::text(OutboundOrigin::FinalReply, "（动作）（说明）");
    assert!(!message_is_parenthetical_only(&two));
    let sentence = OutboundMessage::text(OutboundOrigin::FinalReply, "你好（说明）");
    assert!(!message_is_parenthetical_only(&sentence));
    let media = OutboundMessage::segments(
        OutboundOrigin::FinalReply,
        vec![
            OutboundSegment::Text("（图片）".to_string()),
            OutboundSegment::ImageBytes {
                mime: "image/png".to_string(),
                data: Arc::from([1_u8]),
                alt: String::new(),
            },
        ],
    );
    assert!(!message_is_parenthetical_only(&media));
}

#[tokio::test]
async fn parenthetical_only_model_reply_never_reaches_the_adapter() {
    let (_temp, context, adapter) = test_turn_context(false);
    context
        .send(OutboundMessage::segments(
            OutboundOrigin::FinalReply,
            vec![
                OutboundSegment::Mention("123".to_string()),
                OutboundSegment::Text("（无视）".to_string()),
            ],
        ))
        .await
        .unwrap();
    assert_eq!(adapter.calls.load(AtomicOrdering::Relaxed), 0);

    context
        .send(OutboundMessage::text(
            OutboundOrigin::FinalReply,
            "正常回复（补充）",
        ))
        .await
        .unwrap();
    assert_eq!(adapter.calls.load(AtomicOrdering::Relaxed), 1);
}

pub(crate) fn test_turn_context(
    fail_first: bool,
) -> (tempfile::TempDir, PlatformTurnContext, Arc<CountingAdapter>) {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let adapter = Arc::new(CountingAdapter {
        calls: AtomicUsize::new(0),
        fail_first,
        messages: Mutex::new(Vec::new()),
        group_members: test_group_members(),
    });
    // Unique conversation per context: the delivered-image ledger is
    // process-global and keyed by conversation, so two test contexts
    // sharing an id would observe each other's deliveries.
    static NEXT_CONVERSATION: AtomicUsize = AtomicUsize::new(0);
    let conversation_id = format!(
        "20000-{}",
        NEXT_CONVERSATION.fetch_add(1, AtomicOrdering::Relaxed)
    );
    let context = PlatformTurnContext::new(
        PlatformConversation {
            platform: "onebot".to_string(),
            account_id: "10000".to_string(),
            kind: ConversationKind::Private,
            conversation_id,
        },
        "20000".to_string(),
        "tester".to_string(),
        false,
        AppConfig::default(),
        paths.clone(),
        StateStore::new(&paths).unwrap(),
        adapter.clone(),
        Arc::new(plugins::PlatformPluginRegistry::new(vec![Arc::new(
            SuppressingToolPlugin,
        )])),
    );
    (temp, context, adapter)
}

#[tokio::test]
async fn intermediate_flush_sends_round_text_once() {
    let (_temp, context, adapter) = test_turn_context(false);
    let suppression = ReplySuppression::default();
    flush_intermediate_reply(&context, "第一轮的说明。", &suppression).await;
    let messages = adapter.messages.lock().unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].origin, OutboundOrigin::IntermediateReply);
    let OutboundBody::Segments(segments) = &messages[0].body else {
        panic!("intermediate reply must be a normal message");
    };
    assert!(matches!(
        segments.as_slice(),
        [OutboundSegment::Markdown(text)] if text == "第一轮的说明。"
    ));
}

#[tokio::test]
async fn intermediate_flush_skips_empty_and_cuts_direct_send_ranges() {
    let (_temp, context, adapter) = test_turn_context(false);

    // Nothing to say: no message goes out.
    flush_intermediate_reply(&context, "   ", &ReplySuppression::default()).await;
    assert!(adapter.messages.lock().unwrap().is_empty());

    // The model continuation after a confirmed direct tool send is
    // suppressed, so only the part before the send is flushed.
    let text = "前半部分。已被工具直发的确认。";
    let mut suppression = ReplySuppression::default();
    suppression.direct_send_succeeded("前半部分。".len());
    flush_intermediate_reply(&context, text, &suppression).await;
    let messages = adapter.messages.lock().unwrap();
    assert_eq!(messages.len(), 1);
    let OutboundBody::Segments(segments) = &messages[0].body else {
        panic!("intermediate reply must be a normal message");
    };
    assert!(matches!(
        segments.as_slice(),
        [OutboundSegment::Markdown(text)] if text == "前半部分。"
    ));
}

#[test]
fn rate_window_allows_then_drops_with_single_notice() {
    let mut window = RateWindow::new();
    let start = Instant::now();
    let limit = PlatformRateLimit {
        max_messages: 3,
        window_seconds: 60,
    };
    for _ in 0..3 {
        assert_eq!(
            window.check_at(start, "group:1", limit),
            RateDecision::Allow
        );
    }
    assert_eq!(
        window.check_at(start, "group:1", limit),
        RateDecision::DropWithNotice
    );
    assert_eq!(
        window.check_at(start, "group:1", limit),
        RateDecision::DropSilently
    );
    // Another conversation is unaffected by the first group's quota.
    assert_eq!(
        window.check_at(start, "group:2", limit),
        RateDecision::Allow
    );
    // The window resets after a minute.
    let later = start + Duration::from_secs(61);
    assert_eq!(
        window.check_at(later, "group:1", limit),
        RateDecision::Allow
    );
}

#[test]
fn rate_availability_preflight_never_consumes_quota() {
    let mut window = RateWindow::new();
    let start = Instant::now();
    let limit = PlatformRateLimit {
        max_messages: 1,
        window_seconds: 60,
    };
    assert!(window.available_at(start, "group:1", limit));
    assert!(window.available_at(start, "group:1", limit));
    assert_eq!(
        window.check_at(start, "group:1", limit),
        RateDecision::Allow
    );
    assert!(!window.available_at(start, "group:1", limit));
    assert_eq!(
        window.check_at(start, "group:1", limit),
        RateDecision::DropWithNotice
    );
}

#[test]
fn rate_windows_are_independent_and_support_three_minute_quotas() {
    let mut window = RateWindow::new();
    let start = Instant::now();
    let limit = PlatformRateLimit {
        max_messages: 1,
        window_seconds: 180,
    };
    assert_eq!(
        window.check_at(start, "private:1", limit),
        RateDecision::Allow
    );
    assert_eq!(
        window.check_at(start + Duration::from_secs(30), "private:2", limit),
        RateDecision::Allow
    );
    assert_eq!(
        window.check_at(start + Duration::from_secs(179), "private:1", limit),
        RateDecision::DropWithNotice
    );
    assert_eq!(
        window.check_at(start + Duration::from_secs(180), "private:1", limit),
        RateDecision::Allow
    );
}

#[test]
fn rate_window_zero_is_unlimited() {
    let mut unlimited = RateWindow::new();
    let start = Instant::now();
    let limit = PlatformRateLimit::default();
    for i in 0..100 {
        assert_eq!(
            unlimited.check_at(start, &format!("group:{i}"), limit),
            RateDecision::Allow
        );
    }
}

#[test]
fn markdown_to_plain_strips_decoration_keeps_content() {
    let input = "# 标题\n\n**加粗** 与 `代码` 和 [链接](https://a.b)\n\n```rust\nlet x = 1; // **不动**\n```\n\n- 列表项\n> 引用";
    let plain = markdown_to_plain(input);
    assert_eq!(
        plain,
        "标题\n\n加粗 与 代码 和 链接 (https://a.b)\n\nlet x = 1; // **不动**\n\n- 列表项\n引用"
    );
}

#[test]
fn markdown_link_edge_cases() {
    assert_eq!(strip_inline_markup("[a](b"), "[a](b");
    assert_eq!(strip_inline_markup("纯 [文本] 括号"), "纯 [文本] 括号");
    // Identical text/url collapses to one copy.
    assert_eq!(
        strip_inline_markup("[https://x.y](https://x.y)"),
        "https://x.y"
    );
}

#[test]
fn split_reply_paragraph_line_and_hard_boundaries() {
    assert_eq!(split_reply("短", 10), vec!["短"]);
    assert!(split_reply("  ", 10).is_empty());
    // 0 disables splitting.
    let long = "a".repeat(50);
    assert_eq!(split_reply(&long, 0), vec![long.clone()]);

    let text = "第一段落。\n\n第二段落。";
    let chunks = split_reply(text, 6);
    assert_eq!(chunks, vec!["第一段落。", "第二段落。"]);

    // CJK hard split never panics and keeps every char.
    let cjk = "汉".repeat(25);
    let chunks = split_reply(&cjk, 10);
    assert_eq!(chunks.len(), 3);
    assert_eq!(chunks.join(""), cjk);
}

#[test]
fn sniff_image_mime_by_magic() {
    assert_eq!(sniff_image_mime(&[0x89, b'P', b'N', b'G', 0]), "image/png");
    assert_eq!(sniff_image_mime(&[0xFF, 0xD8, 0xFF, 0xE0]), "image/jpeg");
    assert_eq!(sniff_image_mime(b"GIF89a"), "image/gif");
    assert_eq!(sniff_image_mime(b"RIFF\0\0\0\0WEBPVP8 "), "image/webp");
    assert_eq!(sniff_image_mime(b"????"), "image/png");
}

#[test]
fn message_activity_counts_other_senders_and_deduplicates_events() {
    let registry = MessageActivityRegistry::default();
    let now = Instant::now();
    let (activity, start, _) = registry.observe("onebot:1:group:2", "m1", "alice", now);
    assert_eq!(start.total_messages, 1);
    assert_eq!(start.sender_messages, 1);

    let (_, first_other, first_received_at) =
        registry.observe("onebot:1:group:2", "m2", "bob", now);
    let (_, duplicate, duplicate_received_at) = registry.observe(
        "onebot:1:group:2",
        "m2",
        "bob",
        now + Duration::from_secs(10),
    );
    assert_eq!(duplicate, first_other);
    assert_eq!(duplicate_received_at, first_received_at);
    registry.observe("onebot:1:group:2", "m3", "alice", now);

    let current = activity.position_for("alice");
    assert_eq!(current.total_messages, 3);
    assert_eq!(current.sender_messages, 2);
    let other_messages = current
        .total_messages
        .saturating_sub(start.total_messages)
        .saturating_sub(
            current
                .sender_messages
                .saturating_sub(start.sender_messages),
        );
    assert_eq!(other_messages, 1);

    let (_, isolated, _) = registry.observe("onebot:1:group:3", "m4", "bob", now);
    assert_eq!(isolated.total_messages, 1);
}

#[test]
fn adaptive_response_target_uses_independent_inclusive_boundaries() {
    let now = Instant::now();
    let start = PlatformMessagePosition {
        total_messages: 10,
        sender_messages: 2,
    };
    let target = ResponseTarget {
        message_id: "message-1".to_string(),
        user_id: "alice".to_string(),
        quote: true,
        mention: true,
        explicit_mention_user_ids: Vec::new(),
    };
    let policy = AdaptiveResponseTargetPolicy::new(Some(start), now, 5, 15);

    let before_both = policy.resolve(
        target.clone(),
        Some(PlatformMessagePosition {
            total_messages: 15,
            sender_messages: 3,
        }),
        now + Duration::from_secs(14),
    );
    assert!(before_both.is_none());

    let quote_only = policy
        .resolve(
            target.clone(),
            Some(PlatformMessagePosition {
                total_messages: 15,
                sender_messages: 2,
            }),
            now + Duration::from_secs(14),
        )
        .unwrap();
    assert!(quote_only.quote);
    assert!(!quote_only.mention);

    let mention_only = policy
        .resolve(
            target.clone(),
            Some(PlatformMessagePosition {
                total_messages: 15,
                sender_messages: 3,
            }),
            now + Duration::from_secs(15),
        )
        .unwrap();
    assert!(!mention_only.quote);
    assert!(mention_only.mention);

    let both = policy
        .resolve(
            target,
            Some(PlatformMessagePosition {
                total_messages: 15,
                sender_messages: 2,
            }),
            now + Duration::from_secs(15),
        )
        .unwrap();
    assert!(both.quote);
    assert!(both.mention);
}

#[test]
fn adaptive_response_target_mention_uses_known_message_activity() {
    let now = Instant::now();
    let start = PlatformMessagePosition {
        total_messages: 10,
        sender_messages: 2,
    };
    let target = ResponseTarget {
        message_id: "message-1".to_string(),
        user_id: "alice".to_string(),
        quote: false,
        mention: true,
        explicit_mention_user_ids: Vec::new(),
    };
    let policy = AdaptiveResponseTargetPolicy::new(Some(start), now, 5, 15);
    let same_sender_message = PlatformMessagePosition {
        total_messages: 11,
        sender_messages: 3,
    };
    let other_sender_message = PlatformMessagePosition {
        total_messages: 11,
        sender_messages: 2,
    };
    let cases = [
        ("before threshold without messages", Some(start), 14, false),
        ("at threshold without messages", Some(start), 15, false),
        (
            "at threshold after same sender",
            Some(same_sender_message),
            15,
            false,
        ),
        (
            "before threshold after other sender",
            Some(other_sender_message),
            14,
            false,
        ),
        (
            "at threshold after other sender",
            Some(other_sender_message),
            15,
            true,
        ),
        ("before threshold with unknown activity", None, 14, false),
        ("at threshold with unknown activity", None, 15, true),
    ];

    for (case, current, elapsed_seconds, expected) in cases {
        let mention = policy
            .resolve(
                target.clone(),
                current,
                now + Duration::from_secs(elapsed_seconds),
            )
            .is_some_and(|target| target.mention);
        assert_eq!(mention, expected, "{case}");
    }
}

#[tokio::test]
async fn direct_final_suppression_requires_primary_send_success() {
    let (_temp, success, _adapter) = test_turn_context(false);
    success
        .send(OutboundMessage::text(OutboundOrigin::Tool, "sent"))
        .await
        .unwrap();
    assert!(success.take_final_reply_suppression());
    assert!(!success.take_final_reply_suppression());

    let (_temp, fallback, _adapter) = test_turn_context(true);
    fallback
        .send(OutboundMessage::text(OutboundOrigin::Tool, "fallback"))
        .await
        .unwrap();
    assert!(!fallback.take_final_reply_suppression());
}

#[tokio::test]
async fn delivery_ledger_records_only_confirmed_images() {
    let bytes: Arc<[u8]> = Arc::from([1_u8, 2, 3]);
    let digest = blake3::hash(&bytes);
    let image_message = || {
        OutboundMessage::segments(
            OutboundOrigin::Tool,
            vec![OutboundSegment::ImageBytes {
                mime: "image/png".to_string(),
                data: bytes.clone(),
                alt: String::new(),
            }],
        )
    };

    let (_temp, success, _adapter) = test_turn_context(false);
    success.send(image_message()).await.unwrap();
    assert!(success.delivered_image_digests().contains(&digest));

    let (_temp, mut failed, _adapter) = test_turn_context(true);
    failed.plugins = Arc::new(plugins::PlatformPluginRegistry::default());
    assert!(failed.send(image_message()).await.is_err());
    assert!(failed.delivered_image_digests().is_empty());
}

#[tokio::test]
async fn partial_delivery_is_recorded_without_sending_a_full_fallback() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let digest = blake3::hash(&[1_u8, 2, 3]);
    let adapter = Arc::new(PartialFailureAdapter {
        calls: AtomicUsize::new(0),
        digest,
        response_target_delivered: false,
    });
    let context = PlatformTurnContext::new(
        PlatformConversation {
            platform: "onebot".to_string(),
            account_id: "10000".to_string(),
            kind: ConversationKind::Private,
            conversation_id: "20000".to_string(),
        },
        "20000".to_string(),
        "tester".to_string(),
        false,
        AppConfig::default(),
        paths.clone(),
        StateStore::new(&paths).unwrap(),
        adapter.clone(),
        Arc::new(plugins::PlatformPluginRegistry::new(vec![Arc::new(
            SuppressingToolPlugin,
        )])),
    );

    let result = context
        .send(OutboundMessage::segments(
            OutboundOrigin::Tool,
            vec![OutboundSegment::ImageBytes {
                mime: "image/png".to_string(),
                data: Arc::from([1_u8, 2, 3]),
                alt: String::new(),
            }],
        ))
        .await;

    assert!(result.is_err());
    assert_eq!(adapter.calls.load(AtomicOrdering::Relaxed), 1);
    assert!(context.delivered_image_digests().contains(&digest));
}

#[tokio::test]
async fn response_target_is_consumed_once_and_survives_primary_fallback() {
    let (_temp, context, adapter) = test_turn_context(true);
    let target = ResponseTarget {
        message_id: "message-9".to_string(),
        user_id: "user-4".to_string(),
        quote: true,
        mention: true,
        explicit_mention_user_ids: Vec::new(),
    };
    context.set_response_target(Some(target.clone()));

    context
        .send(OutboundMessage::text(OutboundOrigin::Tool, "first"))
        .await
        .unwrap();
    context
        .send(OutboundMessage::text(OutboundOrigin::FinalReply, "second"))
        .await
        .unwrap();

    let messages = adapter.messages.lock().unwrap();
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[0].response_target, Some(target.clone()));
    assert_eq!(messages[1].response_target, Some(target));
    assert_eq!(messages[2].response_target, None);
    assert_eq!(context.response_target(), None);
}

#[tokio::test]
async fn partially_delivered_response_target_is_not_restored() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let adapter = Arc::new(PartialFailureAdapter {
        calls: AtomicUsize::new(0),
        digest: blake3::hash(&[1_u8]),
        response_target_delivered: true,
    });
    let context = PlatformTurnContext::new(
        PlatformConversation {
            platform: "onebot".to_string(),
            account_id: "10000".to_string(),
            kind: ConversationKind::Group,
            conversation_id: "20000".to_string(),
        },
        "20000".to_string(),
        "tester".to_string(),
        false,
        AppConfig::default(),
        paths.clone(),
        StateStore::new(&paths).unwrap(),
        adapter,
        Arc::new(plugins::PlatformPluginRegistry::default()),
    );
    context.set_explicit_response_mentions(vec!["30000".to_string()]);

    assert!(context
        .send(OutboundMessage::text(OutboundOrigin::FinalReply, "first"))
        .await
        .is_err());
    assert!(context.response_target().is_none());
}

#[test]
fn failed_older_send_merges_mentions_into_a_newer_response_target() {
    let (_temp, context, _adapter) = test_turn_context(false);
    context.set_explicit_response_mentions(vec!["30000".to_string()]);
    let reserved = context
        .response_target
        .lock()
        .unwrap()
        .take()
        .expect("explicit target exists");
    context.set_adaptive_response_target(
        Some(ResponseTarget {
            message_id: "message-2".to_string(),
            user_id: "20000".to_string(),
            quote: true,
            mention: true,
            explicit_mention_user_ids: Vec::new(),
        }),
        AdaptiveResponseTargetPolicy::new(None, Instant::now(), 1, 1),
    );

    context.restore_response_target(reserved);

    assert_eq!(
        context.response_target(),
        Some(ResponseTarget {
            message_id: "message-2".to_string(),
            user_id: "20000".to_string(),
            quote: true,
            mention: false,
            explicit_mention_user_ids: vec!["30000".to_string()],
        })
    );
}
