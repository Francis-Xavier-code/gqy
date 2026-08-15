//! editors — 自 src/config_tui.rs 拆分。

pub(crate) use super::*;

pub(crate) fn platform_conversation_kind_label(kind: PlatformConversationKind) -> &'static str {
    match kind {
        PlatformConversationKind::Private => t("Private chat", "私聊"),
        PlatformConversationKind::Group => t("Group chat", "群聊"),
    }
}

pub(crate) fn platform_conversation_id_label(kind: PlatformConversationKind) -> &'static str {
    match kind {
        PlatformConversationKind::Private => t("QQ id", "QQ 号"),
        PlatformConversationKind::Group => t("Group id", "群号"),
    }
}

pub(crate) fn platform_persona_summary(persona: &PlatformPersonaOverride) -> String {
    match persona {
        PlatformPersonaOverride::Inherit => {
            t("inherit current persona", "继承当前人格").to_string()
        }
        PlatformPersonaOverride::GQY => "GQY".to_string(),
        PlatformPersonaOverride::Custom { name } => persona_display_name(name).to_string(),
    }
}

pub(crate) fn edit_platform_personas(
    stdout: &mut io::Stdout,
    paths: &GQYPaths,
    config: &mut AppConfig,
    persona: &mut PlatformPersonaOverride,
) -> Result<()> {
    if let Some(updated) = manage_personas(
        stdout,
        paths,
        config,
        PersonaMenuTarget::Platform(persona.clone()),
    )? {
        *persona = updated;
    }
    Ok(())
}

pub(crate) fn select_platform_conversation_kind(
    stdout: &mut io::Stdout,
    kind: &mut PlatformConversationKind,
) -> Result<()> {
    let choices = [
        platform_conversation_kind_label(PlatformConversationKind::Private).to_string(),
        platform_conversation_kind_label(PlatformConversationKind::Group).to_string(),
    ];
    let current = platform_conversation_kind_label(*kind);
    let selected = select_choice(
        stdout,
        t("Conversation type", "会话类型"),
        current,
        &choices,
        "",
        false,
    )?;
    *kind = if selected == choices[1] {
        PlatformConversationKind::Group
    } else {
        PlatformConversationKind::Private
    };
    Ok(())
}

pub(crate) fn edit_conversation_extra_prompt(
    stdout: &mut io::Stdout,
    prompt: &mut String,
) -> Result<()> {
    edit_textarea(stdout, prompt)?;
    Ok(())
}

pub(crate) fn route_pool_summary(
    pool: Option<&[ActiveProviderModelConfig]>,
    inheritance: PlatformModelPoolInheritance,
) -> String {
    match pool {
        None | Some([]) if inheritance == PlatformModelPoolInheritance::Global => {
            t("inherit global", "继承全局池").to_string()
        }
        None | Some([]) => t("inherit platform", "继承 QQ 平台池").to_string(),
        Some(entries) if entries.len() == 1 => {
            format!("{} / {}", entries[0].provider_id, entries[0].model)
        }
        Some(entries) => format!("{} {}", entries.len(), t("models", "个模型")),
    }
}

pub(crate) fn qq_pool_summary(pool: Option<&[ActiveProviderModelConfig]>) -> String {
    match pool {
        None | Some([]) => t("inherit global", "继承全局").to_string(),
        Some(entries) => route_pool_summary(Some(entries), PlatformModelPoolInheritance::Platform),
    }
}

pub(crate) fn select_platform_route_models(
    stdout: &mut io::Stdout,
    config: &AppConfig,
    pool: &mut Option<Vec<ActiveProviderModelConfig>>,
    inheritance: &mut PlatformModelPoolInheritance,
    multimodal: bool,
) -> Result<()> {
    let choices = if multimodal {
        config.multimodal_provider_model_choices()
    } else {
        config.text_provider_model_choices()
    };
    let mut selected = 0usize;
    loop {
        let mut options = Vec::with_capacity(choices.len() + 2);
        let inherit_platform_marker = if pool.as_ref().is_none_or(Vec::is_empty)
            && *inheritance == PlatformModelPoolInheritance::Platform
        {
            "[*] "
        } else {
            "[ ] "
        };
        options.push(format!(
            "{inherit_platform_marker}{}",
            t("Inherit QQ platform model pool", "继承 QQ 平台模型池")
        ));
        let inherit_global_marker = if pool.as_ref().is_none_or(Vec::is_empty)
            && *inheritance == PlatformModelPoolInheritance::Global
        {
            "[*] "
        } else {
            "[ ] "
        };
        options.push(format!(
            "{inherit_global_marker}{}",
            if multimodal {
                t(
                    "Inherit global multimodal model pool",
                    "继承全局多模态模型池",
                )
            } else {
                t("Inherit global model pool", "继承全局模型池")
            }
        ));
        options.extend(choices.iter().map(|choice| {
            let active = pool.as_ref().is_some_and(|entries| {
                entries.iter().any(|entry| {
                    entry.provider_id == choice.provider_id && entry.model == choice.model
                })
            });
            let marker = if active { "[*] " } else { "[ ] " };
            format!("{marker}{}", choice.label())
        }));
        draw_menu(
            stdout,
            if multimodal {
                t(" SESSION MULTIMODAL MODELS ", " 会话多模态模型 ")
            } else {
                t(" SESSION TEXT MODELS ", " 会话文本模型 ")
            },
            &options,
            selected,
            t(
                "[Tab]add/remove [Enter/q]confirm",
                "[Tab]加入/移出 [Enter/q]确认",
            ),
        )?;
        match read_key()? {
            KeyCode::Char('q') | KeyCode::Esc | KeyCode::Enter => return Ok(()),
            KeyCode::Up | KeyCode::Char('k') => selected = selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => selected = (selected + 1).min(options.len() - 1),
            KeyCode::Tab if selected == 0 => {
                *pool = None;
                *inheritance = PlatformModelPoolInheritance::Platform;
            }
            KeyCode::Tab if selected == 1 => {
                *pool = None;
                *inheritance = PlatformModelPoolInheritance::Global;
            }
            KeyCode::Tab => {
                *inheritance = PlatformModelPoolInheritance::Platform;
                let choice = &choices[selected - 2];
                let entries = pool.get_or_insert_with(Vec::new);
                if let Some(index) = entries.iter().position(|entry| {
                    entry.provider_id == choice.provider_id && entry.model == choice.model
                }) {
                    entries.remove(index);
                } else {
                    entries.push(ActiveProviderModelConfig {
                        provider_id: choice.provider_id.clone(),
                        model: choice.model.clone(),
                    });
                }
                if entries.is_empty() {
                    *pool = None;
                }
            }
            _ => {}
        }
    }
}

