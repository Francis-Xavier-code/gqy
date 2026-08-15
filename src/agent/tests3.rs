//! tests3 — 自 src/agent/mod.rs 外移。
#![cfg(test)]

use super::tests4::*;
pub(crate) use super::*;
use crate::tools::{empty_parameters, ToolSpec};
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;

#[tokio::test]
async fn parallel_task_calls_run_concurrently_and_map_outputs() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let config = AppConfig::default();
    let state = StateStore::new(&paths).unwrap();
    state.init_files().unwrap();
    let client =
        OpenAiCompatibleClient::new(config.provider(None).unwrap(), &config, &paths).unwrap();
    let mut registry = ToolRegistry::new();
    registry.register(crate::tools::ToolSpec::new(
        "task",
        "stub subagent",
        crate::tools::empty_parameters(),
        |args| async move {
            tokio::time::sleep(Duration::from_millis(80)).await;
            Ok(format!(
                "done:{}",
                args.get("n")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("?")
            ))
        },
    ));
    let agent = Agent::new(
        config,
        &paths,
        state.clone(),
        client,
        registry,
        AgentMode::Normal,
    )
    .unwrap();

    let calls: Vec<crate::llm::ToolCall> = (0..3)
        .map(|index| crate::llm::ToolCall {
            id: format!("call_{index}"),
            kind: "function".to_string(),
            function: crate::llm::ToolCallFunction {
                name: "task".to_string(),
                arguments: format!(r#"{{"n":"{index}"}}"#),
            },
        })
        .collect();
    let mut events = Vec::new();
    let started = std::time::Instant::now();
    let outputs = agent
        .execute_parallel_task_calls(&calls, &std::collections::BTreeSet::new(), &mut |event| {
            match &event {
                AgentEvent::ToolCall { call_id, .. } => events.push((call_id.clone(), "call")),
                AgentEvent::ToolResult {
                    call_id, ok: true, ..
                } => events.push((call_id.clone(), "ok")),
                AgentEvent::ToolResult {
                    call_id, ok: false, ..
                } => events.push((call_id.clone(), "err")),
                _ => {}
            }
            Ok(())
        })
        .await
        .unwrap();
    let elapsed = started.elapsed();

    assert_eq!(outputs.len(), 3);
    for index in 0..3 {
        assert_eq!(outputs[&index].output, format!("done:{index}"));
    }
    // Three 80ms tasks run concurrently, not sequentially (~240ms).
    assert!(
        elapsed < Duration::from_millis(200),
        "tasks did not run in parallel: {elapsed:?}"
    );
    for index in 0..3 {
        let call_id = format!("call_{index}");
        assert!(events.contains(&(call_id.clone(), "call")));
        assert!(events.contains(&(call_id, "ok")));
    }

    // Fewer than two task calls: empty map, serial path handles it.
    let single = agent
        .execute_parallel_task_calls(&calls[..1], &std::collections::BTreeSet::new(), &mut |_| {
            Ok(())
        })
        .await
        .unwrap();
    assert!(single.is_empty());
}

#[test]
fn trim_visible_context_keeps_summary_and_removes_oldest_turn() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let config = AppConfig {
        tools: crate::config::ToolsConfig {
            enabled: false,
            ..AppConfig::default().tools
        },
        ..AppConfig::default()
    };
    let state = StateStore::new(&paths).unwrap();
    state.init_files().unwrap();
    let client =
        OpenAiCompatibleClient::new(config.provider(None).unwrap(), &config, &paths).unwrap();
    let mut agent = Agent::new(
        config,
        &paths,
        state.clone(),
        client,
        ToolRegistry::new(),
        AgentMode::Normal,
    )
    .unwrap();
    state
        .insert_summary_turn(&"summary ".repeat(2_000), TurnTokens::default(), true)
        .unwrap();
    for id in ["t1", "t2"] {
        state
            .start_turn(id, &format!("{id} {}", "question ".repeat(2_000)), 999999)
            .unwrap();
        state
            .complete_turn(id, &"answer ".repeat(2_000), None)
            .unwrap();
    }
    agent.trim_at_ratio = 1.0;
    let context_window = agent.effective_context_tokens().unwrap() as usize;
    let choice = agent.config.active_provider_model_choices().remove(0);
    agent
        .config
        .providers
        .iter_mut()
        .find(|provider| provider.id == choice.provider_id)
        .unwrap()
        .model_context_window
        .insert(choice.model, context_window);
    assert_eq!(agent.context_window(), Some(context_window));

    let evicted = agent.trim_visible_context().unwrap();

    assert!(!evicted.is_empty());
    let visible = state.load_visible_turns().unwrap();
    assert_eq!(visible.len(), 2);
    assert!(visible[0].is_summary);
    assert_eq!(visible[1].turn_id, "t2");
}

