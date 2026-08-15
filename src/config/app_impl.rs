//! app_impl — 自 src/config.rs 拆分。

use super::*;

mod app_impl2;
use app_impl2::*;
impl AppConfig {
    pub fn display_language_hint(paths: &GQYPaths) -> Option<String> {
        let raw = std::fs::read_to_string(&paths.config_file).ok()?;
        let stripped = json_comments::StripComments::new(raw.as_bytes());
        let value: serde_json::Value = serde_json::from_reader(stripped).ok()?;
        value
            .get("display")?
            .get("language")?
            .as_str()
            .map(str::to_string)
    }

    pub fn memory_config(&self) -> &MemoryConfig {
        if self.memory != MemoryConfig::default() {
            &self.memory
        } else {
            &self.plugins.memory
        }
    }

    pub fn load(paths: &GQYPaths) -> Result<Self> {
        // Platform multimodal routes may rely on cached models.dev
        // capabilities. Load the full cache before validation; callers can
        // compact it to their active configuration afterwards.
        crate::models_cache::try_load(paths);
        let raw = std::fs::read_to_string(&paths.config_file)
            .with_context(|| format!("failed to read {}", paths.config_file.display()))?;
        let stripped = json_comments::StripComments::new(raw.as_bytes());
        let mut config: Self = serde_json::from_reader(stripped)
            .with_context(|| format!("invalid JSONC in {}", paths.config_file.display()))?;
        config.migrate()?;
        config.normalize_builtin_providers();
        config.normalize_api_quota_accounts();
        config.normalize_managed_output_paths(paths);
        config.normalize_platform_model_routes();
        config.validate()?;
        config.validate_persona_files(paths)?;
        Ok(config)
    }

    pub fn load_or_default(paths: &GQYPaths) -> Result<Self> {
        if paths.config_file.exists() {
            Self::load(paths)
        } else {
            Ok(Self::default())
        }
    }

    pub fn init_files(paths: &GQYPaths) -> Result<()> {
        paths.create_dirs()?;
        if !paths.config_file.exists() {
            Self::default().save(paths)?;
        }
        // Dev 模式提示词:一行、可编辑、不混淆(与 GQY 人格提示词的内嵌
        // 不可编辑形成对照)。缺失时写默认;用户改成什么都以文件为准。
        let dev_prompt = paths.config_dir.join(DEV_PROMPT_FILE);
        if !dev_prompt.exists() {
            std::fs::write(&dev_prompt, format!("{DEFAULT_DEV_SYSTEM_PROMPT}\n"))?;
        }
        Ok(())
    }

    /// Dev 模式系统提示词:读 `config/dev-prompt.md`,缺失或清空回退内置
    /// 默认一行(极简原则 + 贴近训练分布的措辞,见 08-15 实验记录)。
    pub fn dev_system_prompt(&self, paths: &GQYPaths) -> Result<String> {
        let path = paths.config_dir.join(DEV_PROMPT_FILE);
        match std::fs::read_to_string(&path) {
            Ok(content) if !content.trim().is_empty() => Ok(content.trim().to_string()),
            _ => Ok(DEFAULT_DEV_SYSTEM_PROMPT.to_string()),
        }
    }

    pub fn save(&self, paths: &GQYPaths) -> Result<()> {
        let mut config = self.clone();
        config.migrate()?;
        config.normalize_api_quota_accounts();
        config.normalize_platform_model_routes();
        // Also on save, not just on load: a value healed only in memory is
        // rewritten stale on the next write, so the file never recovers.
        config.normalize_managed_output_paths(paths);
        let effective_memory = config.memory_config().clone();
        config.plugins.memory = effective_memory;
        config.memory = MemoryConfig::default();
        config.validate()?;
        paths.create_dirs()?;
        if let Some(prompt) = config.system_prompt.take() {
            let prompt_file = config.system_prompt_path(paths);
            if let Some(parent) = prompt_file.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let prompt = prompt.trim_end();
            let content = if prompt.is_empty() {
                String::new()
            } else {
                format!("{prompt}\n")
            };
            std::fs::write(prompt_file, content)?;
        }
        if config
            .system_prompt_file
            .as_deref()
            .map(str::trim)
            .unwrap_or_default()
            .is_empty()
        {
            config.system_prompt_file = Some("system-prompt.md".to_string());
        }
        let raw = serde_json::to_string_pretty(&config)?;
        std::fs::write(&paths.config_file, format!("{raw}\n"))?;
        Ok(())
    }

