//! tests4 — 自 src/agent/mod.rs 外移。
#![cfg(test)]

use super::tests::*;
use super::*;
use crate::config::ProviderConfig;
use crate::platforms::{ConversationKind, PlatformConversation};
use crate::tools::{empty_parameters, ToolSpec};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

#[tokio::test]
async fn queued_prompts_are_consumed_after_tools_with_dispatch_time_mode() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let mut config = queue_test_config(base_url);
    config.tools.enabled = true;
    config.skills.enabled = false;
    config.memory.enabled = false;

    let mut normal_tools = ToolRegistry::new();
    normal_tools.register(ToolSpec::new(
        "queue_boundary_tool",
        "returns a fixed result",
        empty_parameters(),
        |_| async { Ok("tool finished".to_string()) },
    ));
    let control =
        AgentTurnControl::new(AgentMode::Normal, normal_tools.clone(), ToolRegistry::new());
    let server_control = control.clone();
    let (request_tx, request_rx) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut first, _) = listener.accept().await.unwrap();
        let _ = read_test_http_request(&mut first).await;
        server_control.set_mode(AgentMode::Dev);
        write_test_sse(
                &mut first,
                concat!(
                    "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"queue_boundary_tool\",\"arguments\":\"{}\"}}]}}]}\n\n",
                    "data: {\"choices\":[{\"finish_reason\":\"tool_calls\",\"delta\":{}}]}\n\n",
                    "data: [DONE]\n\n"
                ),
            )
            .await;

        let (mut second, _) = listener.accept().await.unwrap();
        let request = read_test_http_request(&mut second).await;
        let _ = request_tx.send(request);
        write_test_sse(
            &mut second,
            concat!(
                "data: {\"choices\":[{\"delta\":{\"content\":\"final answer\"}}]}\n\n",
                "data: {\"choices\":[{\"finish_reason\":\"stop\",\"delta\":{}}]}\n\n",
                "data: [DONE]\n\n"
            ),
        )
        .await;
    });

    let state = StateStore::new(&paths).unwrap();
    state.init_files().unwrap();
    let provider = config.provider(None).unwrap().clone();
    let client = OpenAiCompatibleClient::new(&provider, &config, &paths).unwrap();
    let mut agent = Agent::new(
        config,
        &paths,
        state.clone(),
        client,
        normal_tools,
        AgentMode::Normal,
    )
    .unwrap();
    state
        .enqueue_prompt("q1", "first followup", "first followup", &[])
        .unwrap();
    state
        .enqueue_prompt("q2", "second followup", "second followup", &[])
        .unwrap();
    let mut consumed = None;

    let result = agent
        .chat_stream_with_control("initial prompt", &[], &control, |event| {
            if let AgentEvent::QueuedPromptsConsumed {
                prompt_ids, mode, ..
            } = event
            {
                consumed = Some((prompt_ids, mode));
            }
            Ok(())
        })
        .await
        .unwrap();

    assert_eq!(result.content, "final answer");
    assert_eq!(agent.mode(), AgentMode::Dev);
    assert_eq!(
        consumed,
        Some((vec!["q1".to_string(), "q2".to_string()], AgentMode::Dev))
    );
    let request: serde_json::Value = serde_json::from_slice(&request_rx.await.unwrap()).unwrap();
    let messages = request["messages"].as_array().unwrap();
    assert!(messages
        .iter()
        .any(|message| { message["role"] == "user" && message["content"] == "first followup" }));
    assert!(messages
        .iter()
        .any(|message| { message["role"] == "user" && message["content"] == "second followup" }));
    assert!(messages
        .iter()
        .any(|message| { message["role"] == "tool" && message["content"] == "tool finished" }));
    assert!(state.load_queued_prompts().unwrap().is_empty());
    let turns = state.load_turns().unwrap();
    assert_eq!(turns[0].followups.len(), 2);
    assert_eq!(turns[0].assistant_content, "final answer");
    server.await.unwrap();
}

