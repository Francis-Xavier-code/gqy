//! tests3 — 自 src/config.rs 外移。
#![cfg(test)]

use super::tests::*;

#[test]
fn real_context_models_follow_provider_lifecycle() {
    let mut config = route_test_config();
    let old_id = config.providers[0].id.clone();
    let settings = RealContextPluginSettings {
        text_models: Some(vec![ActiveProviderModelConfig {
            provider_id: old_id.clone(),
            model: "text-only".to_string(),
        }]),
        ..RealContextPluginSettings::default()
    };
    let mut instance = PlatformPluginInstanceConfig::default();
    instance
        .settings
        .insert("future_option".to_string(), serde_json::json!(true));
    merge_real_context_settings(&mut instance, &settings);
    config
        .platforms
        .qq
        .plugins
        .insert(REAL_CONTEXT_PLUGIN_ID.to_string(), instance);

    config.providers[0].id = "renamed".to_string();
    config.rename_provider_references(&old_id, "renamed");
    let instance = &config.platforms.qq.plugins[REAL_CONTEXT_PLUGIN_ID];
    let reparsed = RealContextPluginSettings::from_instance(instance).unwrap();
    assert_eq!(reparsed.text_models.unwrap()[0].provider_id, "renamed");
    assert_eq!(instance.settings["future_option"], true);

    config.remove_active_model_references("renamed", "text-only");
    let reparsed = RealContextPluginSettings::from_instance(
        &config.platforms.qq.plugins[REAL_CONTEXT_PLUGIN_ID],
    )
    .unwrap();
    assert!(reparsed.text_models.is_none());
}

#[test]
fn platform_model_route_normalization_uses_none_for_inheritance() {
    let mut config = route_test_config();
    let provider_id = config.providers[0].id.clone();
    let mut route = test_route(&config);
    route.conversation.id = " 20002 ".to_string();
    route.extra_prompt = "  group prompt  ".to_string();
    route.text_models = Some(vec![
        ActiveProviderModelConfig {
            provider_id: format!(" {provider_id} "),
            model: " text-only ".to_string(),
        },
        ActiveProviderModelConfig {
            provider_id: provider_id.clone(),
            model: "text-only".to_string(),
        },
    ]);
    route.text_models_inheritance = PlatformModelPoolInheritance::Global;
    route.multimodal_models = Some(Vec::new());
    route.multimodal_models_inheritance = PlatformModelPoolInheritance::Global;
    config.platforms.qq.conversations.push(route);
    config.normalize_platform_model_routes();

    let normalized = &config.platforms.qq.conversations[0];
    assert_eq!(normalized.conversation.id, "20002");
    assert_eq!(normalized.extra_prompt, "group prompt");
    assert_eq!(normalized.text_models.as_ref().unwrap().len(), 1);
    assert_eq!(
        normalized.text_models_inheritance,
        PlatformModelPoolInheritance::Platform
    );
    assert!(normalized.multimodal_models.is_none());
    assert_eq!(
        normalized.multimodal_models_inheritance,
        PlatformModelPoolInheritance::Global
    );

    config.platforms.qq.conversations[0].text_models = Some(Vec::new());
    config.normalize_platform_model_routes();
    assert_eq!(config.platforms.qq.conversations.len(), 1);
    assert!(config.platforms.qq.conversations[0].text_models.is_none());
}

#[test]
fn platform_model_route_validation_rejects_bad_identity_models_and_duplicates() {
    let mut config = route_test_config();
    let mut route = test_route(&config);
    route.conversation.id = "0".to_string();
    assert!(config.validate_platform_model_route(&route).is_err());
    route.conversation.id = "not-a-qq".to_string();
    assert!(config.validate_platform_model_route(&route).is_err());

    route.conversation.id = "20002".to_string();
    route.multimodal_models.as_mut().unwrap()[0].model = "text-only".to_string();
    assert!(config.validate_platform_model_route(&route).is_err());

    route.multimodal_models = None;
    route.text_models.as_mut().unwrap()[0].model = "missing".to_string();
    assert!(config.validate_platform_model_route(&route).is_err());

    let route = test_route(&config);
    config.platforms.qq.conversations = vec![route.clone(), route];
    assert!(config.validate().is_err());
}

