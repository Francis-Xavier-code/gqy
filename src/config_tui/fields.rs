//! fields — 自 src/config_tui.rs 拆分。

use super::*;

pub(crate) fn auto_configure_model_tags(
    paths: &GQYPaths,
    provider: &mut ProviderConfig,
    model: &str,
) {
    if provider.model_modalities.contains_key(model) {
        return;
    }
    if let Some(modalities) =
        crate::models_cache::input_modalities_blocking(paths, &provider.id, model)
            .filter(|modalities| !modalities.is_empty())
    {
        provider
            .model_modalities
            .insert(model.to_string(), modalities);
    }
}

pub(crate) fn models_url(base_url: &str) -> String {
    let mut url = base_url.trim().trim_end_matches('/').to_string();
    if url.ends_with("/chat/completions") {
        url.truncate(url.len() - "/chat/completions".len());
    }
    if url.ends_with("/v1") {
        format!("{url}/models")
    } else {
        format!("{url}/v1/models")
    }
}

#[derive(Deserialize)]
pub(crate) struct ModelsResponse {
    data: Vec<ModelInfo>,
}

#[derive(Deserialize)]
pub(crate) struct ModelInfo {
    id: String,
}

pub(crate) fn select_active_provider(
    stdout: &mut io::Stdout,
    config: &mut AppConfig,
) -> Result<()> {
    let mut choices = config.text_provider_model_choices();
    if choices.is_empty() {
        message(
            stdout,
            t(
                "No text models are selected. Activate one with Tab under Providers and models first.",
                "没有已勾选的文本模型，请先在供应商和模型里用 Tab 激活模型。",
            ),
        )?;
        return Ok(());
    }
    let mut selected = choices
        .iter()
        .position(|choice| config.is_active_provider_model(&choice.provider_id, &choice.model))
        .unwrap_or(0);
    loop {
        let options = choices
            .iter()
            .map(|choice| {
                let marker = if config.is_active_provider_model(&choice.provider_id, &choice.model)
                {
                    "[*] "
                } else {
                    "[ ] "
                };
                format!("{marker}{}", choice.label())
            })
            .collect::<Vec<_>>();
        draw_menu(
            stdout,
            t(" SELECT TEXT MODEL ", " 选择文本模型 "),
            &options,
            selected,
            t(
                "[Tab]activate/deactivate [Enter/q]confirm [d]remove",
                "[Tab]激活/取消 [Enter/q]确认 [d]移除",
            ),
        )?;
        match read_key()? {
            KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
            KeyCode::Up | KeyCode::Char('k') => selected = selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => selected = (selected + 1).min(options.len() - 1),
            KeyCode::Enter => return Ok(()),
            KeyCode::Tab => {
                let choice = choices[selected].clone();
                config.toggle_active_provider_model(&choice.provider_id, &choice.model)?;
            }
            KeyCode::Char('d') => {
                let choice = choices[selected].clone();
                config.remove_active_provider_model(&choice.provider_id, &choice.model)?;
                choices = config.text_provider_model_choices();
                if choices.is_empty() {
                    message(
                        stdout,
                        t(
                            "The active model was removed; no models are currently available.",
                            "已移除激活模型，当前没有可用模型。",
                        ),
                    )?;
                    return Ok(());
                }
                selected = selected.min(choices.len().saturating_sub(1));
            }
            _ => {}
        }
    }
}

use crate::config::EMBEDDING_MODALITY;

pub(crate) fn model_is_embedding(provider: &ProviderConfig, model: &str) -> bool {
    AppConfig::model_is_embedding(provider, model)
}

pub(crate) fn embedding_model_label(config: &AppConfig) -> String {
    if config.embedding.is_configured() {
        format!(
            "{}/{}",
            config.embedding.provider_id.trim(),
            config.embedding.model.trim()
        )
    } else {
        t("not set", "未设置").to_string()
    }
}

