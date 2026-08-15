//! variant — 自 src/cli.rs 拆分。

pub(crate) use super::*;

pub(crate) fn draw_inline_single_variant(
    stdout: &mut io::Stdout,
    anchor_y: u16,
    menu_lines: u16,
    item: &VariantMenuItem,
    scroll: usize,
) -> Result<()> {
    let (cols, _) = terminal::size().unwrap_or((80, 24));
    let bar = inline_fuzzy_bar();
    let available = (cols as usize).saturating_sub(visible_width(&bar)).max(1);
    let width = single_variant_content_width(item).min(available);
    let visible = menu_lines.saturating_sub(2) as usize;
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
        Print(variant_menu_header(
            t("Thinking variant", "思考档位"),
            true,
            width,
        )),
    )?;
    for row in 0..visible {
        let index = scroll + row;
        let line = item.options.get(index).map_or_else(
            || " ".repeat(width),
            |variant| {
                variant_menu_cell(
                    &variant.label,
                    index == item.cursor,
                    index == item.cursor,
                    Some(index == item.selected),
                    width,
                )
            },
        );
        queue!(
            stdout,
            MoveTo(0, anchor_y + row as u16 + 1),
            Print(&bar),
            Print(line),
        )?;
    }
    queue!(
        stdout,
        MoveTo(0, anchor_y + menu_lines.saturating_sub(1)),
        Print(&bar),
        Print(format!(
            "\x1b[2m{}\x1b[0m",
            truncate_visible_width(
                t(
                    "j/k move · Tab select · Enter confirm · Esc/q cancel",
                    "j/k 移动 · Tab 勾选 · Enter 确认 · Esc/q 取消"
                ),
                available,
            )
        ))
    )?;
    stdout.flush()?;
    Ok(())
}

pub(crate) fn single_variant_content_width(item: &VariantMenuItem) -> usize {
    item.options
        .iter()
        .map(|option| visible_width(&option.label).saturating_add(6))
        .chain(std::iter::once(visible_width(t(
            "Thinking variant",
            "思考档位",
        ))))
        .max()
        .unwrap_or(1)
}

#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_inline_variant(
    stdout: &mut io::Stdout,
    anchor_y: u16,
    menu_lines: u16,
    items: &[VariantMenuItem],
    active_column: usize,
    model_index: usize,
    model_scroll: usize,
    variant_scroll: usize,
) -> Result<()> {
    let (cols, _) = terminal::size().unwrap_or((80, 24));
    let bar = inline_fuzzy_bar();
    let width = (cols as usize).saturating_sub(visible_width(&bar)).max(1);
    let separator = if width >= 3 { " │ " } else { "" };
    let available = width.saturating_sub(visible_width(separator));
    let (left_width, right_width) = variant_menu_column_widths(items, available);
    let visible = menu_lines.saturating_sub(2) as usize;
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
        Print(variant_menu_header(
            t("Provider / Model", "Provider / 模型"),
            active_column == 0,
            left_width,
        )),
        Print(format!("\x1b[2m{separator}\x1b[0m")),
        Print(variant_menu_header(
            t("Thinking variant", "思考档位"),
            active_column == 1,
            right_width,
        )),
    )?;
    let variants = &items[model_index];
    for row in 0..visible {
        let left_index = model_scroll + row;
        let right_index = variant_scroll + row;
        let left = items.get(left_index).map_or_else(
            || " ".repeat(left_width),
            |item| {
                variant_menu_cell(
                    &format!("{} / {}", item.provider_id, item.model),
                    active_column == 0 && left_index == model_index,
                    left_index == model_index,
                    None,
                    left_width,
                )
            },
        );
        let right = variants.options.get(right_index).map_or_else(
            || " ".repeat(right_width),
            |variant| {
                variant_menu_cell(
                    &variant.label,
                    active_column == 1 && right_index == variants.cursor,
                    right_index == variants.cursor,
                    Some(right_index == variants.selected),
                    right_width,
                )
            },
        );
        queue!(
            stdout,
            MoveTo(0, anchor_y + row as u16 + 1),
            Print(&bar),
            Print(left),
            Print(format!("\x1b[2m{separator}\x1b[0m")),
            Print(right),
        )?;
    }
    queue!(
        stdout,
        MoveTo(0, anchor_y + menu_lines.saturating_sub(1)),
        Print(&bar),
        Print(format!(
            "\x1b[2m{}\x1b[0m",
            truncate_visible_width(
                t(
                    "h/l switch · j/k move · Tab select · Enter confirm · Esc/q cancel",
                    "h/l 切栏 · j/k 移动 · Tab 勾选 · Enter 确认 · Esc/q 取消"
                ),
                width,
            )
        ))
    )?;
    stdout.flush()?;
    Ok(())
}

