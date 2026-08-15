//! plugins — 自 src/config_tui.rs 拆分。

pub(crate) use super::*;

impl PersonaMenuTarget {
    pub(crate) fn custom_offset(&self) -> usize {
        match self {
            Self::Global => 1,
            Self::Platform(_) => 2,
        }
    }

    pub(crate) fn is_gqy(&self, config: &AppConfig) -> bool {
        match self {
            Self::Global => config.prompt.active_persona.trim().is_empty(),
            Self::Platform(persona) => matches!(persona, PlatformPersonaOverride::GQY),
        }
    }

    pub(crate) fn custom_name<'a>(&'a self, config: &'a AppConfig) -> Option<&'a str> {
        match self {
            Self::Global => (!config.prompt.active_persona.trim().is_empty())
                .then_some(config.prompt.active_persona.as_str()),
            Self::Platform(persona) => persona.custom_name(),
        }
    }

    pub(crate) fn activate_inherit(&mut self) {
        if let Self::Platform(persona) = self {
            *persona = PlatformPersonaOverride::Inherit;
        }
    }

    pub(crate) fn activate_gqy(&mut self, config: &mut AppConfig) {
        match self {
            Self::Global => config.prompt.active_persona.clear(),
            Self::Platform(persona) => *persona = PlatformPersonaOverride::GQY,
        }
    }

    pub(crate) fn activate_custom(&mut self, config: &mut AppConfig, name: String) {
        match self {
            Self::Global => config.prompt.active_persona = name,
            Self::Platform(persona) => *persona = PlatformPersonaOverride::Custom { name },
        }
    }

    pub(crate) fn rename_custom(&mut self, old_name: &str, new_name: &str) {
        if let Self::Platform(persona) = self {
            if persona.custom_name() == Some(old_name) {
                *persona = PlatformPersonaOverride::Custom {
                    name: new_name.to_string(),
                };
            }
        }
    }

    pub(crate) fn pending_reference_count(&self, name: &str) -> usize {
        usize::from(matches!(
            self,
            Self::Platform(PlatformPersonaOverride::Custom { name: current }) if current == name
        ))
    }

    pub(crate) fn into_platform(self) -> Option<PlatformPersonaOverride> {
        match self {
            Self::Global => None,
            Self::Platform(persona) => Some(persona),
        }
    }
}

pub(crate) fn manage_personas(
    stdout: &mut io::Stdout,
    paths: &GQYPaths,
    config: &mut AppConfig,
    mut target: PersonaMenuTarget,
) -> Result<Option<PlatformPersonaOverride>> {
    std::fs::create_dir_all(config.prompts_dir_path(paths))?;
    let mut selected = 0usize;
    loop {
        let personas = list_personas(paths, config)?;
        let custom_offset = target.custom_offset();
        let mut options = Vec::with_capacity(personas.len() + custom_offset);
        if let PersonaMenuTarget::Platform(persona) = &target {
            options.push(format!(
                "{}{}",
                if persona.is_inherit() { "* " } else { "  " },
                t("Inherit current persona", "继承当前人格")
            ));
        }
        options.push(format!(
            "{}GQY",
            if target.is_gqy(config) { "* " } else { "  " }
        ));
        options.extend(personas.iter().map(|name| {
            let display = persona_display_name(name);
            if target.custom_name(config) == Some(name.as_str()) {
                format!("* {display}")
            } else {
                format!("  {display}")
            }
        }));
        selected = selected.min(options.len().saturating_sub(1));
        draw_menu(
            stdout,
            match &target {
                PersonaMenuTarget::Global => t(" AI PERSONA ", " AI 人格 "),
                PersonaMenuTarget::Platform(_) => {
                    t(" QQ CONVERSATION PERSONA ", " QQ 会话 AI 人格 ")
                }
            },
            &options,
            selected,
            t(
                "[Tab]activate [Enter]edit [a]add [d]delete [j/k]move [q]back",
                "[Tab]激活 [Enter]编辑 [a]新增 [d]删除 [j/k]移动 [q]返回",
            ),
        )?;
        match read_key()? {
            KeyCode::Esc | KeyCode::Char('q') => return Ok(target.into_platform()),
            KeyCode::Up | KeyCode::Char('k') => selected = selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => selected = (selected + 1).min(options.len() - 1),
            KeyCode::Tab => {
                if matches!(&target, PersonaMenuTarget::Platform(_)) && selected == 0 {
                    target.activate_inherit();
                } else if selected + 1 == custom_offset {
                    target.activate_gqy(config);
                } else if let Some(name) = personas.get(selected.saturating_sub(custom_offset)) {
                    target.activate_custom(config, name.clone());
                }
            }
            KeyCode::Char('a') => {
                if let Some(name) = new_persona(stdout, paths, config)? {
                    target.activate_custom(config, name);
                }
            }
            KeyCode::Enter if selected >= custom_offset => {
                if let Some(name) = personas.get(selected - custom_offset) {
                    if let Some(values) = edit_persona(stdout, paths, config, name)? {
                        apply_persona_edit(paths, config, name, &values.name, &values.content)?;
                        write_persona_aux(
                            paths,
                            config,
                            &crate::config::persona_scope_name(&values.name),
                            &values.hint,
                            &values.dialogs,
                        )?;
                        target.rename_custom(name, &values.name);
                    }
                }
            }
            // 默认 GQY 人格本体只读,但防失忆提示与预设对话是独立文件
            // (hints/default.md、dialogs/default.md),回车打开精简表单。
            KeyCode::Enter if selected + 1 == custom_offset => {
                edit_gqy_persona_extras(stdout, paths, config)?;
            }
            KeyCode::Char('d') if selected >= custom_offset => {
                if let Some(name) = personas.get(selected - custom_offset) {
                    let persisted = AppConfig::load_or_default(paths)?;
                    let references = config
                        .platforms
                        .persona_reference_count(name)
                        .max(persisted.platforms.persona_reference_count(name))
                        .max(target.pending_reference_count(name));
                    if references > 0 {
                        message(
                            stdout,
                            &if is_zh() {
                                format!(
                                    "该人格仍被 {references} 个 QQ 会话配置引用，请先解除引用。"
                                )
                            } else {
                                format!(
                                    "This persona is still used by {references} QQ conversation configuration(s). Remove those references first."
                                )
                            },
                        )?;
                        continue;
                    }
                    apply_persona_delete(paths, config, persisted, name)?;
                    selected = selected.saturating_sub(1);
                }
            }
            _ => {}
        }
    }
}

