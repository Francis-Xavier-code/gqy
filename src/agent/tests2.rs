//! tests2 — 自 src/agent/mod.rs 外移。
#![cfg(test)]

use super::tests::*;
use super::tests4::*;
use super::*;
use crate::config::{ActiveProviderModelConfig, ProviderConfig};
use crate::platforms::{ConversationKind, PlatformConversation};
use crate::tools::{empty_parameters, ToolSpec};
use std::net::{TcpListener, TcpStream};

fn visible_association_lines_collects_only_replayed_memory_blocks() {
    let block = "<associative-memory>\n以下是根据当前输入联想到的完整人格记忆。\n\n曾经记住的相关知识点：\n- [2026-08-10] [公共知识] Homebrew 镜像只读\n</associative-memory>";
    let messages = vec![
        ChatMessage::system("prompt"),
        // 回放的化石块：user 角色、正文以标签开头 → 计入
        ChatMessage::plain("user", block),
        // 用户正文中途引用同样文本 → 不以标签开头，不计入
        ChatMessage::plain("user", format!("用户引用了 {block}")),
        // 非 user 角色 → 不计入
        ChatMessage::plain("assistant", "- [2026-08-10] [公共知识] Homebrew 镜像只读"),
    ];
    let seen = visible_association_lines(&messages);
    assert_eq!(seen.len(), 1);
    assert!(seen.contains("- [2026-08-10] [公共知识] Homebrew 镜像只读"));
}

#[test]
fn turn_context_blocks_already_visible_in_fossils_are_skipped() {
    let notice = "[SystemInfo:LongReplyImageConversion]\n1. 你的一条长回复（约 480 字）已被自动渲染为 1 张图片发送。";
    let messages = vec![
        ChatMessage::system("prompt"),
        // 上一轮化石里已经带着同样的通知
        ChatMessage::plain(
            "user",
            format!("<qq-request-context>…</qq-request-context>\n\n{notice}"),
        ),
        ChatMessage::plain("assistant", "回复"),
    ];
    assert!(turn_context_block_visible(&messages, notice));
    // 内容变化(记录数不同)不再匹配,照常注入
    let changed = "[SystemInfo:LongReplyImageConversion]\n1. 你的一条长回复（约 480 字）已被自动渲染为 1 张图片发送。\n2. 你的一条长回复（约 900 字）已被自动渲染为 2 张图片发送。";
    assert!(!turn_context_block_visible(&messages, changed));
    // 非 user 角色的出现不算
    let assistant_only = vec![ChatMessage::plain("assistant", notice)];
    assert!(!turn_context_block_visible(&assistant_only, notice));
    // 只有 [SystemInfo: 前缀的常驻通告参与去重;指涉"当前回合"的块
    // (唤醒通知/身份告警/审核初判)即使字节相同也必须重发
    assert!(notice.starts_with(STANDING_ADVISORY_PREFIX));
    assert!(!"本轮由系统自动触发：一个后台任务刚刚结束。".starts_with(STANDING_ADVISORY_PREFIX));
    assert!(!"<qq-identity-warning>…</qq-identity-warning>".starts_with(STANDING_ADVISORY_PREFIX));
}

#[test]
fn vision_support_requires_every_effective_text_pool_model() {
    let mut config = AppConfig::default();
    let provider = config.providers.first_mut().unwrap();
    provider.default_model = "vision-model".to_string();
    provider.models = vec!["vision-model".to_string(), "text-model".to_string()];
    provider.model_modalities.insert(
        "vision-model".to_string(),
        vec!["text".to_string(), "image".to_string()],
    );
    provider
        .model_modalities
        .insert("text-model".to_string(), vec!["text".to_string()]);
    let provider_id = provider.id.clone();

    config.active_provider_models = Some(vec![ActiveProviderModelConfig {
        provider_id: provider_id.clone(),
        model: "vision-model".to_string(),
    }]);
    assert!(active_text_pool_supports_vision(&config));

    config
        .active_provider_models
        .as_mut()
        .unwrap()
        .push(ActiveProviderModelConfig {
            provider_id,
            model: "text-model".to_string(),
        });
    assert!(!active_text_pool_supports_vision(&config));
}