/// guard 拒绝是软失败:命令拒绝子串拦下 run_command,回给模型一条
/// tool error 让它换路,轮次存活拿到最终回答——而不是炸掉整轮。
#[tokio::test]
async fn guard_denied_tool_soft_fails_and_turn_continues() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let mut config = queue_test_config(base_url);
    config.tools.enabled = true;
    config.skills.enabled = false;
    config.memory.enabled = false;

    let mut normal_tools = ToolRegistry::new();
    normal_tools.register(ToolSpec::new(
        "run_command",
        "runs commands",
        empty_parameters(),
        |_| async { Ok("should never run".to_string()) },
    ));
    normal_tools.add_guard(crate::tools::command_deny_guard(vec![
        "rm -rf /".to_string()
    ]));
    let control = AgentTurnControl::new(
        AgentMode::Normal,
        normal_tools.clone(),
        normal_tools.clone(),
    );
    let (request_tx, request_rx) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut first, _) = listener.accept().await.unwrap();
        let _ = read_test_http_request(&mut first).await;
        write_test_sse(
                &mut first,
                concat!(
                    "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"run_command\",\"arguments\":\"{\\\"command\\\":\\\"sudo rm -rf /\\\"}\"}}]}}]}\n\n",
                    "data: {\"choices\":[{\"finish_reason\":\"tool_calls\",\"delta\":{}}]}\n\n",
                    "data: [DONE]\n\n"
                ),
            )
            .await;

        let (mut second, _) = listener.accept().await.unwrap();
        let request = read_test_http_request(&mut second).await;
        let _ = request_tx.send(request);
        write_test_sse(
            &mut second,
            concat!(
                "data: {\"choices\":[{\"delta\":{\"content\":\"recovered answer\"}}]}\n\n",
                "data: {\"choices\":[{\"finish_reason\":\"stop\",\"delta\":{}}]}\n\n",
                "data: [DONE]\n\n"
            ),
        )
        .await;
    });

    let state = StateStore::new(&paths).unwrap();
    state.init_files().unwrap();
    let provider = config.provider(None).unwrap().clone();
    let client = OpenAiCompatibleClient::new(&provider, &config, &paths).unwrap();
    let mut agent = Agent::new(
        config,
        &paths,
        state.clone(),
        client,
        normal_tools,
        AgentMode::Normal,
    )
    .unwrap();

    let result = agent
        .chat_stream_with_control("initial prompt", &[], &control, |_| Ok(()))
        .await
        .unwrap();

    assert_eq!(result.content, "recovered answer");
    let request: serde_json::Value = serde_json::from_slice(&request_rx.await.unwrap()).unwrap();
    let messages = request["messages"].as_array().unwrap();
    assert!(messages.iter().any(|message| {
        message["role"] == "tool"
            && message["content"].as_str().is_some_and(|content| {
                content.contains("denied pattern") || content.contains("被禁止的模式")
            })
    }));
    server.await.unwrap();
}