pub(crate) fn apply_persona_edit(
    paths: &GQYPaths,
    config: &mut AppConfig,
    old_name: &str,
    new_name: &str,
    content: &str,
) -> Result<()> {
    ensure_persona_name_available(paths, config, new_name, Some(old_name))?;
    if old_name == new_name {
        return write_persona(paths, config, new_name, content);
    }

    let old_path = config.persona_path(paths, old_name);
    let new_path = config.persona_path(paths, new_name);
    let old_content = std::fs::read(&old_path)?;
    let mut persisted = AppConfig::load_or_default(paths)?;
    let state = crate::state::StateStore::new(paths)?;
    write_persona(paths, config, new_name, content)?;
    if let Err(error) = move_persona_scope(paths, config, old_name, new_name) {
        let _ = std::fs::remove_file(&new_path);
        return Err(error);
    }

    let old_scope = crate::config::persona_scope_name(old_name);
    let new_scope = crate::config::persona_scope_name(new_name);
    if let Err(error) = state.rename_persona_scope(&old_scope, &new_scope) {
        let _ = move_persona_scope(paths, config, new_name, old_name);
        let _ = std::fs::remove_file(&new_path);
        return Err(error);
    }
    if let Err(error) = std::fs::remove_file(&old_path) {
        let _ = state.rename_persona_scope(&new_scope, &old_scope);
        let _ = move_persona_scope(paths, config, new_name, old_name);
        let _ = std::fs::remove_file(&new_path);
        return Err(error.into());
    }

    persisted
        .platforms
        .rename_persona_references(old_name, new_name);
    if persisted.prompt.active_persona == old_name {
        persisted.prompt.active_persona = new_name.to_string();
    }
    if let Err(error) = persisted.save(paths) {
        let _ = std::fs::write(&old_path, old_content);
        let _ = std::fs::remove_file(&new_path);
        let _ = state.rename_persona_scope(&new_scope, &old_scope);
        let _ = move_persona_scope(paths, config, new_name, old_name);
        return Err(error);
    }

    config
        .platforms
        .rename_persona_references(old_name, new_name);
    if config.prompt.active_persona == old_name {
        config.prompt.active_persona = new_name.to_string();
    }
    Ok(())
}

pub(crate) fn apply_persona_delete(
    paths: &GQYPaths,
    config: &mut AppConfig,
    mut persisted: AppConfig,
    name: &str,
) -> Result<()> {
    if persisted.prompt.active_persona == name {
        persisted.prompt.active_persona.clear();
        persisted.save(paths)?;
    }
    let scope = crate::config::persona_scope_name(name);
    crate::state::StateStore::new(paths)?.delete_persona_scope(&scope)?;
    let path = config.persona_path(paths, name);
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    remove_persona_scope(paths, config, name)?;
    if config.prompt.active_persona == name {
        config.prompt.active_persona.clear();
    }
    Ok(())
}

pub(crate) struct PersonaFormValues {
    name: String,
    content: String,
    hint: String,
    dialogs: String,
}

/// 人格附属文件现值:防失忆提示(hints/<scope>.md)与预设对话
/// (dialogs/<scope>.md)。
pub(crate) fn persona_aux_values(
    paths: &GQYPaths,
    config: &AppConfig,
    scope: &str,
) -> (String, String) {
    let hint = std::fs::read_to_string(crate::persona_hint::manual_hint_path(config, paths, scope))
        .map(|text| text.trim().to_string())
        .unwrap_or_default();
    let dialogs = crate::persona_hint::dialogs_raw(config, paths, scope);
    (hint, dialogs)
}

/// 附属文件落盘:非空写入,空则删除(清空提示=回到自动蒸馏,清空
/// 对话=不注入)。
pub(crate) fn write_persona_aux(
    paths: &GQYPaths,
    config: &AppConfig,
    scope: &str,
    hint: &str,
    dialogs: &str,
) -> Result<()> {
    let targets = [
        (
            crate::persona_hint::manual_hint_path(config, paths, scope),
            hint,
        ),
        (
            crate::persona_hint::dialogs_path(config, paths, scope),
            dialogs,
        ),
    ];
    for (path, value) in targets {
        let value = value.trim();
        if value.is_empty() {
            if path.exists() {
                std::fs::remove_file(&path)?;
            }
        } else {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&path, format!("{value}\n"))?;
        }
    }
    Ok(())
}