#[test]
fn trim_accounts_for_tool_definitions_unloaded_with_a_popped_turn() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let mut config = AppConfig::default();
    config.tools.loading_mode = "hybrid".to_string();
    let state = StateStore::new(&paths).unwrap();
    state.init_files().unwrap();
    let client =
        OpenAiCompatibleClient::new(config.provider(None).unwrap(), &config, &paths).unwrap();
    let mut tools = ToolRegistry::new();
    tools.register(
        ToolSpec::new(
            "heavy_context_tool",
            "heavy context ".repeat(20_000),
            empty_parameters(),
            |_| async { Ok(String::new()) },
        )
        .with_always_loaded(false),
    );
    let mut agent = Agent::new(
        config,
        &paths,
        state.clone(),
        client,
        tools,
        AgentMode::Normal,
    )
    .unwrap();
    for id in ["t1", "t2"] {
        state.start_turn(id, id, 999999).unwrap();
        state.complete_turn(id, "reply", None).unwrap();
    }
    state
        .add_session_loaded_tools(&["heavy_context_tool".to_string()], Some("t1"))
        .unwrap();
    agent.trim_at_ratio = 1.0;
    agent.trim_batch_ratio = 0.5;
    let context_window = agent.effective_context_tokens().unwrap() as usize;
    let choice = agent.config.active_provider_model_choices().remove(0);
    agent
        .config
        .providers
        .iter_mut()
        .find(|provider| provider.id == choice.provider_id)
        .unwrap()
        .model_context_window
        .insert(choice.model, context_window);

    agent.trim_visible_context().unwrap();

    let visible = state.load_visible_turns().unwrap();
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].turn_id, "t2");
    assert!(state.load_session_loaded_tools().unwrap().is_empty());
}

#[test]
fn trim_ignores_stale_loaded_tool_sources_when_persistence_is_disabled() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let mut config = AppConfig::default();
    config.tools.loading_mode = "hybrid".to_string();
    config.tools.persist_loaded_tools = false;
    let state = StateStore::new(&paths).unwrap();
    state.init_files().unwrap();
    let client =
        OpenAiCompatibleClient::new(config.provider(None).unwrap(), &config, &paths).unwrap();
    let mut tools = ToolRegistry::new();
    tools.register(
        ToolSpec::new(
            "stale_heavy_tool",
            "stale heavy context ".repeat(20_000),
            empty_parameters(),
            |_| async { Ok(String::new()) },
        )
        .with_always_loaded(false),
    );
    let mut agent = Agent::new(
        config,
        &paths,
        state.clone(),
        client,
        tools,
        AgentMode::Normal,
    )
    .unwrap();
    for id in ["t1", "t2"] {
        state.start_turn(id, id, 999999).unwrap();
        state.complete_turn(id, "reply", None).unwrap();
    }
    state
        .add_session_loaded_tools(&["stale_heavy_tool".to_string()], Some("t1"))
        .unwrap();
    agent.trim_at_ratio = 1.0;
    agent.trim_batch_ratio = 0.5;
    let context_window = agent.effective_context_tokens().unwrap() as usize;
    let choice = agent.config.active_provider_model_choices().remove(0);
    agent
        .config
        .providers
        .iter_mut()
        .find(|provider| provider.id == choice.provider_id)
        .unwrap()
        .model_context_window
        .insert(choice.model, context_window);

    agent.trim_visible_context().unwrap();

    assert!(state.load_visible_turns().unwrap().is_empty());
}