    pub fn migrate(&mut self) -> Result<()> {
        if self.config_version > CURRENT_CONFIG_VERSION {
            bail!(
                "unsupported config version {}; maximum supported version is {}",
                self.config_version,
                CURRENT_CONFIG_VERSION
            );
        }
        if self.config_version < 1 {
            for provider in &mut self.providers {
                if (provider.temperature - LEGACY_DEFAULT_TEMPERATURE).abs() < f32::EPSILON {
                    provider.temperature = default_temperature();
                }
            }
        }
        // The embedding model used to live under the knowledge base, which is
        // where it was first needed. It now also backs memory recall, and a
        // knowledge-base setting silently steering group-chat search is a trap
        // for whoever reads this next.
        if !self.embedding.is_configured() {
            let kb = &self.plugins.knowledge_base;
            if !kb.embedding_provider_id.trim().is_empty() && !kb.embedding_model.trim().is_empty()
            {
                self.embedding.provider_id = kb.embedding_provider_id.trim().to_string();
                self.embedding.model = kb.embedding_model.trim().to_string();
                if kb.embedding_timeout_seconds > 0 {
                    self.embedding.timeout_seconds = kb.embedding_timeout_seconds;
                }
                self.embedding.min_score = kb.semantic_min_score;
            }
        }
        self.config_version = CURRENT_CONFIG_VERSION;
        Ok(())
    }

    pub fn normalize_builtin_providers(&mut self) {
        for provider in ProviderConfig::default_templates() {
            if !self.providers.iter().any(|item| {
                item.id == provider.id
                    || provider.id == OPENCODE_PROVIDER_ID && item.is_opencode_zen()
            }) {
                self.providers.push(provider);
            }
        }
        if self.active_provider == "opencodezen" {
            self.active_provider = OPENCODE_PROVIDER_ID.to_string();
        }
        for provider in &mut self.providers {
            if provider.is_legacy_default_anthropic_model() {
                provider.models.clear();
                provider.default_model.clear();
            }
        }
        if let Some(active_models) = &mut self.active_provider_models {
            for active in active_models {
                if active.provider_id == "opencodezen" {
                    active.provider_id = OPENCODE_PROVIDER_ID.to_string();
                }
            }
        }
        if let Some(active_models) = &mut self.active_multimodal_provider_models {
            for active in active_models {
                if active.provider_id == "opencodezen" {
                    active.provider_id = OPENCODE_PROVIDER_ID.to_string();
                }
            }
        }
        self.platforms
            .rename_provider_references("opencodezen", OPENCODE_PROVIDER_ID);
        self.prune_stale_active_provider_models();
        self.normalize_platform_model_routes();
        if self.plugins.vision.vision_provider_id == "opencodezen" {
            self.plugins.vision.vision_provider_id = OPENCODE_PROVIDER_ID.to_string();
        }
        if self
            .provider(None)
            .map(|provider| provider.default_model.trim().is_empty())
            .unwrap_or(true)
        {
            self.active_provider = OPENCODE_PROVIDER_ID.to_string();
        }
        if self
            .active_provider_models
            .as_ref()
            .is_some_and(Vec::is_empty)
        {
            self.active_provider_models = Some(vec![ActiveProviderModelConfig {
                provider_id: OPENCODE_PROVIDER_ID.to_string(),
                model: OPENCODE_DEFAULT_CHAT_MODEL.to_string(),
            }]);
        }
    }

    pub fn normalize_api_quota_accounts(&mut self) {
        normalize_api_quota_provider(&mut self.plugins.api_quota.deepseek);
        normalize_api_quota_provider(&mut self.plugins.api_quota.openrouter);
    }

    pub fn normalize_managed_output_paths(&mut self, paths: &GQYPaths) {
        let Some(base) = directories::BaseDirs::new() else {
            return;
        };
        let documents = directories::UserDirs::new()
            .and_then(|dirs| dirs.document_dir().map(PathBuf::from))
            .unwrap_or_else(|| base.home_dir().join("Documents"));
        let pictures = std::env::var_os("XDG_PICTURES_DIR")
            .map(PathBuf::from)
            .or_else(|| {
                directories::UserDirs::new().and_then(|dirs| dirs.picture_dir().map(PathBuf::from))
            })
            .unwrap_or_else(|| base.home_dir().join("Pictures"));
        // The XDG data root is a legacy root too: an upgrade that ran while
        // `data_dir` still pointed at `~/.local/share/gqy` remapped these
        // fields onto it and persisted the result, so the value we now have to
        // heal is one this function itself wrote.
        let legacy_data = base.data_dir().join("gqy");
        if let Some((from, to)) = remap_managed_output_dir(
            &mut self.plugins.deep_research.output_dir,
            &[
                documents.join("GQY"),
                documents.join("gqy"),
                legacy_data.join("documents"),
            ],
            &paths.data_dir.join("documents"),
            base.home_dir(),
        ) {
            relocate_managed_output(&from, &to);
        }
        if let Some((from, to)) = remap_managed_output_dir(
            &mut self.plugins.image_generation.output_dir,
            &[
                pictures.join("gqy"),
                pictures.join("GQY"),
                legacy_data.join("pictures"),
            ],
            &paths.data_dir.join("pictures"),
            base.home_dir(),
        ) {
            relocate_managed_output(&from, &to);
        }
    }