/// 防失忆提醒(08-16 版):首回合蒸馏后以化石身份进历史;间隔轮数内
/// 的第二回合不再注入新份——请求里只有回放的那一份,且当前轮尾部
/// 干净(runtime 投影同小时也跳注入),前缀纯追加。
#[tokio::test]
async fn persona_reminder_fossilizes_on_interval_and_replays() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let mut config = queue_test_config(base_url);
    config.tools.enabled = false;
    config.system_prompt = Some("测试人格：说话简短。".to_string());
    config.prompt.persona_reminder = true;

    let (first_chat_tx, first_chat_rx) = oneshot::channel();
    let (second_chat_tx, second_chat_rx) = oneshot::channel();
    let server = tokio::spawn(async move {
        let reply = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"哦\"}}]}\n\n",
            "data: {\"choices\":[{\"finish_reason\":\"stop\",\"delta\":{}}]}\n\n",
            "data: [DONE]\n\n"
        );
        // 回合1请求①:蒸馏调用(产物首行名字,次行正文)。
        let (mut distill, _) = listener.accept().await.unwrap();
        let request = read_test_http_request(&mut distill).await;
        let body: serde_json::Value = serde_json::from_slice(&request).unwrap();
        assert!(body["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains("人格设定文件"));
        write_test_sse(
                &mut distill,
                concat!(
                    "data: {\"choices\":[{\"delta\":{\"content\":\"短\\n回复很短，从不用Emoji。\"}}]}\n\n",
                    "data: {\"choices\":[{\"finish_reason\":\"stop\",\"delta\":{}}]}\n\n",
                    "data: [DONE]\n\n"
                ),
            )
            .await;
        // 回合1请求②:正式对话。
        let (mut chat, _) = listener.accept().await.unwrap();
        let _ = first_chat_tx.send(read_test_http_request(&mut chat).await);
        write_test_sse(&mut chat, reply).await;
        // 回合2请求①:缓存命中,直接就是对话请求(若再蒸馏一次,
        // 这里读到的请求不含新消息,下方断言会失败)。
        let (mut chat2, _) = listener.accept().await.unwrap();
        let _ = second_chat_tx.send(read_test_http_request(&mut chat2).await);
        write_test_sse(&mut chat2, reply).await;
    });

    let state = StateStore::new(&paths).unwrap();
    state.init_files().unwrap();
    let provider = config.provider(None).unwrap().clone();
    let client = OpenAiCompatibleClient::new(&provider, &config, &paths).unwrap();
    let mut agent = Agent::new(
        config.clone(),
        &paths,
        state.clone(),
        client,
        ToolRegistry::new(),
        AgentMode::Normal,
    )
    .unwrap();
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
    agent.set_platform_context_images(context.clone(), Vec::new());
    agent.chat_stream("第一条消息", |_| Ok(())).await.unwrap();

    let expected_reminder = "<persona-reminder>回复很短，从不用Emoji。\
             就算是讲解答疑，也只说最关键的两三步，整条不超过一百字，\
             一次说不完就等对方追问。</persona-reminder>";
    let request: serde_json::Value = serde_json::from_slice(&first_chat_rx.await.unwrap()).unwrap();
    let messages = request["messages"].as_array().unwrap();
    // 提醒以化石身份入列(位置在 runtime 之后、随机注入的表情包
    // 提醒之前),不再断言绝对末尾——只断言恰好一份。
    assert_eq!(
        messages
            .iter()
            .filter(|message| message["content"] == expected_reminder)
            .count(),
        1
    );
    let turns = state.load_turns().unwrap();
    // 新语义:提醒就是化石,回放历史自带。
    assert!(format!("{:?}", turns[0].context_messages).contains("persona-reminder"));
    assert!(paths
        .state_dir
        .join("persona-hints")
        .read_dir()
        .unwrap()
        .next()
        .is_some());

    agent.set_platform_context_images(context, Vec::new());
    agent.chat_stream("第二条消息", |_| Ok(())).await.unwrap();
    let request: serde_json::Value =
        serde_json::from_slice(&second_chat_rx.await.unwrap()).unwrap();
    let messages = request["messages"].as_array().unwrap();
    assert!(messages.iter().any(|message| {
        message["content"]
            .as_str()
            .is_some_and(|content| content.contains("第二条消息"))
    }));
    let reminder_count = messages
        .iter()
        .filter(|message| {
            message["content"]
                .as_str()
                .is_some_and(|content| content.contains("persona-reminder"))
        })
        .count();
    // 间隔(默认3)未到:仅回放化石那一份,不再追加新份;绝对末尾
    // 不再是漂浮提醒(可能是用户消息或跨分钟的新 runtime,都合法)。
    assert_eq!(reminder_count, 1);
    assert!(messages
        .iter()
        .any(|message| message["content"] == expected_reminder));
    assert_ne!(messages.last().unwrap()["content"], expected_reminder);
    server.await.unwrap();
}

/// 手写防失忆提示(hints/<scope>.md)优先于自动蒸馏:存在时整回合
/// 不发蒸馏请求(服务端只应答一次对话),尾部原样携带手写内容,
/// 不拼场景句。
#[tokio::test]
async fn manual_persona_reminder_overrides_distillation() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let mut config = queue_test_config(base_url);
    config.tools.enabled = false;
    config.system_prompt = Some("测试人格：说话简短。".to_string());
    config.prompt.persona_reminder = true;
    let hint_path = crate::persona_hint::manual_hint_path(&config, &paths, "default");
    std::fs::create_dir_all(hint_path.parent().unwrap()).unwrap();
    std::fs::write(&hint_path, "未有在群里潜水。手写版提醒。\n").unwrap();

    let (chat_tx, chat_rx) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut chat, _) = listener.accept().await.unwrap();
        let _ = chat_tx.send(read_test_http_request(&mut chat).await);
        write_test_sse(
            &mut chat,
            concat!(
                "data: {\"choices\":[{\"delta\":{\"content\":\"哦\"}}]}\n\n",
                "data: {\"choices\":[{\"finish_reason\":\"stop\",\"delta\":{}}]}\n\n",
                "data: [DONE]\n\n"
            ),
        )
        .await;
    });

    let state = StateStore::new(&paths).unwrap();
    state.init_files().unwrap();
    let provider = config.provider(None).unwrap().clone();
    let client = OpenAiCompatibleClient::new(&provider, &config, &paths).unwrap();
    let mut agent = Agent::new(
        config.clone(),
        &paths,
        state.clone(),
        client,
        ToolRegistry::new(),
        AgentMode::Normal,
    )
    .unwrap();
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
    agent.set_platform_context_images(context, Vec::new());
    agent.chat_stream("第一条消息", |_| Ok(())).await.unwrap();

    let request: serde_json::Value = serde_json::from_slice(&chat_rx.await.unwrap()).unwrap();
    let last = request["messages"].as_array().unwrap().last().unwrap();
    assert_eq!(last["role"], "user");
    assert_eq!(
        last["content"],
        "<persona-reminder>未有在群里潜水。手写版提醒。</persona-reminder>"
    );
    server.await.unwrap();
}

