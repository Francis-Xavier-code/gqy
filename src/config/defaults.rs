//! defaults — 自 src/config.rs 拆分。

use super::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebImagesPluginConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_web_images_source_mode")]
    pub source_mode: String,
    #[serde(default = "default_web_images_max_results")]
    pub max_results: usize,
    #[serde(default = "default_web_images_max_download_mb")]
    pub max_download_mb: f64,
    #[serde(default = "default_true")]
    pub safe_search: bool,
    #[serde(default = "default_true")]
    pub vision_screening_enabled: bool,
    #[serde(default = "default_true")]
    pub auto_preview: bool,
    #[serde(default = "default_web_images_preview_count")]
    pub preview_count: usize,
    #[serde(default = "default_web_images_timeout")]
    pub timeout_seconds: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeepResearchPluginConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_deep_research_dir")]
    pub output_dir: String,
    #[serde(default = "default_deep_research_depth")]
    pub thinking_depth: String,
    #[serde(default = "default_deep_research_max_review_revisions")]
    pub max_review_revisions: usize,
    #[serde(default = "default_deep_research_max_tool_steps")]
    pub max_tool_steps_per_round: usize,
    #[serde(default)]
    pub max_final_answer_chars: usize,
    #[serde(default = "default_deep_research_tool_timeout")]
    pub tool_call_timeout_seconds: u64,
    #[serde(default = "default_true")]
    pub show_progress: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VisionPluginConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_true")]
    pub prefer_current_multimodal_model: bool,
    #[serde(default)]
    pub vision_provider_id: String,
    #[serde(default)]
    pub vision_model: String,
    #[serde(default = "default_vision_response_header_timeout")]
    pub response_header_timeout_seconds: u64,
    #[serde(default = "default_vision_stream_idle_timeout")]
    pub stream_idle_timeout_seconds: u64,
    #[serde(default = "default_vision_image_timeout")]
    pub image_timeout_seconds: u64,
    #[serde(default = "default_true")]
    pub preview_with_chafa: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExchangeRatePluginConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub api_key: String,
    #[serde(default = "default_true")]
    pub free_fallback_enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageGenerationPluginConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_image_generation_provider_type")]
    pub provider_type: String,
    #[serde(default = "default_openai_images_base_url")]
    pub base_url: String,
    #[serde(default)]
    pub api_keys: Vec<String>,
    #[serde(default = "default_image_generation_model")]
    pub model: String,
    #[serde(default = "default_image_generation_aspect_ratio")]
    pub default_aspect_ratio: String,
    #[serde(default = "default_image_generation_resolution")]
    pub default_resolution: String,
    #[serde(default = "default_image_generation_output_dir")]
    pub output_dir: String,
    #[serde(default)]
    pub auto_print: bool,
    #[serde(default = "default_image_generation_timeout")]
    pub timeout_seconds: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrintImagePluginConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_print_image_width_percent")]
    pub width_percent: u8,
    #[serde(default = "default_print_image_height_percent")]
    pub height_percent: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemesPluginConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub persona_libraries: HashMap<String, String>,
    #[serde(default = "default_memes_width_percent")]
    pub width_percent: u8,
    #[serde(default = "default_memes_height_percent")]
    pub height_percent: u8,
    #[serde(default = "default_memes_max_image_mb")]
    pub max_image_mb: u64,
    #[serde(default = "default_memes_search_max_results")]
    pub search_max_results: usize,
    #[serde(default)]
    pub allow_gif_animation: bool,
    /// 终端/WebUI 会话的自动提示发送表情,默认开。
    #[serde(default = "default_true")]
    pub auto_send_enabled: bool,
    /// 通讯平台会话的自动提示发送表情:与终端/WebUI 的 auto_send_enabled
    /// 独立,默认开——表情包本来就是平台聊天的语言。
    #[serde(default = "default_true")]
    pub auto_send_platform_enabled: bool,
    #[serde(default = "default_memes_auto_send_probability")]
    pub auto_send_probability: f32,
}