pub(crate) fn select_qq_model_pool(
    stdout: &mut io::Stdout,
    config: &mut AppConfig,
    multimodal: bool,
) -> Result<()> {
    let choices = if multimodal {
        config.multimodal_provider_model_choices()
    } else {
        config.text_provider_model_choices()
    };
    let title = if multimodal {
        t(" QQ MULTIMODAL MODELS ", " QQ 多模态模型 ")
    } else {
        t(" QQ TEXT MODELS ", " QQ 文本模型 ")
    };
    let inherit = if multimodal {
        t(
            "Inherit global multimodal model pool",
            "继承全局多模态模型池",
        )
    } else {
        t("Inherit global model pool", "继承全局模型池")
    };
    select_model_pool(
        stdout,
        choices,
        if multimodal {
            &mut config.platforms.qq.multimodal_models
        } else {
            &mut config.platforms.qq.text_models
        },
        multimodal,
        title,
        inherit,
    )
}

pub(crate) fn select_non_whitelist_model_pool(
    stdout: &mut io::Stdout,
    config: &mut AppConfig,
) -> Result<()> {
    let choices = config.text_provider_model_choices();
    select_model_pool(
        stdout,
        choices,
        &mut config.platforms.qq.non_whitelist_text_models,
        false,
        t(" NON-WHITELIST TEXT MODELS ", " 非白名单模型池 "),
        t("Inherit QQ platform model pool", "继承 QQ 平台模型池"),
    )
}

pub(crate) fn select_model_pool(
    stdout: &mut io::Stdout,
    choices: Vec<ProviderModelChoice>,
    pool: &mut Option<Vec<ActiveProviderModelConfig>>,
    _multimodal: bool,
    title: &str,
    inherit_label: &str,
) -> Result<()> {
    let mut selected = 0usize;
    loop {
        let mut options = Vec::with_capacity(choices.len() + 1);
        let inherit_marker = if pool.as_ref().is_none_or(Vec::is_empty) {
            "[*] "
        } else {
            "[ ] "
        };
        options.push(format!("{inherit_marker}{inherit_label}"));
        options.extend(choices.iter().map(|choice| {
            let active = pool.as_ref().is_some_and(|entries| {
                entries.iter().any(|entry| {
                    entry.provider_id == choice.provider_id && entry.model == choice.model
                })
            });
            format!("{}{}", if active { "[*] " } else { "[ ] " }, choice.label())
        }));
        draw_menu(
            stdout,
            title,
            &options,
            selected,
            t(
                "[Tab]add/remove [Enter/q]confirm",
                "[Tab]加入/移出 [Enter/q]确认",
            ),
        )?;
        match read_key()? {
            KeyCode::Char('q') | KeyCode::Esc | KeyCode::Enter => return Ok(()),
            KeyCode::Up | KeyCode::Char('k') => selected = selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => selected = (selected + 1).min(options.len() - 1),
            KeyCode::Tab if selected == 0 => *pool = None,
            KeyCode::Tab => {
                let choice = &choices[selected - 1];
                let entries = pool.get_or_insert_with(Vec::new);
                if let Some(index) = entries.iter().position(|entry| {
                    entry.provider_id == choice.provider_id && entry.model == choice.model
                }) {
                    entries.remove(index);
                } else {
                    entries.push(ActiveProviderModelConfig {
                        provider_id: choice.provider_id.clone(),
                        model: choice.model.clone(),
                    });
                }
                if entries.is_empty() {
                    *pool = None;
                }
            }
            _ => {}
        }
    }
}

pub(crate) const REPLY_PROCESSOR_PLUGIN_ID: &str = "reply_processor";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct ReplyProcessorSettingsForm {
    pub(crate) default_enabled: bool,
    pub(crate) threshold: usize,
    pub(crate) mode: String,
    pub(crate) followup_mention: bool,
    pub(crate) strip_period: bool,
    pub(crate) theme: String,
    pub(crate) max_height: u32,
    pub(crate) font_size: u32,
    pub(crate) code_font_size: u32,
    pub(crate) padding: u32,
    pub(crate) context_notice: bool,
    pub(crate) ttl_hours: u64,
    pub(crate) max_records: usize,
    pub(crate) send_tool_intercept: bool,
    pub(crate) font: String,
    pub(crate) title_font: String,
    pub(crate) code_font: String,
    pub(crate) emoji_font: String,
}

impl Default for ReplyProcessorSettingsForm {
    fn default() -> Self {
        Self {
            default_enabled: true,
            threshold: 200,
            mode: "image".to_string(),
            followup_mention: true,
            strip_period: true,
            theme: "paper".to_string(),
            max_height: 2600,
            font_size: 36,
            code_font_size: 30,
            padding: 64,
            context_notice: true,
            ttl_hours: 24,
            max_records: 3,
            send_tool_intercept: true,
            font: String::new(),
            title_font: String::new(),
            code_font: String::new(),
            emoji_font: String::new(),
        }
    }
}

