//! tests — 自 src/config.rs 外移。
#![cfg(test)]

pub(crate) use super::*;

#[cfg(test)]
#[test]
fn model_temperature_override_beats_provider_default() {
    let mut provider = ProviderConfig::default_opencodezen();
    provider.temperature = 0.6;
    provider.default_model = "a".to_string();
    assert_eq!(provider.effective_temperature(), 0.6);
    provider.model_temperature.insert("a".to_string(), 0.1);
    assert_eq!(provider.effective_temperature(), 0.1);
    // 别的模型不受覆盖牵连(验收:曾把供应商全局温度当模型温度写)。
    provider.default_model = "b".to_string();
    assert_eq!(provider.effective_temperature(), 0.6);
}

#[test]
fn a_stale_xdg_output_dir_is_healed_and_its_files_follow() {
    // The value being healed is one an earlier upgrade wrote itself: it
    // remapped onto data_dir while data_dir still pointed at the legacy
    // XDG root, so the old root has to be a legacy root too.
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path();
    let legacy = home.join(".local/share/gqy/pictures/generated-images");
    std::fs::create_dir_all(&legacy).unwrap();
    std::fs::write(legacy.join("one.png"), "a").unwrap();
    std::fs::write(legacy.join("two.png"), "b").unwrap();

    let destination_root = home.join(".gqy/data/pictures");
    let mut value = legacy.display().to_string();
    let moved = remap_managed_output_dir(
        &mut value,
        &[home.join(".local/share/gqy/pictures")],
        &destination_root,
        home,
    );
    let (from, to) = moved.expect("the stale root must be recognised");
    assert_eq!(to, destination_root.join("generated-images"));
    assert_eq!(value, to.display().to_string());

    relocate_managed_output(&from, &to);
    assert!(to.join("one.png").exists());
    assert!(to.join("two.png").exists());
    assert!(
        !from.exists(),
        "an emptied stale directory should not linger"
    );
}

#[test]
fn a_path_outside_every_legacy_root_is_left_alone() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path();
    let mut value = home.join("my-own-folder").display().to_string();
    let before = value.clone();
    let moved = remap_managed_output_dir(
        &mut value,
        &[home.join(".local/share/gqy/pictures")],
        &home.join(".gqy/data/pictures"),
        home,
    );
    assert!(moved.is_none());
    assert_eq!(value, before);
}

#[test]
fn api_quota_partial_provider_configs_keep_defaults() {
    let config: ApiQuotaPluginConfig = serde_json::from_value(serde_json::json!({
        "deepseek": { "api_key": "deepseek-key" },
        "openrouter": { "api_key": "openrouter-key" }
    }))
    .unwrap();
    assert!(config.enabled);
    assert_eq!(config.deepseek.api_key, "deepseek-key");
    assert_eq!(config.openrouter.api_key, "openrouter-key");
}

#[test]
fn api_quota_legacy_key_migrates_to_a_stable_default_account() {
    let mut config = AppConfig::default();
    config.plugins.api_quota.deepseek.accounts.clear();
    config.plugins.api_quota.deepseek.api_key = "legacy-key".to_string();
    config.normalize_api_quota_accounts();
    assert!(config.plugins.api_quota.deepseek.api_key.is_empty());
    assert_eq!(config.plugins.api_quota.deepseek.accounts.len(), 1);
    assert_eq!(
        config.plugins.api_quota.deepseek.accounts[0].id,
        "account-1"
    );
    assert_eq!(
        config.plugins.api_quota.deepseek.accounts[0].api_key,
        "legacy-key"
    );
}

#[test]
fn api_quota_mixed_config_preserves_both_keys() {
    let mut config = ApiQuotaProviderConfig::default();
    config.accounts[0].api_key = "new-key".to_string();
    config.api_key = "legacy-key".to_string();
    normalize_api_quota_provider(&mut config);
    assert!(config.api_key.is_empty());
    assert_eq!(config.accounts.len(), 2);
    assert_eq!(config.accounts[0].api_key, "new-key");
    assert_eq!(config.accounts[1].api_key, "legacy-key");
    assert_ne!(config.accounts[0].id, config.accounts[1].id);
}

