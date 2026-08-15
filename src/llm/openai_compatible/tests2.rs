//! tests2 — 自 src/llm/openai_compatible.rs 外移。
#![cfg(test)]

use super::tests3::*;
use super::*;

use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;

#[test]
fn anthropic_stream_emits_reasoning_content_and_usage() {
    let mut state = AnthropicStreamState::default();
    let mut chunks = Vec::new();
    let mut on_chunk = |chunk| {
        chunks.push(chunk);
        Ok(())
    };

    for data in [
        r#"{"type":"message_start","message":{"usage":{"input_tokens":3,"output_tokens":0}}}"#,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"想"}}"#,
        r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"答"}}"#,
        r#"{"type":"message_delta","usage":{"output_tokens":2},"delta":{"stop_reason":"end_turn"}}"#,
        r#"{"type":"message_stop"}"#,
    ] {
        let done = handle_anthropic_sse_data(data, &mut state, &mut on_chunk).unwrap();
        if data.contains("message_stop") {
            assert!(done);
        }
    }

    assert_eq!(chunks.len(), 4);
    assert_eq!(chunks[0].kind, ChatStreamKind::ReasoningPartStart);
    assert_eq!(chunks[1].kind, ChatStreamKind::Reasoning);
    assert_eq!(chunks[1].text, "想");
    assert_eq!(chunks[2].kind, ChatStreamKind::ReasoningPartEnd);
    assert_eq!(chunks[3].kind, ChatStreamKind::Content);
    assert_eq!(chunks[3].text, "答");
    let usage = state.usage.unwrap();
    assert_eq!(usage.prompt_tokens, 3);
    assert_eq!(usage.completion_tokens, 2);
    assert_eq!(usage.total_tokens, 5);
}

#[test]
fn anthropic_stream_accepts_thinking_signature_delta() {
    let mut state = AnthropicStreamState::default();
    let mut on_chunk = |_| Ok(());

    handle_anthropic_sse_data(
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sig_123"}}"#,
            &mut state,
            &mut on_chunk,
        )
        .unwrap();

    assert_eq!(state.thinking_signature.as_deref(), Some("sig_123"));
    assert!(state.reasoning.is_empty());
}

#[test]
fn anthropic_stream_separates_multiple_thinking_blocks() {
    let mut state = AnthropicStreamState::default();
    let mut chunks = Vec::new();
    let mut on_chunk = |chunk| {
        chunks.push(chunk);
        Ok(())
    };

    for data in [
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"Planning"}}"#,
        r#"{"type":"content_block_stop","index":0}"#,
        r#"{"type":"content_block_start","index":1,"content_block":{"type":"thinking","thinking":"Designing"}}"#,
        r#"{"type":"content_block_stop","index":1}"#,
    ] {
        handle_anthropic_sse_data(data, &mut state, &mut on_chunk).unwrap();
    }

    assert_eq!(state.reasoning, "Planning\n\nDesigning");
    assert_eq!(
        chunks.iter().map(|chunk| chunk.kind).collect::<Vec<_>>(),
        vec![
            ChatStreamKind::ReasoningPartStart,
            ChatStreamKind::Reasoning,
            ChatStreamKind::ReasoningPartEnd,
            ChatStreamKind::ReasoningPartStart,
            ChatStreamKind::Reasoning,
            ChatStreamKind::ReasoningPartEnd,
        ]
    );
}

#[test]
fn anthropic_stream_collects_tool_calls() {
    let mut state = AnthropicStreamState::default();
    let mut on_chunk = |_| Ok(());

    for data in [
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_1","name":"calc","input":{}}}"#,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"x\":"}}"#,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"1}"}}"#,
    ] {
        handle_anthropic_sse_data(data, &mut state, &mut on_chunk).unwrap();
    }

    let calls = state.tool_calls.finish();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].id, "toolu_1");
    assert_eq!(calls[0].function.name, "calc");
    assert_eq!(calls[0].function.arguments, r#"{"x":1}"#);
}