pub(crate) fn select_platform_plugins(
    stdout: &mut io::Stdout,
    paths: &GQYPaths,
    config: &mut AppConfig,
) -> Result<()> {
    let mut selected = 0usize;
    loop {
        let reply_enabled = config
            .platforms
            .qq
            .plugins
            .get(REPLY_PROCESSOR_PLUGIN_ID)
            .map(|plugin| plugin.enabled_or(true))
            .unwrap_or(true);
        let reply_state = if reply_enabled {
            t("enabled", "已启用")
        } else {
            t("disabled", "未启用")
        };
        let real_context_enabled = config
            .platforms
            .qq
            .plugins
            .get(REAL_CONTEXT_PLUGIN_ID)
            .map(|plugin| plugin.enabled_or(true))
            .unwrap_or(true);
        let real_context_state = if real_context_enabled {
            t("enabled", "已启用")
        } else {
            t("disabled", "未启用")
        };
        let message_history_enabled = config
            .platforms
            .qq
            .plugins
            .get(QQ_MESSAGE_HISTORY_PLUGIN_ID)
            .map(|plugin| plugin.enabled_or(true))
            .unwrap_or(true);
        let message_history_state = if message_history_enabled {
            t("enabled", "已启用")
        } else {
            t("disabled", "未启用")
        };
        let meme_collector_enabled = config
            .platforms
            .qq
            .plugins
            .get(QQ_MEME_COLLECTOR_PLUGIN_ID)
            .map(|plugin| plugin.enabled_or(true))
            .unwrap_or(true);
        let meme_collector_state = if meme_collector_enabled {
            t("enabled", "已启用")
        } else {
            t("disabled", "未启用")
        };
        let options = [
            format!("{}: {reply_state}", t("Reply processor", "回复处理")),
            format!(
                "{}: {real_context_state}",
                t("Group real-context replies", "群聊真实上下文回复")
            ),
            format!(
                "{}: {message_history_state}",
                t("QQ text message history", "QQ 纯文字消息历史")
            ),
            format!(
                "{}: {meme_collector_state}",
                t("QQ meme pocket", "QQ 表情口袋")
            ),
        ];
        draw_menu(
            stdout,
            t(" TENCENT QQ PLUGINS ", " QQ 插件配置 "),
            &options,
            selected,
            t(
                "[Enter]configure [j/k]move [q]back",
                "[Enter]配置 [j/k]移动 [q]返回",
            ),
        )?;
        match read_key()? {
            KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
            KeyCode::Up | KeyCode::Char('k') => selected = selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => selected = (selected + 1).min(options.len() - 1),
            KeyCode::Enter => match selected {
                0 => edit_reply_processor(stdout, config)?,
                1 => edit_real_context(stdout, paths, config)?,
                2 => edit_message_history(stdout, config)?,
                3 => edit_meme_collector(stdout, config)?,
                _ => {}
            },
            _ => {}
        }
    }
}

pub(crate) fn edit_message_history(stdout: &mut io::Stdout, config: &mut AppConfig) -> Result<()> {
    let instance = config
        .platforms
        .qq
        .plugins
        .get(QQ_MESSAGE_HISTORY_PLUGIN_ID);
    let enabled = instance.map(|value| value.enabled_or(true)).unwrap_or(true);
    let settings = instance
        .map(QqMessageHistoryPluginSettings::from_instance)
        .transpose()?
        .unwrap_or_default();
    let mut fields = vec![
        Field::boolean(t("Plugin", "插件状态"), enabled),
        Field::new(
            t(
                "Maximum query results (0 = safety limit)",
                "查询工具单次最多返回（0=安全页上限）",
            ),
            settings.history_search_max_results.to_string(),
        ),
        Field::new(
            t("Query safety page limit", "查询安全页上限"),
            settings.history_safe_page_limit.to_string(),
        ),
        Field::boolean(
            t(
                "Allow administrators to access other conversations",
                "允许管理员访问其他会话",
            ),
            settings.allow_cross_conversation_search,
        ),
    ];
    if !run_form(
        stdout,
        t(" QQ TEXT MESSAGE HISTORY ", " QQ 纯文字消息历史 "),
        &mut fields,
    )? {
        return Ok(());
    }
    let enabled = fields[0].value.parse::<bool>()?;
    let settings = QqMessageHistoryPluginSettings {
        history_search_max_results: fields[1].value.trim().parse()?,
        history_safe_page_limit: fields[2].value.trim().parse()?,
        allow_cross_conversation_search: fields[3].value.parse()?,
    };
    settings.validate()?;
    let mut candidate = config.clone();
    let instance = candidate
        .platforms
        .qq
        .plugins
        .entry(QQ_MESSAGE_HISTORY_PLUGIN_ID.to_string())
        .or_default();
    instance.enabled = (!enabled).then_some(false);
    instance.settings.insert(
        "history_search_max_results".to_string(),
        serde_json::json!(settings.history_search_max_results),
    );
    instance.settings.insert(
        "history_safe_page_limit".to_string(),
        serde_json::json!(settings.history_safe_page_limit),
    );
    instance.settings.insert(
        "allow_cross_conversation_search".to_string(),
        serde_json::json!(settings.allow_cross_conversation_search),
    );
    candidate.normalize_platform_model_routes();
    candidate.validate()?;
    *config = candidate;
    Ok(())
}