/// 手动模型价格(每 1M tokens):目录查不到价的中转/赠送端点用它,
/// 设了就覆盖 models.dev 的价目。缓存价缺省时按输入价计。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ModelCostConfig {
    #[serde(default)]
    pub currency: CostCurrency,
    #[serde(default)]
    pub input: f64,
    #[serde(default)]
    pub output: f64,
    #[serde(default, alias = "cache", skip_serializing_if = "Option::is_none")]
    pub cache_read: Option<f64>,
}

/// 手动价格的币种。统计聚合统一折算成 USD 展示。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum CostCurrency {
    #[default]
    #[serde(rename = "USD", alias = "usd")]
    Usd,
    #[serde(rename = "CNY", alias = "cny", alias = "rmb", alias = "¥")]
    Cny,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeBasePluginConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub data_dir: String,
    #[serde(default = "default_kb_max_search_results")]
    pub max_search_results: usize,
    #[serde(default = "default_kb_snippet_context_chars")]
    pub snippet_context_chars: usize,
    #[serde(default = "default_kb_proximity_window_chars")]
    pub proximity_window_chars: usize,
    #[serde(default = "default_kb_max_read_lines")]
    pub max_read_lines: usize,
    #[serde(default = "default_kb_max_file_size_kb")]
    pub max_file_size_kb: usize,
    #[serde(default = "default_kb_allowed_extensions")]
    pub allowed_extensions: String,
    #[serde(default = "default_kb_allowed_filenames")]
    pub allowed_filenames: String,
    #[serde(default = "default_true")]
    pub upload_tool_enabled: bool,
    #[serde(default = "default_true")]
    pub embedding_enabled: bool,
    #[serde(default)]
    pub embedding_provider_id: String,
    #[serde(default)]
    pub embedding_model: String,
    #[serde(default = "default_kb_semantic_chunk_chars")]
    pub semantic_chunk_chars: usize,
    #[serde(default = "default_kb_semantic_chunk_overlap")]
    pub semantic_chunk_overlap: usize,
    #[serde(default = "default_kb_semantic_top_k")]
    pub semantic_top_k: usize,
    #[serde(default = "default_kb_semantic_min_score")]
    pub semantic_min_score: f32,
    #[serde(default = "default_kb_keyword_strong_score_threshold")]
    pub keyword_strong_score_threshold: f32,
    #[serde(default = "default_kb_embedding_timeout_seconds")]
    pub embedding_timeout_seconds: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalculatorPluginConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_calculator_backend")]
    pub backend: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiagnosticsPluginConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_diagnostics_timeout")]
    pub command_timeout_seconds: u64,
    #[serde(default = "default_diagnostics_max_stdout_chars")]
    pub max_stdout_chars: usize,
    #[serde(default = "default_diagnostics_max_stderr_chars")]
    pub max_stderr_chars: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiQuotaPluginConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub deepseek: ApiQuotaProviderConfig,
    #[serde(default)]
    pub openrouter: ApiQuotaProviderConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiQuotaProviderConfig {
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub accounts: Vec<ApiQuotaAccountConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiQuotaAccountConfig {
    #[serde(default)]
    pub id: String,
    #[serde(default = "default_api_quota_account_name")]
    pub name: String,
    #[serde(default)]
    pub api_key: String,
}

pub(crate) fn default_api_quota_account_name() -> String {
    "默认账号".to_string()
}

pub(crate) fn normalize_api_quota_provider(config: &mut ApiQuotaProviderConfig) {
    let legacy_key = config.api_key.trim().to_string();
    if config.accounts.is_empty() {
        config.accounts.push(ApiQuotaAccountConfig {
            id: "account-1".to_string(),
            name: default_api_quota_account_name(),
            api_key: legacy_key.clone(),
        });
    } else if !legacy_key.is_empty()
        && config
            .accounts
            .iter()
            .all(|account| account.api_key.trim() != legacy_key)
    {
        if config.accounts[0].api_key.trim().is_empty() {
            config.accounts[0].api_key = legacy_key.clone();
        } else if config.accounts.len() < 32 {
            let mut number = 2usize;
            let name = loop {
                let candidate = format!("账号 {number}");
                if config
                    .accounts
                    .iter()
                    .all(|account| account.name != candidate)
                {
                    break candidate;
                }
                number += 1;
            };
            config.accounts.push(ApiQuotaAccountConfig {
                id: String::new(),
                name,
                api_key: legacy_key.clone(),
            });
        }
    }
    if legacy_key.is_empty()
        || config
            .accounts
            .iter()
            .any(|account| account.api_key.trim() == legacy_key)
    {
        config.api_key.clear();
    }
    let mut used_ids = HashSet::with_capacity(config.accounts.len());
    for (index, account) in config.accounts.iter_mut().enumerate() {
        account.name = account.name.trim().to_string();
        if account.name.is_empty() {
            account.name = if index == 0 {
                default_api_quota_account_name()
            } else {
                format!("账号 {}", index + 1)
            };
        }
        if account.id.trim().is_empty() || !used_ids.insert(account.id.clone()) {
            let mut number = index + 1;
            loop {
                let id = format!("account-{number}");
                if used_ids.insert(id.clone()) {
                    account.id = id;
                    break;
                }
                number += 1;
            }
        }
    }
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            config_version: CURRENT_CONFIG_VERSION,
            active_provider: OPENCODE_PROVIDER_ID.to_string(),
            active_provider_models: None,
            active_multimodal_provider_models: None,
            providers: ProviderConfig::default_templates(),
            embedding: EmbeddingConfig::default(),
            context: ContextConfig::default(),
            tools: ToolsConfig::default(),
            cache: CacheConfig::default(),
            mcp: McpConfig::default(),
            skills: SkillsConfig::default(),
            display: DisplayConfig::default(),
            notifications: NotificationsConfig::default(),
            prompt: PromptConfig::default(),
            plugins: PluginsConfig::default(),
            memory: MemoryConfig::default(),
            system_prompt_file: Some("system-prompt.md".to_string()),
            default_mode: String::new(),
            system_prompt: None,
            subagent_tiers: SubagentTiersConfig::default(),
            platforms: PlatformsConfig::default(),
        }
    }
}