#[test]
fn platform_model_references_are_renamed_and_pruned() {
    let mut config = route_test_config();
    let old_provider = config.providers[0].id.clone();
    config.platforms.qq.non_whitelist_text_models = Some(vec![ActiveProviderModelConfig {
        provider_id: old_provider.clone(),
        model: "text-only".to_string(),
    }]);
    config.platforms.qq.conversations.push(test_route(&config));

    config.rename_platform_provider_references(&old_provider, "renamed");
    assert_eq!(
        config
            .platforms
            .qq
            .non_whitelist_text_models
            .as_ref()
            .unwrap()[0]
            .provider_id,
        "renamed"
    );
    let route = &config.platforms.qq.conversations[0];
    assert_eq!(
        route.text_models.as_ref().unwrap()[0].provider_id,
        "renamed"
    );
    assert_eq!(
        route.multimodal_models.as_ref().unwrap()[0].provider_id,
        "renamed"
    );

    config.rename_platform_provider_references("renamed", &old_provider);
    config.remove_active_model_references(&old_provider, "vision");
    assert!(config.platforms.qq.conversations[0]
        .multimodal_models
        .is_none());
    config.remove_active_model_references(&old_provider, "text-only");
    assert_eq!(config.platforms.qq.conversations.len(), 1);
    assert!(config.platforms.qq.conversations[0].text_models.is_none());
    assert!(config.platforms.qq.non_whitelist_text_models.is_none());
}

#[test]
fn provider_reference_updates_cover_every_model_pool_and_plugin() {
    let mut config = route_test_config();
    let old_id = config.providers[0].id.clone();
    config.active_provider = old_id.clone();
    config.active_provider_models = Some(vec![ActiveProviderModelConfig {
        provider_id: old_id.clone(),
        model: "text-only".to_string(),
    }]);
    config.active_multimodal_provider_models = Some(vec![ActiveProviderModelConfig {
        provider_id: old_id.clone(),
        model: "vision".to_string(),
    }]);
    config.subagent_tiers.cheap.push(ActiveProviderModelConfig {
        provider_id: old_id.clone(),
        model: "text-only".to_string(),
    });
    config.platforms.qq.non_whitelist_text_models = Some(vec![ActiveProviderModelConfig {
        provider_id: old_id.clone(),
        model: "text-only".to_string(),
    }]);
    config.platforms.qq.conversations.push(test_route(&config));
    config.plugins.vision.vision_provider_id = old_id.clone();
    config.plugins.vision.vision_model = "vision".to_string();
    config.plugins.knowledge_base.embedding_provider_id = old_id.clone();
    config.plugins.knowledge_base.embedding_model = "text-only".to_string();

    config.providers[0].id = "renamed".to_string();
    config.rename_provider_references(&old_id, "renamed");

    assert_eq!(config.active_provider, "renamed");
    assert_eq!(
        config.active_provider_models.as_ref().unwrap()[0].provider_id,
        "renamed"
    );
    assert_eq!(
        config.active_multimodal_provider_models.as_ref().unwrap()[0].provider_id,
        "renamed"
    );
    assert_eq!(config.subagent_tiers.cheap[0].provider_id, "renamed");
    assert_eq!(
        config
            .platforms
            .qq
            .non_whitelist_text_models
            .as_ref()
            .unwrap()[0]
            .provider_id,
        "renamed"
    );
    assert_eq!(
        config.platforms.qq.conversations[0]
            .text_models
            .as_ref()
            .unwrap()[0]
            .provider_id,
        "renamed"
    );
    assert_eq!(config.plugins.vision.vision_provider_id, "renamed");
    assert_eq!(
        config.plugins.knowledge_base.embedding_provider_id,
        "renamed"
    );
    assert!(config.validate().is_ok());

    config.providers.remove(0);
    config.remove_provider_references("renamed");
    assert!(config.active_provider_models.is_none());
    assert!(config.active_multimodal_provider_models.is_none());
    assert!(config.subagent_tiers.cheap.is_empty());
    assert!(config.platforms.qq.non_whitelist_text_models.is_none());
    assert_eq!(config.platforms.qq.conversations.len(), 1);
    assert!(config.platforms.qq.conversations[0].text_models.is_none());
    assert!(config.plugins.vision.vision_provider_id.is_empty());
    assert!(config
        .plugins
        .knowledge_base
        .embedding_provider_id
        .is_empty());
    assert_ne!(config.active_provider, "renamed");
}