pub(crate) fn edit_meme_collector(stdout: &mut io::Stdout, config: &mut AppConfig) -> Result<()> {
    let instance = config.platforms.qq.plugins.get(QQ_MEME_COLLECTOR_PLUGIN_ID);
    let enabled = instance.map(|value| value.enabled_or(true)).unwrap_or(true);
    let settings = instance
        .map(QqMemeCollectorPluginSettings::from_instance)
        .transpose()?
        .unwrap_or_default();
    let mut fields = vec![
        Field::boolean(t("Plugin", "插件状态"), enabled),
        Field::new(
            t("Collection probability (0..1)", "收图概率（0..1）"),
            settings.collect_probability.to_string(),
        ),
        Field::new(
            t("Maximum images per message", "每条消息最多图片数"),
            settings.max_images_per_message.to_string(),
        ),
        Field::boolean(
            t(
                "Allow non-admin save meme tool",
                "允许非管理员使用存表情工具",
            ),
            settings.allow_non_admin_save_tool,
        ),
    ];
    if !run_form(stdout, t(" QQ MEME POCKET ", " QQ 表情口袋 "), &mut fields)? {
        return Ok(());
    }
    let enabled = fields[0].value.parse::<bool>()?;
    let collect_probability = fields[1].value.trim().parse::<f64>()?;
    let max_images_per_message = fields[2].value.trim().parse::<usize>()?;
    let allow_non_admin_save_tool = fields[3].value.parse::<bool>()?;
    let mut candidate = config.clone();
    let instance = candidate
        .platforms
        .qq
        .plugins
        .entry(QQ_MEME_COLLECTOR_PLUGIN_ID.to_string())
        .or_default();
    instance.enabled = (!enabled).then_some(false);
    instance.settings.insert(
        "collect_probability".to_string(),
        serde_json::json!(collect_probability),
    );
    instance.settings.insert(
        "max_images_per_message".to_string(),
        serde_json::json!(max_images_per_message),
    );
    instance.settings.insert(
        "allow_non_admin_save_tool".to_string(),
        serde_json::json!(allow_non_admin_save_tool),
    );
    if let Err(error) = candidate.validate() {
        message(stdout, &error.to_string())?;
        return Ok(());
    }
    *config = candidate;
    Ok(())
}

pub(crate) fn real_context_values(config: &AppConfig) -> Result<(bool, RealContextPluginSettings)> {
    let Some(instance) = config.platforms.qq.plugins.get(REAL_CONTEXT_PLUGIN_ID) else {
        return Ok((true, RealContextPluginSettings::default()));
    };
    Ok((
        instance.enabled_or(true),
        RealContextPluginSettings::from_instance(instance)?,
    ))
}

pub(crate) fn apply_real_context_values(
    config: &mut AppConfig,
    enabled: bool,
    settings: &RealContextPluginSettings,
) {
    let instance = config
        .platforms
        .qq
        .plugins
        .entry(REAL_CONTEXT_PLUGIN_ID.to_string())
        .or_default();
    instance.enabled = (!enabled).then_some(false);
    merge_real_context_settings(instance, settings);
}

pub(crate) fn edit_real_context(
    stdout: &mut io::Stdout,
    paths: &GQYPaths,
    config: &mut AppConfig,
) -> Result<()> {
    let (mut enabled, mut settings) = real_context_values(config)?;
    let mut selected = 0usize;
    loop {
        let state = if enabled {
            t("enabled", "已启用")
        } else {
            t("disabled", "未启用")
        };
        let options = vec![
            format!("{}: {state}", t("Plugin", "插件状态")),
            format!(
                "{}: {}",
                t("Text model pool", "文本模型池"),
                real_context_model_pool_summary(settings.text_models.as_deref())
            ),
            format!(
                "{}: {}",
                t("Reply context window", "回复上下文消息数"),
                settings.reply_context_window
            ),
            t("Group member information", "群成员信息查询").to_string(),
            t("Active reply judgement", "主动回复判断").to_string(),
            t("Quote, mention, and reactions", "引用艾特和贴表情").to_string(),
            t("Safety checks", "违规判断").to_string(),
            t("Affection and relationship", "好感度与关系").to_string(),
            t("Identity mappings", "识人映射").to_string(),
        ];
        draw_menu(
            stdout,
            t(" GROUP REAL CONTEXT ", " 群聊真实上下文回复 "),
            &options,
            selected,
            "",
        )?;
        match read_key()? {
            KeyCode::Char('q') | KeyCode::Esc => {
                settings.normalize();
                let mut candidate = config.clone();
                apply_real_context_values(&mut candidate, enabled, &settings);
                if let Err(error) = candidate.validate() {
                    message(stdout, &error.to_string())?;
                } else {
                    apply_real_context_values(config, enabled, &settings);
                    return Ok(());
                }
            }
            KeyCode::Up | KeyCode::Char('k') => selected = selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => selected = (selected + 1).min(options.len() - 1),
            KeyCode::Enter => match selected {
                0 => enabled = select_bool(stdout, t("Plugin", "插件状态"), enabled)?,
                1 => select_real_context_model_pool(stdout, config, &mut settings.text_models)?,
                2 => edit_real_context_number(
                    stdout,
                    t("Reply context window", "回复上下文消息数"),
                    settings.reply_context_window,
                    &mut settings,
                    |candidate, value| candidate.reply_context_window = value,
                )?,
                3 => edit_real_context_history(stdout, &mut settings)?,
                4 => match StateStore::new(paths) {
                    Ok(state) => edit_real_context_active_reply(stdout, &state, &mut settings)?,
                    Err(error) => message(
                        stdout,
                        &format!(
                            "{}: {error}",
                            t("Unable to open persistent state", "无法打开持久状态数据库")
                        ),
                    )?,
                },
                5 => edit_real_context_reply_target(stdout, &mut settings)?,
                6 => edit_real_context_moderation(stdout, &mut settings)?,
                7 => edit_real_context_affection(stdout, config, &mut settings)?,
                8 => edit_real_context_identities(stdout, &mut settings)?,
                _ => {}
            },
            _ => {}
        }
    }
}

pub(crate) fn edit_real_context_history(
    stdout: &mut io::Stdout,
    settings: &mut RealContextPluginSettings,
) -> Result<()> {
    loop {
        let mut fields = vec![Field::new(
            t(
                "Maximum group member search results",
                "群成员搜索工具最大返回数量",
            ),
            settings.group_member_search_max_results.to_string(),
        )];
        if !run_form(
            stdout,
            t(" GROUP MEMBER INFORMATION ", " 群成员信息查询 "),
            &mut fields,
        )? {
            return Ok(());
        }
        let mut candidate = settings.clone();
        let parsed = (|| -> std::result::Result<(), String> {
            candidate.group_member_search_max_results = real_context_value(&fields, 0)?;
            candidate.validate().map_err(|error| error.to_string())
        })();
        match parsed {
            Ok(()) => {
                *settings = candidate;
                return Ok(());
            }
            Err(error) => message(stdout, &error)?,
        }
    }
}

