//! app_impl2 — 自 src/config/app_impl.rs 拆分。

use super::*;

impl AppConfig {
    pub fn remove_active_model_references(&mut self, provider_id: &str, model: &str) {
        if let Some(active_models) = &mut self.active_provider_models {
            active_models
                .retain(|active| !(active.provider_id == provider_id && active.model == model));
        }
        if let Some(active_models) = &mut self.active_multimodal_provider_models {
            active_models
                .retain(|active| !(active.provider_id == provider_id && active.model == model));
        }
        // A model gone from the text models must leave every tier pool too.
        for tier in ModelTier::ALL {
            self.subagent_tiers
                .pool_mut(tier)
                .retain(|entry| !(entry.provider_id == provider_id && entry.model == model));
        }
        self.platforms.remove_model_references(provider_id, model);
        if self.plugins.vision.vision_provider_id == provider_id
            && self.plugins.vision.vision_model == model
        {
            self.plugins.vision.vision_provider_id.clear();
            self.plugins.vision.vision_model.clear();
        }
        if self.plugins.knowledge_base.embedding_provider_id == provider_id
            && self.plugins.knowledge_base.embedding_model == model
        {
            self.plugins.knowledge_base.embedding_provider_id.clear();
            self.plugins.knowledge_base.embedding_model.clear();
        }
        retain_nonempty_pool(&mut self.active_provider_models);
        retain_nonempty_pool(&mut self.active_multimodal_provider_models);
    }

    pub fn toggle_active_multimodal_provider_model(
        &mut self,
        provider_id: &str,
        model: &str,
    ) -> Result<bool> {
        if model.trim().is_empty() {
            bail!("model cannot be empty");
        }
        if let Some(active_models) = &mut self.active_multimodal_provider_models {
            if let Some(index) = active_models
                .iter()
                .position(|active| active.provider_id == provider_id && active.model == model)
            {
                active_models.remove(index);
                return Ok(false);
            }
        }
        let provider = self.provider(Some(provider_id))?;
        if !provider.has_configured_model(model) {
            bail!("model is not configured for provider {provider_id}: {model}");
        }
        if !provider
            .input_modalities(model)
            .is_some_and(|modalities| modalities.iter().any(|item| item == "image"))
        {
            bail!("multimodal model does not declare image input: {provider_id} / {model}");
        }
        let active_models = self
            .active_multimodal_provider_models
            .get_or_insert_with(Vec::new);
        active_models.push(ActiveProviderModelConfig {
            provider_id: provider_id.to_string(),
            model: model.to_string(),
        });
        Ok(true)
    }

    pub fn model_supports_any_input(
        &self,
        provider_id: &str,
        model: &str,
        inputs: &[&str],
    ) -> bool {
        self.provider(Some(provider_id))
            .ok()
            .and_then(|provider| provider.input_modalities(model))
            .map(|modalities| {
                modalities
                    .iter()
                    .any(|m| inputs.iter().any(|input| m == input))
            })
            .unwrap_or(false)
    }

    pub fn vision_provider_choice(&self) -> Result<(String, String)> {
        let vision = &self.plugins.vision;
        if !vision.vision_provider_id.trim().is_empty() {
            let provider_id = vision.vision_provider_id.trim().to_string();
            let provider = self.provider(Some(&provider_id))?;
            let model = if vision.vision_model.trim().is_empty() {
                provider.default_model.clone()
            } else {
                vision.vision_model.trim().to_string()
            };
            if !provider
                .input_modalities(&model)
                .is_some_and(|modalities| modalities.iter().any(|item| item == "image"))
            {
                bail!("vision model does not declare image input: {provider_id} / {model}");
            }
            return Ok((provider_id, model));
        }
        if let Some(active) = self.active_multimodal_provider_models.as_ref() {
            if let Some(choice) = self
                .active_multimodal_provider_model_choices()
                .into_iter()
                .find(|choice| {
                    self.model_supports_any_input(&choice.provider_id, &choice.model, &["image"])
                })
            {
                return Ok((choice.provider_id, choice.model));
            }
            if !active.is_empty() {
                bail!("the configured multimodal model pool has no image-capable model");
            }
        }
        Ok((
            OPENCODE_PROVIDER_ID.to_string(),
            OPENCODE_DEFAULT_VISION_MODEL.to_string(),
        ))
    }

