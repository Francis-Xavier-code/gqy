//! menus — 自 src/config_tui.rs 拆分。

use super::*;

pub(crate) fn edit_real_context_affection(
    stdout: &mut io::Stdout,
    _config: &AppConfig,
    settings: &mut RealContextPluginSettings,
) -> Result<()> {
    let mut selected = 0usize;
    loop {
        let options = [
            format!(
                "{}: {}",
                t("Affection system", "好感度系统"),
                boolean_label(settings.affection_enable)
            ),
            format!(
                "{}: {}",
                t(
                    "Judge affection changes after replies",
                    "回复后判断好感度变化",
                ),
                boolean_label(settings.affection_update_enable)
            ),
            t("Score and limits", "分值与限制").to_string(),
            t("Relationship prompts", "关系提示词").to_string(),
            format!(
                "{}: {}",
                t("Top-tier QQ IDs", "允许到达最高挡位的 QQ 号"),
                settings.affection_unlimited_user_ids.len()
            ),
        ];
        draw_menu(
            stdout,
            t(" AFFECTION AND RELATIONSHIP ", " 好感度与关系 "),
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
                    settings.affection_enable = select_bool(
                        stdout,
                        t("Affection system", "好感度系统"),
                        settings.affection_enable,
                    )?;
                }
                1 => {
                    settings.affection_update_enable = select_bool(
                        stdout,
                        t(
                            "Judge affection changes after replies",
                            "回复后判断好感度变化",
                        ),
                        settings.affection_update_enable,
                    )?;
                }
                2 => edit_real_context_affection_values(stdout, settings)?,
                3 => edit_real_context_affection_prompts(stdout, settings)?,
                4 => {
                    let mut raw = settings
                        .affection_unlimited_user_ids
                        .iter()
                        .map(i64::to_string)
                        .collect::<Vec<_>>()
                        .join("\n");
                    edit_textarea(stdout, &mut raw)?;
                    match parse_id_list(&raw) {
                        Ok(ids) => settings.affection_unlimited_user_ids = ids,
                        Err(error) => message(stdout, &error.to_string())?,
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }
}

pub(crate) fn edit_real_context_affection_values(
    stdout: &mut io::Stdout,
    settings: &mut RealContextPluginSettings,
) -> Result<()> {
    loop {
        let mut fields = vec![
            Field::new(
                t("Initial score", "首次互动默认好感度"),
                settings.affection_initial_score.to_string(),
            ),
            Field::new(
                t("Minimum score", "好感度下限"),
                settings.affection_min_score.to_string(),
            ),
            Field::new(
                t("Global maximum score", "全局最高好感度"),
                settings.affection_max_score.to_string(),
            ),
            Field::new(
                t("Regular-user maximum", "普通用户最高好感度"),
                settings.affection_regular_max_score.to_string(),
            ),
            Field::new(
                t("Reply bias minimum", "主动回复最低加值"),
                settings.affection_bias_min.to_string(),
            ),
            Field::new(
                t("Reply bias maximum", "主动回复最高加值"),
                settings.affection_bias_max.to_string(),
            ),
            Field::new(
                t("Gain pivot", "好感增益拐点"),
                settings.affection_gain_pivot.to_string(),
            ),
            Field::new(
                t("Delta scale", "好感变化倍率"),
                settings.affection_delta_scale.to_string(),
            ),
            Field::new(
                t("Single-change minimum", "单次变化下限"),
                settings.affection_delta_min.to_string(),
            ),
            Field::new(
                t("Single-change maximum", "单次变化上限"),
                settings.affection_delta_max.to_string(),
            ),
            Field::new(
                t("Confidence threshold", "变化置信度阈值"),
                settings.affection_update_confidence_threshold.to_string(),
            ),
            Field::new(
                t(
                    "Daily gain limit (0 = unlimited)",
                    "单日正向上限（0 = 不限）",
                ),
                settings.affection_daily_gain_limit.to_string(),
            ),
            Field::new(
                t(
                    "Daily loss limit (0 = unlimited)",
                    "单日负向上限（0 = 不限）",
                ),
                settings.affection_daily_loss_limit.to_string(),
            ),
            Field::boolean(
                t("Automatic tags", "自动标签"),
                settings.affection_auto_tag_enable,
            ),
            Field::new(
                t("Maximum tags (0 = unlimited)", "标签上限（0 = 不限）"),
                settings.affection_max_tags.to_string(),
            ),
            Field::new(
                t("Recent events in prompt", "注入提示词的近期变化条数"),
                settings.affection_recent_events_for_prompt.to_string(),
            ),
            Field::new(
                t(
                    "Update timeout (seconds; 0 = unlimited)",
                    "更新超时（秒；0 = 不限）",
                ),
                settings.affection_update_timeout_seconds.to_string(),
            ),
        ];
        if !run_form(
            stdout,
            t(" AFFECTION SCORE AND LIMITS ", " 好感度分值与限制 "),
            &mut fields,
        )? {
            return Ok(());
        }
        let mut candidate = settings.clone();
        let parsed = (|| -> std::result::Result<(), String> {
            candidate.affection_initial_score = real_context_value(&fields, 0)?;
            candidate.affection_min_score = real_context_value(&fields, 1)?;
            candidate.affection_max_score = real_context_value(&fields, 2)?;
            candidate.affection_regular_max_score = real_context_value(&fields, 3)?;
            candidate.affection_bias_min = real_context_value(&fields, 4)?;
            candidate.affection_bias_max = real_context_value(&fields, 5)?;
            candidate.affection_gain_pivot = real_context_value(&fields, 6)?;
            candidate.affection_delta_scale = real_context_value(&fields, 7)?;
            candidate.affection_delta_min = real_context_value(&fields, 8)?;
            candidate.affection_delta_max = real_context_value(&fields, 9)?;
            candidate.affection_update_confidence_threshold = real_context_value(&fields, 10)?;
            candidate.affection_daily_gain_limit = real_context_value(&fields, 11)?;
            candidate.affection_daily_loss_limit = real_context_value(&fields, 12)?;
            candidate.affection_auto_tag_enable = real_context_bool(&fields, 13)?;
            candidate.affection_max_tags = real_context_value(&fields, 14)?;
            candidate.affection_recent_events_for_prompt = real_context_value(&fields, 15)?;
            candidate.affection_update_timeout_seconds = real_context_value(&fields, 16)?;
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

pub(crate) fn edit_real_context_affection_prompts(
    stdout: &mut io::Stdout,
    settings: &mut RealContextPluginSettings,
) -> Result<()> {
    let prompts = [
        (
            t("Estranged", "刻意疏远"),
            &mut settings.affection_prompt_estranged,
        ),
        (t("Cold", "冷漠"), &mut settings.affection_prompt_cold),
        (t("Neutral", "中立"), &mut settings.affection_prompt_neutral),
        (t("Known", "认识"), &mut settings.affection_prompt_known),
        (t("Friend", "好友"), &mut settings.affection_prompt_friend),
        (t("Trusted", "信任"), &mut settings.affection_prompt_trusted),
        (t("Close", "亲近"), &mut settings.affection_prompt_close),
    ];
    let mut selected = 0usize;
    loop {
        let options = prompts
            .iter()
            .map(|(label, value)| {
                format!(
                    "{label}: {}",
                    if value.is_empty() {
                        t("unset", "未设置")
                    } else {
                        t("set", "已设置")
                    }
                )
            })
            .collect::<Vec<_>>();
        draw_menu(
            stdout,
            t(" AFFECTION RELATIONSHIP PROMPTS ", " 好感度关系提示词 "),
            &options,
            selected,
            "",
        )?;
        match read_key()? {
            KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
            KeyCode::Up | KeyCode::Char('k') => selected = selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => selected = (selected + 1).min(options.len() - 1),
            KeyCode::Enter => edit_textarea(stdout, prompts[selected].1)?,
            _ => {}
        }
    }
}

pub(crate) fn edit_real_context_identities(
    stdout: &mut io::Stdout,
    settings: &mut RealContextPluginSettings,
) -> Result<()> {
    let mut selected = 0usize;
    loop {
        let mut options = vec![
            t("+ Add one", "+ 新增一项").to_string(),
            t("+ Add multiple", "+ 批量新增").to_string(),
        ];
        options.extend(
            settings
                .identity_mappings
                .iter()
                .map(|mapping| format!("{} -> {}", mapping.nickname, mapping.user_id)),
        );
        selected = selected.min(options.len() - 1);
        draw_menu(
            stdout,
            t(" IDENTITY MAPPINGS ", " 识人映射 "),
            &options,
            selected,
            t(
                "[Enter]configure [Delete]remove [j/k]move [q]back",
                "[Enter]配置 [Delete]删除 [j/k]移动 [q]返回",
            ),
        )?;
        match read_key()? {
            KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
            KeyCode::Up | KeyCode::Char('k') => selected = selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => selected = (selected + 1).min(options.len() - 1),
            KeyCode::Enter if selected == 0 => {
                if let Some(mapping) = prompt_real_context_identity(stdout, None)? {
                    upsert_real_context_identity(&mut settings.identity_mappings, mapping);
                }
            }
            KeyCode::Enter if selected == 1 => {
                let mut raw = format!(
                    "# {}",
                    t(
                        "one per line: nickname<Tab>QQ-id",
                        "每行一项：昵称<Tab>QQ号"
                    )
                );
                edit_textarea(stdout, &mut raw)?;
                match parse_real_context_identity_lines(&raw) {
                    Ok(mappings) => {
                        for mapping in mappings {
                            upsert_real_context_identity(&mut settings.identity_mappings, mapping);
                        }
                    }
                    Err(error) => message(stdout, &error)?,
                }
            }
            KeyCode::Enter => {
                let index = selected - 2;
                if let Some(mapping) = prompt_real_context_identity(
                    stdout,
                    settings.identity_mappings.get(index).cloned(),
                )? {
                    if settings
                        .identity_mappings
                        .iter()
                        .enumerate()
                        .any(|(other, item)| other != index && item.nickname == mapping.nickname)
                    {
                        message(stdout, t("That nickname already exists.", "该昵称已存在。"))?;
                    } else if let Some(item) = settings.identity_mappings.get_mut(index) {
                        *item = mapping;
                    }
                }
            }
            KeyCode::Delete | KeyCode::Backspace if selected >= 2 => {
                settings.identity_mappings.remove(selected - 2);
                selected = selected.min(settings.identity_mappings.len() + 1);
            }
            _ => {}
        }
    }
}

pub(crate) fn prompt_real_context_identity(
    stdout: &mut io::Stdout,
    current: Option<RealContextIdentityMapping>,
) -> Result<Option<RealContextIdentityMapping>> {
    let mut fields = vec![
        Field::new(
            t("Protected nickname", "受保护昵称"),
            current
                .as_ref()
                .map(|mapping| mapping.nickname.clone())
                .unwrap_or_default(),
        ),
        Field::new(
            t("Expected QQ id", "对应 QQ 号"),
            current
                .as_ref()
                .map(|mapping| mapping.user_id.to_string())
                .unwrap_or_default(),
        ),
    ];
    if !run_form(
        stdout,
        t(" IDENTITY MAPPING ", " 编辑识人映射 "),
        &mut fields,
    )? {
        return Ok(None);
    }
    let nickname = fields[0].value.trim();
    if nickname.is_empty()
        || nickname.chars().count() > 128
        || nickname.chars().any(char::is_control)
    {
        message(
            stdout,
            t(
                "Nickname must be 1-128 characters without control characters.",
                "昵称必须为 1 到 128 个字符，且不能包含控制字符。",
            ),
        )?;
        return Ok(None);
    }
    let user_id = match parse_positive_id(&fields[1].value) {
        Ok(user_id) => user_id,
        Err(error) => {
            message(stdout, &error)?;
            return Ok(None);
        }
    };
    Ok(Some(RealContextIdentityMapping {
        nickname: nickname.to_string(),
        user_id,
    }))
}

pub(crate) fn upsert_real_context_identity(
    mappings: &mut Vec<RealContextIdentityMapping>,
    mapping: RealContextIdentityMapping,
) {
    if let Some(existing) = mappings
        .iter_mut()
        .find(|existing| existing.nickname == mapping.nickname)
    {
        *existing = mapping;
    } else {
        mappings.push(mapping);
    }
}

pub(crate) fn parse_real_context_identity_lines(
    raw: &str,
) -> std::result::Result<Vec<RealContextIdentityMapping>, String> {
    let mut mappings = Vec::new();
    for (index, line) in raw.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((nickname, user_id)) = line.rsplit_once('\t').or_else(|| line.rsplit_once('='))
        else {
            return Err(format!(
                "{} {}: {}",
                t("Line", "第"),
                index + 1,
                t("use nickname<Tab>QQ-id", "请使用 昵称<Tab>QQ号 格式")
            ));
        };
        let nickname = nickname.trim();
        if nickname.is_empty()
            || nickname.chars().count() > 128
            || nickname.chars().any(char::is_control)
        {
            return Err(format!(
                "{} {}: {}",
                t("Line", "第"),
                index + 1,
                t("invalid nickname", "昵称无效")
            ));
        }
        let user_id = parse_positive_id(user_id)?;
        if mappings
            .iter()
            .any(|mapping: &RealContextIdentityMapping| mapping.nickname == nickname)
        {
            return Err(format!(
                "{} {}: {}",
                t("Line", "第"),
                index + 1,
                t("duplicate nickname", "昵称重复")
            ));
        }
        mappings.push(RealContextIdentityMapping {
            nickname: nickname.to_string(),
            user_id,
        });
    }
    Ok(mappings)
}

pub(crate) fn edit_real_context_string_lines(
    stdout: &mut io::Stdout,
    _title: &'static str,
    values: &mut Vec<String>,
    maximum_chars: usize,
) -> Result<()> {
    let mut raw = values.join("\n");
    edit_textarea(stdout, &mut raw)?;
    match parse_real_context_string_lines(&raw, maximum_chars) {
        Ok(parsed) => *values = parsed,
        Err(error) => message(stdout, &error)?,
    }
    Ok(())
}

pub(crate) fn parse_real_context_string_lines(
    raw: &str,
    maximum_chars: usize,
) -> std::result::Result<Vec<String>, String> {
    let mut values = Vec::new();
    for (index, line) in raw.lines().enumerate() {
        let value = line.trim();
        if value.is_empty() {
            continue;
        }
        if value.chars().count() > maximum_chars || value.chars().any(char::is_control) {
            return Err(format!(
                "{} {}: {}",
                t("Line", "第"),
                index + 1,
                t("value is invalid or too long", "内容无效或过长")
            ));
        }
        if !values.iter().any(|existing| existing == value) {
            values.push(value.to_string());
        }
    }
    Ok(values)
}

pub(crate) fn real_context_bool(
    fields: &[Field],
    index: usize,
) -> std::result::Result<bool, String> {
    parse_bool_field(&fields[index].value).map_err(|error| error.to_string())
}

pub(crate) fn real_context_value<T>(
    fields: &[Field],
    index: usize,
) -> std::result::Result<T, String>
where
    T: std::str::FromStr,
{
    fields[index]
        .value
        .trim()
        .parse()
        .map_err(|_| t("Invalid value.", "数值无效。").to_string())
}

pub(crate) fn edit_real_context_number<T>(
    stdout: &mut io::Stdout,
    label: &'static str,
    current: T,
    settings: &mut RealContextPluginSettings,
    assign: impl Fn(&mut RealContextPluginSettings, T),
) -> Result<()>
where
    T: Copy + ToString + std::str::FromStr,
{
    loop {
        let Some(raw) = edit_inline_value(stdout, label, &current.to_string(), false)? else {
            return Ok(());
        };
        let value = match raw.trim().parse() {
            Ok(value) => value,
            Err(_) => {
                message(stdout, t("Invalid value.", "数值无效。"))?;
                continue;
            }
        };
        let mut candidate = settings.clone();
        assign(&mut candidate, value);
        match candidate.validate() {
            Ok(()) => {
                *settings = candidate;
                return Ok(());
            }
            Err(error) => message(stdout, &error.to_string())?,
        }
    }
}

pub(crate) fn real_context_media_mode_label(value: &str) -> &'static str {
    match value {
        "off" => t("Off", "不记录"),
        "metadata" => t("Metadata", "保留元数据"),
        _ => t("Placeholder", "仅占位"),
    }
}

pub(crate) fn real_context_media_mode_value(value: &str) -> Option<&'static str> {
    match value.trim() {
        "off" | "Off" | "不记录" => Some("off"),
        "placeholder" | "Placeholder" | "仅占位" => Some("placeholder"),
        "metadata" | "Metadata" | "保留元数据" => Some("metadata"),
        _ => None,
    }
}

pub(crate) fn real_context_restraint_label(value: &str) -> &'static str {
    match value {
        "light" => t("Light", "轻度"),
        "strong" => t("Strong", "强烈"),
        _ => t("Medium", "中度"),
    }
}

pub(crate) fn real_context_restraint_value(value: &str) -> Option<&'static str> {
    match value.trim() {
        "light" | "Light" | "轻度" => Some("light"),
        "medium" | "Medium" | "中度" => Some("medium"),
        "strong" | "Strong" | "强烈" => Some("strong"),
        _ => None,
    }
}

pub(crate) fn real_context_model_pool_summary(
    pool: Option<&[ActiveProviderModelConfig]>,
) -> String {
    match pool {
        None | Some([]) => t("inherit platform", "继承平台池").to_string(),
        Some(entries) => route_pool_summary(Some(entries), PlatformModelPoolInheritance::Platform),
    }
}

pub(crate) fn select_real_context_model_pool(
    stdout: &mut io::Stdout,
    config: &AppConfig,
    pool: &mut Option<Vec<ActiveProviderModelConfig>>,
) -> Result<()> {
    select_model_pool(
        stdout,
        config.text_provider_model_choices(),
        pool,
        false,
        t(" REAL-CONTEXT TEXT MODELS ", " 真实上下文文本模型 "),
        t("Inherit QQ platform model pool", "继承 QQ 平台模型池"),
    )
}

pub(crate) fn reply_processor_values(
    config: &AppConfig,
) -> Result<(bool, ReplyProcessorSettingsForm)> {
    let Some(instance) = config.platforms.qq.plugins.get(REPLY_PROCESSOR_PLUGIN_ID) else {
        return Ok((true, ReplyProcessorSettingsForm::default()));
    };
    let settings = serde_json::from_value(serde_json::Value::Object(instance.settings.clone()))?;
    Ok((instance.enabled_or(true), settings))
}

pub(crate) fn apply_reply_processor_values(
    config: &mut AppConfig,
    enabled: bool,
    settings: &ReplyProcessorSettingsForm,
) -> Result<()> {
    let serialized = serde_json::to_value(settings)?;
    let serde_json::Value::Object(known_settings) = serialized else {
        bail!("reply processor settings must serialize as an object");
    };
    let instance = config
        .platforms
        .qq
        .plugins
        .entry(REPLY_PROCESSOR_PLUGIN_ID.to_string())
        .or_default();
    instance.enabled = (!enabled).then_some(false);
    for (key, value) in known_settings {
        instance.settings.insert(key, value);
    }
    Ok(())
}

pub(crate) fn edit_reply_processor(stdout: &mut io::Stdout, config: &mut AppConfig) -> Result<()> {
    let (mut plugin_enabled, mut settings) = reply_processor_values(config)?;
    loop {
        let mode_choices = vec![
            reply_processor_mode_label("image"),
            reply_processor_mode_label("forward"),
        ];
        let mut fields = vec![
            Field::boolean(t("Plugin enabled", "启用插件"), plugin_enabled),
            Field::boolean(
                t("Enabled for new conversations", "新会话默认启用"),
                settings.default_enabled,
            ),
            Field::new(
                t("Long reply threshold (characters)", "长回复阈值（字符）"),
                settings.threshold.to_string(),
            ),
            Field::new(
                t("Long reply processing mode", "长回复处理模式"),
                reply_processor_mode_label(&settings.mode),
            )
            .choices_owned(mode_choices)
            .raw_choice_labels(),
            Field::boolean(
                t("Mention sender after forwarding", "转发后艾特发起者"),
                settings.followup_mention,
            ),
            Field::boolean(
                t("Strip trailing Chinese period", "移除末尾中文句号"),
                settings.strip_period,
            ),
            Field::new(t("Image theme", "长图主题"), settings.theme.clone())
                .choices(&["paper", "light", "dark"]),
            Field::new(
                t("Image maximum height", "长图最大高度"),
                settings.max_height.to_string(),
            ),
            Field::new(
                t("Body font size", "正文字号"),
                settings.font_size.to_string(),
            ),
            Field::new(
                t("Code font size", "代码字号"),
                settings.code_font_size.to_string(),
            ),
            Field::new(t("Image padding", "长图边距"), settings.padding.to_string()),
            Field::boolean(
                t("Add image context notice", "注入长图上下文提示"),
                settings.context_notice,
            ),
            Field::new(
                t("Context notice TTL (hours)", "上下文提示保留小时"),
                settings.ttl_hours.to_string(),
            ),
            Field::new(
                t("Maximum context records", "上下文提示最大条数"),
                settings.max_records.to_string(),
            ),
            Field::boolean(
                t("Intercept send-message tool", "接管发送消息工具"),
                settings.send_tool_intercept,
            ),
            Field::new(
                t(
                    "Body font file path (empty = bundled default)",
                    "正文字体文件路径（空 = 内置默认字体）",
                ),
                settings.font.clone(),
            ),
            Field::new(
                t(
                    "Title font file path (empty = body font)",
                    "标题字体文件路径（空 = 跟随正文字体）",
                ),
                settings.title_font.clone(),
            ),
            Field::new(
                t(
                    "Code font file path (empty = bundled default)",
                    "代码字体文件路径（空 = 内置默认字体）",
                ),
                settings.code_font.clone(),
            ),
            Field::new(
                t(
                    "Emoji font file path (empty = bundled default)",
                    "Emoji 字体文件路径（空 = 内置默认字体）",
                ),
                settings.emoji_font.clone(),
            ),
        ];
        run_form_without_buttons(stdout, t(" REPLY PROCESSOR ", " 回复处理 "), &mut fields)?;
        plugin_enabled = parse_bool_field(&fields[0].value)?;
        settings = match parse_reply_processor_fields(&fields) {
            Ok(settings) => settings,
            Err(error) => {
                message(stdout, &error)?;
                continue;
            }
        };
        apply_reply_processor_values(config, plugin_enabled, &settings)?;
        return Ok(());
    }
}

pub(crate) fn parse_reply_processor_fields(
    fields: &[Field],
) -> std::result::Result<ReplyProcessorSettingsForm, String> {
    let bool_at =
        |index: usize| parse_bool_field(&fields[index].value).map_err(|error| error.to_string());
    let mode = reply_processor_mode_value(&fields[3].value)
        .map(str::to_string)
        .unwrap_or_else(|| fields[3].value.trim().to_string());
    let settings = ReplyProcessorSettingsForm {
        default_enabled: bool_at(1)?,
        threshold: parse_reply_processor_value(fields, 2, t("threshold", "阈值"))?,
        mode,
        followup_mention: bool_at(4)?,
        strip_period: bool_at(5)?,
        theme: fields[6].value.trim().to_string(),
        max_height: parse_reply_processor_value(fields, 7, t("maximum height", "最大高度"))?,
        font_size: parse_reply_processor_value(fields, 8, t("font size", "字号"))?,
        code_font_size: parse_reply_processor_value(fields, 9, t("code font size", "代码字号"))?,
        padding: parse_reply_processor_value(fields, 10, t("padding", "边距"))?,
        context_notice: bool_at(11)?,
        ttl_hours: parse_reply_processor_value(fields, 12, "TTL")?,
        max_records: parse_reply_processor_value(fields, 13, t("maximum records", "最大条数"))?,
        send_tool_intercept: bool_at(14)?,
        font: fields[15].value.trim().to_string(),
        title_font: fields[16].value.trim().to_string(),
        code_font: fields[17].value.trim().to_string(),
        emoji_font: fields[18].value.trim().to_string(),
    };
    validate_reply_processor_settings(&settings)?;
    Ok(settings)
}

pub(crate) fn reply_processor_mode_label(value: &str) -> String {
    match value.trim() {
        "image" => t("Convert to image", "转图片"),
        "forward" => t("Merged forward", "合并转发"),
        value => value,
    }
    .to_string()
}

pub(crate) fn reply_processor_mode_value(value: &str) -> Option<&'static str> {
    match value.trim() {
        "image" | "Convert to image" | "转图片" => Some("image"),
        "forward" | "Merged forward" | "合并转发" => Some("forward"),
        _ => None,
    }
}