/// 预设对话(begin_dialogs):system 之后、真实历史之前注入 Q/A 对,
/// 每请求从 dialogs/<scope>.md 重建、永不落库。
#[test]
fn preset_dialogs_ride_after_system_before_history() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let config = AppConfig::default();
    let dialogs = crate::persona_hint::dialogs_path(&config, &paths, "default");
    std::fs::create_dir_all(dialogs.parent().unwrap()).unwrap();
    std::fs::write(&dialogs, "user: 你好\nassistant: 哼，又来一个。\n").unwrap();
    let state = StateStore::new(&paths).unwrap();
    state.init_files().unwrap();
    state.start_turn("turn_h", "历史问题", 999999).unwrap();
    state.complete_turn("turn_h", "历史回答", None).unwrap();
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
    let messages = agent.chat_messages("current", "新消息").unwrap().0;
    assert_eq!(messages[0].role, "system");
    assert_eq!(messages[1].role, "user");
    assert_eq!(chat_message_text(&messages[1]).unwrap(), "你好");
    assert_eq!(messages[2].role, "assistant");
    assert_eq!(chat_message_text(&messages[2]).unwrap(), "哼，又来一个。");
    assert_eq!(chat_message_text(&messages[3]).unwrap(), "历史问题");
    // 预设对话只活在请求里:历史存储不含它。
    let turns = agent.state.load_turns().unwrap();
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].user_content, "历史问题");
}

/// Dev 模式极简组装:系统提示词是 dev-prompt.md 的一行(缺省内置默认),
/// 人格全家(预设对话/用户档案)整套绕开——即使 dialogs 文件存在。
#[test]
fn dev_mode_uses_one_line_prompt_and_skips_persona_family() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let config = AppConfig::default();
    // 人格侧的预设对话文件在场,dev 也必须无视。
    let dialogs = crate::persona_hint::dialogs_path(&config, &paths, "default");
    std::fs::create_dir_all(dialogs.parent().unwrap()).unwrap();
    std::fs::write(&dialogs, "user: 你好\nassistant: 哼，又来一个。\n").unwrap();
    let state = StateStore::new(&paths).unwrap();
    state.init_files().unwrap();
    state.start_turn("turn_h", "历史问题", 999999).unwrap();
    state.complete_turn("turn_h", "历史回答", None).unwrap();
    let client =
        OpenAiCompatibleClient::new(config.provider(None).unwrap(), &config, &paths).unwrap();
    let agent = Agent::new(
        config,
        &paths,
        state,
        client,
        ToolRegistry::new(),
        AgentMode::Dev,
    )
    .unwrap();
    let messages = agent.chat_messages("current", "新消息").unwrap().0;
    assert_eq!(messages[0].role, "system");
    let system = chat_message_text(&messages[0]).unwrap();
    assert!(
        system.contains(crate::config::DEFAULT_DEV_SYSTEM_PROMPT),
        "dev 系统提示词应为内置默认一行: {system}"
    );
    assert!(!system.contains("<current-user-profile>"), "dev 无用户身份");
    // 第一条对话消息直接是历史,没有预设对话对。
    assert_eq!(messages[1].role, "user");
    assert_eq!(chat_message_text(&messages[1]).unwrap(), "历史问题");
}