pub(crate) fn persona_aux_fields(hint: String, dialogs: String, gqy: bool) -> Vec<Field> {
    let (hint_label, dialogs_label) = if gqy {
        (
            t(
                "Anti-amnesia reminder (empty = built-in default)",
                "防失忆提示(清空=恢复内置默认)",
            ),
            t(
                "Preset dialogs (Enter = list editor; empty = built-in default)",
                "预设对话(回车进列表编辑,清空=恢复内置默认)",
            ),
        )
    } else {
        (
            t(
                "Anti-amnesia reminder (empty = auto distill)",
                "防失忆提示(留空=自动蒸馏)",
            ),
            t(
                "Preset dialogs (Enter = list editor)",
                "预设对话(回车进列表编辑)",
            ),
        )
    };
    vec![
        Field::textarea(hint_label, hint),
        Field::dialog_list(dialogs_label, dialogs),
    ]
}

pub(crate) fn new_persona(
    stdout: &mut io::Stdout,
    paths: &GQYPaths,
    config: &AppConfig,
) -> Result<Option<String>> {
    let mut fields = vec![
        Field::new(t("Name", "名称"), String::new()),
        Field::textarea(t("Content", "内容"), String::new()),
    ];
    fields.extend(persona_aux_fields(String::new(), String::new(), false));
    if !run_form(stdout, t(" NEW PERSONA ", " 新建人格 "), &mut fields)? {
        return Ok(None);
    }
    let name = sanitize_persona_name(&fields[0].value)?;
    ensure_persona_name_available(paths, config, &name, None)?;
    write_persona(paths, config, &name, &fields[1].value)?;
    write_persona_aux(
        paths,
        config,
        &crate::config::persona_scope_name(&name),
        &fields[2].value,
        &fields[3].value,
    )?;
    Ok(Some(name))
}

pub(crate) fn edit_persona(
    stdout: &mut io::Stdout,
    paths: &GQYPaths,
    config: &AppConfig,
    current_name: &str,
) -> Result<Option<PersonaFormValues>> {
    let content = read_persona(paths, config, current_name)?;
    let (hint, dialogs) = persona_aux_values(
        paths,
        config,
        &crate::config::persona_scope_name(current_name),
    );
    let mut fields = vec![
        Field::new(
            t("Name", "名称"),
            persona_display_name(current_name).to_string(),
        ),
        Field::textarea(t("Content", "内容"), content),
    ];
    fields.extend(persona_aux_fields(hint, dialogs, false));
    if !run_form(stdout, t(" EDIT PERSONA ", " 编辑人格 "), &mut fields)? {
        return Ok(None);
    }
    let name = sanitize_persona_name(&fields[0].value)?;
    Ok(Some(PersonaFormValues {
        name,
        content: fields[1].value.clone(),
        hint: fields[2].value.clone(),
        dialogs: fields[3].value.clone(),
    }))
}

/// 默认 GQY 人格:本体只读,回车只编辑附属的防失忆提示与预设对话
/// (scope 固定为 default)。
pub(crate) fn edit_gqy_persona_extras(
    stdout: &mut io::Stdout,
    paths: &GQYPaths,
    config: &AppConfig,
) -> Result<()> {
    let (hint, dialogs) = crate::persona_hint::gqy_aux_prefill(config, paths);
    let mut fields = persona_aux_fields(hint, dialogs, true);
    if !run_form(stdout, t(" GQY EXTRAS ", " GQY 人格附加 "), &mut fields)? {
        return Ok(());
    }
    write_persona_aux(paths, config, "default", &fields[0].value, &fields[1].value)
}

pub(crate) fn ensure_persona_name_available(
    paths: &GQYPaths,
    config: &AppConfig,
    candidate: &str,
    current: Option<&str>,
) -> Result<()> {
    let candidate_scope = crate::config::persona_scope_name(candidate);
    for existing in list_personas(paths, config)? {
        if current == Some(existing.as_str()) {
            continue;
        }
        if existing == candidate {
            bail!(
                "{}",
                t(
                    "A persona with this name already exists.",
                    "同名人格已存在。"
                )
            );
        }
        if crate::config::persona_scope_name(&existing) == candidate_scope {
            bail!(
                "{}",
                t(
                    "This persona name conflicts with another persona's persistent scope.",
                    "该人格名称与另一个人格的持久化作用域冲突。",
                )
            );
        }
    }
    Ok(())
}

pub(crate) fn move_persona_scope(
    paths: &GQYPaths,
    config: &AppConfig,
    old_name: &str,
    new_name: &str,
) -> Result<()> {
    if old_name == new_name
        || crate::config::persona_scope_name(old_name)
            == crate::config::persona_scope_name(new_name)
    {
        return Ok(());
    }
    let moves = [
        (
            config.persona_memory_data_dir(paths, old_name),
            config.persona_memory_data_dir(paths, new_name),
        ),
        (
            config.persona_memory_state_dir(paths, old_name),
            config.persona_memory_state_dir(paths, new_name),
        ),
        (
            config.persona_skills_dir(paths, old_name),
            config.persona_skills_dir(paths, new_name),
        ),
    ];
    if let Some((_, target)) = moves
        .iter()
        .find(|(source, target)| source.exists() && target.exists())
    {
        bail!(
            "persona scope destination already exists: {}",
            target.display()
        );
    }
    let mut completed = Vec::new();
    for (source, target) in moves {
        if let Err(error) = move_dir_if_exists(source.clone(), target.clone()) {
            for (from, to) in completed.into_iter().rev() {
                let _ = move_dir_if_exists(to, from);
            }
            return Err(error);
        }
        if target.exists() {
            completed.push((source, target));
        }
    }
    let old_scope = crate::config::persona_scope_name(old_name);
    let new_scope = crate::config::persona_scope_name(new_name);
    let file_moves = [
        (
            crate::persona_hint::manual_hint_path(config, paths, &old_scope),
            crate::persona_hint::manual_hint_path(config, paths, &new_scope),
        ),
        (
            crate::persona_hint::dialogs_path(config, paths, &old_scope),
            crate::persona_hint::dialogs_path(config, paths, &new_scope),
        ),
    ];
    for (source, target) in file_moves {
        if source.exists() && !target.exists() {
            std::fs::rename(&source, &target)?;
        }
    }
    Ok(())
}