    pub fn prune_stale_active_provider_models(&mut self) {
        if let Some(active_models) = &mut self.active_provider_models {
            active_models.retain(|active| active_model_exists(&self.providers, active));
        }
        if let Some(active_models) = &mut self.active_multimodal_provider_models {
            active_models.retain(|active| active_model_supports_image(&self.providers, active));
        }
    }

    pub fn validate(&self) -> Result<()> {
        if crate::i18n::UiLanguage::parse(&self.display.language).is_none() {
            bail!(
                "{}",
                crate::i18n::text(
                    "display.language must be 'auto', 'en', or 'zh'",
                    "display.language 必须是 'auto'、'en' 或 'zh'"
                )
            );
        }
        if self.active_provider.trim().is_empty() {
            bail!("active_provider cannot be empty");
        }
        if self.providers.is_empty() {
            bail!("at least one provider is required");
        }
        let mut provider_ids = HashSet::with_capacity(self.providers.len());
        for provider in &self.providers {
            if provider.id.trim().is_empty() {
                bail!("provider id cannot be empty");
            }
            if provider.id.trim() != provider.id {
                bail!(
                    "provider id must not contain surrounding whitespace: {}",
                    provider.id
                );
            }
            if !provider_ids.insert(provider.id.as_str()) {
                bail!("duplicate provider id: {}", provider.id);
            }
            if provider.base_url.trim().is_empty() {
                bail!("provider {} base_url cannot be empty", provider.id);
            }
        }
        if !(0.1..=1.0).contains(&self.context.trim_at_ratio) {
            bail!("context.trim_at_ratio must be between 0.1 and 1.0");
        }
        if !(0.1..=1.0).contains(&self.context.compact_force_ratio) {
            bail!("context.compact_force_ratio must be between 0.1 and 1.0");
        }
        if self.context.compact_force_ratio < self.context.trim_at_ratio {
            bail!("context.compact_force_ratio must be >= context.trim_at_ratio");
        }
        if !(0.05..=1.0).contains(&self.context.compact_soft_ratio)
            || !(0.05..=1.0).contains(&self.context.compact_snip_ratio)
        {
            bail!("context.compact_soft_ratio and compact_snip_ratio must be between 0.05 and 1.0");
        }
        if self.context.compact_soft_ratio > self.context.compact_snip_ratio
            || self.context.compact_snip_ratio > self.context.trim_at_ratio
        {
            bail!("context watermarks must be ordered: compact_soft_ratio <= compact_snip_ratio <= trim_at_ratio <= compact_force_ratio");
        }
        if !(0.01..=0.9).contains(&self.context.trim_batch_ratio) {
            bail!("context.trim_batch_ratio must be between 0.01 and 0.9");
        }
        match self.context.on_overflow.as_str() {
            "pop" | "compact" => {}
            value => bail!("context.on_overflow must be 'pop' or 'compact', got: {value}"),
        }
        if self.display.repl_replay_turns > MAX_REPL_REPLAY_TURNS {
            bail!("display.repl_replay_turns must be between 0 and {MAX_REPL_REPLAY_TURNS}");
        }
        if self.display.command_output_lines > MAX_COMMAND_OUTPUT_LINES {
            bail!("display.command_output_lines must be between 0 and {MAX_COMMAND_OUTPUT_LINES}");
        }
        if self.plugins.print_image.width_percent == 0
            || self.plugins.print_image.width_percent > 100
        {
            bail!("plugins.print_image.width_percent must be between 1 and 100");
        }
        if self.plugins.print_image.height_percent == 0
            || self.plugins.print_image.height_percent > 100
        {
            bail!("plugins.print_image.height_percent must be between 1 and 100");
        }
        if self.plugins.web.max_results == 0 {
            bail!("plugins.web.max_results must be greater than 0");
        }
        match self.plugins.deep_research.thinking_depth.as_str() {
            "minimal" | "low" | "medium" | "high" | "xhigh" => {}
            value => bail!("plugins.deep_research.thinking_depth is invalid: {value}"),
        }
        match self.plugins.image_generation.provider_type.as_str() {
            "openai" | "rightcode" => {}
            value => bail!("plugins.image_generation.provider_type is invalid: {value}"),
        }
        match self.plugins.image_generation.default_aspect_ratio.as_str() {
            "自动" | "1:1" | "2:3" | "3:2" | "3:4" | "4:3" | "4:5" | "5:4" | "9:16" | "16:9"
            | "21:9" => {}
            value => bail!("plugins.image_generation.default_aspect_ratio is invalid: {value}"),
        }
        match self.plugins.image_generation.default_resolution.as_str() {
            "1K" | "2K" | "4K" => {}
            value => bail!("plugins.image_generation.default_resolution is invalid: {value}"),
        }
        if self.plugins.image_generation.timeout_seconds == 0 {
            bail!("plugins.image_generation.timeout_seconds must be greater than 0");
        }
        if self.plugins.knowledge_base.max_search_results == 0 {
            bail!("plugins.knowledge_base.max_search_results must be greater than 0");
        }
        if self.plugins.knowledge_base.max_read_lines == 0 {
            bail!("plugins.knowledge_base.max_read_lines must be greater than 0");
        }
        if self.plugins.knowledge_base.max_file_size_kb == 0 {
            bail!("plugins.knowledge_base.max_file_size_kb must be greater than 0");
        }
        if self.plugins.knowledge_base.semantic_chunk_chars < 128 {
            bail!("plugins.knowledge_base.semantic_chunk_chars must be at least 128");
        }
        if self.plugins.knowledge_base.semantic_chunk_overlap
            >= self.plugins.knowledge_base.semantic_chunk_chars
        {
            bail!("plugins.knowledge_base.semantic_chunk_overlap must be smaller than semantic_chunk_chars");
        }
        if self.plugins.knowledge_base.semantic_top_k == 0 {
            bail!("plugins.knowledge_base.semantic_top_k must be greater than 0");
        }
        if self.plugins.knowledge_base.embedding_timeout_seconds == 0 {
            bail!("plugins.knowledge_base.embedding_timeout_seconds must be greater than 0");
        }
        if !(0.0..=2.0).contains(&self.provider(None)?.temperature) {
            bail!("provider temperature must be between 0.0 and 2.0");
        }
        for provider in &self.providers {
            if provider.timeout_seconds == 0 {
                bail!(
                    "provider {} timeout_seconds must be greater than 0",
                    provider.id
                );
            }
            if !(0.0..=2.0).contains(&provider.temperature) {
                bail!(
                    "provider {} temperature must be between 0.0 and 2.0",
                    provider.id
                );
            }
            if provider.anthropic_max_tokens == 0 {
                bail!(
                    "provider {} anthropic_max_tokens must be greater than 0",
                    provider.id
                );
            }
        }
        for provider in &self.providers {
            for (model, cost) in &provider.model_costs {
                if cost.input < 0.0
                    || cost.output < 0.0
                    || cost.cache_read.is_some_and(|price| price < 0.0)
                {
                    bail!(
                        "provider {} model {model} price must be non-negative",
                        provider.id
                    );
                }
            }
        }
        if !(0.0..=1.0).contains(&self.plugins.memes.auto_send_probability) {
            bail!("plugins.memes.auto_send_probability must be between 0.0 and 1.0");
        }
        if self.plugins.memes.width_percent == 0 || self.plugins.memes.width_percent > 100 {
            bail!("plugins.memes.width_percent must be between 1 and 100");
        }
        if self.plugins.memes.height_percent == 0 || self.plugins.memes.height_percent > 100 {
            bail!("plugins.memes.height_percent must be between 1 and 100");
        }
        if self.plugins.memes.search_max_results == 0 || self.plugins.memes.search_max_results > 3 {
            bail!("plugins.memes.search_max_results must be between 1 and 3");
        }
        let mem = self.memory_config();
        if mem.forgetting_half_life_days <= 0.0 {
            bail!("memory.forgetting_half_life_days must be greater than 0");
        }
        if mem.forget_after_days == 0 {
            bail!("memory.forget_after_days must be greater than 0");
        }
        if !(2..=100).contains(&mem.diary_batch_size) {
            bail!("memory.diary_batch_size must be between 2 and 100");
        }
        if !(1..=3650).contains(&mem.short_diary_retention_days) {
            bail!("memory.short_diary_retention_days must be between 1 and 3650");
        }
        if !(1..=100).contains(&mem.diary_promotion_recalls) {
            bail!("memory.diary_promotion_recalls must be between 1 and 100");
        }
        if !(5..=600).contains(&mem.organizer_timeout_seconds) {
            bail!("memory.organizer_timeout_seconds must be between 5 and 600");
        }
        if !(0.0..=1.0).contains(&self.plugins.knowledge_base.semantic_min_score) {
            bail!("plugins.knowledge_base.semantic_min_score must be between 0.0 and 1.0");
        }
        validate_api_quota_accounts("deepseek", &self.plugins.api_quota.deepseek)?;
        validate_api_quota_accounts("openrouter", &self.plugins.api_quota.openrouter)?;
        self.validate_model_references()?;
        self.validate_global_multimodal_config()?;
        self.validate_platforms()?;
        self.provider(None)?;
        Ok(())
    }