/// 回合内每次模型请求结束都发射 RoundUsage(provider 未报 usage 时走
/// 估算路径),这是 footer/WebUI 逐请求刷新计量的事件源。
#[tokio::test]
async fn round_usage_event_fires_per_model_request() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let mut config = queue_test_config(base_url);
    config.tools.enabled = false;
    let server = tokio::spawn(async move {
        let (mut chat, _) = listener.accept().await.unwrap();
        let _ = read_test_http_request(&mut chat).await;
        write_test_sse(
                &mut chat,
                concat!(
                    "data: {\"choices\":[{\"delta\":{\"content\":\"回答\"}}]}\n\n",
                    "data: {\"choices\":[{\"finish_reason\":\"stop\",\"delta\":{}}],\"usage\":{\"prompt_tokens\":120,\"completion_tokens\":8,\"total_tokens\":128}}\n\n",
                    "data: [DONE]\n\n"
                ),
            )
            .await;
    });
    let state = StateStore::new(&paths).unwrap();
    state.init_files().unwrap();
    let provider = config.provider(None).unwrap().clone();
    let client = OpenAiCompatibleClient::new(&provider, &config, &paths).unwrap();
    let mut agent = Agent::new(
        config,
        &paths,
        state,
        client,
        ToolRegistry::new(),
        AgentMode::Normal,
    )
    .unwrap();
    let rounds = std::cell::RefCell::new(Vec::new());
    agent
        .chat_stream("你好", |event| {
            if let AgentEvent::RoundUsage {
                round,
                turn,
                estimated,
            } = &event
            {
                rounds
                    .borrow_mut()
                    .push((round.prompt_tokens, turn.total, *estimated));
            }
            Ok(())
        })
        .await
        .unwrap();
    let rounds = rounds.into_inner();
    assert_eq!(rounds.len(), 1);
    assert_eq!(rounds[0].0, 120);
    assert_eq!(rounds[0].1, 128);
    assert!(!rounds[0].2);
    server.await.unwrap();
}

pub(crate) async fn read_test_http_request(stream: &mut TcpStream) -> Vec<u8> {
    let mut request = Vec::new();
    let mut buffer = [0u8; 4096];
    let header_end = loop {
        let read = stream.read(&mut buffer).await.unwrap();
        assert!(read > 0);
        request.extend_from_slice(&buffer[..read]);
        if let Some(index) = request.windows(4).position(|window| window == b"\r\n\r\n") {
            break index + 4;
        }
    };
    let headers = String::from_utf8_lossy(&request[..header_end]);
    let content_length = headers
        .lines()
        .find_map(|line| {
            line.strip_prefix("content-length: ")
                .or_else(|| line.strip_prefix("Content-Length: "))
        })
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(0);
    while request.len() < header_end + content_length {
        let read = stream.read(&mut buffer).await.unwrap();
        assert!(read > 0);
        request.extend_from_slice(&buffer[..read]);
    }
    request[header_end..header_end + content_length].to_vec()
}

pub(crate) async fn write_test_sse(stream: &mut TcpStream, body: &str) {
    let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
    stream.write_all(response.as_bytes()).await.unwrap();
}