pub(crate) fn edit_embedding_model(stdout: &mut io::Stdout, config: &mut AppConfig) -> Result<()> {
    let mut candidates: Vec<(String, String)> = Vec::new();
    for provider in &config.providers {
        for model in &provider.models {
            if model_is_embedding(provider, model) {
                candidates.push((provider.id.clone(), model.clone()));
            }
        }
    }
    if candidates.is_empty() {
        message(
            stdout,
            t(
                "No embedding models yet. Mark one in Providers and models -> Edit model.",
                "还没有语义模型。请在「供应商和模型」->「编辑模型」里把某个模型标记为语义模型。",
            ),
        )?;
        return Ok(());
    }
    let mut options: Vec<String> = candidates
        .iter()
        .map(|(provider, model)| format!("{provider}/{model}"))
        .collect();
    options.push(t("Advanced settings", "高级设置").to_string());
    options.push(t("Clear selection", "清除选择").to_string());
    let mut selected = candidates
        .iter()
        .position(|(provider, model)| {
            provider == config.embedding.provider_id.trim()
                && model == config.embedding.model.trim()
        })
        .unwrap_or(0);
    loop {
        draw_menu(
            stdout,
            t(" EMBEDDING MODEL ", " EMBEDDING 模型 "),
            &options,
            selected,
            t(
                "[Enter]select [j/k]move [q]back",
                "[Enter]选择 [j/k]移动 [q]返回",
            ),
        )?;
        match read_key()? {
            KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
            KeyCode::Up | KeyCode::Char('k') => selected = selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => selected = (selected + 1).min(options.len() - 1),
            KeyCode::Enter => {
                if selected == options.len() - 1 {
                    config.embedding.provider_id.clear();
                    config.embedding.model.clear();
                    return Ok(());
                }
                if selected == options.len() - 2 {
                    edit_embedding_advanced(stdout, config)?;
                    continue;
                }
                let (provider, model) = candidates[selected].clone();
                config.embedding.provider_id = provider;
                config.embedding.model = model;
                return Ok(());
            }
            _ => {}
        }
    }
}

pub(crate) fn edit_embedding_advanced(
    stdout: &mut io::Stdout,
    config: &mut AppConfig,
) -> Result<()> {
    let mut fields = vec![
        Field::new(
            t("Request timeout (seconds)", "请求超时（秒）"),
            config.embedding.timeout_seconds.to_string(),
        ),
        Field::new(
            t("Similarity floor (0-1)", "相似度下限（0-1）"),
            config.embedding.min_score.to_string(),
        ),
    ];
    if !run_form(
        stdout,
        t(" EMBEDDING ADVANCED ", " EMBEDDING 高级设置 "),
        &mut fields,
    )? {
        return Ok(());
    }
    let timeout: u64 = fields[0]
        .value
        .trim()
        .parse()
        .map_err(|_| anyhow::anyhow!(t("Invalid timeout.", "超时数值无效。")))?;
    let score: f32 = fields[1]
        .value
        .trim()
        .parse()
        .map_err(|_| anyhow::anyhow!(t("Invalid similarity floor.", "相似度下限无效。")))?;
    if timeout == 0 {
        return Err(anyhow::anyhow!(t(
            "Timeout must be positive.",
            "超时必须大于 0。"
        )));
    }
    if !(0.0..=1.0).contains(&score) {
        return Err(anyhow::anyhow!(t(
            "Similarity floor must be between 0 and 1.",
            "相似度下限必须在 0 与 1 之间。"
        )));
    }
    config.embedding.timeout_seconds = timeout;
    config.embedding.min_score = score;
    Ok(())
}

pub(crate) fn subagent_tiers_label(config: &AppConfig) -> String {
    let counts = crate::config::ModelTier::ALL.map(|tier| config.subagent_tier_choices(tier).len());
    if counts.iter().all(|count| *count == 0) {
        t("not configured", "未配置").to_string()
    } else {
        format!(
            "cheap:{} balanced:{} strong:{}",
            counts[0], counts[1], counts[2]
        )
    }
}

pub(crate) fn tier_display_name(tier: crate::config::ModelTier) -> &'static str {
    use crate::config::ModelTier;
    match tier {
        ModelTier::Cheap => "cheap",
        ModelTier::Balanced => "balanced",
        ModelTier::Strong => "strong",
    }
}