pub(crate) fn parse_reply_processor_value<T>(
    fields: &[Field],
    index: usize,
    label: &str,
) -> std::result::Result<T, String>
where
    T: std::str::FromStr,
{
    fields[index]
        .value
        .trim()
        .parse()
        .map_err(|_| format!("{}: {label}", t("Invalid value", "无效值")))
}

pub(crate) fn validate_reply_processor_settings(
    settings: &ReplyProcessorSettingsForm,
) -> std::result::Result<(), String> {
    if settings.threshold == 0 || settings.threshold > 100_000 {
        return Err(t(
            "Threshold must be between 1 and 100000.",
            "阈值必须在 1 到 100000 之间。",
        )
        .to_string());
    }
    if !matches!(settings.mode.as_str(), "image" | "forward") {
        return Err(t(
            "Mode must be Convert to image or Merged forward.",
            "模式必须是转图片或合并转发。",
        )
        .to_string());
    }
    if !matches!(settings.theme.as_str(), "paper" | "light" | "dark") {
        return Err(t(
            "Theme must be paper, light, or dark.",
            "主题必须是 paper、light 或 dark。",
        )
        .to_string());
    }
    if !(1000..=5000).contains(&settings.max_height) {
        return Err(t(
            "Image maximum height must be between 1000 and 5000.",
            "长图最大高度必须在 1000 到 5000 之间。",
        )
        .to_string());
    }
    if !(24..=56).contains(&settings.font_size) || !(20..=46).contains(&settings.code_font_size) {
        return Err(t(
            "Body font size must be 24-56 and code font size must be 20-46.",
            "正文字号必须为 24-56，代码字号必须为 20-46。",
        )
        .to_string());
    }
    if !(36..=120).contains(&settings.padding) {
        return Err(t(
            "Image padding must be between 36 and 120.",
            "长图边距必须在 36 到 120 之间。",
        )
        .to_string());
    }
    if !(1..=168).contains(&settings.ttl_hours) || !(1..=10).contains(&settings.max_records) {
        return Err(t(
            "Context TTL must be 1-168 hours and maximum records must be 1-10.",
            "上下文保留时间必须为 1-168 小时，最大条数必须为 1-10。",
        )
        .to_string());
    }
    Ok(())
}