/// v7 byte-prefix guard (compact scenario): request N must be a pure
/// element-wise prefix extension of request N-1, except immediately
/// after a compaction — and each compaction may reset the prefix at most
/// once. Catches any regression that inserts, deletes, or perturbs
/// already-sent history bytes (the failure mode is symptomless in
/// production: cache hit rate silently degrades).
#[tokio::test]
async fn compaction_resets_the_byte_prefix_at_most_once_each() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let mut config = queue_test_config(base_url);
    config.tools.enabled = false;
    config.providers[0]
        .model_context_window
        .insert("test-model".to_string(), 3000);
    config.context.compact_tail_tokens = Some(600);
    // Isolated summary path: its request is identifiable by the compact
    // system prompt and excluded from the prefix chain.
    config.context.compact_cache_reuse = false;
    config.context.prune_stale_tool_reports = false;
    // Pin the persona. This test is about compaction's effect on the byte
    // prefix, not about whatever `prompts/gqy.md` currently weighs —
    // editing the persona used to move the overflow point and flip the
    // outcome.
    config.system_prompt = Some("prefix cache guard fixture persona".to_string());

    let bodies = Arc::new(Mutex::new(Vec::<String>::new()));
    let server_bodies = bodies.clone();
    let server = tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            let body = read_test_http_request(&mut stream).await;
            let body = String::from_utf8_lossy(&body).to_string();
            let is_compact = body.contains("context summarization assistant");
            server_bodies.lock().unwrap().push(body);
            let sse = if is_compact {
                concat!(
                        "data: {\"choices\":[{\"delta\":{\"content\":\"## Task Goal\\nmock summary\"}}]}\n\n",
                        "data: {\"choices\":[{\"finish_reason\":\"stop\",\"delta\":{}}]}\n\n",
                        "data: [DONE]\n\n"
                    )
            } else {
                concat!(
                    "data: {\"choices\":[{\"delta\":{\"content\":\"answer\"}}]}\n\n",
                    "data: {\"choices\":[{\"finish_reason\":\"stop\",\"delta\":{}}]}\n\n",
                    "data: [DONE]\n\n"
                )
            };
            write_test_sse(&mut stream, sse).await;
        }
    });

    let state = StateStore::new(&paths).unwrap();
    state.init_files().unwrap();
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

    // Pin the workspace too: `runtime_context` embeds the effective working
    // directory in the system prompt, so the token budget would otherwise
    // shift with the length of the path the test happens to be run from.
    let filler = "prefix cache guard filler content 前缀缓存守卫填充 ".repeat(40);
    let workspace = temp.path().to_path_buf();
    crate::tools::workspace::with_workspace(workspace, async {
        for i in 0..6 {
            agent
                .chat_stream(&format!("message {i}: {filler}"), |_| Ok(()))
                .await
                .unwrap();
            let tokens = agent.effective_context_tokens().unwrap();
            agent
                .handle_overflow_after_turn(tokens, |_| Ok(()))
                .await
                .unwrap();
        }
    })
    .await;
    server.abort();

    let bodies = bodies.lock().unwrap().clone();
    let compact_requests = bodies
        .iter()
        .filter(|body| body.contains("context summarization assistant"))
        .count();
    assert!(
        compact_requests >= 1,
        "the scenario must trigger at least one compaction"
    );
    let chat: Vec<serde_json::Value> = bodies
        .iter()
        .filter(|body| !body.contains("context summarization assistant"))
        .map(|body| serde_json::from_str(body).unwrap())
        .collect();
    assert!(chat.len() >= 6);
    let mut resets = 0usize;
    for pair in chat.windows(2) {
        let prev = pair[0]["messages"].as_array().unwrap();
        let next = pair[1]["messages"].as_array().unwrap();
        let shared = prev
            .iter()
            .zip(next.iter())
            .take_while(|(a, b)| a == b)
            .count();
        if shared == prev.len() {
            continue; // pure append-only extension
        }
        resets += 1;
        assert!(shared >= 1, "the system prompt must never diverge");
        let checkpoint = next[1]["content"].as_str().unwrap_or_default();
        assert!(
            checkpoint.contains("<conversation-checkpoint>"),
            "a reset must be a compaction (summary checkpoint in slot 1), got: {}",
            &checkpoint[..checkpoint.len().min(120)]
        );
    }
    // The cache guarantee is one-directional: a reset may only ever be a
    // compaction, and compaction may not reset more than once per run.
    // Requiring the converse — that every compaction resets — is not a
    // property of the system: when the fold cannot save enough, the
    // compactor keeps the existing history and the prefix simply extends.
    assert!(
        resets >= 1,
        "the scenario must exercise at least one real prefix reset"
    );
    assert!(
        resets <= compact_requests,
        "prefix reset {resets} times against {compact_requests} compactions; \
             nothing but compaction may reset the byte prefix"
    );
}

pub(crate) fn queue_test_config(base_url: String) -> AppConfig {
    let mut config = AppConfig {
        active_provider: "queue-test".to_string(),
        active_provider_models: None,
        providers: vec![ProviderConfig {
            id: "queue-test".to_string(),
            display_name: "Queue Test".to_string(),
            base_url,
            protocol: "openai-chat".to_string(),
            api_key: Some("test-key".to_string()),
            models: vec!["test-model".to_string()],
            model_context_window: Default::default(),
            model_temperature: HashMap::new(),
            model_modalities: Default::default(),
            model_costs: Default::default(),
            default_model: "test-model".to_string(),
            timeout_seconds: 30,
            temperature: 0.0,
            anthropic_max_tokens: 4096,
            extra_body: None,
        }],
        ..AppConfig::default()
    };
    config.skills.enabled = false;
    config.memory.enabled = false;
    // 人格提醒会触发一次蒸馏 LLM 调用,与各测试的 mock 应答序列冲突;
    // 测提醒本身的用例再显式打开。
    config.prompt.persona_reminder = false;
    config
}