#[test]
fn explicit_pop_archives_context_content_but_not_reasoning() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let config = AppConfig::default();
    let state = StateStore::new(&paths).unwrap();
    state.start_turn("t1", "promptonlyalpha", 999999).unwrap();
    state
        .complete_turn("t1", "answeronlybeta", Some("reasoningonlyquasar"))
        .unwrap();
    state
        .append_persisted_context("t1", "toolonlygamma")
        .unwrap();
    let memory = MemoryStore::new(&config, &paths);
    let turns = state.oldest_evictable_visible_turns(1).unwrap();

    archive_and_delete_visible_turns(&state, &memory, &turns).unwrap();

    assert!(state.load_visible_turns().unwrap().is_empty());
    for query in ["promptonlyalpha", "answeronlybeta", "toolonlygamma"] {
        assert!(
            !memory.search_evicted_context(query, 10).unwrap()["results"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
    assert!(memory
        .search_evicted_context("reasoningonlyquasar", 10)
        .unwrap()["results"]
        .as_array()
        .unwrap()
        .is_empty());
}

#[test]
fn explicit_pop_still_deletes_when_evicted_context_archiving_is_disabled() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let mut config = AppConfig::default();
    config.memory.evicted_context_enabled = false;
    let state = StateStore::new(&paths).unwrap();
    state.start_turn("t1", "unarchived-marker", 999999).unwrap();
    state.complete_turn("t1", "reply", None).unwrap();
    let memory = MemoryStore::new(&config, &paths);
    let turns = state.oldest_evictable_visible_turns(1).unwrap();

    archive_and_delete_visible_turns(&state, &memory, &turns).unwrap();

    assert!(state.load_visible_turns().unwrap().is_empty());
    assert!(memory
        .search_evicted_context("unarchived-marker", 10)
        .unwrap()["results"]
        .as_array()
        .unwrap()
        .is_empty());
}

#[test]
fn explicit_pop_does_not_archive_a_turn_removed_before_commit() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let config = AppConfig::default();
    let state = StateStore::new(&paths).unwrap();
    state
        .start_turn("t1", "stale-archive-quasar", 999999)
        .unwrap();
    state.complete_turn("t1", "reply", None).unwrap();
    let turns = state.oldest_evictable_visible_turns(1).unwrap();
    state.delete_visible_turns(&["t1".to_string()]).unwrap();
    let memory = MemoryStore::new(&config, &paths);

    assert!(archive_and_delete_visible_turns(&state, &memory, &turns).is_err());

    assert!(memory
        .search_evicted_context("stale-archive-quasar", 10)
        .unwrap()["results"]
        .as_array()
        .unwrap()
        .is_empty());
}

#[test]
fn failed_concurrent_pop_preserves_archive_from_the_successful_pop() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let config = AppConfig::default();
    let state = StateStore::new(&paths).unwrap();
    state
        .start_turn("t1", "successful-pop-quasar", 999999)
        .unwrap();
    state.complete_turn("t1", "reply", None).unwrap();
    let turns = state.oldest_evictable_visible_turns(1).unwrap();
    let memory = MemoryStore::new(&config, &paths);

    archive_and_delete_visible_turns(&state, &memory, &turns).unwrap();
    assert!(archive_and_delete_visible_turns(&state, &memory, &turns).is_err());

    assert!(!memory
        .search_evicted_context("successful-pop-quasar", 10)
        .unwrap()["results"]
        .as_array()
        .unwrap()
        .is_empty());
}