pub(crate) fn format_id_list(ids: &[i64]) -> String {
    ids.iter()
        .map(i64::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

pub(crate) fn parse_id_list(value: &str) -> Result<Vec<i64>> {
    value
        .split([',', ' ', '\u{3000}', ';', '\n', '\r', '\t'])
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(|item| {
            let id = item.parse::<i64>().map_err(|_| {
                anyhow::anyhow!(t("invalid id: {}", "无效的号码：{}").replace("{}", item))
            })?;
            if id <= 0 {
                bail!(t(
                    "QQ and group ids must be positive",
                    "QQ 号和群号必须为正数"
                ));
            }
            Ok(id)
        })
        .collect()
}

pub(crate) fn edit_provider_form(
    stdout: &mut io::Stdout,
    provider: ProviderConfig,
) -> Result<Option<ProviderConfig>> {
    // 将 extra_body 格式化为 JSON 字符串，方便编辑
    let extra_body_string = provider
        .extra_body
        .as_ref()
        .and_then(|v| serde_json::to_string_pretty(v).ok())
        .unwrap_or_default();

    let mut fields = vec![
        Field::new(t("Configuration ID", "配置 ID"), provider.id.clone()),
        Field::new(t("Display name", "显示名称"), provider.display_name.clone()),
        Field::new("Base URL", provider.base_url.clone()),
        Field::new(t("Protocol", "协议"), provider.protocol.clone()).choices(&[
            "auto",
            "openai-chat",
            "openai-responses",
            "anthropic",
        ]),
        Field::new(
            t("API Key or $env:NAME", "API Key 或 $env:NAME"),
            provider.api_key.clone().unwrap_or_default(),
        )
        .sensitive(),
        Field::new(
            t("Current model", "当前模型"),
            provider.default_model.clone(),
        ),
        Field::new(
            t("Timeout (seconds)", "超时秒数"),
            provider.timeout_seconds.to_string(),
        ),
        Field::textarea(
            t("Extra request body (JSON)", "额外请求体 (JSON)"),
            extra_body_string,
        ),
    ];

    // 循环直到用户取消或输入合法 JSON 对象
    loop {
        if !run_form(stdout, t(" EDIT PROVIDER ", " 编辑供应商 "), &mut fields)? {
            return Ok(None);
        }

        // 温度与上下文窗口都是按模型的事,归模型菜单管;供应商表单
        // 不再放这两项(验收:曾牵连全部模型)。
        let default_model = fields[5].value.trim().to_string();
        let timeout = fields[6].value.trim().parse().unwrap_or(60);

        let extra_body = match parse_extra_body(&fields[7].value) {
            Ok(extra_body) => extra_body,
            Err(error) => {
                message(stdout, &error)?;
                continue;
            }
        };

        let mut models = provider.models.clone();
        if !default_model.trim().is_empty() && !models.iter().any(|item| item == &default_model) {
            models.push(default_model.clone());
        }

        // 所有验证通过，返回新的 ProviderConfig
        return Ok(Some(ProviderConfig {
            id: fields[0].value.trim().to_string(),
            display_name: fields[1].value.trim().to_string(),
            base_url: normalize_base_url(&fields[2].value),
            protocol: fields[3].value.trim().to_string(),
            api_key: Some(fields[4].value.trim().to_string()).filter(|value| !value.is_empty()),
            models,
            model_context_window: provider.model_context_window.clone(),
            model_temperature: provider.model_temperature.clone(),
            model_modalities: provider.model_modalities.clone(),
            model_costs: provider.model_costs.clone(),
            default_model,
            timeout_seconds: timeout,
            temperature: provider.temperature,
            anthropic_max_tokens: provider.anthropic_max_tokens,
            extra_body,
        }));
    }
}

pub(crate) fn parse_extra_body(
    value: &str,
) -> std::result::Result<Option<serde_json::Map<String, serde_json::Value>>, String> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    match serde_json::from_str::<serde_json::Value>(value) {
        Ok(serde_json::Value::Object(object)) => Ok(Some(object)),
        Ok(_) => Err(t(
            "The extra request body must be a JSON object (for example {\"key\": \"value\"})",
            "额外请求体必须是 JSON 对象 (如 {\"key\": \"value\"})",
        )
        .to_string()),
        Err(error) => Err(if is_zh() {
            format!("无效 JSON: {error}")
        } else {
            format!("Invalid JSON: {error}")
        }),
    }
}

pub(crate) fn edit_model_form(
    stdout: &mut io::Stdout,
    provider: &mut ProviderConfig,
    model: &str,
    thinking_variants: &mut ThinkingVariantPreferences,
) -> Result<bool> {
    let context_window = provider
        .model_context_window
        .get(model)
        .copied()
        .unwrap_or_default();
    let stored_variant = thinking_variants
        .selected(&provider.id, model)
        .filter(|selected| !selected.trim().is_empty())
        .map(str::to_string);
    let variant_options =
        thinking_variant_options_for_model(provider, model, stored_variant.as_deref());
    let initial_variant = stored_variant.clone();
    let cost = provider.model_costs.get(model).copied();
    let currency_value = cost
        .map(|cost| match cost.currency {
            crate::config::CostCurrency::Usd => "USD",
            crate::config::CostCurrency::Cny => "CNY",
        })
        .unwrap_or("")
        .to_string();
    let price_text = |value: Option<f64>| value.map(|v| v.to_string()).unwrap_or_default();
    let mut fields = vec![
        Field::modalities(
            t("Supported input", "支持输入"),
            modality_field_value(provider, model),
        ),
        Field::boolean(
            t("Is an embedding model", "这是语义模型吗"),
            model_is_embedding(provider, model),
        ),
        Field::new(
            t(
                "Model context window (tokens, 0=auto)",
                "模型上下文窗口 (tokens, 0=自动)",
            ),
            context_window.to_string(),
        ),
        thinking_variant_field(&variant_options, stored_variant.as_deref()),
        Field::new(
            t(
                "Temperature (empty = provider default)",
                "Temperature (留空=供应商默认)",
            ),
            provider
                .model_temperature
                .get(model)
                .map(|value| value.to_string())
                .unwrap_or_default(),
        ),
        Field::new(
            t(
                "Price currency (empty = models.dev)",
                "价格货币 (留空=用 models.dev 目录价)",
            ),
            currency_value,
        )
        .choices(&["", "USD", "CNY"])
        .empty_choice_label(t("catalogue", "目录价")),
        Field::new(
            t("Input price / 1M tokens", "输入价 / 1M tokens"),
            price_text(cost.map(|c| c.input)),
        ),
        Field::new(
            t("Output price / 1M tokens", "输出价 / 1M tokens"),
            price_text(cost.map(|c| c.output)),
        ),
        Field::new(
            t(
                "Cache-hit price / 1M (empty = input price)",
                "缓存命中价 / 1M (留空=按输入价)",
            ),
            price_text(cost.and_then(|c| c.cache_read)),
        ),
    ];
    loop {
        if !run_form(stdout, t(" EDIT MODEL ", " 编辑模型 "), &mut fields)? {
            return Ok(false);
        }
        // 价格:选了货币才生效;三个价按所选货币记,估算时统一折 USD。
        match fields[5].value.trim() {
            "" => {
                provider.model_costs.remove(model);
            }
            currency => {
                let parse = |value: &str| -> Option<f64> {
                    let value = value.trim();
                    if value.is_empty() {
                        return None;
                    }
                    value.parse::<f64>().ok().filter(|price| *price >= 0.0)
                };
                let (input, output) = match (parse(&fields[6].value), parse(&fields[7].value)) {
                    (Some(input), Some(output)) => (input, output),
                    _ => {
                        message(
                            stdout,
                            t(
                                "Input and output prices are required non-negative numbers",
                                "输入价与输出价必须是非负数字",
                            ),
                        )?;
                        continue;
                    }
                };
                let cache_read = match (fields[8].value.trim().is_empty(), parse(&fields[8].value))
                {
                    (true, _) => None,
                    (false, Some(price)) => Some(price),
                    (false, None) => {
                        message(
                            stdout,
                            t(
                                "Cache-hit price must be a non-negative number",
                                "缓存命中价必须是非负数字",
                            ),
                        )?;
                        continue;
                    }
                };
                provider.model_costs.insert(
                    model.to_string(),
                    crate::config::ModelCostConfig {
                        currency: if currency == "CNY" {
                            crate::config::CostCurrency::Cny
                        } else {
                            crate::config::CostCurrency::Usd
                        },
                        input,
                        output,
                        cache_read,
                    },
                );
            }
        }
        let mut modalities = parse_modalities(&fields[0].value);
        modalities.retain(|item| item != EMBEDDING_MODALITY);
        if parse_bool_field(&fields[1].value)? {
            modalities.push(EMBEDDING_MODALITY.to_string());
        }
        provider
            .model_modalities
            .insert(model.to_string(), modalities);
        match fields[2].value.trim().parse::<usize>().unwrap_or_default() {
            0 => {
                provider.model_context_window.remove(model);
            }
            value => {
                provider
                    .model_context_window
                    .insert(model.to_string(), value);
            }
        }
        let selected_variant =
            (!fields[3].value.trim().is_empty()).then(|| fields[3].value.trim().to_string());
        if selected_variant != initial_variant {
            thinking_variants.set(&provider.id, model, selected_variant);
        }
        match fields[4].value.trim().parse::<f32>() {
            Ok(value) => {
                provider.model_temperature.insert(model.to_string(), value);
            }
            Err(_) => {
                provider.model_temperature.remove(model);
            }
        }
        return Ok(true);
    }
}

pub(crate) fn thinking_variant_field(
    options: &ThinkingVariantOptions,
    stored: Option<&str>,
) -> Field {
    let mut choices = Vec::with_capacity(options.variants.len() + 2);
    choices.push(String::new());
    if let Some(stored) = stored.filter(|stored| {
        !stored.is_empty() && !options.variants.iter().any(|variant| variant == *stored)
    }) {
        choices.push(stored.to_string());
    }
    choices.extend(options.variants.iter().cloned());
    Field::new(
        t("Thinking variant", "思考程度"),
        stored.unwrap_or_default().to_string(),
    )
    .choices_owned(choices)
    .raw_choice_labels()
    .empty_choice_label("default")
}

pub(crate) fn edit_settings(stdout: &mut io::Stdout, config: &mut AppConfig) -> Result<()> {
    let language = language_choice_value(&config.display.language).unwrap_or("auto");
    let mut fields = vec![
        Field::boolean(t("Enable tools", "工具启用"), config.tools.enabled),
        Field::new(
            t("Maximum tool rounds", "工具最大轮数"),
            config.tools.max_rounds.to_string(),
        ),
        Field::new(
            t("Tool loading mode", "工具加载模式"),
            config.tools.loading_mode.clone(),
        )
        .choices(&["full", "hybrid", "stub"]),
        Field::boolean(
            t("Remember loaded tools", "记住已加载工具"),
            config.tools.persist_loaded_tools,
        ),
        Field::boolean(t("Enable skills", "Skills 启用"), config.skills.enabled),
        Field::boolean(
            t("Allow command execution", "允许执行命令"),
            config.skills.allow_command_execution,
        ),
        Field::new(t("Interface language", "界面语言"), language.to_string())
            .choices(&["auto", "en", "zh"]),
        Field::new(
            t("Show reasoning", "显示思考过程"),
            config.display.reasoning.clone(),
        )
        .choices(&["summary", "full", "hidden"]),
        Field::new(
            t("Show tool call details", "显示工具调用信息"),
            config.display.tool_calls.clone(),
        )
        .choices(&["summary", "full", "hidden"]),
        Field::new(
            t("Command output lines", "命令输出显示行数"),
            config.display.command_output_lines.to_string(),
        ),
        Field::boolean(
            t("Readable tool names", "工具名可读显示"),
            config.display.readable_tool_names,
        ),
        Field::boolean(
            t(
                "Show token usage in shell conversations",
                "Shell 无缝对话显示 Token 计数",
            ),
            config.display.show_token_usage,
        ),
        Field::new(
            t(
                "Show current provider/model in Mixed mode",
                "Mixed 时显示本次供应商/模型",
            ),
            parse_mixed_endpoint_display(&config.display.mixed_model_endpoint_display),
        )
        .choices(&["off", "interactive", "all"]),
        Field::new(
            t("When context reaches its limit", "上下文到达上限后"),
            config.context.on_overflow.clone(),
        )
        .choices(&["compact", "pop"]),
        // Appended rather than inserted: the read-back below is positional.
        Field::new(
            t(
                "Turns replayed when reopening the REPL",
                "重开 REPL 回放的轮数",
            ),
            config.display.repl_replay_turns.to_string(),
        ),
        // 验收:default_mode 只能改 config.jsonc 不像话——空=裸 gqy 出帮助。
        Field::new(
            t("Bare `gqy` default mode", "裸 gqy 默认模式"),
            config.default_mode.clone(),
        )
        .choices(&["", "normal", "dev"])
        .empty_choice_label(t("Help screen", "帮助信息")),
    ];
    // The read-back below is by index, so an insert in the middle silently
    // writes every later value into the wrong setting. This catches that in
    // debug builds; new fields go on the end.
    debug_assert_eq!(
        fields.len(),
        16,
        "global settings fields changed: update the positional read-back below"
    );
    run_form_without_buttons(stdout, t(" GLOBAL SETTINGS ", " 全局设置 "), &mut fields)?;
    config.tools.enabled = parse_bool_field(&fields[0].value)?;
    config.tools.max_rounds = fields[1].value.trim().parse::<usize>()?;
    config.tools.loading_mode = normalize_tools_loading_mode(&fields[2].value);
    config.tools.persist_loaded_tools = parse_bool_field(&fields[3].value)?;
    config.skills.enabled = parse_bool_field(&fields[4].value)?;
    config.skills.allow_command_execution = parse_bool_field(&fields[5].value)?;
    config.display.language = language_choice_value(&fields[6].value)
        .unwrap_or("auto")
        .to_string();
    config.display.reasoning = fields[7].value.trim().to_string();
    config.display.tool_calls = fields[8].value.trim().to_string();
    config.display.command_output_lines = fields[9]
        .value
        .trim()
        .parse::<usize>()?
        .min(MAX_COMMAND_OUTPUT_LINES);
    config.display.readable_tool_names = parse_bool_field(&fields[10].value)?;
    config.display.show_token_usage = parse_bool_field(&fields[11].value)?;
    config.display.mixed_model_endpoint_display = parse_mixed_endpoint_display(&fields[12].value);
    config.context.on_overflow = fields[13].value.trim().to_string();
    config.display.repl_replay_turns = fields[14]
        .value
        .trim()
        .parse::<usize>()?
        .min(MAX_REPL_REPLAY_TURNS);
    config.default_mode = fields[15].value.trim().to_string();
    Ok(())
}

pub(crate) fn language_choice_label(value: &str, zh: bool) -> Option<&'static str> {
    match (value.trim(), zh) {
        ("auto", false) => Some("Auto"),
        ("auto", true) => Some("自动"),
        ("en", false) => Some("English"),
        ("en", true) => Some("英语"),
        ("zh", false) => Some("Simplified Chinese"),
        ("zh", true) => Some("简体中文"),
        _ => None,
    }
}