impl Default for PromptConfig {
    fn default() -> Self {
        Self {
            prompts_dir: default_prompts_dir(),
            identities_dir: default_identities_dir(),
            user_identity_file: default_user_identity_file(),
            active_persona: String::new(),
            active_identity: String::new(),
            persona_reminder: false,
            persona_reminder_interval: default_persona_reminder_interval(),
        }
    }
}

impl Default for DisplayConfig {
    fn default() -> Self {
        Self {
            language: default_display_language(),
            reasoning: default_reasoning_display(),
            tool_calls: default_tool_call_display(),
            readable_tool_names: default_true(),
            show_token_usage: false,
            mixed_model_endpoint_display: default_mixed_model_endpoint_display(),
            command_output_lines: default_command_output_lines(),
            repl_replay_turns: default_repl_replay_turns(),
        }
    }
}

impl Default for PluginsConfig {
    fn default() -> Self {
        Self {
            weather: PluginEnabledConfig::default(),
            web: WebPluginConfig::default(),
            web_images: WebImagesPluginConfig::default(),
            deep_research: DeepResearchPluginConfig::default(),
            vision: VisionPluginConfig::default(),
            exchange_rate: ExchangeRatePluginConfig::default(),
            xuanxue: PluginEnabledConfig::default(),
            image_generation: ImageGenerationPluginConfig::default(),
            print_image: PrintImagePluginConfig::default(),
            memes: MemesPluginConfig::default(),
            knowledge_base: KnowledgeBasePluginConfig::default(),
            brew: PluginEnabledConfig::default(),
            man: PluginEnabledConfig::default(),
            moegirl: PluginEnabledConfig::default(),
            hash_codec: PluginEnabledConfig::default(),
            calculator: CalculatorPluginConfig::default(),
            package_advisor: PluginEnabledConfig::default(),
            diagnostics: DiagnosticsPluginConfig::default(),
            api_quota: ApiQuotaPluginConfig::default(),
            memory: MemoryConfig::default(),
        }
    }
}

impl Default for ApiQuotaPluginConfig {
    fn default() -> Self {
        Self {
            enabled: default_true(),
            deepseek: ApiQuotaProviderConfig::default(),
            openrouter: ApiQuotaProviderConfig::default(),
        }
    }
}

