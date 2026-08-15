//! tests2 — 自 src/config.rs 外移。
#![cfg(test)]

use super::*;

    fn qq_non_whitelist_model_pool_normalizes_for_dynamic_inheritance() {
        let mut config = route_test_config();
        let provider_id = config.providers[0].id.clone();
        config.platforms.qq.non_whitelist_text_models = Some(vec![
            ActiveProviderModelConfig {
                provider_id: format!(" {provider_id} "),
                model: " text-only ".to_string(),
            },
            ActiveProviderModelConfig {
                provider_id: provider_id.clone(),
                model: "text-only".to_string(),
            },
        ]);

        config.normalize_platform_model_routes();
        assert_eq!(
            config
                .platforms
                .qq
                .non_whitelist_text_models
                .as_ref()
                .unwrap()
                .len(),
            1
        );

        config.platforms.qq.non_whitelist_text_models = Some(Vec::new());
        config.normalize_platform_model_routes();
        assert!(config.platforms.qq.non_whitelist_text_models.is_none());
    }

    #[test]
    fn qq_session_limits_resolve_from_conversation_then_kind_then_platform() {
        let mut qq = OneBotConfig::default();
        assert_eq!(qq.session_limits.running, 8);
        assert_eq!(qq.session_limits.queued, 16);
        qq.session_limits = PlatformSessionLimits {
            running: 2,
            queued: 3,
        };
        qq.group_chats.session_limits = Some(PlatformSessionLimits {
            running: 3,
            queued: 5,
        });
        qq.conversations.push(PlatformModelRoute {
            conversation: PlatformConversationConfig {
                kind: PlatformConversationKind::Group,
                id: "42".to_string(),
            },
            persona: PlatformPersonaOverride::Inherit,
            text_models_inheritance: PlatformModelPoolInheritance::Platform,
            text_models: None,
            multimodal_models_inheritance: PlatformModelPoolInheritance::Platform,
            multimodal_models: None,
            extra_prompt: String::new(),
            session_limits: Some(PlatformSessionLimits {
                running: 4,
                queued: 7,
            }),
        });
        assert_eq!(
            qq.session_limits(PlatformConversationKind::Group, "42"),
            PlatformSessionLimits {
                running: 4,
                queued: 7
            }
        );
        assert_eq!(
            qq.session_limits(PlatformConversationKind::Group, "43"),
            PlatformSessionLimits {
                running: 3,
                queued: 5
            }
        );
        assert_eq!(
            qq.session_limits(PlatformConversationKind::Private, "42"),
            PlatformSessionLimits {
                running: 2,
                queued: 3
            }
        );
    }

    #[test]
    fn qq_text_model_pool_resolution_preserves_conversation_priority() {
        let mut config = route_test_config();
        let provider_id = config.providers[0].id.clone();
        let pool = |model: &str| {
            vec![ActiveProviderModelConfig {
                provider_id: provider_id.clone(),
                model: model.to_string(),
            }]
        };
        config.active_provider_models = Some(pool("global"));
        config.active_multimodal_provider_models = Some(pool("global-media"));
        config.platforms.qq.text_models = Some(pool("platform"));
        config.platforms.qq.multimodal_models = Some(pool("platform-media"));
        config.platforms.qq.non_whitelist_text_models = Some(pool("non-whitelist"));
        config.platforms.qq.conversations.push(PlatformModelRoute {
            conversation: PlatformConversationConfig {
                kind: PlatformConversationKind::Group,
                id: "20002".to_string(),
            },
            persona: PlatformPersonaOverride::Inherit,
            text_models_inheritance: PlatformModelPoolInheritance::Platform,
            text_models: Some(pool("conversation")),
            multimodal_models_inheritance: PlatformModelPoolInheritance::Platform,
            multimodal_models: None,
            extra_prompt: String::new(),
            session_limits: None,
        });

        {
            let resolved = |conversation_id, use_non_whitelist_pool| {
                config
                    .qq_text_model_pool(
                        PlatformConversationKind::Group,
                        conversation_id,
                        use_non_whitelist_pool,
                    )
                    .unwrap()[0]
                    .model
                    .as_str()
            };
            assert_eq!(resolved("20002", true), "conversation");
            assert_eq!(resolved("30003", true), "non-whitelist");
            assert_eq!(resolved("30003", false), "platform");
        }
        assert_eq!(
            config
                .qq_multimodal_model_pool(PlatformConversationKind::Group, "20002")
                .unwrap()[0]
                .model,
            "platform-media"
        );
        let route = &mut config.platforms.qq.conversations[0];
        route.text_models = None;
        route.text_models_inheritance = PlatformModelPoolInheritance::Global;
        assert_eq!(
            config
                .qq_text_model_pool(PlatformConversationKind::Group, "20002", true)
                .unwrap()[0]
                .model,
            "global"
        );
        assert_eq!(
            config
                .qq_multimodal_model_pool(PlatformConversationKind::Group, "20002")
                .unwrap()[0]
                .model,
            "platform-media"
        );
        config.platforms.qq.conversations[0].multimodal_models_inheritance =
            PlatformModelPoolInheritance::Global;
        assert_eq!(
            config
                .qq_multimodal_model_pool(PlatformConversationKind::Group, "20002")
                .unwrap()[0]
                .model,
            "global-media"
        );
        config.platforms.qq.non_whitelist_text_models = None;
        assert_eq!(
            config
                .qq_text_model_pool(PlatformConversationKind::Group, "30003", true)
                .unwrap()[0]
                .model,
            "platform"
        );
        config.platforms.qq.text_models = None;
        assert_eq!(
            config
                .qq_text_model_pool(PlatformConversationKind::Group, "30003", true)
                .unwrap()[0]
                .model,
            "global"
        );
    }

    #[test]
    fn qq_model_pool_inheritance_is_backward_compatible_and_round_trips() {
        let mut route: PlatformModelRoute = serde_json::from_value(serde_json::json!({
            "conversation": { "kind": "private", "id": "42" }
        }))
        .unwrap();
        assert_eq!(
            route.text_models_inheritance,
            PlatformModelPoolInheritance::Platform
        );
        assert_eq!(
            route.multimodal_models_inheritance,
            PlatformModelPoolInheritance::Platform
        );
        let legacy_value = serde_json::to_value(&route).unwrap();
        assert!(legacy_value.get("text_models_inheritance").is_none());
        assert!(legacy_value.get("multimodal_models_inheritance").is_none());

        route.text_models_inheritance = PlatformModelPoolInheritance::Global;
        route.multimodal_models_inheritance = PlatformModelPoolInheritance::Global;
        let value = serde_json::to_value(&route).unwrap();
        assert_eq!(value["text_models_inheritance"], "global");
        assert_eq!(value["multimodal_models_inheritance"], "global");
        assert_eq!(
            serde_json::from_value::<PlatformModelRoute>(value).unwrap(),
            route
        );
    }

    #[test]
    fn qq_conversation_persona_override_is_explicit_and_tracks_renames() {
        let mut config = route_test_config();
        config.prompt.active_persona = "Global.md".to_string();
        let mut route = test_route(&config);
        route.persona = PlatformPersonaOverride::Custom {
            name: "Group.md".to_string(),
        };
        config.platforms.qq.conversations.push(route);

        let mut effective = config.clone();
        effective.apply_qq_conversation_persona(PlatformConversationKind::Group, "20002");
        assert_eq!(effective.prompt.active_persona, "Group.md");
        assert_eq!(config.platforms.persona_reference_count("Group.md"), 1);

        config
            .platforms
            .rename_persona_references("Group.md", "Renamed.md");
        assert_eq!(
            config.platforms.qq.conversations[0].persona.custom_name(),
            Some("Renamed.md")
        );
        assert!(config.validate().is_ok());

        config.platforms.qq.conversations[0].persona = PlatformPersonaOverride::GQY;
        config.apply_qq_conversation_persona(PlatformConversationKind::Group, "20002");
        assert!(config.prompt.active_persona.is_empty());
    }

    #[test]
    fn qq_conversation_persona_rejects_unsafe_custom_names() {
        let mut config = route_test_config();
        let mut route = test_route(&config);
        route.persona = PlatformPersonaOverride::Custom {
            name: "../persona.md".to_string(),
        };
        config.platforms.qq.conversations.push(route);
        assert!(config.validate().is_err());
    }

    #[test]
    fn platform_model_routes_roundtrip_lookup_and_plugin_shape() {
        let mut config = route_test_config();
        let route = test_route(&config);
        config.platforms.upsert_model_route(route.clone());
        config.platforms.qq.plugins.insert(
            "reply_processor".to_string(),
            PlatformPluginInstanceConfig {
                enabled: Some(false),
                settings: serde_json::json!({"threshold": 150})
                    .as_object()
                    .unwrap()
                    .clone(),
            },
        );

        let found = config
            .platform_model_route(PlatformConversationKind::Group, "20002")
            .unwrap();
        assert_eq!(found, &route);
        assert!(config.validate().is_ok());

        let json = serde_json::to_string(&config).unwrap();
        let reparsed: AppConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(reparsed.platforms, config.platforms);
        assert_eq!(
            reparsed.platforms.qq.plugins["reply_processor"].enabled,
            Some(false)
        );
        assert_eq!(
            reparsed.platforms.qq.plugins["reply_processor"].settings["threshold"],
            150
        );
    }

    #[test]
    fn built_in_platform_plugin_settings_are_validated() {
        let mut config = AppConfig::default();
        config.platforms.qq.plugins.insert(
            "reply_processor".to_string(),
            PlatformPluginInstanceConfig {
                enabled: None,
                settings: serde_json::json!({"threshold": 0, "mode": "invalid"})
                    .as_object()
                    .unwrap()
                    .clone(),
            },
        );
        assert!(config.validate().is_err());

        config
            .platforms
            .qq
            .plugins
            .get_mut("reply_processor")
            .unwrap()
            .settings = serde_json::json!({
            "threshold": 150,
            "mode": "image",
            "future_option": 1
        })
        .as_object()
        .unwrap()
        .clone();
        assert!(config.validate().is_ok());

        config.platforms.qq.plugins.insert(
            QQ_MEME_COLLECTOR_PLUGIN_ID.to_string(),
            PlatformPluginInstanceConfig {
                enabled: Some(true),
                settings: serde_json::json!({
                    "collect_probability": 0.02,
                    "max_images_per_message": 2
                })
                .as_object()
                .unwrap()
                .clone(),
            },
        );
        assert!(config.validate().is_ok());
        config
            .platforms
            .qq
            .plugins
            .get_mut(QQ_MEME_COLLECTOR_PLUGIN_ID)
            .unwrap()
            .settings
            .insert("collect_probability".to_string(), serde_json::json!(1.01));
        assert!(config.validate().is_err());
    }

    #[test]
    fn qq_meme_collector_defaults_are_conservative() {
        let settings = QqMemeCollectorPluginSettings::default();
        assert_eq!(settings.collect_probability, 0.02);
        assert_eq!(settings.max_images_per_message, 2);
        assert!(!settings.allow_non_admin_save_tool);
    }

    #[test]
    fn qq_message_history_defaults_to_full_text_recording() {
        let settings = QqMessageHistoryPluginSettings::default();

        assert_eq!(settings.history_search_max_results, 0);
        assert_eq!(settings.history_safe_page_limit, 500);
        assert!(settings.allow_cross_conversation_search);
        assert!(settings.validate().is_ok());
    }

    #[test]
    fn legacy_real_context_history_limits_move_to_message_history() {
        let mut config = AppConfig::default();
        config.platforms.qq.plugins.insert(
            REAL_CONTEXT_PLUGIN_ID.to_string(),
            PlatformPluginInstanceConfig {
                enabled: None,
                settings: serde_json::json!({
                    "history_search_max_results": 25,
                    "history_safe_page_limit": 250,
                    "allow_cross_group_search": false
                })
                .as_object()
                .unwrap()
                .clone(),
            },
        );

        config.normalize_platform_model_routes();

        let history = QqMessageHistoryPluginSettings::from_instance(
            &config.platforms.qq.plugins[QQ_MESSAGE_HISTORY_PLUGIN_ID],
        )
        .unwrap();
        assert_eq!(history.history_search_max_results, 25);
        assert_eq!(history.history_safe_page_limit, 250);
        assert!(!history.allow_cross_conversation_search);
        assert!(config
            .platforms
            .qq
            .plugins
            .get(REAL_CONTEXT_PLUGIN_ID)
            .is_none());
    }

    #[test]
    fn real_context_defaults_match_the_deployed_contract() {
        let settings = RealContextPluginSettings::default();

        assert_eq!(settings.reply_context_window, 25);
        assert_eq!(settings.judge_context_window, 20);
        assert_eq!(settings.group_member_search_max_results, 200);
        assert!(settings.active_reply_enable);
        assert!(settings.judge_include_persona);
        assert!(settings.judge_persona_prompt.is_empty());
        assert!(settings.text_models.is_none());
        assert_eq!(settings.active_judge_probability, 0.05);
        assert_eq!(settings.reply_threshold, 0.8);
        assert_eq!(settings.judge_timeout_seconds, 60);
        assert_eq!(settings.judge_endpoint_timeout_seconds, 15);
        assert_eq!(settings.judge_max_concurrency, 4);
        assert_eq!(settings.judge_max_retries, 1);
        assert_eq!(settings.active_reply_supersede_window_seconds, 5);
        assert_eq!(settings.continuation_window_seconds, 15);
        assert!(settings.takeover_direct_trigger_enable);
        assert_eq!(settings.takeover_direct_trigger_boost_score, 0.3);
        assert!(settings.privileged_direct_trigger_skip_active_judgement);
        assert_eq!(settings.active_reply_reaction_emoji_ids, [289]);
        assert_eq!(settings.active_reply_reaction_timeout_seconds, 600);
        assert!(settings.reply_target_quote_enable);
        assert_eq!(settings.reply_target_quote_after_other_messages, 4);
        assert!(settings.reply_target_mention_enable);
        assert_eq!(settings.reply_target_mention_after_seconds, 15);
        assert_eq!(settings.moderation_min_severity, 7.0);
        assert_eq!(settings.base64_moderation_min_chars, 24);
        assert_eq!(settings.base64_moderation_max_decoded_chars, 5_000);
        assert_eq!(settings.base64_moderation_min_printable_ratio, 0.85);
        assert_eq!(settings.moderation_keywords.len(), 175);
        assert!(settings.identity_mappings.is_empty());
        assert!(!settings.affection_enable);
        assert!(settings.validate().is_ok());
    }

    #[test]
    fn qq_default_non_whitelist_rate_limits_match_the_deployed_contract() {
        let qq = OneBotConfig::default();

        assert_eq!(
            qq.private_chats.non_whitelist_rate_limit,
            PlatformRateLimit {
                max_messages: 2,
                window_seconds: 600,
            }
        );
        assert_eq!(
            qq.group_chats.non_whitelist_rate_limit,
            PlatformRateLimit {
                max_messages: 2,
                window_seconds: 600,
            }
        );

        let explicit: OneBotConfig = serde_json::from_value(serde_json::json!({
            "private_chats": {
                "non_whitelist_rate_limit": {
                    "max_messages": 1,
                    "window_seconds": 120
                }
            },
            "group_chats": {
                "non_whitelist_rate_limit": {
                    "max_messages": 5,
                    "window_seconds": 60
                }
            }
        }))
        .unwrap();
        assert_eq!(
            explicit.private_chats.non_whitelist_rate_limit.max_messages,
            1
        );
        assert_eq!(
            explicit
                .private_chats
                .non_whitelist_rate_limit
                .window_seconds,
            120
        );
        assert_eq!(
            explicit.group_chats.non_whitelist_rate_limit.max_messages,
            5
        );
        assert_eq!(
            explicit.group_chats.non_whitelist_rate_limit.window_seconds,
            60
        );
    }

    #[test]
    fn real_context_migrates_group_member_page_size_to_search_max_results() {
        let mut instance = PlatformPluginInstanceConfig {
            enabled: None,
            settings: serde_json::json!({ "group_member_page_size": 17 })
                .as_object()
                .unwrap()
                .clone(),
        };

        let settings = RealContextPluginSettings::from_instance(&instance).unwrap();
        assert_eq!(settings.group_member_search_max_results, 17);

        merge_real_context_settings(&mut instance, &settings);
        assert_eq!(instance.settings["group_member_search_max_results"], 17);
        assert!(!instance.settings.contains_key("group_member_page_size"));
    }

    #[test]
    fn real_context_migrates_continuation_minutes_to_seconds() {
        let mut former_default = PlatformPluginInstanceConfig {
            enabled: None,
            settings: serde_json::json!({ "continuation_window_minutes": 3 })
                .as_object()
                .unwrap()
                .clone(),
        };
        let settings = RealContextPluginSettings::from_instance(&former_default).unwrap();
        // The old default must land on the current one, whatever that is.
        assert_eq!(
            settings.continuation_window_seconds,
            RealContextPluginSettings::default().continuation_window_seconds
        );
        merge_real_context_settings(&mut former_default, &settings);
        assert!(!former_default
            .settings
            .contains_key("continuation_window_minutes"));
        assert!(!former_default
            .settings
            .contains_key("continuation_window_seconds"));

        let mut custom = PlatformPluginInstanceConfig {
            enabled: None,
            settings: serde_json::json!({ "continuation_window_minutes": 7 })
                .as_object()
                .unwrap()
                .clone(),
        };
        let settings = RealContextPluginSettings::from_instance(&custom).unwrap();
        assert_eq!(settings.continuation_window_seconds, 420);
        merge_real_context_settings(&mut custom, &settings);
        assert_eq!(custom.settings["continuation_window_seconds"], 420);
        assert!(!custom.settings.contains_key("continuation_window_minutes"));
    }

    #[test]
    fn an_embedding_model_never_reaches_the_chat_pickers() {
        // It produces vectors, not replies; the multimodal list derives from
        // the text one, so filtering at the source covers both.
        let mut config = AppConfig::default();
        let provider = config.providers.first_mut().unwrap();
        provider.models = vec!["chat-model".to_string(), "vector-model".to_string()];
        provider
            .model_modalities
            .insert("chat-model".to_string(), vec!["text".to_string()]);
        provider.model_modalities.insert(
            "vector-model".to_string(),
            vec![EMBEDDING_MODALITY.to_string()],
        );

        let text: Vec<String> = config
            .text_provider_model_choices()
            .into_iter()
            .map(|choice| choice.model)
            .collect();
        assert!(text.contains(&"chat-model".to_string()), "{text:?}");
        assert!(!text.contains(&"vector-model".to_string()), "{text:?}");
    }

    #[test]
    fn the_embedding_model_moves_out_from_under_the_knowledge_base() {
        // It was configured there because that is where it was first needed;
        // it now also backs memory recall, and a knowledge-base setting quietly
        // steering group-chat search is a trap for whoever reads this next.
        let mut config = AppConfig::default();
        config.plugins.knowledge_base.embedding_provider_id = "omlx".to_string();
        config.plugins.knowledge_base.embedding_model = "bge-m3".to_string();
        config.plugins.knowledge_base.embedding_timeout_seconds = 45;
        config.plugins.knowledge_base.semantic_min_score = 0.5;
        config.config_version = 0;
        config.migrate().unwrap();
        assert_eq!(config.embedding.provider_id, "omlx");
        assert_eq!(config.embedding.model, "bge-m3");
        assert_eq!(config.embedding.timeout_seconds, 45);
        assert!((config.embedding.min_score - 0.5).abs() < f32::EPSILON);

        // Configuring a model only makes it available; there is no switch.
        assert!(config.embedding.is_configured());
        assert!(!AppConfig::default().embedding.is_configured());
    }

    #[test]
    fn a_legacy_shared_window_seeds_both_new_windows() {
        // One knob used to drive both the reply turn and the judge. Their best
        // values point opposite ways — the reply wants a generous opening
        // snapshot, the judge a tight recent window — so the knob split, and an
        // existing config has to land on its old value for both rather than
        // silently jumping to the new defaults.
        let mut settings = serde_json::Map::new();
        settings.insert("context_messages".to_string(), serde_json::json!(12));
        migrate_real_context_settings_map(&mut settings);
        assert_eq!(settings["reply_context_window"], 12);
        assert_eq!(settings["judge_context_window"], 12);

        // An explicit new value wins over the legacy one.
        let mut settings = serde_json::Map::new();
        settings.insert("context_messages".to_string(), serde_json::json!(12));
        settings.insert("judge_context_window".to_string(), serde_json::json!(30));
        migrate_real_context_settings_map(&mut settings);
        assert_eq!(settings["reply_context_window"], 12);
        assert_eq!(settings["judge_context_window"], 30);
    }

    #[test]
    fn real_context_legacy_settings_migrate_and_deprecated_keys_are_removed() {
        let mut instance = PlatformPluginInstanceConfig {
            enabled: None,
            settings: serde_json::json!({
                "reply_context_messages": 37,
                "active_context_messages": 5,
                "takeover_system_trigger_enable": true,
                "takeover_system_trigger_boost_score": 0.4,
                "judge_models": [{"provider_id": "judge", "model": "primary"}],
                "affection_judge_models": [{"provider_id": "affection", "model": "secondary"}],
                "activity_statistics_enable": false,
                "future_option": {"value": 1}
            })
            .as_object()
            .unwrap()
            .clone(),
        };

        let settings = RealContextPluginSettings::from_instance(&instance).unwrap();
        assert_eq!(settings.reply_context_window, 37);
        assert_eq!(settings.judge_context_window, 37);
        assert!(settings.takeover_direct_trigger_enable);
        assert_eq!(settings.takeover_direct_trigger_boost_score, 0.4);
        assert_eq!(
            settings.text_models.as_ref().unwrap()[0].provider_id,
            "judge"
        );

        merge_real_context_settings(&mut instance, &settings);
        assert_eq!(instance.settings["reply_context_window"], 37);
        // Migrated to `true`, which now equals the default and is pruned from
        // the persisted map; the effective value is asserted above.
        assert!(!instance
            .settings
            .contains_key("takeover_direct_trigger_enable"));
        assert_eq!(instance.settings["text_models"][0]["provider_id"], "judge");
        assert_eq!(instance.settings["future_option"]["value"], 1);
        for key in DEPRECATED_REAL_CONTEXT_SETTINGS {
            assert!(!instance.settings.contains_key(*key));
        }
    }

    #[test]
    fn real_context_judge_persona_prompt_normalizes_validates_and_roundtrips() {
        let legacy =
            RealContextPluginSettings::from_instance(&PlatformPluginInstanceConfig::default())
                .unwrap();
        assert!(legacy.judge_persona_prompt.is_empty());

        let mut settings = RealContextPluginSettings {
            judge_persona_prompt: "  custom persona\n".to_string(),
            ..RealContextPluginSettings::default()
        };
        settings.normalize();
        assert_eq!(settings.judge_persona_prompt, "custom persona");
        assert!(settings.validate().is_ok());

        let mut instance = PlatformPluginInstanceConfig::default();
        instance
            .settings
            .insert("future_option".to_string(), serde_json::json!(true));
        merge_real_context_settings(&mut instance, &settings);
        assert_eq!(instance.settings["judge_persona_prompt"], "custom persona");
        assert_eq!(instance.settings["future_option"], true);
        let reparsed = RealContextPluginSettings::from_instance(&instance).unwrap();
        assert_eq!(reparsed.judge_persona_prompt, "custom persona");

        let mut cleared = reparsed;
        cleared.judge_persona_prompt = " \n ".to_string();
        cleared.normalize();
        merge_real_context_settings(&mut instance, &cleared);
        assert!(!instance.settings.contains_key("judge_persona_prompt"));
        assert_eq!(instance.settings["future_option"], true);

        assert!(RealContextPluginSettings {
            judge_persona_prompt: "bad\0prompt".to_string(),
            ..RealContextPluginSettings::default()
        }
        .validate()
        .is_err());
        assert!(RealContextPluginSettings {
            judge_persona_prompt: "x".repeat(32_769),
            ..RealContextPluginSettings::default()
        }
        .validate()
        .is_err());
    }

    #[test]
    fn real_context_plugin_rejects_invalid_types_ranges_and_models() {
        let mut config = route_test_config();
        let mut instance = PlatformPluginInstanceConfig::default();
        instance.settings.insert(
            "active_judge_probability".to_string(),
            serde_json::json!(1.1),
        );
        config
            .platforms
            .qq
            .plugins
            .insert(REAL_CONTEXT_PLUGIN_ID.to_string(), instance);
        assert!(config.validate().is_err());

        config.platforms.qq.plugins.insert(
            REAL_CONTEXT_PLUGIN_ID.to_string(),
            PlatformPluginInstanceConfig {
                enabled: None,
                settings: serde_json::json!({"active_reply_enable": "yes"})
                    .as_object()
                    .unwrap()
                    .clone(),
            },
        );
        assert!(config.validate().is_err());

        let mut settings = RealContextPluginSettings {
            text_models: Some(vec![ActiveProviderModelConfig {
                provider_id: config.providers[0].id.clone(),
                model: "missing".to_string(),
            }]),
            ..RealContextPluginSettings::default()
        };
        let mut instance = PlatformPluginInstanceConfig::default();
        merge_real_context_settings(&mut instance, &settings);
        config
            .platforms
            .qq
            .plugins
            .insert(REAL_CONTEXT_PLUGIN_ID.to_string(), instance);
        assert!(config.validate().is_err());

        settings.text_models.as_mut().unwrap()[0].model = "text-only".to_string();
        merge_real_context_settings(
            config
                .platforms
                .qq
                .plugins
                .get_mut(REAL_CONTEXT_PLUGIN_ID)
                .unwrap(),
            &settings,
        );
        assert!(config.validate().is_ok());
    }