#[test]
fn vision_preference_controls_direct_image_delivery_to_the_text_pool() {
    let mut config = AppConfig::default();
    let provider = config.providers.first_mut().unwrap();
    provider.model_modalities.insert(
        provider.default_model.clone(),
        vec!["text".to_string(), "image".to_string()],
    );

    assert!(should_use_active_text_pool_for_images(&config));
    config.plugins.vision.prefer_current_multimodal_model = false;
    assert!(!should_use_active_text_pool_for_images(&config));
}

#[tokio::test]
async fn platform_images_register_a_turn_scoped_vision_tool() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let config = AppConfig::default();
    let state = StateStore::new(&paths).unwrap();
    let client =
        OpenAiCompatibleClient::new(config.provider(None).unwrap(), &config, &paths).unwrap();
    let mut agent = Agent::new(
        config,
        &paths,
        state,
        client,
        ToolRegistry::new(),
        AgentMode::Normal,
    )
    .unwrap();
    agent.set_image_platform("qq", "QQ");
    let images = vec![Some(PastedImage::Binary(ClipboardImage::new(
        "image/png".to_string(),
        vec![1, 2, 3],
    )))];

    let prepared = agent.prepare_user_input("看图", &images).await.unwrap();
    let hint = format!("{:?}", prepared.hints);
    assert!(hint.contains("vision_analyze"));
    let tools = agent.tools.lock().unwrap().clone();
    assert!(tools.contains("vision_analyze"));
    let error = tools
        .call("vision_analyze", r#"{"image":"/etc/passwd"}"#)
        .await
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("image is not attached to the current platform turn"));
}

#[tokio::test]
async fn context_image_ids_register_vision_without_a_current_image() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let config = AppConfig::default();
    let state = StateStore::new(&paths).unwrap();
    let client =
        OpenAiCompatibleClient::new(config.provider(None).unwrap(), &config, &paths).unwrap();
    let mut agent = Agent::new(
        config.clone(),
        &paths,
        state,
        client,
        ToolRegistry::new(),
        AgentMode::Normal,
    )
    .unwrap();
    agent.set_image_platform("qq", "QQ");
    let context = Arc::new(PlatformTurnContext::new(
        PlatformConversation {
            platform: "onebot".to_string(),
            account_id: "10000".to_string(),
            kind: ConversationKind::Group,
            conversation_id: "20000".to_string(),
        },
        "30000".to_string(),
        "tester".to_string(),
        false,
        config,
        paths.clone(),
        StateStore::new(&paths).unwrap(),
        Arc::new(NoopPlatformAdapter),
        Arc::new(crate::platforms::plugins::PlatformPluginRegistry::default()),
    ));
    agent.set_platform_context_images(
        context,
        vec![PlatformContextImageRef {
            id: "context_image_1".to_string(),
            message_id: "90".to_string(),
            image_index: 1,
        }],
    );

    let prepared = agent.prepare_user_input("接着说", &[]).await.unwrap();
    assert!(format!("{:?}", prepared.hints).contains("context_image_1"));
    let tools = agent.tools.lock().unwrap();
    assert!(tools.contains("vision_analyze"));
    let definition = tools
        .definitions()
        .into_iter()
        .find(|definition| definition.function.name == "vision_analyze")
        .unwrap();
    assert!(definition.function.description.contains("context_image_N"));
}

