//! client_impl — 自 src/llm/openai_compatible.rs 拆分。

#![allow(clippy::obfuscated_if_else)]
pub(crate) use super::*;

impl OpenAiCompatibleClient {
    /// 当前主 provider id,用量历史记账用(具体模型以 ChatResult 为准)。
    pub fn provider_id(&self) -> &str {
        &self.provider.id
    }

    /// 该端点的 Responses 续传是否已被记为不可用(记录或本进程自愈置位)。
    pub fn responses_continuation_suppressed(&self) -> bool {
        self.continuation_health
            .unsupported
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// 自愈标记:上游拒了 previous_response_id(任务#16 签名 400)。进程内
    /// 立即生效并持久化;首个标记者负责落盘,后续幂等。
    pub fn mark_responses_continuation_unsupported(&self) {
        let health = &self.continuation_health;
        if !health
            .unsupported
            .swap(true, std::sync::atomic::Ordering::Relaxed)
        {
            if !health.base_url.is_empty() {
                crate::llm::provider_capabilities::record_continuation_unsupported(
                    &health.store,
                    &health.base_url,
                );
            }
            tracing::warn!(
                provider = %health.provider_id,
                "responses continuation rejected upstream; falling back to stateless full replay for this provider"
            );
        }
    }

    pub fn from_config(config: &AppConfig, paths: &GQYPaths) -> Result<Self> {
        crate::llm::cache_log::configure(paths, &config.cache);
        let endpoints = llm_endpoints(config, paths)?;
        let first = endpoints
            .first()
            .with_context(|| "no active provider/model endpoint is configured")?;
        let continuation_health = ResponsesContinuationHealth::for_provider(paths, &first.provider);
        let mut client = Self {
            client: first.client.clone(),
            provider: first.provider.clone(),
            api_key: first.api_key.clone(),
            endpoints: Arc::new(endpoints),
            thinking_variants: HashMap::new(),
            reasoning_visibility: reasoning_visibility(config),
            buffered_delivery: false,
            detailed_reasoning_summary: reasoning_summary_is_detailed(config),
            request_timeouts: None,
            max_tokens_override: None,
            request_scope: "chat",
            continuation_health,
        };
        client.restore_saved_thinking_variants(paths);
        Ok(client)
    }

    /// Builds a client over an explicit provider/model pool (e.g. a
    /// subagent tier pool). Requests load-balance across the pool through
    /// the shared endpoint scheduler, exactly like the main model pool.
    pub fn from_choices(
        config: &AppConfig,
        paths: &GQYPaths,
        choices: &[crate::config::ProviderModelChoice],
    ) -> Result<Self> {
        crate::llm::cache_log::configure(paths, &config.cache);
        let mut endpoints = Vec::new();
        let mut errors = Vec::new();
        for choice in choices {
            let mut provider = match config.provider(Some(&choice.provider_id)) {
                Ok(provider) => provider.clone(),
                Err(err) => {
                    errors.push(format!("{} / {}: {err}", choice.provider_id, choice.model));
                    continue;
                }
            };
            provider.default_model = choice.model.clone();
            let client = endpoint_client(&provider)?;
            match provider.resolved_api_keys(paths) {
                Ok(keys) => {
                    for key in keys {
                        endpoints.push(LlmEndpoint {
                            client: client.clone(),
                            provider: provider.clone(),
                            api_key: key.value,
                            key_index: key.index,
                        });
                    }
                }
                Err(err) => errors.push(format!(
                    "{} / {}: {err}",
                    provider.id, provider.default_model
                )),
            }
        }
        let first = match endpoints.first() {
            Some(first) => first,
            None => bail!(
                "no usable endpoint in the model pool:\n- {}",
                errors.join("\n- ")
            ),
        };
        let continuation_health = ResponsesContinuationHealth::for_provider(paths, &first.provider);
        let mut client = Self {
            client: first.client.clone(),
            provider: first.provider.clone(),
            api_key: first.api_key.clone(),
            endpoints: Arc::new(endpoints),
            thinking_variants: HashMap::new(),
            reasoning_visibility: reasoning_visibility(config),
            buffered_delivery: false,
            detailed_reasoning_summary: reasoning_summary_is_detailed(config),
            request_timeouts: None,
            max_tokens_override: None,
            request_scope: "chat",
            continuation_health,
        };
        client.restore_saved_thinking_variants(paths);
        Ok(client)
    }

    pub fn new(provider: &ProviderConfig, config: &AppConfig, paths: &GQYPaths) -> Result<Self> {
        if provider.default_model.trim().is_empty() {
            bail!(
                "{}: {}",
                t(
                    "provider has no active model; select a model before chatting",
                    "provider 没有当前模型；请先选择模型再聊天",
                ),
                provider.id
            );
        }
        let client = endpoint_client(provider)?;
        let key = provider
            .resolved_api_keys(paths)?
            .into_iter()
            .next()
            .with_context(|| format!("missing API key for provider {}", provider.id))?;
        let endpoint = LlmEndpoint {
            client: client.clone(),
            provider: provider.clone(),
            api_key: key.value.clone(),
            key_index: key.index,
        };
        let continuation_health = ResponsesContinuationHealth::for_provider(paths, provider);
        let mut client = Self {
            client,
            provider: provider.clone(),
            api_key: key.value,
            endpoints: Arc::new(vec![endpoint]),
            thinking_variants: HashMap::new(),
            reasoning_visibility: reasoning_visibility(config),
            buffered_delivery: false,
            detailed_reasoning_summary: reasoning_summary_is_detailed(config),
            request_timeouts: None,
            max_tokens_override: None,
            request_scope: "chat",
            continuation_health,
        };
        client.restore_saved_thinking_variants(paths);
        Ok(client)
    }

    pub fn context_window(&self, config: &AppConfig) -> Result<Option<usize>> {
        let choices = self.endpoint_model_choices();
        let mut windows = Vec::with_capacity(choices.len());
        for (provider_id, model) in choices {
            let Some(window) = config.context_window_for_provider_model(&provider_id, &model)?
            else {
                return Ok(None);
            };
            windows.push(window);
        }
        Ok(windows.into_iter().min())
    }

    /// Marks a client whose caller collects output and delivers it in one
    /// piece. A truncated stream can then be retried without the person
    /// seeing the false start.
    pub fn with_buffered_delivery(mut self, buffered: bool) -> Self {
        self.buffered_delivery = buffered;
        self
    }

    pub fn for_subagent_output(mut self, full: bool) -> Self {
        self.reasoning_visibility = if full {
            ReasoningVisibility::Full
        } else {
            ReasoningVisibility::Hidden
        };
        self.detailed_reasoning_summary = full;
        self
    }

    pub fn with_request_timeouts(
        mut self,
        response_header: Duration,
        stream_idle: Duration,
    ) -> Self {
        self.request_timeouts = Some(RequestTimeouts {
            response_header: response_header.max(Duration::from_millis(1)),
            stream_idle: stream_idle.max(Duration::from_millis(1)),
        });
        self
    }

    pub fn models_without_context_window(&self, config: &AppConfig) -> Vec<String> {
        self.endpoint_model_choices()
            .into_iter()
            .filter(|(provider_id, model)| {
                config
                    .context_window_for_provider_model(provider_id, model)
                    .ok()
                    .flatten()
                    .is_none()
            })
            .map(|(provider_id, model)| format!("{provider_id} / {model}"))
            .collect()
    }

    pub fn endpoint_model_choices(&self) -> BTreeSet<(String, String)> {
        self.endpoints
            .iter()
            .map(|endpoint| {
                (
                    endpoint.provider.id.clone(),
                    endpoint.provider.default_model.clone(),
                )
            })
            .collect()
    }

    pub fn with_endpoint(&self, endpoint: &LlmEndpoint) -> Self {
        Self {
            client: endpoint.client.clone(),
            provider: endpoint.provider.clone(),
            api_key: endpoint.api_key.clone(),
            endpoints: self.endpoints.clone(),
            thinking_variants: self.thinking_variants.clone(),
            reasoning_visibility: self.reasoning_visibility,
            buffered_delivery: self.buffered_delivery,
            detailed_reasoning_summary: self.detailed_reasoning_summary,
            request_timeouts: self.request_timeouts,
            max_tokens_override: self.max_tokens_override,
            request_scope: self.request_scope,
            // failover 换端点共享同一健康位(续传本就钉在原端点)。
            continuation_health: self.continuation_health.clone(),
        }
    }

    /// Returns a clone whose chat completions are capped at `max_tokens`.
    pub fn with_request_scope(mut self, scope: &'static str) -> Self {
        self.request_scope = scope;
        self
    }

    pub fn with_max_tokens(&self, max_tokens: u32) -> Self {
        let mut clone = self.clone();
        clone.max_tokens_override = Some(max_tokens.max(1));
        clone
    }

    pub fn available_thinking_variants(&self) -> Vec<String> {
        let options = self.thinking_variant_options();
        (options.len() == 1)
            .then(|| options[0].variants.clone())
            .unwrap_or_default()
    }

    pub fn set_thinking_variant(&mut self, variant: Option<String>) -> Result<()> {
        let options = self.thinking_variant_options();
        if options.len() != 1 {
            bail!("a model must be specified when multiple models are active");
        }
        let option = &options[0];
        self.set_thinking_variants(&[(option.provider_id.clone(), option.model.clone(), variant)])
    }

    pub fn set_thinking_variants(
        &mut self,
        selections: &[(String, String, Option<String>)],
    ) -> Result<()> {
        let options = self.thinking_variant_options();
        for (provider_id, model, selected) in selections {
            let option = options
                .iter()
                .find(|option| option.provider_id == *provider_id && option.model == *model)
                .ok_or_else(|| anyhow::anyhow!("inactive model: {provider_id} / {model}"))?;
            if let Some(selected) = selected {
                if !option.variants.iter().any(|variant| variant == selected) {
                    bail!(
                        "thinking variant is unavailable for {provider_id} / {model}: {selected}"
                    );
                }
            }
        }
        for (provider_id, model, selected) in selections {
            let key = thinking_variant_key(provider_id, model);
            if let Some(selected) = selected.as_ref().filter(|value| !value.trim().is_empty()) {
                self.thinking_variants.insert(key, selected.clone());
            } else {
                self.thinking_variants.remove(&key);
            }
        }
        Ok(())
    }

    pub fn restore_thinking_variants(&mut self, selections: &[(String, String, String)]) {
        let active = self.endpoint_model_preferences();
        for (provider_id, model, selected) in selections {
            if active.iter().any(|(active_provider, active_model)| {
                active_provider == provider_id && active_model == model
            }) {
                self.thinking_variants
                    .insert(thinking_variant_key(provider_id, model), selected.clone());
            }
        }
    }

    pub fn restore_saved_thinking_variants(&mut self, paths: &GQYPaths) {
        crate::llm::request_log::install_dir(paths.logs_dir());
        let preferences = load_thinking_variant_preferences(paths);
        let selections = self
            .endpoint_model_preferences()
            .into_iter()
            .filter_map(|(provider_id, model)| {
                let selected = preferences
                    .selected(&provider_id, &model)
                    .map(str::to_string)?;
                Some((provider_id, model, selected))
            })
            .collect::<Vec<_>>();
        self.restore_thinking_variants(&selections);
    }

    pub fn save_thinking_variants(&self, paths: &GQYPaths) -> Result<()> {
        let mut preferences = load_thinking_variant_preferences(paths);
        for (provider_id, model) in self.endpoint_model_preferences() {
            let key = thinking_variant_key(&provider_id, &model);
            preferences.set(
                &provider_id,
                &model,
                self.thinking_variants.get(&key).cloned(),
            );
        }
        preferences.save(paths)
    }

    pub fn thinking_variant_options(&self) -> Vec<ThinkingVariantOptions> {
        self.endpoint_model_preferences()
            .into_iter()
            .filter_map(|(provider_id, model)| {
                let provider = &self
                    .endpoints
                    .iter()
                    .find(|endpoint| {
                        endpoint.provider.id == provider_id
                            && endpoint.provider.default_model == model
                    })?
                    .provider;
                let selected = self
                    .thinking_variants
                    .get(&thinking_variant_key(&provider_id, &model))
                    .map(String::as_str);
                Some(thinking_variant_options_for_model(
                    provider, &model, selected,
                ))
            })
            .collect()
    }

    pub fn thinking_variant_summary(&self) -> Option<String> {
        let options = self.thinking_variant_options();
        let mut variants = options.iter().map(|option| option.selected.as_deref());
        let first = variants.next()?;
        if variants.all(|variant| variant == first) {
            first.map(str::to_string)
        } else {
            Some("mixed".to_string())
        }
    }

    pub fn thinking_variant_for(&self, provider_id: &str, model: &str) -> Option<String> {
        self.thinking_variant_options()
            .into_iter()
            .find(|options| options.provider_id == provider_id && options.model == model)
            .and_then(|options| options.selected)
    }

    pub fn endpoint_model_preferences(&self) -> Vec<(String, String)> {
        self.endpoints
            .iter()
            .map(|endpoint| {
                (
                    endpoint.provider.id.clone(),
                    endpoint.provider.default_model.clone(),
                )
            })
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }
}