#[test]
fn anthropic_stream_announces_question_tool_when_block_starts() {
    let mut state = AnthropicStreamState::default();
    let mut chunks = Vec::new();
    let mut on_chunk = |chunk| {
        chunks.push(chunk);
        Ok(())
    };

    handle_anthropic_sse_data(
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_1","name":"ask_question","input":{}}}"#,
            &mut state,
            &mut on_chunk,
        )
        .unwrap();

    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].kind, ChatStreamKind::ToolCall);
    assert_eq!(chunks[0].text, "ask_question");
}

#[tokio::test]
async fn transport_connect_failure_is_retried_once() {
    let unavailable = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let unavailable_addr = unavailable.local_addr().unwrap();
    drop(unavailable);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let available_url = format!("http://{}/ok", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        read_http_headers(&mut stream).await;
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
            .await
            .unwrap();
    });

    let client = test_client(test_provider("test", "http://example.invalid/v1"));
    let unavailable_url = format!("http://{unavailable_addr}/unavailable");
    let mut builds = 0;
    let response = client
        .send_with_transport_retry("request-test", "chat.send", || {
            builds += 1;
            client.client.get(if builds == 1 {
                &unavailable_url
            } else {
                &available_url
            })
        })
        .await
        .unwrap();

    assert_eq!(builds, 2);
    assert_eq!(response.text().await.unwrap(), "ok");
    server.await.unwrap();
}

#[tokio::test]
async fn transient_http_server_errors_are_retried() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/retry", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        for status in [500, 503, 200] {
            let (mut stream, _) = listener.accept().await.unwrap();
            read_http_headers(&mut stream).await;
            let reason = if status == 200 {
                "OK"
            } else {
                "Internal Server Error"
            };
            let body = if status == 200 { "ok" } else { "error" };
            let response = format!(
                    "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            stream.write_all(response.as_bytes()).await.unwrap();
        }
    });

    let client = test_client(test_provider("test", "http://example.invalid/v1"));
    let mut builds = 0;
    let response = client
        .send_with_transport_retry("request-test", "chat.send", || {
            builds += 1;
            client.client.get(&url)
        })
        .await
        .unwrap();

    assert_eq!(builds, 3);
    assert_eq!(response.status().as_u16(), 200);
    assert_eq!(response.text().await.unwrap(), "ok");
    server.await.unwrap();
}

#[tokio::test]
async fn persistent_http_server_errors_stop_after_three_attempts() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/retry", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        for _ in 0..MAX_SEND_ATTEMPTS {
            let (mut stream, _) = listener.accept().await.unwrap();
            read_http_headers(&mut stream).await;
            stream
                    .write_all(
                        b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 5\r\nConnection: close\r\n\r\nerror",
                    )
                    .await
                    .unwrap();
        }
    });

    let client = test_client(test_provider("test", "http://example.invalid/v1"));
    let mut builds = 0;
    let error = tokio::time::timeout(
        Duration::from_secs(1),
        client.send_with_transport_retry("request-test", "chat.send", || {
            builds += 1;
            client.client.get(&url)
        }),
    )
    .await
    .expect("persistent 5xx retries did not stop")
    .unwrap_err();

    assert_eq!(builds, MAX_SEND_ATTEMPTS);
    let failure = error.downcast_ref::<HttpStatusFailure>().unwrap();
    assert_eq!(failure.status, 500);
    server.await.unwrap();
}

#[tokio::test]
async fn response_header_timeout_stops_a_stalled_endpoint() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        read_http_headers(&mut stream).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
    });

    let mut provider = test_provider("header-timeout-test", &url);
    provider.protocol = "openai-chat".to_string();
    let client = test_client(provider)
        .with_request_timeouts(Duration::from_millis(20), Duration::from_secs(1));
    let error = client
        .chat_stream(vec![ChatMessage::plain("user", "hi")], Vec::new(), |_| {
            Ok(())
        })
        .await
        .unwrap_err();

    let message = format!("{error:#}");
    assert!(message.contains("response header timed out"), "{message}");
    server.await.unwrap();
}