#[tokio::test]
async fn binary_image_reaches_vision_pool_then_text_model() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let vision_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let text_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut config =
        queue_test_config(format!("http://{}/v1", text_listener.local_addr().unwrap()));
    config.tools.enabled = false;
    config.plugins.vision.enabled = true;
    config.providers.push(ProviderConfig {
        id: "vision-test".to_string(),
        display_name: "Vision Test".to_string(),
        base_url: format!("http://{}/v1", vision_listener.local_addr().unwrap()),
        protocol: "openai-chat".to_string(),
        api_key: Some("test-key".to_string()),
        models: vec!["vision-model".to_string()],
        model_context_window: Default::default(),
        model_temperature: HashMap::new(),
        model_modalities: [(
            "vision-model".to_string(),
            vec!["text".to_string(), "image".to_string()],
        )]
        .into(),
        model_costs: Default::default(),
        default_model: "vision-model".to_string(),
        timeout_seconds: 30,
        temperature: 0.0,
        anthropic_max_tokens: 4096,
        extra_body: None,
    });
    config.active_multimodal_provider_models = Some(vec![ActiveProviderModelConfig {
        provider_id: "vision-test".to_string(),
        model: "vision-model".to_string(),
    }]);

    let (vision_request_tx, vision_request_rx) = oneshot::channel();
    let vision_server = tokio::spawn(async move {
        let (mut stream, _) = vision_listener.accept().await.unwrap();
        let request = read_test_http_request(&mut stream).await;
        let _ = vision_request_tx.send(request);
        write_test_sse(
            &mut stream,
            concat!(
                "data: {\"choices\":[{\"delta\":{\"content\":\"a red square\"}}]}\n\n",
                "data: {\"choices\":[{\"finish_reason\":\"stop\",\"delta\":{}}]}\n\n",
                "data: [DONE]\n\n"
            ),
        )
        .await;
    });
    let (text_request_tx, text_request_rx) = oneshot::channel();
    let text_server = tokio::spawn(async move {
        let (mut stream, _) = text_listener.accept().await.unwrap();
        let request = read_test_http_request(&mut stream).await;
        let _ = text_request_tx.send(request);
        write_test_sse(
            &mut stream,
            concat!(
                "data: {\"choices\":[{\"delta\":{\"content\":\"I can see it.\"}}]}\n\n",
                "data: {\"choices\":[{\"finish_reason\":\"stop\",\"delta\":{}}]}\n\n",
                "data: [DONE]\n\n"
            ),
        )
        .await;
    });

    let state = StateStore::new(&paths).unwrap();
    state.init_files().unwrap();
    let text_provider = config.provider(None).unwrap().clone();
    let client = OpenAiCompatibleClient::new(&text_provider, &config, &paths).unwrap();
    let mut agent = Agent::new(
        config,
        &paths,
        state,
        client,
        ToolRegistry::new(),
        AgentMode::Normal,
    )
    .unwrap();
    let image = PastedImage::Binary(ClipboardImage::new(
        "image/png".to_string(),
        b"qq-image-bytes".to_vec(),
    ));

    let result = agent
        .chat_stream_with_images("What is shown?", &[Some(image)], |_| Ok(()))
        .await
        .unwrap();

    assert_eq!(result.content, "I can see it.");
    let vision_request: Value = serde_json::from_slice(&vision_request_rx.await.unwrap()).unwrap();
    let vision_parts = vision_request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|message| message["role"] == "user")
        .unwrap()["content"]
        .as_array()
        .unwrap();
    assert!(vision_parts.iter().any(|part| {
        part["type"] == "image_url"
            && part["image_url"]["url"]
                .as_str()
                .is_some_and(|url| url.starts_with("data:image/png;base64,"))
    }));

    let text_request: Value = serde_json::from_slice(&text_request_rx.await.unwrap()).unwrap();
    let serialized = serde_json::to_string(&text_request).unwrap();
    assert!(serialized.contains("What is shown?"));
    assert!(serialized.contains("a red square"));
    vision_server.await.unwrap();
    text_server.await.unwrap();
}

