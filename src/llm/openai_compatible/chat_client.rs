//! chat_client — OpenAI/Anthropic/Responses 聊天客户端实现（原 client_impl2）。

pub(crate) use super::*;

impl OpenAiCompatibleClient {
    pub fn selected_reasoning_variant(&self) -> Option<(ModelReasoningInfo, ReasoningVariant)> {
        let id = self.selected_thinking_variant_id()?;
        let info = models_cache::reasoning_info(&self.provider.id, &self.provider.default_model)?;
        let variant = info
            .variants
            .iter()
            .find(|candidate| candidate.id.as_str() == id)
            .cloned()?;
        reasoning_variant_supported(
            &self.provider,
            &self.provider.default_model,
            &info,
            &variant,
        )
        .then_some((info, variant))
    }

    pub fn selected_thinking_variant_id(&self) -> Option<&str> {
        self.thinking_variants
            .get(&thinking_variant_key(
                &self.provider.id,
                &self.provider.default_model,
            ))
            .map(String::as_str)
    }

    pub fn chat_variant_extra_body(&self) -> Option<Map<String, Value>> {
        let (info, variant) = self.selected_reasoning_variant()?;
        chat_variant_body(&self.provider, &info, variant.setting)
    }

    pub fn responses_reasoning(&self) -> Option<ResponsesReasoning> {
        let summary = self.responses_reasoning_summary();
        let Some((_, variant)) = self.selected_reasoning_variant() else {
            return Some(default_responses_reasoning(summary));
        };
        match variant.setting {
            ReasoningSetting::Effort(effort) => Some(ResponsesReasoning {
                effort: Some(effort),
                summary: Some(summary.to_string()),
            }),
            ReasoningSetting::Toggle(true) => Some(default_responses_reasoning(summary)),
            ReasoningSetting::Toggle(false) | ReasoningSetting::Disabled => None,
            ReasoningSetting::BudgetTokens(_) => Some(default_responses_reasoning(summary)),
        }
    }

