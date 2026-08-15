//! tests3 — 自 src/llm/openai_compatible.rs 外移。
#![cfg(test)]

use super::*;
use std::net::TcpListener;
use std::sync::atomic::AtomicUsize;

#[test]
fn typed_failures_drive_endpoint_cooldowns() {
    let rate_limit =
        anyhow::anyhow!("provider body").context(HttpStatusFailure::classify(429, "provider body"));
    let quota = anyhow::anyhow!("provider body")
        .context(HttpStatusFailure::classify(400, "quota exceeded"));
    let invalid_key = anyhow::anyhow!("provider body")
        .context(HttpStatusFailure::classify(400, "invalid api key"));
    let transport = anyhow::anyhow!("socket source").context(TransportFailure {
        stage: "chat.send",
        kind: TransportFailureKind::Connect,
    });
    let protocol = anyhow::anyhow!("invalid response shape");

    assert_eq!(
        cooldown_for_error(&rate_limit),
        Some(Duration::from_secs(600))
    );
    assert_eq!(cooldown_for_error(&quota), Some(Duration::from_secs(600)));
    assert_eq!(
        cooldown_for_error(&invalid_key),
        Some(Duration::from_secs(600))
    );
    assert_eq!(
        cooldown_for_error(&transport),
        Some(Duration::from_secs(120))
    );
    assert_eq!(cooldown_for_error(&protocol), None);
}

#[test]
fn structured_provider_errors_drive_failure_semantics() {
    let rate_limit = HttpStatusFailure::classify(
        400,
        r#"{"error":{"type":"rate_limit_error","code":"rate_limit_exceeded"}}"#,
    );
    let invalid_key = HttpStatusFailure::classify(
        400,
        r#"{"error":{"type":"authentication_error","code":"invalid_api_key"}}"#,
    );
    let unavailable_model = HttpStatusFailure::classify(
        400,
        r#"{"error":{"type":"invalid_request_error","code":"model_not_available"}}"#,
    );
    let incompatible_endpoint = HttpStatusFailure::classify(
        400,
        r#"{"error":{"type":"invalid_request_error","message":"Unknown parameter: tools"}}"#,
    );
    let invalid_request = HttpStatusFailure::classify(
        400,
        r#"{"error":{"type":"invalid_request_error","message":"Malformed request body"}}"#,
    );
    let google_invalid_request = HttpStatusFailure::classify(
        400,
        r#"{"error":{"status":"InvalidArgument","message":"request rejected"}}"#,
    );
    let azure_missing_deployment = HttpStatusFailure::classify(
        400,
        r#"{"error":{"code":"DeploymentNotFound","message":"missing"}}"#,
    );
    let unknown = HttpStatusFailure::classify(400, r#"{"error":{"message":"failed"}}"#);

    assert_eq!(rate_limit.kind, HttpFailureKind::RateLimit);
    assert_eq!(invalid_key.kind, HttpFailureKind::Authentication);
    assert_eq!(unavailable_model.kind, HttpFailureKind::EndpointUnavailable);
    assert_eq!(
        incompatible_endpoint.kind,
        HttpFailureKind::EndpointIncompatible
    );
    assert_eq!(invalid_request.kind, HttpFailureKind::InvalidRequest);
    assert_eq!(google_invalid_request.kind, HttpFailureKind::InvalidRequest);
    assert_eq!(
        azure_missing_deployment.kind,
        HttpFailureKind::EndpointUnavailable
    );
    assert_eq!(unknown.kind, HttpFailureKind::Status);

    assert!(endpoint_failover_allowed(&anyhow::Error::new(
        incompatible_endpoint
    )));
    let invalid_request = anyhow::Error::new(invalid_request);
    assert_eq!(cooldown_for_error(&invalid_request), None);
    assert!(!endpoint_failover_allowed(&invalid_request));
    assert!(endpoint_failover_allowed(&anyhow::Error::new(unknown)));
}

#[test]
fn scheduler_skips_cooling_endpoints_and_reports_an_exhausted_pool() {
    let first = test_client(test_provider(
        "scheduler-first",
        "http://example.invalid/v1",
    ));
    let second = test_client(test_provider(
        "scheduler-second",
        "http://example.invalid/v1",
    ));
    let endpoints = vec![first.endpoints[0].clone(), second.endpoints[0].clone()];
    let mut scheduler = LlmScheduler::default();

    scheduler.mark_failure(endpoints[0].id(), Duration::from_secs(60));
    assert_eq!(scheduler.ordered_indices(&endpoints), vec![1]);

    scheduler.mark_failure(endpoints[1].id(), Duration::from_secs(60));
    assert!(scheduler.ordered_indices(&endpoints).is_empty());

    scheduler.mark_success(&endpoints[0].id());
    assert_eq!(scheduler.ordered_indices(&endpoints), vec![0]);
}

#[tokio::test]
async fn invalid_request_does_not_fail_over_to_another_endpoint() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        read_http_headers(&mut stream).await;
        let body =
            r#"{"error":{"type":"invalid_request_error","message":"Malformed request body"}}"#;
        let response = format!(
            "HTTP/1.1 400 Bad Request\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(response.as_bytes()).await.unwrap();
        tokio::time::timeout(Duration::from_millis(100), listener.accept())
            .await
            .is_err()
    });

    let mut first = test_provider("invalid-request-first", &url);
    first.protocol = "openai-chat".to_string();
    let mut second = test_provider("invalid-request-second", &url);
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

    let error = client
        .chat_stream(vec![ChatMessage::plain("user", "hi")], Vec::new(), |_| {
            Ok(())
        })
        .await
        .unwrap_err();

    assert!(format!("{error:#}").contains("endpoint failover was suppressed"));
    assert!(
        server.await.unwrap(),
        "a second endpoint received the request"
    );
}