pub(crate) fn remove_persona_scope(paths: &GQYPaths, config: &AppConfig, name: &str) -> Result<()> {
    remove_dir_if_exists(config.persona_memory_data_dir(paths, name))?;
    remove_dir_if_exists(config.persona_memory_state_dir(paths, name))?;
    remove_dir_if_exists(config.persona_skills_dir(paths, name))?;
    let scope = crate::config::persona_scope_name(name);
    for path in [
        crate::persona_hint::manual_hint_path(config, paths, &scope),
        crate::persona_hint::dialogs_path(config, paths, &scope),
    ] {
        if path.exists() {
            std::fs::remove_file(path)?;
        }
    }
    Ok(())
}

pub(crate) fn move_dir_if_exists(from: PathBuf, to: PathBuf) -> Result<()> {
    if !from.exists() {
        return Ok(());
    }
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::rename(from, to)?;
    Ok(())
}

pub(crate) fn remove_dir_if_exists(path: PathBuf) -> Result<()> {
    if path.exists() {
        std::fs::remove_dir_all(path)?;
    }
    Ok(())
}

pub(crate) fn edit_identities(
    stdout: &mut io::Stdout,
    paths: &GQYPaths,
    config: &mut AppConfig,
) -> Result<()> {
    std::fs::create_dir_all(config.identities_dir_path(paths))?;
    let mut selected = 0usize;
    loop {
        let identities = list_identities(paths, config)?;
        let mut options = Vec::with_capacity(identities.len() + 1);
        let default_marker = if config.prompt.active_identity.trim().is_empty() {
            "* "
        } else {
            "  "
        };
        options.push(format!(
            "{default_marker}{}",
            t("Do not use a user identity", "不使用用户身份")
        ));
        options.extend(identities.iter().map(|name| {
            let display = persona_display_name(name);
            if *name == config.prompt.active_identity {
                format!("* {display}")
            } else {
                format!("  {display}")
            }
        }));
        selected = selected.min(options.len().saturating_sub(1));
        draw_menu(
            stdout,
            t(" USER IDENTITY ", " 用户身份 "),
            &options,
            selected,
            t(
                "[Tab]activate [Enter]edit [a]add [d]delete [j/k]move [q]back",
                "[Tab]激活 [Enter]编辑 [a]新增 [d]删除 [j/k]移动 [q]返回",
            ),
        )?;
        match read_key()? {
            KeyCode::Esc | KeyCode::Char('q') => return Ok(()),
            KeyCode::Up | KeyCode::Char('k') => selected = selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => selected = (selected + 1).min(options.len() - 1),
            KeyCode::Tab => {
                config.prompt.active_identity = if selected == 0 {
                    String::new()
                } else {
                    identities.get(selected - 1).cloned().unwrap_or_default()
                };
            }
            KeyCode::Char('a') => {
                if let Some(name) = new_identity(stdout, paths, config)? {
                    config.prompt.active_identity = name;
                }
            }
            KeyCode::Enter if selected > 0 => {
                if let Some(name) = identities.get(selected - 1) {
                    if let Some(new_name) = edit_identity(stdout, paths, config, name)? {
                        if config.prompt.active_identity == *name {
                            config.prompt.active_identity = new_name;
                        }
                    }
                }
            }
            KeyCode::Char('d') if selected > 0 => {
                if let Some(name) = identities.get(selected - 1) {
                    let path = config.identity_path(paths, name);
                    if path.exists() {
                        std::fs::remove_file(path)?;
                    }
                    if config.prompt.active_identity == *name {
                        config.prompt.active_identity.clear();
                    }
                    selected = selected.saturating_sub(1);
                }
            }
            _ => {}
        }
    }
}

pub(crate) fn new_identity(
    stdout: &mut io::Stdout,
    paths: &GQYPaths,
    config: &AppConfig,
) -> Result<Option<String>> {
    edit_prompt_file_form(
        stdout,
        t(" NEW IDENTITY ", " 新建用户身份 "),
        None,
        String::new(),
        |name, content| write_identity(paths, config, name, content),
    )
}

pub(crate) fn edit_identity(
    stdout: &mut io::Stdout,
    paths: &GQYPaths,
    config: &AppConfig,
    current_name: &str,
) -> Result<Option<String>> {
    let content = read_identity(paths, config, current_name)?;
    edit_prompt_file_form(
        stdout,
        t(" EDIT IDENTITY ", " 编辑用户身份 "),
        Some(current_name),
        content,
        |name, content| {
            if name != current_name {
                let old_path = config.identity_path(paths, current_name);
                if old_path.exists() {
                    std::fs::remove_file(old_path)?;
                }
            }
            write_identity(paths, config, name, content)
        },
    )
}