    pub fn responses_reasoning_summary(&self) -> &'static str {
        if self.detailed_reasoning_summary {
            "detailed"
        } else {
            "auto"
        }
    }

    pub fn anthropic_variant(
        &self,
        thinking_enabled: bool,
    ) -> (Option<Value>, Option<Map<String, Value>>) {
        if !thinking_enabled {
            return (None, None);
        }
        let Some((_, variant)) = self.selected_reasoning_variant() else {
            return (Some(anthropic_thinking_config()), None);
        };
        match variant.setting {
            ReasoningSetting::Effort(effort) => (
                Some(anthropic_thinking_config()),
                Some(
                    json!({ "output_config": { "effort": effort } })
                        .as_object()
                        .unwrap()
                        .clone(),
                ),
            ),
            ReasoningSetting::Toggle(true) => (Some(anthropic_thinking_config()), None),
            ReasoningSetting::Toggle(false) | ReasoningSetting::Disabled => (None, None),
            ReasoningSetting::BudgetTokens(budget) => {
                let budget = anthropic_reasoning_budget(self.provider.anthropic_max_tokens, budget)
                    .expect("unsupported Anthropic budget variant should be filtered");
                (
                    Some(json!({ "type": "enabled", "budget_tokens": budget })),
                    None,
                )
            }
        }
    }

    pub async fn chat_stream<F>(
        &self,
        messages: Vec<ChatMessage>,
        tools: Vec<ToolDefinition>,
        mut on_chunk: F,
    ) -> Result<ChatResult>
    where
        F: FnMut(ChatStreamChunk) -> Result<()>,
    {
        self.chat_stream_inner(messages, tools, None, false, &mut on_chunk)
            .await
    }

    pub(crate) async fn chat_stream_with_continuation<F>(
        &self,
        messages: Vec<ChatMessage>,
        tools: Vec<ToolDefinition>,
        continuation: Option<&ResponsesContinuation>,
        mut on_chunk: F,
    ) -> Result<ChatResult>
    where
        F: FnMut(ChatStreamChunk) -> Result<()>,
    {
        self.chat_stream_inner(messages, tools, continuation, false, &mut on_chunk)
            .await
    }

    /// Runs an internal completion without exposing partial output. Since no
    /// chunk is committed to a user, a failed endpoint can be safely replaced
    /// even after it emitted an incomplete response.
    pub async fn chat_buffered(
        &self,
        messages: Vec<ChatMessage>,
        tools: Vec<ToolDefinition>,
    ) -> Result<ChatResult> {
        self.chat_stream_inner(messages, tools, None, true, &mut |_| Ok(()))
            .await
    }

    /// Cache keepalive ping (v7 DeepSeek 高命中策略): re-sends the exact
    /// prompt prefix of the last live request as a non-streaming
    /// max_tokens=1 completion so best-effort provider caches keep the deep
    /// prefix alive between user turns. The messages/tools serialization goes
    /// through the same path as live chat, so the server-rendered prompt is
    /// byte-identical (measured: extra body params like max_tokens do not
    /// affect the provider prefix cache key). Returns the reported usage, or
    /// None when the selected endpoint speaks a protocol where the ping does
    /// not apply (Anthropic / OpenAI Responses).
    pub async fn cache_keepalive(
        &self,
        messages: Vec<ChatMessage>,
        tools: Vec<ToolDefinition>,
        endpoint_hint: Option<&(String, String)>,
    ) -> Result<Option<Usage>> {
        let endpoints = self.endpoints.as_ref();
        // 钉住上一条真实请求的 endpoint:缓存按 (供应商, 前缀) 存活,
        // 轮转选出的"下一家"没有这份前缀,ping 过去只是白买 miss。
        let hinted = endpoint_hint.and_then(|(provider, model)| {
            endpoints.iter().position(|endpoint| {
                endpoint.provider.id == *provider && endpoint.provider.default_model == *model
            })
        });
        let index = hinted.unwrap_or_else(|| {
            ordered_endpoint_indices(endpoints)
                .first()
                .copied()
                .unwrap_or(0)
        });
        let endpoint = endpoints
            .get(index)
            .context("no LLM endpoint configured for cache keepalive")?;
        let client = self.with_endpoint(endpoint);
        if client.uses_openai_responses() || client.uses_anthropic_messages() {
            return Ok(None);
        }
        client.cache_keepalive_single(messages, tools).await
    }

    pub async fn cache_keepalive_single(
        &self,
        messages: Vec<ChatMessage>,
        tools: Vec<ToolDefinition>,
    ) -> Result<Option<Usage>> {
        let request_id = gen_llm_request_id();
        let extra_body = merge_extra_body(
            sanitize_extra_body(self.provider.extra_body.clone(), CHAT_RESERVED_BODY_KEYS),
            self.chat_variant_extra_body(),
        );
        let messages = prepare_chat_messages_for_provider(&self.provider, messages);
        let request = ChatRequest {
            model: self.provider.default_model.clone(),
            messages,
            temperature: self.provider.effective_temperature(),
            stream: false,
            stream_options: None,
            max_tokens: Some(1),
            tools: (!tools.is_empty()).then_some(tools),
            chat_template_kwargs: taotoken_glm_chat_template_kwargs(&self.provider),
            extra_body,
        };
        let url = format!(
            "{}/chat/completions",
            self.provider.base_url.trim_end_matches('/')
        );
        let response = self
            .send_chat_completion_request(&url, &request, &request_id, "chat.cache_keepalive")
            .await?;
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if !status.is_success() {
            bail!("cache keepalive ping failed with HTTP {status}: {body}");
        }
        let value: serde_json::Value = serde_json::from_str(&body)
            .with_context(|| "cache keepalive response was not valid JSON")?;
        let usage = value
            .get("usage")
            .cloned()
            .and_then(|usage| serde_json::from_value::<Usage>(usage).ok())
            .map(|mut usage| {
                usage.normalize_cache_fields();
                usage
            });
        // 保温 ping 也进 cache-usage 记账:不然命中率诊断里多出一段
        // "看不见的流量"(deepseek 报告 P2 的观测盲区)。
        crate::llm::cache_log::record(
            "keepalive",
            &self.provider.id,
            &self.provider.default_model,
            0,
            &request_id,
            usage.as_ref(),
        );
        Ok(usage)
    }

    pub async fn chat_stream_inner<F>(
        &self,
        messages: Vec<ChatMessage>,
        tools: Vec<ToolDefinition>,
        continuation: Option<&ResponsesContinuation>,
        buffered: bool,
        on_chunk: &mut F,
    ) -> Result<ChatResult>
    where
        F: FnMut(ChatStreamChunk) -> Result<()>,
    {
        let request_id = gen_llm_request_id();
        let endpoints = self.endpoints.as_ref();
        let mut errors = Vec::new();
        let mut order = if let Some(continuation) = continuation {
            let index = endpoints
                .iter()
                .position(|endpoint| endpoint.id() == continuation.endpoint_id)
                .with_context(|| {
                    format!(
                        "Responses continuation endpoint is no longer available: {}",
                        continuation.endpoint_id
                    )
                })?;
            vec![index]
        } else {
            ordered_endpoint_indices(endpoints)
        };
        // Every endpoint is cooling down. Refusing outright would strand a
        // single-endpoint user for the whole cooldown, so one probe still goes
        // out — but exactly one, and to whichever endpoint recovers first.
        // Refilling the pool here (and then padding it below) meant a rate
        // limit cost three requests per turn *for the entire cooldown*, which
        // made the cooldown worse than useless.
        let probe_only = order.is_empty();
        if probe_only {
            tracing::warn!(
                request_id,
                endpoint_count = endpoints.len(),
                all_endpoints_cooling_down = true,
                "{}",
                t(
                    "All LLM endpoints are cooling down; sending a single probe",
                    "所有 LLM 端点均在冷却；仅发送一次探测请求"
                )
            );
            order = soonest_ready_endpoint_index(endpoints)
                .into_iter()
                .collect();
        }
        // A dropped stream or a 5xx is a moment in time, not a verdict on the
        // endpoint. Tying the number of attempts to the number of configured
        // endpoints meant someone with a single model got no retry at all,
        // which is backwards: they are the ones with nowhere else to go. Pad
        // the attempt list by cycling so every setup gets the same budget.
        // Errors that a retry cannot fix still stop on the first attempt —
        // `endpoint_failover_allowed` returns before the next one is tried,
        // and `same_endpoint_retry_allowed` skips the padded repeats of an
        // endpoint that answered 429/401.
        if !probe_only && !order.is_empty() && order.len() < MIN_ENDPOINT_ATTEMPTS {
            let cycle: Vec<usize> = order.clone();
            while order.len() < MIN_ENDPOINT_ATTEMPTS {
                order.extend(cycle.iter().copied());
            }
            order.truncate(MIN_ENDPOINT_ATTEMPTS);
        }
        tracing::debug!(
            request_id,
            endpoint_count = order.len(),
            message_count = messages.len(),
            tool_count = tools.len(),
            continued = continuation.is_some(),
            "{}",
            t("LLM request started", "LLM 请求已开始")
        );
        let mut exhausted: Vec<String> = Vec::new();
        for (attempt, index) in order.into_iter().enumerate() {
            let endpoint = &endpoints[index];
            if exhausted.contains(&endpoint.id()) {
                continue;
            }
            let client = self.with_endpoint(endpoint);
            if attempt > 0 {
                on_chunk(ChatStreamChunk {
                    kind: ChatStreamKind::ReasoningReset,
                    text: String::new(),
                })?;
            }
            let started = Instant::now();
            tracing::debug!(
                request_id,
                attempt = attempt + 1,
                provider = %endpoint.provider.id,
                model = %endpoint.provider.default_model,
                key_index = endpoint.key_index + 1,
                "{}",
                t("LLM endpoint attempt started", "LLM 端点尝试已开始")
            );
            let mut attempt_committed = false;
            let result = {
                let buffered = buffered || self.buffered_delivery;
                let mut attempt_on_chunk = |chunk: ChatStreamChunk| {
                    if !buffered {
                        attempt_committed |=
                            stream_chunk_commits_attempt(&chunk, client.reasoning_visibility);
                    }
                    on_chunk(chunk)
                };
                client
                    .chat_stream_single(
                        messages.clone(),
                        tools.clone(),
                        continuation.map(|continuation| continuation.response_id.as_str()),
                        &request_id,
                        &mut attempt_on_chunk,
                    )
                    .await
            };
            match result {
                Ok(mut result) => {
                    result.provider_id = Some(endpoint.provider.id.clone());
                    result.model = Some(endpoint.provider.default_model.clone());
                    if let Some(next) = result.responses_continuation.as_mut() {
                        next.endpoint_id = endpoint.id();
                    }
                    mark_endpoint_success(endpoint);
                    crate::llm::cache_log::record(
                        self.request_scope,
                        &endpoint.provider.id,
                        &endpoint.provider.default_model,
                        endpoint.key_index,
                        &request_id,
                        result.usage.as_ref(),
                    );
                    tracing::debug!(
                        request_id,
                        attempt = attempt + 1,
                        provider = %endpoint.provider.id,
                        model = %endpoint.provider.default_model,
                        elapsed_ms = started.elapsed().as_millis(),
                        "{}",
                        t("LLM endpoint succeeded", "LLM 端点请求成功")
                    );
                    return Ok(result);
                }
                Err(err) => {
                    let cooldown = mark_endpoint_failure(endpoint, &err);
                    let endpoint_cooling_down = cooldown.is_some();
                    let cooldown_seconds = cooldown.map(|duration| duration.as_secs()).unwrap_or(0);
                    if let Some(failure) = err.downcast_ref::<TransportFailure>() {
                        tracing::error!(
                            request_id,
                            attempt = attempt + 1,
                            provider = %endpoint.provider.id,
                            model = %endpoint.provider.default_model,
                            stage = failure.stage,
                            transport_kind = %failure.kind,
                            endpoint_cooling_down,
                            cooldown_seconds,
                            elapsed_ms = started.elapsed().as_millis(),
                            error = %format!("{err:#}"),
                            "{}",
                            t("LLM endpoint transport failure", "LLM 端点传输失败")
                        );
                    } else if let Some(failure) = err.downcast_ref::<HttpStatusFailure>() {
                        tracing::error!(
                            request_id,
                            attempt = attempt + 1,
                            provider = %endpoint.provider.id,
                            model = %endpoint.provider.default_model,
                            status = failure.status,
                            failure_kind = %failure.kind,
                            endpoint_cooling_down,
                            cooldown_seconds,
                            elapsed_ms = started.elapsed().as_millis(),
                            "{}",
                            t("LLM endpoint HTTP failure", "LLM 端点 HTTP 请求失败")
                        );
                    } else {
                        tracing::error!(
                            request_id,
                            attempt = attempt + 1,
                            provider = %endpoint.provider.id,
                            model = %endpoint.provider.default_model,
                            endpoint_cooling_down,
                            cooldown_seconds,
                            elapsed_ms = started.elapsed().as_millis(),
                            error = %format!("{err:#}"),
                            "{}",
                            t(
                                "LLM endpoint failed outside the HTTP send stage",
                                "LLM 端点在 HTTP 发送阶段之外失败"
                            )
                        );
                    }
                    let message = format!("{err:#}");
                    errors.push(format!(
                        "{} / {} key#{}: {message}",
                        endpoint.provider.id,
                        endpoint.provider.default_model,
                        endpoint.key_index + 1
                    ));
                    if !same_endpoint_retry_allowed(&err) {
                        exhausted.push(endpoint.id());
                    }
                    if attempt_committed {
                        return Err(err.context(
                            "LLM stream failed after emitting output; endpoint failover was suppressed",
                        ));
                    }
                    if !endpoint_failover_allowed(&err) {
                        return Err(err.context(
                            "LLM request was rejected; endpoint failover was suppressed",
                        ));
                    }
                }
            }
        }
        bail!(
            "no LLM provider/model endpoint succeeded (request {request_id}):\n- {}",
            errors.join("\n- ")
        )
    }

    pub async fn chat_stream_single<F>(
        &self,
        messages: Vec<ChatMessage>,
        tools: Vec<ToolDefinition>,
        previous_response_id: Option<&str>,
        request_id: &str,
        on_chunk: &mut F,
    ) -> Result<ChatResult>
    where
        F: FnMut(ChatStreamChunk) -> Result<()>,
    {
        let protocol = ProviderProtocol::from_provider(&self.provider)?;
        let uses_responses = protocol == ProviderProtocol::OpenAiResponses
            || (protocol == ProviderProtocol::Auto && self.uses_openai_responses());
        if previous_response_id.is_some() && !uses_responses {
            bail!("Responses continuation endpoint no longer uses the Responses protocol");
        }
        if protocol == ProviderProtocol::Anthropic
            || (protocol == ProviderProtocol::Auto && self.uses_anthropic_messages())
        {
            return self
                .chat_anthropic_stream(messages, tools, request_id, on_chunk)
                .await;
        }
        if uses_responses {
            if let Some(result) = self
                .chat_responses_stream(
                    messages.clone(),
                    tools.clone(),
                    previous_response_id,
                    request_id,
                    on_chunk,
                )
                .await?
            {
                return Ok(result);
            }
            if previous_response_id.is_some() {
                bail!("OpenAI Responses continuation is not supported by this provider");
            }
            if protocol == ProviderProtocol::OpenAiResponses {
                bail!("OpenAI Responses protocol is not supported by this provider");
            }
            if let Some((info, variant)) = self.selected_reasoning_variant() {
                if !reasoning_variant_supported_for_protocol(
                    &self.provider,
                    &info,
                    &variant,
                    ProviderProtocol::OpenAiChat,
                ) {
                    bail!(
                        "thinking variant '{}' cannot be applied after falling back from OpenAI Responses to Chat Completions",
                        variant.id
                    );
                }
            }
        }
        let extra_body = merge_extra_body(
            sanitize_extra_body(self.provider.extra_body.clone(), CHAT_RESERVED_BODY_KEYS),
            self.chat_variant_extra_body(),
        );
        let messages = prepare_chat_messages_for_provider(&self.provider, messages);
        let mut request = ChatRequest {
            model: self.provider.default_model.clone(),
            messages,
            temperature: self.provider.effective_temperature(),
            stream: true,
            stream_options: Some(ChatStreamOptions {
                include_usage: true,
            }),
            max_tokens: self.max_tokens_override,
            tools: (!tools.is_empty()).then_some(tools),
            chat_template_kwargs: taotoken_glm_chat_template_kwargs(&self.provider),
            extra_body,
        };
        let url = format!(
            "{}/chat/completions",
            self.provider.base_url.trim_end_matches('/')
        );
        let mut response = self
            .send_chat_completion_request(&url, &request, request_id, "chat.send")
            .await?;
        let mut status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            if non_stream_quota_fallback_candidate(status.as_u16(), &body) {
                let mut retry = request.clone();
                retry.stream = false;
                retry.stream_options = None;
                let response = self
                    .send_chat_completion_request(
                        &url,
                        &retry,
                        request_id,
                        "chat.retry_without_streaming",
                    )
                    .await?;
                let retry_status = response.status();
                if retry_status.is_success() {
                    tracing::info!(
                        request_id,
                        provider = %self.provider.id,
                        model = %self.provider.default_model,
                        "{}",
                        t(
                            "streaming quota was unavailable; non-streaming compatibility retry succeeded",
                            "流式配额不可用；非流式兼容重试成功"
                        )
                    );
                    return self
                        .consume_chat_completion_response(response, on_chunk)
                        .await;
                }
                let retry_body = response.text().await.unwrap_or_default();
                tracing::debug!(
                    request_id,
                    status = retry_status.as_u16(),
                    "{}",
                    t(
                        "non-streaming quota compatibility retry returned an HTTP error",
                        "非流式配额兼容重试返回 HTTP 错误"
                    )
                );
                return self.bail_chat_completion_failure(retry_status.as_u16(), &retry_body);
            }
            if stream_options_unsupported(status.as_u16(), &body) {
                request.stream_options = None;
                response = self
                    .send_chat_completion_request(
                        &url,
                        &request,
                        request_id,
                        "chat.retry_without_stream_options",
                    )
                    .await?;
                status = response.status();
                if status.is_success() {
                    return self
                        .consume_chat_completion_stream(response, on_chunk)
                        .await;
                }
                let body = response.text().await.unwrap_or_default();
                if let Some(result) = self
                    .try_zen_chat_completion_compat_retry(
                        &url,
                        &request,
                        status.as_u16(),
                        &body,
                        request_id,
                        on_chunk,
                    )
                    .await?
                {
                    return Ok(result);
                }
                return self.bail_chat_completion_failure(status.as_u16(), &body);
            }
            if let Some(result) = self
                .try_zen_chat_completion_compat_retry(
                    &url,
                    &request,
                    status.as_u16(),
                    &body,
                    request_id,
                    on_chunk,
                )
                .await?
            {
                return Ok(result);
            }
            return self.bail_chat_completion_failure(status.as_u16(), &body);
        }

        self.consume_chat_completion_stream(response, on_chunk)
            .await
    }

    pub async fn send_chat_completion_request(
        &self,
        url: &str,
        request: &ChatRequest,
        request_id: &str,
        stage: &'static str,
    ) -> Result<reqwest::Response> {
        crate::llm::request_log::record(
            &self.provider.id,
            &self.provider.default_model,
            "chat",
            self.request_scope,
            url,
            request,
        );
        self.send_with_transport_retry(request_id, stage, || {
            self.client
                .post(url)
                .bearer_auth(&self.api_key)
                .json(request)
        })
        .await
    }

    pub async fn send_with_transport_retry<F>(
        &self,
        request_id: &str,
        stage: &'static str,
        mut build_request: F,
    ) -> Result<reqwest::Response>
    where
        F: FnMut() -> reqwest::RequestBuilder,
    {
        let mut connect_retry_used = false;
        let mut attempt = 0usize;
        loop {
            attempt = attempt.saturating_add(1);
            let started = Instant::now();
            let send = build_request().send();
            let response = if let Some(timeouts) = self.request_timeouts {
                match tokio::time::timeout(timeouts.response_header, send).await {
                    Ok(response) => response,
                    Err(_) => {
                        tracing::warn!(
                            request_id,
                            stage,
                            attempt,
                            timeout_seconds = timeouts.response_header.as_secs(),
                            "{}",
                            t("LLM response header timed out", "LLM 响应头等待超时")
                        );
                        return Err(anyhow::anyhow!(
                            "LLM response header timed out after {} seconds",
                            timeouts.response_header.as_secs()
                        )
                        .context(TransportFailure {
                            stage,
                            kind: TransportFailureKind::Timeout,
                        }));
                    }
                }
            } else {
                send.await
            };
            match response {
                Ok(response) => {
                    let status = response.status().as_u16();
                    let retryable_status = retryable_http_status(status);
                    let will_retry = retryable_status && attempt < MAX_SEND_ATTEMPTS;
                    tracing::debug!(
                        request_id,
                        stage,
                        attempt,
                        status,
                        will_retry,
                        elapsed_ms = started.elapsed().as_millis(),
                        "{}",
                        t(
                            "LLM HTTP response headers received",
                            "已收到 LLM HTTP 响应头"
                        )
                    );
                    if will_retry {
                        let delay = http_status_retry_delay(attempt);
                        tracing::warn!(
                            request_id,
                            stage,
                            attempt,
                            status,
                            retry_delay_ms = delay.as_millis(),
                            "{}",
                            t(
                                "LLM HTTP request returned a transient server error",
                                "LLM HTTP 请求返回临时服务器错误"
                            )
                        );
                        let _ = response.bytes().await;
                        tokio::time::sleep(delay).await;
                        continue;
                    }
                    if retryable_status {
                        let body = response.text().await.unwrap_or_default();
                        return Err(anyhow::anyhow!(
                            "LLM HTTP request failed after {attempt} attempts: {body}"
                        )
                        .context(HttpStatusFailure::classify(status, &body)));
                    }
                    return Ok(response);
                }
                Err(error) => {
                    let kind = if error.is_connect() {
                        TransportFailureKind::Connect
                    } else if error.is_timeout() {
                        TransportFailureKind::Timeout
                    } else {
                        TransportFailureKind::Other
                    };
                    let will_retry = attempt < MAX_SEND_ATTEMPTS
                        && !connect_retry_used
                        && retryable_transport_failure(kind);
                    connect_retry_used |= will_retry;
                    let error = error.without_url();
                    tracing::warn!(
                        request_id,
                        stage,
                        attempt,
                        transport_kind = %kind,
                        will_retry,
                        elapsed_ms = started.elapsed().as_millis(),
                        error = %format_error_chain(&error),
                        "{}",
                        t("LLM HTTP transport attempt failed", "LLM HTTP 传输尝试失败")
                    );
                    if will_retry {
                        tokio::time::sleep(TRANSPORT_RETRY_DELAY).await;
                        continue;
                    }
                    return Err(anyhow::Error::new(error).context(TransportFailure { stage, kind }));
                }
            }
        }
    }

    pub async fn next_response_chunk<S, T>(
        &self,
        stream: &mut S,
        stage: &'static str,
    ) -> Result<Option<T>>
    where
        S: Stream<Item = std::result::Result<T, reqwest::Error>> + Unpin,
    {
        let next = if let Some(timeouts) = self.request_timeouts {
            match tokio::time::timeout(timeouts.stream_idle, stream.next()).await {
                Ok(next) => next,
                Err(_) => {
                    return Err(anyhow::anyhow!(
                        "LLM response stream was idle for {} seconds",
                        timeouts.stream_idle.as_secs()
                    )
                    .context(TransportFailure {
                        stage,
                        kind: TransportFailureKind::Timeout,
                    }));
                }
            }
        } else {
            stream.next().await
        };
        next.transpose().map_err(|error| {
            anyhow::Error::new(error).context(TransportFailure {
                stage,
                kind: TransportFailureKind::Other,
            })
        })
    }

    pub async fn try_zen_chat_completion_compat_retry<F>(
        &self,
        url: &str,
        request: &ChatRequest,
        status: u16,
        body: &str,
        request_id: &str,
        on_chunk: &mut F,
    ) -> Result<Option<ChatResult>>
    where
        F: FnMut(ChatStreamChunk) -> Result<()>,
    {
        if !zen_upstream_failed(&self.provider, status, body) {
            return Ok(None);
        }

        let mut retries = Vec::new();
        if request.stream_options.is_some() {
            let mut retry = request.clone();
            retry.stream_options = None;
            retries.push(retry);
        }
        if request.tools.is_some() {
            let mut retry = request.clone();
            retry.stream_options = None;
            retry.tools = None;
            retries.push(retry);
        }

        for (attempt, retry) in retries.into_iter().enumerate() {
            let response = self
                .send_chat_completion_request(
                    url,
                    &retry,
                    request_id,
                    "chat.zen_compatibility_retry",
                )
                .await?;
            let status = response.status();
            if status.is_success() {
                return self
                    .consume_chat_completion_stream(response, on_chunk)
                    .await
                    .map(Some);
            }
            tracing::debug!(
                request_id,
                attempt = attempt + 1,
                status = status.as_u16(),
                "{}",
                t(
                    "Zen compatibility retry returned an HTTP error",
                    "Zen 兼容重试返回 HTTP 错误"
                )
            );
            let _ = response.text().await;
        }

        Ok(None)
    }

    pub async fn consume_chat_completion_stream<F>(
        &self,
        response: reqwest::Response,
        on_chunk: &mut F,
    ) -> Result<ChatResult>
    where
        F: FnMut(ChatStreamChunk) -> Result<()>,
    {
        let dsml = dsml_enabled_for(&self.provider);
        let mut buffer = Utf8LineBuffer::default();
        let mut content = String::new();
        let mut content_emitted = 0usize;
        let mut reasoning = String::new();
        let mut reasoning_emitted = 0usize;
        let mut reasoning_part_active = false;
        let mut finish_reason = None;
        let mut usage = None;
        let mut tool_calls = ToolCallAccumulator::default();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = self.next_response_chunk(&mut stream, "chat.stream").await? {
            for line in buffer.push(&chunk)? {
                if let Some(done) = handle_sse_line(
                    &line,
                    &mut content,
                    &mut content_emitted,
                    &mut reasoning,
                    &mut reasoning_emitted,
                    &mut reasoning_part_active,
                    &mut finish_reason,
                    &mut usage,
                    &mut tool_calls,
                    &mut *on_chunk,
                )? {
                    if done {
                        return finalize_stream_result(
                            content,
                            reasoning,
                            usage,
                            tool_calls.finish(),
                            dsml,
                        );
                    }
                }
            }
        }
        for line in buffer.finish()? {
            let _ = handle_sse_line(
                &line,
                &mut content,
                &mut content_emitted,
                &mut reasoning,
                &mut reasoning_emitted,
                &mut reasoning_part_active,
                &mut finish_reason,
                &mut usage,
                &mut tool_calls,
                &mut *on_chunk,
            )?;
        }
        // Reaching here means the socket closed without `[DONE]` — the loop
        // above returns early on that marker. A provider that ends this way
        // still has to have said it was finished somewhere, and `finish_reason`
        // is the only other place it can say so (llama.cpp's Responses
        // endpoint, for one, never sends `[DONE]`). With neither signal the
        // response is a truncated fragment, and returning it as a completed
        // turn is how an empty reply reaches the user with nothing logged.
        //
        // Reported as a transport failure so the existing machinery retries it
        // across endpoints and resets the partial reasoning already streamed.
        // Retrying is safe here: tool calls execute after this returns, so a
        // truncated turn has run nothing yet.
        if finish_reason.is_none() {
            return Err(anyhow::anyhow!(t(
                "the response stream ended before the model said it was done",
                "模型还没说完，响应流就提前结束了"
            ))
            .context(TransportFailure {
                stage: "chat.stream",
                kind: TransportFailureKind::Other,
            }));
        }
        flush_buffer(
            &reasoning,
            &mut reasoning_emitted,
            ChatStreamKind::Reasoning,
            &mut *on_chunk,
            true,
        )?;
        flush_buffer(
            &content,
            &mut content_emitted,
            ChatStreamKind::Content,
            &mut *on_chunk,
            true,
        )?;
        tracing::debug!(
            provider = %self.provider.id,
            model = %self.provider.default_model,
            finish_reason = finish_reason.as_deref(),
            content_chars = content.chars().count(),
            reasoning_chars = reasoning.chars().count(),
            tool_call_count = tool_calls.calls.len(),
            "{}",
            t("Chat completions stream reached EOF", "聊天补全流已到达 EOF")
        );
        let mut result =
            finalize_stream_result(content, reasoning, usage, tool_calls.finish(), dsml)?;
        result.finish_reason = finish_reason;
        if reasoning_part_active {
            on_chunk(ChatStreamChunk {
                kind: ChatStreamKind::ReasoningPartEnd,
                text: String::new(),
            })?;
        }
        Ok(result)
    }

    pub async fn consume_chat_completion_response<F>(
        &self,
        response: reqwest::Response,
        on_chunk: &mut F,
    ) -> Result<ChatResult>
    where
        F: FnMut(ChatStreamChunk) -> Result<()>,
    {
        const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;

        if response
            .content_length()
            .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
        {
            bail!("non-streaming chat response exceeds the 16 MiB limit");
        }
        let mut bytes = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = self
            .next_response_chunk(&mut stream, "chat.response")
            .await?
        {
            if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
                bail!("non-streaming chat response exceeds the 16 MiB limit");
            }
            bytes.extend_from_slice(&chunk);
        }
        let response: ChatCompletionResponse =
            serde_json::from_slice(&bytes).with_context(|| {
                format!(
                    "{}: {}",
                    t(
                        "invalid non-streaming chat completions response",
                        "无效的非流式聊天响应",
                    ),
                    clean_plain_text(String::from_utf8_lossy(&bytes).to_string())
                )
            })?;
        if let Some(error) = response.error {
            bail!(
                "{}: {}",
                t(
                    "non-streaming chat completions returned an error",
                    "非流式聊天响应返回错误"
                ),
                provider_error_text(&error)
            );
        }
        let choice = response
            .choices
            .into_iter()
            .next()
            .context("non-streaming chat response contained no choices")?;
        let mut tool_calls = ToolCallAccumulator::default();
        let reasoning = delta_reasoning_text(&choice.message).unwrap_or_default();
        if !reasoning.is_empty() {
            on_chunk(ChatStreamChunk {
                kind: ChatStreamKind::ReasoningPartStart,
                text: String::new(),
            })?;
            on_chunk(ChatStreamChunk {
                kind: ChatStreamKind::Reasoning,
                text: reasoning.clone(),
            })?;
            on_chunk(ChatStreamChunk {
                kind: ChatStreamKind::ReasoningPartEnd,
                text: String::new(),
            })?;
        }
        let content = choice.message.content.unwrap_or_default();
        if !content.is_empty() {
            on_chunk(ChatStreamChunk {
                kind: ChatStreamKind::Content,
                text: content.clone(),
            })?;
        }
        for tool_call in choice.message.tool_calls {
            if let Some(name) = tool_calls.push(tool_call) {
                on_chunk(ChatStreamChunk {
                    kind: ChatStreamKind::ToolCall,
                    text: name,
                })?;
            }
        }
        tracing::debug!(
            provider = %self.provider.id,
            model = %self.provider.default_model,
            finish_reason = choice.finish_reason.as_deref(),
            content_chars = content.chars().count(),
            reasoning_chars = reasoning.chars().count(),
            tool_call_count = tool_calls.calls.len(),
            "{}",
            t(
                "Non-streaming chat completions response consumed",
                "非流式聊天补全响应已处理"
            )
        );
        let mut result = finalize_stream_result(
            content,
            reasoning,
            response.usage,
            tool_calls.finish(),
            dsml_enabled_for(&self.provider),
        )?;
        result.finish_reason = choice.finish_reason;
        Ok(result)
    }

    pub fn bail_chat_completion_failure<T>(&self, status: u16, body: &str) -> Result<T> {
        let hint = claude_protocol_hint(&self.provider);
        Err(anyhow::anyhow!(
            "{} ({}): {}{}",
            t("chat completions stream request failed", "聊天流式请求失败",),
            status,
            body,
            hint
        )
        .context(HttpStatusFailure::classify(status, body)))
    }

    pub async fn chat_anthropic_stream<F>(
        &self,
        messages: Vec<ChatMessage>,
        tools: Vec<ToolDefinition>,
        request_id: &str,
        on_chunk: &mut F,
    ) -> Result<ChatResult>
    where
        F: FnMut(ChatStreamChunk) -> Result<()>,
    {
        let mut response = self
            .send_anthropic_request(
                &self.anthropic_request(messages.clone(), tools.clone(), true),
                request_id,
                "anthropic.send",
            )
            .await?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            let sent_thinking_blocks = messages.iter().any(|message| {
                message.thinking_signature.is_some()
                    && message
                        .reasoning_content
                        .as_deref()
                        .is_some_and(|text| !text.trim().is_empty())
            });
            if sent_thinking_blocks && anthropic_thinking_unsupported(status.as_u16(), &body) {
                // The request already carried well-formed thinking blocks, so a
                // thinking-shaped 400 here is a protocol bug on our side, not a
                // capability gap. Surface it instead of silently downgrading the
                // whole tool loop (double request per round + split cache).
                return Err(anyhow::anyhow!(
                    "{} ({status}): {body}",
                    t(
                        "anthropic messages stream rejected replayed thinking blocks",
                        "Anthropic Messages 拒绝了回传的 thinking 块"
                    )
                )
                .context(HttpStatusFailure::classify(status.as_u16(), &body)));
            }
            if anthropic_thinking_unsupported(status.as_u16(), &body) {
                response = self
                    .send_anthropic_request(
                        &self.anthropic_request(messages, tools, false),
                        request_id,
                        "anthropic.retry_without_thinking",
                    )
                    .await?;
                let status = response.status();
                if status.is_success() {
                    return self.consume_anthropic_stream(response, on_chunk).await;
                }
                let body = response.text().await.unwrap_or_default();
                return Err(anyhow::anyhow!(
                    "{} ({status}): {body}",
                    t(
                        "anthropic messages stream request failed",
                        "Anthropic Messages 流式请求失败"
                    )
                )
                .context(HttpStatusFailure::classify(status.as_u16(), &body)));
            }
            return Err(anyhow::anyhow!(
                "{} ({status}): {body}",
                t(
                    "anthropic messages stream request failed",
                    "Anthropic Messages 流式请求失败"
                )
            )
            .context(HttpStatusFailure::classify(status.as_u16(), &body)));
        }

        self.consume_anthropic_stream(response, on_chunk).await
    }
    pub fn anthropic_request(
        &self,
        messages: Vec<ChatMessage>,
        tools: Vec<ToolDefinition>,
        thinking: bool,
    ) -> AnthropicRequest {
        let (variant_thinking, variant_extra) = self.anthropic_variant(thinking);
        let extra_body = merge_extra_body(
            sanitize_extra_body(
                self.provider.extra_body.clone(),
                ANTHROPIC_RESERVED_BODY_KEYS,
            ),
            variant_extra,
        );
        AnthropicRequest {
            model: self.provider.default_model.clone(),
            system: lower_anthropic_system(&messages),
            messages: lower_anthropic_messages(messages),
            tools: (!tools.is_empty()).then(|| lower_anthropic_tools(tools)),
            stream: true,
            max_tokens: self
                .max_tokens_override
                .map(|cap| cap.min(self.provider.anthropic_max_tokens))
                .unwrap_or(self.provider.anthropic_max_tokens),
            temperature: Some(self.provider.effective_temperature()),
            thinking: variant_thinking,
            extra_body,
        }
    }

    pub async fn send_anthropic_request(
        &self,
        request: &AnthropicRequest,
        request_id: &str,
        stage: &'static str,
    ) -> Result<reqwest::Response> {
        let url = format!("{}/messages", self.provider.base_url.trim_end_matches('/'));
        crate::llm::request_log::record(
            &self.provider.id,
            &self.provider.default_model,
            "anthropic",
            self.request_scope,
            &url,
            request,
        );
        self.send_with_transport_retry(request_id, stage, || {
            self.client
                .post(&url)
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", "2023-06-01")
                .json(request)
        })
        .await
    }

    pub async fn consume_anthropic_stream<F>(
        &self,
        response: reqwest::Response,
        on_chunk: &mut F,
    ) -> Result<ChatResult>
    where
        F: FnMut(ChatStreamChunk) -> Result<()>,
    {
        let dsml = dsml_enabled_for(&self.provider);
        let mut state = AnthropicStreamState::default();
        let mut buffer = SseDataBuffer::default();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = self
            .next_response_chunk(&mut stream, "anthropic.stream")
            .await?
        {
            for data in buffer.push(&chunk)? {
                if handle_anthropic_sse_data(&data, &mut state, &mut *on_chunk)? {
                    let signature = state.thinking_signature.take();
                    let mut result = finalize_stream_result(
                        state.content,
                        state.reasoning,
                        state.usage,
                        state.tool_calls.finish(),
                        dsml,
                    )?;
                    result.thinking_signature = signature;
                    return Ok(result);
                }
            }
        }
        for data in buffer.finish()? {
            let _ = handle_anthropic_sse_data(&data, &mut state, &mut *on_chunk)?;
        }
        flush_buffer(
            &state.reasoning,
            &mut state.reasoning_emitted,
            ChatStreamKind::Reasoning,
            &mut *on_chunk,
            true,
        )?;
        flush_buffer(
            &state.content,
            &mut state.content_emitted,
            ChatStreamKind::Content,
            &mut *on_chunk,
            true,
        )?;
        let reasoning_part_active = state.reasoning_part_active;
        let signature = state.thinking_signature.take();
        let mut result = finalize_stream_result(
            state.content,
            state.reasoning,
            state.usage,
            state.tool_calls.finish(),
            dsml,
        )?;
        result.thinking_signature = signature;
        if reasoning_part_active {
            on_chunk(ChatStreamChunk {
                kind: ChatStreamKind::ReasoningPartEnd,
                text: String::new(),
            })?;
        }
        Ok(result)
    }

    pub async fn chat_responses_stream<F>(
        &self,
        messages: Vec<ChatMessage>,
        tools: Vec<ToolDefinition>,
        previous_response_id: Option<&str>,
        request_id: &str,
        on_chunk: &mut F,
    ) -> Result<Option<ChatResult>>
    where
        F: FnMut(ChatStreamChunk) -> Result<()>,
    {
        let extra_body = sanitize_extra_body(
            self.provider.extra_body.clone(),
            RESPONSES_RESERVED_BODY_KEYS,
        );
        let store_disabled = extra_body
            .as_ref()
            .and_then(|body| body.get("store"))
            .and_then(Value::as_bool)
            == Some(false);
        if store_disabled && !tools.is_empty() {
            bail!("OpenAI Responses tools require response storage; remove store=false or disable tools");
        }
        if previous_response_id.is_some() && store_disabled {
            bail!("OpenAI Responses tool continuation requires response storage; remove store=false or disable tools");
        }
        let request = ResponsesRequest {
            model: self.provider.default_model.clone(),
            input: lower_responses_messages(messages),
            instructions: None,
            previous_response_id: previous_response_id.map(str::to_string),
            stream: true,
            tools: (!tools.is_empty()).then(|| lower_responses_tools(tools)),
            reasoning: self.responses_reasoning(),
            temperature: Some(self.provider.effective_temperature()),
            extra_body,
        };
        let reasoning_effort = request
            .reasoning
            .as_ref()
            .and_then(|reasoning| reasoning.effort.as_deref())
            .unwrap_or("disabled");
        let reasoning_summary = request
            .reasoning
            .as_ref()
            .and_then(|reasoning| reasoning.summary.as_deref())
            .unwrap_or("disabled");
        tracing::debug!(
            request_id,
            provider = %self.provider.id,
            model = %self.provider.default_model,
            reasoning_effort,
            reasoning_summary,
            "{}",
            t("Responses request configured", "Responses 请求配置完成")
        );
        let url = format!("{}/responses", self.provider.base_url.trim_end_matches('/'));
        crate::llm::request_log::record(
            &self.provider.id,
            &self.provider.default_model,
            "responses",
            self.request_scope,
            &url,
            &request,
        );
        let response = self
            .send_with_transport_retry(request_id, "responses.send", || {
                self.client
                    .post(&url)
                    .bearer_auth(&self.api_key)
                    .json(&request)
            })
            .await?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            if responses_unsupported(status.as_u16(), &body) {
                return Ok(None);
            }
            return Err(anyhow::anyhow!(
                "{} ({status}): {body}",
                t("responses stream request failed", "Responses 流式请求失败")
            )
            .context(HttpStatusFailure::classify(status.as_u16(), &body)));
        }

        let dsml = dsml_enabled_for(&self.provider);
        let mut buffer = Utf8LineBuffer::default();
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
        let mut stream = response.bytes_stream();
        while let Some(chunk) = self
            .next_response_chunk(&mut stream, "responses.stream")
            .await?
        {
            for line in buffer.push(&chunk)? {
                if handle_responses_sse_line(
                    &line,
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
                    &mut *on_chunk,
                )? {
                    return finalize_responses_stream_result(
                        content,
                        reasoning,
                        usage,
                        tool_calls.finish(),
                        dsml,
                        response_id,
                        store_disabled,
                        self.responses_continuation_suppressed(),
                    )
                    .map(Some);
                }
            }
        }
        let mut terminal_event_received = false;
        for line in buffer.finish()? {
            if handle_responses_sse_line(
                &line,
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
                &mut *on_chunk,
            )? {
                terminal_event_received = true;
                break;
            }
        }
        if !terminal_event_received {
            bail!("OpenAI Responses stream ended before a terminal event");
        }
        finalize_responses_stream_result(
            content,
            reasoning,
            usage,
            tool_calls.finish(),
            dsml,
            response_id,
            store_disabled,
            self.responses_continuation_suppressed(),
        )
        .map(Some)
    }

    pub fn uses_openai_responses(&self) -> bool {
        let model = self.provider.default_model.to_ascii_lowercase();
        model.starts_with("gpt-5")
            || model.starts_with("o1")
            || model.starts_with("o3")
            || model.starts_with("o4")
    }

    pub fn uses_anthropic_messages(&self) -> bool {
        provider_looks_anthropic(&self.provider)
    }
}