#[test]
fn only_connect_failures_are_retried() {
    assert!(retryable_transport_failure(TransportFailureKind::Connect));
    assert!(!retryable_transport_failure(TransportFailureKind::Timeout));
    assert!(!retryable_transport_failure(TransportFailureKind::Other));
    assert!(retryable_http_status(500));
    assert!(retryable_http_status(599));
    assert!(!retryable_http_status(429));
    assert!(!retryable_http_status(400));
}

#[test]
fn http_status_retry_delay_caps_at_configured_maximum() {
    assert_eq!(http_status_retry_delay(1), Duration::from_millis(10));
    assert_eq!(http_status_retry_delay(2), Duration::from_millis(20));
    assert_eq!(http_status_retry_delay(3), Duration::from_millis(40));
    assert_eq!(http_status_retry_delay(4), Duration::from_millis(80));
    assert_eq!(http_status_retry_delay(5), Duration::from_millis(120));
    assert_eq!(
        http_status_retry_delay(usize::MAX),
        Duration::from_millis(120)
    );
}

#[test]
fn endpoint_failover_stops_after_irreversible_stream_output() {
    let reasoning = ChatStreamChunk {
        kind: ChatStreamKind::Reasoning,
        text: "partial".to_string(),
    };
    assert!(!stream_chunk_commits_attempt(
        &reasoning,
        ReasoningVisibility::Hidden
    ));
    assert!(!stream_chunk_commits_attempt(
        &reasoning,
        ReasoningVisibility::Summary
    ));
    assert!(stream_chunk_commits_attempt(
        &reasoning,
        ReasoningVisibility::Full
    ));
    assert!(!stream_chunk_commits_attempt(
        &ChatStreamChunk {
            kind: ChatStreamKind::Content,
            text: String::new(),
        },
        ReasoningVisibility::Full,
    ));
    let reasoning_end = ChatStreamChunk {
        kind: ChatStreamKind::ReasoningPartEnd,
        text: String::new(),
    };
    assert!(!stream_chunk_commits_attempt(
        &reasoning_end,
        ReasoningVisibility::Hidden
    ));
    assert!(stream_chunk_commits_attempt(
        &reasoning_end,
        ReasoningVisibility::Summary
    ));
    for chunk in [
        ChatStreamChunk {
            kind: ChatStreamKind::Content,
            text: "answer".to_string(),
        },
        ChatStreamChunk {
            kind: ChatStreamKind::ToolCall,
            text: "ask_question".to_string(),
        },
    ] {
        assert!(stream_chunk_commits_attempt(
            &chunk,
            ReasoningVisibility::Hidden
        ));
    }
}

#[test]
fn reasoning_failover_visibility_only_follows_reasoning_display() {
    let mut config = AppConfig::default();
    assert_eq!(reasoning_visibility(&config), ReasoningVisibility::Summary);

    config.display.reasoning = " full ".to_string();
    assert_eq!(reasoning_visibility(&config), ReasoningVisibility::Full);

    config.display.reasoning = "hidden".to_string();
    config.display.tool_calls = "FULL".to_string();
    assert_eq!(reasoning_visibility(&config), ReasoningVisibility::Hidden);
}

