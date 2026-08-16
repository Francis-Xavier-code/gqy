//! state — 自 src/config_tui.rs 拆分。

#![allow(clippy::manual_clamp, clippy::too_many_arguments)]
pub(crate) use super::*;

pub(crate) fn run_form(stdout: &mut io::Stdout, title: &str, fields: &mut [Field]) -> Result<bool> {
    run_form_from(stdout, title, fields, false)
}

/// `start_editing` puts the caret in the first field straight away, for forms
/// reached from a menu row that already showed the value: the row said what it
/// was, Enter said "change it", so a second Enter to begin typing is a keypress
/// that asks a question nobody had.
pub(crate) fn run_form_editing(
    stdout: &mut io::Stdout,
    title: &str,
    fields: &mut [Field],
) -> Result<bool> {
    run_form_from(stdout, title, fields, true)
}

pub(crate) fn run_form_from(
    stdout: &mut io::Stdout,
    title: &str,
    fields: &mut [Field],
    start_editing: bool,
) -> Result<bool> {
    let mut selected = 0usize;
    let mut ime = InputMethodGuard::new();
    // Only a plain text field can be typed into directly; the others open
    // their own picker on Enter, so landing "inside" them would mean typing
    // free text where a choice was expected.
    let mut editing = start_editing
        && fields.first().is_some_and(|field| {
            !field.boolean && !field.textarea && !field.modalities && field.choices.is_empty()
        });
    if editing {
        ime.enter_editing();
    }
    let mut cursors = fields
        .iter()
        .map(|field| field.value.chars().count())
        .collect::<Vec<_>>();
    loop {
        draw_form(stdout, title, fields, selected, editing, &cursors, true)?;
        match read_key()? {
            KeyCode::Esc if editing => {
                ime.leave_editing();
                editing = false;
            }
            KeyCode::Esc | KeyCode::Char('q') if !editing => return Ok(false),
            KeyCode::Enter if editing => {
                ime.leave_editing();
                editing = false;
            }
            KeyCode::Enter if !editing && selected == fields.len() => return Ok(true),
            KeyCode::Enter if !editing && selected == fields.len() + 1 => return Ok(false),
            KeyCode::Enter if !editing && fields[selected].boolean => {
                let value = select_bool(
                    stdout,
                    fields[selected].label,
                    parse_bool_field(&fields[selected].value)?,
                )?;
                fields[selected].value = value.to_string();
                cursors[selected] = fields[selected].value.chars().count();
            }
            KeyCode::Enter if !editing && fields[selected].modalities => {
                fields[selected].value = select_multi_choice(
                    stdout,
                    fields[selected].label,
                    &fields[selected].value,
                    &["text", "image", "audio", "video", "pdf"]
                        .iter()
                        .map(|item| item.to_string())
                        .collect::<Vec<_>>(),
                )?;
                cursors[selected] = fields[selected].value.chars().count();
            }
            KeyCode::Enter if !editing && !fields[selected].choices.is_empty() => {
                fields[selected].value = select_choice(
                    stdout,
                    fields[selected].label,
                    &fields[selected].value,
                    &fields[selected].choices,
                    fields[selected].empty_choice_label,
                    fields[selected].raw_choice_labels,
                )?;
                cursors[selected] = fields[selected].value.chars().count();
            }
            KeyCode::Enter if !editing && fields[selected].dialog_list => {
                edit_dialog_list(stdout, &mut fields[selected].value)?;
                cursors[selected] = fields[selected].value.chars().count();
            }
            KeyCode::Enter if !editing && fields[selected].textarea => {
                edit_textarea(stdout, &mut fields[selected].value)?;
                cursors[selected] = fields[selected].value.chars().count();
                if !fields[selected].sensitive {
                    return Ok(true);
                }
            }
            KeyCode::Enter if !editing => {
                if !fields[selected].boolean {
                    ime.enter_editing();
                    editing = true;
                }
            }
            KeyCode::Char('s') if !editing => return Ok(true),
            KeyCode::Up | KeyCode::Char('k') if !editing => selected = selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') if !editing => {
                selected = (selected + 1).min(fields.len() + 1)
            }
            KeyCode::Left | KeyCode::Char('h') if !editing && selected == fields.len() + 1 => {
                selected = fields.len()
            }
            KeyCode::Right | KeyCode::Char('l') if !editing && selected == fields.len() => {
                selected = fields.len() + 1
            }
            KeyCode::Left if editing => cursors[selected] = cursors[selected].saturating_sub(1),
            KeyCode::Right if editing => {
                cursors[selected] =
                    (cursors[selected] + 1).min(fields[selected].value.chars().count())
            }
            KeyCode::Home if editing => cursors[selected] = 0,
            KeyCode::End if editing => cursors[selected] = fields[selected].value.chars().count(),
            KeyCode::Backspace if editing => {
                if cursors[selected] > 0 {
                    remove_char_before_cursor(&mut fields[selected].value, &mut cursors[selected]);
                }
            }
            KeyCode::Delete if editing => {
                remove_char_at_cursor(&mut fields[selected].value, cursors[selected])
            }
            KeyCode::Char(char) if editing => {
                insert_char_at_cursor(&mut fields[selected].value, &mut cursors[selected], char)
            }
            _ => {}
        }
    }
}