impl Default for ApiQuotaProviderConfig {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            accounts: vec![ApiQuotaAccountConfig {
                id: "account-1".to_string(),
                name: default_api_quota_account_name(),
                api_key: String::new(),
            }],
        }
    }
}

impl Default for McpConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            servers: Vec::new(),
        }
    }
}

impl Default for PluginEnabledConfig {
    fn default() -> Self {
        Self {
            enabled: default_true(),
        }
    }
}

impl Default for WebPluginConfig {
    fn default() -> Self {
        Self {
            enabled: default_true(),
            max_results: default_web_search_max_results(),
            tavily_api_keys: Vec::new(),
            firecrawl_api_keys: Vec::new(),
            anysearch_api_keys: Vec::new(),
            exa_api_keys: Vec::new(),
            searxng_base_url: String::new(),
        }
    }
}

impl Default for WebImagesPluginConfig {
    fn default() -> Self {
        Self {
            enabled: default_true(),
            source_mode: default_web_images_source_mode(),
            max_results: default_web_images_max_results(),
            max_download_mb: default_web_images_max_download_mb(),
            safe_search: default_true(),
            vision_screening_enabled: default_true(),
            auto_preview: default_true(),
            preview_count: default_web_images_preview_count(),
            timeout_seconds: default_web_images_timeout(),
        }
    }
}

impl Default for DeepResearchPluginConfig {
    fn default() -> Self {
        Self {
            enabled: default_true(),
            output_dir: default_deep_research_dir(),
            thinking_depth: default_deep_research_depth(),
            max_review_revisions: default_deep_research_max_review_revisions(),
            max_tool_steps_per_round: default_deep_research_max_tool_steps(),
            max_final_answer_chars: 0,
            tool_call_timeout_seconds: default_deep_research_tool_timeout(),
            show_progress: default_true(),
        }
    }
}

impl Default for VisionPluginConfig {
    fn default() -> Self {
        Self {
            enabled: default_true(),
            prefer_current_multimodal_model: default_true(),
            vision_provider_id: String::new(),
            vision_model: String::new(),
            response_header_timeout_seconds: default_vision_response_header_timeout(),
            stream_idle_timeout_seconds: default_vision_stream_idle_timeout(),
            image_timeout_seconds: default_vision_image_timeout(),
            preview_with_chafa: default_true(),
        }
    }
}

impl Default for ExchangeRatePluginConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            api_key: String::new(),
            free_fallback_enabled: default_true(),
        }
    }
}

impl Default for ImageGenerationPluginConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            provider_type: default_image_generation_provider_type(),
            base_url: default_openai_images_base_url(),
            api_keys: Vec::new(),
            model: default_image_generation_model(),
            default_aspect_ratio: default_image_generation_aspect_ratio(),
            default_resolution: default_image_generation_resolution(),
            output_dir: default_image_generation_output_dir(),
            auto_print: default_true(),
            timeout_seconds: default_image_generation_timeout(),
        }
    }
}

impl Default for PrintImagePluginConfig {
    fn default() -> Self {
        Self {
            enabled: default_true(),
            width_percent: default_print_image_width_percent(),
            height_percent: default_print_image_height_percent(),
        }
    }
}

impl Default for MemesPluginConfig {
    fn default() -> Self {
        Self {
            enabled: default_true(),
            persona_libraries: HashMap::new(),
            width_percent: default_memes_width_percent(),
            height_percent: default_memes_height_percent(),
            max_image_mb: default_memes_max_image_mb(),
            search_max_results: default_memes_search_max_results(),
            allow_gif_animation: false,
            auto_send_enabled: true,
            auto_send_platform_enabled: true,
            auto_send_probability: default_memes_auto_send_probability(),
        }
    }
}

impl MemesPluginConfig {
    pub fn library_for_persona(&self, persona: &str) -> String {
        if persona.trim().is_empty() {
            return self
                .persona_libraries
                .get("default")
                .cloned()
                .unwrap_or_else(|| "gqy".to_string());
        }
        let persona = persona_scope_name(persona);
        self.persona_libraries
            .get(&persona)
            .cloned()
            .unwrap_or(persona)
    }
}