pub(crate) fn language_choice_value(value: &str) -> Option<&'static str> {
    match value.trim() {
        "auto" | "Auto" | "自动" => Some("auto"),
        "en" | "English" | "英语" => Some("en"),
        "zh" | "Simplified Chinese" | "简体中文" => Some("zh"),
        _ => None,
    }
}

pub(crate) fn parse_mixed_endpoint_display(value: &str) -> String {
    match value.trim() {
        "关" | "Off" | "off" => "off".to_string(),
        "全部模式" | "All modes" | "all" => "all".to_string(),
        _ => "interactive".to_string(),
    }
}

pub(crate) fn normalize_tools_loading_mode(value: &str) -> String {
    match value.trim() {
        "lazy" => "hybrid".to_string(),
        value => value.to_string(),
    }
}

pub(crate) fn parse_bool_field(value: &str) -> Result<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "yes" | "y" | "1" | "on" | "启用" | "是" => Ok(true),
        "false" | "no" | "n" | "0" | "off" | "禁用" | "否" => Ok(false),
        value => {
            if is_zh() {
                bail!("无效的布尔值: {value}")
            } else {
                bail!("Invalid boolean value: {value}")
            }
        }
    }
}

/// TUI 编辑期间挂起输入法。原为 Linux 的 fcitx5-remote 机制(-c/-o);
/// macOS 输入法没有等价命令行,保留空实现以维持编辑流程结构。
pub(crate) struct InputMethodGuard;