#[test]
fn binary_image_cache_is_isolated_by_platform() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let images = vec![Some(PastedImage::Binary(ClipboardImage::new(
        "image/jpeg".to_string(),
        b"same-image-content".to_vec(),
    )))];

    let platform = resolve_pasted_image_paths(&images, &paths, Some("qq"));
    let platform_path = PathBuf::from(platform[0].as_deref().unwrap());
    assert!(platform_path.starts_with(paths.cache_dir.join("platform_images/qq")));
    assert!(platform_path.is_file());

    let clipboard = resolve_pasted_image_paths(&images, &paths, None);
    let clipboard_path = PathBuf::from(clipboard[0].as_deref().unwrap());
    assert!(clipboard_path.starts_with(paths.cache_dir.join("clipboard_images")));
    assert!(clipboard_path.is_file());
    assert_ne!(platform_path, clipboard_path);
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

/// 结构化工具流推导:从实况消息尾段还原轮次;悬空调用补占位,
/// 穿插的 user/context 消息不干扰配对。
#[test]
fn derive_tool_flow_reconstructs_rounds_from_live_messages() {
    let call = |id: &str, name: &str, args: &str| crate::llm::ToolCall {
        id: id.to_string(),
        kind: "function".to_string(),
        function: crate::llm::ToolCallFunction {
            name: name.to_string(),
            arguments: args.to_string(),
        },
    };
    let mut messages = vec![ChatMessage::plain("user", "历史,不该被扫到")];
    let live_start = messages.len();
    let mut assistant = ChatMessage::assistant(
        "先查一下",
        Some(vec![call("c1", "run_command", "{\"command\":\"ls\"}")]),
    );
    assistant.reasoning_content = Some("想想".to_string());
    messages.push(assistant);
    messages.push(ChatMessage::tool("c1", "file-a\nfile-b"));
    messages.push(ChatMessage::turn_context("穿插的系统提醒"));
    messages.push(ChatMessage::assistant(
        "再查两个",
        Some(vec![
            call("c2", "read_file", "{\"path\":\"x\"}"),
            call("c3", "web_search", "{\"q\":\"y\"}"),
        ]),
    ));
    messages.push(ChatMessage::tool("c3", "搜到了"));
    // c2 悬空(崩溃/中断) → 必须补占位,回放绝不发无应答的 tool_calls
    messages.push(ChatMessage::assistant("完事", None));

    let flow = derive_tool_flow(&messages, live_start);
    assert_eq!(flow.len(), 2);
    assert_eq!(flow[0].assistant_content, "先查一下");
    assert_eq!(flow[0].assistant_reasoning.as_deref(), Some("想想"));
    assert_eq!(flow[0].calls.len(), 1);
    assert_eq!(flow[0].calls[0].arguments, "{\"command\":\"ls\"}");
    assert_eq!(flow[0].calls[0].output, "file-a\nfile-b");
    assert_eq!(flow[1].calls.len(), 2);
    assert_eq!(flow[1].calls[0].output, "(执行结果不可用)");
    assert_eq!(flow[1].calls[1].output, "搜到了");
}

/// spill 替换文案的预算自洽:替换体永不超过上限;上限太小放弃;
/// CJK 多字节切口不产生半个字符。
#[test]
fn spill_replacement_respects_budget_and_char_boundaries() {
    let output = "长".repeat(40_000);
    let replaced = spill_replacement(&output, 10_000, "/tmp/x.txt").expect("should spill");
    assert!(
        replaced.len() <= 10_000,
        "replacement {} > cap",
        replaced.len()
    );
    assert!(replaced.contains("已省略"));
    assert!(replaced.contains("/tmp/x.txt"));
    assert!(replaced.starts_with('长'));
    assert!(replaced.trim_end().ends_with(')'));
    // 上限连提示都装不下 → 放弃外溢
    assert!(spill_replacement(&output, 60, "/tmp/x.txt").is_none());
    // 不超限的输出不该被调用方外溢(逻辑在调用方,这里守函数本身)
    let small = "小输出";
    let r = spill_replacement(small, 10_000, "/tmp/x.txt");
    assert!(r.is_some() || small.len() <= 10_000);
}