pub(crate) fn variant_menu_column_widths(
    items: &[VariantMenuItem],
    available: usize,
) -> (usize, usize) {
    if available == 0 {
        return (0, 0);
    }
    if available == 1 {
        return (1, 0);
    }
    let left_needed = items
        .iter()
        .map(|item| {
            visible_width(&format!("{} / {}", item.provider_id, item.model)).saturating_add(2)
        })
        .chain(std::iter::once(visible_width(t(
            "Provider / Model",
            "Provider / 模型",
        ))))
        .max()
        .unwrap_or(1);
    let right_needed = items
        .iter()
        .flat_map(|item| item.options.iter())
        .map(|option| visible_width(&option.label).saturating_add(6))
        .chain(std::iter::once(visible_width(t(
            "Thinking variant",
            "思考档位",
        ))))
        .max()
        .unwrap_or(1);
    if left_needed.saturating_add(right_needed) <= available {
        return (left_needed, right_needed);
    }
    let total_needed = left_needed.saturating_add(right_needed).max(1);
    let left = available
        .saturating_mul(left_needed)
        .saturating_div(total_needed)
        .clamp(1, available - 1);
    (left, available - left)
}

pub(crate) fn variant_menu_header(label: &str, active: bool, width: usize) -> String {
    let label = pad_visible_width(&truncate_visible_width(label, width), width);
    if active {
        format!("\x1b[1m\x1b[35m{label}\x1b[0m")
    } else {
        format!("\x1b[1m{label}\x1b[0m")
    }
}

pub(crate) fn variant_menu_cell(
    label: &str,
    focused: bool,
    highlighted: bool,
    checked: Option<bool>,
    width: usize,
) -> String {
    let marker = if highlighted { "›" } else { " " };
    let check = match checked {
        Some(true) => "[*] ",
        Some(false) => "[ ] ",
        None => "",
    };
    let line = pad_visible_width(
        &truncate_visible_width(&format!("{marker} {check}{label}"), width),
        width,
    );
    if focused {
        format!("\x1b[1m\x1b[35m{line}\x1b[0m")
    } else if checked == Some(true) {
        format!("\x1b[1m\x1b[32m{line}\x1b[0m")
    } else if highlighted {
        format!("\x1b[1m{line}\x1b[0m")
    } else {
        format!("\x1b[2m{line}\x1b[0m")
    }
}

pub(crate) fn pad_visible_width(value: &str, width: usize) -> String {
    format!(
        "{value}{}",
        " ".repeat(width.saturating_sub(visible_width(value)))
    )
}

pub(crate) fn split_repl_command(input: &str) -> (&str, &str) {
    let Some((command, args)) = input.split_once(char::is_whitespace) else {
        return (input, "");
    };
    (command, args)
}

pub(crate) const REPL_HISTORY_CAP: usize = 200;

pub(crate) fn repl_history_file(paths: &GQYPaths) -> PathBuf {
    paths.state_dir.join("repl-history.jsonl")
}

/// Prompt history that survives /reset and restarts: a global append-only
/// file, capped on load. Conversation resets delete turns, so the file is
/// the durable source; the turns-derived list only seeds sessions that
/// predate it.
pub(crate) fn load_persistent_repl_history(paths: &GQYPaths) -> Vec<String> {
    let Ok(content) = std::fs::read_to_string(repl_history_file(paths)) else {
        return Vec::new();
    };
    let mut entries = content
        .lines()
        .filter_map(|line| serde_json::from_str::<String>(line).ok())
        .filter(|entry| !entry.trim().is_empty())
        .collect::<Vec<_>>();
    if entries.len() > REPL_HISTORY_CAP {
        entries = entries.split_off(entries.len() - REPL_HISTORY_CAP);
        // Opportunistic rewrite keeps the file from growing without bound.
        let rewritten = entries
            .iter()
            .filter_map(|entry| serde_json::to_string(entry).ok())
            .collect::<Vec<_>>()
            .join("\n");
        let _ = std::fs::write(repl_history_file(paths), rewritten + "\n");
    }
    entries
}