#[test]
fn api_quota_account_names_must_be_unique() {
    let mut config = AppConfig::default();
    config.plugins.api_quota.deepseek.accounts = vec![
        ApiQuotaAccountConfig {
            id: "first".to_string(),
            name: "账号".to_string(),
            api_key: "first".to_string(),
        },
        ApiQuotaAccountConfig {
            id: "second".to_string(),
            name: "账号".to_string(),
            api_key: "second".to_string(),
        },
    ];
    assert!(config.validate().is_err());
}

#[test]
fn context_overflow_defaults_to_compact() {
    assert_eq!(ContextConfig::default().on_overflow, "compact");

    let deserialized: ContextConfig = serde_json::from_value(serde_json::json!({})).unwrap();
    assert_eq!(deserialized.on_overflow, "compact");
}

#[test]
fn vision_timeouts_have_stable_defaults() {
    let vision: VisionPluginConfig = serde_json::from_value(serde_json::json!({})).unwrap();
    assert_eq!(vision.response_header_timeout_seconds, 15);
    assert_eq!(vision.stream_idle_timeout_seconds, 20);
    assert_eq!(vision.image_timeout_seconds, 60);
}

#[test]
fn provider_config_can_be_saved_without_active_model() {
    let mut config = AppConfig::default();
    config.providers[0].models.clear();
    config.providers[0].default_model.clear();
    assert!(config.validate().is_ok());
}

#[test]
fn provider_model_choices_ignore_unconfigured_models() {
    let mut config = AppConfig::default();
    let provider_id = config.providers[0].id.clone();
    config.providers[0].models.clear();
    config.providers[0].default_model.clear();

    assert!(!config
        .provider_model_choices()
        .iter()
        .any(|choice| choice.provider_id == provider_id));
}

#[test]
fn active_provider_models_are_replaced_as_one_validated_pool() {
    let mut config = AppConfig::default();
    let provider_id = config.providers[0].id.clone();
    config.providers[0].models = vec!["model-a".to_string(), "model-b".to_string()];
    config.providers[0].default_model = "model-a".to_string();
    let before = config.active_provider_models.clone();

    let invalid = vec![
        ActiveProviderModelConfig {
            provider_id: provider_id.clone(),
            model: "model-a".to_string(),
        },
        ActiveProviderModelConfig {
            provider_id: provider_id.clone(),
            model: "missing".to_string(),
        },
    ];
    assert!(config.set_active_provider_models(&invalid).is_err());
    assert_eq!(config.active_provider_models, before);

    let selected = vec![
        ActiveProviderModelConfig {
            provider_id: provider_id.clone(),
            model: "model-b".to_string(),
        },
        ActiveProviderModelConfig {
            provider_id,
            model: "model-a".to_string(),
        },
    ];
    config.set_active_provider_models(&selected).unwrap();
    assert_eq!(
        config.active_provider_models.as_deref(),
        Some(selected.as_slice())
    );
}

#[test]
fn legacy_provider_temperatures_migrate_once() {
    let mut config = AppConfig {
        config_version: 0,
        ..AppConfig::default()
    };
    config.providers[0].temperature = LEGACY_DEFAULT_TEMPERATURE;
    config.providers[1].temperature = 0.5;

    config.migrate().unwrap();

    assert_eq!(config.config_version, CURRENT_CONFIG_VERSION);
    assert_eq!(config.providers[0].temperature, 1.0);
    assert_eq!(config.providers[1].temperature, 0.5);

    config.providers[0].temperature = LEGACY_DEFAULT_TEMPERATURE;
    config.migrate().unwrap();
    assert_eq!(config.providers[0].temperature, LEGACY_DEFAULT_TEMPERATURE);

    config.config_version = CURRENT_CONFIG_VERSION + 1;
    assert!(config.migrate().is_err());
}