    pub fn validate_model_references(&self) -> Result<()> {
        if let Some(pool) = &self.active_provider_models {
            if pool.is_empty() {
                bail!("at least one model endpoint must remain active");
            }
            validate_unique_existing_pool(&self.providers, "active text", pool, false)?;
        }
        let kb_provider = self.plugins.knowledge_base.embedding_provider_id.trim();
        if !kb_provider.is_empty() {
            self.provider(Some(kb_provider))?;
        }
        Ok(())
    }

    pub fn validate_global_multimodal_config(&self) -> Result<()> {
        if let Some(pool) = &self.active_multimodal_provider_models {
            validate_unique_existing_pool(&self.providers, "active multimodal", pool, true)?;
        }
        if self.plugins.vision.enabled && !self.plugins.vision.vision_provider_id.trim().is_empty()
        {
            self.vision_provider_choice()?;
        }
        Ok(())
    }

    pub fn validate_platforms(&self) -> Result<()> {
        let command_prefix = &self.platforms.command_prefix;
        if command_prefix.is_empty()
            || command_prefix.trim() != command_prefix
            || command_prefix.chars().count() > MAX_PLATFORM_COMMAND_PREFIX_CHARS
            || command_prefix
                .chars()
                .any(|character| character.is_whitespace() || character.is_control())
        {
            bail!(
                "platforms.command_prefix must be a trimmed, non-empty value of at most {MAX_PLATFORM_COMMAND_PREFIX_CHARS} characters without whitespace"
            );
        }
        for command in self.platforms.commands.keys() {
            if command.is_empty()
                || command.len() > 64
                || !command.bytes().all(|byte| {
                    byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || matches!(byte, b'_' | b'-')
                })
            {
                bail!(
                    "platforms.commands keys must be lowercase ASCII command ids of at most 64 bytes"
                );
            }
        }
        let qq = &self.platforms.qq;
        if qq.reverse_ws_port == 0 {
            bail!("platforms.qq.reverse_ws_port must be between 1 and 65535");
        }
        for (field, limits) in [
            ("session_limits", Some(qq.session_limits)),
            (
                "private_chats.session_limits",
                qq.private_chats.session_limits,
            ),
            ("group_chats.session_limits", qq.group_chats.session_limits),
        ] {
            if let Some(limits) = limits {
                validate_platform_session_limits(field, limits)?;
            }
        }
        validate_unique_existing_pool(
            &self.providers,
            "QQ text",
            qq.text_models.as_deref().unwrap_or_default(),
            false,
        )?;
        validate_unique_existing_pool(
            &self.providers,
            "QQ multimodal",
            qq.multimodal_models.as_deref().unwrap_or_default(),
            true,
        )?;
        validate_unique_existing_pool(
            &self.providers,
            "QQ non-whitelist text",
            qq.non_whitelist_text_models.as_deref().unwrap_or_default(),
            false,
        )?;
        for (field, limit) in [
            (
                "private_chats.non_whitelist_rate_limit",
                qq.private_chats.non_whitelist_rate_limit,
            ),
            (
                "group_chats.whitelist_rate_limit",
                qq.group_chats.whitelist_rate_limit,
            ),
            (
                "group_chats.non_whitelist_rate_limit",
                qq.group_chats.non_whitelist_rate_limit,
            ),
        ] {
            if limit.window_seconds == 0 || limit.window_seconds > 86_400 {
                bail!("platforms.qq.{field}.window_seconds must be between 1 and 86400");
            }
        }
        for (field, ids) in [
            ("admin_users", qq.admin_users.as_slice()),
            (
                "private_chats.whitelist",
                qq.private_chats.whitelist.as_slice(),
            ),
            ("group_chats.whitelist", qq.group_chats.whitelist.as_slice()),
        ] {
            let mut seen = HashSet::with_capacity(ids.len());
            if ids.iter().any(|id| *id <= 0 || !seen.insert(*id)) {
                bail!("platforms.qq.{field} must contain unique positive QQ ids");
            }
        }
        let mut trigger_keywords = HashSet::with_capacity(qq.group_chats.trigger_keywords.len());
        for keyword in &qq.group_chats.trigger_keywords {
            if keyword.is_empty()
                || keyword.trim() != keyword
                || keyword.chars().count() > 128
                || keyword.chars().any(char::is_control)
                || !trigger_keywords.insert(keyword)
            {
                bail!(
                    "platforms.qq.group_chats.trigger_keywords must contain unique, trimmed, non-empty values of at most 128 characters"
                );
            }
        }
        let mut identities = HashSet::with_capacity(qq.conversations.len());
        for route in &qq.conversations {
            self.validate_platform_model_route(route)?;
            if let Some(limits) = route.session_limits {
                validate_platform_session_limits("conversations[].session_limits", limits)?;
            }
            if !identities.insert(route.identity()) {
                bail!(
                    "duplicate QQ conversation configuration: {} / {}",
                    route.conversation.kind.as_str(),
                    route.conversation.id
                );
            }
        }
        for (plugin_id, instance) in &qq.plugins {
            if plugin_id.trim().is_empty() || plugin_id.trim() != plugin_id {
                bail!("QQ plugin ids must be non-empty and trimmed");
            }
            if let Some((_, validate)) = PLATFORM_PLUGIN_VALIDATORS
                .iter()
                .find(|(id, _)| *id == plugin_id)
            {
                validate(instance)?;
            }
            if plugin_id == REAL_CONTEXT_PLUGIN_ID {
                let settings = RealContextPluginSettings::from_instance(instance)?;
                if let Some(models) = settings.text_models.as_deref() {
                    validate_unique_existing_pool(
                        &self.providers,
                        "real-context text",
                        models,
                        false,
                    )?;
                }
            }
        }
        Ok(())
    }

