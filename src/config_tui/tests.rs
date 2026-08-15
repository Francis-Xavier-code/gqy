//! tests — 自 src/config_tui.rs 外移。
#![cfg(test)]

use super::*;

#[cfg(test)]
mod tests {
    use super::{
        apply_real_context_values, apply_reply_processor_values, choice_display_label,
        field_display_value, language_choice_label, language_choice_value, menu_window,
        parse_extra_body, parse_id_lines, parse_id_list, parse_keyword_lines,
        parse_real_context_identity_lines, parse_real_context_string_lines,
        platform_conversation_id_label, platform_conversation_kind_label, platform_persona_summary,
        real_context_values, reply_processor_mode_label, reply_processor_mode_value,
        reply_processor_values, route_pool_summary, t, thinking_variant_field,
        validate_reply_processor_settings, vision_provider_model_choice_values, Field,
        PersonaMenuTarget, ReplyProcessorSettingsForm, REPLY_PROCESSOR_PLUGIN_ID,
    };
    use crate::config::{
        AppConfig, PlatformConversationKind, PlatformModelPoolInheritance, PlatformPersonaOverride,
        PlatformPluginInstanceConfig, RealContextPluginSettings, REAL_CONTEXT_PLUGIN_ID,
    };
    use crate::llm::ThinkingVariantOptions;

    #[test]
    fn sensitive_field_is_masked_until_actively_edited() {
        let field = Field::new("API Key", "secret-key".to_string()).sensitive();

        assert_eq!(field_display_value(&field, false), "********");
        assert_eq!(field_display_value(&field, true), "secret-key");
    }

    #[test]
    fn empty_sensitive_field_remains_empty() {
        let field = Field::new("API Key", String::new()).sensitive();

        assert_eq!(field_display_value(&field, false), "");
    }

    #[test]
    fn thinking_variant_field_uses_raw_model_options_and_default_choice() {
        let field = thinking_variant_field(
            &ThinkingVariantOptions {
                provider_id: "provider".to_string(),
                model: "model".to_string(),
                variants: vec!["default".to_string(), "high".to_string()],
                selected: Some("default".to_string()),
            },
            Some("default"),
        );

        assert_eq!(field.label, t("Thinking variant", "思考程度"));
        assert_eq!(field.value, "default");
        assert_eq!(field.choices, vec!["", "default", "high"]);
        assert!(field.raw_choice_labels);
        assert_eq!(choice_display_label("high", "", true), "high");
        assert_eq!(field.empty_choice_label, "default");

        let unsupported = thinking_variant_field(
            &ThinkingVariantOptions {
                provider_id: "provider".to_string(),
                model: "plain-model".to_string(),
                variants: Vec::new(),
                selected: None,
            },
            None,
        );
        assert_eq!(unsupported.choices, vec![""]);
        assert_eq!(field_display_value(&unsupported, false), "default");

        let stale = thinking_variant_field(
            &ThinkingVariantOptions {
                provider_id: "provider".to_string(),
                model: "changed-model".to_string(),
                variants: Vec::new(),
                selected: None,
            },
            Some("high"),
        );
        assert_eq!(stale.value, "high");
        assert_eq!(stale.choices, vec!["", "high"]);
        assert_eq!(field_display_value(&stale, false), "high");
    }

    #[test]
    fn sensitive_textarea_displays_configured_item_count() {
        let field = Field::textarea("API Keys", "first\n\nsecond, third".to_string()).sensitive();

        assert_eq!(
            field_display_value(&field, false),
            t("[3 configured]", "[已配置 3 项]")
        );
    }

    #[test]
    fn empty_sensitive_textarea_renders_empty() {
        let field = Field::textarea("API Keys", String::new()).sensitive();

        assert_eq!(field_display_value(&field, false), "");
    }

    #[test]
    fn language_choices_have_locale_specific_labels() {
        assert_eq!(language_choice_label("auto", false), Some("Auto"));
        assert_eq!(language_choice_label("en", false), Some("English"));
        assert_eq!(
            language_choice_label("zh", false),
            Some("Simplified Chinese")
        );
        assert_eq!(language_choice_label("auto", true), Some("自动"));
        assert_eq!(language_choice_label("en", true), Some("英语"));
        assert_eq!(language_choice_label("zh", true), Some("简体中文"));
    }