#[test]
fn model_capability_pruning_clears_all_invalid_image_references() {
    let mut config = route_test_config();
    let provider_id = config.providers[0].id.clone();
    config.active_multimodal_provider_models = Some(vec![ActiveProviderModelConfig {
        provider_id: provider_id.clone(),
        model: "vision".to_string(),
    }]);
    config.platforms.qq.conversations.push(test_route(&config));
    config.plugins.vision.vision_provider_id = provider_id;
    config.plugins.vision.vision_model = "vision".to_string();
    config.providers[0]
        .model_modalities
        .insert("vision".to_string(), vec!["text".to_string()]);

    config.prune_model_references();

    assert!(config.active_multimodal_provider_models.is_none());
    assert!(config.platforms.qq.conversations[0]
        .multimodal_models
        .is_none());
    assert!(config.plugins.vision.vision_provider_id.is_empty());
    assert!(config.plugins.vision.vision_model.is_empty());
}

#[test]
fn duplicate_provider_ids_are_rejected() {
    let mut config = AppConfig::default();
    config.providers.push(config.providers[0].clone());
    assert!(config.validate().is_err());
}

#[test]
fn platform_multimodal_pruning_tracks_provider_capabilities() {
    let mut config = route_test_config();
    config.platforms.qq.conversations.push(test_route(&config));
    config.providers[0]
        .model_modalities
        .insert("vision".to_string(), vec!["text".to_string()]);

    config.prune_platform_model_routes();

    let route = &config.platforms.qq.conversations[0];
    assert!(route.multimodal_models.is_none());
    assert_eq!(route.text_models.as_ref().unwrap().len(), 1);
}

#[test]
fn new_custom_provider_has_no_openai_defaults() {
    let provider = ProviderConfig::new_custom();

    assert!(provider.id.is_empty());
    assert!(provider.display_name.is_empty());
    assert!(provider.base_url.is_empty());
    assert_eq!(provider.protocol, "auto");
    assert!(provider.api_key.is_none());
    assert!(provider.models.is_empty());
    assert!(provider.default_model.is_empty());
}

#[test]
fn default_anthropic_provider_uses_the_global_context_window_default() {
    let mut config = AppConfig::default();
    config.active_provider = "anthropic".to_string();
    let provider = config
        .providers
        .iter_mut()
        .find(|provider| provider.id == "anthropic")
        .unwrap();
    provider.models = vec!["claude-sonnet-4-5".to_string()];
    provider.default_model = "claude-sonnet-4-5".to_string();

    assert_eq!(config.active_context_window().unwrap(), Some(168_000));
}

#[test]
fn mixed_context_window_uses_the_global_default_when_model_metadata_is_missing() {
    let mut config = AppConfig::default();
    let provider = &mut config.providers[0];
    let provider_id = provider.id.clone();
    provider.models = vec![
        "gqy-known-window-model".to_string(),
        "gqy-unknown-window-model".to_string(),
    ];
    provider.default_model = provider.models[0].clone();
    provider
        .model_context_window
        .insert(provider.models[0].clone(), 200_000);
    config.active_provider_models = Some(vec![
        ActiveProviderModelConfig {
            provider_id: provider_id.clone(),
            model: provider.models[0].clone(),
        },
        ActiveProviderModelConfig {
            provider_id,
            model: provider.models[1].clone(),
        },
    ]);

    assert_eq!(config.active_context_window().unwrap(), Some(168_000));
    config.providers[0]
        .model_context_window
        .insert("gqy-unknown-window-model".to_string(), 128_000);
    assert_eq!(config.active_context_window().unwrap(), Some(128_000));
}