    /// A tier pool's usable model choices: configured entries filtered to
    /// models that still exist under their provider (entries whose model
    /// was removed from the text models are ignored, mirroring
    /// `active_provider_model_choices`).
    pub fn subagent_tier_choices(&self, tier: ModelTier) -> Vec<ProviderModelChoice> {
        self.subagent_tiers
            .pool(tier)
            .iter()
            .filter_map(|entry| {
                let provider = self.provider(Some(entry.provider_id.trim())).ok()?;
                let model = entry.model.trim();
                provider
                    .has_configured_model(model)
                    .then(|| ProviderModelChoice {
                        provider_id: provider.id.clone(),
                        provider_name: provider.display_name.clone(),
                        model: model.to_string(),
                    })
            })
            .collect()
    }

    pub fn is_subagent_tier_model(&self, tier: ModelTier, provider_id: &str, model: &str) -> bool {
        self.subagent_tiers
            .pool(tier)
            .iter()
            .any(|entry| entry.provider_id == provider_id && entry.model == model)
    }

    /// Adds/removes a model in a tier pool. Returns `true` when the model
    /// is in the pool after the call.
    pub fn toggle_subagent_tier_model(
        &mut self,
        tier: ModelTier,
        provider_id: &str,
        model: &str,
    ) -> Result<bool> {
        if model.trim().is_empty() {
            bail!("model cannot be empty");
        }
        self.provider(Some(provider_id))?;
        let pool = self.subagent_tiers.pool_mut(tier);
        if let Some(index) = pool
            .iter()
            .position(|entry| entry.provider_id == provider_id && entry.model == model)
        {
            pool.remove(index);
            Ok(false)
        } else {
            pool.push(ActiveProviderModelConfig {
                provider_id: provider_id.to_string(),
                model: model.to_string(),
            });
            Ok(true)
        }
    }

    /// Drops tier pool entries whose model no longer exists among the
    /// configured text models (a model removed from a provider must also
    /// leave every tier pool).
    pub fn prune_subagent_tiers(&mut self) {
        for tier in ModelTier::ALL {
            let providers = &self.providers;
            self.subagent_tiers
                .pool_mut(tier)
                .retain(|entry| active_model_exists(providers, entry));
        }
    }

    pub fn is_active_provider_model(&self, provider_id: &str, model: &str) -> bool {
        match &self.active_provider_models {
            None => self
                .provider(None)
                .map(|provider| provider.id == provider_id && provider.default_model == model)
                .unwrap_or(false),
            Some(active_models) => active_models
                .iter()
                .any(|active| active.provider_id == provider_id && active.model == model),
        }
    }

    pub fn toggle_active_provider_model(&mut self, provider_id: &str, model: &str) -> Result<bool> {
        if model.trim().is_empty() {
            bail!("model cannot be empty");
        }
        self.provider(Some(provider_id))?;
        if self.active_provider_models.is_none() {
            self.active_provider_models = Some(
                self.active_provider_model_choices()
                    .into_iter()
                    .map(|choice| ActiveProviderModelConfig {
                        provider_id: choice.provider_id,
                        model: choice.model,
                    })
                    .collect(),
            );
        }
        let active_models = self.active_provider_models.get_or_insert_with(Vec::new);
        if let Some(index) = active_models
            .iter()
            .position(|active| active.provider_id == provider_id && active.model == model)
        {
            active_models.remove(index);
            return Ok(false);
        }
        active_models.push(ActiveProviderModelConfig {
            provider_id: provider_id.to_string(),
            model: model.to_string(),
        });
        Ok(true)
    }