#[test]
fn responses_summary_uses_auto_and_full_uses_detailed() {
    let mut config = AppConfig::default();
    assert!(!reasoning_summary_is_detailed(&config));

    config.display.reasoning = " FULL ".to_string();
    assert!(reasoning_summary_is_detailed(&config));

    let provider = test_provider("openai", "https://api.openai.com/v1");
    let mut client = test_client(provider);
    let reasoning = client.responses_reasoning().unwrap();
    assert_eq!(reasoning.summary.as_deref(), Some("auto"));

    client.detailed_reasoning_summary = true;
    let reasoning = client.responses_reasoning().unwrap();
    assert_eq!(reasoning.summary.as_deref(), Some("detailed"));
}

#[test]
fn subagent_output_visibility_follows_tool_detail_mode() {
    let provider = test_provider("openai", "https://api.openai.com/v1");
    let hidden = test_client(provider.clone()).for_subagent_output(false);
    assert_eq!(hidden.reasoning_visibility, ReasoningVisibility::Hidden);
    assert!(!hidden.detailed_reasoning_summary);

    let full = test_client(provider).for_subagent_output(true);
    assert_eq!(full.reasoning_visibility, ReasoningVisibility::Full);
    assert!(full.detailed_reasoning_summary);
}

pub(crate) async fn read_http_headers(stream: &mut tokio::net::TcpStream) {
    let mut request = Vec::new();
    let mut byte = [0u8; 1];
    while !request.ends_with(b"\r\n\r\n") {
        let read = stream.read(&mut byte).await.unwrap();
        assert_ne!(read, 0, "connection closed before request headers");
        request.push(byte[0]);
    }
}

pub(crate) async fn write_http_sse_response(stream: &mut tokio::net::TcpStream, body: &str) {
    let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
    stream.write_all(response.as_bytes()).await.unwrap();
}

/// Serves an endless stream of opencode-zen-shaped 429s, counting hits.
/// The listener is bound before the task is spawned: `#[tokio::test]` runs
/// a current-thread runtime, so handing the address back over a blocking
/// channel would deadlock the only thread that could serve it.
async fn spawn_rate_limited_endpoint() -> (String, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();
    let server = tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            read_http_headers(&mut stream).await;
            counter.fetch_add(1, Ordering::SeqCst);
            let body = concat!(
                r#"{"type":"error","error":{"type":"FreeUsageLimitError","#,
                r#""message":"Error from provider (Console): Rate limit exceeded."}}"#
            );
            stream
                    .write_all(
                        format!(
                            "HTTP/1.1 429 Too Many Requests\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
        }
    });
    (url, hits, server)
}

fn rate_limit_test_endpoint(id: &str, url: &str) -> LlmEndpoint {
    let mut provider = test_provider(id, url);
    provider.protocol = "openai-chat".to_string();
    provider.default_model = "big-pickle".to_string();
    LlmEndpoint {
        client: reqwest::Client::new(),
        provider,
        api_key: "public".to_string(),
        key_index: 0,
    }
}

fn client_over(endpoints: Vec<LlmEndpoint>) -> OpenAiCompatibleClient {
    let first = endpoints[0].clone();
    OpenAiCompatibleClient {
        client: first.client.clone(),
        provider: first.provider.clone(),
        api_key: first.api_key.clone(),
        endpoints: Arc::new(endpoints),
        thinking_variants: HashMap::new(),
        reasoning_visibility: ReasoningVisibility::Summary,
        buffered_delivery: false,
        detailed_reasoning_summary: false,
        request_timeouts: None,
        max_tokens_override: None,
        request_scope: "chat",
        continuation_health: ResponsesContinuationHealth::detached(),
    }
}