#[test]
fn default_anthropic_provider_has_no_implicit_active_model() {
    let provider = ProviderConfig::default_anthropic();

    assert!(provider.models.is_empty());
    assert!(provider.default_model.is_empty());
}

#[test]
fn normalizes_legacy_anthropic_template_model() {
    let mut config = AppConfig::default();
    let provider = config
        .providers
        .iter_mut()
        .find(|provider| provider.id == "anthropic")
        .unwrap();
    provider.models = vec!["claude-sonnet-4-5".to_string()];
    provider.default_model = "claude-sonnet-4-5".to_string();

    config.normalize_builtin_providers();
    let provider = config
        .providers
        .iter()
        .find(|provider| provider.id == "anthropic")
        .unwrap();

    assert!(provider.models.is_empty());
    assert!(provider.default_model.is_empty());
}

#[test]
fn anthropic_template_does_not_hardcode_model_context_window() {
    let provider = ProviderConfig::default_anthropic();

    assert!(provider.model_context_window.is_empty());
}

#[test]
fn remove_active_provider_model_clears_removed_current_model() {
    let mut config = AppConfig::default();
    let provider_id = config.providers[0].id.clone();
    config.providers[0].models = vec!["old-model".to_string(), "next-model".to_string()];
    config.providers[0].default_model = "old-model".to_string();
    config.providers[0]
        .model_context_window
        .insert("old-model".to_string(), 8192);
    config.providers[0]
        .model_modalities
        .insert("old-model".to_string(), vec!["text".to_string()]);

    config
        .remove_active_provider_model(&provider_id, "old-model")
        .unwrap();

    assert_eq!(config.providers[0].models, vec!["next-model"]);
    assert_eq!(config.providers[0].default_model, "next-model");
    assert!(!config.providers[0]
        .model_context_window
        .contains_key("old-model"));
    assert!(!config.providers[0]
        .model_modalities
        .contains_key("old-model"));
}

#[test]
fn remove_active_provider_model_clears_last_current_model() {
    let mut config = AppConfig::default();
    let provider_id = config.providers[0].id.clone();
    config.providers[0].models = vec!["old-model".to_string()];
    config.providers[0].default_model = "old-model".to_string();

    config
        .remove_active_provider_model(&provider_id, "old-model")
        .unwrap();

    assert!(config.providers[0].models.is_empty());
    assert!(config.providers[0].default_model.is_empty());
    assert!(!config
        .provider_model_choices()
        .iter()
        .any(|choice| choice.provider_id == provider_id));
}