pub(crate) fn list_identities(paths: &GQYPaths, config: &AppConfig) -> Result<Vec<String>> {
    list_markdown_files(&config.identities_dir_path(paths))
}

pub(crate) fn read_identity(paths: &GQYPaths, config: &AppConfig, name: &str) -> Result<String> {
    let path = config.identity_path(paths, name);
    if path.exists() {
        Ok(std::fs::read_to_string(path)?)
    } else {
        Ok(String::new())
    }
}

pub(crate) fn write_identity(
    paths: &GQYPaths,
    config: &AppConfig,
    name: &str,
    content: &str,
) -> Result<()> {
    let path = config.identity_path(paths, name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, format_text_file(content))?;
    Ok(())
}

pub(crate) fn edit_prompt_file_form<F>(
    stdout: &mut io::Stdout,
    title: &str,
    current_name: Option<&str>,
    content: String,
    write: F,
) -> Result<Option<String>>
where
    F: FnOnce(&str, &str) -> Result<()>,
{
    let Some((name, content)) = edit_prompt_file_values(stdout, title, current_name, content)?
    else {
        return Ok(None);
    };
    write(&name, &content)?;
    Ok(Some(name))
}

pub(crate) fn edit_prompt_file_values(
    stdout: &mut io::Stdout,
    title: &str,
    current_name: Option<&str>,
    content: String,
) -> Result<Option<(String, String)>> {
    let mut fields = vec![
        Field::new(
            t("Name", "名称"),
            current_name
                .map(persona_display_name)
                .unwrap_or("")
                .to_string(),
        ),
        Field::textarea(t("Content", "内容"), content),
    ];
    if !run_form(stdout, title, &mut fields)? {
        return Ok(None);
    }
    let name = sanitize_persona_name(&fields[0].value)?;
    Ok(Some((name, fields[1].value.clone())))
}

pub(crate) fn list_personas(paths: &GQYPaths, config: &AppConfig) -> Result<Vec<String>> {
    let mut names = list_markdown_files(&config.prompts_dir_path(paths))?;
    names.retain(|name| !name.eq_ignore_ascii_case("system-prompt.md"));
    Ok(names)
}

pub(crate) fn list_markdown_files(dir: &std::path::Path) -> Result<Vec<String>> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut names = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_type()?.is_file() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.ends_with(".md") {
                names.push(name);
            }
        }
    }
    names.sort();
    Ok(names)
}

pub(crate) fn read_persona(paths: &GQYPaths, config: &AppConfig, name: &str) -> Result<String> {
    let path = config.persona_path(paths, name);
    if path.exists() {
        Ok(std::fs::read_to_string(path)?)
    } else {
        Ok(String::new())
    }
}

pub(crate) fn write_persona(
    paths: &GQYPaths,
    config: &AppConfig,
    name: &str,
    content: &str,
) -> Result<()> {
    let path = config.persona_path(paths, name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, format_text_file(content))?;
    Ok(())
}

pub(crate) fn sanitize_persona_name(value: &str) -> Result<String> {
    let mut name = value
        .trim()
        .trim_end_matches(".md")
        .replace(['/', '\\'], "-");
    if name.is_empty() {
        bail!("{}", t("Persona name cannot be empty", "人格名称不能为空"));
    }
    name.push_str(".md");
    if name.eq_ignore_ascii_case("system-prompt.md") {
        bail!(
            "{}",
            t(
                "system-prompt.md is reserved",
                "system-prompt.md 是保留文件名"
            )
        );
    }
    // "dev" 是开发模式的保留人格(记忆/技能命名空间挂其名下);同名
    // 用户人格会与 dev 模式共享记忆库,必须挡在创建入口。
    if persona_display_name(&name).eq_ignore_ascii_case(crate::state::DEV_PERSONA) {
        bail!(
            "{}",
            t(
                "\"dev\" is reserved for dev mode",
                "\"dev\" 是开发模式的保留名"
            )
        );
    }
    Ok(name)
}

pub(crate) fn persona_display_name(name: &str) -> &str {
    name.strip_suffix(".md").unwrap_or(name)
}

pub(crate) fn format_text_file(content: &str) -> String {
    let content = content.trim_end();
    if content.is_empty() {
        String::new()
    } else {
        format!("{content}\n")
    }
}

pub(crate) fn parse_key_list(value: &str) -> Vec<String> {
    value
#[allow(clippy::manual_pattern_char_comparison)]
        .split(|ch| ch == ',' || ch == '\n' || ch == '\r')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_string)
        .collect()
}

pub(crate) struct ProviderBrowser<'a> {
    paths: &'a GQYPaths,
    config: &'a mut AppConfig,
    thinking_variants: &'a mut ThinkingVariantPreferences,
    active_col: usize,
    provider_idx: usize,
    provider_scroll: usize,
    org_idx: usize,
    org_scroll: usize,
    model_idx: usize,
    model_scroll: usize,
    filter: String,
    filter_mode: bool,
    raw_models: Vec<String>,
    orgs: Vec<String>,
    models: Vec<ModelEntry>,
    status: String,
    loading: bool,
    fetch_seq: u64,
    fetch_rx: Option<Receiver<FetchResult>>,
}