#[test]
fn effective_context_tokens_include_tool_definitions() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let config = AppConfig::default();
    let state = StateStore::new(&paths).unwrap();
    state.init_files().unwrap();
    let client =
        OpenAiCompatibleClient::new(config.provider(None).unwrap(), &config, &paths).unwrap();
    let mut tools = ToolRegistry::new();
    tools.register(ToolSpec::new(
            "heavy_context_tool",
            "This tool has a deliberately long description so effective context includes tool definitions.",
            empty_parameters(),
            |_| async { Ok(String::new()) },
        ));
    let with_tools = Agent::new(
        config.clone(),
        &paths,
        state.clone(),
        client.clone(),
        tools,
        AgentMode::Normal,
    )
    .unwrap();
    let without_tools = Agent::new(
        AppConfig {
            tools: crate::config::ToolsConfig {
                enabled: false,
                ..config.tools.clone()
            },
            ..config
        },
        &paths,
        state,
        client,
        ToolRegistry::new(),
        AgentMode::Normal,
    )
    .unwrap();

    assert!(
        with_tools.effective_context_tokens().unwrap()
            > without_tools.effective_context_tokens().unwrap()
    );
}

#[test]
fn overflow_check_tokens_triggers_at_threshold() {
    let check = overflow::OverflowCheck::new(Some(100_000), 0.9, None);
    assert!(!check.check_tokens(60_000));
    assert!(check.check_tokens(95_000));
}

#[test]
fn overflow_check_disabled_when_no_window() {
    let check = overflow::OverflowCheck::new(None, 0.9, None);
    assert!(!check.is_enabled());
    assert!(!check.check_tokens(1_998_998));
}

#[test]
fn overflow_check_estimate_triggers() {
    let check = overflow::OverflowCheck::new(Some(1_000), 0.9, None);
    let big_msg = ChatMessage::plain("user", &"token ".repeat(2_000));
    let small_msg = ChatMessage::plain("user", "hi");
    assert!(check.check_estimate(&[big_msg]));
    assert!(!check.check_estimate(&[small_msg]));
}