#[test]
fn empty_active_provider_models_normalizes_to_default_chat_model() {
    let mut config = AppConfig {
        active_provider_models: Some(Vec::new()),
        ..Default::default()
    };

    config.normalize_builtin_providers();

    let choices = config.active_provider_model_choices();
    assert_eq!(choices.len(), 1);
    assert_eq!(choices[0].provider_id, OPENCODE_PROVIDER_ID);
    assert_eq!(choices[0].model, OPENCODE_DEFAULT_CHAT_MODEL);
}

#[test]
fn active_provider_model_choices_ignore_stale_models() {
    let mut config = AppConfig::default();
    let provider_id = config.providers[0].id.clone();
    config.providers[0].models = vec!["deepseek-v4-flash-free".to_string()];
    config.providers[0].default_model = "deepseek-v4-flash-free".to_string();
    config.active_provider_models = Some(vec![
        ActiveProviderModelConfig {
            provider_id: provider_id.clone(),
            model: "mimo-v2.5-free".to_string(),
        },
        ActiveProviderModelConfig {
            provider_id: provider_id.clone(),
            model: "deepseek-v4-flash-free".to_string(),
        },
    ]);

    let choices = config.active_provider_model_choices();

    assert_eq!(choices.len(), 1);
    assert_eq!(choices[0].provider_id, provider_id);
    assert_eq!(choices[0].model, "deepseek-v4-flash-free");
}

#[test]
fn normalize_prunes_stale_active_provider_models() {
    let mut config = AppConfig::default();
    let provider_id = config.providers[0].id.clone();
    config.providers[0].models = vec!["deepseek-v4-flash-free".to_string()];
    config.providers[0].default_model = "deepseek-v4-flash-free".to_string();
    config.active_provider_models = Some(vec![
        ActiveProviderModelConfig {
            provider_id: provider_id.clone(),
            model: "mimo-v2.5-free".to_string(),
        },
        ActiveProviderModelConfig {
            provider_id: provider_id.clone(),
            model: "deepseek-v4-flash-free".to_string(),
        },
    ]);

    config.normalize_builtin_providers();

    assert_eq!(
        config.active_provider_models,
        Some(vec![ActiveProviderModelConfig {
            provider_id,
            model: "deepseek-v4-flash-free".to_string(),
        }])
    );
}

#[test]
fn remove_active_model_references_clears_text_and_multimodal() {
    let mut config = AppConfig::default();
    let provider_id = config.providers[0].id.clone();
    config.active_provider_models = Some(vec![ActiveProviderModelConfig {
        provider_id: provider_id.clone(),
        model: "old-model".to_string(),
    }]);
    config.active_multimodal_provider_models = Some(vec![ActiveProviderModelConfig {
        provider_id: provider_id.clone(),
        model: "old-model".to_string(),
    }]);

    config.remove_active_model_references(&provider_id, "old-model");

    assert_eq!(config.active_provider_models, None);
    assert_eq!(config.active_multimodal_provider_models, None);
}

#[test]
fn multimodal_provider_model_choices_use_input_modalities() {
    let mut config = AppConfig::default();
    let provider = &mut config.providers[0];
    provider.models = vec![
        "text-only".to_string(),
        "audio-only".to_string(),
        "vision-model".to_string(),
    ];
    provider
        .model_modalities
        .insert("text-only".to_string(), vec!["text".to_string()]);
    provider.model_modalities.insert(
        "audio-only".to_string(),
        vec!["text".to_string(), "audio".to_string()],
    );
    provider.model_modalities.insert(
        "vision-model".to_string(),
        vec!["text".to_string(), "image".to_string()],
    );

    let choices = config.multimodal_provider_model_choices();

    assert!(choices.iter().any(|choice| choice.model == "vision-model"));
    assert!(!choices.iter().any(|choice| choice.model == "text-only"));
    assert!(!choices.iter().any(|choice| choice.model == "audio-only"));
}