impl<'a> ProviderBrowser<'a> {
    pub(crate) fn new(
        paths: &'a GQYPaths,
        config: &'a mut AppConfig,
        thinking_variants: &'a mut ThinkingVariantPreferences,
    ) -> Self {
        Self {
            paths,
            config,
            thinking_variants,
            active_col: 0,
            provider_idx: 0,
            provider_scroll: 0,
            org_idx: 0,
            org_scroll: 0,
            model_idx: 0,
            model_scroll: 0,
            filter: String::new(),
            filter_mode: false,
            raw_models: Vec::new(),
            orgs: Vec::new(),
            models: Vec::new(),
            status: String::new(),
            loading: false,
            fetch_seq: 0,
            fetch_rx: None,
        }
    }

    pub(crate) fn run(mut self, stdout: &mut io::Stdout) -> Result<()> {
        self.refresh_models();
        loop {
            self.poll_fetch_result();
            self.draw(stdout)?;
            match read_key_with_timeout(if self.loading {
                Some(Duration::from_millis(100))
            } else {
                None
            })? {
                None => continue,
                Some(key) => match key {
                    key if self.filter_mode => self.handle_filter_key(key),
                    KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                    KeyCode::Left | KeyCode::Char('h') => self.move_left(),
                    KeyCode::Right | KeyCode::Char('l') => self.move_right(),
                    KeyCode::Up | KeyCode::Char('k') => self.move_up(),
                    KeyCode::Down | KeyCode::Char('j') => self.move_down(),
                    KeyCode::Char('/') => {
                        self.filter_mode = true;
                        self.filter.clear();
                        self.rebuild_models();
                    }
                    KeyCode::Char('r') => self.refresh_models(),
                    KeyCode::Char('a') => self.add_provider(stdout)?,
                    KeyCode::Char('d') => self.delete_provider(),
                    KeyCode::Tab if self.active_col == 2 => self.toggle_model_activation(),
                    KeyCode::Enter | KeyCode::Char('i') => self.select_or_edit(stdout)?,
                    _ => {}
                },
            }
        }
    }

    pub(crate) fn handle_filter_key(&mut self, key: KeyCode) {
        match key {
            KeyCode::Esc => {
                self.filter_mode = false;
                self.filter.clear();
            }
            KeyCode::Enter => self.filter_mode = false,
            KeyCode::Backspace => {
                self.filter.pop();
            }
            KeyCode::Char(ch) => self.filter.push(ch),
            _ => {}
        }
        self.rebuild_models();
    }

    pub(crate) fn move_left(&mut self) {
        self.active_col = self.active_col.saturating_sub(1);
    }

    pub(crate) fn move_right(&mut self) {
        self.active_col = (self.active_col + 1).min(2);
    }

    pub(crate) fn move_up(&mut self) {
        match self.active_col {
            0 => {
                self.provider_idx = self.provider_idx.saturating_sub(1);
                self.provider_scroll = column_scroll(
                    self.provider_idx,
                    self.provider_scroll,
                    column_visible_rows(),
                );
                self.refresh_models();
            }
            1 => {
                self.org_idx = self.org_idx.saturating_sub(1);
                self.org_scroll =
                    column_scroll(self.org_idx, self.org_scroll, column_visible_rows());
                self.rebuild_models();
            }
            2 => {
                self.model_idx = self.model_idx.saturating_sub(1);
                self.model_scroll =
                    column_scroll(self.model_idx, self.model_scroll, column_visible_rows());
            }
            _ => {}
        }
    }

    pub(crate) fn move_down(&mut self) {
        match self.active_col {
            0 => {
                self.provider_idx =
                    (self.provider_idx + 1).min(self.config.providers.len().saturating_sub(1));
                self.provider_scroll = column_scroll(
                    self.provider_idx,
                    self.provider_scroll,
                    column_visible_rows(),
                );
                self.refresh_models();
            }
            1 => {
                self.org_idx = (self.org_idx + 1).min(self.orgs.len().saturating_sub(1));
                self.org_scroll =
                    column_scroll(self.org_idx, self.org_scroll, column_visible_rows());
                self.rebuild_models();
            }
            2 => {
                self.model_idx = (self.model_idx + 1).min(self.models.len().saturating_sub(1));
                self.model_scroll =
                    column_scroll(self.model_idx, self.model_scroll, column_visible_rows());
            }
            _ => {}
        }
    }

    pub(crate) fn refresh_models(&mut self) {
        self.provider_idx = self
            .provider_idx
            .min(self.config.providers.len().saturating_sub(1));
        self.raw_models.clear();
        self.orgs = vec!["All".to_string()];
        self.models.clear();
        self.fetch_seq += 1;
        if let Some(provider) = self.config.providers.get(self.provider_idx).cloned() {
            let seq = self.fetch_seq;
            let (tx, rx) = mpsc::channel();
            self.fetch_rx = Some(rx);
            self.loading = true;
            self.status = t("Fetching model list...", "正在获取模型列表...").to_string();
            std::thread::spawn(move || {
                let result = fetch_models(&provider).map_err(|err| err.to_string());
                let _ = tx.send((seq, result));
            });
        } else {
            self.fetch_rx = None;
            self.loading = false;
            self.status.clear();
        }
        self.org_idx = 0;
        self.model_idx = 0;
        self.org_scroll = 0;
        self.model_scroll = 0;
    }

