//! init — 自 src/cli.rs 拆分。

pub(crate) use super::*;

#[derive(Clone, Copy)]
pub(crate) enum InitKind {
    FirstRun,
    Explicit,
}

pub(crate) fn run_init(paths: &GQYPaths, kind: InitKind) -> Result<()> {
    let interactive = io::stdin().is_terminal() && io::stdout().is_terminal();
    if interactive {
        println!(
            "{}\n",
            match kind {
                InitKind::FirstRun => t("GQY first start", "GQY 首次启动"),
                InitKind::Explicit => t("GQY initialization", "GQY 初始化"),
            }
        );
    }
    print_init_step(
        interactive,
        t("Preparing config directory", "正在准备配置目录"),
        &paths.config_dir.display().to_string(),
    )?;
    AppConfig::init_files(paths)?;
    print_init_step(
        interactive,
        t("Writing default config", "正在写入默认配置"),
        &paths.config_file.display().to_string(),
    )?;
    print_init_step(
        interactive,
        t("Creating state files", "正在创建状态文件"),
        &paths.state_dir.display().to_string(),
    )?;
    StateStore::new(paths)?.init_files()?;
    let config = AppConfig::load_or_default(paths)?;
    if crate::default_kb::bundled_available() {
        print_init_step(
            interactive,
            t("Importing default knowledge base", "正在导入默认知识库"),
            &paths.data_dir.join("kb").display().to_string(),
        )?;
        if let Err(err) = crate::default_kb::ensure_initialized(paths, &config) {
            if interactive {
                eprintln!(
                    "{}: {err}",
                    t(
                        "default knowledge base import skipped",
                        "默认知识库导入已跳过"
                    )
                );
            }
        }
    }
    print_init_step(
        interactive,
        t("Preparing data directory", "正在准备数据目录"),
        &paths.data_dir.display().to_string(),
    )?;
    if interactive {
        println!("\n{}\n", t("Initialization complete.", "初始化完成。"));
    } else {
        println!(
            "{} {}",
            t("initialized GQY at", "GQY 已初始化于"),
            paths.config_dir.display()
        );
    }
    Ok(())
}

pub(crate) fn print_init_step(interactive: bool, label: &str, value: &str) -> Result<()> {
    if interactive {
        std::thread::sleep(Duration::from_millis(180));
        println!("  {label:<24} ✓ {value}");
        io::stdout().flush()?;
    }
    Ok(())
}

pub(crate) fn remove_shell_hooks(paths: &GQYPaths) -> Result<()> {
    let removed = shell::fish::uninstall(paths)?;
    let removed = shell::bash::uninstall(paths)? || removed;
    let removed = shell::zsh::uninstall(paths)? || removed;
    if !removed {
        println!(
            "{}",
            t(
                "no installed GQY shell hooks found",
                "未找到已安装的 GQY shell hook"
            )
        );
    }
    Ok(())
}

pub(crate) fn run_alarm_worker(args: AlarmWorkerArgs) -> Result<()> {
    let paths = alarm_worker_paths(args.state_dir, args.cache_dir);
    let seconds = crate::alarm::parse_alarm_seconds(&args.time)?;
    let source = args
        .audio_file
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "builtin".to_string());
    let _ = append_alarm_log(
        &paths,
        &format!("{}: scheduled in {seconds}s; source={source}\n", args.id),
    );
    std::thread::sleep(Duration::from_secs(seconds));
    let _ = crate::alarm::update_status(&paths, &args.id, crate::alarm::AlarmStatus::Ringing);
    let _ = append_alarm_log(&paths, &format!("{}: playback starting\n", args.id));
    let result = play_alarm_once(args.audio_file.as_deref()).or_else(|err| {
        append_alarm_log(
            &paths,
            &format!("{}: audio playback failed: {err}\n", args.id),
        )?;
        terminal_bell_fallback();
        Ok(())
    });
    if result.is_ok() {
        let _ = append_alarm_log(&paths, &format!("{}: playback finished\n", args.id));
    }
    let _ = crate::alarm::remove(&paths, &args.id);
    result
}

pub(crate) fn play_alarm_once(audio_file: Option<&std::path::Path>) -> Result<()> {
    pub(crate) const ALARM_WAV: &[u8] = include_bytes!("../assets/alarm.wav");
    let (_stream, handle) = rodio::OutputStream::try_default()?;
    let audio = match audio_file {
        Some(path) => std::fs::read(path)?,
        None => ALARM_WAV.to_vec(),
    };
    let cursor = Cursor::new(audio);
    let sink = rodio::Sink::try_new(&handle)?;
    let source = rodio::Decoder::new(cursor)?;
    sink.append(source);
    sink.sleep_until_end();
    Ok(())
}

pub(crate) fn terminal_bell_fallback() {
    for _ in 0..5 {
        let _ = std::io::stderr().write_all(b"\x07");
        let _ = std::io::stderr().flush();
        std::thread::sleep(Duration::from_secs(1));
    }
}

pub(crate) fn append_alarm_log(paths: &GQYPaths, line: &str) -> Result<()> {
    std::fs::create_dir_all(paths.logs_dir())?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(crate::alarm::alarm_log_file(paths))?;
    file.write_all(line.as_bytes())?;
    Ok(())
}