pub(crate) fn persist_repl_history_entry(paths: &GQYPaths, entry: &str) {
    let entry = entry.trim();
    if entry.is_empty() {
        return;
    }
    let Ok(line) = serde_json::to_string(entry) else {
        return;
    };
    let path = repl_history_file(paths);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut file| {
            use std::io::Write as _;
            writeln!(file, "{line}")
        });
}

pub(crate) fn load_repl_input_history(state: &StateStore, paths: &GQYPaths) -> Result<Vec<String>> {
    let mut merged: Vec<String> = state
        .load_conversation()?
        .into_iter()
        .filter(|entry| entry.role == "user" && !entry.content.trim().is_empty())
        .map(|entry| strip_terminal_control_sequences(&entry.content))
        .filter(|content| !content.trim().is_empty())
        .collect();
    for entry in load_persistent_repl_history(paths) {
        if !merged.contains(&entry) {
            merged.push(entry);
        }
    }
    Ok(merged)
}

pub(crate) fn print_repl_help() {
    println!("{}", t("commands:", "命令:"));
    let width = REPL_COMMAND_TABLE
        .iter()
        .map(|spec| {
            spec.name.len()
                + if spec.arg_hint.is_empty() {
                    0
                } else {
                    spec.arg_hint.len() + 1
                }
        })
        .max()
        .unwrap_or(0);
    for spec in REPL_COMMAND_TABLE {
        let invocation = if spec.arg_hint.is_empty() {
            spec.name.to_string()
        } else {
            format!("{} {}", spec.name, spec.arg_hint)
        };
        println!("  {invocation:<width$}  {}", t(spec.help_en, spec.help_zh));
    }
    println!("{}", t("keys:", "快捷键:"));
    println!(
        "  Tab         {}",
        t(
            "cycle NORMAL/CHAT, or complete slash commands",
            "循环切换 普通/闲聊，或补全斜杠菜单"
        )
    );
    println!("  Enter       {}", t("send message", "发送消息"));
    println!("  Shift+Enter {}", t("insert newline", "插入换行"));
    println!(
        "  Ctrl+J      {}",
        t(
            "insert newline, same as Shift+Enter",
            "插入换行，与 Shift+Enter 相同"
        )
    );
    println!(
        "  Ctrl+V      {}",
        t(
            "paste image or text from clipboard",
            "从剪贴板粘贴图片或文本"
        )
    );
    println!("  Ctrl+L      {}", t("clear screen", "清屏"));
    println!(
        "  Up/Down     {}",
        t("browse input history", "切换输入历史")
    );
    println!(
        "  Esc Esc     {}",
        t("interrupt running reply", "中断当前回复")
    );
    println!(
        "  Ctrl+C      {}",
        t(
            "clear the draft, else interrupt the reply, else stop background tasks, else exit",
            "先清空输入；输入为空则中断回复；再无回复则停止后台任务；都没有则退出"
        )
    );
    println!("  Ctrl+D      {}", t("exit", "退出"));
}

pub(crate) struct LiveReplEditor {
    pub(crate) mode: AgentMode,
    pub(crate) input: String,
    pub(crate) cursor: usize,
    pub(crate) history: Vec<String>,
    pub(crate) history_index: usize,
    pub(crate) history_clean_index: Option<usize>,
    pub(crate) is_pasted: bool,
    pasted_images: Vec<Option<crate::clipboard::PastedImage>>,
    pasted_texts: Vec<Option<PastedText>>,
    escape_armed_until: Option<Instant>,
    /// Whether the terminal window currently has focus, per the terminal's own
    /// focus reporting. Starts `true`: a terminal that never reports focus
    /// leaves this pinned, and notifications stay quiet rather than firing on
    /// every turn.
    pub(crate) focused: bool,
}

pub(crate) struct LiveSubmission {
    pub(crate) content: String,
    pub(crate) display_content: String,
    pub(crate) images: Vec<Option<crate::clipboard::PastedImage>>,
}