impl Default for KnowledgeBasePluginConfig {
    fn default() -> Self {
        Self {
            enabled: default_true(),
            data_dir: String::new(),
            max_search_results: default_kb_max_search_results(),
            snippet_context_chars: default_kb_snippet_context_chars(),
            proximity_window_chars: default_kb_proximity_window_chars(),
            max_read_lines: default_kb_max_read_lines(),
            max_file_size_kb: default_kb_max_file_size_kb(),
            allowed_extensions: default_kb_allowed_extensions(),
            allowed_filenames: default_kb_allowed_filenames(),
            upload_tool_enabled: default_true(),
            embedding_enabled: false,
            embedding_provider_id: String::new(),
            embedding_model: String::new(),
            semantic_chunk_chars: default_kb_semantic_chunk_chars(),
            semantic_chunk_overlap: default_kb_semantic_chunk_overlap(),
            semantic_top_k: default_kb_semantic_top_k(),
            semantic_min_score: default_kb_semantic_min_score(),
            keyword_strong_score_threshold: default_kb_keyword_strong_score_threshold(),
            embedding_timeout_seconds: default_kb_embedding_timeout_seconds(),
        }
    }
}

impl Default for CalculatorPluginConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            backend: default_calculator_backend(),
        }
    }
}

impl Default for DiagnosticsPluginConfig {
    fn default() -> Self {
        Self {
            enabled: default_true(),
            command_timeout_seconds: default_diagnostics_timeout(),
            max_stdout_chars: default_diagnostics_max_stdout_chars(),
            max_stderr_chars: default_diagnostics_max_stderr_chars(),
        }
    }
}

impl Default for ToolsConfig {
    fn default() -> Self {
        Self {
            enabled: default_true(),
            max_rounds: 0,
            loading_mode: default_tools_loading_mode(),
            persist_loaded_tools: default_true(),
            subagent_concurrency: default_subagent_concurrency(),
            default_timeout_secs: default_tools_timeout_secs(),
            command_deny: default_command_deny(),
        }
    }
}

impl Default for SkillsConfig {
    fn default() -> Self {
        Self {
            enabled: default_true(),
            allow_command_execution: default_true(),
        }
    }
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            enabled: default_true(),
            evicted_context_enabled: default_true(),
            association_enabled: default_true(),
            auto_diary_enabled: default_true(),
            auto_fact_enabled: default_true(),
            diary_batch_size: default_memory_diary_batch_size(),
            short_diary_retention_days: default_memory_short_diary_retention_days(),
            diary_promotion_recalls: default_memory_diary_promotion_recalls(),
            organizer_timeout_seconds: default_memory_organizer_timeout_seconds(),
            auto_skill_enabled: false,
            association_facts: default_memory_association_facts(),
            association_episodes: default_memory_association_episodes(),
            association_max_chars: default_memory_association_max_chars(),
            association_dedup: default_true(),
            snippet_chars: default_memory_snippet_chars(),
            forget_after_days: default_memory_forget_after_days(),
            forgetting_enabled: default_true(),
            forgetting_half_life_days: default_memory_half_life_days(),
            forgetting_min_strength: default_memory_min_strength(),
            forgetting_review_boost: default_memory_review_boost(),
            learning_min_task_chars: default_memory_min_task_chars(),
            learning_min_method_chars: default_memory_min_method_chars(),
        }
    }
}

impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            tool_output_spill_bytes: default_tool_output_spill_bytes(),
            trim_at_ratio: default_trim_at_ratio(),
            trim_batch_ratio: default_trim_batch_ratio(),
            on_overflow: default_on_overflow(),
            default_context_window: default_context_window(),
            compact_force_ratio: default_compact_force_ratio(),
            compact_tail_tokens: None,
            compact_soft_ratio: default_compact_soft_ratio(),
            compact_snip_ratio: default_compact_snip_ratio(),
            prune_stale_tool_reports: true,
            cold_prune_after_minutes: default_cold_prune_after_minutes(),
            compact_cache_reuse: true,
        }
    }
}

impl ProviderConfig {
    /// 当前选中模型(`default_model`)的有效温度:按模型覆盖优先,缺项
    /// 回退供应商默认。
    pub fn effective_temperature(&self) -> f32 {
        self.model_temperature
            .get(&self.default_model)
            .copied()
            .unwrap_or(self.temperature)
    }

