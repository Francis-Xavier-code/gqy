//! fields2 — 自 src/config_tui/fields.rs 拆分。

use super::*;

pub(crate) fn parse_keyword_lines(value: &str) -> std::result::Result<Vec<String>, String> {
    let mut parsed = Vec::new();
    for (index, line) in value.lines().enumerate() {
        let keyword = line.trim();
        if keyword.is_empty() {
            continue;
        }
        if keyword.chars().count() > 128 || keyword.chars().any(char::is_control) {
            return Err(format!(
                "{} {}: {}",
                t("Line", "第"),
                index + 1,
                t("keyword is invalid or too long", "关键词无效或过长")
            ));
        }
        if !parsed.iter().any(|item| item == keyword) {
            parsed.push(keyword.to_string());
        }
    }
    Ok(parsed)
}

pub(crate) fn edit_keyword_list(stdout: &mut io::Stdout, keywords: &mut Vec<String>) -> Result<()> {
    let mut selected = 0usize;
    loop {
        let mut options = vec![
            t("+ Add one", "+ 新增一项").to_string(),
            t("+ Add multiple", "+ 批量新增").to_string(),
        ];
        options.extend(keywords.iter().cloned());
        draw_menu(
            stdout,
            t(" GROUP WAKE KEYWORDS ", " 群聊触发关键词 "),
            &options,
            selected,
            "",
        )?;
        match read_key()? {
            KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
            KeyCode::Up | KeyCode::Char('k') => selected = selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => selected = (selected + 1).min(options.len() - 1),
            KeyCode::Enter if selected == 0 => {
                if let Some(value) =
                    edit_inline_value(stdout, t(" ADD KEYWORD ", " 新增关键词 "), "", false)?
                {
                    match parse_keyword_lines(&value) {
                        Ok(additions) if additions.len() == 1 => {
                            let keyword = additions.into_iter().next().unwrap();
                            if !keywords.contains(&keyword) {
                                keywords.push(keyword);
                            }
                        }
                        _ => message(
                            stdout,
                            t("Enter exactly one valid keyword.", "请输入一个有效关键词。"),
                        )?,
                    }
                }
            }
            KeyCode::Enter if selected == 1 => {
                let mut value = String::new();
                loop {
                    edit_textarea(stdout, &mut value)?;
                    match parse_keyword_lines(&value) {
                        Ok(additions) => {
                            for keyword in additions {
                                if !keywords.contains(&keyword) {
                                    keywords.push(keyword);
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
                if let Some(value) = edit_inline_value(
                    stdout,
                    t(" EDIT KEYWORD ", " 编辑关键词 "),
                    &keywords[index],
                    false,
                )? {
                    match parse_keyword_lines(&value) {
                        Ok(values) if values.len() == 1 => {
                            let value = values[0].clone();
                            if keywords
                                .iter()
                                .enumerate()
                                .any(|(other, item)| other != index && item == &value)
                            {
                                message(
                                    stdout,
                                    t("That keyword already exists.", "该关键词已存在。"),
                                )?;
                            } else {
                                keywords[index] = value;
                            }
                        }
                        _ => message(
                            stdout,
                            t("Enter exactly one valid keyword.", "请输入一个有效关键词。"),
                        )?,
                    }
                }
            }
            KeyCode::Delete | KeyCode::Backspace if selected >= 2 => {
                keywords.remove(selected - 2);
                selected = selected.min(keywords.len() + 1);
            }
            _ => {}
        }
    }
}

pub(crate) fn edit_qq_advanced(stdout: &mut io::Stdout, config: &mut AppConfig) -> Result<()> {
    let qq = &config.platforms.qq;
    let mut fields = vec![
        Field::new(
            t(
                "Asset base URL (empty = automatic)",
                "文件访问基础 URL（空 = 自动推导）",
            ),
            qq.asset_base_url.clone(),
        ),
        Field::new(
            t(
                "Max reply chars per message (0 = no split)",
                "单条回复最大字数（0 = 不分段）",
            ),
            qq.max_reply_chars.to_string(),
        ),
        Field::new(
            t(
                "Group overflow (compact / pop)",
                "群聊上下文溢出策略（compact 摘要 / pop 丢弃最旧）",
            ),
            qq.group_context.on_overflow.clone(),
        ),
        Field::new(
            t(
                "Group trim batch (0-1, share released per trim)",
                "群聊单次丢弃比例（0-1，一次让出的窗口占比）",
            ),
            qq.group_context.trim_batch_ratio.to_string(),
        ),
    ];
    if run_form(stdout, t(" QQ ADVANCED ", " QQ 高级设置 "), &mut fields)? {
        config.platforms.qq.asset_base_url =
            fields[0].value.trim().trim_end_matches('/').to_string();
        let overflow = fields[2].value.trim().to_ascii_lowercase();
        if !matches!(overflow.as_str(), "compact" | "pop") {
            return Err(anyhow::anyhow!(t(
                "Group overflow must be compact or pop.",
                "群聊溢出策略只能是 compact 或 pop。"
            )));
        }
        let batch: f32 = fields[3].value.trim().parse().map_err(|_| {
            anyhow::anyhow!(t("Invalid group trim batch.", "群聊单次丢弃比例无效。"))
        })?;
        if !(0.0..1.0).contains(&batch) {
            return Err(anyhow::anyhow!(t(
                "Group trim batch must be between 0 and 1.",
                "群聊单次丢弃比例必须在 0 与 1 之间。"
            )));
        }
        config.platforms.qq.group_context.on_overflow = overflow;
        config.platforms.qq.group_context.trim_batch_ratio = batch;
        config.platforms.qq.max_reply_chars = fields[1].value.trim().parse().map_err(|_| {
            anyhow::anyhow!(t("Invalid maximum reply length.", "单条回复最大字数无效。"))
        })?;
    }
    Ok(())
}

pub(crate) fn select_platform_model_routes(
    stdout: &mut io::Stdout,
    paths: &GQYPaths,
    config: &mut AppConfig,
) -> Result<()> {
    let mut selected = 0usize;
    loop {
        let mut options = Vec::with_capacity(config.platforms.qq.conversations.len() + 1);
        options.push(t("+ Add conversation", "+ 新增会话配置").to_string());
        options.extend(
            config
                .platforms
                .qq
                .conversations
                .iter()
                .map(platform_model_route_label),
        );
        selected = selected.min(options.len().saturating_sub(1));
        draw_menu(
            stdout,
            t(" QQ CONVERSATIONS ", " 私聊/群聊专属配置 "),
            &options,
            selected,
            t(
                "[Enter]add/edit [d]delete [j/k]move [q]back",
                "[Enter]新增/编辑 [d]删除 [j/k]移动 [q]返回",
            ),
        )?;
        match read_key()? {
            KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
            KeyCode::Up | KeyCode::Char('k') => selected = selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                selected = (selected + 1).min(options.len().saturating_sub(1));
            }
            KeyCode::Enter if selected == 0 => {
                edit_platform_model_route(stdout, paths, config, None)?
            }
            KeyCode::Enter => edit_platform_model_route(stdout, paths, config, Some(selected - 1))?,
            KeyCode::Char('d') | KeyCode::Delete if selected > 0 => {
                config.platforms.qq.conversations.remove(selected - 1);
                selected = selected.min(config.platforms.qq.conversations.len());
            }
            _ => {}
        }
    }
}

pub(crate) fn platform_model_route_label(route: &PlatformModelRoute) -> String {
    let kind = match route.conversation.kind {
        PlatformConversationKind::Private => t("private", "私聊"),
        PlatformConversationKind::Group => t("group", "群聊"),
    };
    let text = route_pool_summary(route.text_models.as_deref(), route.text_models_inheritance);
    let multimodal = route_pool_summary(
        route.multimodal_models.as_deref(),
        route.multimodal_models_inheritance,
    );
    let prompt = if route.extra_prompt.is_empty() {
        t("none", "无")
    } else {
        t("set", "已设置")
    };
    let persona = platform_persona_summary(&route.persona);
    format!(
        "{kind} {} · {}:{persona} · {}:{text} {}:{multimodal} · {}:{prompt}",
        route.conversation.id,
        t("persona", "人格"),
        t("text", "文本"),
        t("media", "多模态"),
        t("prompt", "提示词")
    )
}

pub(crate) fn edit_platform_model_route(
    stdout: &mut io::Stdout,
    paths: &GQYPaths,
    config: &mut AppConfig,
    route_index: Option<usize>,
) -> Result<()> {
    let mut route = route_index
        .and_then(|index| config.platforms.qq.conversations.get(index).cloned())
        .unwrap_or_else(|| PlatformModelRoute {
            conversation: PlatformConversationConfig {
                kind: PlatformConversationKind::Private,
                id: String::new(),
            },
            persona: PlatformPersonaOverride::Inherit,
            text_models_inheritance: PlatformModelPoolInheritance::Platform,
            text_models: None,
            multimodal_models_inheritance: PlatformModelPoolInheritance::Platform,
            multimodal_models: None,
            extra_prompt: String::new(),
            session_limits: None,
        });
    let mut selected = 0usize;
    loop {
        let kind_label = platform_conversation_kind_label(route.conversation.kind);
        let id_label = platform_conversation_id_label(route.conversation.kind);
        let options = [
            format!("{}: {}", t("Conversation type", "会话类型"), kind_label,),
            format!(
                "{id_label}: {}",
                if route.conversation.id.is_empty() {
                    t("not set", "未设置")
                } else {
                    route.conversation.id.as_str()
                },
            ),
            format!(
                "{}: {}",
                t("Override AI persona", "覆盖 AI 人格"),
                platform_persona_summary(&route.persona)
            ),
            format!(
                "{}: {}",
                t("Text model pool", "文本模型池"),
                route_pool_summary(route.text_models.as_deref(), route.text_models_inheritance)
            ),
            format!(
                "{}: {}",
                t("Multimodal model pool", "多模态模型池"),
                route_pool_summary(
                    route.multimodal_models.as_deref(),
                    route.multimodal_models_inheritance,
                )
            ),
            format!(
                "{}: {}",
                t("Extra prompt", "额外提示词"),
                if route.extra_prompt.is_empty() {
                    t("none", "未设置")
                } else {
                    t("set", "已设置")
                }
            ),
            format!(
                "{}: {}",
                t("Override concurrency settings", "覆盖并发配置"),
                route
                    .session_limits
                    .map(session_limits_label)
                    .unwrap_or_else(|| t("inherit", "继承").to_string())
            ),
        ];
        draw_menu(
            stdout,
            t(" EDIT QQ CONVERSATION ", " 编辑 QQ 会话配置 "),
            &options,
            selected,
            "",
        )?;
        match read_key()? {
            KeyCode::Char('q') | KeyCode::Esc => {
                route.normalize();
                if let Err(error) = config.validate_platform_model_route(&route) {
                    if route_index.is_none() {
                        return Ok(());
                    }
                    message(stdout, &error.to_string())?;
                    continue;
                }
                if config
                    .platforms
                    .qq
                    .conversations
                    .iter()
                    .enumerate()
                    .any(|(index, existing)| {
                        Some(index) != route_index && existing.identity() == route.identity()
                    })
                {
                    message(
                        stdout,
                        t(
                            "A configuration for this QQ conversation already exists.",
                            "该 QQ 会话的配置已存在。",
                        ),
                    )?;
                    continue;
                }
                match route_index {
                    Some(index) => config.platforms.qq.conversations[index] = route,
                    None => config.platforms.upsert_model_route(route),
                }
                return Ok(());
            }
            KeyCode::Up | KeyCode::Char('k') => selected = selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => selected = (selected + 1).min(options.len() - 1),
            KeyCode::Enter => match selected {
                0 => select_platform_conversation_kind(stdout, &mut route.conversation.kind)?,
                1 => {
                    let title = format!(" {id_label} ");
                    if let Some(value) =
                        edit_inline_value(stdout, &title, &route.conversation.id, false)?
                    {
                        route.conversation.id = value.trim().to_string();
                    }
                }
                2 => edit_platform_personas(stdout, paths, config, &mut route.persona)?,
                3 => select_platform_route_models(
                    stdout,
                    config,
                    &mut route.text_models,
                    &mut route.text_models_inheritance,
                    false,
                )?,
                4 => select_platform_route_models(
                    stdout,
                    config,
                    &mut route.multimodal_models,
                    &mut route.multimodal_models_inheritance,
                    true,
                )?,
                5 => edit_conversation_extra_prompt(stdout, &mut route.extra_prompt)?,
                6 => {
                    let enabled = select_bool(
                        stdout,
                        t("Override QQ concurrency", "覆盖 QQ 并发配置"),
                        route.session_limits.is_some(),
                    )?;
                    if enabled {
                        let limits = route
                            .session_limits
                            .get_or_insert(config.platforms.qq.session_limits);
                        edit_platform_session_limits(stdout, limits)?;
                    } else {
                        route.session_limits = None;
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }
}