pub(crate) fn edit_real_context_active_reply(
    stdout: &mut io::Stdout,
    state: &StateStore,
    settings: &mut RealContextPluginSettings,
) -> Result<()> {
    let mut selected = 0usize;
    loop {
        let skip_list_summary = active_judgement_skip_ids(state)
            .map(|ids| ids.len().to_string())
            .unwrap_or_else(|_| t("unavailable", "不可用").to_string());
        let options = vec![
            format!(
                "{}: {}",
                t("Scoring and restraint", "评分与克制"),
                boolean_label(settings.active_reply_enable)
            ),
            format!(
                "{}: {}",
                t("Inherit persona during judgement", "判断时继承人格"),
                boolean_label(settings.judge_include_persona)
            ),
            format!(
                "{}: {}",
                t("Custom prompt", "自定义提示词"),
                if settings.judge_persona_prompt.trim().is_empty() {
                    t("none", "未设置")
                } else {
                    t("set", "已设置")
                }
            ),
            format!(
                "{}: {}",
                t("Random judgement probability", "随机进入判断的概率"),
                settings.active_judge_probability
            ),
            format!(
                "{}: {}",
                t("Reply threshold", "回复阈值"),
                settings.reply_threshold
            ),
            format!(
                "{}: {}",
                t("Skip image-only messages", "跳过纯图片消息"),
                boolean_label(settings.skip_pure_image_active_judge)
            ),
            format!(
                "{}: {}",
                t("QQ ids that skip active judgement", "跳过主动判断的 QQ 号"),
                skip_list_summary
            ),
            format!(
                "{}: {}",
                t(
                    "New message supersedes pending judgement",
                    "新消息覆盖待判断消息",
                ),
                boolean_label(settings.active_reply_supersede_enable)
            ),
            format!(
                "{}: {}",
                t("Supersede window (seconds)", "覆盖窗口（秒）"),
                settings.active_reply_supersede_window_seconds
            ),
            format!(
                "{}: {}",
                t("Reply restraint", "回复克制"),
                boolean_label(settings.reply_restraint_enable)
            ),
            format!(
                "{}: {}",
                t("Restraint recovery (minutes)", "克制恢复时间（分钟）"),
                settings.reply_restraint_recover_minutes
            ),
            format!(
                "{}: {}",
                t("Restraint strength", "克制强度"),
                real_context_restraint_label(&settings.reply_restraint_strength)
            ),
            format!(
                "{}: {}",
                t("Restraint multiplier", "克制倍率"),
                settings.reply_restraint_multiplier
            ),
            t("Continuation window", "续聊窗口").to_string(),
            t("Trigger methods", "触发方式").to_string(),
            t("Concurrency and weights", "并发与权重").to_string(),
            format!(
                "{}: {}",
                t("Judge context window", "判断上下文消息数"),
                settings.judge_context_window
            ),
        ];
        draw_menu(
            stdout,
            t(" ACTIVE REPLY JUDGEMENT ", " 主动回复判断 "),
            &options,
            selected,
            "",
        )?;
        match read_key()? {
            KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
            KeyCode::Up | KeyCode::Char('k') => selected = selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => selected = (selected + 1).min(options.len() - 1),
            KeyCode::Enter => match selected {
                0 => {
                    settings.active_reply_enable = select_bool(
                        stdout,
                        t("Scoring and restraint", "评分与克制"),
                        settings.active_reply_enable,
                    )?
                }
                1 => {
                    settings.judge_include_persona = select_bool(
                        stdout,
                        t("Inherit persona during judgement", "判断时继承人格"),
                        settings.judge_include_persona,
                    )?
                }
                2 => edit_textarea(stdout, &mut settings.judge_persona_prompt)?,
                3 => edit_real_context_number(
                    stdout,
                    t("Random judgement probability", "随机进入判断的概率"),
                    settings.active_judge_probability,
                    settings,
                    |candidate, value| candidate.active_judge_probability = value,
                )?,
                4 => edit_real_context_number(
                    stdout,
                    t("Reply threshold", "回复阈值"),
                    settings.reply_threshold,
                    settings,
                    |candidate, value| candidate.reply_threshold = value,
                )?,
                5 => {
                    settings.skip_pure_image_active_judge = select_bool(
                        stdout,
                        t("Skip image-only messages", "跳过纯图片消息"),
                        settings.skip_pure_image_active_judge,
                    )?
                }
                6 => {
                    edit_active_judgement_skip_ids(stdout, state)?;
                }
                7 => {
                    settings.active_reply_supersede_enable = select_bool(
                        stdout,
                        t(
                            "New message supersedes pending judgement",
                            "新消息覆盖待判断消息",
                        ),
                        settings.active_reply_supersede_enable,
                    )?
                }
                8 => edit_real_context_number(
                    stdout,
                    t("Supersede window (seconds)", "覆盖窗口（秒）"),
                    settings.active_reply_supersede_window_seconds,
                    settings,
                    |candidate, value| candidate.active_reply_supersede_window_seconds = value,
                )?,
                9 => {
                    settings.reply_restraint_enable = select_bool(
                        stdout,
                        t("Reply restraint", "回复克制"),
                        settings.reply_restraint_enable,
                    )?
                }
                10 => edit_real_context_number(
                    stdout,
                    t("Restraint recovery (minutes)", "克制恢复时间（分钟）"),
                    settings.reply_restraint_recover_minutes,
                    settings,
                    |candidate, value| candidate.reply_restraint_recover_minutes = value,
                )?,
                11 => edit_real_context_restraint_strength(stdout, settings)?,
                12 => edit_real_context_number(
                    stdout,
                    t("Restraint multiplier", "克制倍率"),
                    settings.reply_restraint_multiplier,
                    settings,
                    |candidate, value| candidate.reply_restraint_multiplier = value,
                )?,
                13 => edit_real_context_continuation(stdout, settings)?,
                14 => edit_real_context_triggers(stdout, settings)?,
                15 => edit_real_context_judge_advanced(stdout, settings)?,
                16 => edit_real_context_number(
                    stdout,
                    t("Judge context window", "判断上下文消息数"),
                    settings.judge_context_window,
                    settings,
                    |candidate, value| candidate.judge_context_window = value,
                )?,
                _ => {}
            },
            _ => {}
        }
    }
}