#[tokio::test]
async fn a_rate_limited_endpoint_costs_one_request_per_turn_not_three() {
    // Regression: `MIN_ENDPOINT_ATTEMPTS` padded the attempt list by
    // cycling the only endpoint, so a single 429 fired three back-to-back
    // requests with no backoff — and the 600s cooldown then refilled the
    // whole pool, repeating the triple every turn for the entire cooldown.
    let (url, hits, server) = spawn_rate_limited_endpoint().await;
    let client = client_over(vec![rate_limit_test_endpoint(
        "rate-limit-single-endpoint-test",
        &url,
    )]);

    for turn in 1..=3 {
        let before = hits.load(Ordering::SeqCst);
        let error = client
            .chat_stream(vec![ChatMessage::plain("user", "hi")], Vec::new(), |_| {
                Ok(())
            })
            .await
            .unwrap_err();
        assert!(
            format!("{error:#}").contains("429"),
            "turn {turn} did not surface the rate limit: {error:#}"
        );
        assert_eq!(
            hits.load(Ordering::SeqCst) - before,
            1,
            "turn {turn} spent more than one request on a rate-limited endpoint"
        );
    }

    server.abort();
}

#[tokio::test]
async fn a_rate_limited_endpoint_still_fails_over_to_a_different_one() {
    // The same-endpoint suppression must not cost cross-endpoint failover:
    // each distinct endpoint is still tried exactly once.
    let (first_url, first_hits, first_server) = spawn_rate_limited_endpoint().await;
    let (second_url, second_hits, second_server) = spawn_rate_limited_endpoint().await;
    let client = client_over(vec![
        rate_limit_test_endpoint("rate-limit-failover-first-test", &first_url),
        rate_limit_test_endpoint("rate-limit-failover-second-test", &second_url),
    ]);

    let error = client
        .chat_stream(vec![ChatMessage::plain("user", "hi")], Vec::new(), |_| {
            Ok(())
        })
        .await
        .unwrap_err();

    assert_eq!(first_hits.load(Ordering::SeqCst), 1);
    assert_eq!(second_hits.load(Ordering::SeqCst), 1);
    let rendered = format!("{error:#}");
    for id in [
        "rate-limit-failover-first-test",
        "rate-limit-failover-second-test",
    ] {
        assert!(
            rendered.contains(id),
            "{id} missing from the failure report: {rendered}"
        );
    }

    first_server.abort();
    second_server.abort();
}

pub(crate) fn test_client(provider: ProviderConfig) -> OpenAiCompatibleClient {
    let client = reqwest::Client::new();
    let endpoint = LlmEndpoint {
        client: client.clone(),
        provider: provider.clone(),
        api_key: "test".to_string(),
        key_index: 0,
    };
    OpenAiCompatibleClient {
        client,
        provider,
        api_key: "test".to_string(),
        endpoints: Arc::new(vec![endpoint]),
        thinking_variants: HashMap::new(),
        reasoning_visibility: ReasoningVisibility::Summary,
        buffered_delivery: false,
        detailed_reasoning_summary: false,
        request_timeouts: None,
        max_tokens_override: None,
        request_scope: "chat",
        continuation_health: ResponsesContinuationHealth::detached(),
    }
}

fn test_paths(root: &std::path::Path) -> GQYPaths {
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
        system_scripts_dir: root.join("system/scripts"),
    }
}

pub(crate) fn test_provider(id: &str, base_url: &str) -> ProviderConfig {
    ProviderConfig {
        id: id.to_string(),
        display_name: id.to_string(),
        base_url: base_url.to_string(),
        protocol: "auto".to_string(),
        api_key: None,
        models: Vec::new(),
        model_context_window: std::collections::HashMap::new(),
        model_temperature: std::collections::HashMap::new(),
        model_modalities: std::collections::HashMap::new(),
        model_costs: std::collections::HashMap::new(),
        default_model: String::new(),
        timeout_seconds: 60,
        temperature: 1.0,
        anthropic_max_tokens: 4096,
        extra_body: None,
    }
}

#[test]
fn client_constructors_restore_saved_thinking_variants() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    std::fs::create_dir_all(&paths.state_dir).unwrap();

    let mut provider = test_provider("custom", "https://example.com/v1");
    provider.default_model = "reasoning-model".to_string();
    provider.models = vec![provider.default_model.clone()];
    provider.api_key = Some("test-key".to_string());
    let preferences = ThinkingVariantPreferences {
        selected: HashMap::from([(
            thinking_variant_key(&provider.id, &provider.default_model),
            "high".to_string(),
        )]),
        ..ThinkingVariantPreferences::default()
    };
    std::fs::write(
        thinking_variant_preferences_file(&paths),
        serde_json::to_string(&preferences).unwrap(),
    )
    .unwrap();

    let config = AppConfig {
        active_provider: provider.id.clone(),
        active_provider_models: None,
        providers: vec![provider.clone()],
        ..AppConfig::default()
    };

    let configured = OpenAiCompatibleClient::from_config(&config, &paths).unwrap();
    assert_eq!(configured.selected_thinking_variant_id(), Some("high"));

    let direct = OpenAiCompatibleClient::new(&provider, &config, &paths).unwrap();
    assert_eq!(direct.selected_thinking_variant_id(), Some("high"));
}