    pub fn validate_platform_model_route(&self, route: &PlatformModelRoute) -> Result<()> {
        if !is_positive_decimal_id(&route.conversation.id) {
            let label = match route.conversation.kind {
                PlatformConversationKind::Private => "QQ id",
                PlatformConversationKind::Group => "group id",
            };
            bail!("QQ conversation id must be a positive decimal {label}");
        }
        if route.extra_prompt.chars().count() > 200_000 || route.extra_prompt.contains('\0') {
            bail!("QQ conversation extra_prompt is invalid or exceeds 200000 characters");
        }
        if let PlatformPersonaOverride::Custom { name } = &route.persona {
            let path = Path::new(name);
            if name.is_empty()
                || name.trim() != name
                || name.chars().count() > 255
                || !name.ends_with(".md")
                || name.chars().any(char::is_control)
                || path.file_name().and_then(|value| value.to_str()) != Some(name.as_str())
            {
                bail!("QQ conversation persona must be a safe Markdown persona filename");
            }
        }
        self.validate_platform_model_pool(
            route,
            "text_models",
            route.text_models.as_deref(),
            false,
        )?;
        self.validate_platform_model_pool(
            route,
            "multimodal_models",
            route.multimodal_models.as_deref(),
            true,
        )?;
        Ok(())
    }