    pub(crate) fn poll_fetch_result(&mut self) {
        let Some(rx) = &self.fetch_rx else {
            return;
        };
        let Ok((seq, result)) = rx.try_recv() else {
            return;
        };
        if seq != self.fetch_seq {
            return;
        }
        self.loading = false;
        self.fetch_rx = None;
        match result {
            Ok(models) => {
                self.status = if is_zh() {
                    format!("已获取 {} 个模型", models.len())
                } else {
                    format!("Fetched {} models", models.len())
                };
                self.raw_models = models;
            }
            Err(err) => {
                let status = if is_zh() {
                    format!("获取模型失败: {err}")
                } else {
                    format!("Failed to fetch models: {err}")
                };
                self.status = format_status_line(&status);
                self.raw_models.clear();
            }
        }
        self.rebuild_models();
    }

    pub(crate) fn rebuild_models(&mut self) {
        let filter = self.filter.to_ascii_lowercase();
        let mut grouped: BTreeMap<String, Vec<ModelEntry>> = BTreeMap::new();
        for model in &self.raw_models {
            if !filter.is_empty() && !model.to_ascii_lowercase().contains(&filter) {
                continue;
            }
            let org = model
                .split_once('/')
                .map(|(org, _)| org)
                .unwrap_or("All")
                .to_string();
            let name = model
                .split_once('/')
                .map(|(_, name)| name)
                .unwrap_or(model)
                .to_string();
            grouped
                .entry("All".to_string())
                .or_default()
                .push(ModelEntry::new(model, model));
            if org != "All" {
                grouped
                    .entry(org)
                    .or_default()
                    .push(ModelEntry::new(&name, model));
            }
        }
        self.orgs = grouped.keys().cloned().collect();
        if self.orgs.is_empty() {
            self.orgs.push("All".to_string());
        }
        self.org_idx = self.org_idx.min(self.orgs.len().saturating_sub(1));
        self.models = grouped.remove(&self.orgs[self.org_idx]).unwrap_or_default();
        self.model_idx = self.model_idx.min(self.models.len().saturating_sub(1));
        self.org_scroll = column_scroll(self.org_idx, self.org_scroll, column_visible_rows());
        self.model_scroll = column_scroll(self.model_idx, self.model_scroll, column_visible_rows());
    }

    pub(crate) fn add_provider(&mut self, stdout: &mut io::Stdout) -> Result<()> {
        if let Some(provider) = edit_provider_form(stdout, ProviderConfig::new_custom())? {
            self.config.upsert_provider(provider);
            self.provider_idx = self.config.providers.len().saturating_sub(1);
            self.refresh_models();
        }
        Ok(())
    }

    pub(crate) fn delete_provider(&mut self) {
        if self.config.providers.is_empty() {
            return;
        }
        let removed = self.config.providers.remove(self.provider_idx);
        self.config.remove_provider_references(&removed.id);
        self.provider_idx = self
            .provider_idx
            .min(self.config.providers.len().saturating_sub(1));
        self.refresh_models();
    }

    pub(crate) fn select_or_edit(&mut self, stdout: &mut io::Stdout) -> Result<()> {
        match self.active_col {
            0 => {
                if let Some(provider) = self.config.providers.get(self.provider_idx).cloned() {
                    if let Some(provider) = edit_provider_form(stdout, provider)? {
                        let old_id = self.config.providers[self.provider_idx].id.clone();
                        self.config.providers[self.provider_idx] = provider.clone();
                        if self.config.active_provider == old_id {
                            self.config.active_provider = provider.id.clone();
                        }
                        if old_id != provider.id {
                            self.config
                                .rename_provider_references(&old_id, &provider.id);
                            self.thinking_variants
                                .rename_provider(&old_id, &provider.id);
                        }
                        self.refresh_models();
                    }
                }
            }
            2 => {
                let mut model_updated = false;
                if let Some(model) = self.models.get(self.model_idx).cloned() {
                    if let Some(provider) = self.config.providers.get_mut(self.provider_idx) {
                        auto_configure_model_tags(self.paths, provider, &model.full);
                    }
                    if let Some(provider) = self.config.providers.get_mut(self.provider_idx) {
                        if edit_model_form(stdout, provider, &model.full, self.thinking_variants)? {
                            self.config.active_provider = provider.id.clone();
                            model_updated = true;
                            self.status = if is_zh() {
                                format!("已更新模型设置: {}", model.full)
                            } else {
                                format!("Updated model settings: {}", model.full)
                            };
                        }
                    }
                }
                if model_updated {
                    self.config.prune_model_references();
                }
            }
            _ => {}
        }
        Ok(())
    }

    pub(crate) fn toggle_model_activation(&mut self) {
        if self.active_col != 2 {
            return;
        }
        let mut removed = None;
        if let (Some(provider), Some(model)) = (
            self.config.providers.get_mut(self.provider_idx),
            self.models.get(self.model_idx),
        ) {
            if let Some(index) = provider.models.iter().position(|item| item == &model.full) {
                let provider_id = provider.id.clone();
                let model = model.full.clone();
                provider.models.remove(index);
                if provider.default_model == model {
                    provider.default_model = provider.models.first().cloned().unwrap_or_default();
                }
                self.status = if is_zh() {
                    format!("已取消激活模型: {model}")
                } else {
                    format!("Deactivated model: {model}")
                };
                removed = Some((provider_id, model));
            } else {
                provider.models.push(model.full.clone());
                auto_configure_model_tags(self.paths, provider, &model.full);
                if provider.default_model.trim().is_empty() {
                    provider.default_model = model.full.clone();
                }
                self.status = if is_zh() {
                    format!("已激活模型: {}", model.full)
                } else {
                    format!("Activated model: {}", model.full)
                };
            }
        }
        if let Some((provider_id, model)) = removed {
            self.config
                .remove_active_model_references(&provider_id, &model);
        }
    }