#[tokio::test]
async fn response_header_timeout_fails_over_to_the_next_endpoint() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (mut stalled, _) = listener.accept().await.unwrap();
        read_http_headers(&mut stalled).await;
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            drop(stalled);
        });
        let (mut healthy, _) = listener.accept().await.unwrap();
        read_http_headers(&mut healthy).await;
        write_http_sse_response(
            &mut healthy,
            concat!(
                "data: {\"choices\":[{\"delta\":{\"content\":\"fallback\"}}]}\n\n",
                "data: [DONE]\n\n"
            ),
        )
        .await;
    });

    let mut first = test_provider("header-timeout-first", &url);
    first.protocol = "openai-chat".to_string();
    let mut second = test_provider("header-timeout-second", &url);
    second.protocol = "openai-chat".to_string();
    let http_client = reqwest::Client::new();
    let endpoints = vec![
        LlmEndpoint {
            client: http_client.clone(),
            provider: first.clone(),
            api_key: "first".to_string(),
            key_index: 0,
        },
        LlmEndpoint {
            client: http_client.clone(),
            provider: second,
            api_key: "second".to_string(),
            key_index: 0,
        },
    ];
    let client = OpenAiCompatibleClient {
        client: http_client,
        provider: first,
        api_key: "first".to_string(),
        endpoints: Arc::new(endpoints),
        thinking_variants: HashMap::new(),
        reasoning_visibility: ReasoningVisibility::Hidden,
        buffered_delivery: false,
        detailed_reasoning_summary: false,
        request_timeouts: Some(RequestTimeouts {
            response_header: Duration::from_millis(20),
            stream_idle: Duration::from_secs(1),
        }),
        max_tokens_override: None,
        request_scope: "chat",
        continuation_health: ResponsesContinuationHealth::detached(),
    };

    let result = client
        .chat_buffered(vec![ChatMessage::plain("user", "hi")], Vec::new())
        .await
        .unwrap();
    assert_eq!(result.content, "fallback");
    server.await.unwrap();
}

#[tokio::test]
async fn stream_idle_timeout_stops_a_stalled_endpoint() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        read_http_headers(&mut stream).await;
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
    });

    let mut provider = test_provider("stream-idle-test", &url);
    provider.protocol = "openai-chat".to_string();
    let client = test_client(provider)
        .with_request_timeouts(Duration::from_secs(1), Duration::from_millis(20));
    let error = client
        .chat_stream(vec![ChatMessage::plain("user", "hi")], Vec::new(), |_| {
            Ok(())
        })
        .await
        .unwrap_err();

    let message = format!("{error:#}");
    assert!(message.contains("response stream was idle"), "{message}");
    server.await.unwrap();
}

/// Writes an SSE body and then hangs up without `[DONE]`, the way a proxy
/// that drops the connection mid-generation does.
async fn write_truncated_sse_response(stream: &mut tokio::net::TcpStream, body: &str) {
    // No Content-Length and no terminating chunk: the peer sees the socket
    // close, which is exactly the "graceful close mid-stream" case.
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n{body}"
    );
    stream.write_all(response.as_bytes()).await.unwrap();
    stream.flush().await.unwrap();
    stream.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_stream_that_stops_before_any_end_signal_is_not_a_completion() {
    // The failure this reproduces: the model was still emitting reasoning
    // when the connection went away, so there is no content, no tool call,
    // no `[DONE]` and no finish_reason. Accepting that as a finished turn
    // is what made a QQ reply vanish silently.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        for _ in 0..MAX_SEND_ATTEMPTS {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            read_http_headers(&mut stream).await;
            write_truncated_sse_response(
                &mut stream,
                "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"在想第一步\"}}]}\n\n",
            )
            .await;
        }
    });

    let mut provider = test_provider("truncated-stream-test", &url);
    provider.protocol = "openai-chat".to_string();
    provider.default_model = "test-model".to_string();
    let client = test_client(provider);

    let outcome = client
        .chat_stream(vec![ChatMessage::plain("user", "hi")], Vec::new(), |_| {
            Ok(())
        })
        .await;

    let error = outcome.expect_err("a truncated stream must not read as a finished turn");
    let message = format!("{error:#}");
    assert!(
        message.contains("ended before") || message.contains("提前结束"),
        "the error should name the truncation: {message}"
    );
    server.abort();
}