/// Tier pool overview: pick a tier, then toggle models for it. Subagents
/// choose a tier by task complexity; unconfigured pools fall back to the
/// main model pool.
pub(crate) fn select_subagent_tiers(stdout: &mut io::Stdout, config: &mut AppConfig) -> Result<()> {
    use crate::config::ModelTier;
    let mut selected = 0usize;
    loop {
        let options = ModelTier::ALL
            .iter()
            .map(|tier| {
                let pool = config.subagent_tier_choices(*tier);
                let summary = if pool.is_empty() {
                    t("fallback to main model", "回退主模型").to_string()
                } else {
                    pool.iter()
                        .map(|choice| choice.model.clone())
                        .collect::<Vec<_>>()
                        .join(", ")
                };
                let hint = match tier {
                    ModelTier::Cheap => t("simple tasks", "简单任务"),
                    ModelTier::Balanced => t("normal tasks", "普通任务"),
                    ModelTier::Strong => t("complex tasks", "复杂任务"),
                };
                format!("{} ({hint}): {summary}", tier_display_name(*tier))
            })
            .collect::<Vec<_>>();
        draw_menu(
            stdout,
            t(" SUBAGENT TIER POOLS ", " 子代理档位池 "),
            &options,
            selected,
            t(
                "[Enter]configure tier [j/k]move [q]back",
                "[Enter]配置该档位 [j/k]移动 [q]返回",
            ),
        )?;
        match read_key()? {
            KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
            KeyCode::Up | KeyCode::Char('k') => selected = selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => selected = (selected + 1).min(options.len() - 1),
            KeyCode::Enter => {
                select_subagent_tier_models(stdout, config, ModelTier::ALL[selected])?
            }
            _ => {}
        }
    }
}

/// Model multi-select for one tier pool, mirroring the text-model picker:
/// candidates are the configured text models, Tab toggles membership.
pub(crate) fn select_subagent_tier_models(
    stdout: &mut io::Stdout,
    config: &mut AppConfig,
    tier: crate::config::ModelTier,
) -> Result<()> {
    let choices = config.text_provider_model_choices();
    if choices.is_empty() {
        message(
            stdout,
            t(
                "No text models are configured. Add models under Providers and models first.",
                "没有可用的文本模型，请先在供应商和模型里添加模型。",
            ),
        )?;
        return Ok(());
    }
    let mut selected = 0usize;
    let title = format!(
        " {} · {} ",
        t("TIER POOL", "档位池"),
        tier_display_name(tier)
    );
    loop {
        let options = choices
            .iter()
            .map(|choice| {
                let marker =
                    if config.is_subagent_tier_model(tier, &choice.provider_id, &choice.model) {
                        "[*] "
                    } else {
                        "[ ] "
                    };
                format!("{marker}{}", choice.label())
            })
            .collect::<Vec<_>>();
        draw_menu(
            stdout,
            &title,
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
            KeyCode::Tab => {
                let choice = choices[selected].clone();
                config.toggle_subagent_tier_model(tier, &choice.provider_id, &choice.model)?;
            }
            _ => {}
        }
    }
}

pub(crate) fn platforms_label(config: &AppConfig) -> String {
    if config.platforms.qq.enabled {
        t("Tencent QQ enabled", "腾讯 QQ 已启用").to_string()
    } else {
        t("disabled", "未启用").to_string()
    }
}

pub(crate) fn select_platforms(
    stdout: &mut io::Stdout,
    paths: &GQYPaths,
    config: &mut AppConfig,
) -> Result<()> {
    let mut selected = 0usize;
    loop {
        let state = if config.platforms.qq.enabled {
            t("enabled", "已启用")
        } else {
            t("disabled", "未启用")
        };
        let options = vec![
            format!("{}: {state}", t("Tencent QQ", "腾讯 QQ")),
            format!(
                "{}: {}",
                t("Command trigger prefix", "命令触发前缀"),
                config.platforms.command_prefix
            ),
            t("Command list", "命令列表").to_string(),
        ];
        draw_menu(
            stdout,
            t(" IM PLATFORMS ", " 接入通讯平台 "),
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
                0 => edit_qq(stdout, paths, config)?,
                1 => edit_platform_command_prefix(stdout, config)?,
                2 => select_platform_commands(stdout, config)?,
                _ => {}
            },
            _ => {}
        }
    }
}