    pub fn default_opencodezen() -> Self {
        Self {
            id: OPENCODE_PROVIDER_ID.to_string(),
            display_name: "opencode Zen".to_string(),
            base_url: OPENCODE_ZEN_BASE_URL.to_string(),
            protocol: default_provider_protocol(),
            api_key: None,
            models: vec![OPENCODE_DEFAULT_CHAT_MODEL.to_string()],
            model_context_window: HashMap::new(),
            model_temperature: HashMap::new(),
            model_modalities: HashMap::new(),
            model_costs: HashMap::new(),
            default_model: OPENCODE_DEFAULT_CHAT_MODEL.to_string(),
            timeout_seconds: default_timeout(),
            temperature: default_temperature(),
            anthropic_max_tokens: default_anthropic_max_tokens(),
            extra_body: None,
        }
    }

    pub fn default_anthropic() -> Self {
        Self {
            id: "anthropic".to_string(),
            display_name: "Anthropic".to_string(),
            base_url: "https://api.anthropic.com/v1".to_string(),
            protocol: "anthropic".to_string(),
            api_key: Some("$env:ANTHROPIC_API_KEY".to_string()),
            models: Vec::new(),
            model_context_window: HashMap::new(),
            model_temperature: HashMap::new(),
            model_modalities: HashMap::new(),
            model_costs: HashMap::new(),
            default_model: String::new(),
            timeout_seconds: default_timeout(),
            temperature: default_temperature(),
            anthropic_max_tokens: default_anthropic_max_tokens(),
            extra_body: None,
        }
    }

    pub fn default_templates() -> Vec<Self> {
        let mut providers = vec![Self::default_opencodezen()];
        providers.extend([
            Self::template("opencodego", "OpenCode Go", "https://opencode.ai/zen/go/v1"),
            Self::template("openai", "OpenAI", "https://api.openai.com/v1"),
            Self::default_anthropic(),
            Self::template("deepseek", "DeepSeek", "https://api.deepseek.com"),
            Self::template(
                "gemini",
                "Gemini",
                "https://generativelanguage.googleapis.com/v1beta/openai",
            ),
            Self::template(
                "xiaomi",
                "Xiaomi",
                "https://token-plan-sgp.xiaomimimo.com/v1",
            ),
            Self::template("minimax", "Minimax", "https://api.minimaxi.com/v1"),
            Self::template("openrouter", "OpenRouter", "https://openrouter.ai/api/v1"),
            Self::template("ollama", "Ollama", "http://localhost:11434/v1"),
            Self::template("lmstudio", "LMStudio", "http://localhost:1234/v1"),
        ]);
        providers
    }

    pub(crate) fn template(id: &str, display_name: &str, base_url: &str) -> Self {
        Self {
            id: id.to_string(),
            display_name: display_name.to_string(),
            base_url: base_url.to_string(),
            protocol: default_provider_protocol(),
            api_key: None,
            models: Vec::new(),
            model_context_window: HashMap::new(),
            model_temperature: HashMap::new(),
            model_modalities: HashMap::new(),
            model_costs: HashMap::new(),
            default_model: String::new(),
            timeout_seconds: default_timeout(),
            temperature: default_temperature(),
            anthropic_max_tokens: default_anthropic_max_tokens(),
            extra_body: None,
        }
    }

    pub fn new_custom() -> Self {
        Self {
            id: String::new(),
            display_name: String::new(),
            base_url: String::new(),
            protocol: default_provider_protocol(),
            api_key: None,
            models: Vec::new(),
            model_context_window: HashMap::new(),
            model_temperature: HashMap::new(),
            model_modalities: HashMap::new(),
            model_costs: HashMap::new(),
            default_model: String::new(),
            timeout_seconds: default_timeout(),
            temperature: default_temperature(),
            anthropic_max_tokens: default_anthropic_max_tokens(),
            extra_body: None,
        }
    }

    pub fn supports_vision(&self, model: &str) -> Option<bool> {
        self.input_modalities(model)
            .map(|modalities| modalities.iter().any(|m| m == "image"))
    }

    pub fn input_modalities(&self, model: &str) -> Option<Vec<String>> {
        if let Some(modalities) = self.model_modalities.get(model) {
            return Some(modalities.clone());
        }
        crate::models_cache::input_modalities(&self.id, model)
    }