pub(crate) fn run_form_without_buttons(
    stdout: &mut io::Stdout,
    title: &str,
    fields: &mut [Field],
) -> Result<()> {
    let mut selected = 0usize;
    let mut editing = false;
    let mut ime = InputMethodGuard::new();
    let mut cursors = fields
        .iter()
        .map(|field| field.value.chars().count())
        .collect::<Vec<_>>();
    loop {
        draw_form(stdout, title, fields, selected, editing, &cursors, false)?;
        match read_key()? {
            KeyCode::Esc if editing => {
                ime.leave_editing();
                editing = false;
            }
            KeyCode::Esc | KeyCode::Char('q') if !editing => return Ok(()),
            KeyCode::Enter if editing => {
                ime.leave_editing();
                editing = false;
            }
            KeyCode::Enter if !editing && fields[selected].boolean => {
                let value = select_bool(
                    stdout,
                    fields[selected].label,
                    parse_bool_field(&fields[selected].value)?,
                )?;
                fields[selected].value = value.to_string();
                cursors[selected] = fields[selected].value.chars().count();
            }
            KeyCode::Enter if !editing && fields[selected].modalities => {
                fields[selected].value = select_multi_choice(
                    stdout,
                    fields[selected].label,
                    &fields[selected].value,
                    &["text", "image", "audio", "video", "pdf"]
                        .iter()
                        .map(|item| item.to_string())
                        .collect::<Vec<_>>(),
                )?;
                cursors[selected] = fields[selected].value.chars().count();
            }
            KeyCode::Enter if !editing && !fields[selected].choices.is_empty() => {
                fields[selected].value = select_choice(
                    stdout,
                    fields[selected].label,
                    &fields[selected].value,
                    &fields[selected].choices,
                    fields[selected].empty_choice_label,
                    fields[selected].raw_choice_labels,
                )?;
                cursors[selected] = fields[selected].value.chars().count();
            }
            KeyCode::Enter if !editing && fields[selected].textarea => {
                edit_textarea(stdout, &mut fields[selected].value)?;
                cursors[selected] = fields[selected].value.chars().count();
                if !fields[selected].sensitive {
                    return Ok(());
                }
            }
            KeyCode::Enter if !editing => {
                if !fields[selected].boolean {
                    ime.enter_editing();
                    editing = true;
                }
            }
            KeyCode::Up | KeyCode::Char('k') if !editing => selected = selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') if !editing => {
                selected = (selected + 1).min(fields.len().saturating_sub(1))
            }
            KeyCode::Left if editing => cursors[selected] = cursors[selected].saturating_sub(1),
            KeyCode::Right if editing => {
                cursors[selected] =
                    (cursors[selected] + 1).min(fields[selected].value.chars().count())
            }
            KeyCode::Home if editing => cursors[selected] = 0,
            KeyCode::End if editing => cursors[selected] = fields[selected].value.chars().count(),
            KeyCode::Backspace if editing => {
                if cursors[selected] > 0 {
                    remove_char_before_cursor(&mut fields[selected].value, &mut cursors[selected]);
                }
            }
            KeyCode::Delete if editing => {
                remove_char_at_cursor(&mut fields[selected].value, cursors[selected])
            }
            KeyCode::Char(char) if editing => {
                insert_char_at_cursor(&mut fields[selected].value, &mut cursors[selected], char)
            }
            _ => {}
        }
    }
}

pub(crate) fn select_bool(stdout: &mut io::Stdout, label: &str, current: bool) -> Result<bool> {
    let mut selected = if current { 0 } else { 1 };
    let options = [
        boolean_label(true).to_string(),
        boolean_label(false).to_string(),
    ];
    loop {
        draw_menu(stdout, label, &options, selected, "")?;
        match read_key()? {
            KeyCode::Esc | KeyCode::Char('q') => return Ok(current),
            KeyCode::Up | KeyCode::Char('k') => selected = selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => selected = (selected + 1).min(options.len() - 1),
            KeyCode::Enter => return Ok(selected == 0),
            _ => {}
        }
    }
}

pub(crate) fn select_choice(
    stdout: &mut io::Stdout,
    label: &str,
    current: &str,
    choices: &[String],
    empty_label: &'static str,
    raw_choice_labels: bool,
) -> Result<String> {
    let mut selected = choices.iter().position(|item| item == current).unwrap_or(0);
    loop {
        let options = choices
            .iter()
            .map(|choice| choice_display_label(choice, empty_label, raw_choice_labels))
            .collect::<Vec<_>>();
        draw_menu(stdout, label, &options, selected, "")?;
        match read_key()? {
            KeyCode::Esc | KeyCode::Char('q') => return Ok(current.to_string()),
            KeyCode::Up | KeyCode::Char('k') => selected = selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => selected = (selected + 1).min(choices.len() - 1),
            KeyCode::Enter => return Ok(choices[selected].clone()),
            _ => {}
        }
    }
}