impl InputMethodGuard {
    pub(crate) fn new() -> Self {
        Self
    }

    pub(crate) fn enter_editing(&mut self) {}

    pub(crate) fn leave_editing(&mut self) {}
}

pub(crate) fn edit_inline_value(
    stdout: &mut io::Stdout,
    title: &str,
    current: &str,
    sensitive: bool,
) -> Result<Option<String>> {
    let mut value = current.to_string();
    let mut cursor = value.chars().count();
    let mut ime = InputMethodGuard::new();
    ime.enter_editing();
    loop {
        draw_inline_editor(stdout, title, &value, cursor, sensitive)?;
        match read_key()? {
            KeyCode::Esc => {
                ime.leave_editing();
                execute!(stdout, Hide)?;
                return Ok(None);
            }
            KeyCode::Enter => {
                ime.leave_editing();
                execute!(stdout, Hide)?;
                return Ok(Some(value));
            }
            KeyCode::Left => cursor = cursor.saturating_sub(1),
            KeyCode::Right => cursor = (cursor + 1).min(value.chars().count()),
            KeyCode::Home => cursor = 0,
            KeyCode::End => cursor = value.chars().count(),
            KeyCode::Backspace if cursor > 0 => remove_char_before_cursor(&mut value, &mut cursor),
            KeyCode::Delete => remove_char_at_cursor(&mut value, cursor),
            KeyCode::Char(ch) => insert_char_at_cursor(&mut value, &mut cursor, ch),
            _ => {}
        }
    }
}

