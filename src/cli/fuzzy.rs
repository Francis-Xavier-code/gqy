//! fuzzy — 自 src/cli.rs 拆分。

use super::*;

/// Single-select variant of the inline fuzzy menu: Tab marks a row (radio),
/// Enter confirms the marked row (or the highlighted one when nothing is
/// marked); Esc / q cancels.
pub(crate) fn inline_fuzzy_select_single(
    items: &[String],
    initial: usize,
) -> Result<Option<usize>> {
    let mut active = vec![false; items.len()];
    if let Some(slot) = active.get_mut(initial) {
        *slot = true;
    }
    let menu_lines = inline_fuzzy_lines(items.len());
    reserve_inline_fuzzy_space(menu_lines)?;
    let mut session = InlineRawMode::start()?;
    let matcher = SkimMatcherV2::default();
    let mut query = String::new();
    let mut selected = 0usize;
    let mut scroll = 0usize;
    let (_, cursor_y) = cursor::position().unwrap_or((0, menu_lines.saturating_sub(1)));
    let anchor_y = cursor_y.saturating_sub(menu_lines.saturating_sub(1));
    loop {
        let matches = fuzzy_matches(&matcher, items, &query);
        if selected >= matches.len() {
            selected = matches.len().saturating_sub(1);
        }
        let visible = matches.len().min(menu_lines.saturating_sub(2) as usize);
        scroll = inline_fuzzy_scroll(selected, scroll, visible);
        draw_inline_fuzzy(
            &mut session.stdout,
            anchor_y,
            menu_lines,
            &query,
            items,
            &matches,
            selected,
            scroll,
            &active,
        )?;
        if let Event::Key(KeyEvent {
            code, modifiers, ..
        }) = event::read()?
        {
            match code {
                KeyCode::Char('c')
                    if modifiers.contains(KeyModifiers::CONTROL)
                        && !modifiers.contains(KeyModifiers::SHIFT) =>
                {
                    clear_inline_fuzzy(&mut session.stdout, anchor_y, menu_lines)?;
                    return Ok(None);
                }
                KeyCode::Esc => {
                    clear_inline_fuzzy(&mut session.stdout, anchor_y, menu_lines)?;
                    return Ok(None);
                }
                KeyCode::Char('q') if query.is_empty() => {
                    clear_inline_fuzzy(&mut session.stdout, anchor_y, menu_lines)?;
                    return Ok(None);
                }
                KeyCode::Enter => {
                    clear_inline_fuzzy(&mut session.stdout, anchor_y, menu_lines)?;
                    let marked = active.iter().position(|value| *value);
                    let fallback = matches.get(selected).map(|(_, index)| *index);
                    return Ok(marked.or(fallback));
                }
                KeyCode::Tab => {
                    if let Some((_, index)) = matches.get(selected) {
                        for slot in active.iter_mut() {
                            *slot = false;
                        }
                        if let Some(slot) = active.get_mut(*index) {
                            *slot = true;
                        }
                    }
                }
                KeyCode::Up | KeyCode::Char('k') => selected = selected.saturating_sub(1),
                KeyCode::Down | KeyCode::Char('j') => {
                    selected = (selected + 1).min(matches.len().saturating_sub(1));
                }
                KeyCode::Backspace => {
                    query.pop();
                    selected = 0;
                    scroll = 0;
                }
                KeyCode::Char(ch) if !modifiers.contains(KeyModifiers::CONTROL) => {
                    query.push(ch);
                    selected = 0;
                    scroll = 0;
                }
                _ => {}
            }
        }
    }
}

pub(crate) fn fuzzy_matches(
    matcher: &SkimMatcherV2,
    items: &[String],
    query: &str,
) -> Vec<(i64, usize)> {
    let mut matches = items
        .iter()
        .enumerate()
        .filter_map(|(index, item)| {
            if query.trim().is_empty() {
                Some((0, index))
            } else {
                matcher.fuzzy_match(item, query).map(|score| (score, index))
            }
        })
        .collect::<Vec<_>>();
    if !query.trim().is_empty() {
        matches.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));
    }
    matches
}