pub(crate) fn select_multi_choice(
    stdout: &mut io::Stdout,
    label: &str,
    current: &str,
    choices: &[String],
) -> Result<String> {
    let mut selected = 0usize;
    let mut active = choices
        .iter()
        .map(|choice| has_modality(current, choice))
        .collect::<Vec<_>>();
    loop {
        let options = choices
            .iter()
            .zip(&active)
            .map(|(choice, active)| {
                format!(
                    "{} {}",
                    if *active { "[*]" } else { "[ ]" },
                    choice_label(choice, "")
                )
            })
            .collect::<Vec<_>>();
        draw_menu(
            stdout,
            label,
            &options,
            selected,
            t(
                "[Tab]select/deselect [Enter/q]confirm",
                "[Tab]选择/取消 [Enter/q]确认",
            ),
        )?;
        match read_key()? {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter => {
                return Ok(choices
                    .iter()
                    .zip(active)
                    .filter_map(|(choice, active)| active.then(|| choice.clone()))
                    .collect::<Vec<_>>()
                    .join(", "))
            }
            KeyCode::Up | KeyCode::Char('k') => selected = selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => selected = (selected + 1).min(choices.len() - 1),
            KeyCode::Tab | KeyCode::Char(' ') => active[selected] = !active[selected],
            _ => {}
        }
    }
}

pub(crate) fn choice_label(choice: &str, empty_label: &str) -> String {
    if choice.is_empty() {
        empty_label.to_string()
    } else if let Some((provider, model)) = choice.split_once('\t') {
        format!("{provider} / {model}")
    } else if let Some(label) = localized_choice_label(choice, is_zh()) {
        label.to_string()
    } else {
        choice.to_string()
    }
}

pub(crate) fn choice_display_label(choice: &str, empty_label: &str, raw: bool) -> String {
    if choice.is_empty() {
        empty_label.to_string()
    } else if raw {
        choice.to_string()
    } else {
        choice_label(choice, empty_label)
    }
}

pub(crate) fn boolean_label(value: bool) -> &'static str {
    if value {
        t("Enabled", "启用")
    } else {
        t("Disabled", "禁用")
    }
}

pub(crate) fn localized_choice_label(value: &str, zh: bool) -> Option<&'static str> {
    if let Some(label) = language_choice_label(value, zh) {
        return Some(label);
    }
    match (value.trim(), zh) {
        ("normal", false) => Some("Normal mode (normal)"),
        ("normal", true) => Some("普通模式（normal）"),
        ("dev", false) => Some("Dev mode (dev)"),
        ("dev", true) => Some("开发模式（dev）"),
        ("minimal", false) => Some("Minimal"),
        ("minimal", true) => Some("最低"),
        ("low", false) => Some("Low"),
        ("low", true) => Some("低"),
        ("medium", false) => Some("Medium"),
        ("medium", true) => Some("中"),
        ("high", false) => Some("High"),
        ("high", true) => Some("高"),
        ("xhigh", false) => Some("Extra high"),
        ("xhigh", true) => Some("极高"),
        ("global", false) => Some("Global"),
        ("global", true) => Some("全球"),
        ("mainland", false) => Some("Mainland China"),
        ("mainland", true) => Some("中国大陆"),
        ("summary", false) => Some("Summary"),
        ("summary", true) => Some("摘要"),
        ("full", false) => Some("Full"),
        ("full", true) => Some("完整"),
        ("hidden", false) => Some("Hidden"),
        ("hidden", true) => Some("隐藏"),
        ("hybrid", false) => Some("Hybrid"),
        ("hybrid", true) => Some("混合"),
        ("stub", false) => Some("Stub"),
        ("stub", true) => Some("精简常驻"),
        ("off", false) => Some("Off"),
        ("off", true) => Some("关"),
        ("interactive", false) => Some("Interactive only"),
        ("interactive", true) => Some("仅交互模式"),
        ("all", false) => Some("All modes"),
        ("all", true) => Some("全部模式"),
        ("pop", false) => Some("Remove oldest"),
        ("pop", true) => Some("弹出旧消息"),
        ("compact", false) => Some("Compact context"),
        ("compact", true) => Some("压缩上下文"),
        ("text", false) => Some("Text"),
        ("text", true) => Some("文本"),
        ("image", false) => Some("Image"),
        ("image", true) => Some("图片"),
        ("audio", false) => Some("Audio"),
        ("audio", true) => Some("音频"),
        ("video", false) => Some("Video"),
        ("video", true) => Some("视频"),
        ("pdf", false) => Some("PDF"),
        ("pdf", true) => Some("PDF"),
        ("自动", false) => Some("Auto"),
        ("自动", true) => Some("自动"),
        _ => None,
    }
}

pub(crate) fn provider_model_choice_values(
    config: &AppConfig,
    include_current: bool,
) -> Vec<String> {
    let mut choices = vec![String::new()];
    if include_current {
        choices.push(format!(
            "{OPENCODE_PROVIDER_ID}\t{OPENCODE_DEFAULT_VISION_MODEL}"
        ));
    }
    choices.extend(
        config
            .provider_model_choices()
            .into_iter()
            .map(|choice| choice.value()),
    );
    choices
}

pub(crate) fn vision_provider_model_choice_values(config: &AppConfig) -> Vec<String> {
    let mut choices = vec![
        String::new(),
        format!("{OPENCODE_PROVIDER_ID}\t{OPENCODE_DEFAULT_VISION_MODEL}"),
    ];
    choices.extend(
        config
            .multimodal_provider_model_choices()
            .into_iter()
            .map(|choice| choice.value()),
    );
    choices.sort();
    choices.dedup();
    choices
}