pub(crate) fn edit_active_judgement_skip_ids(
    stdout: &mut io::Stdout,
    state: &StateStore,
) -> Result<()> {
    let original = match active_judgement_skip_ids(state) {
        Ok(ids) => ids,
        Err(error) => {
            message(
                stdout,
                &format!(
                    "{}: {error}",
                    t(
                        "Unable to read the active judgement skip list",
                        "无法读取主动判断跳过名单"
                    )
                ),
            )?;
            return Ok(());
        }
    };
    let mut edited = original.clone();
    edit_qq_id_list(
        stdout,
        t(" ACTIVE JUDGEMENT SKIP QQ IDS ", " 跳过主动判断的 QQ 号 "),
        t("QQ id", "QQ 号"),
        &mut edited,
    )?;
    if let Err(error) = apply_active_judgement_skip_editor_changes(state, &original, &edited) {
        message(
            stdout,
            &format!(
                "{}: {error}",
                t(
                    "Unable to update the active judgement skip list",
                    "无法更新主动判断跳过名单"
                )
            ),
        )?;
    }
    Ok(())
}

pub(crate) fn edit_real_context_restraint_strength(
    stdout: &mut io::Stdout,
    settings: &mut RealContextPluginSettings,
) -> Result<()> {
    loop {
        let mut fields = vec![Field::new(
            t("Restraint strength", "克制强度"),
            real_context_restraint_label(&settings.reply_restraint_strength).to_string(),
        )
        .choices(&[t("Light", "轻度"), t("Medium", "中度"), t("Strong", "强烈")])];
        if !run_form(stdout, t(" RESTRAINT STRENGTH ", " 克制强度 "), &mut fields)? {
            return Ok(());
        }
        let mut candidate = settings.clone();
        let parsed = (|| -> std::result::Result<(), String> {
            candidate.reply_restraint_strength = real_context_restraint_value(&fields[0].value)
                .ok_or_else(|| t("Invalid restraint strength.", "克制强度无效。").to_string())?
                .to_string();
            candidate.validate().map_err(|error| error.to_string())
        })();
        match parsed {
            Ok(()) => {
                *settings = candidate;
                return Ok(());
            }
            Err(error) => message(stdout, &error)?,
        }
    }
}

pub(crate) fn edit_real_context_judge_advanced(
    stdout: &mut io::Stdout,
    settings: &mut RealContextPluginSettings,
) -> Result<()> {
    loop {
        let mut fields = vec![
            Field::new(
                t("Timeout (seconds)", "判断超时（秒）"),
                settings.judge_timeout_seconds.to_string(),
            ),
            Field::new(
                t("Endpoint timeout (seconds)", "单模型超时（秒）"),
                settings.judge_endpoint_timeout_seconds.to_string(),
            ),
            Field::new(
                t(
                    "Global concurrency wait timeout (seconds)",
                    "全局判断并发等待超时（秒）",
                ),
                settings.judge_queue_wait_timeout_seconds.to_string(),
            ),
            Field::new(
                t("Maximum concurrency", "最大并发数"),
                settings.judge_max_concurrency.to_string(),
            ),
            Field::new(
                t("Maximum retries", "最大重试次数"),
                settings.judge_max_retries.to_string(),
            ),
            Field::new(
                t("Relevance weight", "相关性权重"),
                settings.judge_relevance_weight.to_string(),
            ),
            Field::new(
                t("Willingness weight", "意愿权重"),
                settings.judge_willingness_weight.to_string(),
            ),
            Field::new(
                t("Social weight", "社交适合度权重"),
                settings.judge_social_weight.to_string(),
            ),
            Field::new(
                t("Timing weight", "时机权重"),
                settings.judge_timing_weight.to_string(),
            ),
            Field::new(
                t("Continuity weight", "连续性权重"),
                settings.judge_continuity_weight.to_string(),
            ),
            Field::boolean(
                t("Use judgement recommendation", "采用判断建议加减分"),
                settings.judge_should_reply_adjust_enable,
            ),
            Field::new(
                t("Recommended-reply boost", "建议回复加分"),
                settings.judge_should_reply_boost_score.to_string(),
            ),
            Field::new(
                t("Recommended-silence penalty", "建议不回复减分"),
                settings.judge_should_reply_penalty_score.to_string(),
            ),
        ];
        if !run_form(
            stdout,
            t(" JUDGEMENT ADVANCED ", " 主动判断高级设置 "),
            &mut fields,
        )? {
            return Ok(());
        }
        let mut candidate = settings.clone();
        let parsed = (|| -> std::result::Result<(), String> {
            candidate.judge_timeout_seconds = real_context_value(&fields, 0)?;
            candidate.judge_endpoint_timeout_seconds = real_context_value(&fields, 1)?;
            candidate.judge_queue_wait_timeout_seconds = real_context_value(&fields, 2)?;
            candidate.judge_max_concurrency = real_context_value(&fields, 3)?;
            candidate.judge_max_retries = real_context_value(&fields, 4)?;
            candidate.judge_relevance_weight = real_context_value(&fields, 5)?;
            candidate.judge_willingness_weight = real_context_value(&fields, 6)?;
            candidate.judge_social_weight = real_context_value(&fields, 7)?;
            candidate.judge_timing_weight = real_context_value(&fields, 8)?;
            candidate.judge_continuity_weight = real_context_value(&fields, 9)?;
            candidate.judge_should_reply_adjust_enable = real_context_bool(&fields, 10)?;
            candidate.judge_should_reply_boost_score = real_context_value(&fields, 11)?;
            candidate.judge_should_reply_penalty_score = real_context_value(&fields, 12)?;
            candidate.validate().map_err(|error| error.to_string())
        })();
        match parsed {
            Ok(()) => {
                *settings = candidate;
                return Ok(());
            }
            Err(error) => message(stdout, &error)?,
        }
    }
}