#[test]
fn active_multimodal_pool_rejects_and_prunes_non_image_models() {
    let mut config = AppConfig::default();
    let provider_id = config.providers[0].id.clone();
    config.providers[0]
        .models
        .extend(["audio-only".to_string(), "vision-model".to_string()]);
    config.providers[0].model_modalities.insert(
        "audio-only".to_string(),
        vec!["text".to_string(), "audio".to_string()],
    );
    config.providers[0].model_modalities.insert(
        "vision-model".to_string(),
        vec!["text".to_string(), "image".to_string()],
    );

    assert!(config
        .toggle_active_multimodal_provider_model(&provider_id, "audio-only")
        .is_err());
    assert!(config
        .toggle_active_multimodal_provider_model(&provider_id, "vision-model")
        .unwrap());
    config
        .active_multimodal_provider_models
        .as_mut()
        .unwrap()
        .push(ActiveProviderModelConfig {
            provider_id,
            model: "audio-only".to_string(),
        });
    assert!(config.validate_global_multimodal_config().is_err());

    config.normalize_builtin_providers();

    let active = config.active_multimodal_provider_models.unwrap();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].model, "vision-model");
}

#[test]
fn vision_provider_choice_prefers_multimodal_pool_then_default_mimo() {
    let mut config = AppConfig::default();
    config.providers[0].models.push("vision-model".to_string());
    config.providers[0].model_modalities.insert(
        "vision-model".to_string(),
        vec!["text".to_string(), "image".to_string()],
    );
    config.active_multimodal_provider_models = Some(vec![ActiveProviderModelConfig {
        provider_id: OPENCODE_PROVIDER_ID.to_string(),
        model: "vision-model".to_string(),
    }]);

    assert_eq!(
        config.vision_provider_choice().unwrap(),
        (OPENCODE_PROVIDER_ID.to_string(), "vision-model".to_string())
    );

    config.active_multimodal_provider_models = Some(Vec::new());
    assert_eq!(
        config.vision_provider_choice().unwrap(),
        (
            OPENCODE_PROVIDER_ID.to_string(),
            OPENCODE_DEFAULT_VISION_MODEL.to_string()
        )
    );
}

#[test]
fn vision_provider_choice_rejects_an_audio_only_active_pool() {
    let mut config = AppConfig::default();
    let provider_id = config.providers[0].id.clone();
    config.providers[0].models.push("audio-only".to_string());
    config.providers[0].model_modalities.insert(
        "audio-only".to_string(),
        vec!["text".to_string(), "audio".to_string()],
    );
    config.active_multimodal_provider_models = Some(vec![ActiveProviderModelConfig {
        provider_id,
        model: "audio-only".to_string(),
    }]);

    assert!(config.vision_provider_choice().is_err());
    assert!(config.validate().is_err());
}

#[test]
fn vision_provider_choice_rejects_an_explicit_non_image_model() {
    let mut config = AppConfig::default();
    let provider_id = config.providers[0].id.clone();
    config.providers[0].models.push("audio-only".to_string());
    config.providers[0].model_modalities.insert(
        "audio-only".to_string(),
        vec!["text".to_string(), "audio".to_string()],
    );
    config.plugins.vision.vision_provider_id = provider_id;
    config.plugins.vision.vision_model = "audio-only".to_string();

    assert!(config.vision_provider_choice().is_err());
    assert!(config.validate().is_err());
}