pub(crate) struct LiveAgentInput<'a> {
    pub(crate) content: &'a str,
    pub(crate) images: &'a [Option<crate::clipboard::PastedImage>],
}

pub(crate) enum LiveEditorAction {
    None,
    Redraw,
    ClearScreen,
    EmptySubmit,
    Submit(LiveSubmission),
    Interrupt,
    Exit,
}

impl LiveReplEditor {
    pub(crate) fn new(mode: AgentMode, history: Vec<String>) -> Self {
        let history_index = history.len();
        Self {
            mode,
            input: String::new(),
            cursor: 0,
            history,
            history_index,
            history_clean_index: None,
            is_pasted: false,
            pasted_images: Vec::new(),
            pasted_texts: Vec::new(),
            escape_armed_until: None,
            focused: true,
        }
    }

    pub(crate) fn clear(&mut self) {
        self.input.clear();
        self.cursor = 0;
        self.history_clean_index = None;
        self.is_pasted = false;
        self.pasted_images.clear();
        self.pasted_texts.clear();
        self.escape_armed_until = None;
    }

    pub(crate) fn submit(&mut self) -> Option<LiveSubmission> {
        let display_content = strip_terminal_control_sequences(&self.input);
        let content = expand_pasted_text_placeholders(&display_content, &self.pasted_texts);
        let content = content.trim().to_string();
        if content.is_empty() {
            return None;
        }
        let display_content = display_content.trim().to_string();
        let images = std::mem::take(&mut self.pasted_images);
        self.input.clear();
        self.cursor = 0;
        self.history_clean_index = None;
        self.is_pasted = false;
        self.pasted_texts.clear();
        Some(LiveSubmission {
            content,
            display_content,
            images,
        })
    }

    pub(crate) fn record_history(&mut self, content: &str) {
        self.history.push(content.to_string());
        self.history_index = self.history.len();
    }