pub(crate) fn draw_inline_editor(
    stdout: &mut io::Stdout,
    title: &str,
    value: &str,
    cursor: usize,
    sensitive: bool,
) -> Result<()> {
    let (cols, rows) = terminal::size()?;
    let width = 72_u16.min(cols.saturating_sub(2)).max(12);
    let height = rows.clamp(1, 6);
    let x = cols.saturating_sub(width) / 2;
    let y = rows.saturating_sub(height) / 2;
    let capacity = width.saturating_sub(4) as usize;
    let chars = value.chars().collect::<Vec<_>>();
    let cursor = cursor.min(chars.len());
    let start = cursor
        .saturating_sub(capacity.saturating_sub(1))
        .min(chars.len().saturating_sub(capacity));
    let end = (start + capacity).min(chars.len());
    let visible = if sensitive {
        "*".repeat(end.saturating_sub(start))
    } else {
        chars[start..end].iter().collect::<String>()
    };

    queue!(stdout, Hide, Clear(ClearType::All))?;
    draw_box(stdout, x, y, width, height, title)?;
    queue!(
        stdout,
        MoveTo(x + 2, y + 2),
        Print(pad(&visible, capacity)),
        MoveTo(x + 2, y + 4),
        SetAttribute(Attribute::Dim),
        Print(truncate(
            t("[Enter]save  [Esc]cancel", "[Enter]保存  [Esc]取消"),
            capacity,
        )),
        SetAttribute(Attribute::Reset),
        MoveTo(
            x + 2 + u16::try_from(cursor.saturating_sub(start)).unwrap_or(u16::MAX),
            y + 2,
        ),
        Show,
    )?;
    stdout.flush()?;
    Ok(())
}