#[test]
fn saving_thinking_variants_preserves_inactive_models_and_clears_unset_active_model() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    std::fs::create_dir_all(&paths.state_dir).unwrap();
    let inactive_key = thinking_variant_key("inactive", "old-model");
    let active_key = thinking_variant_key("custom", "reasoning-model");
    let preferences = ThinkingVariantPreferences {
        selected: HashMap::from([(inactive_key.clone(), "max".to_string())]),
        ..ThinkingVariantPreferences::default()
    };
    std::fs::write(
        thinking_variant_preferences_file(&paths),
        serde_json::to_string(&preferences).unwrap(),
    )
    .unwrap();

    let mut provider = test_provider("custom", "https://example.com/v1");
    provider.default_model = "reasoning-model".to_string();
    let mut client = test_client(provider);
    client
        .thinking_variants
        .insert(active_key.clone(), "high".to_string());
    client.save_thinking_variants(&paths).unwrap();

    let saved = load_thinking_variant_preferences(&paths);
    assert_eq!(
        saved.selected.get(&inactive_key).map(String::as_str),
        Some("max")
    );
    assert_eq!(
        saved.selected.get(&active_key).map(String::as_str),
        Some("high")
    );

    client.thinking_variants.remove(&active_key);
    client.save_thinking_variants(&paths).unwrap();
    let saved = load_thinking_variant_preferences(&paths);
    assert_eq!(
        saved.selected.get(&inactive_key).map(String::as_str),
        Some("max")
    );
    assert!(!saved.selected.contains_key(&active_key));
}

#[test]
fn staged_thinking_variant_update_merges_only_the_edited_inactive_model() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let mut staged = ThinkingVariantPreferences::load(&paths);
    staged.set("future-provider", "future-model", Some("high".to_string()));

    std::fs::create_dir_all(&paths.state_dir).unwrap();
    let concurrent_key = thinking_variant_key("other-provider", "other-model");
    let concurrent = ThinkingVariantPreferences {
        selected: HashMap::from([(concurrent_key.clone(), "max".to_string())]),
        ..ThinkingVariantPreferences::default()
    };
    std::fs::write(
        thinking_variant_preferences_file(&paths),
        serde_json::to_string(&concurrent).unwrap(),
    )
    .unwrap();

    staged.save(&paths).unwrap();

    let saved = ThinkingVariantPreferences::load(&paths);
    assert_eq!(
        saved.selected("future-provider", "future-model"),
        Some("high")
    );
    assert_eq!(
        saved.selected.get(&concurrent_key).map(String::as_str),
        Some("max")
    );
}

#[test]
fn malformed_thinking_variant_state_is_not_overwritten() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    std::fs::create_dir_all(&paths.state_dir).unwrap();
    let path = thinking_variant_preferences_file(&paths);
    std::fs::write(&path, "{not-json").unwrap();
    let mut preferences = ThinkingVariantPreferences::load(&paths);
    preferences.set("provider", "model", Some("high".to_string()));

    let error = preferences.save(&paths).unwrap_err();

    assert!(format!("{error:#}").contains("failed to parse thinking variant state"));
    assert_eq!(std::fs::read_to_string(path).unwrap(), "{not-json");
}