pub(crate) fn edit_platform_command_prefix(
    stdout: &mut io::Stdout,
    config: &mut AppConfig,
) -> Result<()> {
    let Some(value) = edit_inline_value(
        stdout,
        t(" COMMAND TRIGGER PREFIX ", " 命令触发前缀 "),
        &config.platforms.command_prefix,
        false,
    )?
    else {
        return Ok(());
    };
    let value = value.trim();
    if value.is_empty()
        || value.chars().count() > MAX_PLATFORM_COMMAND_PREFIX_CHARS
        || value
            .chars()
            .any(|character| character.is_whitespace() || character.is_control())
    {
        message(
            stdout,
            t(
                "The prefix must be 1-32 characters and cannot contain whitespace.",
                "前缀必须为 1 到 32 个字符，且不能包含空白字符。",
            ),
        )?;
    } else {
        config.platforms.command_prefix = value.to_string();
    }
    Ok(())
}

pub(crate) fn platform_command_permission_label(
    permission: PlatformCommandPermission,
) -> &'static str {
    match permission {
        PlatformCommandPermission::Everyone => t("Everyone", "所有人"),
        PlatformCommandPermission::AdminOnly => t("Administrators only", "仅管理员"),
    }
}

pub(crate) fn select_platform_commands(
    stdout: &mut io::Stdout,
    config: &mut AppConfig,
) -> Result<()> {
    let mut selected = 0usize;
    loop {
        let options = commands::BUILTIN_COMMANDS
            .iter()
            .map(|command| {
                let permission = config
                    .platforms
                    .command_permission(command.id, command.default_permission);
                format!(
                    "{}: {}",
                    command.id,
                    platform_command_permission_label(permission)
                )
            })
            .collect::<Vec<_>>();
        draw_menu(
            stdout,
            t(" PLATFORM COMMANDS ", " 命令列表 "),
            &options,
            selected,
            t(
                "[Enter]set permission [j/k]move [q]back",
                "[Enter]设置权限 [j/k]移动 [q]返回",
            ),
        )?;
        match read_key()? {
            KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
            KeyCode::Up | KeyCode::Char('k') => selected = selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => selected = (selected + 1).min(options.len() - 1),
            KeyCode::Enter => {
                edit_platform_command_permission(
                    stdout,
                    config,
                    &commands::BUILTIN_COMMANDS[selected],
                )?;
            }
            _ => {}
        }
    }
}

pub(crate) fn edit_platform_command_permission(
    stdout: &mut io::Stdout,
    config: &mut AppConfig,
    command: &PlatformCommandDescriptor,
) -> Result<()> {
    let permissions = [
        PlatformCommandPermission::Everyone,
        PlatformCommandPermission::AdminOnly,
    ];
    let current = config
        .platforms
        .command_permission(command.id, command.default_permission);
    let mut selected = permissions
        .iter()
        .position(|permission| *permission == current)
        .unwrap_or(0);
    loop {
        let options = permissions
            .iter()
            .map(|permission| platform_command_permission_label(*permission).to_string())
            .collect::<Vec<_>>();
        let title = format!(" {} · {} ", t("COMMAND PERMISSION", "命令权限"), command.id);
        draw_menu(stdout, &title, &options, selected, "")?;
        match read_key()? {
            KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
            KeyCode::Up | KeyCode::Char('k') => selected = selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                selected = (selected + 1).min(permissions.len() - 1)
            }
            KeyCode::Enter => {
                config.platforms.set_command_permission(
                    command.id,
                    permissions[selected],
                    command.default_permission,
                );
                return Ok(());
            }
            _ => {}
        }
    }
}

pub(crate) fn enabled_label(value: bool) -> &'static str {
    if value {
        t("enabled", "已启用")
    } else {
        t("disabled", "已禁用")
    }
}