pub(crate) fn active_multimodal_label(config: &AppConfig) -> String {
    let choices = config.active_multimodal_provider_model_choices();
    if choices.is_empty() {
        format!(
            "{} / {}",
            OPENCODE_PROVIDER_ID, OPENCODE_DEFAULT_VISION_MODEL
        )
    } else if choices.len() == 1 {
        choices[0].label()
    } else {
        t("Mixed", "混合").to_string()
    }
}

pub(crate) fn modality_field_value(provider: &ProviderConfig, model: &str) -> String {
    provider
        .input_modalities(model)
        .unwrap_or_else(|| vec!["text".to_string()])
        .join(", ")
}

pub(crate) fn parse_modalities(value: &str) -> Vec<String> {
    value
        .split(|ch| ch == ',' || ch == '，' || ch == '\n' || ch == '\r')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_string)
        .collect()
}

pub(crate) fn has_modality(value: &str, modality: &str) -> bool {
    parse_modalities(value).iter().any(|item| item == modality)
}

pub(crate) fn select_active_multimodal_provider(
    stdout: &mut io::Stdout,
    config: &mut AppConfig,
) -> Result<()> {
    let choices = config.multimodal_provider_model_choices();
    if choices.is_empty() {
        message(
            stdout,
            t(
                "No models support image input. Configure Supported input under Edit model first.",
                "没有支持图片输入的模型，请先在编辑模型里配置支持输入。",
            ),
        )?;
        return Ok(());
    }
    let mut selected = choices
        .iter()
        .position(|choice| {
            config.is_active_multimodal_provider_model(&choice.provider_id, &choice.model)
        })
        .unwrap_or(0);
    loop {
        let options = choices
            .iter()
            .map(|choice| {
                let marker = if config
                    .is_active_multimodal_provider_model(&choice.provider_id, &choice.model)
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
            t(" SELECT MULTIMODAL MODEL ", " 选择多模态模型 "),
            &options,
            selected,
            t(
                "[Tab]activate/deactivate [Enter/q]confirm",
                "[Tab]激活/取消 [Enter/q]确认",
            ),
        )?;
        match read_key()? {
            KeyCode::Char('q') | KeyCode::Esc | KeyCode::Enter => return Ok(()),
            KeyCode::Up | KeyCode::Char('k') => selected = selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => selected = (selected + 1).min(options.len() - 1),
            KeyCode::Tab => {
                let choice = choices[selected].clone();
                config
                    .toggle_active_multimodal_provider_model(&choice.provider_id, &choice.model)?;
            }
            _ => {}
        }
    }
}

pub(crate) fn vision_provider_value(config: &AppConfig) -> String {
    let vision = &config.plugins.vision;
    if vision.vision_provider_id.trim().is_empty() {
        format!("{OPENCODE_PROVIDER_ID}\t{OPENCODE_DEFAULT_VISION_MODEL}")
    } else if vision.vision_model.trim().is_empty() {
        config
            .provider(Some(vision.vision_provider_id.trim()))
            .map(|provider| format!("{}\t{}", provider.id, provider.default_model))
            .unwrap_or_else(|_| vision.vision_provider_id.clone())
    } else {
        format!("{}\t{}", vision.vision_provider_id, vision.vision_model)
    }
}

pub(crate) fn kb_embedding_provider_value(config: &AppConfig) -> String {
    let kb = &config.plugins.knowledge_base;
    if kb.embedding_provider_id.trim().is_empty() {
        String::new()
    } else if kb.embedding_model.trim().is_empty() {
        config
            .provider(Some(kb.embedding_provider_id.trim()))
            .map(|provider| format!("{}\t{}", provider.id, provider.default_model))
            .unwrap_or_else(|_| kb.embedding_provider_id.clone())
    } else {
        format!("{}\t{}", kb.embedding_provider_id, kb.embedding_model)
    }
}

pub(crate) fn parse_provider_model_choice(value: &str) -> (String, String) {
    let value = value.trim();
    if value.is_empty() {
        return (String::new(), String::new());
    }
    if let Some((provider, model)) = value.split_once('\t') {
        return (provider.trim().to_string(), model.trim().to_string());
    }
    (value.to_string(), String::new())
}

/// 预设对话列表式编辑器(验收 #19):每行一对 user/assistant,回车编辑、
/// [a] 新增、[d] 删除;退出时把列表写回 `user:`/`assistant:` 行格式,
/// 与手写 dialogs 文件同构,存量文件无需迁移。
pub(crate) fn edit_dialog_list(stdout: &mut io::Stdout, value: &mut String) -> Result<()> {
    let mut pairs = crate::persona_hint::parse_dialogs(value);
    let mut selected = 0usize;
    loop {
        let mut options: Vec<String> = pairs
            .iter()
            .map(|(question, answer)| {
                format!(
                    "user: {}  assistant: {}",
                    truncate(question.lines().next().unwrap_or(""), 20),
                    truncate(answer.lines().next().unwrap_or(""), 20),
                )
            })
            .collect();
        if options.is_empty() {
            options.push(t("(no preset dialogs)", "(暂无预设对话)").to_string());
        }
        selected = selected.min(options.len() - 1);
        draw_menu(
            stdout,
            t(" PRESET DIALOGS ", " 预设对话 "),
            &options,
            selected,
            t(
                "[Enter]edit [a]add [d]delete [j/k]move [q]done",
                "[Enter]编辑 [a]新增 [d]删除 [j/k]移动 [q]完成",
            ),
        )?;
        match read_key()? {
            KeyCode::Esc | KeyCode::Char('q') => {
                *value = crate::persona_hint::format_dialogs(&pairs);
                return Ok(());
            }
            KeyCode::Up | KeyCode::Char('k') => selected = selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => selected = (selected + 1).min(options.len() - 1),
            KeyCode::Char('a') => {
                if let Some(pair) =
                    edit_dialog_pair(stdout, t(" NEW DIALOG ", " 新增对话 "), "", "")?
                {
                    pairs.push(pair);
                    selected = pairs.len() - 1;
                }
            }
            KeyCode::Enter if !pairs.is_empty() => {
                let (question, answer) = pairs[selected].clone();
                if let Some(pair) =
                    edit_dialog_pair(stdout, t(" EDIT DIALOG ", " 编辑对话 "), &question, &answer)?
                {
                    pairs[selected] = pair;
                }
            }
            KeyCode::Char('d') if !pairs.is_empty() => {
                pairs.remove(selected);
            }
            _ => {}
        }
    }
}

/// user/assistant 双框表单:打开即落在 user 框内直接输入,回车确认后
/// j 移到 assistant 框。空的一侧视为放弃(与 `parse_dialogs` 丢弃
/// 空对的语义一致)。
pub(crate) fn edit_dialog_pair(
    stdout: &mut io::Stdout,
    title: &str,
    question: &str,
    answer: &str,
) -> Result<Option<(String, String)>> {
    let mut fields = vec![
        Field::new("user", question.to_string()),
        Field::new("assistant", answer.to_string()),
    ];
    if !run_form_editing(stdout, title, &mut fields)? {
        return Ok(None);
    }
    let question = fields[0].value.trim().to_string();
    let answer = fields[1].value.trim().to_string();
    if question.is_empty() || answer.is_empty() {
        return Ok(None);
    }
    Ok(Some((question, answer)))
}

pub(crate) fn edit_textarea(stdout: &mut io::Stdout, value: &mut String) -> Result<()> {
    execute!(
        stdout,
        Show,
        LeaveAlternateScreen,
        Clear(ClearType::All),
        MoveTo(0, 0)
    )?;
    stdout.flush()?;
    terminal::disable_raw_mode()?;
    let mut file = tempfile::NamedTempFile::new()?;
    file.write_all(value.as_bytes())?;
    let path = file.path().to_path_buf();
    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vim".to_string());
    let status = Command::new(&editor)
        .arg(&path)
        .status()
        .or_else(|_| Command::new("nano").arg(&path).status());
    if let Err(err) = status {
        if is_zh() {
            eprintln!("无法打开编辑器: {err}");
        } else {
            eprintln!("Failed to open editor: {err}");
        }
    }
    *value = std::fs::read_to_string(&path)?.trim().to_string();
    terminal::enable_raw_mode()?;
    execute!(stdout, EnterAlternateScreen, Clear(ClearType::All), Hide)?;
    Ok(())
}

pub(crate) fn draw_menu(
    stdout: &mut io::Stdout,
    title: &str,
    options: &[String],
    selected: usize,
    status: &str,
) -> Result<()> {
    let (cols, rows) = terminal::size()?;
    let content_w = options
        .iter()
        .map(|option| option.chars().count())
        .max()
        .unwrap_or(20)
        .max(title.chars().count())
        .max(menu_help(status).chars().count())
        + 6;
    let width = (content_w as u16).min(cols.saturating_sub(4)).max(56);
    let height = (options.len() as u16 + 5)
        .min(rows.saturating_sub(2))
        .max(7);
    let x = cols.saturating_sub(width) / 2;
    let y = rows.saturating_sub(height) / 2;
    let visible_rows = height.saturating_sub(4).max(1) as usize;
    let window = menu_window(options.len(), selected, visible_rows);

    queue!(stdout, Clear(ClearType::All))?;
    draw_box(stdout, x, y, width, height, title)?;
    queue!(
        stdout,
        MoveTo(x + 2, y + height - 1),
        SetAttribute(Attribute::Dim),
        Print(truncate(
            menu_help(status),
            width.saturating_sub(4) as usize
        )),
        SetAttribute(Attribute::Reset)
    )?;
    for (row, index) in window.enumerate() {
        let option = &options[index];
        queue!(stdout, MoveTo(x + 2, y + row as u16 + 2))?;
        if index == selected {
            queue!(
                stdout,
                SetAttribute(Attribute::Reverse),
                Print(pad(option, width.saturating_sub(4) as usize)),
                SetAttribute(Attribute::Reset)
            )?;
        } else {
            queue!(stdout, Print(pad(option, width.saturating_sub(4) as usize)))?;
        }
    }
    stdout.flush()?;
    Ok(())
}

pub(crate) fn menu_window(
    item_count: usize,
    selected: usize,
    visible_rows: usize,
) -> std::ops::Range<usize> {
    if item_count == 0 || visible_rows == 0 {
        return 0..0;
    }
    let visible_rows = visible_rows.min(item_count);
    let selected = selected.min(item_count - 1);
    let start = selected
        .saturating_sub(visible_rows / 2)
        .min(item_count - visible_rows);
    start..start + visible_rows
}

pub(crate) fn menu_help(status: &str) -> &str {
    if status.is_empty() {
        t(
            "[j/k]move [Enter]select [q]back",
            "[j/k]移动 [Enter]选择 [q]返回",
        )
    } else {
        status
    }
}

pub(crate) fn draw_box(
    stdout: &mut io::Stdout,
    x: u16,
    y: u16,
    width: u16,
    height: u16,
    title: &str,
) -> Result<()> {
    queue!(
        stdout,
        MoveTo(x, y),
        Print(format!(
            "┌{}┐",
            "─".repeat(width.saturating_sub(2) as usize)
        ))
    )?;
    for row in 1..height.saturating_sub(1) {
        queue!(
            stdout,
            MoveTo(x, y + row),
            Print(format!(
                "│{}│",
                " ".repeat(width.saturating_sub(2) as usize)
            ))
        )?;
    }
    queue!(
        stdout,
        MoveTo(x, y + height.saturating_sub(1)),
        Print(format!(
            "└{}┘",
            "─".repeat(width.saturating_sub(2) as usize)
        ))
    )?;
    queue!(
        stdout,
        MoveTo(x + 2, y),
        SetAttribute(Attribute::Bold),
        Print(title),
        SetAttribute(Attribute::Reset)
    )?;
    Ok(())
}

pub(crate) fn draw_column(
    stdout: &mut io::Stdout,
    x: u16,
    y: u16,
    width: u16,
    height: u16,
    title: &str,
    items: &[String],
    selected: usize,
    scroll: usize,
    active: bool,
) -> Result<()> {
    let attr = if active {
        Attribute::Reverse
    } else {
        Attribute::Bold
    };
    queue!(
        stdout,
        MoveTo(x, y),
        SetAttribute(attr),
        Print(pad(&truncate(title, width as usize), width as usize)),
        SetAttribute(Attribute::Reset)
    )?;
    let visible_rows = height.saturating_sub(2) as usize;
    let start = column_scroll(selected, scroll, visible_rows);
    for row in 0..visible_rows {
        let index = start + row;
        if index >= items.len() {
            break;
        }
        queue!(stdout, MoveTo(x, y + row as u16 + 1))?;
        let line = truncate(&items[index], width as usize);
        if index == selected {
            queue!(
                stdout,
                SetAttribute(Attribute::Reverse),
                Print(pad(&line, width as usize)),
                SetAttribute(Attribute::Reset)
            )?;
        } else {
            queue!(stdout, Print(pad(&line, width as usize)))?;
        }
    }
    Ok(())
}

pub(crate) fn column_visible_rows() -> usize {
    terminal::size()
        .map(|(_, rows)| rows.saturating_sub(4) as usize)
        .unwrap_or(1)
}

pub(crate) fn column_scroll(selected: usize, scroll: usize, visible_rows: usize) -> usize {
    if visible_rows == 0 {
        return 0;
    }
    if selected < scroll {
        selected
    } else if selected >= scroll + visible_rows {
        selected + 1 - visible_rows
    } else {
        scroll
    }
}

pub(crate) fn draw_form(
    stdout: &mut io::Stdout,
    title: &str,
    fields: &[Field],
    selected: usize,
    editing: bool,
    cursors: &[usize],
    show_buttons: bool,
) -> Result<()> {
    let (cols, rows) = terminal::size()?;
    let width = cols.saturating_sub(8).min(96).max(48);
    let height = (fields.len() as u16 + 8)
        .min(rows.saturating_sub(4))
        .max(10);
    let x = cols.saturating_sub(width) / 2;
    let y = rows.saturating_sub(height) / 2;
    queue!(stdout, Clear(ClearType::All))?;
    draw_box(stdout, x, y, width, height, title)?;
    queue!(
        stdout,
        MoveTo(x + 2, y + 1),
        Print(if show_buttons {
            t(
                "[j/k]move [Enter]edit/open editor [s]confirm [q]back",
                "[j/k]移动 [Enter]编辑/打开编辑器 [s]确认 [q]返回",
            )
        } else {
            t(
                "[j/k]move [Enter]edit/open editor [q]back",
                "[j/k]移动 [Enter]编辑/打开编辑器 [q]返回",
            )
        })
    )?;
    let mut cursor = None;
    for (index, field) in fields.iter().enumerate() {
        let row_y = y + index as u16 + 3;
        queue!(stdout, MoveTo(x + 2, row_y))?;
        let marker = if index == selected { ">" } else { " " };
        let value = field_display_value(field, index == selected && editing);
        let prefix = format!("{marker} {}: ", field.label);
        let line = truncate(
            &format!("{prefix}{value}"),
            width.saturating_sub(4) as usize,
        );
        if index == selected && !editing {
            queue!(
                stdout,
                SetAttribute(Attribute::Reverse),
                Print(pad(&line, width.saturating_sub(4) as usize)),
                SetAttribute(Attribute::Reset)
            )?;
        } else {
            queue!(stdout, Print(pad(&line, width.saturating_sub(4) as usize)))?;
        }
        if index == selected && editing {
            let cursor_text = take_chars(&field.value.replace('\n', " "), cursors[index]);
            let cursor_x = x
                + 2
                + display_width(&prefix) as u16
                + display_width(&truncate(&cursor_text, width.saturating_sub(4) as usize)) as u16;
            cursor = Some((cursor_x.min(x + width.saturating_sub(3)), row_y));
        }
    }
    if show_buttons {
        let button_y = y + fields.len() as u16 + 4;
        draw_form_button(
            stdout,
            x + 2,
            button_y,
            t(" Save ", " 保存 "),
            selected == fields.len() && !editing,
        )?;
        draw_form_button(
            stdout,
            x + 14,
            button_y,
            t(" Back ", " 返回 "),
            selected == fields.len() + 1 && !editing,
        )?;
    }

    let mode = if editing {
        t(
            "Editing; Enter/Esc finishes editing",
            "编辑中，Enter/Esc 结束编辑",
        )
    } else if show_buttons {
        t(
            "Navigating; Enter selects the current item",
            "导航中，Enter 选择当前项",
        )
    } else {
        t(
            "Navigating; Enter selects the current item; [q]back",
            "导航中，Enter 选择当前项，[q]返回",
        )
    };
    queue!(
        stdout,
        MoveTo(x + 2, y + height.saturating_sub(1)),
        Print(truncate(mode, width.saturating_sub(4) as usize))
    )?;
    if let Some((x, y)) = cursor {
        queue!(stdout, Show, MoveTo(x, y))?;
    } else {
        queue!(stdout, Hide)?;
    }
    stdout.flush()?;
    Ok(())
}

pub(crate) fn field_display_value(field: &Field, reveal_sensitive: bool) -> String {
    if field.dialog_list {
        // 列表式字段没有 $EDITOR;摘要成对数,原始序列化文本不上屏。
        let pairs = crate::persona_hint::parse_dialogs(&field.value).len();
        return if pairs == 0 {
            t("(empty; Enter opens the list)", "(空,回车进列表)").to_string()
        } else if is_zh() {
            format!("[{pairs} 对对话]")
        } else {
            format!("[{pairs} dialog pair(s)]")
        };
    }
    if field.sensitive && !field.value.is_empty() && !reveal_sensitive {
        if field.textarea {
            if is_zh() {
                format!("[已配置 {} 项]", parse_key_list(&field.value).len())
            } else {
                format!("[{} configured]", parse_key_list(&field.value).len())
            }
        } else {
            "********".to_string()
        }
    } else if !field.choices.is_empty() && field.value.is_empty() {
        field.empty_choice_label.to_string()
    } else if !field.choices.is_empty() {
        choice_display_label(
            &field.value,
            field.empty_choice_label,
            field.raw_choice_labels,
        )
    } else if field.boolean {
        match parse_bool_field(&field.value) {
            Ok(value) => boolean_label(value).to_string(),
            Err(_) => field.value.clone(),
        }
    } else if field.modalities {
        parse_modalities(&field.value)
            .iter()
            .map(|value| choice_label(value, ""))
            .collect::<Vec<_>>()
            .join(", ")
    } else {
        truncate(&field.value.replace('\n', " "), 70)
    }
}

pub(crate) fn draw_form_button(
    stdout: &mut io::Stdout,
    x: u16,
    y: u16,
    label: &str,
    selected: bool,
) -> Result<()> {
    queue!(stdout, MoveTo(x, y))?;
    if selected {
        queue!(
            stdout,
            SetAttribute(Attribute::Reverse),
            Print(label),
            SetAttribute(Attribute::Reset)
        )?;
    } else {
        queue!(stdout, Print(label))?;
    }
    Ok(())
}

pub(crate) fn insert_char_at_cursor(value: &mut String, cursor: &mut usize, ch: char) {
    let byte_index = byte_index_for_char(value, *cursor);
    value.insert(byte_index, ch);
    *cursor += 1;
}

pub(crate) fn remove_char_before_cursor(value: &mut String, cursor: &mut usize) {
    let end = byte_index_for_char(value, *cursor);
    let start = byte_index_for_char(value, cursor.saturating_sub(1));
    value.replace_range(start..end, "");
    *cursor -= 1;
}

pub(crate) fn remove_char_at_cursor(value: &mut String, cursor: usize) {
    if cursor >= value.chars().count() {
        return;
    }
    let start = byte_index_for_char(value, cursor);
    let end = byte_index_for_char(value, cursor + 1);
    value.replace_range(start..end, "");
}

pub(crate) fn byte_index_for_char(value: &str, char_index: usize) -> usize {
    value
        .char_indices()
        .nth(char_index)
        .map(|(index, _)| index)
        .unwrap_or(value.len())
}

pub(crate) fn take_chars(value: &str, count: usize) -> String {
    value.chars().take(count).collect()
}

pub(crate) fn message(stdout: &mut io::Stdout, text: &str) -> Result<()> {
    queue!(
        stdout,
        Clear(ClearType::All),
        MoveTo(0, 0),
        Print(text),
        MoveTo(0, 2),
        Print(t("Press any key to continue", "按任意键继续"))
    )?;
    stdout.flush()?;
    let _ = read_key()?;
    Ok(())
}

pub(crate) fn read_key() -> Result<KeyCode> {
    read_key_with_timeout(None).map(|key| key.expect("blocking read should return a key"))
}

pub(crate) fn read_key_with_timeout(timeout: Option<Duration>) -> Result<Option<KeyCode>> {
    loop {
        if let Some(timeout) = timeout {
            if !event::poll(timeout)? {
                return Ok(None);
            }
        }
        if let Event::Key(KeyEvent { code, .. }) = event::read()? {
            return Ok(Some(code));
        }
    }
}

pub(crate) fn active_label(config: &AppConfig) -> String {
    match config.active_provider_model_choices().as_slice() {
        [] => t("Not configured", "未配置").to_string(),
        [choice] => format!("{} / {}", choice.provider_name, choice.model),
        _ => t("Mixed", "混合").to_string(),
    }
}

pub(crate) fn normalize_base_url(value: &str) -> String {
    let mut url = value.trim().trim_end_matches('/').to_string();
    if url.ends_with("/chat/completions") {
        url.truncate(url.len() - "/chat/completions".len());
    }
    url
}

pub(crate) fn truncate(value: &str, max: usize) -> String {
    if display_width(value) <= max {
        return value.to_string();
    }
    let mut width = 0usize;
    let mut output = String::new();
    let ellipsis_width = 1usize;
    for ch in value.chars() {
        let char_width = display_width(&ch.to_string());
        if width + char_width + ellipsis_width > max {
            break;
        }
        output.push(ch);
        width += char_width;
    }
    output.push('…');
    output
}

pub(crate) fn display_width(value: &str) -> usize {
    value
        .chars()
        .map(|ch| match ch {
            '\u{1100}'..='\u{115F}'
            | '\u{2329}'..='\u{232A}'
            | '\u{2E80}'..='\u{A4CF}'
            | '\u{AC00}'..='\u{D7A3}'
            | '\u{F900}'..='\u{FAFF}'
            | '\u{FE10}'..='\u{FE19}'
            | '\u{FE30}'..='\u{FE6F}'
            | '\u{FF00}'..='\u{FF60}'
            | '\u{FFE0}'..='\u{FFE6}' => 2,
            _ => 1,
        })
        .sum()
}

pub(crate) fn pad(value: &str, width: usize) -> String {
    let value = truncate(value, width);
    let len = display_width(&value);
    if len >= width {
        value
    } else {
        format!("{value}{}", " ".repeat(width - len))
    }
}

pub(crate) struct Field {
    pub(crate) label: &'static str,
    pub(crate) value: String,
    textarea: bool,
    /// 预设对话列表:Enter 进入列表式子编辑器而不是 $EDITOR(验收 #19),
    /// value 仍是 `user:`/`assistant:` 行格式的序列化文本。
    dialog_list: bool,
    sensitive: bool,
    boolean: bool,
    modalities: bool,
    pub(crate) choices: Vec<String>,
    pub(crate) empty_choice_label: &'static str,
    pub(crate) raw_choice_labels: bool,
}

impl Field {
    pub(crate) fn new(label: &'static str, value: String) -> Self {
        Self {
            label,
            value,
            textarea: false,
            dialog_list: false,
            sensitive: false,
            boolean: false,
            modalities: false,
            choices: Vec::new(),
            empty_choice_label: t("Use current provider", "使用当前 Provider"),
            raw_choice_labels: false,
        }
    }

    pub(crate) fn boolean(label: &'static str, value: bool) -> Self {
        Self {
            label,
            value: value.to_string(),
            textarea: false,
            dialog_list: false,
            sensitive: false,
            boolean: true,
            modalities: false,
            choices: Vec::new(),
            empty_choice_label: t("Use current provider", "使用当前 Provider"),
            raw_choice_labels: false,
        }
    }

    pub(crate) fn textarea(label: &'static str, value: String) -> Self {
        Self {
            label,
            value,
            textarea: true,
            dialog_list: false,
            sensitive: false,
            boolean: false,
            modalities: false,
            choices: Vec::new(),
            empty_choice_label: t("Use current provider", "使用当前 Provider"),
            raw_choice_labels: false,
        }
    }

    pub(crate) fn dialog_list(label: &'static str, value: String) -> Self {
        Self {
            dialog_list: true,
            ..Self::textarea(label, value)
        }
    }

    pub(crate) fn choices(mut self, choices: &[&str]) -> Self {
        self.choices = choices.iter().map(|item| item.to_string()).collect();
        self
    }

    pub(crate) fn sensitive(mut self) -> Self {
        self.sensitive = true;
        self
    }

    pub(crate) fn modalities(label: &'static str, value: String) -> Self {
        Self {
            label,
            value,
            textarea: false,
            dialog_list: false,
            sensitive: false,
            boolean: false,
            modalities: true,
            choices: Vec::new(),
            empty_choice_label: t("Use current provider", "使用当前 Provider"),
            raw_choice_labels: false,
        }
    }

    pub(crate) fn choices_owned(mut self, choices: Vec<String>) -> Self {
        self.choices = choices;
        self
    }

    pub(crate) fn empty_choice_label(mut self, label: &'static str) -> Self {
        self.empty_choice_label = label;
        self
    }

    pub(crate) fn raw_choice_labels(mut self) -> Self {
        self.raw_choice_labels = true;
        self
    }
}