#[test]
fn subagent_tier_pools_toggle_filter_and_prune() {
    let mut config = AppConfig::default();
    let provider_id = config.providers[0].id.clone();
    config.providers[0].models.push("mini-a".to_string());
    config.providers[0].models.push("mini-b".to_string());

    // Unconfigured pool resolves empty.
    assert!(config.subagent_tier_choices(ModelTier::Cheap).is_empty());

    // Toggle in/out mirrors the text-model picker semantics.
    assert!(config
        .toggle_subagent_tier_model(ModelTier::Cheap, &provider_id, "mini-a")
        .unwrap());
    assert!(config
        .toggle_subagent_tier_model(ModelTier::Cheap, &provider_id, "mini-b")
        .unwrap());
    assert!(config.is_subagent_tier_model(ModelTier::Cheap, &provider_id, "mini-a"));
    let choices = config.subagent_tier_choices(ModelTier::Cheap);
    assert_eq!(
        choices.iter().map(|c| c.model.as_str()).collect::<Vec<_>>(),
        vec!["mini-a", "mini-b"]
    );
    assert!(!config
        .toggle_subagent_tier_model(ModelTier::Cheap, &provider_id, "mini-b")
        .unwrap());
    assert_eq!(config.subagent_tier_choices(ModelTier::Cheap).len(), 1);

    // Unknown provider is rejected.
    assert!(config
        .toggle_subagent_tier_model(ModelTier::Strong, "no-such", "x")
        .is_err());

    // A model removed from the text models leaves the pool too.
    config
        .toggle_subagent_tier_model(ModelTier::Balanced, &provider_id, "mini-a")
        .unwrap();
    config
        .remove_active_provider_model(&provider_id, "mini-a")
        .unwrap();
    assert!(config.subagent_tier_choices(ModelTier::Cheap).is_empty());
    assert!(config.subagent_tiers.pool(ModelTier::Cheap).is_empty());
    assert!(config.subagent_tiers.pool(ModelTier::Balanced).is_empty());

    // prune_subagent_tiers drops entries that no longer resolve.
    config
        .toggle_subagent_tier_model(ModelTier::Cheap, &provider_id, "mini-b")
        .unwrap();
    config.providers[0].models.retain(|m| m != "mini-b");
    assert!(config.subagent_tier_choices(ModelTier::Cheap).is_empty());
    config.prune_subagent_tiers();
    assert!(config.subagent_tiers.pool(ModelTier::Cheap).is_empty());
}

#[test]
fn subagent_tiers_roundtrip_and_default_omission() {
    let config = AppConfig::default();
    let json = serde_json::to_string(&config).unwrap();
    // Empty pools stay out of the serialized config.
    assert!(!json.contains("subagent_tiers"));

    let parsed: AppConfig = serde_json::from_str(
        r#"{
            "active_provider": "opencode",
            "providers": [],
            "subagent_tiers": {
                "cheap": [ { "provider_id": "p", "model": "m" } ]
            }
        }"#,
    )
    .unwrap();
    assert_eq!(parsed.subagent_tiers.cheap.len(), 1);
    assert_eq!(parsed.subagent_tiers.cheap[0].model, "m");
    assert!(parsed.subagent_tiers.balanced.is_empty());
    // Choices filter out entries with unknown providers.
    assert!(parsed.subagent_tier_choices(ModelTier::Cheap).is_empty());
}