    pub fn resolved_api_keys(&self, _paths: &GQYPaths) -> Result<Vec<ResolvedProviderKey>> {
        let mut keys = Vec::new();
        if let Some(api_key) = self.api_key.as_deref() {
            append_resolved_api_keys(&mut keys, api_key)?;
        }

        if keys.is_empty() && self.is_opencode_zen() {
            keys.push(ResolvedProviderKey {
                index: 0,
                value: "public".to_string(),
            });
        }

        if keys.is_empty() {
            bail!("missing API key for provider {}", self.id)
        }
        for (index, key) in keys.iter_mut().enumerate() {
            key.index = index;
        }
        Ok(keys)
    }

    pub fn is_opencode_zen(&self) -> bool {
        matches!(self.id.as_str(), OPENCODE_PROVIDER_ID | "opencodezen")
            && self.base_url.trim_end_matches('/') == OPENCODE_ZEN_BASE_URL
    }

    pub(crate) fn has_configured_model(&self, model: &str) -> bool {
        let model = model.trim();
        !model.is_empty()
            && (self.default_model == model || self.models.iter().any(|item| item == model))
    }

    pub(crate) fn is_legacy_default_anthropic_model(&self) -> bool {
        self.id == "anthropic"
            && self.base_url.trim_end_matches('/') == "https://api.anthropic.com/v1"
            && self.protocol == "anthropic"
            && self.api_key.as_deref() == Some("$env:ANTHROPIC_API_KEY")
            && self.models == ["claude-sonnet-4-5"]
            && self.default_model == "claude-sonnet-4-5"
    }
}

pub(crate) fn append_resolved_api_keys(
    out: &mut Vec<ResolvedProviderKey>,
    raw: &str,
) -> Result<()> {
    for item in split_api_keys(raw) {
        let value = if let Some(env_name) = item.strip_prefix("$env:") {
            std::env::var(env_name)
                .with_context(|| format!("environment variable {env_name} is not set"))?
        } else {
            item.to_string()
        };
        let value = value.trim();
        if !value.is_empty() {
            out.push(ResolvedProviderKey {
                index: out.len(),
                value: value.to_string(),
            });
        }
    }
    Ok(())
}

pub(crate) fn split_api_keys(raw: &str) -> Vec<&str> {
    raw.lines()
        .flat_map(|line| line.split(','))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .collect()
}

pub(crate) fn active_model_exists(
    providers: &[ProviderConfig],
    active: &ActiveProviderModelConfig,
) -> bool {
    providers
        .iter()
        .find(|provider| provider.id == active.provider_id.trim())
        .is_some_and(|provider| provider.has_configured_model(&active.model))
}

pub(crate) fn active_model_supports_image(
    providers: &[ProviderConfig],
    active: &ActiveProviderModelConfig,
) -> bool {
    providers
        .iter()
        .find(|provider| provider.id == active.provider_id.trim())
        .filter(|provider| provider.has_configured_model(&active.model))
        .and_then(|provider| provider.input_modalities(&active.model))
        .is_some_and(|modalities| modalities.iter().any(|input| input == "image"))
}

pub(crate) fn validate_unique_existing_pool(
    providers: &[ProviderConfig],
    label: &str,
    pool: &[ActiveProviderModelConfig],
    require_image: bool,
) -> Result<()> {
    let mut seen = HashSet::with_capacity(pool.len());
    for entry in pool {
        if !seen.insert((entry.provider_id.as_str(), entry.model.as_str())) {
            bail!(
                "duplicate {label} model: {} / {}",
                entry.provider_id,
                entry.model
            );
        }
        let valid = if require_image {
            active_model_supports_image(providers, entry)
        } else {
            active_model_exists(providers, entry)
        };
        if !valid {
            let requirement = if require_image {
                "configured image-capable"
            } else {
                "configured"
            };
            bail!(
                "unknown or non-{requirement} {label} model: {} / {}",
                entry.provider_id,
                entry.model
            );
        }
    }
    Ok(())
}

pub(crate) fn is_positive_decimal_id(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| byte.is_ascii_digit())
        && value.parse::<u64>().is_ok_and(|id| id > 0)
}