    pub fn set_active_provider_models(
        &mut self,
        models: &[ActiveProviderModelConfig],
    ) -> Result<()> {
        if models.is_empty() {
            bail!("at least one model endpoint must remain active");
        }
        let choices = self.provider_model_choices();
        let mut seen = std::collections::HashSet::with_capacity(models.len());
        for model in models {
            if model.provider_id.trim().is_empty() || model.model.trim().is_empty() {
                bail!("provider_id and model cannot be empty");
            }
            if !seen.insert((&model.provider_id, &model.model)) {
                bail!(
                    "duplicate active provider/model: {} / {}",
                    model.provider_id,
                    model.model
                );
            }
            if !choices.iter().any(|choice| {
                choice.provider_id == model.provider_id && choice.model == model.model
            }) {
                bail!(
                    "unknown configured provider/model: {} / {}",
                    model.provider_id,
                    model.model
                );
            }
        }
        self.active_provider_models = Some(models.to_vec());
        Ok(())
    }

    pub fn set_active_provider_model(&mut self, provider_id: &str, model: &str) -> Result<()> {
        let provider = self
            .providers
            .iter_mut()
            .find(|provider| provider.id == provider_id)
            .with_context(|| format!("provider not found: {provider_id}"))?;
        if model.trim().is_empty() {
            bail!("model cannot be empty");
        }
        self.active_provider = provider.id.clone();
        provider.default_model = model.to_string();
        self.active_provider_models = Some(vec![ActiveProviderModelConfig {
            provider_id: provider.id.clone(),
            model: model.to_string(),
        }]);
        if !provider.models.iter().any(|item| item == model) {
            provider.models.push(model.to_string());
        }
        Ok(())
    }

    pub fn remove_active_provider_model(&mut self, provider_id: &str, model: &str) -> Result<()> {
        let provider_index = self
            .providers
            .iter()
            .position(|provider| provider.id == provider_id)
            .with_context(|| format!("provider not found: {provider_id}"))?;
        if model.trim().is_empty() {
            bail!("model cannot be empty");
        }
        {
            let provider = &mut self.providers[provider_index];
            provider.models.retain(|item| item != model);
            provider.model_context_window.remove(model);
            provider.model_modalities.remove(model);
            if provider.default_model == model {
                provider.default_model = provider.models.first().cloned().unwrap_or_default();
            }
        }
        self.remove_active_model_references(provider_id, model);
        Ok(())
    }

    pub fn active_context_window(&self) -> Result<Option<usize>> {
        let choices = self.active_provider_model_choices();
        if choices.is_empty() {
            return Ok(None);
        }
        let mut windows = Vec::new();
        for choice in choices {
            let Some(window) =
                self.context_window_for_provider_model(&choice.provider_id, &choice.model)?
            else {
                return Ok(None);
            };
            windows.push(window);
        }
        Ok(windows.into_iter().min())
    }

    pub fn context_window_for_provider_model(
        &self,
        provider_id: &str,
        model: &str,
    ) -> Result<Option<usize>> {
        let provider = self.provider(Some(provider_id))?;
        if let Some(window) = provider
            .model_context_window
            .get(model)
            .copied()
            .filter(|&w| w > 0)
        {
            return Ok(Some(window));
        }
        Ok(crate::models_cache::context_window(provider_id, model)
            .map(|w| w as usize)
            .or_else(|| {
                (self.context.default_context_window > 0)
                    .then_some(self.context.default_context_window)
            }))
    }

    pub fn system_prompt(&self, paths: &GQYPaths) -> Result<String> {
        self.system_prompt_for(paths, PromptAudience::Owner)
    }

    pub fn system_prompt_for(&self, paths: &GQYPaths, audience: PromptAudience) -> Result<String> {
        let mut prompt = self.base_system_prompt(paths)?;
        if audience.includes_user_identity() {
            let user_identity = self.user_identity_prompt(paths)?;
            if !user_identity.trim().is_empty() {
                prompt.push_str("\n\n<current-user-profile>\n");
                prompt.push_str(
                    "This profile describes the user currently interacting with you.\n\n",
                );
                prompt.push_str(user_identity.trim());
                prompt.push_str("\n</current-user-profile>");
            }
        }
        Ok(prompt)
    }