#[tokio::test]
async fn an_empty_error_field_does_not_fail_the_turn() {
    // Some gateways send `{"error":""}` next to the terminal usage event.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        read_http_headers(&mut stream).await;
        write_http_sse_response(
                &mut stream,
                concat!(
                    "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n",
                    "data: {\"error\":\"\",\"choices\":[{\"finish_reason\":\"stop\",\"delta\":{}}]}\n\n",
                    "data: [DONE]\n\n"
                ),
            )
            .await;
    });

    let mut provider = test_provider("empty-error-test", &url);
    provider.protocol = "openai-chat".to_string();
    provider.default_model = "test-model".to_string();
    let client = test_client(provider);

    let result = client
        .chat_stream(vec![ChatMessage::plain("user", "hi")], Vec::new(), |_| {
            Ok(())
        })
        .await
        .expect("an empty error field is not an error");
    assert_eq!(result.content, "hi");
    server.await.unwrap();
}

#[tokio::test]
async fn a_real_error_field_still_fails_the_turn() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        read_http_headers(&mut stream).await;
        write_http_sse_response(
            &mut stream,
            concat!(
                "data: {\"error\":{\"message\":\"上游炸了\"},\"choices\":[]}\n\n",
                "data: [DONE]\n\n"
            ),
        )
        .await;
    });

    let mut provider = test_provider("real-error-test", &url);
    provider.protocol = "openai-chat".to_string();
    provider.default_model = "test-model".to_string();
    let client = test_client(provider);

    let error = client
        .chat_stream(vec![ChatMessage::plain("user", "hi")], Vec::new(), |_| {
            Ok(())
        })
        .await
        .expect_err("an in-band error must not be dressed up as a completion");
    assert!(format!("{error:#}").contains("上游炸了"));
    server.abort();
}

#[tokio::test]
async fn a_lone_endpoint_still_gets_retried() {
    // Attempts used to equal endpoints, so the person with a single model
    // — the one with nowhere else to go — got no retry at all.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        // First connection dies mid-stream; the second answers properly.
        let (mut stream, _) = listener.accept().await.unwrap();
        read_http_headers(&mut stream).await;
        write_truncated_sse_response(
            &mut stream,
            "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"想了一半\"}}]}\n\n",
        )
        .await;

        let (mut stream, _) = listener.accept().await.unwrap();
        read_http_headers(&mut stream).await;
        write_http_sse_response(
            &mut stream,
            concat!(
                "data: {\"choices\":[{\"delta\":{\"content\":\"第二次成功\"}}]}\n\n",
                "data: [DONE]\n\n"
            ),
        )
        .await;
    });

    let mut provider = test_provider("lone-endpoint-test", &url);
    provider.protocol = "openai-chat".to_string();
    provider.default_model = "test-model".to_string();
    let client = test_client(provider);
    assert_eq!(client.endpoints.len(), 1, "the point is a single endpoint");

    let result = client
        .chat_stream(vec![ChatMessage::plain("user", "hi")], Vec::new(), |_| {
            Ok(())
        })
        .await
        .expect("a single endpoint should still be retried");
    assert_eq!(result.content, "第二次成功");
    server.await.unwrap();
}

#[tokio::test]
async fn buffered_delivery_lets_a_committed_attempt_be_retried() {
    // A platform turn collects a whole round before posting it, so content
    // streamed before the drop reached nobody and retrying is invisible.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        read_http_headers(&mut stream).await;
        // Content, not just reasoning: this is what used to pin the turn
        // to the failed attempt.
        write_truncated_sse_response(
            &mut stream,
            "data: {\"choices\":[{\"delta\":{\"content\":\"半句\"}}]}\n\n",
        )
        .await;

        let (mut stream, _) = listener.accept().await.unwrap();
        read_http_headers(&mut stream).await;
        write_http_sse_response(
            &mut stream,
            concat!(
                "data: {\"choices\":[{\"delta\":{\"content\":\"完整回复\"}}]}\n\n",
                "data: [DONE]\n\n"
            ),
        )
        .await;
    });

    let mut provider = test_provider("buffered-delivery-test", &url);
    provider.protocol = "openai-chat".to_string();
    provider.default_model = "test-model".to_string();
    let client = test_client(provider).with_buffered_delivery(true);

    let result = client
        .chat_stream(vec![ChatMessage::plain("user", "hi")], Vec::new(), |_| {
            Ok(())
        })
        .await
        .expect("buffered delivery means the false start was never seen");
    assert_eq!(result.content, "完整回复");
    server.await.unwrap();
}