pub(crate) fn edit_qq(
    stdout: &mut io::Stdout,
    paths: &GQYPaths,
    config: &mut AppConfig,
) -> Result<()> {
    let mut selected = 0usize;
    loop {
        let qq = &config.platforms.qq;
        let options = vec![
            format!(
                "{}: {}",
                t("Enabled", "是否启用"),
                enabled_label(qq.enabled)
            ),
            format!(
                "{}: {}",
                t("Text model pool", "文本模型池"),
                qq_pool_summary(qq.text_models.as_deref())
            ),
            format!(
                "{}: {}",
                t("Multimodal model pool", "多模态模型池"),
                qq_pool_summary(qq.multimodal_models.as_deref())
            ),
            format!(
                "{}: {}",
                t("Reverse WebSocket port", "反向 WebSocket 端口"),
                qq.reverse_ws_port
            ),
            format!(
                "{}: {}",
                t("Reverse WebSocket token", "反向 WebSocket 验证 Token"),
                if qq.access_token.is_empty() {
                    t("empty", "未设置")
                } else {
                    "********"
                }
            ),
            format!(
                "{}: {}",
                t("User identification", "用户识别"),
                enabled_label(qq.user_identification)
            ),
            format!(
                "{}: {}",
                t("Show group name", "显示群名称"),
                enabled_label(qq.show_group_name)
            ),
            format!(
                "{}: {}",
                t("Write persona memory", "写入人格记忆"),
                enabled_label(qq.memory.write_enabled)
            ),
            format!(
                "{}: {}",
                t(
                    "Administrator QQ ids allowed to use the terminal",
                    "允许使用终端的管理员 QQ 号"
                ),
                qq.admin_users.len()
            ),
            format!(
                "{}: {}",
                t(
                    "Allow non-admin computer access",
                    "是否允许非管理员使用电脑"
                ),
                enabled_label(qq.allow_non_admin_host_tools)
            ),
            format!(
                "{}: {}",
                t(
                    "Send intermediate messages in group chats",
                    "群聊是否输出中间消息"
                ),
                enabled_label(qq.group_intermediate_messages)
            ),
            format!(
                "{}: {}",
                t(
                    "Send intermediate messages in private chats",
                    "私聊是否输出中间消息"
                ),
                enabled_label(qq.private_intermediate_messages)
            ),
            format!(
                "{}: {}",
                t("Private whitelist", "私聊白名单"),
                qq.private_chats.whitelist.len()
            ),
            format!(
                "{}: {}",
                t("Non-whitelist model pool", "非白名单模型池"),
                route_pool_summary(
                    qq.non_whitelist_text_models.as_deref(),
                    PlatformModelPoolInheritance::Platform,
                )
            ),
            format!(
                "{}: {}",
                t(
                    "Only private whitelist can add friends",
                    "仅私聊白名单能加好友"
                ),
                enabled_label(qq.private_chats.friend_requests_require_private_whitelist)
            ),
            format!(
                "{}: {}",
                t("Allow non-whitelist private chats", "是否允许非白名单私聊"),
                enabled_label(qq.private_chats.allow_non_whitelist)
            ),
            format!(
                "{}: {}",
                t("Non-whitelist private rate limit", "非白名单私聊限流"),
                rate_limit_label(qq.private_chats.non_whitelist_rate_limit)
            ),
            format!(
                "{}: {}",
                t("Group whitelist", "群聊白名单"),
                qq.group_chats.whitelist.len()
            ),
            format!(
                "{}: {}",
                t("Additional group wake keywords", "额外群聊触发关键词"),
                qq.group_chats.trigger_keywords.len()
            ),
            format!(
                "{}: {}",
                t("Whitelist-group rate limit", "白名单群聊限流"),
                rate_limit_label(qq.group_chats.whitelist_rate_limit)
            ),
            format!(
                "{}: {}",
                t("Allow non-whitelist groups", "是否允许非白名单群聊"),
                enabled_label(qq.group_chats.allow_non_whitelist)
            ),
            format!(
                "{}: {}",
                t("Non-whitelist-group rate limit", "非白名单群聊限流"),
                rate_limit_label(qq.group_chats.non_whitelist_rate_limit)
            ),
            format!(
                "{}: {}",
                t("Conversation concurrency", "会话并发"),
                session_limits_label(qq.session_limits)
            ),
            format!(
                "{}: {}",
                t("Private/group conversation settings", "私聊/群聊专属配置"),
                qq.conversations.len()
            ),
            t("QQ plugins", "QQ 插件配置").to_string(),
            t("Advanced settings", "高级设置").to_string(),
        ];
        draw_menu(
            stdout,
            t(" TENCENT QQ ", " 腾讯 QQ "),
            &options,
            selected,
            "",
        )?;
        let key = read_key()?;
        match key {
            KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
            KeyCode::Up | KeyCode::Char('k') => selected = selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => selected = (selected + 1).min(options.len() - 1),
            KeyCode::Enter | KeyCode::Char(' ') => match selected {
                0 => config.platforms.qq.enabled = !config.platforms.qq.enabled,
                1 if matches!(key, KeyCode::Enter) => select_qq_model_pool(stdout, config, false)?,
                2 if matches!(key, KeyCode::Enter) => select_qq_model_pool(stdout, config, true)?,
                3 if matches!(key, KeyCode::Enter) => {
                    if let Some(value) = edit_u16_value(
                        stdout,
                        t("Reverse WebSocket port", "反向 WebSocket 端口"),
                        config.platforms.qq.reverse_ws_port,
                    )? {
                        if value == 0 {
                            message(
                                stdout,
                                t(
                                    "Port must be between 1 and 65535.",
                                    "端口必须在 1 到 65535 之间。",
                                ),
                            )?;
                        } else {
                            config.platforms.qq.reverse_ws_port = value;
                        }
                    }
                }
                4 if matches!(key, KeyCode::Enter) => edit_qq_token(stdout, config)?,
                5 => {
                    config.platforms.qq.user_identification =
                        !config.platforms.qq.user_identification
                }
                6 => config.platforms.qq.show_group_name = !config.platforms.qq.show_group_name,
                7 => {
                    config.platforms.qq.memory.write_enabled =
                        !config.platforms.qq.memory.write_enabled
                }
                8 if matches!(key, KeyCode::Enter) => edit_qq_id_list(
                    stdout,
                    t(
                        " TERMINAL-ENABLED ADMINISTRATORS ",
                        " 允许使用终端的管理员 QQ 号 ",
                    ),
                    t("QQ id", "QQ 号"),
                    &mut config.platforms.qq.admin_users,
                )?,
                9 => {
                    config.platforms.qq.allow_non_admin_host_tools =
                        !config.platforms.qq.allow_non_admin_host_tools
                }
                10 => {
                    config.platforms.qq.group_intermediate_messages =
                        !config.platforms.qq.group_intermediate_messages
                }
                11 => {
                    config.platforms.qq.private_intermediate_messages =
                        !config.platforms.qq.private_intermediate_messages
                }
                12 if matches!(key, KeyCode::Enter) => edit_qq_id_list(
                    stdout,
                    t(" PRIVATE WHITELIST ", " 私聊白名单 "),
                    t("QQ id", "QQ 号"),
                    &mut config.platforms.qq.private_chats.whitelist,
                )?,
                13 if matches!(key, KeyCode::Enter) => {
                    select_non_whitelist_model_pool(stdout, config)?
                }
                14 => {
                    config
                        .platforms
                        .qq
                        .private_chats
                        .friend_requests_require_private_whitelist = !config
                        .platforms
                        .qq
                        .private_chats
                        .friend_requests_require_private_whitelist
                }
                15 => {
                    config.platforms.qq.private_chats.allow_non_whitelist =
                        !config.platforms.qq.private_chats.allow_non_whitelist
                }
                16 if matches!(key, KeyCode::Enter) => {
                    edit_platform_rate_limit(
                        stdout,
                        &mut config.platforms.qq.private_chats.non_whitelist_rate_limit,
                    )?;
                }
                17 if matches!(key, KeyCode::Enter) => edit_qq_id_list(
                    stdout,
                    t(" GROUP WHITELIST ", " 群聊白名单 "),
                    t("Group id", "群号"),
                    &mut config.platforms.qq.group_chats.whitelist,
                )?,
                18 if matches!(key, KeyCode::Enter) => edit_keyword_list(
                    stdout,
                    &mut config.platforms.qq.group_chats.trigger_keywords,
                )?,
                19 if matches!(key, KeyCode::Enter) => {
                    edit_platform_rate_limit(
                        stdout,
                        &mut config.platforms.qq.group_chats.whitelist_rate_limit,
                    )?;
                }
                20 => {
                    config.platforms.qq.group_chats.allow_non_whitelist =
                        !config.platforms.qq.group_chats.allow_non_whitelist
                }
                21 if matches!(key, KeyCode::Enter) => {
                    edit_platform_rate_limit(
                        stdout,
                        &mut config.platforms.qq.group_chats.non_whitelist_rate_limit,
                    )?;
                }
                22 if matches!(key, KeyCode::Enter) => {
                    edit_platform_session_limits(stdout, &mut config.platforms.qq.session_limits)?
                }
                23 if matches!(key, KeyCode::Enter) => {
                    select_platform_model_routes(stdout, paths, config)?
                }
                24 if matches!(key, KeyCode::Enter) => {
                    select_platform_plugins(stdout, paths, config)?
                }
                25 if matches!(key, KeyCode::Enter) => edit_qq_advanced(stdout, config)?,
                _ => {}
            },
            _ => {}
        }
    }
}