    pub fn base_system_prompt(&self, paths: &GQYPaths) -> Result<String> {
        let persona = self.active_persona_prompt(paths)?;
        if persona.trim().is_empty() {
            Ok(default_system_prompt())
        } else {
            Ok(persona)
        }
    }

    pub fn custom_system_prompt(&self, paths: &GQYPaths) -> Result<String> {
        if let Some(prompt) = self
            .system_prompt
            .as_deref()
            .filter(|prompt| !prompt.trim().is_empty())
        {
            return Ok(prompt.to_string());
        }
        let prompt_file = self.system_prompt_path(paths);
        if prompt_file.exists() {
            return Ok(std::fs::read_to_string(prompt_file)?);
        }
        Ok(String::new())
    }

    pub fn prompts_dir_path(&self, paths: &GQYPaths) -> PathBuf {
        migrated_resource_path(paths, &self.prompt.prompts_dir)
            .unwrap_or_else(|| config_relative_path(paths, &self.prompt.prompts_dir))
    }

    pub fn user_identity_path(&self, paths: &GQYPaths) -> PathBuf {
        if relative_path_equals(&self.prompt.user_identity_file, "user-identity.md") {
            fallback_resource_file(paths, "identities", "user-identity.md")
        } else if let Some(path) = migrated_fallback_file(
            paths,
            &self.prompt.user_identity_file,
            "identities",
            "user-identity.md",
        ) {
            path
        } else if let Some(path) = migrated_resource_path(paths, &self.prompt.user_identity_file) {
            path
        } else {
            config_relative_path(paths, &self.prompt.user_identity_file)
        }
    }

    pub fn identities_dir_path(&self, paths: &GQYPaths) -> PathBuf {
        migrated_resource_path(paths, &self.prompt.identities_dir)
            .unwrap_or_else(|| config_relative_path(paths, &self.prompt.identities_dir))
    }

    pub fn persona_path(&self, paths: &GQYPaths, name: &str) -> PathBuf {
        self.prompts_dir_path(paths).join(name)
    }

    pub fn validate_persona_files(&self, paths: &GQYPaths) -> Result<()> {
        if self
            .prompt
            .active_persona
            .trim()
            .eq_ignore_ascii_case("system-prompt.md")
        {
            bail!("system-prompt.md is reserved and cannot be used as a persona");
        }
        let directory = self.prompts_dir_path(paths);
        if !directory.exists() {
            return Ok(());
        }
        let mut scopes = HashMap::<String, String>::new();
        for entry in std::fs::read_dir(directory)? {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.ends_with(".md") {
                continue;
            }
            if name.eq_ignore_ascii_case("system-prompt.md") {
                continue;
            }
            let scope = persona_scope_name(&name);
            if let Some(existing) = scopes.insert(scope.clone(), name.clone()) {
                bail!(
                    "persona names map to the same persistent scope: {existing} and {name} ({scope})"
                );
            }
        }
        Ok(())
    }

    pub fn identity_path(&self, paths: &GQYPaths, name: &str) -> PathBuf {
        self.identities_dir_path(paths).join(name)
    }

    pub fn persona_memory_data_dir(&self, paths: &GQYPaths, persona: &str) -> PathBuf {
        paths
            .data_dir
            .join("personas")
            .join(persona_scope_name(persona))
    }

    pub fn persona_memory_state_dir(&self, paths: &GQYPaths, persona: &str) -> PathBuf {
        paths
            .state_dir
            .join("personas")
            .join(persona_scope_name(persona))
    }

    pub fn persona_skills_dir(&self, paths: &GQYPaths, persona: &str) -> PathBuf {
        paths
            .skills_dir
            .join("personas")
            .join(persona_scope_name(persona))
    }