#[tokio::test]
async fn a_stream_that_ends_on_finish_reason_alone_is_a_completion() {
    // Some OpenAI-compatible servers never send `[DONE]` (llama.cpp's
    // Responses endpoint, for one). A finish_reason is end enough.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        read_http_headers(&mut stream).await;
        write_truncated_sse_response(
            &mut stream,
            concat!(
                "data: {\"choices\":[{\"delta\":{\"content\":\"done thinking\"}}]}\n\n",
                "data: {\"choices\":[{\"finish_reason\":\"stop\",\"delta\":{}}]}\n\n"
            ),
        )
        .await;
    });

    let mut provider = test_provider("no-done-marker-test", &url);
    provider.protocol = "openai-chat".to_string();
    provider.default_model = "test-model".to_string();
    let client = test_client(provider);

    let result = client
        .chat_stream(vec![ChatMessage::plain("user", "hi")], Vec::new(), |_| {
            Ok(())
        })
        .await
        .expect("finish_reason without [DONE] is a normal completion");
    assert_eq!(result.content, "done thinking");
    server.await.unwrap();
}

#[tokio::test]
async fn endpoint_accepts_reasoning_only_completion() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        read_http_headers(&mut stream).await;
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"partial reasoning\"}}]}\n\n",
            "data: {\"choices\":[{\"finish_reason\":\"length\",\"delta\":{}}]}\n\n",
            "data: [DONE]\n\n"
        );
        write_http_sse_response(&mut stream, body).await;
    });

    let mut provider = test_provider("reasoning-only-test", &url);
    provider.protocol = "openai-chat".to_string();
    provider.default_model = "test-model".to_string();
    let client = test_client(provider);
    let mut chunks = Vec::new();

    let result = client
        .chat_stream(
            vec![ChatMessage::plain("user", "hi")],
            Vec::new(),
            |chunk| {
                chunks.push(chunk);
                Ok(())
            },
        )
        .await
        .unwrap();

    assert!(result.content.is_empty());
    assert_eq!(result.reasoning.as_deref(), Some("partial reasoning"));
    assert_eq!(
        chunks.iter().map(|chunk| chunk.kind).collect::<Vec<_>>(),
        vec![
            ChatStreamKind::ReasoningPartStart,
            ChatStreamKind::Reasoning,
            ChatStreamKind::ReasoningPartEnd,
        ]
    );
    server.await.unwrap();
}

#[tokio::test]
async fn responses_stream_rejects_eof_without_terminal_event() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        read_http_headers(&mut stream).await;
        write_http_sse_response(
                &mut stream,
                concat!(
                    "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_partial\"}}\n\n",
                    "data: {\"type\":\"response.output_text.delta\",\"item_id\":\"msg_1\",\"delta\":\"partial\"}\n\n"
                ),
            )
            .await;
    });

    let mut provider = test_provider("responses-eof-test", &url);
    provider.protocol = "openai-responses".to_string();
    provider.default_model = "gpt-5".to_string();
    let client = test_client(provider);

    let error = client
        .chat_stream(vec![ChatMessage::plain("user", "hi")], Vec::new(), |_| {
            Ok(())
        })
        .await
        .unwrap_err();

    assert!(
        format!("{error:#}").contains("before a terminal event"),
        "{error:#}"
    );
    server.await.unwrap();
}