    pub(crate) fn handle_event(
        &mut self,
        event: Event,
        paths: &GQYPaths,
        allow_interrupt: bool,
    ) -> Result<LiveEditorAction> {
        let is_escape = matches!(
            &event,
            Event::Key(KeyEvent {
                code: KeyCode::Esc,
                ..
            })
        );
        if !is_escape {
            self.escape_armed_until = None;
        }
        match event {
            Event::Key(KeyEvent {
                kind: KeyEventKind::Release,
                ..
            }) => return Ok(LiveEditorAction::None),
            Event::Resize(_, _) => return Ok(LiveEditorAction::Redraw),
            // Focus reporting gates notifications: no popup while you are
            // looking at the window.
            Event::FocusGained => {
                self.focused = true;
                return Ok(LiveEditorAction::None);
            }
            Event::FocusLost => {
                self.focused = false;
                return Ok(LiveEditorAction::None);
            }
            Event::Paste(text) => {
                insert_pasted_text_at_cursor(
                    &mut self.input,
                    &mut self.cursor,
                    text,
                    &mut self.pasted_texts,
                );
                self.history_clean_index = None;
                self.is_pasted = true;
                return Ok(LiveEditorAction::Redraw);
            }
            Event::Key(KeyEvent {
                code, modifiers, ..
            }) => match code {
                KeyCode::Tab => {
                    if self.input.starts_with('/') {
                        if let Some(completed) = complete_repl_command(&self.input) {
                            self.input = completed.to_string();
                            self.cursor = self.input.chars().count();
                            self.history_clean_index = None;
                        }
                    } else {
                        // 会话模式创建时定死:Tab 切换已随闲聊模式一并删除
                        // (中途换模式=系统提示词换血=全量缓存作废)。
                    }
                }
                KeyCode::Esc => {
                    // Esc never clears typed input (Ctrl+C does that); it
                    // only arms the double-press interrupt while a reply is
                    // running.
                    if allow_interrupt {
                        if self
                            .escape_armed_until
                            .is_some_and(|deadline| Instant::now() < deadline)
                        {
                            self.escape_armed_until = None;
                            return Ok(LiveEditorAction::Interrupt);
                        }
                        self.escape_armed_until = Some(Instant::now() + Duration::from_secs(2));
                    }
                }
                KeyCode::Left => {
                    if let Some((start, _)) = placeholder_at_cursor(&self.input, self.cursor) {
                        self.cursor = start;
                    } else {
                        self.cursor = self.cursor.saturating_sub(1);
                    }
                }
                KeyCode::Right => {
                    if let Some((_, end)) = placeholder_at_cursor(&self.input, self.cursor) {
                        self.cursor = end;
                    } else {
                        self.cursor = (self.cursor + 1).min(self.input.chars().count());
                    }
                }
                KeyCode::Home => self.cursor = 0,
                KeyCode::End => self.cursor = self.input.chars().count(),
                KeyCode::Up => {
                    if !self.history.is_empty()
                        && repl_should_browse_history(
                            &self.input,
                            &self.history,
                            self.history_clean_index,
                        )
                    {
                        if self.input.is_empty() {
                            self.history_index = self.history.len();
                        }
                        self.history_index = self.history_index.saturating_sub(1);
                        self.input = self
                            .history
                            .get(self.history_index)
                            .cloned()
                            .unwrap_or_default();
                        self.cursor = self.input.chars().count();
                        self.history_clean_index = Some(self.history_index);
                        self.is_pasted = false;
                        self.pasted_images.clear();
                        self.pasted_texts.clear();
                    } else {
                        self.cursor = repl_move_cursor_vertical("  ", &self.input, self.cursor, -1);
                    }
                }
                KeyCode::Down => {
                    if repl_history_is_clean(&self.input, &self.history, self.history_clean_index) {
                        if self.history_index + 1 < self.history.len() {
                            self.history_index += 1;
                            self.input = self
                                .history
                                .get(self.history_index)
                                .cloned()
                                .unwrap_or_default();
                            self.cursor = self.input.chars().count();
                            self.history_clean_index = Some(self.history_index);
                        } else {
                            self.history_index = self.history.len();
                            self.input.clear();
                            self.cursor = 0;
                            self.history_clean_index = None;
                        }
                        self.is_pasted = false;
                        self.pasted_images.clear();
                        self.pasted_texts.clear();
                    } else {
                        self.cursor = repl_move_cursor_vertical("  ", &self.input, self.cursor, 1);
                    }
                }
                KeyCode::Enter if modifiers.contains(KeyModifiers::SHIFT) => {
                    // Shift+Enter 与 Ctrl+J 相同：在光标处插入换行，不提交
                    insert_newline_at_cursor(&mut self.input, &mut self.cursor);
                    self.history_clean_index = None;
                    self.is_pasted = false;
                }
                KeyCode::Enter => {
                    return Ok(self
                        .submit()
                        .map(LiveEditorAction::Submit)
                        .unwrap_or(LiveEditorAction::EmptySubmit));
                }
                KeyCode::Char('j') if modifiers.contains(KeyModifiers::CONTROL) => {
                    insert_newline_at_cursor(&mut self.input, &mut self.cursor);
                    self.history_clean_index = None;
                    self.is_pasted = false;
                }
                KeyCode::Char('c')
                    if modifiers.contains(KeyModifiers::CONTROL)
                        && !modifiers.contains(KeyModifiers::SHIFT) =>
                {
                    if self.input.is_empty() {
                        return Ok(LiveEditorAction::Interrupt);
                    }
                    self.clear();
                }
                KeyCode::Char('d')
                    if modifiers.contains(KeyModifiers::CONTROL) && self.input.is_empty() =>
                {
                    return Ok(LiveEditorAction::Exit);
                }
                KeyCode::Char('w') if modifiers.contains(KeyModifiers::CONTROL) => {
                    if let Some((start, end)) =
                        placeholder_before_or_at_cursor(&self.input, self.cursor)
                    {
                        clear_placeholder_payload(
                            &self.input,
                            start,
                            end,
                            &mut self.pasted_images,
                            &mut self.pasted_texts,
                        );
                        remove_range_chars(&mut self.input, start, end);
                        self.cursor = start;
                    } else {
                        remove_word_before_cursor(&mut self.input, &mut self.cursor);
                    }
                    self.history_clean_index = None;
                    self.is_pasted = false;
                }
                KeyCode::Backspace => {
                    if self.cursor > 0 {
                        if let Some((start, end)) =
                            placeholder_before_or_at_cursor(&self.input, self.cursor)
                        {
                            clear_placeholder_payload(
                                &self.input,
                                start,
                                end,
                                &mut self.pasted_images,
                                &mut self.pasted_texts,
                            );
                            remove_range_chars(&mut self.input, start, end);
                            self.cursor = start;
                        } else {
                            remove_char_before_cursor(&mut self.input, &mut self.cursor);
                        }
                        self.history_clean_index = None;
                    }
                    self.is_pasted = false;
                }
                KeyCode::Delete => {
                    if let Some((start, end)) =
                        placeholder_after_or_at_cursor(&self.input, self.cursor)
                    {
                        clear_placeholder_payload(
                            &self.input,
                            start,
                            end,
                            &mut self.pasted_images,
                            &mut self.pasted_texts,
                        );
                        remove_range_chars(&mut self.input, start, end);
                    } else {
                        remove_char_at_cursor(&mut self.input, self.cursor);
                    }
                    self.history_clean_index = None;
                    self.is_pasted = false;
                }
                KeyCode::Char('c' | 'C')
                    if modifiers.contains(KeyModifiers::CONTROL)
                        && modifiers.contains(KeyModifiers::SHIFT) =>
                {
                    if let Some(selected) =
                        placeholder_text_near_cursor(&self.input, self.cursor, &self.pasted_texts)
                    {
                        let _ = crate::clipboard::write_clipboard_text(&selected)?;
                    }
                }
                KeyCode::Char('v') if modifiers.contains(KeyModifiers::CONTROL) => {
                    self.paste_clipboard(paths)?;
                }
                KeyCode::Char('l') if modifiers.contains(KeyModifiers::CONTROL) => {
                    return Ok(LiveEditorAction::ClearScreen);
                }
                KeyCode::Char(ch) if !modifiers.contains(KeyModifiers::CONTROL) => {
                    if !is_disallowed_control_char(ch) {
                        if let Some((_, end)) = placeholder_at_cursor(&self.input, self.cursor) {
                            self.cursor = end;
                        }
                        insert_char_at_cursor(&mut self.input, &mut self.cursor, ch);
                        self.history_clean_index = None;
                    }
                    self.is_pasted = false;
                }
                _ => return Ok(LiveEditorAction::None),
            },
            _ => return Ok(LiveEditorAction::None),
        }
        Ok(LiveEditorAction::Redraw)
    }