pub(crate) fn session_limits_label(limits: PlatformSessionLimits) -> String {
    format!(
        "{} {} + {} {}",
        limits.running,
        t("running", "运行"),
        limits.queued,
        t("queued", "等待")
    )
}

pub(crate) fn edit_platform_session_limits(
    stdout: &mut io::Stdout,
    limits: &mut PlatformSessionLimits,
) -> Result<()> {
    let mut fields = vec![
        Field::new(
            t("Running turns", "并行运行数量"),
            limits.running.to_string(),
        ),
        Field::new(t("Queued turns", "等待队列数量"), limits.queued.to_string()),
    ];
    if !run_form_editing(
        stdout,
        t(" CONVERSATION CONCURRENCY ", " 会话并发 "),
        &mut fields,
    )? {
        return Ok(());
    }
    let running = fields[0].value.trim().parse::<usize>()?;
    let queued = fields[1].value.trim().parse::<usize>()?;
    if !(1..=MAX_PLATFORM_SESSION_RUNNING).contains(&running)
        || queued > MAX_PLATFORM_SESSION_QUEUED
    {
        message(
            stdout,
            t(
                "Concurrency values are outside the supported range.",
                "并发数值超出支持范围。",
            ),
        )?;
        return Ok(());
    }
    *limits = PlatformSessionLimits { running, queued };
    Ok(())
}