#[tokio::test]
async fn responses_continuation_is_pinned_to_its_original_endpoint() {
    let first_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let first_url = format!("http://{}/v1", first_listener.local_addr().unwrap());
    let second_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let second_url = format!("http://{}/v1", second_listener.local_addr().unwrap());
    let first_server = tokio::spawn(async move {
        tokio::time::timeout(Duration::from_millis(200), first_listener.accept())
            .await
            .is_ok()
    });
    let second_server = tokio::spawn(async move {
        let (mut first, _) = second_listener.accept().await.unwrap();
        read_http_headers(&mut first).await;
        write_http_sse_response(
                &mut first,
                concat!(
                    "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\"}}\n\n",
                    "data: {\"type\":\"response.output_item.added\",\"item\":{\"type\":\"function_call\",\"id\":\"item_1\",\"call_id\":\"call_1\",\"name\":\"calc\",\"arguments\":\"{}\"}}\n\n",
                    "data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"function_call\",\"id\":\"item_1\",\"call_id\":\"call_1\",\"name\":\"calc\",\"arguments\":\"{}\"}}\n\n",
                    "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\"}}\n\n"
                ),
            )
            .await;

        let (mut second, _) = second_listener.accept().await.unwrap();
        read_http_headers(&mut second).await;
        write_http_sse_response(
                &mut second,
                concat!(
                    "data: {\"type\":\"response.output_text.delta\",\"item_id\":\"msg_2\",\"delta\":\"continued\"}\n\n",
                    "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_2\"}}\n\n"
                ),
            )
            .await;
    });

    let mut first_provider = test_provider("responses-shared", &first_url);
    first_provider.protocol = "openai-responses".to_string();
    first_provider.default_model = "gpt-5".to_string();
    let mut original_provider = test_provider("responses-shared", &second_url);
    original_provider.protocol = "openai-responses".to_string();
    original_provider.default_model = "gpt-5".to_string();
    let http_client = reqwest::Client::new();
    let original_endpoint = LlmEndpoint {
        client: http_client.clone(),
        provider: original_provider.clone(),
        api_key: "second".to_string(),
        key_index: 1,
    };
    let initial_client = OpenAiCompatibleClient {
        client: http_client.clone(),
        provider: original_provider.clone(),
        api_key: "second".to_string(),
        endpoints: Arc::new(vec![original_endpoint.clone()]),
        thinking_variants: HashMap::new(),
        reasoning_visibility: ReasoningVisibility::Summary,
        buffered_delivery: false,
        detailed_reasoning_summary: false,
        request_timeouts: None,
        max_tokens_override: None,
        request_scope: "chat",
        continuation_health: ResponsesContinuationHealth::detached(),
    };
    let initial_result = initial_client
        .chat_stream(vec![ChatMessage::plain("user", "hi")], Vec::new(), |_| {
            Ok(())
        })
        .await
        .unwrap();
    let continuation = initial_result
        .responses_continuation
        .as_deref()
        .unwrap()
        .clone();
    assert_eq!(continuation.endpoint_id, original_endpoint.id());

    let endpoints = vec![
        LlmEndpoint {
            client: http_client.clone(),
            provider: first_provider.clone(),
            api_key: "first".to_string(),
            key_index: 0,
        },
        original_endpoint,
    ];
    let client = OpenAiCompatibleClient {
        client: http_client,
        provider: first_provider,
        api_key: "first".to_string(),
        endpoints: Arc::new(endpoints),
        thinking_variants: HashMap::new(),
        reasoning_visibility: ReasoningVisibility::Summary,
        buffered_delivery: false,
        detailed_reasoning_summary: false,
        request_timeouts: None,
        max_tokens_override: None,
        request_scope: "chat",
        continuation_health: ResponsesContinuationHealth::detached(),
    };

    let result = client
        .chat_stream_with_continuation(
            vec![ChatMessage::tool("call_1", "tool result")],
            Vec::new(),
            Some(&continuation),
            |_| Ok(()),
        )
        .await
        .unwrap();

    assert_eq!(result.content, "continued");
    assert_eq!(result.provider_id.as_deref(), Some("responses-shared"));
    assert!(
        !first_server.await.unwrap(),
        "continuation used another endpoint"
    );
    second_server.await.unwrap();
}