pub(crate) fn alarm_worker_paths(state_dir: PathBuf, cache_dir: PathBuf) -> GQYPaths {
    GQYPaths {
        root_dir: PathBuf::new(),
        config_dir: PathBuf::new(),
        config_file: PathBuf::new(),
        skills_dir: PathBuf::new(),
        data_dir: PathBuf::new(),
        cache_dir,
        state_dir,
        pictures_dir: PathBuf::new(),
        fish_hook_file: PathBuf::new(),
        bash_hook_file: PathBuf::new(),
        zsh_hook_file: PathBuf::new(),
        scripts_dir: PathBuf::new(),
        system_scripts_dir: PathBuf::new(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PopOutcome {
    pub(crate) turns: usize,
    pub(crate) archived: bool,
}

pub(crate) fn run_pop(paths: &GQYPaths, args: PopArgs) -> Result<()> {
    let config = AppConfig::load_or_default(paths)?;
    let state = StateStore::new(paths)?;
    state.recover_stale_turns()?;
    if let Some(outcome) = execute_pop(paths, &config, &state, args.count)? {
        print_pop_outcome(outcome);
    }
    Ok(())
}

/// Pop while the daemon owns the core: candidates are selected locally
/// (read-only), but the mutation goes through IPC so the daemon stays the
/// single writer.
pub(crate) async fn run_pop_via_daemon(paths: &GQYPaths, args: PopArgs) -> Result<()> {
    let state = StateStore::new(paths)?;
    let turn_ids: Vec<String> = match args.count {
        Some(count) => {
            validate_pop_count(count)?;
            state
                .oldest_evictable_visible_turns(count)?
                .into_iter()
                .map(|turn| turn.turn_id)
                .collect()
        }
        None => {
            if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
                bail!(
                    "{}",
                    t(
                        "interactive pop requires a terminal; use `gqy pop <count>`",
                        "交互 pop 需要终端；请使用 `gqy pop <数量>`",
                    )
                );
            }
            let limit = usize::try_from(i64::MAX).unwrap_or(usize::MAX);
            let candidates = state.oldest_evictable_visible_turns(limit)?;
            if candidates.is_empty() {
                print_nothing_to_pop();
                return Ok(());
            }
            let Some(selected) = inline_pop_select(&candidates)? else {
                return Ok(());
            };
            candidates
                .into_iter()
                .zip(selected)
                .filter_map(|(turn, selected)| selected.then_some(turn.turn_id))
                .collect()
        }
    };
    if turn_ids.is_empty() {
        print_nothing_to_pop();
        return Ok(());
    }
    let (_, data) = send_ipc_admin(
        paths,
        IpcCommand::Pop {
            target: crate::ipc::SessionRef::Current,
            turn_ids,
        },
    )
    .await?;
    let turns = data
        .get("turns")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    if turns > 0 {
        print_pop_outcome(PopOutcome {
            turns: turns as usize,
            archived: data
                .get("archived")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
        });
    } else {
        print_nothing_to_pop();
    }
    Ok(())
}

pub(crate) fn execute_pop(
    paths: &GQYPaths,
    config: &AppConfig,
    state: &StateStore,
    count: Option<usize>,
) -> Result<Option<PopOutcome>> {
    let turns = match count {
        Some(count) => {
            validate_pop_count(count)?;
            state.oldest_evictable_visible_turns(count)?
        }
        None => {
            if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
                bail!(
                    "{}",
                    t(
                        "interactive pop requires a terminal; use `gqy pop <count>`",
                        "交互 pop 需要终端；请使用 `gqy pop <数量>`",
                    )
                );
            }
            let limit = usize::try_from(i64::MAX).unwrap_or(usize::MAX);
            let candidates = state.oldest_evictable_visible_turns(limit)?;
            if candidates.is_empty() {
                print_nothing_to_pop();
                return Ok(None);
            }
            let Some(selected) = inline_pop_select(&candidates)? else {
                return Ok(None);
            };
            let selected = candidates
                .into_iter()
                .zip(selected)
                .filter_map(|(turn, selected)| selected.then_some(turn))
                .collect::<Vec<_>>();
            if selected.is_empty() {
                return Ok(None);
            }
            selected
        }
    };
    if turns.is_empty() {
        print_nothing_to_pop();
        return Ok(None);
    }

    let memory = MemoryStore::new(config, paths);
    archive_and_delete_visible_turns(state, &memory, &turns)?;
    let memory_config = config.memory_config();
    Ok(Some(PopOutcome {
        turns: turns.len(),
        archived: memory_config.enabled && memory_config.evicted_context_enabled,
    }))
}

pub(crate) fn validate_pop_count(count: usize) -> Result<usize> {
    if count == 0 {
        bail!(
            "{}",
            t("pop count must be greater than zero", "pop 数量必须大于 0")
        );
    }
    Ok(count)
}

pub(crate) fn parse_positive_pop_count(value: &str) -> std::result::Result<usize, String> {
    let count = value.parse::<usize>().map_err(|_| {
        t(
            "pop count must be a positive integer",
            "pop 数量必须是正整数",
        )
        .to_string()
    })?;
    if count == 0 {
        return Err(t("pop count must be greater than zero", "pop 数量必须大于 0").to_string());
    }
    Ok(count)
}

pub(crate) fn parse_repl_pop_count(args: &str) -> Result<Option<usize>> {
    let mut parts = args.split_whitespace();
    let Some(value) = parts.next() else {
        return Ok(None);
    };
    if parts.next().is_some() {
        bail!(
            "{}",
            t("usage: /pop [positive integer]", "用法：/pop [正整数]")
        );
    }
    let count = parse_positive_pop_count(value).map_err(anyhow::Error::msg)?;
    validate_pop_count(count).map(Some)
}

pub(crate) fn print_pop_outcome(outcome: PopOutcome) {
    let message = if is_zh() {
        if outcome.archived {
            format!("已弹出 {} 轮 · 已归档", outcome.turns)
        } else {
            format!(
                "已弹出 {} 轮 · 未归档（弹出上下文归档已关闭）",
                outcome.turns
            )
        }
    } else {
        let turns = if outcome.turns == 1 { "turn" } else { "turns" };
        if outcome.archived {
            format!("popped {} {turns} · archived", outcome.turns)
        } else {
            format!(
                "popped {} {turns} · not archived (evicted-context archiving is disabled)",
                outcome.turns
            )
        }
    };
    println!("\x1b[2m{message}\x1b[0m\n");
}

pub(crate) fn print_nothing_to_pop() {
    println!(
        "\x1b[2m{}\x1b[0m\n",
        t(
            "no conversation turns are available to pop",
            "没有可弹出的上下文轮次"
        )
    );
}

pub(crate) fn inline_pop_select(turns: &[Turn]) -> Result<Option<Vec<bool>>> {
    let menu_lines = inline_pop_lines(turns.len());
    let visible_items = menu_lines.saturating_sub(2) as usize / 3;
    reserve_inline_fuzzy_space(menu_lines)?;
    let mut session = InlineRawMode::start()?;
    let matcher = SkimMatcherV2::default();
    let search_items = turns.iter().map(pop_search_text).collect::<Vec<_>>();
    let mut active = vec![false; turns.len()];
    let mut query = String::new();
    let mut selected = 0usize;
    let mut scroll = 0usize;
    let (_, cursor_y) = cursor::position().unwrap_or((0, menu_lines.saturating_sub(1)));
    let anchor_y = cursor_y.saturating_sub(menu_lines.saturating_sub(1));
    loop {
        let matches = pop_matches(&matcher, &search_items, &query);
        if selected >= matches.len() {
            selected = matches.len().saturating_sub(1);
        }
        let visible = matches.len().min(visible_items);
        scroll = inline_fuzzy_scroll(selected, scroll, visible);
        draw_inline_pop(
            &mut session.stdout,
            anchor_y,
            menu_lines,
            turns,
            &matches,
            selected,
            scroll,
            &active,
            &query,
        )?;
        if let Event::Key(KeyEvent {
            code, modifiers, ..
        }) = event::read()?
        {
            match code {
                KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => {
                    clear_inline_fuzzy(&mut session.stdout, anchor_y, menu_lines)?;
                    return Ok(None);
                }
                KeyCode::Esc => {
                    clear_inline_fuzzy(&mut session.stdout, anchor_y, menu_lines)?;
                    return Ok(None);
                }
                KeyCode::Char('q') => {
                    clear_inline_fuzzy(&mut session.stdout, anchor_y, menu_lines)?;
                    return Ok(None);
                }
                KeyCode::Enter => {
                    clear_inline_fuzzy(&mut session.stdout, anchor_y, menu_lines)?;
                    return Ok(Some(active));
                }
                KeyCode::Tab => {
                    if let Some(index) = matches.get(selected) {
                        if let Some(value) = active.get_mut(*index) {
                            *value = !*value;
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

pub(crate) fn pop_matches(matcher: &SkimMatcherV2, items: &[String], query: &str) -> Vec<usize> {
    items
        .iter()
        .enumerate()
        .filter_map(|(index, item)| {
            (query.trim().is_empty() || matcher.fuzzy_match(item, query).is_some()).then_some(index)
        })
        .collect()
}

pub(crate) fn draw_inline_pop(
    stdout: &mut io::Stdout,
    anchor_y: u16,
    menu_lines: u16,
    turns: &[Turn],
    matches: &[usize],
    selected: usize,
    scroll: usize,
    active: &[bool],
    query: &str,
) -> Result<()> {
    let (cols, _) = terminal::size().unwrap_or((80, 24));
    let bar = inline_fuzzy_bar();
    let width = (cols as usize).saturating_sub(visible_width(&bar)).max(1);
    let visible_items = menu_lines.saturating_sub(2) as usize / 3;
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
        Print(pop_menu_header(
            query,
            active.iter().filter(|selected| **selected).count(),
            turns.len(),
            width,
        )),
    )?;
    if matches.is_empty() {
        queue!(
            stdout,
            MoveTo(0, anchor_y + 1),
            Print(&bar),
            Print(format!("\x1b[2m{}\x1b[0m", t("no matches", "没有匹配项")))
        )?;
    } else {
        for (row, item_index) in matches.iter().skip(scroll).take(visible_items).enumerate() {
            let focused = scroll + row == selected;
            let checked = active.get(*item_index).copied().unwrap_or(false);
            let lines = pop_menu_turn_lines(&turns[*item_index], focused, checked, width);
            for (line_offset, line) in lines.into_iter().enumerate() {
                queue!(
                    stdout,
                    MoveTo(0, anchor_y + 1 + row as u16 * 3 + line_offset as u16),
                    Print(&bar),
                    Print(line)
                )?;
            }
        }
    }
    queue!(
        stdout,
        MoveTo(0, anchor_y + menu_lines.saturating_sub(1)),
        Print(&bar),
        Print(pop_menu_help_line(width))
    )?;
    stdout.flush()?;
    Ok(())
}

pub(crate) fn pop_menu_header(query: &str, selected: usize, total: usize, width: usize) -> String {
    let title = if query.trim().is_empty() {
        t("Pop context", "弹出上下文").to_string()
    } else {
        format!(
            "{} · {}: {}",
            t("Pop context", "弹出上下文"),
            t("Search", "搜索"),
            query.trim()
        )
    };
    let count = if is_zh() {
        format!("已选 {selected} / {total}")
    } else {
        format!("selected {selected} / {total}")
    };
    let count_width = visible_width(&count);
    if count_width >= width {
        return format!("\x1b[2m{}\x1b[0m", truncate_visible_width(&count, width));
    }
    let title_width = width.saturating_sub(count_width + 1);
    let title = truncate_visible_width(&title, title_width);
    let gap = width
        .saturating_sub(visible_width(&title).saturating_add(count_width))
        .max(1);
    format!(
        "\x1b[1m{title}\x1b[0m{}\x1b[2m{count}\x1b[0m",
        " ".repeat(gap)
    )
}

pub(crate) fn pop_menu_turn_lines(
    turn: &Turn,
    focused: bool,
    checked: bool,
    width: usize,
) -> [String; 3] {
    let cursor = if focused { "›" } else { " " };
    let marker = if checked { "[*]" } else { "[ ]" };
    let lines = [
        format!(
            "{cursor} {marker} {}",
            pop_menu_timestamp(&turn.user_timestamp)
        ),
        format!(
            "      {}{}",
            t("You: ", "你："),
            pop_menu_summary(&turn.user_content)
        ),
        format!(
            "      {}{}",
            t("AI: ", "AI："),
            pop_menu_assistant_summary(turn)
        ),
    ];
    lines.map(|line| {
        let line = truncate_visible_width(&line, width);
        if focused {
            format!("\x1b[1m\x1b[35m{line}\x1b[0m")
        } else if checked {
            format!("\x1b[1m\x1b[32m{line}\x1b[0m")
        } else {
            format!("\x1b[2m{line}\x1b[0m")
        }
    })
}

pub(crate) fn pop_menu_timestamp(timestamp: &str) -> String {
    DateTime::parse_from_rfc3339(timestamp)
        .map(|timestamp| {
            timestamp
                .with_timezone(&Local)
                .format("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_else(|_| pop_menu_summary(timestamp))
}

pub(crate) fn pop_menu_assistant_summary(turn: &Turn) -> String {
    if turn.status == TurnStatus::Interrupted {
        t("(reply interrupted)", "（回复已中断）").to_string()
    } else {
        pop_menu_summary(&turn.assistant_content)
    }
}

pub(crate) fn pop_menu_summary(content: &str) -> String {
    strip_terminal_control_sequences(content)
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_else(|| t("(empty)", "（空）"))
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub(crate) fn pop_search_text(turn: &Turn) -> String {
    format!(
        "{} {} {}",
        pop_menu_timestamp(&turn.user_timestamp),
        pop_menu_summary(&turn.user_content),
        pop_menu_assistant_summary(turn)
    )
}

pub(crate) fn pop_menu_help_line(width: usize) -> String {
    let line = t(
        "Up/Down or j/k move · Tab toggle · Enter pop · Esc/q cancel",
        "↑/↓ 或 j/k 移动 · Tab 勾选 · Enter 弹出 · Esc/q 取消",
    );
    format!("\x1b[2m{}\x1b[0m", truncate_visible_width(line, width))
}

pub(crate) fn inline_pop_lines(item_count: usize) -> u16 {
    let (_, terminal_rows) = terminal::size().unwrap_or((80, 24));
    let available_items = terminal_rows.saturating_sub(2).saturating_div(3).max(1) as usize;
    let visible_items = item_count.min(5).min(available_items).max(1);
    (visible_items as u16).saturating_mul(3).saturating_add(2)
}

pub(crate) async fn run_models(paths: &GQYPaths, args: ModelsArgs) -> Result<()> {
    run_models_for_session(paths, args, None).await
}

/// Switches the model pool of one session (the current session when
/// `session_id` is None). The override persists on the session, so reopening
/// it restores the model; the global pool is managed in `gqy config`.
pub(crate) async fn run_models_for_session(
    paths: &GQYPaths,
    args: ModelsArgs,
    session_id: Option<&str>,
) -> Result<()> {
    let config = AppConfig::load(paths)?;
    let choices = config.text_provider_model_choices();
    if choices.is_empty() {
        bail!(
            "{}",
            t(
                "no configured provider models; configure a model first",
                "没有已配置的 provider 模型；请先配置模型",
            )
        );
    }
    if let Some(target) = args.target.as_deref() {
        let target = target.trim();
        if target.eq_ignore_ascii_case("default") || target.eq_ignore_ascii_case("global") {
            set_session_models(paths, session_id, Vec::new()).await?;
            println!(
                "{}",
                t(
                    "this session now follows the global active pool",
                    "当前会话已恢复跟随全局激活模型池"
                )
            );
            return Ok(());
        }
        let choice = crate::config::resolve_provider_model_argument(&choices, target)
            .map_err(anyhow::Error::msg)?;
        let label = choice.label();
        let models = vec![ActiveProviderModelConfig {
            provider_id: choice.provider_id.clone(),
            model: choice.model.clone(),
        }];
        set_session_models(paths, session_id, models).await?;
        println!("{}: {label}", t("session model", "当前会话模型"));
        return Ok(());
    }
    if io::stdout().is_terminal() && io::stdin().is_terminal() {
        let override_pool = session_model_override_snapshot(paths, session_id)?;
        let initial = choices
            .iter()
            .map(|choice| match override_pool.as_deref() {
                Some(pool) => pool.iter().any(|model| {
                    model.provider_id == choice.provider_id && model.model == choice.model
                }),
                None => config.is_active_provider_model(&choice.provider_id, &choice.model),
            })
            .collect::<Vec<_>>();
        if let Some(active) = inline_fuzzy_select(
            &choices
                .iter()
                .map(|choice| choice.label())
                .collect::<Vec<_>>(),
            initial.clone(),
        )? {
            if active == initial {
                println!(
                    "{}",
                    t(
                        "no changes (Enter picks the highlighted model; Tab multi-selects)",
                        "未做修改（回车=选定高亮模型,Tab=多选勾选）"
                    )
                );
                return Ok(());
            }
            let models = choices
                .iter()
                .zip(active)
                .filter_map(|(choice, active)| {
                    active.then(|| ActiveProviderModelConfig {
                        provider_id: choice.provider_id.clone(),
                        model: choice.model.clone(),
                    })
                })
                .collect::<Vec<_>>();
            let cleared = models.is_empty();
            set_session_models(paths, session_id, models).await?;
            if cleared {
                println!(
                    "{}",
                    t(
                        "this session now follows the global active pool",
                        "当前会话已恢复跟随全局激活模型池"
                    )
                );
            } else {
                println!("{}", t("session models updated", "已更新当前会话模型"));
            }
        }
        return Ok(());
    }
    print_model_choices(&config, &choices, None);
    Ok(())
}

pub(crate) const DEFAULT_PERSONA_LABEL_ZH: &str = "GQY（内置默认）";
pub(crate) const DEFAULT_PERSONA_LABEL_EN: &str = "GQY (built-in default)";

pub(crate) fn list_persona_files(paths: &GQYPaths, config: &AppConfig) -> Result<Vec<String>> {
    let dir = config.prompts_dir_path(paths);
    let mut names = Vec::new();
    if dir.exists() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            if entry.file_type()?.is_file() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.ends_with(".md") && !name.eq_ignore_ascii_case("system-prompt.md") {
                    names.push(name);
                }
            }
        }
    }
    names.sort();
    Ok(names)
}

/// Interactive persona picker (single-select). Returns true when the active
/// persona changed and the config was saved.
pub(crate) fn run_persona_picker(paths: &GQYPaths, argument: &str) -> Result<bool> {
    let mut config = AppConfig::load(paths)?;
    let personas = list_persona_files(paths, &config)?;
    let current = config.prompt.active_persona.trim().to_string();
    let argument = argument.trim();
    let chosen: Option<String> = if !argument.is_empty() {
        if argument.eq_ignore_ascii_case("default")
            || argument.eq_ignore_ascii_case("gqy")
            || argument == "内置"
        {
            Some(String::new())
        } else {
            let needle = argument.to_ascii_lowercase();
            let matched = personas.iter().find(|name| {
                name.eq_ignore_ascii_case(argument)
                    || name
                        .to_ascii_lowercase()
                        .trim_end_matches(".md")
                        .contains(needle.trim_end_matches(".md"))
            });
            match matched {
                Some(name) => Some(name.clone()),
                None => bail!(
                    "{}: {argument}",
                    t("no persona file matches", "没有匹配的人格文件")
                ),
            }
        }
    } else if io::stdout().is_terminal() && io::stdin().is_terminal() {
        let default_label = t(DEFAULT_PERSONA_LABEL_EN, DEFAULT_PERSONA_LABEL_ZH).to_string();
        let mut items = vec![default_label];
        items.extend(personas.iter().cloned());
        let initial = if current.is_empty() {
            0
        } else {
            personas
                .iter()
                .position(|name| *name == current)
                .map(|index| index + 1)
                .unwrap_or(0)
        };
        match inline_fuzzy_select_single(&items, initial)? {
            Some(0) => Some(String::new()),
            Some(index) => personas.get(index - 1).cloned(),
            None => None,
        }
    } else {
        println!(
            "{}: {}",
            t("current persona", "当前人格"),
            if current.is_empty() {
                t(DEFAULT_PERSONA_LABEL_EN, DEFAULT_PERSONA_LABEL_ZH).to_string()
            } else {
                current.clone()
            }
        );
        for name in &personas {
            println!("  {name}");
        }
        println!(
            "{}",
            t("switch with: /persona <name>", "切换：/persona <名称>")
        );
        return Ok(false);
    };
    let Some(target) = chosen else {
        return Ok(false);
    };
    if target == current {
        println!("{}", t("no changes", "未做修改"));
        return Ok(false);
    }
    config.prompt.active_persona = target.clone();
    config.save(paths)?;
    println!(
        "{}: {}",
        t("active persona", "当前人格"),
        if target.is_empty() {
            t(DEFAULT_PERSONA_LABEL_EN, DEFAULT_PERSONA_LABEL_ZH).to_string()
        } else {
            target
        }
    );
    Ok(true)
}

/// One line per context watermark: absolute tokens left before each tier
/// fires (soft notice / mechanical prune / compaction / forced compaction).
/// Absolute values, not percentages — same reasoning as the cache accounting
/// log line.
pub(crate) fn compact_watermark_text(
    context_tokens: usize,
    window: usize,
    context: &crate::config::ContextConfig,
) -> String {
    let tier = |label: &str, ratio: f32| -> String {
        let threshold = (window as f32 * ratio).max(1.0) as usize;
        if context_tokens >= threshold {
            format!("{label} {}✓", t("reached", "已达"))
        } else {
            format!("{label} -{}", threshold - context_tokens)
        }
    };
    format!(
        "{}: {} / {} · {}",
        t("Context watermarks", "上下文水位"),
        context_tokens,
        window,
        [
            tier(t("notice", "提示"), context.compact_soft_ratio),
            tier(t("prune", "折叠"), context.compact_snip_ratio),
            tier(t("compact", "压缩"), context.trim_at_ratio),
            tier(t("force", "强制"), context.compact_force_ratio),
        ]
        .join(" · ")
    )
}

pub(crate) fn usage_overview_text(
    snapshot: &crate::state::UsageSnapshot,
    context: Option<(u64, Option<usize>)>,
) -> String {
    let compact = render::format_compact_count;
    let mut lines = Vec::new();
    lines.push(format!(
        "\x1b[1m{}\x1b[0m \x1b[2m{}\x1b[0m",
        t("Token usage", "Token 用量"),
        t(
            "(global totals: all sessions + background calls)",
            "（全局累计：含所有会话与后台调用）"
        )
    ));
    lines.push(format!(
        "  {:<10} {}",
        t("requests", "请求次数"),
        compact(snapshot.requests)
    ));
    let cached = snapshot.cache_read_tokens;
    let fresh = snapshot.prompt_tokens.saturating_sub(cached);
    let mut input_line = format!(
        "  {:<10} {}",
        t("input", "输入"),
        compact(snapshot.prompt_tokens)
    );
    if cached > 0 {
        input_line.push_str(&format!(
            "\x1b[2m（{} {} · {} {}",
            t("cache hits", "缓存命中"),
            compact(cached),
            t("billed new", "计费新输入"),
            compact(fresh)
        ));
        if snapshot.cache_write_tokens > 0 {
            input_line.push_str(&format!(
                " · {} {}",
                t("cache writes", "缓存写入"),
                compact(snapshot.cache_write_tokens)
            ));
        }
        input_line.push_str("）\x1b[0m");
    }
    lines.push(input_line);
    let mut output_line = format!(
        "  {:<10} {}",
        t("output", "输出"),
        compact(snapshot.completion_tokens)
    );
    if snapshot.reasoning_tokens > 0 {
        output_line.push_str(&format!(
            "\x1b[2m（{} {}）\x1b[0m",
            t("reasoning", "其中思考"),
            compact(snapshot.reasoning_tokens)
        ));
    }
    lines.push(output_line);
    lines.push(format!(
        "  {:<10} {} \x1b[2m· {} Σ{}\x1b[0m",
        t("total", "总计"),
        compact(snapshot.total_tokens),
        t("conversation", "对话口径"),
        compact(snapshot.conversation_tokens)
    ));
    if let Some(last) = snapshot
        .last_conversation_usage
        .as_ref()
        .or(snapshot.last_usage.as_ref())
    {
        let mut line = format!(
            "  {:<10} in {}",
            t("last turn", "最近一轮"),
            compact(last.prompt_tokens)
        );
        if last.cache_read_tokens > 0 {
            line.push_str(&format!("(C{})", compact(last.cache_read_tokens)));
        }
        line.push_str(&format!(" · out {}", compact(last.completion_tokens)));
        lines.push(line);
    }
    if let Some((tokens, window)) = context {
        let window = window
            .map(|value| render::format_compact_count(value as u64))
            .unwrap_or_else(|| "?".to_string());
        lines.push(format!(
            "  {:<10} {} / {window}",
            t("context", "会话上下文"),
            compact(tokens)
        ));
    }
    lines.join("\n")
}

/// Suggested archive name when the user did not pick one.
pub(crate) fn default_export_name() -> String {
    let host = std::fs::read_to_string("/etc/hostname")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "gqy".to_string());
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    format!("gqy-export-{host}-{stamp}.tar.gz")
}

pub(crate) fn readable_bytes(bytes: u64) -> String {
    pub(crate) const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit + 1 < UNITS.len() {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} {}", UNITS[0])
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

/// `t` for messages built at runtime — the static version cannot take a `format!`.
pub(crate) fn owned(en: String, zh: String) -> String {
    if crate::i18n::is_zh() {
        zh
    } else {
        en
    }
}

pub(crate) fn run_export(paths: &GQYPaths, args: ExportArgs) -> Result<()> {
    let output = args
        .output
        .unwrap_or_else(|| PathBuf::from(default_export_name()));
    let options = crate::transfer::export::ExportOptions {
        all: args.all,
        index: args.index,
        platforms: args.platforms,
        no_secrets: args.no_secrets,
        dry_run: args.dry_run,
        force: args.force,
    };
    let report = crate::transfer::export::export(paths, &output, &options)?;

    if options.dry_run {
        for (unit, bytes) in &report.by_unit {
            println!("  {:>10}  {unit}", readable_bytes(*bytes));
        }
    }

    let count = report.entries;
    let size = readable_bytes(report.bytes);
    println!(
        "{}",
        match &report.archive {
            None => owned(
                format!("Dry run: {count} files, {size} would be packed."),
                format!("试运行：将打包 {count} 个文件，共 {size}。"),
            ),
            Some(path) => {
                let path = path.display();
                owned(
                    format!("Exported {count} files ({size}) to {path}"),
                    format!("已导出 {count} 个文件（{size}）到 {path}"),
                )
            }
        }
    );

    // The archive is plaintext-credentialed unless asked otherwise; the user
    // needs to know that before they put it on a USB stick or a chat app.
    if report.secrets_included && report.archive.is_some() {
        eprintln!(
            "{}",
            t(
                "Warning: this archive contains API keys and access tokens in plain text. Keep it private, or re-export with --no-secrets.",
                "警告：归档内含明文 API key 与访问令牌。请妥善保管，或改用 --no-secrets 重新导出。",
            )
        );
    }
    if !options.all && !options.index {
        println!(
            "{}",
            t(
                "The knowledge-base vector index was left out; run `gqy kb embed` after importing (or re-export with --index).",
                "未包含知识库向量索引；导入后请运行 gqy kb embed（或改用 --index 重新导出）。",
            )
        );
    }
    Ok(())
}

pub(crate) async fn run_import(paths: &GQYPaths, args: ImportArgs) -> Result<()> {
    // The daemon holds conversation.db's WAL open; replacing the file under it
    // would leave both the old process and the new database inconsistent.
    if crate::ipc::daemon_info(paths).await.is_some() {
        anyhow::bail!(
            "{}",
            t(
                "the GQY daemon is running and holds the database open; stop it first with `gqy daemon stop`",
                "GQY daemon 正在运行并占用数据库；请先执行 gqy daemon stop",
            )
        );
    }

    let options = crate::transfer::import::ImportOptions { force: args.force };
    let report = crate::transfer::import::import(paths, &args.archive, &options)?;

    if let Some(backup) = &report.backup {
        let path = backup.display();
        println!(
            "{}",
            owned(
                format!("Backed up the previous installation to {path}"),
                format!("已把覆盖前的安装备份到 {path}"),
            )
        );
    }
    let restored = report.restored;
    println!(
        "{}",
        owned(
            format!("Restored {restored} files."),
            format!("已恢复 {restored} 个文件。"),
        )
    );
    if !report.unknown_units.is_empty() {
        // A newer GQY wrote data this build has no name for. It is on disk;
        // say so rather than let it look like it vanished.
        let units = report
            .unknown_units
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        println!(
            "{}",
            owned(
                format!(
                    "Restored data this version does not recognise \
                     (written by a newer GQY): {units}"
                ),
                format!("恢复了本版本不认识的数据（由更新版本的 GQY 写入）：{units}"),
            )
        );
    }
    if report.cleared_workspaces > 0 {
        let cleared = report.cleared_workspaces;
        println!(
            "{}",
            owned(
                format!(
                    "Cleared {cleared} session workspace(s) pointing at \
                     directories this machine does not have."
                ),
                format!("已清除 {cleared} 个指向本机不存在目录的会话工作区。"),
            )
        );
    }

    println!("\n{}", t("Next steps:", "接下来需要手动完成："));
    println!(
        "  {}",
        t(
            "reinstall the shell integration: `gqy fish-init` / `bash-init` / `zsh-init`",
            "重装 shell 集成：gqy fish-init / bash-init / zsh-init",
        )
    );
    println!(
        "  {}",
        t(
            "`gqy kb reindex` — the knowledge base records absolute paths from the old machine",
            "gqy kb reindex —— 知识库记录的是旧机器上的绝对路径",
        )
    );
    if !report.index_included {
        println!(
            "  {}",
            t(
                "`gqy kb embed` — the vector index was not in the archive",
                "gqy kb embed —— 归档中不含向量索引",
            )
        );
    }
    if !report.secrets_included {
        println!(
            "  {}",
            t(
                "refill API keys and access tokens: `gqy config`",
                "补填 API key 与访问令牌：gqy config",
            )
        );
    }
    Ok(())
}

pub(crate) fn run_list_models(paths: &GQYPaths) -> Result<()> {
    let config = AppConfig::load(paths)?;
    let choices = config.text_provider_model_choices();
    if choices.is_empty() {
        bail!(
            "{}",
            t(
                "no configured provider models; configure a model first",
                "没有已配置的 provider 模型；请先配置模型",
            )
        );
    }
    let override_pool = session_model_override_snapshot(paths, None)?;
    print_model_choices(&config, &choices, override_pool.as_deref());
    println!(
        "{}",
        t(
            "switch with: gqy models <index|provider/model>; 'gqy models default' follows the global pool",
            "切换：gqy models <序号|供应商/模型>；gqy models default 恢复跟随全局模型池"
        )
    );
    Ok(())
}

pub(crate) fn print_model_choices(
    config: &AppConfig,
    choices: &[crate::config::ProviderModelChoice],
    override_pool: Option<&[ActiveProviderModelConfig]>,
) {
    for (index, choice) in choices.iter().enumerate() {
        let active = match override_pool {
            Some(pool) => pool.iter().any(|model| {
                model.provider_id == choice.provider_id && model.model == choice.model
            }),
            None => config.is_active_provider_model(&choice.provider_id, &choice.model),
        };
        let marker = if active { "[*]" } else { "[ ]" };
        println!("{marker} {}. {}", index + 1, choice.label());
    }
    match override_pool {
        Some(_) => println!(
            "{}",
            t(
                "[*] = models pinned to the current session",
                "[*] = 当前会话固定使用的模型"
            )
        ),
        None => println!(
            "{}",
            t(
                "[*] = global active pool (the current session follows it)",
                "[*] = 全局激活模型池（当前会话跟随全局）"
            )
        ),
    }
}

/// Reads a session's model override straight from the shared state database;
/// works whether or not the daemon is running.
pub(crate) fn session_model_override_snapshot(
    paths: &GQYPaths,
    session_id: Option<&str>,
) -> Result<Option<Vec<ActiveProviderModelConfig>>> {
    let store = StateStore::new(paths)?;
    let session_id = match session_id {
        Some(session_id) => session_id.to_string(),
        None => store.session_id().to_string(),
    };
    store.session_model_override(&session_id)
}

pub(crate) async fn set_session_models(
    paths: &GQYPaths,
    session_id: Option<&str>,
    models: Vec<ActiveProviderModelConfig>,
) -> Result<()> {
    if ipc::daemon_info(paths).await.is_some() {
        let target = match session_id {
            Some(id) => ipc::SessionRef::Id { id: id.to_string() },
            None => ipc::SessionRef::Current,
        };
        send_ipc_command(paths, IpcCommand::SetSessionModels { target, models }).await?;
        return Ok(());
    }
    let config = AppConfig::load(paths)?;
    let choices = config.text_provider_model_choices();
    for model in &models {
        if !choices
            .iter()
            .any(|choice| choice.provider_id == model.provider_id && choice.model == model.model)
        {
            bail!(
                "{}{}/{}",
                t("unknown model: ", "未知模型："),
                model.provider_id,
                model.model
            );
        }
    }
    let store = StateStore::new(paths)?;
    let session_id = match session_id {
        Some(session_id) => session_id.to_string(),
        None => store.session_id().to_string(),
    };
    store.set_session_model_override(
        &session_id,
        (!models.is_empty()).then_some(models.as_slice()),
    )
}

pub(crate) fn inline_fuzzy_select(
    items: &[String],
    mut active: Vec<bool>,
) -> Result<Option<Vec<bool>>> {
    let menu_lines = inline_fuzzy_lines(items.len());
    reserve_inline_fuzzy_space(menu_lines)?;
    let mut session = InlineRawMode::start()?;
    let matcher = SkimMatcherV2::default();
    let mut query = String::new();
    let mut selected = 0usize;
    let mut scroll = 0usize;
    // 验收三轮:用户搜到模型直接回车,期望"切到它";多选语义却要求
    // Tab 勾选,回车成了"确认没改"→静默未做修改。记住入场快照与是否
    // 表达过意图(搜索/移动),回车时没动过勾选就按单选切换处理。
    let initial = active.clone();
    let mut navigated = false;
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
                    return Ok(Some(active));
                }
                KeyCode::Char('q') if query.is_empty() => {
                    clear_inline_fuzzy(&mut session.stdout, anchor_y, menu_lines)?;
                    return Ok(Some(active));
                }
                KeyCode::Enter => {
                    clear_inline_fuzzy(&mut session.stdout, anchor_y, menu_lines)?;
                    if active == initial && (navigated || !query.is_empty()) {
                        if let Some((_, index)) = matches.get(selected) {
                            let mut solo = vec![false; active.len()];
                            solo[*index] = true;
                            return Ok(Some(solo));
                        }
                    }
                    return Ok(Some(active));
                }
                KeyCode::Tab => {
                    if let Some((_, index)) = matches.get(selected) {
                        if let Some(value) = active.get_mut(*index) {
                            *value = !*value;
                        }
                    }
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    navigated = true;
                    selected = selected.saturating_sub(1);
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    navigated = true;
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