    pub(crate) fn draw(&self, stdout: &mut io::Stdout) -> Result<()> {
        let (cols, rows) = terminal::size()?;
        let inner_x = 0;
        let inner_y = 0;
        let inner_w = cols;
        let inner_h = rows.saturating_sub(2);
        let left_w = inner_w.saturating_mul(28).saturating_div(100).max(20);
        let mid_w = inner_w.saturating_mul(22).saturating_div(100).max(16);
        let right_w = inner_w
            .saturating_sub(left_w)
            .saturating_sub(mid_w)
            .saturating_sub(2)
            .max(18);
        let providers = self
            .config
            .providers
            .iter()
            .map(|provider| {
                let active = if provider.id == self.config.active_provider {
                    "* "
                } else {
                    "  "
                };
                format!("{active}{}", provider.display_name)
            })
            .collect::<Vec<_>>();
        let models = self
            .models
            .iter()
            .map(|model| {
                let active = self
                    .config
                    .providers
                    .get(self.provider_idx)
                    .map(|provider| provider.models.iter().any(|item| item == &model.full))
                    .unwrap_or(false);
                format!("{} {}", if active { "[*]" } else { "[ ]" }, model.name)
            })
            .collect::<Vec<_>>();
        let orgs = self
            .orgs
            .iter()
            .map(|org| {
                if org == "All" {
                    t("All", "全部").to_string()
                } else {
                    org.clone()
                }
            })
            .collect::<Vec<_>>();

        queue!(stdout, Clear(ClearType::All))?;
        draw_column(
            stdout,
            inner_x,
            inner_y,
            left_w,
            inner_h,
            t(" PROVIDERS ", " 供应商 "),
            &providers,
            self.provider_idx,
            self.provider_scroll,
            self.active_col == 0,
        )?;
        draw_column(
            stdout,
            inner_x + left_w + 1,
            inner_y,
            mid_w,
            inner_h,
            t(" ORGANIZATION ", " 组织 "),
            &orgs,
            self.org_idx,
            self.org_scroll,
            self.active_col == 1,
        )?;
        let title = if self.filter.is_empty() {
            t(" MODELS ", " 模型 ").to_string()
        } else if is_zh() {
            format!(" 模型 /{} ", self.filter)
        } else {
            format!(" MODELS /{} ", self.filter)
        };
        draw_column(
            stdout,
            inner_x + left_w + mid_w + 2,
            inner_y,
            right_w,
            inner_h,
            &title,
            &models,
            self.model_idx,
            self.model_scroll,
            self.active_col == 2,
        )?;
        let help = if self.filter_mode {
            if is_zh() {
                format!("搜索: {}_  [Enter]确认 [Esc]取消", self.filter)
            } else {
                format!("Search: {}_  [Enter]confirm [Esc]cancel", self.filter)
            }
        } else {
            t(
                "[h/l]column [j/k]move [Tab]activate model [Enter]model settings [/]search [r]refresh [a]add [d]delete [q]back",
                "[h/l]切栏 [j/k]移动 [Tab]激活模型 [Enter]模型设置 [/]搜索 [r]刷新 [a]添加 [d]删除 [q]返回",
            )
            .to_string()
        };
        let status = if self.loading {
#[allow(clippy::useless_format)]
            format!("{}", self.status)
        } else {
            self.status.clone()
        };
        queue!(
            stdout,
            MoveTo(0, rows.saturating_sub(2)),
            Clear(ClearType::CurrentLine),
            Print(truncate(&status, cols as usize))
        )?;
        queue!(
            stdout,
            MoveTo(0, rows.saturating_sub(1)),
            Clear(ClearType::CurrentLine),
            Print(truncate(&help, cols as usize))
        )?;
        stdout.flush()?;
        Ok(())
    }
}

type FetchResult = (u64, Result<Vec<String>, String>);

pub(crate) fn format_status_line(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[derive(Clone)]
pub(crate) struct ModelEntry {
    name: String,
    full: String,
}

impl ModelEntry {
    pub(crate) fn new(name: &str, full: &str) -> Self {
        Self {
            name: name.to_string(),
            full: full.to_string(),
        }
    }
}

pub(crate) fn fetch_models(provider: &ProviderConfig) -> Result<Vec<String>> {
    let api_key = provider.api_key.as_deref().unwrap_or_default();
    let mut api_key = if let Some(env_name) = api_key.strip_prefix("$env:") {
        std::env::var(env_name).unwrap_or_default()
    } else {
        api_key.to_string()
    };
    if api_key.is_empty() && provider.is_opencode_zen() {
        api_key = "public".to_string();
    }
    let url = models_url(&provider.base_url);
    let mut request = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(provider.timeout_seconds))
        .build()?
        .get(url)
        .header("Accept", "application/json")
        .header("User-Agent", "gqy-config");
    if !api_key.is_empty() {
        request = request.bearer_auth(api_key);
    }
    let response = request.send()?;
    let status = response.status();
    let body = response.text()?;
    if !status.is_success() {
        bail!("{status}: {body}");
    }
    let parsed: ModelsResponse = serde_json::from_str(&body)?;
    Ok(parsed
        .data
        .into_iter()
        .map(|model| model.id)
        .filter(|id| !id.is_empty())
        .collect())
}
