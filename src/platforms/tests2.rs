//! tests2 — 自 src/platforms/mod.rs 外移。
#![cfg(test)]

use super::tests::*;
pub(crate) use super::*;
use std::sync::atomic::Ordering as AtomicOrdering;

use crate::config::AppConfig;
use crate::config::PlatformSessionLimits;
use crate::state::StateStore;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::sync::Weak;
#[tokio::test]
async fn adaptive_response_target_is_identical_on_primary_and_fallback() {
    let (_temp, mut context, adapter) = test_turn_context(true);
    let registry = MessageActivityRegistry::default();
    let (activity, start, _) = registry.observe("onebot:1:group:2", "m1", "alice", Instant::now());
    for index in 0..5 {
        registry.observe(
            "onebot:1:group:2",
            &format!("other-{index}"),
            "bob",
            Instant::now(),
        );
    }
    context.message_activity = Some(activity);
    let target = ResponseTarget {
        message_id: "m1".to_string(),
        user_id: "alice".to_string(),
        quote: true,
        mention: true,
        explicit_mention_user_ids: Vec::new(),
    };
    context.set_adaptive_response_target(
        Some(target.clone()),
        AdaptiveResponseTargetPolicy::new(
            Some(start),
            Instant::now().checked_sub(Duration::from_secs(15)).unwrap(),
            5,
            15,
        ),
    );
    // The OneBot trigger pipeline writes the final static decision after
    // the plugin has selected its adaptive policy; the matching target
    // must not discard that policy.
    context.set_response_target(Some(target.clone()));

    context
        .send(OutboundMessage::text(OutboundOrigin::Tool, "answer"))
        .await
        .unwrap();

    let messages = adapter.messages.lock().unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].response_target, Some(target.clone()));
    assert_eq!(messages[1].response_target, Some(target));
}

#[tokio::test]
async fn session_turns_are_fifo_and_lock_entries_are_reclaimed() {
    let runtime = PlatformRuntime::new().unwrap();
    let limits = PlatformSessionLimits {
        running: 1,
        queued: 2,
    };
    let first = runtime
        .acquire_session_turn("session-a", limits)
        .await
        .unwrap();
    let (order_tx, mut order_rx) = tokio::sync::mpsc::unbounded_channel();

    let second_runtime = runtime.clone();
    let second_tx = order_tx.clone();
    let second = tokio::spawn(async move {
        let _lease = second_runtime
            .acquire_session_turn("session-a", limits)
            .await
            .unwrap();
        second_tx.send(2).unwrap();
    });
    while runtime
        .session_turn_locks
        .lock()
        .unwrap()
        .get("session-a")
        .map(Weak::strong_count)
        .unwrap_or(0)
        < 2
    {
        tokio::task::yield_now().await;
    }

    let third_runtime = runtime.clone();
    let third = tokio::spawn(async move {
        let _lease = third_runtime
            .acquire_session_turn("session-a", limits)
            .await
            .unwrap();
        order_tx.send(3).unwrap();
    });
    while runtime
        .session_turn_locks
        .lock()
        .unwrap()
        .get("session-a")
        .map(Weak::strong_count)
        .unwrap_or(0)
        < 3
    {
        tokio::task::yield_now().await;
    }

    drop(first);
    assert_eq!(order_rx.recv().await, Some(2));
    assert_eq!(order_rx.recv().await, Some(3));
    second.await.unwrap();
    third.await.unwrap();
    assert!(runtime.session_turn_locks.lock().unwrap().is_empty());
}