#[test]
fn platforms_config_roundtrip_and_default_omission() {
    let config = AppConfig::default();
    let json = serde_json::to_string(&config).unwrap();
    // An untouched platforms config stays out of the serialized file.
    assert!(!json.contains("platforms"));

    let mut parsed: AppConfig = serde_json::from_str(
        r#"{
            "active_provider": "opencode",
            "providers": [],
            "platforms": {
                "command_prefix": "!",
                "commands": {
                    "reset": { "permission": "everyone" }
                },
                "qq": {
                    "enabled": true,
                    "reverse_ws_port": 8400,
                    "access_token": "secret",
                    "admin_users": [9988],
                    "asset_base_url": "https://assets.example.test",
                    "memory": {
                        "write_enabled": false
                    },
                    "private_chats": {
                        "whitelist": [12345],
                        "friend_requests_require_private_whitelist": false,
                        "allow_non_whitelist": false,
                        "non_whitelist_rate_per_minute": 4
                    },
                    "group_chats": {
                        "whitelist": [54321],
                        "trigger_keywords": ["GQY"],
                        "whitelist_rate_per_minute": 30,
                        "allow_non_whitelist": true,
                        "non_whitelist_rate_per_minute": 10
                    }
                }
            }
        }"#,
    )
    .unwrap();
    parsed.normalize_platform_model_routes();
    let qq = &parsed.platforms.qq;
    assert_eq!(parsed.platforms.command_prefix, "!");
    assert_eq!(
        parsed
            .platforms
            .command_permission("reset", PlatformCommandPermission::AdminOnly),
        PlatformCommandPermission::Everyone
    );
    assert!(qq.enabled);
    assert_eq!(qq.reverse_ws_port, 8400);
    assert_eq!(qq.access_token, "secret");
    assert_eq!(qq.admin_users, vec![9988]);
    assert!(qq.user_identification);
    assert!(qq.show_group_name);
    assert!(!qq.memory.write_enabled);
    assert_eq!(qq.asset_base_url, "https://assets.example.test");
    assert_eq!(qq.private_chats.whitelist, vec![12345]);
    assert!(!qq.private_chats.friend_requests_require_private_whitelist);
    assert!(!qq.private_chats.allow_non_whitelist);
    assert_eq!(
        qq.private_chats.non_whitelist_rate_limit,
        PlatformRateLimit {
            max_messages: 4,
            window_seconds: 60,
        }
    );
    assert_eq!(qq.group_chats.whitelist, vec![54321]);
    assert_eq!(qq.group_chats.trigger_keywords, vec!["GQY"]);
    assert_eq!(qq.group_chats.whitelist_rate_limit.max_messages, 30);
    assert_eq!(qq.group_chats.non_whitelist_rate_limit.max_messages, 10);
    assert_eq!(qq.max_reply_chars, 3000);

    // Round-trip preserves the non-default config.
    let json = serde_json::to_string(&parsed).unwrap();
    let reparsed: AppConfig = serde_json::from_str(&json).unwrap();
    assert_eq!(reparsed.platforms, parsed.platforms);

    // The retired protocol-shaped key is a clean break and does not
    // silently enable Tencent QQ under the new defaults.
    let legacy: AppConfig = serde_json::from_str(
        r#"{"active_provider":"opencode","providers":[],"platforms":{"onebot":{"enabled":true}}}"#,
    )
    .unwrap();
    assert!(!legacy.platforms.qq.enabled);
    assert_eq!(legacy.platforms.command_prefix, "/");
    assert!(legacy.platforms.commands.is_empty());

    let missing_friend_request_setting: AppConfig = serde_json::from_str(
        r#"{
            "active_provider": "opencode",
            "providers": [],
            "platforms": {
                "qq": {
                    "private_chats": { "whitelist": [12345] }
                }
            }
        }"#,
    )
    .unwrap();
    assert!(
        missing_friend_request_setting
            .platforms
            .qq
            .private_chats
            .friend_requests_require_private_whitelist
    );
}

#[test]
fn qq_prompt_identity_options_default_on_and_roundtrip() {
    let defaults: OneBotConfig = serde_json::from_str("{}").unwrap();
    assert!(defaults.user_identification);
    assert!(defaults.show_group_name);
    assert!(defaults.memory.write_enabled);

    let disabled = OneBotConfig {
        user_identification: false,
        show_group_name: false,
        ..Default::default()
    };
    let json = serde_json::to_value(&disabled).unwrap();
    assert_eq!(json["user_identification"], false);
    assert_eq!(json["show_group_name"], false);
    assert_eq!(
        serde_json::from_value::<OneBotConfig>(json).unwrap(),
        disabled
    );
}