    pub fn validate_platform_model_pool(
        &self,
        route: &PlatformModelRoute,
        field: &str,
        pool: Option<&[ActiveProviderModelConfig]>,
        require_multimodal: bool,
    ) -> Result<()> {
        let Some(pool) = pool else {
            return Ok(());
        };
        let mut seen = HashSet::with_capacity(pool.len());
        for entry in pool {
            if !seen.insert((entry.provider_id.as_str(), entry.model.as_str())) {
                bail!(
                    "duplicate {} model in platform route: {} / {}",
                    field,
                    entry.provider_id,
                    entry.model
                );
            }
            if !active_model_exists(&self.providers, entry) {
                bail!(
                    "unknown {} provider/model in QQ conversation {} / {}: {} / {}",
                    field,
                    route.conversation.kind.as_str(),
                    route.conversation.id,
                    entry.provider_id,
                    entry.model
                );
            }
            if require_multimodal
                && !self.model_supports_any_input(&entry.provider_id, &entry.model, &["image"])
            {
                bail!(
                    "platform route multimodal model does not declare image input: {} / {}",
                    entry.provider_id,
                    entry.model
                );
            }
        }
        Ok(())
    }

    pub fn platform_model_route(
        &self,
        kind: PlatformConversationKind,
        conversation_id: &str,
    ) -> Option<&PlatformModelRoute> {
        self.platforms.model_route(kind, conversation_id)
    }

