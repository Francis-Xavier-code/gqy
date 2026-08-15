//! tests — 自 src/llm/openai_compatible.rs 外移。
#![cfg(test)]

use super::*;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{ChatContent, ChatContentPart, ImageUrlContent};
    use std::sync::atomic::AtomicUsize;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[derive(Debug)]
    struct ResponsesTestOutput {
        content: String,
        chunks: Vec<ChatStreamChunk>,
        response_id: Option<String>,
        terminal: bool,
    }

    fn run_responses_test_events(lines: &[&str]) -> Result<ResponsesTestOutput> {
        let mut content = String::new();
        let mut content_emitted = 0usize;
        let mut reasoning = String::new();
        let mut reasoning_emitted = 0usize;
        let mut reasoning_part_active = false;
        let mut usage = None;
        let mut content_started = false;
        let mut output_text_delta_parts = HashSet::new();
        let mut refusal_delta_parts = HashSet::new();
        let mut response_id = None;
        let mut tool_calls = ResponsesToolAccumulator::default();
        let mut chunks = Vec::new();
        let mut terminal = false;
        let mut on_chunk = |chunk| {
            chunks.push(chunk);
            Ok(())
        };
        for line in lines {
            terminal = handle_responses_sse_line(
                line,
                &mut content,
                &mut content_emitted,
                &mut reasoning,
                &mut reasoning_emitted,
                &mut reasoning_part_active,
                &mut usage,
                &mut content_started,
                &mut output_text_delta_parts,
                &mut refusal_delta_parts,
                &mut response_id,
                &mut tool_calls,
                &mut on_chunk,
            )?;
            if terminal {
                break;
            }
        }
        Ok(ResponsesTestOutput {
            content,
            chunks,
            response_id,
            terminal,
        })
    }

    #[test]
    fn tool_call_accumulators_drop_out_of_range_indices() {
        // A malformed upstream chunk with a huge index must not make the
        // accumulator allocate gigabytes (regression: 160GB VmSize).
        let mut acc = ToolCallAccumulator::default();
        let huge = ToolCallDelta {
            index: 1 << 30,
            id: Some("x".to_string()),
            kind: None,
            function: ToolCallFunctionDelta {
                name: Some("evil".to_string()),
                arguments: None,
            },
        };
        assert!(acc.push(huge).is_none());
        assert!(acc.calls.is_empty());
        let ok = ToolCallDelta {
            index: 0,
            id: Some("a".to_string()),
            kind: None,
            function: ToolCallFunctionDelta {
                name: Some("fine".to_string()),
                arguments: Some("{}".to_string()),
            },
        };
        assert!(acc.push(ok).is_some());
        assert_eq!(acc.calls.len(), 1);

        let mut anthropic = AnthropicToolAccumulator::default();
        assert!(anthropic
            .start(
                usize::MAX,
                AnthropicStreamBlock {
                    kind: "tool_use".to_string(),
                    id: Some("x".to_string()),
                    name: Some("evil".to_string()),
                    text: None,
                    thinking: None,
                },
            )
            .is_none());
        anthropic.append_arguments(1 << 30, "{}".to_string());
        assert!(anthropic.calls.is_empty());
    }

    #[test]
    fn stream_chunk_accepts_null_tool_calls() {
        let raw = r#"{"choices":[{"delta":{"content":"在","tool_calls":null}}]}"#;
        let parsed: ChatStreamResponse = serde_json::from_str(raw).unwrap();

        assert_eq!(parsed.choices.len(), 1);
        assert_eq!(parsed.choices[0].delta.content.as_deref(), Some("在"));
        assert!(parsed.choices[0].delta.tool_calls.is_empty());
    }

    #[test]
    fn stream_chunk_accepts_taotoken_glm_nulls() {
        let raw = r#"{"created":1782742568,"usage":null,"model":"glm_for_coding","id":"9981f6121a31494387131c61bd2ad7a2","choices":[{"finish_reason":null,"matched_stop":null,"delta":{"role":null,"tool_calls":null,"content":"在","reasoning_content":null},"index":0,"logprobs":null}],"object":"chat.completion.chunk"}"#;
        let parsed: ChatStreamResponse = serde_json::from_str(raw).unwrap();

        assert!(parsed.usage.is_none());
        assert_eq!(parsed.choices.len(), 1);
        assert!(parsed.choices[0].finish_reason.is_none());
        assert_eq!(parsed.choices[0].delta.content.as_deref(), Some("在"));
        assert!(parsed.choices[0].delta.reasoning_content.is_none());
        assert!(parsed.choices[0].delta.tool_calls.is_empty());
    }

    #[test]
    fn stream_chunk_emits_glm_reasoning_content() {
        let mut content = String::new();
        let mut content_emitted = 0usize;
        let mut reasoning = String::new();
        let mut reasoning_emitted = 0usize;
        let mut reasoning_part_active = false;
        let mut finish_reason = None;
        let mut usage = None;
        let mut tool_calls = ToolCallAccumulator::default();
        let mut chunks = Vec::new();
        let mut on_chunk = |chunk| {
            chunks.push(chunk);
            Ok(())
        };

        handle_sse_line(
            r#"data: {"choices":[{"finish_reason":"length","delta":{"reasoning_content":"先想一下","content":"","tool_calls":null}}]}"#,
            &mut content,
            &mut content_emitted,
            &mut reasoning,
            &mut reasoning_emitted,
            &mut reasoning_part_active,
            &mut finish_reason,
            &mut usage,
            &mut tool_calls,
            &mut on_chunk,
        )
        .unwrap();

        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].kind, ChatStreamKind::ReasoningPartStart);
        assert_eq!(chunks[1].kind, ChatStreamKind::Reasoning);
        assert_eq!(chunks[1].text, "先想一下");
        assert_eq!(finish_reason.as_deref(), Some("length"));
    }

    #[test]
    fn chat_stream_announces_question_tool_before_arguments() {
        let mut content = String::new();
        let mut content_emitted = 0usize;
        let mut reasoning = String::new();
        let mut reasoning_emitted = 0usize;
        let mut reasoning_part_active = false;
        let mut finish_reason = None;
        let mut usage = None;
        let mut tool_calls = ToolCallAccumulator::default();
        let mut chunks = Vec::new();
        let mut on_chunk = |chunk| {
            chunks.push(chunk);
            Ok(())
        };

        handle_sse_line(
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"ask_question","arguments":""}}]}}]}"#,
            &mut content,
            &mut content_emitted,
            &mut reasoning,
            &mut reasoning_emitted,
            &mut reasoning_part_active,
            &mut finish_reason,
            &mut usage,
            &mut tool_calls,
            &mut on_chunk,
        )
        .unwrap();

        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].kind, ChatStreamKind::ToolCall);
        assert_eq!(chunks[0].text, "ask_question");
    }

    #[test]
    fn chat_stream_surfaces_sse_error_objects() {
        let mut content = String::new();
        let mut content_emitted = 0usize;
        let mut reasoning = String::new();
        let mut reasoning_emitted = 0usize;
        let mut reasoning_part_active = false;
        let mut finish_reason = None;
        let mut usage = None;
        let mut tool_calls = ToolCallAccumulator::default();

        let error = handle_sse_line(
            r#"data: {"error":{"message":"upstream generation timed out"}}"#,
            &mut content,
            &mut content_emitted,
            &mut reasoning,
            &mut reasoning_emitted,
            &mut reasoning_part_active,
            &mut finish_reason,
            &mut usage,
            &mut tool_calls,
            &mut |_| Ok(()),
        )
        .unwrap_err();

        assert!(format!("{error:#}").contains("upstream generation timed out"));
    }

    #[test]
    fn reasoning_only_stream_result_is_preserved() {
        let result = finalize_stream_result(
            String::new(),
            "完整思考内容".to_string(),
            None,
            Vec::new(),
            false,
        )
        .unwrap();

        assert!(result.content.is_empty());
        assert_eq!(result.reasoning.as_deref(), Some("完整思考内容"));
    }

    #[test]
    fn fully_empty_stream_result_is_rejected() {
        let error = finalize_stream_result(String::new(), String::new(), None, Vec::new(), false)
            .unwrap_err();

        let message = format!("{error:#}");
        assert!(message.contains("流式响应为空") || message.contains("stream response was empty"));
    }

    #[test]
    fn sse_buffer_preserves_utf8_split_across_byte_chunks() {
        let line = r#"data: {"choices":[{"delta":{"content":"等","tool_calls":null}}]}"#;
        let split = line.find("等").unwrap() + 1;
        let mut buffer = Utf8LineBuffer::default();

        assert!(buffer.push(&line.as_bytes()[..split]).unwrap().is_empty());
        let lines = buffer.push(&line.as_bytes()[split..]).unwrap();

        assert!(lines.is_empty());
        assert_eq!(buffer.finish().unwrap(), vec![line]);
    }

    #[test]
    fn previous_lossy_chunk_decode_corrupts_split_utf8() {
        let text = "等";
        let mut decoded = String::new();

        decoded.push_str(&String::from_utf8_lossy(&text.as_bytes()[..1]));
        decoded.push_str(&String::from_utf8_lossy(&text.as_bytes()[1..]));

        assert_eq!(decoded, "���");
    }

    #[test]
    fn taotoken_glm_request_enables_thinking() {
        let mut provider = test_provider("taotoken", "https://taotoken.net/api/v1");
        provider.default_model = "glm_for_coding".to_string();

        assert!(taotoken_glm_chat_template_kwargs(&provider)
            .is_some_and(|kwargs| kwargs.enable_thinking));
    }

    #[test]
    fn non_taotoken_glm_request_keeps_default_body() {
        let mut provider = test_provider("local", "http://localhost:11434/v1");
        provider.default_model = "glm-5".to_string();

        assert!(taotoken_glm_chat_template_kwargs(&provider).is_none());
    }

    #[test]
    fn chat_request_includes_stream_usage_options() {
        let request = ChatRequest {
            model: "model".to_string(),
            messages: vec![ChatMessage::plain("user", "hi")],
            temperature: 0.0,
            stream: true,
            stream_options: Some(ChatStreamOptions {
                include_usage: true,
            }),
            max_tokens: None,
            tools: None,
            chat_template_kwargs: None,
            extra_body: None,
        };

        let value = serde_json::to_value(request).unwrap();

        assert_eq!(value["stream_options"]["include_usage"], true);
    }

    #[test]
    fn stream_options_unsupported_detects_retryable_error() {
        assert!(stream_options_unsupported(
            400,
            "unknown parameter: stream_options"
        ));
        assert!(stream_options_unsupported(
            422,
            "stream_options is not supported"
        ));
        assert!(!stream_options_unsupported(403, "stream_options forbidden"));
        assert!(!stream_options_unsupported(400, "invalid api key"));
    }

    #[test]
    fn quota_compatibility_retry_is_narrowly_scoped() {
        assert!(non_stream_quota_fallback_candidate(
            429,
            r#"{"error":{"code":"insufficient_quota"}}"#
        ));
        assert!(!non_stream_quota_fallback_candidate(
            429,
            r#"{"error":{"code":"rate_limit_exceeded"}}"#
        ));
        assert!(!non_stream_quota_fallback_candidate(
            400,
            r#"{"error":{"code":"insufficient_quota"}}"#
        ));
    }

    #[test]
    fn zen_upstream_failed_detects_opencode_zen_compat_error() {
        let provider = test_provider("myopencode", OPENCODE_ZEN_BASE_URL);

        assert!(zen_upstream_failed(
            &provider,
            400,
            r#"{"error":{"message":"Error from provider (Console): Upstream request failed"}}"#,
        ));
        assert!(!zen_upstream_failed(
            &provider,
            401,
            "Upstream request failed"
        ));
        assert!(!zen_upstream_failed(
            &test_provider("other", "https://example.com/v1"),
            400,
            "Upstream request failed"
        ));
    }

    #[test]
    fn openai_gpt5_uses_responses_api() {
        let mut provider = test_provider("openai", "https://api.openai.com/v1");
        provider.default_model = "gpt-5.5".to_string();
        let client = test_client(provider);

        assert!(client.uses_openai_responses());
    }

    #[test]
    fn openai_compatible_gpt5_tries_responses_api() {
        let mut provider = test_provider("taotoken", "https://taotoken.net/api/v1");
        provider.default_model = "gpt-5.5".to_string();
        let client = test_client(provider);

        assert!(client.uses_openai_responses());
    }

    #[test]
    fn auto_protocol_uses_anthropic_for_official_provider() {
        let provider = test_provider("anthropic", "https://api.anthropic.com/v1");
        let client = test_client(provider);

        assert!(client.uses_anthropic_messages());
    }

    #[test]
    fn auto_protocol_keeps_openai_compatible_claude_proxy() {
        let mut provider = test_provider("openrouter", "https://openrouter.ai/api/v1");
        provider.default_model = "anthropic/claude-sonnet-4-5".to_string();
        let client = test_client(provider);

        assert!(!client.uses_anthropic_messages());
    }

    #[test]
    fn responses_unsupported_allows_chat_fallback() {
        assert!(responses_unsupported(404, "not found"));
        assert!(responses_unsupported(400, "unsupported endpoint"));
        assert!(!responses_unsupported(401, "invalid api key"));
    }

    #[test]
    fn openai_tool_schema_flattens_top_level_any_of() {
        let schema = json!({
            "anyOf": [
                {"type":"object","properties":{"path":{"type":"string"}},"required":["path"]},
                {"type":"object","properties":{"resource":{"anyOf":[{"type":"string"},{"type":"null"}]}},"required":["resource"]}
            ]
        });

        let normalized = openai_tool_input_schema(schema);

        assert_eq!(normalized["type"], "object");
        assert_eq!(normalized["additionalProperties"], false);
        assert_eq!(normalized["properties"]["path"]["type"], "string");
        assert_eq!(normalized["properties"]["resource"]["type"], "string");
        assert!(normalized.get("anyOf").is_none());
    }

    #[test]
    fn responses_assistant_history_uses_easy_input_message() {
        let input = lower_responses_messages(vec![ChatMessage::assistant("prior answer", None)]);

        assert_eq!(
            input,
            vec![json!({"role": "assistant", "content": "prior answer"})]
        );
    }

    #[test]
    fn responses_stream_emits_reasoning_and_content() {
        let mut content = String::new();
        let mut content_emitted = 0usize;
        let mut reasoning = String::new();
        let mut reasoning_emitted = 0usize;
        let mut reasoning_part_active = false;
        let mut usage = None;
        let mut content_started = false;
        let mut output_text_delta_parts = HashSet::new();
        let mut refusal_delta_parts = HashSet::new();
        let mut response_id = None;
        let mut tool_calls = ResponsesToolAccumulator::default();
        let mut chunks = Vec::new();
        let mut on_chunk = |chunk| {
            chunks.push(chunk);
            Ok(())
        };

        handle_responses_sse_line(
            r#"data: {"type":"response.reasoning_summary_text.delta","item_id":"rs_1","delta":"思考"}"#,
            &mut content,
            &mut content_emitted,
            &mut reasoning,
            &mut reasoning_emitted,
            &mut reasoning_part_active,
            &mut usage,
            &mut content_started,
            &mut output_text_delta_parts,
            &mut refusal_delta_parts,
            &mut response_id,
            &mut tool_calls,
            &mut on_chunk,
        )
        .unwrap();
        handle_responses_sse_line(
            r#"data: {"type":"response.output_text.delta","item_id":"msg_1","delta":""}"#,
            &mut content,
            &mut content_emitted,
            &mut reasoning,
            &mut reasoning_emitted,
            &mut reasoning_part_active,
            &mut usage,
            &mut content_started,
            &mut output_text_delta_parts,
            &mut refusal_delta_parts,
            &mut response_id,
            &mut tool_calls,
            &mut on_chunk,
        )
        .unwrap();
        handle_responses_sse_line(
            r#"data: {"type":"response.output_text.delta","item_id":"msg_1","delta":"答案"}"#,
            &mut content,
            &mut content_emitted,
            &mut reasoning,
            &mut reasoning_emitted,
            &mut reasoning_part_active,
            &mut usage,
            &mut content_started,
            &mut output_text_delta_parts,
            &mut refusal_delta_parts,
            &mut response_id,
            &mut tool_calls,
            &mut on_chunk,
        )
        .unwrap();

        assert_eq!(chunks.len(), 4);
        assert_eq!(chunks[0].kind, ChatStreamKind::ReasoningPartStart);
        assert_eq!(chunks[1].kind, ChatStreamKind::Reasoning);
        assert_eq!(chunks[1].text, "思考");
        assert_eq!(chunks[2].kind, ChatStreamKind::ReasoningPartEnd);
        assert_eq!(chunks[3].kind, ChatStreamKind::Content);
        assert_eq!(chunks[3].text, "答案");
    }

    #[test]
    fn responses_reasoning_done_emits_content_boundary() {
        let mut content = String::new();
        let mut content_emitted = 0usize;
        let mut reasoning = String::new();
        let mut reasoning_emitted = 0usize;
        let mut reasoning_part_active = false;
        let mut usage = None;
        let mut content_started = false;
        let mut output_text_delta_parts = HashSet::new();
        let mut refusal_delta_parts = HashSet::new();
        let mut response_id = None;
        let mut tool_calls = ResponsesToolAccumulator::default();
        let mut chunks = Vec::new();
        let mut on_chunk = |chunk| {
            chunks.push(chunk);
            Ok(())
        };

        for line in [
            r#"data: {"type":"response.reasoning_summary_text.delta","item_id":"rs_1","delta":"思考"}"#,
            r#"data: {"type":"response.reasoning_summary_text.done","item_id":"rs_1"}"#,
            r#"data: {"type":"response.output_text.delta","item_id":"msg_1","delta":"答案"}"#,
            r#"data: {"type":"response.reasoning_summary_text.delta","item_id":"rs_1","delta":"晚到"}"#,
        ] {
            handle_responses_sse_line(
                line,
                &mut content,
                &mut content_emitted,
                &mut reasoning,
                &mut reasoning_emitted,
                &mut reasoning_part_active,
                &mut usage,
                &mut content_started,
                &mut output_text_delta_parts,
                &mut refusal_delta_parts,
                &mut response_id,
                &mut tool_calls,
                &mut on_chunk,
            )
            .unwrap();
        }

        assert_eq!(chunks.len(), 7);
        assert_eq!(chunks[0].kind, ChatStreamKind::ReasoningPartStart);
        assert_eq!(chunks[1].kind, ChatStreamKind::Reasoning);
        assert_eq!(chunks[1].text, "思考");
        assert_eq!(chunks[2].kind, ChatStreamKind::ReasoningPartEnd);
        assert_eq!(chunks[3].kind, ChatStreamKind::Content);
        assert!(chunks[3].text.is_empty());
        assert_eq!(chunks[4].kind, ChatStreamKind::Content);
        assert_eq!(chunks[4].text, "答案");
        assert_eq!(chunks[5].kind, ChatStreamKind::ReasoningPartStart);
        assert_eq!(chunks[6].kind, ChatStreamKind::Reasoning);
        assert_eq!(chunks[6].text, "\n\n晚到");
        assert_eq!(reasoning, "思考\n\n晚到");
    }

    #[test]
    fn responses_stream_preserves_multiple_reasoning_summary_parts() {
        let mut content = String::new();
        let mut content_emitted = 0usize;
        let mut reasoning = String::new();
        let mut reasoning_emitted = 0usize;
        let mut reasoning_part_active = false;
        let mut usage = None;
        let mut content_started = false;
        let mut output_text_delta_parts = HashSet::new();
        let mut refusal_delta_parts = HashSet::new();
        let mut response_id = None;
        let mut tool_calls = ResponsesToolAccumulator::default();
        let mut chunks = Vec::new();
        let mut on_chunk = |chunk| {
            chunks.push(chunk);
            Ok(())
        };

        for line in [
            r#"data: {"type":"response.reasoning_summary_part.added","item_id":"rs_1","summary_index":0}"#,
            r#"data: {"type":"response.reasoning_summary_text.delta","item_id":"rs_1","summary_index":0,"delta":"**Planning response**"}"#,
            r#"data: {"type":"response.reasoning_summary_part.done","item_id":"rs_1","summary_index":0}"#,
            r#"data: {"type":"response.reasoning_summary_part.added","item_id":"rs_1","summary_index":1}"#,
            r#"data: {"type":"response.reasoning_summary_text.delta","item_id":"rs_1","summary_index":1,"delta":"**Designing helper**"}"#,
            r#"data: {"type":"response.reasoning_summary_part.done","item_id":"rs_1","summary_index":1}"#,
        ] {
            handle_responses_sse_line(
                line,
                &mut content,
                &mut content_emitted,
                &mut reasoning,
                &mut reasoning_emitted,
                &mut reasoning_part_active,
                &mut usage,
                &mut content_started,
                &mut output_text_delta_parts,
                &mut refusal_delta_parts,
                &mut response_id,
                &mut tool_calls,
                &mut on_chunk,
            )
            .unwrap();
        }

        let kinds = chunks.iter().map(|chunk| chunk.kind).collect::<Vec<_>>();
        assert_eq!(
            kinds,
            vec![
                ChatStreamKind::ReasoningPartStart,
                ChatStreamKind::Reasoning,
                ChatStreamKind::ReasoningPartEnd,
                ChatStreamKind::ReasoningPartStart,
                ChatStreamKind::Reasoning,
                ChatStreamKind::ReasoningPartEnd,
            ]
        );
        assert_eq!(reasoning, "**Planning response**\n\n**Designing helper**");
    }

    #[test]
    fn stream_filter_skips_split_system_reminder() {
        let mut content = String::new();
        let mut emitted = 0usize;
        let mut chunks = Vec::new();
        let mut on_chunk = |chunk| {
            chunks.push(chunk);
            Ok(())
        };

        push_buffered_chunk(
            &mut content,
            &mut emitted,
            ChatStreamKind::Content,
            "hello <system-rem".to_string(),
            &mut on_chunk,
        )
        .unwrap();
        push_buffered_chunk(
            &mut content,
            &mut emitted,
            ChatStreamKind::Content,
            "inder>hidden</system-reminder> world".to_string(),
            &mut on_chunk,
        )
        .unwrap();

        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].text, "hello ");
        assert_eq!(chunks[1].text, " world");
    }

    #[test]
    fn stream_filter_skips_underscore_system_reminder() {
        let mut content = String::new();
        let mut emitted = 0usize;
        let mut chunks = Vec::new();
        let mut on_chunk = |chunk| {
            chunks.push(chunk);
            Ok(())
        };

        push_buffered_chunk(
            &mut content,
            &mut emitted,
            ChatStreamKind::Content,
            "a<system_reminder>hidden</system_reminder>b".to_string(),
            &mut on_chunk,
        )
        .unwrap();

        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].text, "a");
        assert_eq!(chunks[1].text, "b");
    }

    #[test]
    fn responses_stream_collects_tool_calls() {
        let mut content = String::new();
        let mut content_emitted = 0usize;
        let mut reasoning = String::new();
        let mut reasoning_emitted = 0usize;
        let mut reasoning_part_active = false;
        let mut usage = None;
        let mut content_started = false;
        let mut output_text_delta_parts = HashSet::new();
        let mut refusal_delta_parts = HashSet::new();
        let mut response_id = None;
        let mut tool_calls = ResponsesToolAccumulator::default();
        let mut on_chunk = |_| Ok(());

        for line in [
            r#"data: {"type":"response.output_item.added","item":{"type":"function_call","id":"item_1","call_id":"call_1","name":"calc","arguments":""}}"#,
            r#"data: {"type":"response.function_call_arguments.delta","item_id":"item_1","delta":"{\"x\":"}"#,
            r#"data: {"type":"response.function_call_arguments.delta","item_id":"item_1","delta":"1}"}"#,
            r#"data: {"type":"response.output_item.done","item":{"type":"function_call","id":"item_1","call_id":"call_1","name":"calc","arguments":"{\"x\":1}"}}"#,
        ] {
            handle_responses_sse_line(
                line,
                &mut content,
                &mut content_emitted,
                &mut reasoning,
                &mut reasoning_emitted,
                &mut reasoning_part_active,
                &mut usage,
                &mut content_started,
                &mut output_text_delta_parts,
                &mut refusal_delta_parts,
                &mut response_id,
                &mut tool_calls,
                &mut on_chunk,
            )
            .unwrap();
        }

        let calls = tool_calls.finish();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "call_1");
        assert_eq!(calls[0].function.name, "calc");
        assert_eq!(calls[0].function.arguments, r#"{"x":1}"#);
    }

    #[test]
    fn responses_stream_announces_question_tool_when_item_starts() {
        let mut content = String::new();
        let mut content_emitted = 0usize;
        let mut reasoning = String::new();
        let mut reasoning_emitted = 0usize;
        let mut reasoning_part_active = false;
        let mut usage = None;
        let mut content_started = false;
        let mut output_text_delta_parts = HashSet::new();
        let mut refusal_delta_parts = HashSet::new();
        let mut response_id = None;
        let mut tool_calls = ResponsesToolAccumulator::default();
        let mut chunks = Vec::new();
        let mut on_chunk = |chunk| {
            chunks.push(chunk);
            Ok(())
        };

        handle_responses_sse_line(
            r#"data: {"type":"response.output_item.added","item":{"type":"function_call","id":"item_1","call_id":"call_1","name":"ask_question","arguments":""}}"#,
            &mut content,
            &mut content_emitted,
            &mut reasoning,
            &mut reasoning_emitted,
            &mut reasoning_part_active,
            &mut usage,
            &mut content_started,
            &mut output_text_delta_parts,
            &mut refusal_delta_parts,
            &mut response_id,
            &mut tool_calls,
            &mut on_chunk,
        )
        .unwrap();

        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].kind, ChatStreamKind::ToolCall);
        assert_eq!(chunks[0].text, "ask_question");
    }

    #[test]
    fn responses_tool_arguments_follow_output_item_ids() {
        let mut tool_calls = ResponsesToolAccumulator::default();
        for (item_id, call_id, name) in [
            ("item_a", "call_a", "first"),
            ("item_b", "call_b", "second"),
        ] {
            tool_calls.start(ResponsesStreamItem {
                kind: "function_call".to_string(),
                id: Some(item_id.to_string()),
                call_id: Some(call_id.to_string()),
                name: Some(name.to_string()),
                arguments: Some(String::new()),
            });
        }

        tool_calls.append_arguments(Some("item_a".to_string()), "{\"a\":".to_string());
        tool_calls.append_arguments(Some("item_b".to_string()), "{\"b\":2}".to_string());
        tool_calls.append_arguments(Some("item_a".to_string()), "1}".to_string());
        tool_calls.append_arguments(Some("unknown".to_string()), "ignored".to_string());

        let calls = tool_calls.finish();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].id, "call_a");
        assert_eq!(calls[0].function.arguments, r#"{"a":1}"#);
        assert_eq!(calls[1].id, "call_b");
        assert_eq!(calls[1].function.arguments, r#"{"b":2}"#);
    }

    #[test]
    fn responses_stream_surfaces_refusal_text() {
        let output = run_responses_test_events(&[
            r#"data: {"type":"response.created","response":{"id":"resp_refusal"}}"#,
            r#"data: {"type":"response.refusal.delta","item_id":"msg_1","delta":"Cannot "}"#,
            r#"data: {"type":"response.refusal.delta","item_id":"msg_1","delta":"help"}"#,
            r#"data: {"type":"response.refusal.done","item_id":"msg_1","refusal":"Cannot help"}"#,
            r#"data: {"type":"response.completed","response":{"id":"resp_refusal"}}"#,
        ])
        .unwrap();

        assert!(output.terminal);
        assert_eq!(output.content, "Cannot help");
        assert_eq!(output.response_id.as_deref(), Some("resp_refusal"));
        assert_eq!(
            output
                .chunks
                .iter()
                .filter(|chunk| chunk.kind == ChatStreamKind::Content)
                .map(|chunk| chunk.text.as_str())
                .collect::<String>(),
            "Cannot help"
        );
    }

    #[test]
    fn responses_stream_accepts_done_only_refusal() {
        let output = run_responses_test_events(&[
            r#"data: {"type":"response.refusal.done","item_id":"msg_1","refusal":"Cannot help"}"#,
            r#"data: {"type":"response.completed","response":{"id":"resp_refusal"}}"#,
        ])
        .unwrap();

        assert_eq!(output.content, "Cannot help");
    }

    #[test]
    fn responses_stream_accepts_done_only_output_text() {
        let output = run_responses_test_events(&[
            r#"data: {"type":"response.output_text.delta","item_id":"msg_1","delta":""}"#,
            r#"data: {"type":"response.output_text.done","item_id":"msg_1","text":"final text"}"#,
            r#"data: {"type":"response.output_text.done","item_id":"msg_2","text":" second"}"#,
            r#"data: {"type":"response.completed","response":{"id":"resp_text"}}"#,
        ])
        .unwrap();

        assert_eq!(output.content, "final text second");
    }

    #[test]
    fn responses_incomplete_is_not_a_successful_terminal_event() {
        let error = run_responses_test_events(&[r#"data: {"type":"response.incomplete","response":{"id":"resp_incomplete","incomplete_details":{"reason":"max_output_tokens"}}}"#])
            .unwrap_err();

        assert!(error.to_string().contains("max_output_tokens"), "{error:#}");
    }

    #[test]
    fn responses_tool_calls_require_stateful_continuation() {
        let tool_call = ToolCall {
            id: "call_1".to_string(),
            kind: "function".to_string(),
            function: ToolCallFunction {
                name: "calc".to_string(),
                arguments: "{}".to_string(),
            },
        };

        let store_error = finalize_responses_stream_result(
            String::new(),
            String::new(),
            None,
            vec![tool_call.clone()],
            false,
            Some("resp_1".to_string()),
            true,
            false,
        )
        .unwrap_err();
        assert!(store_error.to_string().contains("store=false"));

        let id_error = finalize_responses_stream_result(
            String::new(),
            String::new(),
            None,
            vec![tool_call.clone()],
            false,
            None,
            false,
            false,
        )
        .unwrap_err();
        assert!(id_error.to_string().contains("without a response ID"));

        // 续传被记为不可用:带工具调用也不设 continuation(无状态全量回放),
        // 且不再要求 response_id。
        let suppressed = finalize_responses_stream_result(
            String::new(),
            String::new(),
            None,
            vec![tool_call],
            false,
            None,
            false,
            true,
        )
        .unwrap();
        assert!(suppressed.responses_continuation.is_none());
    }

    #[tokio::test]
    async fn responses_store_false_rejects_tools_before_sending() {
        let mut provider = test_provider("responses-store-test", "http://127.0.0.1:1/v1");
        provider.protocol = "openai-responses".to_string();
        provider.default_model = "gpt-5".to_string();
        provider.extra_body = json!({"store": false}).as_object().cloned();
        let client = test_client(provider);
        let tools = vec![ToolDefinition {
            kind: "function",
            function: crate::llm::FunctionDefinition {
                name: "calc".to_string(),
                description: "calculate".to_string(),
                parameters: json!({"type": "object", "properties": {}}),
            },
        }];

        let error = client
            .chat_responses_stream(
                vec![ChatMessage::plain("user", "hi")],
                tools,
                None,
                "request-test",
                &mut |_| Ok(()),
            )
            .await
            .unwrap_err();

        assert!(error.to_string().contains("remove store=false"));
    }

    #[test]
    fn protocol_config_accepts_explicit_anthropic() {
        let mut provider = test_provider("anthropic", "https://api.anthropic.com/v1");
        provider.protocol = "anthropic".to_string();

        assert_eq!(
            ProviderProtocol::from_provider(&provider).unwrap(),
            ProviderProtocol::Anthropic
        );
    }

    #[test]
    fn protocol_config_accepts_anthropic_aliases() {
        let mut provider = test_provider("anthropic", "https://api.anthropic.com/v1");

        for protocol in ["anthropic-messages", "claude", "claude-messages"] {
            provider.protocol = protocol.to_string();
            assert_eq!(
                ProviderProtocol::from_provider(&provider).unwrap(),
                ProviderProtocol::Anthropic
            );
        }
    }

    #[test]
    fn anthropic_lowering_keeps_remote_image_urls() {
        let content = lower_anthropic_user_content(Some(ChatContent::Parts(vec![
            ChatContentPart::ImageUrl {
                image_url: ImageUrlContent {
                    url: "https://example.com/image.png".to_string(),
                },
            },
            ChatContentPart::Text {
                text: "describe".to_string(),
            },
        ])));
        let json = serde_json::to_value(content).unwrap();

        assert_eq!(json[0]["type"], "image");
        assert_eq!(json[0]["source"]["type"], "url");
        assert_eq!(json[0]["source"]["url"], "https://example.com/image.png");
        assert_eq!(json[1]["text"], "describe");
    }

    #[test]
    fn anthropic_stream_waits_for_message_stop() {
        let mut state = AnthropicStreamState::default();
        let mut on_chunk = |_| Ok(());

        let done = handle_anthropic_sse_data(
            r#"{"type":"message_delta","usage":{"input_tokens":3,"output_tokens":2},"delta":{"stop_reason":"end_turn"}}"#,
            &mut state,
            &mut on_chunk,
        )
        .unwrap();
        assert!(!done);

        let done =
            handle_anthropic_sse_data(r#"{"type":"message_stop"}"#, &mut state, &mut on_chunk)
                .unwrap();
        assert!(done);
    }

    #[test]
    fn official_anthropic_template_sets_messages_protocol() {
        let provider = ProviderConfig::default_anthropic();

        assert_eq!(provider.id, "anthropic");
        assert_eq!(provider.protocol, "anthropic");
        assert_eq!(provider.base_url, "https://api.anthropic.com/v1");
        assert_eq!(provider.api_key.as_deref(), Some("$env:ANTHROPIC_API_KEY"));
        assert!(provider.models.is_empty());
        assert!(provider.default_model.is_empty());
    }

    #[test]
    fn anthropic_request_enables_adaptive_summarized_thinking_by_default() {
        let mut provider = test_provider("anthropic", "https://api.anthropic.com/v1");
        provider.default_model = "claude-sonnet-4-5".to_string();
        let client = test_client(provider);

        let request =
            client.anthropic_request(vec![ChatMessage::plain("user", "hi")], Vec::new(), true);
        let json = serde_json::to_value(request).unwrap();

        assert_eq!(json["thinking"]["type"], "adaptive");
        assert_eq!(json["thinking"]["display"], "summarized");
    }

    #[test]
    fn anthropic_request_can_disable_thinking_for_fallback() {
        let mut provider = test_provider("anthropic", "https://api.anthropic.com/v1");
        provider.default_model = "claude-sonnet-4-5".to_string();
        let client = test_client(provider);

        let request =
            client.anthropic_request(vec![ChatMessage::plain("user", "hi")], Vec::new(), false);
        let json = serde_json::to_value(request).unwrap();

        assert!(json.get("thinking").is_none());
    }

    #[test]
    fn anthropic_thinking_unsupported_detects_retryable_errors() {
        assert!(anthropic_thinking_unsupported(
            400,
            "invalid request: thinking is not supported by this model"
        ));
        assert!(anthropic_thinking_unsupported(
            422,
            "unknown parameter: thinking"
        ));
        assert!(!anthropic_thinking_unsupported(401, "invalid api key"));
        assert!(!anthropic_thinking_unsupported(
            400,
            "max_tokens is too low"
        ));
    }
}