pub(crate) fn draw_inline_fuzzy(
    stdout: &mut io::Stdout,
    anchor_y: u16,
    menu_lines: u16,
    query: &str,
    items: &[String],
    matches: &[(i64, usize)],
    selected: usize,
    scroll: usize,
    active: &[bool],
) -> Result<()> {
    let (cols, _) = terminal::size().unwrap_or((80, 24));
    let bar = inline_fuzzy_bar();
    let width = (cols as usize).saturating_sub(visible_width(&bar)).max(1);
    let visible = matches.len().min(menu_lines.saturating_sub(2) as usize);
    queue!(stdout, Hide)?;
    for row in 0..menu_lines {
        queue!(
            stdout,
            MoveTo(0, anchor_y + row),
            Clear(ClearType::CurrentLine)
        )?;
    }
    queue!(
        stdout,
        MoveTo(0, anchor_y),
        Print(&bar),
        Print(inline_fuzzy_header(query, width)),
    )?;
    if matches.is_empty() {
        queue!(
            stdout,
            MoveTo(0, anchor_y + 1),
            Print(&bar),
            Print(format!("\x1b[2m{}\x1b[0m", t("no matches", "没有匹配项")))
        )?;
    } else {
        for (row, (_, item_index)) in matches.iter().skip(scroll).take(visible).enumerate() {
            queue!(
                stdout,
                MoveTo(0, anchor_y + row as u16 + 1),
                Print(&bar),
                Print(inline_fuzzy_item_line(
                    items[*item_index].as_str(),
                    scroll + row == selected,
                    active.get(*item_index).copied().unwrap_or(false),
                    width
                ))
            )?;
        }
    }
    queue!(
        stdout,
        MoveTo(0, anchor_y + menu_lines.saturating_sub(1)),
        Print(&bar),
        Print(inline_fuzzy_help_line(width))
    )?;
    stdout.flush()?;
    Ok(())
}

pub(crate) fn inline_fuzzy_scroll(selected: usize, scroll: usize, visible: usize) -> usize {
    if visible == 0 || selected < scroll {
        selected
    } else if selected >= scroll + visible {
        selected + 1 - visible
    } else {
        scroll
    }
}

pub(crate) fn inline_fuzzy_bar() -> String {
    input_prompt_bar(AgentMode::Normal)
}

pub(crate) fn inline_fuzzy_header(query: &str, width: usize) -> String {
    let title = t("Select model", "选择模型");
    let line = if query.trim().is_empty() {
        title.to_string()
    } else {
        format!("{title} · {}", query.trim())
    };
    format!("\x1b[1m{}\x1b[0m", truncate_visible_width(&line, width))
}

pub(crate) fn inline_fuzzy_item_line(
    item: &str,
    selected: bool,
    active: bool,
    width: usize,
) -> String {
    let marker = if active { "[*]" } else { "[ ]" };
    let line = if selected {
        format!("› {marker} {item}")
    } else {
        format!("  {marker} {item}")
    };
    let line = truncate_visible_width(&line, width);
    if selected {
        format!(
            "\x1b[1m\x1b[35m›\x1b[0m\x1b[1m{}\x1b[0m",
            line.strip_prefix('›').unwrap_or(&line)
        )
    } else if active {
        format!("\x1b[1m\x1b[32m{}\x1b[0m", line)
    } else {
        format!("\x1b[2m{}\x1b[0m", line)
    }
}

pub(crate) fn inline_fuzzy_help_line(width: usize) -> String {
    let line = t(
        "type search · j/k move · Tab toggle · Enter/q confirm",
        "输入搜索 · j/k 移动 · Enter 选定 · Tab 多选 · q 完成",
    );
    format!("\x1b[2m{}\x1b[0m", truncate_visible_width(line, width))
}