#[test]
fn thinking_variant_preferences_follow_provider_renames() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    std::fs::create_dir_all(&paths.state_dir).unwrap();
    let preferences = ThinkingVariantPreferences {
        selected: HashMap::from([
            (thinking_variant_key("old", "first"), "high".to_string()),
            (thinking_variant_key("old", "second"), "max".to_string()),
            (thinking_variant_key("other", "first"), "low".to_string()),
        ]),
        ..ThinkingVariantPreferences::default()
    };
    std::fs::write(
        thinking_variant_preferences_file(&paths),
        serde_json::to_string(&preferences).unwrap(),
    )
    .unwrap();
    let mut preferences = ThinkingVariantPreferences::load(&paths);

    preferences.set("old", "second", Some("low".to_string()));
    preferences.rename_provider("old", "new");
    let mut concurrent = ThinkingVariantPreferences::load(&paths);
    concurrent.set("old", "first", Some("medium".to_string()));
    concurrent.set("old", "second", Some("high".to_string()));
    concurrent.set("old", "late", Some("medium".to_string()));
    concurrent.save(&paths).unwrap();
    preferences.save(&paths).unwrap();

    let saved = ThinkingVariantPreferences::load(&paths);
    assert_eq!(saved.selected("new", "first"), Some("medium"));
    assert_eq!(saved.selected("new", "second"), Some("low"));
    assert_eq!(saved.selected("new", "late"), Some("medium"));
    assert_eq!(saved.selected("other", "first"), Some("low"));
    assert_eq!(saved.selected("old", "first"), None);
    assert_eq!(saved.selected("old", "second"), None);
    assert_eq!(saved.selected("old", "late"), None);
}

#[test]
fn provider_rename_replays_when_the_initial_variant_snapshot_was_empty() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let mut renaming = ThinkingVariantPreferences::load(&paths);
    renaming.rename_provider("old", "new");

    let mut concurrent = ThinkingVariantPreferences::load(&paths);
    concurrent.set("old", "late", Some("high".to_string()));
    concurrent.save(&paths).unwrap();
    renaming.save(&paths).unwrap();

    let saved = ThinkingVariantPreferences::load(&paths);
    assert_eq!(saved.selected("new", "late"), Some("high"));
    assert_eq!(saved.selected("old", "late"), None);
}

#[test]
fn concurrent_thinking_variant_updates_keep_distinct_models() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let handles = ["first", "second"].map(|model| {
        let paths = paths.clone();
        let barrier = barrier.clone();
        std::thread::spawn(move || {
            let mut preferences = ThinkingVariantPreferences::load(&paths);
            preferences.set("provider", model, Some("high".to_string()));
            barrier.wait();
            preferences.save(&paths).unwrap();
        })
    });
    for handle in handles {
        handle.join().unwrap();
    }

    let saved = ThinkingVariantPreferences::load(&paths);
    assert_eq!(saved.selected("provider", "first"), Some("high"));
    assert_eq!(saved.selected("provider", "second"), Some("high"));
}

#[test]
fn reasoning_variants_use_current_wire_protocol_mapping() {
    let info = ModelReasoningInfo {
        provider_npm: Some("@openrouter/ai-sdk-provider".to_string()),
        variants: Vec::new(),
    };
    let effort = ReasoningVariant {
        id: "high".to_string(),
        setting: ReasoningSetting::Effort("high".to_string()),
    };
    let budget = ReasoningVariant {
        id: "max".to_string(),
        setting: ReasoningSetting::BudgetTokens(8000),
    };
    let provider = test_provider("openrouter", "https://openrouter.ai/api/v1");
    assert!(reasoning_variant_supported(
        &provider,
        "test-model",
        &info,
        &effort
    ));
    assert!(reasoning_variant_supported(
        &provider,
        "test-model",
        &info,
        &budget
    ));

    let unknown_info = ModelReasoningInfo {
        provider_npm: Some("@unknown/provider".to_string()),
        variants: Vec::new(),
    };
    let unknown = test_provider("proxy", "https://proxy.example/v1");
    assert!(reasoning_variant_supported(
        &unknown,
        "test-model",
        &unknown_info,
        &effort
    ));
    assert!(!reasoning_variant_supported(
        &unknown,
        "test-model",
        &unknown_info,
        &budget
    ));

    let alibaba = test_provider("alibaba-token-plan", "https://example.com/v1");
    let toggle = ReasoningVariant {
        id: "on".to_string(),
        setting: ReasoningSetting::Toggle(true),
    };
    assert!(reasoning_variant_supported(
        &alibaba,
        "test-model",
        &unknown_info,
        &toggle
    ));

    assert!(reasoning_variant_supported(
        &unknown,
        "gpt-5-mini",
        &unknown_info,
        &toggle
    ));
    assert!(!reasoning_variant_supported(
        &unknown,
        "gpt-4.1",
        &unknown_info,
        &toggle
    ));
    assert!(!reasoning_variant_supported_for_protocol(
        &unknown,
        &unknown_info,
        &toggle,
        ProviderProtocol::OpenAiChat
    ));
}