#[test]
fn structured_tool_business_failure_marks_the_event_failed() {
    assert!(!tool_output_succeeded(r#"{"success":false}"#));
    assert!(!tool_output_succeeded(r#"{"ok":false}"#));
    assert!(tool_output_succeeded(r#"{"success":true}"#));
    assert!(tool_output_succeeded("plain tool output"));
}

#[tokio::test]
async fn queue_ingress_waits_for_a_reserved_tool_followup() {
    let barrier = Arc::new(QueueIngressBarrier::default());
    barrier.tool_started("call_1");
    let reservation = barrier
        .try_reserve()
        .expect("active tool accepts follow-up");
    barrier.tool_finished("call_1");

    assert!(tokio::time::timeout(
        Duration::from_millis(10),
        barrier.wait_for_reserved_ingress()
    )
    .await
    .is_err());
    assert!(barrier.try_reserve().is_none());

    drop(reservation);
    tokio::time::timeout(
        Duration::from_millis(100),
        barrier.wait_for_reserved_ingress(),
    )
    .await
    .expect("released follow-up reservation wakes the agent");
}

#[test]
fn queue_ingress_tracks_parallel_tool_calls_by_id() {
    let barrier = Arc::new(QueueIngressBarrier::default());
    barrier.tool_started("call_1");
    barrier.tool_started("call_2");
    barrier.tool_finished("call_1");
    assert!(barrier.try_reserve().is_some());
    barrier.tool_finished("call_2");
    assert!(barrier.try_reserve().is_none());
}

#[test]
fn journal_persists_a_stream_batch_before_displaying_it() {
    let temp = tempfile::tempdir().unwrap();
    let state = crate::state::StateStore::new(&test_paths(temp.path())).unwrap();
    state
        .start_turn("journal-turn", "long task", std::process::id())
        .unwrap();
    let mut sink = TurnJournalSink::new(state.clone(), "journal-turn".to_string(), 0);
    let mut displayed = Vec::new();
    {
        let mut on_event = |event| {
            if let AgentEvent::Chunk(chunk) = event {
                displayed.push(chunk.text);
            }
            Ok(())
        };
        sink.emit(
            AgentEvent::Chunk(ChatStreamChunk {
                kind: ChatStreamKind::Content,
                text: "durable partial".to_string(),
            }),
            &mut on_event,
        )
        .unwrap();
    }
    assert!(displayed.is_empty());
    assert!(state.load_turns().unwrap()[0].journal_events.is_empty());

    {
        let mut on_event = |event| {
            if let AgentEvent::Chunk(chunk) = event {
                displayed.push(chunk.text);
            }
            Ok(())
        };
        sink.emit(AgentEvent::SpinnerTick, &mut on_event).unwrap();
    }
    assert_eq!(displayed, ["durable partial"]);
    assert_eq!(state.load_turns().unwrap()[0].journal_events.len(), 1);

    state.interrupt_turn("journal-turn").unwrap();
    assert!(state.load_turns().unwrap()[0]
        .assistant_content
        .contains("durable partial"));
}

#[test]
fn raw_reasoning_is_batched_before_filtered_display() {
    let temp = tempfile::tempdir().unwrap();
    let state = crate::state::StateStore::new(&test_paths(temp.path())).unwrap();
    state
        .start_turn("reasoning-turn", "long task", std::process::id())
        .unwrap();
    let mut sink = TurnJournalSink::new(state.clone(), "reasoning-turn".to_string(), 0);
    let mut displayed = Vec::new();
    {
        let mut on_event = |event| {
            if let AgentEvent::Chunk(chunk) = event {
                displayed.push(chunk.text);
            }
            Ok(())
        };
        sink.emit(
            AgentEvent::RawReasoning(ChatStreamChunk {
                kind: ChatStreamKind::Reasoning,
                text: "raw reasoning".to_string(),
            }),
            &mut on_event,
        )
        .unwrap();
        sink.emit(
            AgentEvent::Chunk(ChatStreamChunk {
                kind: ChatStreamKind::Reasoning,
                text: "filtered reasoning".to_string(),
            }),
            &mut on_event,
        )
        .unwrap();
    }
    assert!(displayed.is_empty());
    assert!(state.load_turns().unwrap()[0].journal_events.is_empty());

    {
        let mut on_event = |event| {
            if let AgentEvent::Chunk(chunk) = event {
                displayed.push(chunk.text);
            }
            Ok(())
        };
        sink.emit(AgentEvent::SpinnerTick, &mut on_event).unwrap();
    }

    assert_eq!(displayed, ["filtered reasoning"]);
    assert_eq!(state.load_turns().unwrap()[0].journal_events.len(), 1);
    assert_eq!(
        state.load_turns().unwrap()[0].journal_events[0]
            .text_payload
            .as_deref(),
        Some("raw reasoning")
    );
}

#[test]
fn journal_flush_precedes_queued_prompt_boundary() {
    let temp = tempfile::tempdir().unwrap();
    let state = crate::state::StateStore::new(&test_paths(temp.path())).unwrap();
    state
        .start_turn("boundary-turn", "long task", std::process::id())
        .unwrap();
    state
        .enqueue_prompt("q1", "followup", "followup", &[])
        .unwrap();
    let mut sink = TurnJournalSink::new(state.clone(), "boundary-turn".to_string(), 0);
    let mut displayed = Vec::new();
    let mut transport = |event| {
        if let AgentEvent::Chunk(chunk) = event {
            displayed.push(chunk.text);
        }
        Ok(())
    };
    let mut journaled = |event| sink.emit(event, &mut transport);

    journaled(AgentEvent::Chunk(ChatStreamChunk {
        kind: ChatStreamKind::Content,
        text: "answer before followup".to_string(),
    }))
    .unwrap();
    journaled(AgentEvent::FlushJournal).unwrap();
    state
        .consume_queued_prompts(
            "boundary-turn",
            &[("q1".to_string(), "followup".to_string())],
            Some("answer before followup"),
            None,
        )
        .unwrap();
    journaled(AgentEvent::QueuedPromptsConsumed {
        prompt_ids: vec!["q1".to_string()],
        mode: AgentMode::Normal,
        provider_id: None,
        model: None,
    })
    .unwrap();

    let events = state.load_turns().unwrap()[0].journal_events.clone();
    assert_eq!(displayed, ["answer before followup"]);
    assert_eq!(
        events
            .iter()
            .map(|event| event.kind.as_str())
            .collect::<Vec<_>>(),
        ["assistant_content", "queued_prompts_consumed"]
    );
}

#[test]
fn turn_context_tokens_match_sent_messages() {
    let mut turn = crate::state::Turn {
        turn_id: "t1".to_string(),
        seq: 1,
        user_content: "question".to_string(),
        display_content: "question".to_string(),
        user_timestamp: String::new(),
        assistant_content: "answer".to_string(),
        assistant_reasoning: Some("hidden reasoning ".repeat(1_000)),
        assistant_provider_id: None,
        assistant_model: None,
        assistant_timestamp: None,
        status: crate::state::TurnStatus::Completed,
        tool_reports: Vec::new(),
        tool_flow: Vec::new(),
        question_exchanges: Vec::new(),
        followups: Vec::new(),
        attachments: Vec::new(),
        hidden: false,
        is_summary: false,
        owner_pid: None,
        token_total: 0,
        token_prompt: 0,
        token_cache_read: 0,
        token_usage_estimated: false,
        revision: 0,
        journal_events: Vec::new(),
        context_messages: Vec::new(),
    };
    let with_reasoning = turn_context_tokens(&turn);
    turn.assistant_reasoning = None;
    let without_reasoning = turn_context_tokens(&turn);
    // 跨轮思考回放退役:完成轮的思维链不再计入(也不再发送)。
    assert_eq!(with_reasoning, without_reasoning);

    turn.tool_reports.push("persisted tool result".to_string());
    assert!(turn_context_tokens(&turn) > without_reasoning);
}

#[test]
fn assistant_reasoning_is_not_replayed_across_turns() {
    // 跨轮思考回放退役(08-16):完成轮只回放正式回复;中断恢复走
    // journal 专道(interrupted_turn_replay_messages),不经此函数。
    let mut messages = Vec::new();
    push_assistant_context_messages(
        &mut messages,
        "visible answer",
        Some("raw provider reasoning"),
        true,
    );

    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].role, "assistant");
    assert!(matches!(
        messages[0].content.as_ref(),
        Some(ChatContent::Text(content)) if content == "visible answer"
    ));
}

#[test]
fn interrupted_redo_replays_prefix_followups_before_new_boundaries() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let config = AppConfig::default();
    let state = StateStore::new(&paths).unwrap();
    let client =
        OpenAiCompatibleClient::new(config.provider(None).unwrap(), &config, &paths).unwrap();
    let agent = Agent::new(
        config,
        &paths,
        state,
        client,
        ToolRegistry::new(),
        AgentMode::Normal,
    )
    .unwrap();
    let followup = |prompt_id: &str, content: &str, preceding: &str| crate::state::TurnFollowup {
        prompt_id: prompt_id.to_string(),
        content: content.to_string(),
        display_content: content.to_string(),
        attachments: Vec::new(),
        uploaded_attachments: Vec::new(),
        submitted_at: String::new(),
        preceding_assistant_content: Some(preceding.to_string()),
        preceding_assistant_reasoning: None,
        preceding_assistant_provider_id: None,
        preceding_assistant_model: None,
    };
    let mut turn = crate::state::Turn {
        turn_id: "redo-turn".to_string(),
        seq: 1,
        user_content: "initial".to_string(),
        display_content: "initial".to_string(),
        user_timestamp: String::new(),
        assistant_content: crate::state::pending_placeholder().to_string(),
        assistant_reasoning: None,
        assistant_provider_id: None,
        assistant_model: None,
        assistant_timestamp: None,
        status: crate::state::TurnStatus::Interrupted,
        tool_reports: Vec::new(),
        tool_flow: Vec::new(),
        question_exchanges: vec![
            QuestionExchange {
                questions: vec![crate::question::QuestionPrompt {
                    header: "Route".to_string(),
                    question: "Pick a route".to_string(),
                    options: vec![crate::question::QuestionOption {
                        label: "A".to_string(),
                        description: "".to_string(),
                    }],
                    multiple: false,
                    custom: false,
                }],
                answers: vec![vec!["A".to_string()]],
                answered_at: String::new(),
            },
            QuestionExchange {
                questions: vec![crate::question::QuestionPrompt {
                    header: "Branch".to_string(),
                    question: "Current branch question".to_string(),
                    options: vec![crate::question::QuestionOption {
                        label: "B".to_string(),
                        description: "".to_string(),
                    }],
                    multiple: false,
                    custom: false,
                }],
                answers: vec![vec!["B".to_string()]],
                answered_at: String::new(),
            },
        ],
        followups: vec![
            followup("q1", "edited first followup", "first answer"),
            followup("q2", "new followup", "after q1"),
        ],
        attachments: Vec::new(),
        hidden: false,
        is_summary: false,
        owner_pid: None,
        token_total: 0,
        token_prompt: 0,
        token_cache_read: 0,
        token_usage_estimated: false,
        revision: 1,
        journal_events: vec![
            crate::state::TurnJournalEvent {
                event_id: 0,
                revision: 1,
                segment_index: 0,
                kind: "redo_prefix_question_count".to_string(),
                call_id: None,
                name: None,
                text_payload: Some("1".to_string()),
                blob_payload: None,
                ok: None,
            },
            crate::state::TurnJournalEvent {
                event_id: 1,
                revision: 1,
                segment_index: 0,
                kind: "assistant_content".to_string(),
                call_id: None,
                name: None,
                text_payload: Some("after q1".to_string()),
                blob_payload: None,
                ok: None,
            },
            crate::state::TurnJournalEvent {
                event_id: 2,
                revision: 1,
                segment_index: 0,
                kind: "queued_prompts_consumed".to_string(),
                call_id: None,
                name: None,
                text_payload: Some("[\"q2\"]".to_string()),
                blob_payload: None,
                ok: None,
            },
            crate::state::TurnJournalEvent {
                event_id: 3,
                revision: 1,
                segment_index: 1,
                kind: "assistant_content".to_string(),
                call_id: None,
                name: None,
                text_payload: Some("after q2".to_string()),
                blob_payload: None,
                ok: None,
            },
        ],
        context_messages: Vec::new(),
    };

    let messages = interrupted_turn_replay_messages(&agent, &turn);
    let text_messages = messages
        .iter()
        .filter_map(|message| match message.content.as_ref() {
            Some(ChatContent::Text(text)) => Some((message.role.as_str(), text.as_str())),
            _ => None,
        })
        .collect::<Vec<_>>();
    let q1 = text_messages
        .iter()
        .position(|(_, text)| *text == "edited first followup")
        .unwrap();
    let clarification = text_messages
        .iter()
        .position(|(_, text)| text.contains("Pick a route"))
        .unwrap();
    assert!(!text_messages
        .iter()
        .any(|(_, text)| text.contains("Current branch question")));
    let after_q1 = text_messages
        .iter()
        .position(|(_, text)| *text == "after q1")
        .unwrap();
    let q2 = text_messages
        .iter()
        .position(|(_, text)| *text == "new followup")
        .unwrap();
    let after_q2 = text_messages
        .iter()
        .position(|(_, text)| *text == "after q2")
        .unwrap();
    assert!(clarification < q1);
    assert!(q1 < after_q1);
    assert!(after_q1 < q2);
    assert!(q2 < after_q2);

    turn.journal_events
        .retain(|event| event.kind != "redo_prefix_question_count");
    turn.journal_events.push(crate::state::TurnJournalEvent {
        event_id: 4,
        revision: 1,
        segment_index: 1,
        kind: "tool_result".to_string(),
        call_id: Some("question-call".to_string()),
        name: Some("ask_question".to_string()),
        text_payload: Some("{\"status\":\"answered\"}".to_string()),
        blob_payload: None,
        ok: Some(true),
    });
    let legacy_messages = interrupted_turn_replay_messages(&agent, &turn);
    let legacy_text = legacy_messages
        .iter()
        .filter_map(|message| match message.content.as_ref() {
            Some(ChatContent::Text(text)) => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(legacy_text.iter().any(|text| text.contains("Pick a route")));
    assert!(!legacy_text
        .iter()
        .any(|text| text.contains("Current branch question")));
}