#[tokio::test]
async fn session_turn_limits_bound_running_and_waiting_work() {
    let runtime = PlatformRuntime::new().unwrap();
    let limits = PlatformSessionLimits {
        running: 4,
        queued: 8,
    };
    let mut running = Vec::new();
    for _ in 0..4 {
        running.push(
            runtime
                .acquire_session_turn("bounded", limits)
                .await
                .unwrap(),
        );
    }
    let mut queued = Vec::new();
    for _ in 0..8 {
        let runtime = runtime.clone();
        queued.push(tokio::spawn(async move {
            runtime
                .acquire_session_turn("bounded", limits)
                .await
                .unwrap()
        }));
    }
    loop {
        let waiting = runtime
            .session_turn_locks
            .lock()
            .unwrap()
            .get("bounded")
            .and_then(Weak::upgrade)
            .map(|state| state.waiting.load(Ordering::Acquire))
            .unwrap_or_default();
        if waiting == 8 {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(matches!(
        runtime.acquire_session_turn("bounded", limits).await,
        Err(SessionTurnAcquireError::Full)
    ));
    drop(running);
    for task in queued {
        drop(task.await.unwrap());
    }
    assert!(runtime.session_turn_locks.lock().unwrap().is_empty());
}

#[tokio::test]
async fn session_preemption_invalidates_old_waiters_but_not_new_arrivals() {
    let runtime = PlatformRuntime::new().unwrap();
    let limits = PlatformSessionLimits {
        running: 1,
        queued: 8,
    };
    let first = runtime
        .acquire_session_turn("session-a", limits)
        .await
        .unwrap();
    let old_ticket = runtime.session_turn_ticket("session-a", limits);
    let (order_tx, mut order_rx) = tokio::sync::mpsc::unbounded_channel();
    let (started_tx, mut started_rx) = tokio::sync::mpsc::unbounded_channel();

    let old_tx = order_tx.clone();
    let old_started = started_tx.clone();
    let old = tokio::spawn(async move {
        old_started.send("old").unwrap();
        let lease = old_ticket.acquire().await.unwrap();
        old_tx.send(("old", lease.is_valid())).unwrap();
    });
    assert_eq!(started_rx.recv().await, Some("old"));

    let command_ticket = runtime.preempt_session_turns("session-a");
    assert!(!first.is_valid());
    let command_tx = order_tx.clone();
    let command_started = started_tx.clone();
    let command = tokio::spawn(async move {
        command_started.send("command").unwrap();
        let lease = command_ticket.acquire().await.unwrap();
        command_tx.send(("command", lease.is_valid())).unwrap();
    });
    assert_eq!(started_rx.recv().await, Some("command"));

    let new_ticket = runtime.session_turn_ticket("session-a", limits);
    let new = tokio::spawn(async move {
        let lease = new_ticket.acquire().await.unwrap();
        order_tx.send(("new", lease.is_valid())).unwrap();
    });

    drop(first);
    assert_eq!(order_rx.recv().await, Some(("command", true)));
    assert_eq!(order_rx.recv().await, Some(("old", false)));
    assert_eq!(order_rx.recv().await, Some(("new", true)));
    old.await.unwrap();
    command.await.unwrap();
    new.await.unwrap();
    assert!(runtime.session_turn_locks.lock().unwrap().is_empty());
}

#[tokio::test]
async fn different_platform_sessions_do_not_block_each_other() {
    let runtime = PlatformRuntime::new().unwrap();
    let limits = PlatformSessionLimits {
        running: 1,
        queued: 1,
    };
    let _first = runtime
        .acquire_session_turn("session-a", limits)
        .await
        .unwrap();
    let independent = tokio::time::timeout(
        Duration::from_secs(1),
        runtime.acquire_session_turn("session-b", limits),
    )
    .await;
    assert!(independent.is_ok());
}

#[tokio::test]
async fn running_platform_turn_does_not_block_an_independent_dispatch() {
    let daemon_temp = tempfile::tempdir().unwrap();
    let state = DaemonState::for_test(test_paths(daemon_temp.path()), 8300).unwrap();
    let session = state
        .state_store
        .create_session("gqy", "queued platform test", "user", None)
        .unwrap();
    state
        .state_store
        .pinned(&session.session_id)
        .start_turn("running-platform-turn", "first", std::process::id())
        .unwrap();

    let error = match run_platform_turn(
        &state,
        Arc::from(session.session_id.as_str()),
        "must stay separate".to_string(),
        Vec::new(),
        TurnProfile::default(),
    )
    .await
    {
        Err(error) => error,
        Ok(_) => panic!("independent platform turn should reach the unavailable worker"),
    };

    assert!(error.to_string().contains("worker is unavailable"));
    assert!(state
        .state_store
        .pinned(&session.session_id)
        .load_queued_prompts()
        .unwrap()
        .is_empty());
}

#[test]
fn direct_send_without_later_prompt_covers_an_empty_final_reply() {
    let mut suppression = ReplySuppression::default();
    suppression.direct_send_succeeded(8);
    let (ranges, already_sent) = suppression.finish(8);
    assert!(ranges.is_empty());
    assert!(already_sent);
}

#[test]
fn model_round_boundary_keeps_only_the_latest_visible_text() {
    let mut text = String::new();
    let mut suppression = ReplySuppression::default();

    start_model_reply(&mut text, &mut suppression);
    text.push_str("text before tool");
    start_model_reply(&mut text, &mut suppression);
    text.push_str("final tool follow-up");

    assert_eq!(text, "final tool follow-up");
    assert_eq!(suppression.finish(text.len()), (Vec::new(), false));
}

#[test]
fn ordinary_single_round_reply_is_unchanged() {
    let mut text = String::new();
    let mut suppression = ReplySuppression::default();

    start_model_reply(&mut text, &mut suppression);
    text.push_str("ordinary single round");

    assert_eq!(text, "ordinary single round");
    assert_eq!(suppression.finish(text.len()), (Vec::new(), false));
}

#[test]
fn platform_tool_payload_pretty_prints_small_json() {
    assert_eq!(
        format_platform_tool_payload_for(r#"{"query":"GQY","limit":2}"#, Locale::Zh),
        "{\n  \"limit\": 2,\n  \"query\": \"GQY\"\n}"
    );
}

#[test]
fn platform_tool_payload_truncates_on_unicode_boundaries() {
    let payload = "喵".repeat(PLATFORM_TOOL_LOG_MAX_CHARS + 1);
    let formatted = format_platform_tool_payload_for(&payload, Locale::Zh);
    let (kept, notice) = formatted.split_once('\n').unwrap();

    assert_eq!(kept.chars().count(), PLATFORM_TOOL_LOG_MAX_CHARS);
    assert!(kept.chars().all(|character| character == '喵'));
    assert_eq!(notice, "... 已截断 1 字符 ...");
}

#[test]
fn platform_reply_log_truncates_on_unicode_boundaries() {
    let payload = "喵".repeat(PLATFORM_REPLY_LOG_MAX_CHARS + 7);
    let formatted = truncate_platform_reply_log_for(&payload, Locale::Zh);
    let kept = formatted.lines().next().unwrap();

    assert_eq!(kept.chars().count(), PLATFORM_REPLY_LOG_MAX_CHARS);
    assert!(formatted.ends_with("... 已截断 7 字符 ..."));
    assert_eq!(
        truncate_platform_reply_log_for("safe\u{1b}[31m", Locale::Zh),
        "safe\\u{1b}[31m"
    );
}

#[test]
fn platform_tool_logs_include_correlation_and_result_details() {
    let started = format_platform_tool_started_log_for(
        "run_123",
        &serde_json::json!({
            "tool_id": "run_123_tool_2",
            "name": "web_search",
            "display_name": "网页搜索",
            "arguments": "{\"query\":\"GQY\"}"
        }),
        Locale::Zh,
    );
    assert!(started.starts_with("【工具：web_search】\n运行：run_123"));
    assert!(started.contains("调用 ID：run_123_tool_2"));
    assert!(started.contains("显示名称：网页搜索"));
    assert!(started.contains("\"query\": \"GQY\""));

    let finished = format_platform_tool_finished_log_for(
        "run_123",
        &serde_json::json!({
            "tool_id": "run_123_tool_2",
            "name": "web_search",
            "display_name": "网页搜索",
            "ok": false,
            "output": "request timed out"
        }),
        Locale::Zh,
    );
    assert!(finished.starts_with("【工具结果：web_search】\n运行：run_123"));
    assert!(finished.contains("调用 ID：run_123_tool_2"));
    assert!(finished.contains("显示名称：网页搜索"));
    assert!(finished.contains("状态：失败"));
    assert!(finished.ends_with("结果：\nrequest timed out"));

    let english = format_platform_tool_finished_log_for(
        "run_123",
        &serde_json::json!({
            "tool_id": "run_123_tool_2",
            "name": "web_search",
            "ok": true,
            "output": "done"
        }),
        Locale::En,
    );
    assert!(english.starts_with("[Tool result: web_search]\nRun: run_123"));
    assert!(english.contains("Status: success"));

    let sanitized = format_platform_tool_finished_log_for(
        "run_123",
        &serde_json::json!({
            "tool_id": "run_123_tool_2",
            "name": "web_search\nforged",
            "ok": true,
            "output": "safe\u{1b}[31m"
        }),
        Locale::En,
    );
    assert!(sanitized.starts_with("[Tool result: web_search forged]"));
    assert!(sanitized.ends_with("Result:\nsafe\\u{1b}[31m"));
}

#[test]
fn platform_final_reply_log_is_bilingual() {
    let (_temp, context, _adapter) = test_turn_context(false);
    let outcome = TurnOutcome {
        run_id: "run_123".to_string(),
        text: "hello".to_string(),
        provider_id: Some("provider".to_string()),
        model: Some("model".to_string()),
        image_assets: Vec::new(),
        suppressed_reply_ranges: Vec::new(),
        final_reply_already_sent: false,
    };

    let chinese = format_platform_final_reply_log_for(&outcome, &context, "你好", 0, Locale::Zh);
    assert!(chinese.starts_with("【AI 最终回复】\n运行：run_123"));
    assert!(chinese.contains("模型：provider / model"));

    let english = format_platform_final_reply_log_for(&outcome, &context, "hello", 0, Locale::En);
    assert!(english.starts_with("[AI final reply]\nRun: run_123"));
    assert!(english.contains("Model: provider / model"));
}

#[test]
fn direct_send_suppresses_the_next_model_round() {
    let mut text = String::new();
    let mut suppression = ReplySuppression::default();
    start_model_reply(&mut text, &mut suppression);
    text.push_str("text before tool");
    suppression.direct_send_succeeded(text.len());

    start_model_reply(&mut text, &mut suppression);
    text.push_str("duplicate confirmation");
    let (ranges, already_sent) = suppression.finish(text.len());

    assert_eq!(ranges, vec![(0, text.len())]);
    assert!(already_sent);
}

#[test]
fn queued_followup_resets_prior_direct_send_suppression() {
    let mut text = String::new();
    let mut suppression = ReplySuppression::default();
    start_model_reply(&mut text, &mut suppression);
    suppression.direct_send_succeeded(0);
    start_model_reply(&mut text, &mut suppression);
    text.push_str("reply before queued follow-up");
    suppression.queued_prompt_consumed();

    start_model_reply(&mut text, &mut suppression);
    text.push_str("queued follow-up answer");

    assert_eq!(text, "queued follow-up answer");
    assert_eq!(suppression.finish(text.len()), (Vec::new(), false));
}

#[test]
fn host_tools_follow_admin_and_private_whitelist_policy() {
    let (_temp, mut context, _adapter) = test_turn_context(false);
    assert!(!context.host_tools_allowed());
    context.is_admin = true;
    assert!(context.host_tools_allowed());

    context.is_admin = false;
    context.config.platforms.qq.allow_non_admin_host_tools = true;
    assert!(!context.host_tools_allowed());
    let dynamic_key = access_control::global_grant_key(
        access_control::AccessPermission::PrivateWhitelist,
        "20000".to_string(),
    );
    let actor = crate::state::PlatformAccessActor {
        platform: "onebot".to_string(),
        account_id: "10000".to_string(),
        user_id: "42".to_string(),
        conversation_kind: "private".to_string(),
        conversation_id: "42".to_string(),
        message_id: "message-1".to_string(),
    };
    context
        .state_store
        .add_platform_access_grant(&dynamic_key, &actor)
        .unwrap();
    assert!(context.host_tools_allowed());
    context
        .state_store
        .remove_platform_access_grant(&dynamic_key, &actor)
        .unwrap();
    assert!(!context.host_tools_allowed());
    context
        .config
        .platforms
        .qq
        .private_chats
        .whitelist
        .push(20_000);
    assert!(context.host_tools_allowed());

    context.conversation.kind = ConversationKind::Group;
    assert!(!context.host_tools_allowed());
}

#[test]
fn untrusted_send_tool_schema_does_not_expose_local_attachments() {
    let (_temp, context, _adapter) = test_turn_context(false);
    let mut registry = crate::tools::ToolRegistry::new();
    register_platform_tools(&mut registry, Arc::new(context));
    let parameters = &registry.get("send_message_to_user").unwrap().parameters;

    assert!(parameters["properties"].get("text").is_some());
    assert!(parameters["properties"].get("images").is_none());
    assert!(parameters["properties"].get("files").is_none());
}

#[tokio::test]
async fn usage_query_tool_reports_platform_history() {
    let (_temp, context, _adapter) = test_turn_context(false);
    context
        .state_store
        .add_usage(
            &crate::llm::Usage {
                prompt_tokens: 1000,
                completion_tokens: 200,
                total_tokens: 1200,
                cache_read_tokens: 400,
                ..crate::llm::Usage::default()
            },
            crate::state::UsageMeta {
                source: "onebot",
                provider: Some("prov"),
                model: Some("test-model"),
            },
        )
        .unwrap();
    let mut registry = crate::tools::ToolRegistry::new();
    register_platform_tools(&mut registry, Arc::new(context));
    let output = registry
        .call("query_token_usage", r#"{"range":"7d"}"#)
        .await
        .unwrap();
    assert!(output.contains("Token 消耗"), "{output}");
    assert!(output.contains("QQ"), "{output}");
    assert!(output.contains("test-model"), "{output}");
    assert!(output.contains("缓存命中率 40%"), "{output}");
}

#[test]
fn multi_mention_tool_is_only_registered_for_group_turns() {
    let (_private_temp, private, _adapter) = test_turn_context(false);
    let mut private_tools = crate::tools::ToolRegistry::new();
    register_platform_tools(&mut private_tools, Arc::new(private));
    assert!(private_tools.get("qq_mention_users").is_none());

    let (_group_temp, mut group, _adapter) = test_turn_context(false);
    group.conversation.kind = ConversationKind::Group;
    let mut group_tools = crate::tools::ToolRegistry::new();
    register_platform_tools(&mut group_tools, Arc::new(group));
    assert!(group_tools.get("qq_mention_user").is_none());
    let tool = group_tools.get("qq_mention_users").unwrap();
    assert_eq!(tool.parameters["required"], serde_json::json!(["user_ids"]));
    assert_eq!(tool.parameters["additionalProperties"], false);
    assert_eq!(tool.parameters["properties"]["user_ids"]["minItems"], 1);
    assert_eq!(tool.parameters["properties"]["user_ids"]["maxItems"], 32);
    assert_eq!(
        tool.parameters["properties"]["user_ids"]["items"]["pattern"],
        "^[1-9][0-9]{4,11}$"
    );
}

#[tokio::test]
async fn multi_mention_tool_overrides_automatic_mention_without_sending_an_extra_message() {
    let (_temp, mut context, adapter) = test_turn_context(false);
    context.conversation.kind = ConversationKind::Group;
    let context = Arc::new(context);
    context.set_response_target(Some(ResponseTarget {
        message_id: "message-1".to_string(),
        user_id: "20000".to_string(),
        quote: true,
        mention: true,
        explicit_mention_user_ids: Vec::new(),
    }));
    let mut registry = crate::tools::ToolRegistry::new();
    register_platform_tools(&mut registry, context.clone());

    registry
        .call("qq_mention_users", r#"{"user_ids":["50000"]}"#)
        .await
        .unwrap();
    let output = registry
        .call(
            "qq_mention_users",
            r#"{"user_ids":["30000","40000","30000"]}"#,
        )
        .await
        .unwrap();
    let output: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(output["user_ids"], serde_json::json!(["30000", "40000"]));
    assert_eq!(adapter.calls.load(AtomicOrdering::Relaxed), 0);
    assert_eq!(
        context.response_target(),
        Some(ResponseTarget {
            message_id: "message-1".to_string(),
            user_id: "20000".to_string(),
            quote: true,
            mention: false,
            explicit_mention_user_ids: vec!["30000".to_string(), "40000".to_string()],
        })
    );

    context
        .send(OutboundMessage::text(OutboundOrigin::FinalReply, "你好"))
        .await
        .unwrap();
    assert_eq!(adapter.calls.load(AtomicOrdering::Relaxed), 1);
    assert!(context.response_target().is_none());
    let messages = adapter.messages.lock().unwrap();
    assert_eq!(
        messages[0].response_target,
        Some(ResponseTarget {
            message_id: "message-1".to_string(),
            user_id: "20000".to_string(),
            quote: true,
            mention: false,
            explicit_mention_user_ids: vec!["30000".to_string(), "40000".to_string()],
        })
    );
}

#[tokio::test]
async fn multi_mention_tool_preserves_the_adaptive_quote_policy() {
    let (_temp, mut context, adapter) = test_turn_context(false);
    context.conversation.kind = ConversationKind::Group;
    let context = Arc::new(context);
    context.set_adaptive_response_target(
        Some(ResponseTarget {
            message_id: "message-1".to_string(),
            user_id: "20000".to_string(),
            quote: true,
            mention: true,
            explicit_mention_user_ids: Vec::new(),
        }),
        AdaptiveResponseTargetPolicy::new(None, Instant::now(), 1, 0),
    );
    let mut registry = crate::tools::ToolRegistry::new();
    register_platform_tools(&mut registry, context.clone());

    registry
        .call("qq_mention_users", r#"{"user_ids":["30000"]}"#)
        .await
        .unwrap();
    context.set_adaptive_response_target(
        Some(ResponseTarget {
            message_id: "message-2".to_string(),
            user_id: "20000".to_string(),
            quote: true,
            mention: true,
            explicit_mention_user_ids: Vec::new(),
        }),
        AdaptiveResponseTargetPolicy::new(None, Instant::now(), 1, 0),
    );
    context
        .send(OutboundMessage::text(OutboundOrigin::FinalReply, "你好"))
        .await
        .unwrap();

    let messages = adapter.messages.lock().unwrap();
    assert_eq!(
        messages[0].response_target,
        Some(ResponseTarget {
            message_id: "message-2".to_string(),
            user_id: "20000".to_string(),
            quote: false,
            mention: false,
            explicit_mention_user_ids: vec!["30000".to_string()],
        })
    );
}

#[tokio::test]
async fn multi_mention_tool_rejects_invalid_or_excessive_targets() {
    let (_temp, mut context, adapter) = test_turn_context(false);
    context.conversation.kind = ConversationKind::Group;
    let mut registry = crate::tools::ToolRegistry::new();
    register_platform_tools(&mut registry, Arc::new(context));

    let error = registry
        .call("qq_mention_users", r#"{"user_ids":["all"]}"#)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("5-12 digit QQ ID"));

    let error = registry
        .call("qq_mention_users", r#"{"user_ids":["+30000"]}"#)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("5-12 digit QQ ID"));

    let error = registry
        .call("qq_mention_users", r#"{"user_ids":[" 30000 "]}"#)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("5-12 digit QQ ID"));

    let error = registry
        .call(
            "qq_mention_users",
            r#"{"user_ids":["30000"],"group_id":"99999"}"#,
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("only user_ids"));

    let error = registry
        .call("qq_mention_users", r#"{"user_ids":["60000"]}"#)
        .await
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("not members of the current group"));

    let error = registry
        .call("qq_mention_users", r#"{"user_ids":[]}"#)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("at least one QQ ID"));

    let user_ids = (1..=33).map(|id| id.to_string()).collect::<Vec<_>>();
    let arguments = serde_json::json!({ "user_ids": user_ids }).to_string();
    let error = registry
        .call("qq_mention_users", &arguments)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("at most 32 users"));
    assert_eq!(adapter.calls.load(AtomicOrdering::Relaxed), 0);
}

fn built_in_test_context(kind: ConversationKind) -> (tempfile::TempDir, Arc<PlatformTurnContext>) {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let adapter = Arc::new(CountingAdapter {
        calls: AtomicUsize::new(0),
        fail_first: false,
        messages: Mutex::new(Vec::new()),
        group_members: test_group_members(),
    });
    let context = PlatformTurnContext::new(
        PlatformConversation {
            platform: "onebot".to_string(),
            account_id: "10000".to_string(),
            kind,
            conversation_id: "20000".to_string(),
        },
        "20000".to_string(),
        "tester".to_string(),
        false,
        AppConfig::default(),
        paths.clone(),
        StateStore::new(&paths).unwrap(),
        adapter,
        Arc::new(plugins::PlatformPluginRegistry::built_in().unwrap()),
    );
    (temp, Arc::new(context))
}

#[tokio::test]
async fn one_recall_tool_is_registered_for_every_qq_turn() {
    let (_private_temp, private) = built_in_test_context(ConversationKind::Private);
    private.prepare_turn("test".to_string()).await;
    let mut private_tools = crate::tools::ToolRegistry::new();
    register_platform_tools(&mut private_tools, private);
    assert!(private_tools.get("qq_withdraw_message").is_some());

    let (_group_temp, group) = built_in_test_context(ConversationKind::Group);
    group.prepare_turn("test".to_string()).await;
    let mut group_tools = crate::tools::ToolRegistry::new();
    register_platform_tools(&mut group_tools, group);
    assert!(group_tools.get("qq_withdraw_message").is_some());

    let (_member_temp, member_group) = built_in_test_context(ConversationKind::Group);
    member_group.set_plugin_value(
        "qq_group_management.bot_role",
        Value::String("member".to_string()),
    );
    let mut member_tools = crate::tools::ToolRegistry::new();
    register_platform_tools(&mut member_tools, member_group);
    assert!(member_tools.get("qq_withdraw_message").is_some());
}