#[test]
fn anthropic_budget_is_bounded_by_max_tokens() {
    assert_eq!(anthropic_reasoning_budget(4096, 2048), Some(2048));
    assert_eq!(anthropic_reasoning_budget(4096, 32_000), None);
    assert_eq!(anthropic_reasoning_budget(1024, 32_000), None);
}

#[test]
fn custom_openai_compatible_provider_uses_reasoning_effort() {
    let mut provider = test_provider("ririxin", "https://token.sensenova.cn/v1");
    provider.default_model = "deepseek-v4-flash".to_string();
    let info = ModelReasoningInfo {
        provider_npm: Some("@ai-sdk/openai-compatible".to_string()),
        variants: Vec::new(),
    };

    let body = chat_variant_body(
        &provider,
        &info,
        ReasoningSetting::Effort("high".to_string()),
    )
    .unwrap();
    assert_eq!(body["reasoning_effort"], "high");
    assert!(body.get("reasoning").is_none());
}

#[test]
fn mixed_client_keeps_variants_per_provider_and_model() {
    let mut first = test_provider("ririxin", "https://token.sensenova.cn/v1");
    first.default_model = "deepseek-v4-flash".to_string();
    let mut second = test_provider("opencode", "https://opencode.ai/zen/v1");
    second.default_model = "mimo-v2.5-free".to_string();
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
    let mut client = OpenAiCompatibleClient {
        client: first_client,
        provider: first,
        api_key: "first".to_string(),
        endpoints: Arc::new(endpoints),
        thinking_variants: HashMap::from([(
            thinking_variant_key("ririxin", "deepseek-v4-flash"),
            "high".to_string(),
        )]),
        reasoning_visibility: ReasoningVisibility::Summary,
        buffered_delivery: false,
        detailed_reasoning_summary: false,
        request_timeouts: None,
        max_tokens_override: None,
        request_scope: "chat",
        continuation_health: ResponsesContinuationHealth::detached(),
    };

    let first_endpoint = client.with_endpoint(&client.endpoints[0]);
    let second_endpoint = client.with_endpoint(&client.endpoints[1]);
    assert_eq!(first_endpoint.selected_thinking_variant_id(), Some("high"));
    assert_eq!(second_endpoint.selected_thinking_variant_id(), None);
    client.thinking_variants.insert(
        thinking_variant_key("opencode", "mimo-v2.5-free"),
        "max".to_string(),
    );
    let second_endpoint = client.with_endpoint(&client.endpoints[1]);
    assert_eq!(second_endpoint.selected_thinking_variant_id(), Some("max"));
    assert_eq!(first_endpoint.selected_thinking_variant_id(), Some("high"));
}

#[test]
fn variant_extra_body_merges_nested_reasoning_fields() {
    let base = json!({ "reasoning": { "exclude": true }, "custom": 1 })
        .as_object()
        .cloned();
    let variant = json!({ "reasoning": { "effort": "high" } })
        .as_object()
        .cloned();

    let merged = merge_extra_body(base, variant).unwrap();
    assert_eq!(merged["reasoning"]["exclude"], true);
    assert_eq!(merged["reasoning"]["effort"], "high");
    assert_eq!(merged["custom"], 1);
}

#[test]
fn test_chat_request_extra_body_flatten() {
    use serde_json::json;

    let extra = json!({
        "model": "override",
        "messages": [],
        "enable_thinking": false,
        "custom_param": "value"
    })
    .as_object()
    .cloned();

    let request = ChatRequest {
        model: "gpt-4".to_string(),
        messages: vec![ChatMessage::plain("user", "Hello")],
        temperature: 0.7,
        stream: true,
        stream_options: Some(ChatStreamOptions {
            include_usage: true,
        }),
        max_tokens: None,
        tools: None,
        chat_template_kwargs: None,
        extra_body: sanitize_extra_body(extra, CHAT_RESERVED_BODY_KEYS),
    };

    let serialized = serde_json::to_string(&request).unwrap();
    let value = serde_json::to_value(&request).unwrap();

    assert_eq!(value["enable_thinking"], false);
    assert_eq!(value["custom_param"], "value");
    assert_eq!(value["model"], "gpt-4");
    let temp = value["temperature"].as_f64().unwrap();
    assert!((temp - 0.7).abs() < 1e-6);
    assert!(value.get("extra_body").is_none());
    assert_eq!(serialized.matches("\"model\":").count(), 1);
    assert_eq!(serialized.matches("\"messages\":").count(), 1);
}