pub(crate) fn edit_real_context_triggers(
    stdout: &mut io::Stdout,
    settings: &mut RealContextPluginSettings,
) -> Result<()> {
    loop {
        let mut fields = vec![
            Field::boolean(
                t("Take over direct triggers", "接管直接触发"),
                settings.takeover_direct_trigger_enable,
            ),
            Field::new(
                t("Direct-trigger boost", "直接触发加分"),
                settings.takeover_direct_trigger_boost_score.to_string(),
            ),
            Field::boolean(
                t(
                    "Privileged users skip group active judgement",
                    "管理员和私聊白名单跳过群聊主动回复判断",
                ),
                settings.privileged_direct_trigger_skip_active_judgement,
            ),
        ];
        if !run_form(stdout, t(" TRIGGER METHODS ", " 触发方式 "), &mut fields)? {
            return Ok(());
        }
        let mut candidate = settings.clone();
        let parsed = (|| -> std::result::Result<(), String> {
            candidate.takeover_direct_trigger_enable = real_context_bool(&fields, 0)?;
            candidate.takeover_direct_trigger_boost_score = real_context_value(&fields, 1)?;
            candidate.privileged_direct_trigger_skip_active_judgement =
                real_context_bool(&fields, 2)?;
            candidate.validate().map_err(|error| error.to_string())
        })();
        match parsed {
            Ok(()) => {
                *settings = candidate;
                return Ok(());
            }
            Err(error) => message(stdout, &error)?,
        }
    }
}

pub(crate) fn edit_real_context_continuation(
    stdout: &mut io::Stdout,
    settings: &mut RealContextPluginSettings,
) -> Result<()> {
    loop {
        let mut fields = vec![
            Field::boolean(
                t("Natural continuation", "自然续聊"),
                settings.continuation_enable,
            ),
            Field::new(
                t("Continuation window (seconds)", "续聊窗口（秒）"),
                settings.continuation_window_seconds.to_string(),
            ),
            Field::new(
                t("Continuation boost", "续聊加分"),
                settings.continuation_boost_score.to_string(),
            ),
        ];
        if !run_form(
            stdout,
            t(" CONTINUATION WINDOW ", " 续聊窗口 "),
            &mut fields,
        )? {
            return Ok(());
        }
        let mut candidate = settings.clone();
        let parsed = (|| -> std::result::Result<(), String> {
            candidate.continuation_enable = real_context_bool(&fields, 0)?;
            candidate.continuation_window_seconds = real_context_value(&fields, 1)?;
            candidate.continuation_boost_score = real_context_value(&fields, 2)?;
            candidate.validate().map_err(|error| error.to_string())
        })();
        match parsed {
            Ok(()) => {
                *settings = candidate;
                return Ok(());
            }
            Err(error) => message(stdout, &error)?,
        }
    }
}

pub(crate) fn edit_real_context_reply_target(
    stdout: &mut io::Stdout,
    settings: &mut RealContextPluginSettings,
) -> Result<()> {
    let mut selected = 0usize;
    loop {
        let options = vec![
            format!(
                "{}: {}",
                t("Target the replied-to user", "定向回复对象"),
                boolean_label(settings.reply_target_enable)
            ),
            format!(
                "{}: {}",
                t("Quote target message", "引用目标消息"),
                boolean_label(settings.reply_target_quote_enable)
            ),
            format!(
                "{}: {}",
                t(
                    "Quote after intervening messages from others",
                    "和原消息间隔几条消息则引用"
                ),
                settings.reply_target_quote_after_other_messages
            ),
            format!(
                "{}: {}",
                t("Mention target user", "艾特目标用户"),
                boolean_label(settings.reply_target_mention_enable)
            ),
            format!(
                "{}: {}",
                t("Mention after elapsed seconds", "回复时间超过多少秒则艾特"),
                settings.reply_target_mention_after_seconds
            ),
            format!(
                "{}: {}",
                t(
                    "React after an active reply is accepted",
                    "确认主动回复后贴表情"
                ),
                boolean_label(settings.active_reply_reaction_enable)
            ),
            format!(
                "{}: {}",
                t("Active-reply reaction id", "主动回复贴的表情ID"),
                settings
                    .active_reply_reaction_emoji_ids
                    .first()
                    .copied()
                    .unwrap_or_default()
            ),
            format!(
                "{}: {}",
                t("Reaction cleanup timeout (seconds)", "表情清理超时（秒）"),
                settings.active_reply_reaction_timeout_seconds
            ),
        ];
        draw_menu(
            stdout,
            t(" QUOTE, MENTION, AND REACTIONS ", " 引用艾特和贴表情 "),
            &options,
            selected,
            "",
        )?;
        match read_key()? {
            KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
            KeyCode::Up | KeyCode::Char('k') => selected = selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => selected = (selected + 1).min(options.len() - 1),
            KeyCode::Enter => match selected {
                0 => {
                    settings.reply_target_enable = select_bool(
                        stdout,
                        t("Target the replied-to user", "定向回复对象"),
                        settings.reply_target_enable,
                    )?
                }
                1 => {
                    settings.reply_target_quote_enable = select_bool(
                        stdout,
                        t("Quote target message", "引用目标消息"),
                        settings.reply_target_quote_enable,
                    )?
                }
                2 => edit_real_context_number(
                    stdout,
                    t(
                        "Quote after intervening messages from others",
                        "和原消息间隔几条消息则引用",
                    ),
                    settings.reply_target_quote_after_other_messages,
                    settings,
                    |candidate, value| candidate.reply_target_quote_after_other_messages = value,
                )?,
                3 => {
                    settings.reply_target_mention_enable = select_bool(
                        stdout,
                        t("Mention target user", "艾特目标用户"),
                        settings.reply_target_mention_enable,
                    )?
                }
                4 => edit_real_context_number(
                    stdout,
                    t("Mention after elapsed seconds", "回复时间超过多少秒则艾特"),
                    settings.reply_target_mention_after_seconds,
                    settings,
                    |candidate, value| candidate.reply_target_mention_after_seconds = value,
                )?,
                5 => {
                    settings.active_reply_reaction_enable = select_bool(
                        stdout,
                        t(
                            "React after an active reply is accepted",
                            "确认主动回复后贴表情",
                        ),
                        settings.active_reply_reaction_enable,
                    )?
                }
                6 => {
                    let current = settings
                        .active_reply_reaction_emoji_ids
                        .first()
                        .copied()
                        .unwrap_or_default();
                    edit_real_context_number(
                        stdout,
                        t("Active-reply reaction id", "主动回复贴的表情ID"),
                        current,
                        settings,
                        |candidate, value| candidate.active_reply_reaction_emoji_ids = vec![value],
                    )?;
                }
                7 => edit_real_context_number(
                    stdout,
                    t("Reaction cleanup timeout (seconds)", "表情清理超时（秒）"),
                    settings.active_reply_reaction_timeout_seconds,
                    settings,
                    |candidate, value| candidate.active_reply_reaction_timeout_seconds = value,
                )?,
                _ => {}
            },
            _ => {}
        }
    }
}