#[tokio::test]
async fn insufficient_streaming_quota_falls_back_to_non_streaming_once() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (mut first, _) = listener.accept().await.unwrap();
        read_http_headers(&mut first).await;
        let quota = r#"{"error":{"message":"quota","code":"insufficient_quota"}}"#;
        let response = format!(
            "HTTP/1.1 429 Too Many Requests\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            quota.len(),
            quota
        );
        first.write_all(response.as_bytes()).await.unwrap();

        let (mut second, _) = listener.accept().await.unwrap();
        read_http_headers(&mut second).await;
        let body = r#"{"choices":[{"finish_reason":"stop","message":{"reasoning_content":"think","content":"answer"}}],"usage":{"prompt_tokens":3,"completion_tokens":2,"total_tokens":5}}"#;
        let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
        second.write_all(response.as_bytes()).await.unwrap();
    });

    let mut provider = test_provider("quota-fallback-test", &url);
    provider.protocol = "openai-chat".to_string();
    provider.default_model = "test-model".to_string();
    let client = test_client(provider);
    let mut chunks = Vec::new();
    let result = client
        .chat_stream(
            vec![ChatMessage::plain("user", "hi")],
            Vec::new(),
            |chunk| {
                chunks.push(chunk);
                Ok(())
            },
        )
        .await
        .unwrap();

    assert_eq!(result.content, "answer");
    assert_eq!(result.reasoning.as_deref(), Some("think"));
    assert_eq!(result.usage.unwrap().total_tokens, 5);
    assert_eq!(
        chunks.iter().map(|chunk| chunk.kind).collect::<Vec<_>>(),
        vec![
            ChatStreamKind::ReasoningPartStart,
            ChatStreamKind::Reasoning,
            ChatStreamKind::ReasoningPartEnd,
            ChatStreamKind::Content,
        ]
    );
    server.await.unwrap();
}

#[tokio::test]
async fn endpoint_failover_resets_partial_reasoning_before_retry() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let bodies = [
            concat!(
                "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"old\"}}]}\n\n",
                "data: {\"error\":{\"message\":\"upstream stream failed\"}}\n\n"
            ),
            concat!(
                "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"new\"}}]}\n\n",
                "data: {\"choices\":[{\"delta\":{\"content\":\"answer\"}}]}\n\n",
                "data: [DONE]\n\n"
            ),
        ];
        for body in bodies {
            let (mut stream, _) = listener.accept().await.unwrap();
            read_http_headers(&mut stream).await;
            write_http_sse_response(&mut stream, body).await;
        }
    });

    let mut first = test_provider("failover-first-test", &url);
    first.protocol = "openai-chat".to_string();
    first.default_model = "test-model".to_string();
    let mut second = test_provider("failover-second-test", &url);
    second.protocol = "openai-chat".to_string();
    second.default_model = "test-model".to_string();
    let first_client = reqwest::Client::new();
    let second_client = reqwest::Client::new();
    let endpoints = vec![
        LlmEndpoint {
            client: first_client.clone(),
            provider: first.clone(),
            api_key: "first".to_string(),
            key_index: 0,
        },
        LlmEndpoint {
            client: second_client,
            provider: second,
            api_key: "second".to_string(),
            key_index: 0,
        },
    ];
    let client = OpenAiCompatibleClient {
        client: first_client,
        provider: first,
        api_key: "first".to_string(),
        endpoints: Arc::new(endpoints),
        thinking_variants: HashMap::new(),
        reasoning_visibility: ReasoningVisibility::Summary,
        buffered_delivery: false,
        detailed_reasoning_summary: false,
        request_timeouts: None,
        max_tokens_override: None,
        request_scope: "chat",
        continuation_health: ResponsesContinuationHealth::detached(),
    };
    let mut chunks = Vec::new();

    let result = client
        .chat_stream(
            vec![ChatMessage::plain("user", "hi")],
            Vec::new(),
            |chunk| {
                chunks.push(chunk);
                Ok(())
            },
        )
        .await
        .unwrap();

    assert_eq!(result.reasoning.as_deref(), Some("new"));
    assert_eq!(result.content, "answer");
    assert_eq!(
        chunks.iter().map(|chunk| chunk.kind).collect::<Vec<_>>(),
        vec![
            ChatStreamKind::ReasoningPartStart,
            ChatStreamKind::Reasoning,
            ChatStreamKind::ReasoningReset,
            ChatStreamKind::ReasoningPartStart,
            ChatStreamKind::Reasoning,
            ChatStreamKind::ReasoningPartEnd,
            ChatStreamKind::Content,
        ]
    );
    server.await.unwrap();
}