    pub(crate) fn paste_clipboard(&mut self, paths: &GQYPaths) -> Result<()> {
        match crate::clipboard::read_clipboard() {
            Ok(crate::clipboard::ClipboardContent::Image(image)) => {
                let index = self.pasted_images.len() + 1;
                let placeholder = match image.write_temp_file(&paths.cache_dir, index) {
                    Ok(path) => {
                        let filename = path
                            .file_name()
                            .and_then(|name| name.to_str())
                            .unwrap_or("image");
                        format!("[Image {index}: {filename}]")
                    }
                    Err(_) => format!("[Image {index}]"),
                };
                insert_str_at_cursor(&mut self.input, &mut self.cursor, &placeholder);
                self.pasted_images
                    .push(Some(crate::clipboard::PastedImage::Binary(image)));
                self.is_pasted = false;
            }
            Ok(crate::clipboard::ClipboardContent::ImagePath(path)) => {
                let index = self.pasted_images.len() + 1;
                let filename = std::path::Path::new(&path)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("image");
                insert_str_at_cursor(
                    &mut self.input,
                    &mut self.cursor,
                    &format!("[Image {index}: {filename}]"),
                );
                self.pasted_images
                    .push(Some(crate::clipboard::PastedImage::Path(path)));
                self.is_pasted = false;
            }
            Ok(crate::clipboard::ClipboardContent::TextPath(path)) => {
                insert_str_at_cursor(&mut self.input, &mut self.cursor, &path);
                self.is_pasted = false;
            }
            _ => {
                if let Ok(Some(text)) = crate::clipboard::read_clipboard_text() {
                    insert_pasted_text_at_cursor(
                        &mut self.input,
                        &mut self.cursor,
                        text,
                        &mut self.pasted_texts,
                    );
                    self.is_pasted = true;
                }
            }
        }
        self.history_clean_index = None;
        Ok(())
    }
}