pub(crate) fn rate_limit_label(limit: PlatformRateLimit) -> String {
    if limit.max_messages == 0 {
        return t("unlimited", "不限").to_string();
    }
    format!(
        "{} / {} {}",
        limit.max_messages,
        limit.window_seconds,
        t("seconds", "秒")
    )
}

/// Both numbers live on one form, the way `edit_platform_session_limits`
/// already does it. The menu row above already renders "N / M 秒", so routing
/// Enter through a two-item submenu only restated that summary before letting
/// anyone type — two keypresses to reach a field that was never in doubt.
pub(crate) fn edit_platform_rate_limit(
    stdout: &mut io::Stdout,
    limit: &mut PlatformRateLimit,
) -> Result<()> {
    let mut fields = vec![
        Field::new(
            t(
                "Maximum messages (0 = unlimited)",
                "窗口内消息上限（0 = 不限）",
            ),
            limit.max_messages.to_string(),
        ),
        Field::new(
            t("Window seconds (1-86400)", "窗口秒数（1-86400）"),
            limit.window_seconds.to_string(),
        ),
    ];
    if !run_form_editing(stdout, t(" RATE LIMIT ", " 限流配置 "), &mut fields)? {
        return Ok(());
    }
    let (Ok(max_messages), Ok(window_seconds)) = (
        fields[0].value.trim().parse::<u32>(),
        fields[1].value.trim().parse::<u32>(),
    ) else {
        message(stdout, t("Invalid number.", "数值无效。"))?;
        return Ok(());
    };
    if !(1..=86_400).contains(&window_seconds) {
        message(
            stdout,
            t(
                "Window seconds must be between 1 and 86400.",
                "窗口秒数必须在 1 到 86400 之间。",
            ),
        )?;
        return Ok(());
    }
    *limit = PlatformRateLimit {
        max_messages,
        window_seconds,
    };
    Ok(())
}

pub(crate) fn edit_u16_value(
    stdout: &mut io::Stdout,
    label: &'static str,
    current: u16,
) -> Result<Option<u16>> {
    let mut fields = vec![Field::new(label, current.to_string())];
    if !run_form_editing(stdout, t(" EDIT VALUE ", " 编辑数值 "), &mut fields)? {
        return Ok(None);
    }
    match fields[0].value.trim().parse() {
        Ok(value) => Ok(Some(value)),
        Err(_) => {
            message(stdout, t("Invalid number.", "数值无效。"))?;
            Ok(None)
        }
    }
}