#[tokio::test]
async fn buffered_completion_fails_over_after_partial_content() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let bodies = [
            concat!(
                "data: {\"choices\":[{\"delta\":{\"content\":\"incomplete\"}}]}\n\n",
                "data: {\"error\":{\"message\":\"upstream stream failed\"}}\n\n"
            ),
            concat!(
                "data: {\"choices\":[{\"delta\":{\"content\":\"answer\"}}]}\n\n",
                "data: [DONE]\n\n"
            ),
        ];
        for body in bodies {
            let (mut stream, _) = listener.accept().await.unwrap();
            read_http_headers(&mut stream).await;
            write_http_sse_response(&mut stream, body).await;
        }
    });

    let mut first = test_provider("buffered-first-test", &url);
    first.protocol = "openai-chat".to_string();
    let mut second = test_provider("buffered-second-test", &url);
    second.protocol = "openai-chat".to_string();
    let http_client = reqwest::Client::new();
    let endpoints = vec![
        LlmEndpoint {
            client: http_client.clone(),
            provider: first.clone(),
            api_key: "first".to_string(),
            key_index: 0,
        },
        LlmEndpoint {
            client: http_client.clone(),
            provider: second,
            api_key: "second".to_string(),
            key_index: 0,
        },
    ];
    let client = OpenAiCompatibleClient {
        client: http_client,
        provider: first,
        api_key: "first".to_string(),
        endpoints: Arc::new(endpoints),
        thinking_variants: HashMap::new(),
        reasoning_visibility: ReasoningVisibility::Summary,
        buffered_delivery: false,
        detailed_reasoning_summary: false,
        request_timeouts: None,
        max_tokens_override: None,
        request_scope: "chat",
        continuation_health: ResponsesContinuationHealth::detached(),
    };

    let result = client
        .chat_buffered(vec![ChatMessage::plain("user", "hi")], Vec::new())
        .await
        .unwrap();
    assert_eq!(result.content, "answer");
    server.await.unwrap();
}

#[tokio::test]
async fn endpoint_client_reuses_one_tcp_connection() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/reuse", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        for _ in 0..2 {
            read_http_headers(&mut stream).await;
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: keep-alive\r\n\r\nok",
                )
                .await
                .unwrap();
        }
    });

    let client = test_client(test_provider("test", "http://example.invalid/v1"));
    for request_id in ["request-one", "request-two"] {
        let endpoint_client = client.with_endpoint(&client.endpoints[0]);
        let response = tokio::time::timeout(
            Duration::from_secs(2),
            endpoint_client.send_with_transport_retry(request_id, "chat.send", || {
                endpoint_client.client.get(&url)
            }),
        )
        .await
        .expect("request timed out instead of reusing the connection")
        .unwrap();
        assert_eq!(response.text().await.unwrap(), "ok");
    }
    tokio::time::timeout(Duration::from_secs(2), server)
        .await
        .expect("server did not observe two requests on one connection")
        .unwrap();
}

#[tokio::test]
async fn transport_error_keeps_source_chain_without_url() {
    let unavailable = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = unavailable.local_addr().unwrap();
    drop(unavailable);
    let url = format!("http://{addr}/secret?api_key=do-not-log");
    let client = test_client(test_provider("test", "http://example.invalid/v1"));

    let error = client
        .send_with_transport_retry("request-test", "chat.send", || client.client.get(&url))
        .await
        .unwrap_err();
    let rendered = format!("{error:#}");

    assert!(rendered.contains("chat.send transport failed (connect)"));
    assert!(rendered.contains("error sending request"));
    assert!(!rendered.contains("api_key"));
    assert!(!rendered.contains("do-not-log"));
}