#[test]
fn explicit_pop_removes_new_archive_when_the_turn_still_exists_hidden() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let config = AppConfig::default();
    let state = StateStore::new(&paths).unwrap();
    state
        .start_turn("t1", "hidden-stale-quasar", 999999)
        .unwrap();
    state.complete_turn("t1", "reply", None).unwrap();
    let turns = state.oldest_evictable_visible_turns(1).unwrap();
    state
        .replace_visible_with_summary(
            &["t1".to_string()],
            &["t1".to_string()],
            "summary",
            TurnTokens::default(),
            false,
            None,
        )
        .unwrap();
    let memory = MemoryStore::new(&config, &paths);

    assert!(archive_and_delete_visible_turns(&state, &memory, &turns).is_err());

    assert!(memory
        .search_evicted_context("hidden-stale-quasar", 10)
        .unwrap()["results"]
        .as_array()
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn queued_prompt_continues_after_a_completed_model_call() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let mut config = queue_test_config(base_url);
    config.tools.enabled = false;
    config.providers[0].model_modalities.insert(
        "test-model".to_string(),
        vec!["text".to_string(), "image".to_string()],
    );
    let control =
        AgentTurnControl::new(AgentMode::Normal, ToolRegistry::new(), ToolRegistry::new());
    let server_control = control.clone();
    let (request_tx, request_rx) = oneshot::channel();
    let (redo_request_tx, redo_request_rx) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut first, _) = listener.accept().await.unwrap();
        let _ = read_test_http_request(&mut first).await;
        server_control.set_mode(AgentMode::Dev);
        write_test_sse(
            &mut first,
            concat!(
                "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"first reasoning\"}}]}\n\n",
                "data: {\"choices\":[{\"delta\":{\"content\":\"first answer\"}}]}\n\n",
                "data: {\"choices\":[{\"finish_reason\":\"stop\",\"delta\":{}}]}\n\n",
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
                "data: {\"choices\":[{\"delta\":{\"content\":\"continued answer\"}}]}\n\n",
                "data: {\"choices\":[{\"finish_reason\":\"stop\",\"delta\":{}}]}\n\n",
                "data: [DONE]\n\n"
            ),
        )
        .await;

        let (mut third, _) = listener.accept().await.unwrap();
        let request = read_test_http_request(&mut third).await;
        let _ = redo_request_tx.send(request);
        write_test_sse(
            &mut third,
            concat!(
                "data: {\"choices\":[{\"delta\":{\"content\":\"redone answer\"}}]}\n\n",
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
        ToolRegistry::new(),
        AgentMode::Normal,
    )
    .unwrap();
    state
        .enqueue_prompt(
            "q1",
            "queued followup",
            "queued followup",
            &[QueuedPromptAttachment::Binary {
                mime: "image/png".to_string(),
                data_base64: base64::engine::general_purpose::STANDARD.encode(b"image-data"),
            }],
        )
        .unwrap();

    let result = agent
        .chat_stream_with_control("initial prompt", &[], &control, |_| Ok(()))
        .await
        .unwrap();

    assert_eq!(result.content, "continued answer");
    assert_eq!(agent.mode(), AgentMode::Dev);
    let request: serde_json::Value = serde_json::from_slice(&request_rx.await.unwrap()).unwrap();
    let messages = request["messages"].as_array().unwrap();
    let first_answer = messages
        .iter()
        .position(|message| message["role"] == "assistant" && message["content"] == "first answer")
        .unwrap();
    let followup = messages
        .iter()
        .position(|message| {
            message["role"] == "user"
                && message["content"].as_array().is_some_and(|parts| {
                    parts
                        .iter()
                        .any(|part| part["type"] == "text" && part["text"] == "queued followup")
                        && parts.iter().any(|part| part["type"] == "image_url")
                })
        })
        .unwrap();
    // 跨轮思考回放退役:live 与回放同刀,followup 边界不再夹带思维链。
    assert!(!messages.iter().any(|message| {
        message["role"] == "user"
            && message["content"]
                .as_str()
                .is_some_and(|content| content.contains("<previous_assistant_reasoning>"))
    }));
    assert!(first_answer < followup);
    let turns = state.load_turns().unwrap();
    assert_eq!(
        turns[0].followups[0].preceding_assistant_content.as_deref(),
        Some("first answer")
    );
    assert_eq!(
        turns[0].followups[0]
            .preceding_assistant_reasoning
            .as_deref(),
        Some("first reasoning")
    );
    let history = agent.chat_messages("", "next prompt").unwrap().0;
    assert!(history.iter().any(|message| {
        matches!(
            message.content.as_ref(),
            Some(ChatContent::Parts(parts))
                if parts.iter().any(|part| matches!(part, ChatContentPart::ImageUrl { .. }))
        )
    }));
    let candidate = state.redo_candidate().unwrap().unwrap();
    let redo = agent
        .redo_stream_with_control(
            &candidate,
            vec![RedoPromptInput {
                prompt_id: "q1".to_string(),
                content: "edited followup".to_string(),
                display_content: "edited followup".to_string(),
                images: vec![Some(PastedImage::Binary(ClipboardImage::new(
                    "image/png".to_string(),
                    b"image-data".to_vec(),
                )))],
            }],
            &control,
            |_| Ok(()),
        )
        .await
        .unwrap();
    assert_eq!(redo.content, "redone answer");
    let redo_request: serde_json::Value =
        serde_json::from_slice(&redo_request_rx.await.unwrap()).unwrap();
    let redo_messages = redo_request["messages"].as_array().unwrap();
    assert!(redo_messages
        .iter()
        .any(|message| { message["role"] == "assistant" && message["content"] == "first answer" }));
    assert!(redo_messages.iter().any(|message| {
        message["role"] == "user"
            && message["content"].as_array().is_some_and(|parts| {
                parts
                    .iter()
                    .any(|part| part["type"] == "text" && part["text"] == "edited followup")
            })
    }));
    assert!(!redo_messages.iter().any(|message| {
        message["role"] == "assistant" && message["content"] == "continued answer"
    }));
    let turn = state.load_turns().unwrap().remove(0);
    assert_eq!(turn.assistant_content, "redone answer");
    assert_eq!(turn.followups[0].content, "edited followup");
    assert_eq!(turn.revision, 1);
    server.await.unwrap();
}