pub(crate) fn edit_qq_token(stdout: &mut io::Stdout, config: &mut AppConfig) -> Result<()> {
    if let Some(value) = edit_inline_value(
        stdout,
        t(" REVERSE WEBSOCKET TOKEN ", " 反向 WebSocket 验证 Token "),
        &config.platforms.qq.access_token,
        true,
    )? {
        config.platforms.qq.access_token = value.trim().to_string();
    }
    Ok(())
}

pub(crate) fn parse_positive_id(value: &str) -> std::result::Result<i64, String> {
    value
        .trim()
        .parse::<i64>()
        .ok()
        .filter(|id| *id > 0)
        .ok_or_else(|| {
            t(
                "QQ/group id must be a positive integer.",
                "QQ 号/群号必须是正整数。",
            )
            .to_string()
        })
}

pub(crate) fn parse_id_lines(value: &str) -> std::result::Result<Vec<i64>, String> {
    let mut parsed = Vec::new();
    for (index, line) in value.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let id = parse_positive_id(line)
            .map_err(|error| format!("{} {}: {error}", t("Line", "第"), index + 1))?;
        if !parsed.contains(&id) {
            parsed.push(id);
        }
    }
    Ok(parsed)
}

pub(crate) fn prompt_single_id(
    stdout: &mut io::Stdout,
    item_label: &str,
    current: Option<i64>,
) -> Result<Option<i64>> {
    let action = if current.is_some() {
        t("Edit", "编辑")
    } else {
        t("Add", "新增")
    };
    let title = format!(" {action} {item_label} ");
    let Some(value) = edit_inline_value(
        stdout,
        &title,
        &current.map(|id| id.to_string()).unwrap_or_default(),
        false,
    )?
    else {
        return Ok(None);
    };
    match parse_positive_id(&value) {
        Ok(id) => Ok(Some(id)),
        Err(error) => {
            message(stdout, &error)?;
            Ok(None)
        }
    }
}

pub(crate) fn edit_qq_id_list(
    stdout: &mut io::Stdout,
    title: &'static str,
    item_label: &'static str,
    ids: &mut Vec<i64>,
) -> Result<()> {
    let mut selected = 0usize;
    loop {
        let mut options = vec![
            t("+ Add one", "+ 新增一项").to_string(),
            t("+ Add multiple", "+ 批量新增").to_string(),
        ];
        options.extend(ids.iter().map(ToString::to_string));
        draw_menu(
            stdout,
            title,
            &options,
            selected,
            t(
                "[Enter]add/edit [Delete]remove [j/k]move [q]back",
                "[Enter]新增/编辑 [Delete]删除 [j/k]移动 [q]返回",
            ),
        )?;
        match read_key()? {
            KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
            KeyCode::Up | KeyCode::Char('k') => selected = selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => selected = (selected + 1).min(options.len() - 1),
            KeyCode::Enter if selected == 0 => {
                if let Some(id) = prompt_single_id(stdout, item_label, None)? {
                    if !ids.contains(&id) {
                        ids.push(id);
                    }
                }
            }
            KeyCode::Enter if selected == 1 => {
                let mut value = String::new();
                loop {
                    edit_textarea(stdout, &mut value)?;
                    match parse_id_lines(&value) {
                        Ok(additions) => {
                            for id in additions {
                                if !ids.contains(&id) {
                                    ids.push(id);
                                }
                            }
                            break;
                        }
                        Err(error) => message(stdout, &error.to_string())?,
                    }
                }
            }
            KeyCode::Enter => {
                let index = selected - 2;
                if let Some(id) = prompt_single_id(stdout, item_label, ids.get(index).copied())? {
                    if ids
                        .iter()
                        .enumerate()
                        .any(|(other, item)| other != index && *item == id)
                    {
                        message(stdout, t("That id already exists.", "该号码已存在。"))?;
                    } else if let Some(item) = ids.get_mut(index) {
                        *item = id;
                    }
                }
            }
            KeyCode::Delete | KeyCode::Backspace if selected >= 2 => {
                ids.remove(selected - 2);
                selected = selected.min(ids.len() + 1);
            }
            _ => {}
        }
    }
}