pub(crate) fn edit_real_context_moderation(
    stdout: &mut io::Stdout,
    settings: &mut RealContextPluginSettings,
) -> Result<()> {
    let mut selected = 0usize;
    loop {
        let options = vec![
            format!(
                "{}: {}",
                t("Moderation", "违规判断"),
                boolean_label(settings.moderation_enable)
            ),
            format!(
                "{}: {}",
                t("Keyword precheck", "关键词触发初判"),
                boolean_label(settings.moderation_keyword_trigger_enable)
            ),
            format!(
                "{}: {}",
                t("Moderation keywords", "违规初判关键词"),
                settings.moderation_keywords.len()
            ),
            format!(
                "{}: {}",
                t("Moderation rules prompt", "违规规则提示词"),
                if settings.moderation_custom_rules.is_empty() {
                    t("none", "未设置")
                } else {
                    t("set", "已设置")
                }
            ),
            format!(
                "{}: {}",
                t("Minimum severity", "判断违规的阈值"),
                settings.moderation_min_severity
            ),
            format!(
                "{}: {}",
                t("Moderation timeout (seconds)", "违规判断超时"),
                settings.moderation_timeout_seconds
            ),
            format!(
                "{}: {}",
                t("Decode Base64 text", "Base64 违规初判"),
                boolean_label(settings.base64_moderation_enable)
            ),
            format!(
                "{}: {}",
                t("Minimum Base64 length", "Base64 最短长度"),
                settings.base64_moderation_min_chars
            ),
            format!(
                "{}: {}",
                t("Maximum decoded characters", "Base64 最大解码字符数"),
                settings.base64_moderation_max_decoded_chars
            ),
            format!(
                "{}: {}",
                t("Minimum printable ratio", "Base64 最低可打印比例"),
                settings.base64_moderation_min_printable_ratio
            ),
        ];
        draw_menu(
            stdout,
            t(" SAFETY CHECKS ", " 违规判断 "),
            &options,
            selected,
            "",
        )?;
        match read_key()? {
            KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
            KeyCode::Up | KeyCode::Char('k') => selected = selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => selected = (selected + 1).min(options.len() - 1),
            KeyCode::Enter if selected == 0 => {
                settings.moderation_enable = select_bool(
                    stdout,
                    t("Moderation", "违规判断"),
                    settings.moderation_enable,
                )?
            }
            KeyCode::Enter if selected == 1 => {
                settings.moderation_keyword_trigger_enable = select_bool(
                    stdout,
                    t("Keyword precheck", "关键词触发初判"),
                    settings.moderation_keyword_trigger_enable,
                )?
            }
            KeyCode::Enter if selected == 2 => edit_real_context_string_lines(
                stdout,
                t(" MODERATION KEYWORDS ", " 违规初判关键词 "),
                &mut settings.moderation_keywords,
                256,
            )?,
            KeyCode::Enter if selected == 3 => {
                edit_textarea(stdout, &mut settings.moderation_custom_rules)?
            }
            KeyCode::Enter if selected == 4 => edit_real_context_number(
                stdout,
                t("Minimum severity", "判断违规的阈值"),
                settings.moderation_min_severity,
                settings,
                |candidate, value| candidate.moderation_min_severity = value,
            )?,
            KeyCode::Enter if selected == 5 => edit_real_context_number(
                stdout,
                t("Moderation timeout (seconds)", "违规判断超时"),
                settings.moderation_timeout_seconds,
                settings,
                |candidate, value| candidate.moderation_timeout_seconds = value,
            )?,
            KeyCode::Enter if selected == 6 => {
                settings.base64_moderation_enable = select_bool(
                    stdout,
                    t("Decode Base64 text", "Base64 违规初判"),
                    settings.base64_moderation_enable,
                )?
            }
            KeyCode::Enter if selected == 7 => edit_real_context_number(
                stdout,
                t("Minimum Base64 length", "Base64 最短长度"),
                settings.base64_moderation_min_chars,
                settings,
                |candidate, value| candidate.base64_moderation_min_chars = value,
            )?,
            KeyCode::Enter if selected == 8 => edit_real_context_number(
                stdout,
                t("Maximum decoded characters", "Base64 最大解码字符数"),
                settings.base64_moderation_max_decoded_chars,
                settings,
                |candidate, value| candidate.base64_moderation_max_decoded_chars = value,
            )?,
            KeyCode::Enter if selected == 9 => edit_real_context_number(
                stdout,
                t("Minimum printable ratio", "Base64 最低可打印比例"),
                settings.base64_moderation_min_printable_ratio,
                settings,
                |candidate, value| candidate.base64_moderation_min_printable_ratio = value,
            )?,
            _ => {}
        }
    }
}