#[tokio::test]
async fn supersede_restarts_the_same_turn_without_replaying_partial_output() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let mut config = queue_test_config(base_url);
    config.tools.enabled = false;
    let (partial_tx, partial_rx) = oneshot::channel();
    let (second_request_tx, second_request_rx) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut first, _) = listener.accept().await.unwrap();
        let _ = read_test_http_request(&mut first).await;
        first
            .write_all(
                concat!(
                    "HTTP/1.1 200 OK\r\n",
                    "content-type: text/event-stream\r\n",
                    "connection: close\r\n\r\n",
                    "data: {\"choices\":[{\"delta\":{\"content\":\"discarded partial\"}}]}\n\n"
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        first.flush().await.unwrap();
        let _ = partial_tx.send(());
        tokio::time::sleep(Duration::from_millis(100)).await;
        drop(first);

        let (mut second, _) = listener.accept().await.unwrap();
        let request = read_test_http_request(&mut second).await;
        let _ = second_request_tx.send(request);
        write_test_sse(
            &mut second,
            concat!(
                "data: {\"choices\":[{\"delta\":{\"content\":\"updated final\"}}]}\n\n",
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
        ToolRegistry::new(),
        AgentMode::Normal,
    )
    .unwrap();
    let signal = Arc::new(TurnSupersedeSignal::default());
    let mut control =
        AgentTurnControl::new(AgentMode::Normal, ToolRegistry::new(), ToolRegistry::new());
    control.set_supersede_signal(signal.clone());
    let events = Arc::new(Mutex::new(Vec::<&'static str>::new()));
    let event_log = events.clone();
    let chat = agent.chat_stream_with_control("original", &[], &control, move |event| {
        if matches!(event, AgentEvent::GenerationSuperseded { .. }) {
            event_log.lock().unwrap().push("superseded");
        }
        Ok(())
    });
    let enqueue = async {
        partial_rx.await.unwrap();
        state
            .enqueue_prompt("update", "changed requirement", "changed requirement", &[])
            .unwrap();
        signal.trigger();
    };
    let (result, ()) = tokio::join!(chat, enqueue);
    let result = result.unwrap();
    assert_eq!(result.content, "updated final");
    assert_eq!(&*events.lock().unwrap(), &["superseded"]);
    let request: Value = serde_json::from_slice(&second_request_rx.await.unwrap()).unwrap();
    let serialized = serde_json::to_string(&request["messages"]).unwrap();
    assert!(serialized.contains("changed requirement"));
    assert!(!serialized.contains("discarded partial"));
    let turns = state.load_turns().unwrap();
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].assistant_content, "updated final");
    assert_eq!(turns[0].followups.len(), 1);
    assert!(turns[0].followups[0].preceding_assistant_content.is_none());
    server.await.unwrap();
}

#[tokio::test]
async fn responses_tool_round_uses_previous_response_id_and_only_new_input() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let mut config = queue_test_config(base_url);
    config.tools.enabled = true;
    config.tools.loading_mode = "full".to_string();
    config.skills.enabled = false;
    config.memory.enabled = false;
    config.providers[0].protocol = "openai-responses".to_string();
    config.providers[0].models = vec!["gpt-5".to_string()];
    config.providers[0].default_model = "gpt-5".to_string();

    let mut tools = ToolRegistry::new();
    tools.register(ToolSpec::new(
        "responses_continuation_tool",
        "returns a fixed result",
        empty_parameters(),
        |_| async { Ok("tool finished".to_string()) },
    ));
    let control = AgentTurnControl::new(AgentMode::Normal, tools.clone(), tools.clone());
    let server_control = control.clone();

    let (first_request_tx, first_request_rx) = oneshot::channel();
    let (second_request_tx, second_request_rx) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut first, _) = listener.accept().await.unwrap();
        let first_request = read_test_http_request(&mut first).await;
        let _ = first_request_tx.send(first_request);
        server_control.set_mode(AgentMode::Dev);
        write_test_sse(
                &mut first,
                concat!(
                    "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\"}}\n\n",
                    "data: {\"type\":\"response.output_item.added\",\"item\":{\"type\":\"function_call\",\"id\":\"item_1\",\"call_id\":\"call_1\",\"name\":\"responses_continuation_tool\",\"arguments\":\"\"}}\n\n",
                    "data: {\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"item_1\",\"delta\":\"{}\"}\n\n",
                    "data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"function_call\",\"id\":\"item_1\",\"call_id\":\"call_1\",\"name\":\"responses_continuation_tool\",\"arguments\":\"{}\"}}\n\n",
                    "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\",\"usage\":{\"input_tokens\":5,\"output_tokens\":2,\"total_tokens\":7}}}\n\n"
                ),
            )
            .await;

        let (mut second, _) = listener.accept().await.unwrap();
        let second_request = read_test_http_request(&mut second).await;
        let _ = second_request_tx.send(second_request);
        write_test_sse(
                &mut second,
                concat!(
                    "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_2\"}}\n\n",
                    "data: {\"type\":\"response.output_text.delta\",\"item_id\":\"msg_2\",\"delta\":\"final answer\"}\n\n",
                    "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_2\"}}\n\n"
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
        tools,
        AgentMode::Normal,
    )
    .unwrap();
    state
        .enqueue_prompt("q1", "queued followup", "queued followup", &[])
        .unwrap();

    let result = agent
        .chat_stream_with_control("initial prompt", &[], &control, |_| Ok(()))
        .await
        .unwrap();

    assert_eq!(result.content, "final answer");
    assert_eq!(agent.mode(), AgentMode::Dev);
    assert!(result.responses_continuation.is_none());
    assert!(result.usage_estimated);
    let tool_only_tokens =
        overflow::estimate_messages_tokens(&[ChatMessage::tool("call_1", "tool finished")]) as u64;
    assert!(result.usage.as_ref().unwrap().prompt_tokens > 5 + tool_only_tokens);
    let first_request: Value = serde_json::from_slice(&first_request_rx.await.unwrap()).unwrap();
    assert!(first_request.get("previous_response_id").is_none());
    assert!(first_request["input"].as_array().is_some_and(|input| {
        input.iter().any(|item| item["role"] == "user")
            && input.iter().any(|item| item["role"] == "system")
    }));

    let second_request: Value = serde_json::from_slice(&second_request_rx.await.unwrap()).unwrap();
    assert_eq!(second_request["previous_response_id"], "resp_1");
    let input = second_request["input"].as_array().unwrap();
    let function_output = input
        .iter()
        .find(|item| item["type"] == "function_call_output")
        .unwrap();
    assert_eq!(function_output["call_id"], "call_1");
    assert_eq!(function_output["output"], "tool finished");
    let function_index = input
        .iter()
        .position(|item| item["type"] == "function_call_output")
        .unwrap();
    // Responses-style user items carry their text as `input_text` parts,
    // so the block has to be read through both shapes.
    let item_text = |item: &Value| -> String {
        match &item["content"] {
            Value::String(text) => text.clone(),
            Value::Array(parts) => parts
                .iter()
                .filter_map(|part| part["text"].as_str())
                .collect::<Vec<_>>()
                .join(""),
            _ => String::new(),
        }
    };
    let is_mode_update = |item: &Value| {
        let text = item_text(item);
        item["role"] == "user" && text.contains("<mode-update active=\"dev\">")
    };
    let mode_index = input.iter().position(is_mode_update).unwrap();
    assert!(input.iter().any(is_mode_update));
    let queued_index = input
        .iter()
        .position(|item| {
            item["role"] == "user"
                && item["content"].as_array().is_some_and(|parts| {
                    parts.iter().any(|part| {
                        part["type"] == "input_text" && part["text"] == "queued followup"
                    })
                })
        })
        .unwrap();
    assert!(input.iter().any(|item| {
        item["role"] == "user"
            && item["content"].as_array().is_some_and(|parts| {
                parts
                    .iter()
                    .any(|part| part["type"] == "input_text" && part["text"] == "queued followup")
            })
    }));
    assert!(function_index < mode_index && mode_index < queued_index);
    assert!(!serde_json::to_string(input)
        .unwrap()
        .contains("initial prompt"));
    assert!(second_request["tools"].as_array().is_some_and(|tools| {
        tools
            .iter()
            .any(|tool| tool["name"] == "responses_continuation_tool")
    }));
    assert_eq!(
        state.load_turns().unwrap()[0].assistant_content,
        "final answer"
    );
    server.await.unwrap();
}