    #[test]
    fn language_choice_labels_map_to_stable_values() {
        for value in ["auto", "Auto", "自动"] {
            assert_eq!(language_choice_value(value), Some("auto"));
        }
        for value in ["en", "English", "英语"] {
            assert_eq!(language_choice_value(value), Some("en"));
        }
        for value in ["zh", "Simplified Chinese", "简体中文"] {
            assert_eq!(language_choice_value(value), Some("zh"));
        }
        assert_eq!(language_choice_value("unsupported"), None);
    }

    #[test]
    fn menu_window_keeps_selection_visible_for_long_lists() {
        assert_eq!(menu_window(100, 0, 5), 0..5);
        assert_eq!(menu_window(100, 50, 5), 48..53);
        assert_eq!(menu_window(100, 99, 5), 95..100);
        assert_eq!(menu_window(3, 2, 10), 0..3);
        assert_eq!(menu_window(0, 0, 5), 0..0);
    }

    #[test]
    fn extra_body_parser_accepts_only_json_objects() {
        for input in ["true", "\"hello\"", "[1, 2, 3]", "{invalid"] {
            assert!(parse_extra_body(input).is_err());
        }

        let parsed = parse_extra_body(r#"{"enable_thinking":false}"#)
            .unwrap()
            .unwrap();
        assert_eq!(parsed["enable_thinking"], false);
        assert!(parse_extra_body("  ").unwrap().is_none());
    }

    #[test]
    fn reply_processor_defaults_match_platform_contract() {
        let config = AppConfig::default();
        let (enabled, settings) = reply_processor_values(&config).unwrap();

        assert!(enabled);
        assert!(settings.default_enabled);
        assert_eq!(settings.threshold, 200);
        assert_eq!(settings.mode, "image");
        assert!(settings.followup_mention);
        assert!(settings.strip_period);
        assert_eq!(settings.theme, "paper");
        assert_eq!(settings.max_height, 2600);
        assert_eq!(settings.font_size, 36);
        assert_eq!(settings.code_font_size, 30);
        assert_eq!(settings.padding, 64);
        assert!(settings.context_notice);
        assert_eq!(settings.ttl_hours, 24);
        assert_eq!(settings.max_records, 3);
        assert!(settings.send_tool_intercept);
        assert!(settings.font.is_empty());
        assert!(settings.title_font.is_empty());
        assert!(settings.code_font.is_empty());
        assert!(settings.emoji_font.is_empty());
    }

    #[test]
    fn reply_processor_mode_labels_preserve_config_values() {
        assert_eq!(
            reply_processor_mode_label("image"),
            t("Convert to image", "转图片")
        );
        assert_eq!(
            reply_processor_mode_label("forward"),
            t("Merged forward", "合并转发")
        );
        assert_eq!(reply_processor_mode_value("转图片"), Some("image"));
        assert_eq!(
            reply_processor_mode_value("Merged forward"),
            Some("forward")
        );
        assert_eq!(reply_processor_mode_value("unsupported"), None);
    }

    #[test]
    fn reply_processor_settings_use_generic_map_and_preserve_unknown_keys() {
        let mut config = AppConfig::default();
        let mut instance = PlatformPluginInstanceConfig {
            enabled: Some(false),
            ..PlatformPluginInstanceConfig::default()
        };
        instance
            .settings
            .insert("future_option".to_string(), serde_json::json!({"value": 1}));
        config
            .platforms
            .qq
            .plugins
            .insert(REPLY_PROCESSOR_PLUGIN_ID.to_string(), instance);
        let settings = ReplyProcessorSettingsForm {
            threshold: 512,
            mode: "forward".to_string(),
            ..ReplyProcessorSettingsForm::default()
        };

        apply_reply_processor_values(&mut config, true, &settings).unwrap();

        let instance = &config.platforms.qq.plugins[REPLY_PROCESSOR_PLUGIN_ID];
        assert_eq!(instance.enabled, None);
        assert_eq!(instance.settings["threshold"], 512);
        assert_eq!(instance.settings["mode"], "forward");
        assert_eq!(instance.settings["future_option"]["value"], 1);
        let (enabled, reparsed) = reply_processor_values(&config).unwrap();
        assert!(enabled);
        assert_eq!(reparsed, settings);
    }

    #[test]
    fn reply_processor_range_validation_rejects_unsafe_render_settings() {
        assert!(validate_reply_processor_settings(&ReplyProcessorSettingsForm::default()).is_ok());
        assert!(
            validate_reply_processor_settings(&ReplyProcessorSettingsForm {
                threshold: 0,
                ..ReplyProcessorSettingsForm::default()
            })
            .is_err()
        );
        assert!(
            validate_reply_processor_settings(&ReplyProcessorSettingsForm {
                max_height: 999,
                ..ReplyProcessorSettingsForm::default()
            })
            .is_err()
        );
        assert!(
            validate_reply_processor_settings(&ReplyProcessorSettingsForm {
                ttl_hours: 169,
                ..ReplyProcessorSettingsForm::default()
            })
            .is_err()
        );
    }

    #[test]
    fn real_context_settings_use_generic_map_and_preserve_unknown_keys() {
        let mut config = AppConfig::default();
        let mut instance = PlatformPluginInstanceConfig::default();
        instance
            .settings
            .insert("future_option".to_string(), serde_json::json!({"value": 1}));
        config
            .platforms
            .qq
            .plugins
            .insert(REAL_CONTEXT_PLUGIN_ID.to_string(), instance);
        let settings = RealContextPluginSettings {
            reply_threshold: 0.9,
            reply_context_window: 42,
            judge_persona_prompt: "judge persona".to_string(),
            ..RealContextPluginSettings::default()
        };

        apply_real_context_values(&mut config, false, &settings);

        let instance = &config.platforms.qq.plugins[REAL_CONTEXT_PLUGIN_ID];
        assert_eq!(instance.enabled, Some(false));
        assert_eq!(instance.settings["reply_threshold"], 0.9);
        assert_eq!(instance.settings["reply_context_window"], 42);
        assert_eq!(instance.settings["judge_persona_prompt"], "judge persona");
        assert_eq!(instance.settings["future_option"]["value"], 1);
        let (enabled, reparsed) = real_context_values(&config).unwrap();
        assert!(!enabled);
        assert_eq!(reparsed, settings);
    }

    #[test]
    fn real_context_batch_parsers_are_line_based_and_deduplicated() {
        let mappings =
            parse_real_context_identity_lines("# 昵称<Tab>QQ号\nGQY\t123\n小羽 = 456").unwrap();
        assert_eq!(mappings.len(), 2);
        assert_eq!(mappings[0].nickname, "GQY");
        assert_eq!(mappings[0].user_id, 123);
        assert!(parse_real_context_identity_lines("GQY\t123\nGQY\t456").is_err());
        assert!(parse_real_context_identity_lines("GQY 123").is_err());

        assert_eq!(
            parse_real_context_string_lines("晚安\n 晚安 \nGQY", 128).unwrap(),
            vec!["晚安", "GQY"]
        );
    }

    #[test]
    fn route_pool_and_id_helpers_express_inheritance_and_positive_ids() {
        assert_eq!(
            route_pool_summary(None, PlatformModelPoolInheritance::Platform),
            t("inherit platform", "继承 QQ 平台池")
        );
        assert_eq!(
            route_pool_summary(Some(&[]), PlatformModelPoolInheritance::Platform),
            t("inherit platform", "继承 QQ 平台池")
        );
        assert_eq!(
            route_pool_summary(None, PlatformModelPoolInheritance::Global),
            t("inherit global", "继承全局池")
        );
        assert_eq!(parse_id_list("123, 456").unwrap(), vec![123, 456]);
        assert!(parse_id_list("0").is_err());
        assert!(parse_id_list("-1").is_err());
        assert_eq!(parse_id_lines("123\n456\n123\n").unwrap(), vec![123, 456]);
        assert!(parse_id_lines("123\ninvalid\n456").is_err());
        assert_eq!(
            parse_keyword_lines("GQY\n 小羽 \nGQY").unwrap(),
            vec!["GQY", "小羽"]
        );
    }

    #[test]
    fn qq_batch_inputs_are_line_based_trimmed_and_deduplicated() {
        assert_eq!(
            parse_id_lines(" 123 \r\n\r\n456\n123\n").unwrap(),
            vec![123, 456]
        );
        assert!(parse_id_lines("123,456").is_err());
        assert_eq!(
            parse_keyword_lines(" GQY \r\n\r\n小羽\nGQY\n").unwrap(),
            vec!["GQY", "小羽"]
        );
    }

    #[test]
    fn qq_conversation_labels_are_localized_and_id_label_tracks_type() {
        assert_eq!(
            platform_conversation_kind_label(PlatformConversationKind::Private),
            t("Private chat", "私聊")
        );
        assert_eq!(
            platform_conversation_kind_label(PlatformConversationKind::Group),
            t("Group chat", "群聊")
        );
        assert_eq!(
            platform_conversation_id_label(PlatformConversationKind::Private),
            t("QQ id", "QQ 号")
        );
        assert_eq!(
            platform_conversation_id_label(PlatformConversationKind::Group),
            t("Group id", "群号")
        );
    }

    #[test]
    fn qq_conversation_persona_summary_distinguishes_inheritance_and_gqy() {
        assert_eq!(
            platform_persona_summary(&PlatformPersonaOverride::Inherit),
            t("inherit current persona", "继承当前人格")
        );
        assert_eq!(
            platform_persona_summary(&PlatformPersonaOverride::GQY),
            "GQY"
        );
        assert_eq!(
            platform_persona_summary(&PlatformPersonaOverride::Custom {
                name: "Group.md".to_string()
            }),
            "Group"
        );
    }

    #[test]
    fn qq_persona_menu_target_isolated_from_global_persona_and_tracks_renames() {
        let mut config = AppConfig::default();
        config.prompt.active_persona = "Global.md".to_string();
        let mut target = PersonaMenuTarget::Platform(PlatformPersonaOverride::Inherit);

        assert_eq!(target.custom_offset(), 2);
        target.activate_custom(&mut config, "Session.md".to_string());
        assert_eq!(config.prompt.active_persona, "Global.md");
        assert_eq!(target.custom_name(&config), Some("Session.md"));
        assert_eq!(target.pending_reference_count("Session.md"), 1);

        target.rename_custom("Session.md", "Renamed.md");
        assert_eq!(target.custom_name(&config), Some("Renamed.md"));
        assert_eq!(target.pending_reference_count("Session.md"), 0);
        assert_eq!(target.pending_reference_count("Renamed.md"), 1);

        target.activate_gqy(&mut config);
        assert!(target.is_gqy(&config));
        assert_eq!(config.prompt.active_persona, "Global.md");
        target.activate_inherit();
        assert!(matches!(
            target,
            PersonaMenuTarget::Platform(PlatformPersonaOverride::Inherit)
        ));
    }

    #[test]
    fn global_persona_menu_target_preserves_activation_behavior() {
        let mut config = AppConfig::default();
        let mut target = PersonaMenuTarget::Global;

        assert_eq!(target.custom_offset(), 1);
        assert!(target.is_gqy(&config));
        target.activate_custom(&mut config, "Global.md".to_string());
        assert_eq!(target.custom_name(&config), Some("Global.md"));
        assert_eq!(target.pending_reference_count("Global.md"), 0);

        target.activate_gqy(&mut config);
        assert!(config.prompt.active_persona.is_empty());
        assert!(target.is_gqy(&config));
    }

    #[test]
    fn explicit_vision_choices_only_include_image_capable_models() {
        let mut config = AppConfig::default();
        let provider = &mut config.providers[0];
        provider.models = vec!["text-only".to_string(), "vision".to_string()];
        provider
            .model_modalities
            .insert("text-only".to_string(), vec!["text".to_string()]);
        provider.model_modalities.insert(
            "vision".to_string(),
            vec!["text".to_string(), "image".to_string()],
        );
        let provider_id = provider.id.clone();

        let choices = vision_provider_model_choice_values(&config);

        assert!(choices.contains(&format!("{provider_id}\tvision")));
        assert!(!choices.contains(&format!("{provider_id}\ttext-only")));
    }
}