#[test]
fn test_responses_request_extra_body_flatten() {
    use serde_json::json;

    let extra = json!({
        "input": [],
        "previous_response_id": "wrong",
        "reasoning": {"effort": "high"},
        "reasoning_effort": "high",
        "parallel_tool_calls": false
    })
    .as_object()
    .cloned();

    let request = ResponsesRequest {
        model: "gpt-5".to_string(),
        input: vec![json!({"role": "user", "content": "Hello"})],
        instructions: None,
        previous_response_id: Some("resp_good".to_string()),
        stream: true,
        tools: None,
        reasoning: Some(ResponsesReasoning {
            effort: Some("medium".to_string()),
            summary: Some("concise".to_string()),
        }),
        temperature: Some(0.5),
        extra_body: sanitize_extra_body(extra, RESPONSES_RESERVED_BODY_KEYS),
    };

    let serialized = serde_json::to_string(&request).unwrap();
    let value = serde_json::to_value(&request).unwrap();

    assert_eq!(value["reasoning_effort"], "high");
    assert_eq!(value["parallel_tool_calls"], false);
    assert_eq!(value["model"], "gpt-5");
    assert_eq!(value["previous_response_id"], "resp_good");
    assert_eq!(value["reasoning"]["effort"], "medium");
    assert_eq!(value["temperature"], 0.5);
    assert!(value.get("extra_body").is_none());
    assert_eq!(serialized.matches("\"input\":").count(), 1);
    assert_eq!(serialized.matches("\"previous_response_id\":").count(), 1);
    assert_eq!(serialized.matches("\"reasoning\":").count(), 1);
}

#[test]
fn test_anthropic_request_extra_body_flatten() {
    use serde_json::json;

    let extra = json!({
        "system": "override",
        "max_tokens": 1,
        "thinking": {"type": "disabled"},
        "metadata": {"user_id": "123"}
    })
    .as_object()
    .cloned();
    let mut provider = test_provider("anthropic", "https://api.anthropic.com/v1");
    provider.default_model = "claude-3-opus".to_string();
    provider.extra_body = extra;
    let client = test_client(provider);
    let request = client.anthropic_request(
        vec![
            ChatMessage::plain("system", "You are helpful"),
            ChatMessage::plain("user", "Hello"),
        ],
        Vec::new(),
        true,
    );

    let serialized = serde_json::to_string(&request).unwrap();
    let value = serde_json::to_value(&request).unwrap();

    assert_eq!(value["metadata"]["user_id"], "123");
    assert_eq!(value["system"], "You are helpful");
    assert_eq!(value["thinking"]["type"], "adaptive");
    assert_eq!(value["model"], "claude-3-opus");
    assert_eq!(value["max_tokens"], 4096);
    assert!(value.get("extra_body").is_none());
    assert_eq!(serialized.matches("\"system\":").count(), 1);
    assert_eq!(serialized.matches("\"max_tokens\":").count(), 1);
    assert_eq!(serialized.matches("\"thinking\":").count(), 1);
}

#[test]
fn extra_body_reserved_keys_match_each_protocol() {
    for reserved in [
        CHAT_RESERVED_BODY_KEYS,
        RESPONSES_RESERVED_BODY_KEYS,
        ANTHROPIC_RESERVED_BODY_KEYS,
    ] {
        let mut extra = serde_json::Map::new();
        for key in reserved {
            extra.insert((*key).to_string(), serde_json::json!("override"));
        }
        extra.insert("custom".to_string(), serde_json::json!("keep"));

        let sanitized = sanitize_extra_body(Some(extra), reserved).unwrap();
        assert_eq!(sanitized.len(), 1);
        assert_eq!(sanitized["custom"], "keep");
    }
}

fn strip_tagged_sections(mut text: String, tag: &str) -> String {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let open_prefix = format!("<{tag}");
    loop {
        let Some(start) = text.find(&open_prefix) else {
            break;
        };
        let content_start = text[start..]
            .find('>')
            .map(|offset| start + offset + 1)
            .unwrap_or(start + open.len());
        let Some(relative_end) = text[content_start..].find(&close) else {
            text.replace_range(start.., "");
            break;
        };
        let end = content_start + relative_end + close.len();
        text.replace_range(start..end, "");
    }
    text
}