    /// Sanitized scope name of the active persona; also the namespace key for
    /// sessions and per-persona state directories.
    pub fn active_persona_scope(&self) -> String {
        persona_scope_name(self.prompt.active_persona.trim())
    }

    /// Dev 模式的作用域配置:人格指针换成保留人格 "dev",记忆/技能目录
    /// 随之落入独立命名空间。键是常量人格名而非提示词内容——编辑
    /// dev-prompt.md 只改提示词,永远不会切库丢记忆。
    pub fn dev_scoped(&self) -> AppConfig {
        let mut config = self.clone();
        config.prompt.active_persona = crate::state::DEV_PERSONA.to_string();
        config
    }

    pub fn active_persona_memory_data_dir(&self, paths: &GQYPaths) -> PathBuf {
        self.persona_memory_data_dir(paths, self.prompt.active_persona.trim())
    }

    pub fn active_persona_memory_state_dir(&self, paths: &GQYPaths) -> PathBuf {
        self.persona_memory_state_dir(paths, self.prompt.active_persona.trim())
    }

    pub fn active_persona_skills_dir(&self, paths: &GQYPaths) -> PathBuf {
        self.persona_skills_dir(paths, self.prompt.active_persona.trim())
    }

    pub fn active_persona_prompt(&self, paths: &GQYPaths) -> Result<String> {
        if !self.prompt.active_persona.trim().is_empty() {
            let path = self.persona_path(paths, self.prompt.active_persona.trim());
            if path.exists() {
                return std::fs::read_to_string(&path)
                    .with_context(|| format!("failed to read {}", path.display()));
            }
        }
        if let Some(prompt) = self
            .system_prompt
            .as_deref()
            .filter(|prompt| !prompt.trim().is_empty())
        {
            return Ok(prompt.to_string());
        }
        let legacy = self.custom_system_prompt(paths)?;
        if legacy.trim().is_empty() {
            Ok(String::new())
        } else {
            Ok(legacy)
        }
    }

    pub fn user_identity_prompt(&self, paths: &GQYPaths) -> Result<String> {
        if !self.prompt.active_identity.trim().is_empty() {
            let path = self.identity_path(paths, self.prompt.active_identity.trim());
            if path.exists() {
                return std::fs::read_to_string(&path)
                    .with_context(|| format!("failed to read {}", path.display()));
            }
        }
        let path = self.user_identity_path(paths);
        if path.exists() {
            return std::fs::read_to_string(&path)
                .with_context(|| format!("failed to read {}", path.display()));
        }
        Ok(String::new())
    }

    pub fn system_prompt_path(&self, paths: &GQYPaths) -> PathBuf {
        let value = self
            .system_prompt_file
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("system-prompt.md");
        if relative_path_equals(value, "system-prompt.md") {
            fallback_resource_file(paths, "prompts", "system-prompt.md")
        } else if let Some(path) =
            migrated_fallback_file(paths, value, "prompts", "system-prompt.md")
        {
            path
        } else if let Some(path) = migrated_resource_path(paths, value) {
            path
        } else {
            let path = PathBuf::from(value);
            if path.is_absolute() {
                path
            } else {
                paths.config_dir.join(path)
            }
        }
    }

    pub fn upsert_provider(&mut self, provider: ProviderConfig) {
        self.active_provider = provider.id.clone();
        self.active_provider_models = if provider.default_model.trim().is_empty() {
            Some(vec![ActiveProviderModelConfig {
                provider_id: OPENCODE_PROVIDER_ID.to_string(),
                model: OPENCODE_DEFAULT_CHAT_MODEL.to_string(),
            }])
        } else {
            Some(vec![ActiveProviderModelConfig {
                provider_id: provider.id.clone(),
                model: provider.default_model.clone(),
            }])
        };
        match self
            .providers
            .iter()
            .position(|item| item.id == provider.id)
        {
            Some(index) => self.providers[index] = provider,
            None => self.providers.push(provider),
        }
    }
}