    pub fn qq_text_model_pool<'a>(
        &'a self,
        kind: PlatformConversationKind,
        conversation_id: &str,
        use_non_whitelist_pool: bool,
    ) -> Option<&'a [ActiveProviderModelConfig]> {
        if let Some(route) = self.platform_model_route(kind, conversation_id) {
            if route.text_models.is_some() {
                return route.text_models.as_deref();
            }
            if route.text_models_inheritance == PlatformModelPoolInheritance::Global {
                return self.active_provider_models.as_deref();
            }
        }
        if use_non_whitelist_pool {
            if let Some(pool) = self.platforms.qq.non_whitelist_text_models.as_deref() {
                return Some(pool);
            }
        }
        self.platforms
            .qq
            .text_models
            .as_deref()
            .or(self.active_provider_models.as_deref())
    }

    pub fn qq_multimodal_model_pool(
        &self,
        kind: PlatformConversationKind,
        conversation_id: &str,
    ) -> Option<&[ActiveProviderModelConfig]> {
        if let Some(route) = self.platform_model_route(kind, conversation_id) {
            if route.multimodal_models.is_some() {
                return route.multimodal_models.as_deref();
            }
            if route.multimodal_models_inheritance == PlatformModelPoolInheritance::Global {
                return self.active_multimodal_provider_models.as_deref();
            }
        }
        self.platforms
            .qq
            .multimodal_models
            .as_deref()
            .or(self.active_multimodal_provider_models.as_deref())
    }

    pub fn apply_qq_conversation_persona(
        &mut self,
        kind: PlatformConversationKind,
        conversation_id: &str,
    ) {
        let persona = self
            .platform_model_route(kind, conversation_id)
            .map(|route| route.persona.clone())
            .unwrap_or_default();
        match persona {
            PlatformPersonaOverride::Inherit => {}
            PlatformPersonaOverride::GQY => self.prompt.active_persona.clear(),
            PlatformPersonaOverride::Custom { name } => self.prompt.active_persona = name,
        }
    }

    pub fn normalize_platform_model_routes(&mut self) {
        self.platforms.normalize_model_routes();
    }

    pub fn prune_platform_model_routes(&mut self) {
        self.platforms.prune_model_references(&self.providers);
    }

    pub fn rename_platform_provider_references(&mut self, old_id: &str, new_id: &str) {
        self.platforms.rename_provider_references(old_id, new_id);
    }

    pub fn rename_platform_model_references(&mut self, provider_id: &str, old: &str, new: &str) {
        self.platforms
            .rename_model_references(provider_id, old, new);
    }

    pub fn rename_provider_references(&mut self, old_id: &str, new_id: &str) {
        if old_id == new_id || old_id.is_empty() || new_id.is_empty() {
            return;
        }
        if self.active_provider == old_id {
            self.active_provider = new_id.to_string();
        }
        for entries in [
            self.active_provider_models.as_mut(),
            self.active_multimodal_provider_models.as_mut(),
        ]
        .into_iter()
        .flatten()
        {
            rename_provider_in_pool(entries, old_id, new_id);
        }
        for tier in ModelTier::ALL {
            rename_provider_in_pool(self.subagent_tiers.pool_mut(tier), old_id, new_id);
        }
        self.platforms.rename_provider_references(old_id, new_id);
        if self.plugins.vision.vision_provider_id == old_id {
            self.plugins.vision.vision_provider_id = new_id.to_string();
        }
        if self.plugins.knowledge_base.embedding_provider_id == old_id {
            self.plugins.knowledge_base.embedding_provider_id = new_id.to_string();
        }
    }

    /// Removes references after a provider has been deleted from `providers`.
    pub fn remove_provider_references(&mut self, provider_id: &str) {
        retain_provider_pool(&mut self.active_provider_models, provider_id);
        retain_provider_pool(&mut self.active_multimodal_provider_models, provider_id);
        for tier in ModelTier::ALL {
            self.subagent_tiers
                .pool_mut(tier)
                .retain(|entry| entry.provider_id != provider_id);
        }
        self.platforms.remove_provider_references(provider_id);
        if self.plugins.vision.vision_provider_id == provider_id {
            self.plugins.vision.vision_provider_id.clear();
            self.plugins.vision.vision_model.clear();
        }
        if self.plugins.knowledge_base.embedding_provider_id == provider_id {
            self.plugins.knowledge_base.embedding_provider_id.clear();
            self.plugins.knowledge_base.embedding_model.clear();
        }
        if self.active_provider == provider_id {
            self.active_provider = self
                .active_provider_models
                .as_ref()
                .and_then(|pool| pool.first())
                .map(|entry| entry.provider_id.clone())
                .or_else(|| {
                    self.providers
                        .iter()
                        .find(|provider| !provider.default_model.trim().is_empty())
                        .or_else(|| self.providers.first())
                        .map(|provider| provider.id.clone())
                })
                .unwrap_or_default();
        }
    }

    /// Reconciles every model reference with the current provider models and
    /// input capabilities after an editor changes model metadata.
    pub fn prune_model_references(&mut self) {
        self.prune_stale_active_provider_models();
        retain_nonempty_pool(&mut self.active_provider_models);
        retain_nonempty_pool(&mut self.active_multimodal_provider_models);
        self.prune_subagent_tiers();
        self.prune_platform_model_routes();

        let vision_provider_id = self.plugins.vision.vision_provider_id.trim();
        if !vision_provider_id.is_empty() {
            let vision_model = self.plugins.vision.vision_model.trim();
            let valid = self
                .provider(Some(vision_provider_id))
                .ok()
                .map(|provider| {
                    let model = if vision_model.is_empty() {
                        provider.default_model.as_str()
                    } else {
                        vision_model
                    };
                    provider
                        .input_modalities(model)
                        .is_some_and(|modalities| modalities.iter().any(|item| item == "image"))
                })
                .unwrap_or(false);
            if !valid {
                self.plugins.vision.vision_provider_id.clear();
                self.plugins.vision.vision_model.clear();
            }
        }

        let kb_provider_id = self.plugins.knowledge_base.embedding_provider_id.trim();
        if !kb_provider_id.is_empty() && self.provider(Some(kb_provider_id)).is_err() {
            self.plugins.knowledge_base.embedding_provider_id.clear();
            self.plugins.knowledge_base.embedding_model.clear();
        }
    }

    pub fn provider(&self, id: Option<&str>) -> Result<&ProviderConfig> {
        let target = id.unwrap_or(&self.active_provider);
        self.providers
            .iter()
            .find(|provider| provider.id == target)
            .with_context(|| format!("provider not found: {target}"))
    }

    pub fn provider_model_choices(&self) -> Vec<ProviderModelChoice> {
        self.providers
            .iter()
            .flat_map(|provider| {
                let models =
                    if provider.models.is_empty() && !provider.default_model.trim().is_empty() {
                        vec![provider.default_model.clone()]
                    } else {
                        provider.models.clone()
                    };
                models
                    .into_iter()
                    .filter(|model| !model.trim().is_empty())
                    .map(|model| ProviderModelChoice {
                        provider_id: provider.id.clone(),
                        provider_name: provider.display_name.clone(),
                        model,
                    })
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    /// Embedding models are excluded: they produce vectors, not replies, and
    /// picking one here is always a mistake. The multimodal list derives from
    /// this one, so filtering here covers both.
    pub fn text_provider_model_choices(&self) -> Vec<ProviderModelChoice> {
        self.providers
            .iter()
            .flat_map(|provider| {
                provider
                    .models
                    .iter()
                    .filter(|model| !model.trim().is_empty())
                    .filter(|model| !Self::model_is_embedding(provider, model))
                    .map(|model| ProviderModelChoice {
                        provider_id: provider.id.clone(),
                        provider_name: provider.display_name.clone(),
                        model: model.clone(),
                    })
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    /// A model marked as producing vectors rather than chat. Stored beside the
    /// input modalities because it answers the same question — what the model
    /// is for.
    pub fn model_is_embedding(provider: &ProviderConfig, model: &str) -> bool {
        provider
            .model_modalities
            .get(model)
            .is_some_and(|modalities| modalities.iter().any(|item| item == EMBEDDING_MODALITY))
    }

    pub fn active_provider_model_choices(&self) -> Vec<ProviderModelChoice> {
        match &self.active_provider_models {
            None => self
                .provider(None)
                .ok()
                .filter(|provider| !provider.default_model.trim().is_empty())
                .map(|provider| {
                    vec![ProviderModelChoice {
                        provider_id: provider.id.clone(),
                        provider_name: provider.display_name.clone(),
                        model: provider.default_model.clone(),
                    }]
                })
                .unwrap_or_default(),
            Some(active_models) => active_models
                .iter()
                .filter_map(|active| {
                    let provider = self.provider(Some(active.provider_id.trim())).ok()?;
                    let model = active.model.trim();
                    provider
                        .has_configured_model(model)
                        .then(|| ProviderModelChoice {
                            provider_id: provider.id.clone(),
                            provider_name: provider.display_name.clone(),
                            model: model.to_string(),
                        })
                })
                .collect(),
        }
    }

    pub fn multimodal_provider_model_choices(&self) -> Vec<ProviderModelChoice> {
        self.text_provider_model_choices()
            .into_iter()
            .filter(|choice| {
                self.model_supports_any_input(&choice.provider_id, &choice.model, &["image"])
            })
            .collect()
    }

    pub fn active_multimodal_provider_model_choices(&self) -> Vec<ProviderModelChoice> {
        match &self.active_multimodal_provider_models {
            Some(active_models) => active_models
                .iter()
                .filter_map(|active| {
                    let provider = self.provider(Some(active.provider_id.trim())).ok()?;
                    let model = active.model.trim();
                    (provider.has_configured_model(model)
                        && provider.input_modalities(model).is_some_and(|modalities| {
                            modalities.iter().any(|item| item == "image")
                        }))
                    .then(|| ProviderModelChoice {
                        provider_id: provider.id.clone(),
                        provider_name: provider.display_name.clone(),
                        model: model.to_string(),
                    })
                })
                .collect(),
            None => Vec::new(),
        }
    }

    pub fn is_active_multimodal_provider_model(&self, provider_id: &str, model: &str) -> bool {
        self.active_multimodal_provider_models
            .as_ref()
            .map(|active_models| {
                active_models
                    .iter()
                    .any(|active| active.provider_id == provider_id && active.model == model)
            })
            .unwrap_or(false)
    }
}