pub(crate) fn clear_inline_fuzzy(stdout: &mut io::Stdout, anchor_y: u16, lines: u16) -> Result<()> {
    for row in 0..lines {
        queue!(
            stdout,
            MoveTo(0, anchor_y + row),
            Clear(ClearType::CurrentLine)
        )?;
    }
    queue!(stdout, MoveTo(0, anchor_y), Show)?;
    stdout.flush()?;
    Ok(())
}

pub(crate) fn reserve_inline_fuzzy_space(lines: u16) -> Result<()> {
    for _ in 1..lines {
        println!();
    }
    io::stdout().flush()?;
    Ok(())
}

pub(crate) fn inline_fuzzy_lines(item_count: usize) -> u16 {
    ((item_count.min(10) + 2) as u16).max(3)
}

/// In-place single-choice fuzzy picker; same environment and screen handling
/// as `inline_pop_select` (editor suspended, cooked mode on entry). `lines`
/// are the rendered rows and `search` the parallel fuzzy-match texts.
/// Returns the selected index, or `None` when cancelled.
/// What an inline picker returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InlineSelectOutcome {
    Cancelled,
    Chosen(usize),
    /// Ctrl+D on a row, confirmed in place. The caller does the deletion and
    /// reopens the picker on the refreshed list.
    Deleted(usize),
}

/// A picker keystroke, resolved away from the draw loop so the delete
/// confirmation flow is testable without a terminal. Every printable character
/// is search input, which is why deletion needs a modifier key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InlineSelectKey {
    Cancel,
    Accept,
    Up,
    Down,
    Backspace,
    DeleteRequest,
    Char(char),
    Ignore,
}

pub(crate) fn inline_select_key(
    code: KeyCode,
    modifiers: KeyModifiers,
    deletable: bool,
) -> InlineSelectKey {
    let control = modifiers.contains(KeyModifiers::CONTROL);
    match code {
        KeyCode::Char('c') if control => InlineSelectKey::Cancel,
        KeyCode::Char('d') if control && deletable => InlineSelectKey::DeleteRequest,
        KeyCode::Delete if deletable => InlineSelectKey::DeleteRequest,
        KeyCode::Esc => InlineSelectKey::Cancel,
        KeyCode::Enter => InlineSelectKey::Accept,
        KeyCode::Up | KeyCode::Char('k') if !control => InlineSelectKey::Up,
        KeyCode::Down | KeyCode::Char('j') if !control => InlineSelectKey::Down,
        KeyCode::Backspace => InlineSelectKey::Backspace,
        KeyCode::Char(ch) if !control => InlineSelectKey::Char(ch),
        _ => InlineSelectKey::Ignore,
    }
}

pub(crate) fn inline_single_select(
    title: &str,
    lines: &[String],
    search: &[String],
    initial_selected: usize,
) -> Result<Option<usize>> {
    match inline_single_select_deletable(title, lines, search, initial_selected, None)? {
        InlineSelectOutcome::Chosen(index) => Ok(Some(index)),
        // Unreachable without delete labels, but folding it into `None` keeps
        // callers that never opted in from having to care.
        InlineSelectOutcome::Cancelled | InlineSelectOutcome::Deleted(_) => Ok(None),
    }
}