#[test]
fn platform_command_defaults_overrides_and_validation() {
    let mut config = AppConfig::default();
    assert_eq!(config.platforms.command_prefix, "/");
    assert_eq!(
        config
            .platforms
            .command_permission("reset", PlatformCommandPermission::AdminOnly),
        PlatformCommandPermission::AdminOnly
    );
    config.platforms.set_command_permission(
        "reset",
        PlatformCommandPermission::Everyone,
        PlatformCommandPermission::AdminOnly,
    );
    assert_eq!(
        config.platforms.commands["reset"].permission,
        PlatformCommandPermission::Everyone
    );
    config.platforms.set_command_permission(
        "reset",
        PlatformCommandPermission::AdminOnly,
        PlatformCommandPermission::AdminOnly,
    );
    assert!(config.platforms.commands.is_empty());

    for invalid in [
        "",
        " ",
        "/ reset",
        "\n",
        "/////////////////////////////////",
    ] {
        config.platforms.command_prefix = invalid.to_string();
        assert!(
            config.validate().is_err(),
            "prefix should be invalid: {invalid:?}"
        );
    }
    config.platforms.command_prefix = "/".to_string();
    config
        .platforms
        .commands
        .insert("Reset".to_string(), PlatformCommandConfig::default());
    assert!(config.validate().is_err());
}

pub(crate) fn route_test_config() -> AppConfig {
    let mut config = AppConfig::default();
    let provider = &mut config.providers[0];
    provider.models = vec!["text-only".to_string(), "vision".to_string()];
    provider.default_model = "text-only".to_string();
    provider
        .model_modalities
        .insert("text-only".to_string(), vec!["text".to_string()]);
    provider.model_modalities.insert(
        "vision".to_string(),
        vec!["text".to_string(), "image".to_string()],
    );
    config
}

pub(crate) fn test_route(config: &AppConfig) -> PlatformModelRoute {
    PlatformModelRoute {
        conversation: PlatformConversationConfig {
            kind: PlatformConversationKind::Group,
            id: "20002".to_string(),
        },
        persona: PlatformPersonaOverride::Inherit,
        text_models_inheritance: PlatformModelPoolInheritance::Platform,
        text_models: Some(vec![ActiveProviderModelConfig {
            provider_id: config.providers[0].id.clone(),
            model: "text-only".to_string(),
        }]),
        multimodal_models_inheritance: PlatformModelPoolInheritance::Platform,
        multimodal_models: Some(vec![ActiveProviderModelConfig {
            provider_id: config.providers[0].id.clone(),
            model: "vision".to_string(),
        }]),
        extra_prompt: "Reply naturally in this group.".to_string(),
        session_limits: None,
    }
}

#[test]
fn qq_platform_model_pools_validate_and_round_trip() {
    let mut config = route_test_config();
    let provider_id = config.providers[0].id.clone();
    config.platforms.qq.text_models = Some(vec![ActiveProviderModelConfig {
        provider_id: provider_id.clone(),
        model: "text-only".to_string(),
    }]);
    config.platforms.qq.non_whitelist_text_models = Some(vec![ActiveProviderModelConfig {
        provider_id: provider_id.clone(),
        model: "text-only".to_string(),
    }]);
    config.platforms.qq.multimodal_models = Some(vec![ActiveProviderModelConfig {
        provider_id,
        model: "vision".to_string(),
    }]);

    assert!(config.validate().is_ok());
    let value = serde_json::to_value(&config).unwrap();
    let reparsed: AppConfig = serde_json::from_value(value).unwrap();
    assert_eq!(
        reparsed.platforms.qq.text_models,
        config.platforms.qq.text_models
    );
    assert_eq!(
        reparsed.platforms.qq.multimodal_models,
        config.platforms.qq.multimodal_models
    );
    assert_eq!(
        reparsed.platforms.qq.non_whitelist_text_models,
        config.platforms.qq.non_whitelist_text_models
    );

    config.platforms.qq.multimodal_models.as_mut().unwrap()[0].model = "text-only".to_string();
    assert!(config.validate().is_err());
    config.platforms.qq.multimodal_models.as_mut().unwrap()[0].model = "vision".to_string();
    config
        .platforms
        .qq
        .non_whitelist_text_models
        .as_mut()
        .unwrap()[0]
        .model = "missing".to_string();
    assert!(config.validate().is_err());
}