#[test]
fn display_readable_tool_names_defaults_enabled() {
    let display: DisplayConfig = serde_json::from_str(r#"{"tool_calls":"summary"}"#).unwrap();
    assert_eq!(display.language, "auto");
    assert!(display.readable_tool_names);
    assert!(!display.show_token_usage);
    assert_eq!(display.mixed_model_endpoint_display, "interactive");
    assert_eq!(display.command_output_lines, 10);

    let display: DisplayConfig = serde_json::from_str(r#"{"command_output_lines":3}"#).unwrap();
    assert_eq!(display.command_output_lines, 3);
    assert!(serde_json::to_string(&display)
        .unwrap()
        .contains(r#""command_output_lines":3"#));

    let mut config = AppConfig::default();
    config.display.command_output_lines = MAX_COMMAND_OUTPUT_LINES + 1;
    assert!(config.validate().is_err());

    let display: DisplayConfig = serde_json::from_str(r#"{"show_token_usage":true}"#).unwrap();
    assert!(display.show_token_usage);

    let display: DisplayConfig =
        serde_json::from_str(r#"{"show_mixed_model_endpoint":false}"#).unwrap();
    assert_eq!(display.mixed_model_endpoint_display, "off");

    let display: DisplayConfig =
        serde_json::from_str(r#"{"show_mixed_model_endpoint":true}"#).unwrap();
    assert_eq!(display.mixed_model_endpoint_display, "all");
}

#[test]
fn display_language_roundtrips_and_rejects_unknown_values() {
    let display: DisplayConfig = serde_json::from_str(r#"{"language":"zh"}"#).unwrap();
    assert_eq!(display.language, "zh");
    assert!(serde_json::to_string(&display)
        .unwrap()
        .contains(r#""language":"zh""#));

    let mut config = AppConfig::default();
    config.display.language = "fr".to_string();
    assert!(config.validate().is_err());
    config.display.language.clear();
    assert!(config.validate().is_err());
}

#[test]
fn display_language_hint_reads_jsonc_without_loading_full_config() {
    let temp = tempfile::tempdir().unwrap();
    let config_file = temp.path().join("config.jsonc");
    std::fs::write(
        &config_file,
        "{\n  // UI preference\n  \"display\": { \"language\": \"en\" }\n}\n",
    )
    .unwrap();
    let paths = GQYPaths {
        root_dir: temp.path().to_path_buf(),
        config_dir: temp.path().to_path_buf(),
        config_file,
        skills_dir: temp.path().join("skills"),
        data_dir: temp.path().join("data"),
        cache_dir: temp.path().join("cache"),
        state_dir: temp.path().join("state"),
        pictures_dir: temp.path().join("pictures"),
        fish_hook_file: temp.path().join("gqy.fish"),
        bash_hook_file: temp.path().join("gqy.bash"),
        zsh_hook_file: temp.path().join("gqy.zsh"),
        scripts_dir: temp.path().join("scripts"),
        system_scripts_dir: temp.path().join("system-scripts"),
    };

    assert_eq!(
        AppConfig::display_language_hint(&paths).as_deref(),
        Some("en")
    );
}

#[test]
fn meme_library_defaults_follow_persona() {
    let memes = MemesPluginConfig::default();
    assert_eq!(memes.library_for_persona(""), "gqy");
    assert_eq!(
        memes.library_for_persona("Custom Persona"),
        "custom-persona"
    );
    assert!(memes.auto_send_enabled);
    assert!(memes.auto_send_platform_enabled);
    assert_eq!(memes.search_max_results, 1);
    assert_eq!(memes.auto_send_probability, 0.05);
}

#[test]
fn extra_body_roundtrip() {
    let original = ProviderConfig {
        id: "test".to_string(),
        display_name: "Test".to_string(),
        base_url: "https://example.com".to_string(),
        protocol: "auto".to_string(),
        api_key: None,
        models: vec![],
        model_context_window: HashMap::new(),
        model_temperature: HashMap::new(),
        model_modalities: HashMap::new(),
        model_costs: HashMap::new(),
        default_model: String::new(),
        timeout_seconds: 60,
        temperature: 1.0,
        anthropic_max_tokens: 4096,
        extra_body: serde_json::json!({
            "enable_thinking": false,
            "reasoning_effort": "low"
        })
        .as_object()
        .cloned(),
    };

    let serialized = serde_json::to_string(&original).unwrap();
    let deserialized: ProviderConfig = serde_json::from_str(&serialized).unwrap();

    assert_eq!(original.extra_body, deserialized.extra_body);
    assert_eq!(original.id, deserialized.id);
}

#[test]
fn extra_body_rejects_non_object_config_values() {
    for extra_body in [
        serde_json::json!(true),
        serde_json::json!("invalid"),
        serde_json::json!([1, 2, 3]),
    ] {
        let provider = serde_json::json!({
            "id": "test",
            "display_name": "Test",
            "base_url": "https://example.com",
            "extra_body": extra_body
        });

        assert!(serde_json::from_value::<ProviderConfig>(provider).is_err());
    }
}

#[test]
fn memory_diary_lifecycle_defaults_and_roundtrip_are_stable() {
    let defaults: MemoryConfig = serde_json::from_str("{}").unwrap();
    assert_eq!(defaults.diary_batch_size, 14);
    assert_eq!(defaults.short_diary_retention_days, 14);
    assert_eq!(defaults.diary_promotion_recalls, 3);
    assert_eq!(defaults.organizer_timeout_seconds, 120);
    assert!(!defaults.auto_skill_enabled);

    let parsed: MemoryConfig = serde_json::from_str(
        r#"{
                "diary_batch_size": 20,
                "short_diary_retention_days": 7,
                "diary_promotion_recalls": 4,
                "organizer_timeout_seconds": 90
            }"#,
    )
    .unwrap();
    assert_eq!(parsed.diary_batch_size, 20);
    assert_eq!(parsed.short_diary_retention_days, 7);
    assert_eq!(parsed.diary_promotion_recalls, 4);
    assert_eq!(parsed.organizer_timeout_seconds, 90);
}

#[test]
fn default_prompt_resources_follow_the_data_resource_layout() {
    let temp = tempfile::tempdir().unwrap();
    let paths = GQYPaths {
        root_dir: temp.path().to_path_buf(),
        config_dir: temp.path().join("config"),
        config_file: temp.path().join("config/config.jsonc"),
        skills_dir: temp.path().join("data/skills"),
        data_dir: temp.path().join("data"),
        cache_dir: temp.path().join("cache"),
        state_dir: temp.path().join("state"),
        pictures_dir: temp.path().join("data/pictures"),
        fish_hook_file: temp.path().join("fish/gqy.fish"),
        bash_hook_file: temp.path().join("config/shell/bash-hook.sh"),
        zsh_hook_file: temp.path().join("config/shell/zsh-hook.zsh"),
        scripts_dir: temp.path().join("data/scripts"),
        system_scripts_dir: PathBuf::new(),
    };
    let mut config = AppConfig::default();
    assert_eq!(
        config.prompts_dir_path(&paths),
        paths.data_dir.join("prompts")
    );
    assert_eq!(
        config.identities_dir_path(&paths),
        paths.data_dir.join("identities")
    );
    assert_eq!(
        config.user_identity_path(&paths),
        paths.data_dir.join("identities/user-identity.md")
    );
    assert_eq!(
        config.system_prompt_path(&paths),
        paths.data_dir.join("prompts/system-prompt.md")
    );

    config.prompt.prompts_dir = "./prompts/team".to_string();
    config.prompt.identities_dir = "identities/team".to_string();
    config.prompt.user_identity_file = "identities/team/user.md".to_string();
    config.system_prompt_file = Some("prompts/team/system.md".to_string());
    assert_eq!(
        config.prompts_dir_path(&paths),
        paths.data_dir.join("prompts/team")
    );
    assert_eq!(
        config.identities_dir_path(&paths),
        paths.data_dir.join("identities/team")
    );
    assert_eq!(
        config.user_identity_path(&paths),
        paths.data_dir.join("identities/team/user.md")
    );
    assert_eq!(
        config.system_prompt_path(&paths),
        paths.data_dir.join("prompts/team/system.md")
    );

    config.prompt.prompts_dir = "prompts/../scripts/personas".to_string();
    config.prompt.identities_dir = paths
        .config_dir
        .join("identities/team")
        .display()
        .to_string();
    assert_eq!(
        config.prompts_dir_path(&paths),
        paths.data_dir.join("scripts/personas")
    );
    assert_eq!(
        config.identities_dir_path(&paths),
        paths.data_dir.join("identities/team")
    );

    config.prompt.user_identity_file = "./user-identity.md".to_string();
    config.system_prompt_file = Some("./system-prompt.md".to_string());
    assert_eq!(
        config.user_identity_path(&paths),
        paths.data_dir.join("identities/user-identity.md")
    );
    assert_eq!(
        config.system_prompt_path(&paths),
        paths.data_dir.join("prompts/system-prompt.md")
    );

    config.prompt.user_identity_file = paths
        .config_dir
        .join("user-identity.md")
        .display()
        .to_string();
    config.system_prompt_file = Some(
        paths
            .config_dir
            .join("system-prompt.md")
            .display()
            .to_string(),
    );
    assert_eq!(
        config.user_identity_path(&paths),
        paths.data_dir.join("identities/user-identity.md")
    );
    assert_eq!(
        config.system_prompt_path(&paths),
        paths.data_dir.join("prompts/system-prompt.md")
    );

    config.prompt.prompts_dir = "custom-prompts".to_string();
    config.prompt.identities_dir = "custom-identities".to_string();
    config.prompt.user_identity_file = "custom-user.md".to_string();
    config.system_prompt_file = Some("custom-system.md".to_string());
    assert_eq!(
        config.prompts_dir_path(&paths),
        paths.config_dir.join("custom-prompts")
    );
    assert_eq!(
        config.identities_dir_path(&paths),
        paths.config_dir.join("custom-identities")
    );
    assert_eq!(
        config.user_identity_path(&paths),
        paths.config_dir.join("custom-user.md")
    );
    assert_eq!(
        config.system_prompt_path(&paths),
        paths.config_dir.join("custom-system.md")
    );

    let mut deferred_paths = paths.clone();
    deferred_paths.skills_dir = deferred_paths.config_dir.join("skills");
    deferred_paths.scripts_dir = deferred_paths.config_dir.join("scripts");
    let deferred = AppConfig::default();
    assert_eq!(
        deferred.user_identity_path(&deferred_paths),
        deferred_paths.config_dir.join("user-identity.md")
    );
    assert_eq!(
        deferred.system_prompt_path(&deferred_paths),
        deferred_paths.config_dir.join("system-prompt.md")
    );

    let base = directories::BaseDirs::new().unwrap();
    let root = base.home_dir().join(".gqy");
    let mut legacy_paths = paths.clone();
    legacy_paths.config_dir = root.join("config");
    legacy_paths.config_file = root.join("config/config.jsonc");
    legacy_paths.data_dir = root.join("data");
    legacy_paths.skills_dir = root.join("data/skills");
    legacy_paths.scripts_dir = root.join("data/scripts");
    let mut legacy_absolute = AppConfig::default();
    legacy_absolute.prompt.user_identity_file = base
        .config_dir()
        .join("gqy/user-identity.md")
        .display()
        .to_string();
    legacy_absolute.system_prompt_file = Some(
        base.config_dir()
            .join("gqy/system-prompt.md")
            .display()
            .to_string(),
    );
    assert_eq!(
        legacy_absolute.user_identity_path(&legacy_paths),
        root.join("data/identities/user-identity.md")
    );
    assert_eq!(
        legacy_absolute.system_prompt_path(&legacy_paths),
        root.join("data/prompts/system-prompt.md")
    );
}

#[test]
fn reserved_system_prompt_file_is_not_a_persona() {
    let temp = tempfile::tempdir().unwrap();
    let paths = GQYPaths {
        root_dir: temp.path().to_path_buf(),
        config_dir: temp.path().join("config"),
        config_file: temp.path().join("config/config.jsonc"),
        skills_dir: temp.path().join("data/skills"),
        data_dir: temp.path().join("data"),
        cache_dir: temp.path().join("cache"),
        state_dir: temp.path().join("state"),
        pictures_dir: temp.path().join("data/pictures"),
        fish_hook_file: temp.path().join("fish/gqy.fish"),
        bash_hook_file: temp.path().join("config/shell/bash-hook.sh"),
        zsh_hook_file: temp.path().join("config/shell/zsh-hook.zsh"),
        scripts_dir: temp.path().join("data/scripts"),
        system_scripts_dir: PathBuf::new(),
    };
    std::fs::create_dir_all(paths.prompts_dir()).unwrap();
    std::fs::write(paths.prompts_dir().join("system-prompt.md"), "fallback").unwrap();
    std::fs::write(paths.prompts_dir().join("System Prompt.md"), "persona").unwrap();
    let mut config = AppConfig::default();
    assert!(config.validate_persona_files(&paths).is_ok());
    config.prompt.active_persona = "system-prompt.md".to_string();
    assert!(config.validate_persona_files(&paths).is_err());
}