/// Fuzzy picker. Passing `delete_labels` (one per row, used in the inline
/// confirmation) enables Ctrl+D deletion and returns `Deleted` once the user
/// confirms; the caller performs the deletion and decides whether to reopen.
pub(crate) fn inline_single_select_deletable(
    title: &str,
    lines: &[String],
    search: &[String],
    initial_selected: usize,
    delete_labels: Option<&[String]>,
) -> Result<InlineSelectOutcome> {
    let menu_lines = inline_fuzzy_lines(lines.len());
    reserve_inline_fuzzy_space(menu_lines)?;
    let mut session = InlineRawMode::start()?;
    let matcher = SkimMatcherV2::default();
    let mut query = String::new();
    let mut selected = initial_selected.min(lines.len().saturating_sub(1));
    let mut scroll = 0usize;
    let (_, cursor_y) = cursor::position().unwrap_or((0, menu_lines.saturating_sub(1)));
    let anchor_y = cursor_y.saturating_sub(menu_lines.saturating_sub(1));
    let deletable = delete_labels.is_some();
    // Index awaiting a y/N answer. Confirming inside the picker keeps the
    // drawing intact instead of tearing it down for a separate prompt.
    let mut confirming: Option<usize> = None;
    loop {
        let matches = fuzzy_matches(&matcher, search, &query);
        if selected >= matches.len() {
            selected = matches.len().saturating_sub(1);
        }
        let visible = matches.len().min(menu_lines.saturating_sub(2) as usize);
        scroll = inline_fuzzy_scroll(selected, scroll, visible);
        let confirm_label = confirming.and_then(|index| {
            delete_labels
                .and_then(|labels| labels.get(index))
                .map(String::as_str)
        });
        draw_inline_single(
            &mut session.stdout,
            anchor_y,
            menu_lines,
            title,
            &query,
            lines,
            &matches,
            selected,
            scroll,
            deletable,
            confirm_label,
        )?;
        let Event::Key(KeyEvent {
            code, modifiers, ..
        }) = event::read()?
        else {
            continue;
        };
        if let Some(index) = confirming {
            // Only an explicit yes deletes; every other key backs out.
            let confirmed = matches!(code, KeyCode::Char('y') | KeyCode::Char('Y'));
            confirming = None;
            if confirmed {
                clear_inline_fuzzy(&mut session.stdout, anchor_y, menu_lines)?;
                return Ok(InlineSelectOutcome::Deleted(index));
            }
            continue;
        }
        match inline_select_key(code, modifiers, deletable) {
            InlineSelectKey::Cancel => {
                clear_inline_fuzzy(&mut session.stdout, anchor_y, menu_lines)?;
                return Ok(InlineSelectOutcome::Cancelled);
            }
            InlineSelectKey::Accept => {
                let choice = matches.get(selected).map(|(_, index)| *index);
                clear_inline_fuzzy(&mut session.stdout, anchor_y, menu_lines)?;
                return Ok(match choice {
                    Some(index) => InlineSelectOutcome::Chosen(index),
                    None => InlineSelectOutcome::Cancelled,
                });
            }
            InlineSelectKey::DeleteRequest => {
                confirming = matches.get(selected).map(|(_, index)| *index);
            }
            InlineSelectKey::Up => selected = selected.saturating_sub(1),
            InlineSelectKey::Down => {
                selected = (selected + 1).min(matches.len().saturating_sub(1));
            }
            InlineSelectKey::Backspace => {
                query.pop();
                selected = 0;
                scroll = 0;
            }
            InlineSelectKey::Char(ch) => {
                query.push(ch);
                selected = 0;
                scroll = 0;
            }
            InlineSelectKey::Ignore => {}
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_inline_single(
    stdout: &mut io::Stdout,
    anchor_y: u16,
    menu_lines: u16,
    title: &str,
    query: &str,
    lines: &[String],
    matches: &[(i64, usize)],
    selected: usize,
    scroll: usize,
    deletable: bool,
    confirm_label: Option<&str>,
) -> Result<()> {
    let (cols, _) = terminal::size().unwrap_or((80, 24));
    let bar = inline_fuzzy_bar();
    let width = (cols as usize).saturating_sub(visible_width(&bar)).max(1);
    let visible = matches.len().min(menu_lines.saturating_sub(2) as usize);
    queue!(stdout, Hide)?;
    for row in 0..menu_lines {
        queue!(
            stdout,
            MoveTo(0, anchor_y + row),
            Clear(ClearType::CurrentLine)
        )?;
    }
    let header = match confirm_label {
        Some(label) => inline_single_confirm_header(label, width),
        None => inline_single_header(title, query, width),
    };
    queue!(stdout, MoveTo(0, anchor_y), Print(&bar), Print(header))?;
    if matches.is_empty() {
        queue!(
            stdout,
            MoveTo(0, anchor_y + 1),
            Print(&bar),
            Print(format!("\x1b[2m{}\x1b[0m", t("no matches", "没有匹配项")))
        )?;
    } else {
        for (row, (_, item_index)) in matches.iter().skip(scroll).take(visible).enumerate() {
            queue!(
                stdout,
                MoveTo(0, anchor_y + row as u16 + 1),
                Print(&bar),
                Print(inline_single_item_line(
                    lines[*item_index].as_str(),
                    scroll + row == selected,
                    width
                ))
            )?;
        }
    }
    queue!(
        stdout,
        MoveTo(0, anchor_y + menu_lines.saturating_sub(1)),
        Print(&bar),
        Print(inline_single_help_line(width, deletable))
    )?;
    stdout.flush()?;
    Ok(())
}

pub(crate) fn inline_single_header(title: &str, query: &str, width: usize) -> String {
    let line = if query.trim().is_empty() {
        title.to_string()
    } else {
        format!("{title} · {}", query.trim())
    };
    format!("\x1b[1m{}\x1b[0m", truncate_visible_width(&line, width))
}

pub(crate) fn inline_single_item_line(item: &str, selected: bool, width: usize) -> String {
    let line = if selected {
        format!("› {item}")
    } else {
        format!("  {item}")
    };
    let line = truncate_visible_width(&line, width);
    if selected {
        format!(
            "\x1b[1m\x1b[35m›\x1b[0m\x1b[1m{}\x1b[0m",
            line.strip_prefix('›').unwrap_or(&line)
        )
    } else {
        format!("\x1b[2m{line}\x1b[0m")
    }
}

pub(crate) fn inline_single_confirm_header(label: &str, width: usize) -> String {
    let line = if is_zh() {
        format!("删除「{label}」？y/N")
    } else {
        format!("delete \"{label}\"? y/N")
    };
    format!(
        "\x1b[1m\x1b[31m{}\x1b[0m",
        truncate_visible_width(&line, width)
    )
}

pub(crate) fn inline_single_help_line(width: usize, deletable: bool) -> String {
    let line = if deletable {
        t(
            "type search · j/k move · Enter select · Ctrl+D delete · Esc cancel",
            "输入搜索 · j/k 移动 · Enter 选择 · Ctrl+D 删除 · Esc 取消",
        )
    } else {
        t(
            "type search · j/k move · Enter select · Esc cancel",
            "输入搜索 · j/k 移动 · Enter 选择 · Esc 取消",
        )
    };
    format!("\x1b[2m{}\x1b[0m", truncate_visible_width(line, width))
}

pub(crate) fn truncate_display(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        value.to_string()
    } else {
        format!(
            "{}…",
            value
                .chars()
                .take(max.saturating_sub(1))
                .collect::<String>()
        )
    }
}

pub(crate) struct InlineRawMode {
    stdout: io::Stdout,
}

impl InlineRawMode {
    pub(crate) fn start() -> Result<Self> {
        terminal::enable_raw_mode()?;
        spawn_hangup_watchdog();
        Ok(Self {
            stdout: io::stdout(),
        })
    }
}

impl Drop for InlineRawMode {
    fn drop(&mut self) {
        let _ = execute!(self.stdout, Show);
        let _ = terminal::disable_raw_mode();
    }
}

pub(crate) async fn run_config(paths: &GQYPaths, args: ConfigArgs) -> Result<bool> {
    match args.command {
        Some(ConfigCommand::Validate) => {
            AppConfig::load(paths)?;
            println!(
                "{}: {}",
                t("config is valid", "配置有效"),
                paths.config_file.display()
            );
            Ok(false)
        }
        Some(ConfigCommand::Paths) => {
            paths.print();
            Ok(false)
        }
        Some(ConfigCommand::PromptSource) => {
            let config = AppConfig::load(paths)?;
            let persona = config.prompt.active_persona.trim();
            let identity = config.prompt.active_identity.trim();
            let persona_path = (!persona.is_empty()).then(|| config.persona_path(paths, persona));
            let legacy_prompt = config.custom_system_prompt(paths)?;
            let legacy_prompt_path = config.system_prompt_path(paths);
            let base_prompt_source =
                if let Some(path) = persona_path.as_ref().filter(|path| path.exists()) {
                    format!("persona ({})", path.display())
                } else if !legacy_prompt.trim().is_empty() {
                    format!("legacy_custom ({})", legacy_prompt_path.display())
                } else {
                    "built-in".to_string()
                };
            println!("base_prompt_source: {}", base_prompt_source);
            println!(
                "active_persona: {}",
                if persona.is_empty() {
                    "(none)"
                } else {
                    persona
                }
            );
            if let Some(path) = persona_path {
                println!("active_persona_file: {}", path.display());
            }
            println!(
                "active_identity: {}",
                if identity.is_empty() {
                    "(none)"
                } else {
                    identity
                }
            );
            println!("prompts_dir: {}", config.prompts_dir_path(paths).display());
            println!(
                "identities_dir: {}",
                config.identities_dir_path(paths).display()
            );
            let system_prompt = config.system_prompt(paths)?;
            println!(
                "system_prompt_first_line: {}",
                system_prompt.lines().next().unwrap_or("")
            );
            println!("system_prompt_chars: {}", system_prompt.chars().count());
            Ok(false)
        }
        None => crate::config_tui::run(paths),
    }
}

pub(crate) fn run_clipboard_paste(paths: &GQYPaths) -> Result<()> {
    match crate::clipboard::read_clipboard() {
        Ok(crate::clipboard::ClipboardContent::Image(img)) => {
            let path = img.write_temp_file(&paths.cache_dir, 0)?;
            let filename = path.file_name().and_then(|n| n.to_str()).unwrap_or("image");
            print!("[Image 1: {}]", filename);
            io::stdout().flush()?;
            Ok(())
        }
        Ok(crate::clipboard::ClipboardContent::ImagePath(path)) => {
            let filename = std::path::Path::new(&path)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("image");
            let dir = paths.cache_dir.join("clipboard_images");
            std::fs::create_dir_all(&dir)?;
            crate::clipboard::cleanup_clipboard_images(&dir);
            let link_path = dir.join(filename);
            let need_create = if link_path.is_symlink() {
                !link_path.exists()
            } else {
                !link_path.exists()
            };
            if need_create {
                if link_path.exists() || link_path.is_symlink() {
                    std::fs::remove_file(&link_path)?;
                }
                std::os::unix::fs::symlink(&path, &link_path)?;
            }
            print!("[Image 1: {}]", filename);
            io::stdout().flush()?;
            Ok(())
        }
        Ok(crate::clipboard::ClipboardContent::TextPath(path)) => {
            print!("{}", path);
            io::stdout().flush()?;
            Ok(())
        }
        Ok(crate::clipboard::ClipboardContent::Text(text)) => {
            if should_summarize_pasted_text(&text) {
                let index = shell_pasted_text_index(&paths.cache_dir, &text)?;
                let placeholder = pasted_text_placeholder(index, pasted_text_line_count(&text));
                print!("{}", placeholder);
            } else {
                print!("{}", text);
            }
            io::stdout().flush()?;
            Ok(())
        }
        _ => {
            std::process::exit(1);
        }
    }
}

pub(crate) fn shell_pasted_text_index(cache_dir: &std::path::Path, text: &str) -> Result<usize> {
    let dir = cache_dir.join("clipboard_texts");
    std::fs::create_dir_all(&dir)?;
    let mut index = 1;
    loop {
        let path = dir.join(format!("{index}.txt"));
        if !path.exists() {
            std::fs::write(path, text)?;
            return Ok(index);
        }
        index += 1;
    }
}

pub(crate) fn shell_message_from_input(use_stdin: bool, message: Vec<String>) -> Result<String> {
    if use_stdin {
        let mut input = String::new();
        io::stdin().read_to_string(&mut input)?;
        Ok(input)
    } else {
        Ok(join_message(message))
    }
}

pub(crate) fn run_shell_classify(shell_name: &str, message: &str) -> Result<()> {
    if !matches!(shell_name, "fish" | "bash" | "zsh") {
        std::process::exit(2);
    }
    if shell::is_shell_command(message, shell_name) {
        std::process::exit(0);
    }
    std::process::exit(1);
}

pub(crate) async fn run_shell_intercept(
    paths: &GQYPaths,
    shell_name: &str,
    message: String,
) -> Result<()> {
    if !matches!(shell_name, "fish" | "bash" | "zsh") {
        bail!("{}: {shell_name}", t("unsupported shell", "不支持的 shell"));
    }
    if message.trim().is_empty() {
        bail!(
            "{}",
            t("not a natural language command", "不是自然语言命令")
        );
    }

    let message = expand_shell_pasted_text_placeholders(paths, &message)?;
    let (clean_message, pasted_images) = extract_image_placeholders(&message);

    let result = if pasted_images.is_empty() {
        // shell-hook keeps landing in the terminal session: that lane is the
        // whole point of typing natural language at the prompt.
        run_chat_with_options(
            paths,
            clean_message,
            None,
            false,
            AgentMode::Normal,
            TurnSession::Current,
        )
        .await
    } else {
        run_chat_with_images(paths, clean_message, pasted_images).await
    };
    drain_stdin();
    if let Err(err) = &result {
        println!("\x1b[31m{}: {err}\x1b[0m", t("error", "错误"));
    }
    result
}

pub(crate) fn expand_shell_pasted_text_placeholders(
    paths: &GQYPaths,
    message: &str,
) -> Result<String> {
    let placeholders = find_pasted_text_placeholders(message);
    if placeholders.is_empty() {
        return Ok(message.to_string());
    }

    let chars: Vec<char> = message.chars().collect();
    let mut expanded = String::new();
    let mut last_end = 0;
    let dir = paths.cache_dir.join("clipboard_texts");
    for (start, end, index) in placeholders {
        expanded.extend(&chars[last_end..start]);
        let path = dir.join(format!("{index}.txt"));
        match std::fs::read_to_string(&path) {
            Ok(text) => expanded.push_str(&text),
            Err(_) => expanded.extend(&chars[start..end]),
        }
        last_end = end;
    }
    expanded.extend(&chars[last_end..]);
    Ok(expanded)
}

pub(crate) fn extract_image_placeholders(
    message: &str,
) -> (String, Vec<Option<crate::clipboard::PastedImage>>) {
    let placeholders = find_image_placeholders(message);
    if placeholders.is_empty() {
        return (message.to_string(), Vec::new());
    }

    let cache_images_dir = GQYPaths::new()
        .map(|p| p.cache_dir.join("clipboard_images"))
        .ok();

    let chars: Vec<char> = message.chars().collect();
    let mut clean = String::new();
    let mut images: Vec<Option<crate::clipboard::PastedImage>> = Vec::new();
    let mut last_end = 0;

    for (start, end) in &placeholders {
        clean.extend(&chars[last_end..*start]);
        let segment: String = chars[*start..*end].iter().collect();
        let name_str = segment
            .strip_prefix("[Image ")
            .and_then(|s| s.strip_prefix(|c: char| c.is_ascii_digit()))
            .and_then(|s| s.strip_prefix(':'))
            .and_then(|s| s.strip_suffix(']'))
            .map(|s| s.trim().to_string());

        if let Some(name_str) = name_str {
            if let Some(dir) = &cache_images_dir {
                let candidate = dir.join(&name_str);
                if candidate.exists() {
                    images.push(Some(crate::clipboard::PastedImage::Path(
                        candidate.display().to_string(),
                    )));
                } else {
                    images.push(None);
                }
            } else {
                images.push(None);
            }
        } else {
            images.push(None);
        }
        clean.push_str(&format!("[Image {}]", images.len()));
        last_end = *end;
    }
    clean.extend(&chars[last_end..]);

    (clean, images)
}

pub(crate) async fn run_chat_with_images(
    paths: &GQYPaths,
    message: String,
    pasted_images: Vec<Option<crate::clipboard::PastedImage>>,
) -> Result<()> {
    if !direct_mode_requested() {
        match try_run_remote_chat(
            paths,
            None,
            &message,
            None,
            false,
            AgentMode::Normal,
            &pasted_images,
            None,
            None,
        )
        .await
        {
            Ok(Some(_)) => return Ok(()),
            Ok(None) => {}
            Err(err) => return Err(err),
        }
    }
    let _core_lease = ipc::acquire_direct_core(paths)?;
    initialize_models_cache(paths);
    AppConfig::init_files(paths)?;
    let config = AppConfig::load_or_default(paths)?;
    let state = StateStore::new(paths)?;
    state.init_files()?;
    let memory_organizer = MemoryOrganizer::spawn()?;
    let memory_organizer_handle = memory_organizer.handle();
    memory_organizer_handle.wake(config.clone(), paths.clone(), state.clone());
    let client = OpenAiCompatibleClient::from_config(&config, paths)?;
    let registry = build_tool_registry(
        &config,
        paths,
        AgentMode::Normal,
        crate::question_tui::available(false),
    )?;
    let reasoning_mode = render::ReasoningDisplayMode::from_config(&config.display.reasoning);
    let tool_call_mode = render::ToolCallDisplayMode::from_config(&config.display.tool_calls);
    let readable_tool_names = config.display.readable_tool_names;
    let command_output_lines = config.display.command_output_lines;
    let show_token_usage = config.display.show_token_usage;
    let show_mixed_model_endpoint = show_mixed_model_endpoint(&config, false);
    let display_config = config.clone();
    let mut agent = Agent::new(
        config,
        paths,
        state.clone(),
        client,
        registry,
        AgentMode::Normal,
    )?;
    agent.set_memory_organizer(memory_organizer_handle);
    agent.prepare_for_turn()?;
    let mut renderer = render::StreamRenderer::new(
        reasoning_mode,
        tool_call_mode,
        false,
        readable_tool_names,
        command_output_lines,
    );
    renderer.start_waiting()?;
    let result = agent
        .chat_stream_with_images(&message, &pasted_images, |event| {
            handle_agent_event(&mut renderer, event)
        })
        .await;
    renderer.finish()?;
    let result = match result {
        Ok(result) => result,
        Err(err) if crate::question::is_question_cancelled(&err) => return Ok(()),
        Err(err) => return Err(err),
    };
    print_mixed_model_endpoint(show_mixed_model_endpoint, &result, None);
    let mut cumulative_tokens = TurnTokens::from_usage(result.usage.as_ref());
    let context_tokens = agent.effective_context_tokens()?;
    print_chat_token_usage(
        &result,
        show_token_usage,
        context_tokens,
        result_context_window(&display_config, &result).or(agent.context_window()),
        cumulative_tokens,
    )?;
    let overflow_result = handle_post_turn_overflow(
        &agent,
        &mut renderer,
        context_tokens,
        show_token_usage,
        Some(&mut cumulative_tokens),
    )
    .await?;
    let updated_context_tokens = agent.effective_context_tokens()?;
    if overflow_result.is_none() && updated_context_tokens != context_tokens {
        print_chat_token_usage(
            &result,
            show_token_usage,
            updated_context_tokens,
            result_context_window(&display_config, &result).or(agent.context_window()),
            cumulative_tokens,
        )?;
    }
    Ok(())
}

pub(crate) fn drain_stdin() {
    use std::os::fd::AsRawFd;

    let stdin = io::stdin();
    if !stdin.is_terminal() {
        return;
    }
    let fd = stdin.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return;
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return;
    }

    let mut handle = stdin.lock();
    let mut buffer = [0_u8; 4096];
    loop {
        match handle.read(&mut buffer) {
            Ok(0) => break,
            Ok(_) => continue,
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => break,
            Err(_) => break,
        }
    }

    let _ = unsafe { libc::fcntl(fd, libc::F_SETFL, flags) };
}

pub(crate) const STDIN_MAX_CHARS: usize = 50_000;
pub(crate) const STDIN_TIMEOUT_SECS: u64 = 5;
