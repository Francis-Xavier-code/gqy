//! direct — 自 src/cli.rs 拆分。

#![allow(clippy::type_complexity)]
pub(crate) use super::*;

pub(crate) fn direct_mode_requested() -> bool {
    std::env::var_os("GQY_DIRECT").is_some_and(|value| value != "0")
}

pub(crate) async fn run_direct_repl(paths: &GQYPaths, initial_mode: AgentMode) -> Result<()> {
    let _core_lease = ipc::acquire_direct_core(paths)?;
    initialize_models_cache(paths);
    let _cursor_restore = ReplCursorRestore;
    AppConfig::init_files(paths)?;
    let mut config = AppConfig::load_or_default(paths)?;
    tools::jobs::init(paths);
    let state = StateStore::new(paths)?;
    state.init_files()?;
    // Same lane as the remote REPL: resume where the last REPL was, not where
    // shell-hook happens to be pointing.
    let persona = if initial_mode == AgentMode::Dev {
        crate::state::DEV_PERSONA.to_string()
    } else {
        config.active_persona_scope()
    };
    if let Ok(Some(session_id)) = state.repl_session(&persona) {
        state.adopt_session(&session_id);
    } else if initial_mode == AgentMode::Dev {
        // dev 无「终端会话」可退:自举一个 dev 会话并钉住指针。
        let record = state.create_session(
            crate::state::DEV_PERSONA,
            "",
            crate::state::USER_SESSION_KIND,
            None,
        )?;
        state.adopt_session(&record.session_id);
        let _ = state.set_repl_session(&persona, &record.session_id);
    } else {
        let _ = state.set_repl_session(&persona, &state.session_id());
    }
    apply_session_model_override(&state, &mut config);
    let memory_organizer = MemoryOrganizer::spawn()?;
    let memory_organizer_handle = memory_organizer.handle();
    memory_organizer_handle.wake(config.clone(), paths.clone(), state.clone());
    let mut client = OpenAiCompatibleClient::from_config(&config, paths)?;
    let mut mode = initial_mode;
    let mut input_history = load_repl_input_history(&state, paths)?;
    let mut prefill = None::<String>;
    let mut live_repl = None::<LiveReplTail>;

    crate::default_kb::check_update_if_due(paths).ok();
    if let Ok(Some(message)) = crate::default_kb::notice_if_update_available(paths) {
        println!("\x1b[2m{message}\x1b[0m");
    }
    let mut cumulative_tokens = state.session_cumulative_token_totals().unwrap_or_default();
    let mut show_shortcut_hint = true;
    let initial_registry =
        build_tool_registry(&config, paths, mode, crate::question_tui::available(false))?;
    let mut agent = Agent::new(
        config.clone(),
        paths,
        state.clone(),
        client.clone(),
        initial_registry,
        mode,
    )?;
    agent.set_memory_organizer(memory_organizer_handle);
    agent.prepare_for_turn()?;
    let mut footer = ReplFooterStatus::from_config(
        &config,
        agent.effective_context_tokens()?,
        TurnTokens::default(),
    );
    let thinking_summary = client.thinking_variant_summary();
    footer.update_thinking_variant(thinking_summary.as_deref());
    footer.update_context_window(agent.context_window());
    loop {
        let thinking_summary = client.thinking_variant_summary();
        footer.update_thinking_variant(thinking_summary.as_deref());
        let next_input = if let Some(live) = live_repl.as_mut() {
            live.set_footer(footer.clone());
            let input = match read_live_repl_input(live, paths, &JobsFeed::Local, None)? {
                LiveReplOutcome::Exit | LiveReplOutcome::FollowWake { .. } => None,
                // Direct mode owns its jobs in-process, so stop them here
                // rather than through the daemon.
                LiveReplOutcome::StopJobs => {
                    for job in crate::tools::jobs::overview() {
                        if job.running {
                            let _ = crate::tools::jobs::stop_job(&job.job_id).await;
                        }
                    }
                    continue;
                }
                LiveReplOutcome::Submit(next_mode, input, images) => {
                    Some((next_mode, input, images))
                }
            };
            // The user moved on: finished background commands count as
            // reported in direct mode (no daemon wake exists here).
            for job in crate::tools::jobs::overview() {
                if !job.running {
                    crate::tools::jobs::acknowledge(&job.job_id);
                }
            }
            input
        } else {
            read_repl_input(
                paths,
                mode,
                prefill.take(),
                &input_history,
                &footer,
                show_shortcut_hint,
            )?
        };
        let (input, pasted_images) = match next_input {
            Some((new_mode, input, pasted_images)) => {
                mode = new_mode;
                (input, pasted_images)
            }
            None => break,
        };
        let input = input.trim();
        let (command_input, command_args) = split_repl_command(input);
        let command = resolve_repl_command(command_input);
        let command_args_empty = command_args.trim().is_empty();
        if input.eq_ignore_ascii_case("exit")
            || input.eq_ignore_ascii_case("quit")
            || (command.eq_ignore_ascii_case("/exit") && command_args_empty)
        {
            break;
        }
        if command.eq_ignore_ascii_case("/help") && command_args_empty {
            print_repl_help();
            continue;
        }
        if command.eq_ignore_ascii_case("/usage") && command_args_empty {
            let snapshot = state.usage_snapshot()?;
            let context_tokens = agent.effective_context_tokens()?;
            let context = Some((context_tokens, agent.context_window()));
            println!("{}", usage_overview_text(&snapshot, context));
            if let Some(window) = agent.context_window() {
                println!(
                    "{}",
                    compact_watermark_text(context_tokens as usize, window, &config.context)
                );
            }
            println!();
            continue;
        }
        if command.eq_ignore_ascii_case("/persona") {
            match run_persona_picker(paths, command_args) {
                Ok(true) => {
                    reload_repl_config(paths, &state, &mut config, &mut client)?;
                    footer = ReplFooterStatus::from_config(
                        &config,
                        agent.effective_context_tokens()?,
                        cumulative_tokens,
                    );
                    let thinking_summary = client.thinking_variant_summary();
                    footer.update_thinking_variant(thinking_summary.as_deref());
                    let registry = build_tool_registry(
                        &config,
                        paths,
                        mode,
                        crate::question_tui::available(false),
                    )?;
                    agent.reload_config(config.clone(), client.clone())?;
                    agent.switch_mode(mode, registry);
                    footer.update_context_window(agent.context_window());
                    println!("{}", t("configuration reloaded", "配置已重新加载"));
                }
                Ok(false) => {}
                Err(error) => println!("\x1b[31m{error:#}\x1b[0m"),
            }
            println!();
            continue;
        }
        if command.eq_ignore_ascii_case("/models") {
            let argument = command_args.trim();
            let repl_session_id = state.session_id();
            run_models_for_session(
                paths,
                ModelsArgs {
                    target: (!argument.is_empty()).then(|| argument.to_string()),
                },
                Some(&repl_session_id),
            )
            .await?;
            reload_repl_config(paths, &state, &mut config, &mut client)?;
            footer = ReplFooterStatus::from_config(
                &config,
                agent.effective_context_tokens()?,
                cumulative_tokens,
            );
            let thinking_summary = client.thinking_variant_summary();
            footer.update_thinking_variant(thinking_summary.as_deref());
            let registry =
                build_tool_registry(&config, paths, mode, crate::question_tui::available(false))?;
            agent.reload_config(config.clone(), client.clone())?;
            agent.switch_mode(mode, registry);
            footer.update_context_window(agent.context_window());
            if let Some(live) = live_repl.as_mut() {
                live.set_footer(footer.clone());
            }
            println!("{}", t("configuration reloaded", "配置已重新加载"));
            println!();
            continue;
        }
        if command.eq_ignore_ascii_case("/config") && command_args_empty {
            crate::config_tui::run(paths)?;
            reload_repl_config(paths, &state, &mut config, &mut client)?;
            footer = ReplFooterStatus::from_config(
                &config,
                agent.effective_context_tokens()?,
                cumulative_tokens,
            );
            let thinking_summary = client.thinking_variant_summary();
            footer.update_thinking_variant(thinking_summary.as_deref());
            let registry =
                build_tool_registry(&config, paths, mode, crate::question_tui::available(false))?;
            agent.reload_config(config.clone(), client.clone())?;
            agent.switch_mode(mode, registry);
            footer.update_context_window(agent.context_window());
            if let Some(live) = live_repl.as_mut() {
                live.set_footer(footer.clone());
            }
            println!("{}", t("configuration reloaded", "配置已重新加载"));
            println!();
            continue;
        }
        if command.eq_ignore_ascii_case("/variant") {
            if !crate::models_cache::is_loaded() {
                println!(
                    "{}\n",
                    t(
                        "model metadata is still loading; try /variant again shortly",
                        "模型元数据仍在加载，请稍后重试 /variant"
                    )
                );
                continue;
            }
            let selected = command_args.trim();
            match execute_variant(
                paths,
                &mut client,
                (!selected.is_empty()).then_some(selected),
                "/variant",
            )? {
                VariantOutcome::Updated => {
                    let thinking_summary = client.thinking_variant_summary();
                    footer.update_thinking_variant(thinking_summary.as_deref());
                    agent.replace_client(client.clone());
                    print_variant_updated();
                }
                VariantOutcome::Cancelled => {}
                VariantOutcome::Rejected(message) => {
                    eprintln!("\x1b[31m{message}\x1b[0m");
                }
            }
            continue;
        }
        if command.eq_ignore_ascii_case("/undo") && command_args_empty {
            let (removed, prompt) = state.undo_last_turn()?;
            footer.update_session_tokens(agent.effective_context_tokens()?);
            if removed > 0 && prompt.is_none() {
                println!("{}", t("context compaction undone", "已撤销上下文压缩"));
            } else {
                println!("{}: {removed}", t("undone messages", "已撤销消息数"));
            }
            if let Some(prompt) = prompt {
                if let Some(live) = live_repl.as_mut() {
                    live.editor.input = prompt;
                    live.editor.cursor = live.editor.input.chars().count();
                    live.editor.history_clean_index = None;
                } else {
                    prefill = Some(prompt);
                }
            }
            continue;
        }
        if command.eq_ignore_ascii_case("/pop") {
            let count = match parse_repl_pop_count(command_args) {
                Ok(count) => count,
                Err(err) => {
                    eprintln!("\x1b[31m{}: {err}\x1b[0m", t("error", "错误"));
                    continue;
                }
            };
            state.recover_stale_turns()?;
            match execute_pop(paths, &config, &state, count) {
                Ok(Some(outcome)) => {
                    print_pop_outcome(outcome);
                    footer.update_session_tokens(agent.effective_context_tokens()?);
                }
                Ok(None) => {}
                Err(err) => {
                    eprintln!("\x1b[31m{}: {err}\x1b[0m", t("error", "错误"));
                }
            }
            continue;
        }
        if command.eq_ignore_ascii_case("/compact") && command_args_empty {
            let reasoning_mode =
                render::ReasoningDisplayMode::from_config(&config.display.reasoning);
            let tool_call_mode =
                render::ToolCallDisplayMode::from_config(&config.display.tool_calls);
            let mut renderer = render::StreamRenderer::new(
                reasoning_mode,
                tool_call_mode,
                false,
                config.display.readable_tool_names,
                config.display.command_output_lines,
            );
            match agent
                .compact_now(|event| handle_agent_event(&mut renderer, event))
                .await
            {
                Ok(Some(result)) => {
                    renderer.finish()?;
                    if let Some(usage) = result.usage.as_ref() {
                        cumulative_tokens.add(TurnTokens::from_usage(Some(usage)));
                    }
                    footer.update_token_usage(
                        &result,
                        agent.effective_context_tokens()?,
                        agent.context_window(),
                        cumulative_tokens,
                    );
                    if config.display.show_token_usage {
                        print_chat_token_usage(
                            &result,
                            true,
                            agent.effective_context_tokens()?,
                            agent.context_window(),
                            cumulative_tokens,
                        )?;
                    }
                }
                Ok(None) => {
                    renderer.finish()?;
                    println!(
                        "\x1b[2m{}\x1b[0m",
                        t("nothing to compact", "没有可压缩的上下文")
                    );
                    footer.update_session_tokens(agent.effective_context_tokens()?);
                }
                Err(err) => {
                    renderer.finish()?;
                    eprintln!("\x1b[31m{}: {err}\x1b[0m", t("error", "错误"));
                }
            }
            continue;
        }
        if command.eq_ignore_ascii_case("/reset-memory") {
            if !confirm_stdin(t(
                "erase this mode's long-term memory (facts, diary, episodes)?",
                "确认清空当前模式的长期记忆（事实/日记/经历）？",
            ))? {
                println!("{}", t("cancelled", "已取消"));
                continue;
            }
            agent.wipe_memory()?;
            println!("{}", t("long-term memory erased", "长期记忆已清空"));
            continue;
        }
        if command.eq_ignore_ascii_case("/reset") && command_args.trim().is_empty() {
            run_reset(paths).await?;
            cumulative_tokens = TurnTokens::default();
            footer.reset_token_usage(agent.effective_context_tokens()?, agent.context_window());
            // 直连道同病同修(验收问题四):不重绘,Σ 旧数一直挂在屏上。
            if let Some(live) = live_repl.as_mut() {
                live.queued.clear();
                live.refresh_footer(footer.clone())?;
            }
            continue;
        }
        if command.eq_ignore_ascii_case("/wipe") {
            println!("{}", wipe_summary());
            if !confirm_stdin(t("wipe everything?", "确认全部抹掉？"))? {
                println!("{}", t("cancelled", "已取消"));
                continue;
            }
            run_wipe(paths, true).await?;
            agent.reset_memory()?;
            cumulative_tokens = TurnTokens::default();
            footer.reset_token_usage(agent.effective_context_tokens()?, agent.context_window());
            if let Some(live) = live_repl.as_mut() {
                live.queued.clear();
                live.refresh_footer(footer.clone())?;
            }
            continue;
        }
        // 命令泄漏守门(任务#14):直连道的 if 链只实现了命令表的子集,
        // 落到这里的表内命令(如 /new /session)以前会原文发给模型当聊天
        // ——人格实验冒烟时实锤过。现在一律拦下提示,绝不进对话;完整的
        // 双 dispatch 后端归一记为技术债,此守门先消灭整个 bug 类。
        if input.starts_with('/') {
            let known = REPL_COMMAND_TABLE
                .iter()
                .any(|spec| spec.name.eq_ignore_ascii_case(&command));
            if known {
                println!(
                    "{}",
                    t(
                        "this command needs the full (daemon) REPL; start without GQY_DIRECT to use it",
                        "该命令需要完整(daemon)REPL;不带 GQY_DIRECT 启动即可使用"
                    )
                );
            } else {
                println!("{}: {command_input}", t("unknown command", "未知命令"));
            }
            continue;
        }
        if input.is_empty() {
            continue;
        }
        input_history.push(input.to_string());
        persist_repl_history_entry(paths, input);
        if let Some(live) = live_repl.as_mut() {
            live.editor.record_history(input);
        }
        if agent.mode() != mode {
            let registry =
                build_tool_registry(&config, paths, mode, crate::question_tui::available(false))?;
            agent.switch_mode(mode, registry);
        }
        agent.prepare_for_turn()?;
        let reasoning_mode = render::ReasoningDisplayMode::from_config(&config.display.reasoning);
        let tool_call_mode = render::ToolCallDisplayMode::from_config(&config.display.tool_calls);
        let mut renderer = render::StreamRenderer::new(
            reasoning_mode,
            tool_call_mode,
            false,
            config.display.readable_tool_names,
            config.display.command_output_lines,
        );
        let control = AgentTurnControl::new(
            mode,
            build_tool_registry(
                &config,
                paths,
                AgentMode::Normal,
                crate::question_tui::available(false),
            )?,
            build_tool_registry(
                &config,
                paths,
                AgentMode::Dev,
                crate::question_tui::available(false),
            )?,
        );
        if live_repl.is_none() {
            live_repl = Some(LiveReplTail::new(
                mode,
                input_history.clone(),
                state.load_queued_prompts()?,
                footer.clone(),
            )?);
        }
        let live = live_repl.as_mut().expect("live REPL was initialized");
        let chat_result = run_live_agent_turn(
            live,
            paths,
            &state,
            &mut agent,
            LiveAgentInput {
                content: input,
                images: &pasted_images,
            },
            &control,
            &mut renderer,
        )
        .await;
        mode = live.mode();
        match chat_result {
            Ok(Some(result)) => {
                let context_window =
                    result_context_window(&config, &result).or(agent.context_window());
                let mut turn_tokens = TurnTokens::from_usage(result.usage.as_ref());
                if let Some(usage) = result.usage.as_ref() {
                    cumulative_tokens.add(TurnTokens::from_usage(Some(usage)));
                }
                let context_tokens = agent.effective_context_tokens()?;
                footer.update_token_usage(
                    &result,
                    context_tokens,
                    context_window,
                    cumulative_tokens,
                );
                let endpoint_variant = result.provider_id.as_deref().and_then(|provider_id| {
                    result
                        .model
                        .as_deref()
                        .and_then(|model| client.thinking_variant_for(provider_id, model))
                });
                if show_mixed_model_endpoint(&config, true) {
                    let provider = result.provider_id.as_deref().unwrap_or("-");
                    let model = result.model.as_deref().unwrap_or("-");
                    let frame = format!(
                        "\x1b[2m{}\x1b[0m\n",
                        mixed_model_endpoint_label(provider, model, endpoint_variant.as_deref())
                    );
                    live.apply_output_frame(frame.as_bytes())?;
                }
                match handle_live_post_turn_overflow(
                    live,
                    &agent,
                    &mut renderer,
                    context_tokens,
                    config.display.show_token_usage,
                    Some(&mut cumulative_tokens),
                )
                .await
                {
                    Ok(Some(compact_result)) => {
                        if let Some(usage) = compact_result.usage.as_ref() {
                            turn_tokens.add(TurnTokens::from_usage(Some(usage)));
                        }
                        footer.set_token_usage_with_cache(
                            turn_tokens,
                            agent.effective_context_tokens()?,
                            agent.context_window(),
                            cumulative_tokens,
                        );
                    }
                    Ok(None) => {
                        footer.update_session_tokens(agent.effective_context_tokens()?);
                    }
                    Err(err) => {
                        let frame = format!("\x1b[31m{}: {err}\x1b[0m\n", t("error", "错误"));
                        live.apply_output_frame(frame.as_bytes())?;
                        continue;
                    }
                }
                live.refresh_footer(footer.clone())?;
                show_shortcut_hint = false;
            }
            Ok(None) => {
                // An explicit cancel also withdraws the queued follow-ups;
                // reloading afterwards clears their bubbles.
                let _ = state.delete_queued_prompts();
                if let Some(live) = live_repl.as_mut() {
                    synchronized_terminal_update(CursorAfterUpdate::Shown, || {
                        live.reload_queue(&state)
                    })?;
                }
                // An interrupted turn is persisted and will be replayed into
                // the next request, so the context meter must reflect it.
                cumulative_tokens = state
                    .session_cumulative_token_totals()
                    .unwrap_or(cumulative_tokens);
                footer.update_session_tokens(agent.effective_context_tokens()?);
                footer.update_cumulative_tokens(cumulative_tokens);
            }
            Err(err) if crate::question::is_question_cancelled(&err) => {
                let _ = state.delete_queued_prompts();
                if let Some(live) = live_repl.as_mut() {
                    synchronized_terminal_update(CursorAfterUpdate::Shown, || {
                        live.reload_queue(&state)
                    })?;
                }
                cumulative_tokens = state
                    .session_cumulative_token_totals()
                    .unwrap_or(cumulative_tokens);
                footer.update_session_tokens(agent.effective_context_tokens()?);
                footer.update_cumulative_tokens(cumulative_tokens);
                continue;
            }
            Err(err) => {
                if let Some(live) = live_repl.as_mut() {
                    let frame = format!("\x1b[31m{}: {err}\x1b[0m\n", t("error", "错误"));
                    live.apply_output_frame(frame.as_bytes())?;
                    synchronized_terminal_update(CursorAfterUpdate::Shown, || {
                        live.reload_queue(&state)
                    })?;
                }
                continue;
            }
        }
    }
    state.discard_queued_prompts()?;
    // Background jobs are children of this REPL process; never leave them
    // running once the host is gone.
    tools::jobs::shutdown_all();
    Ok(())
}

pub(crate) fn reload_repl_config(
    paths: &GQYPaths,
    state: &StateStore,
    config: &mut AppConfig,
    client: &mut OpenAiCompatibleClient,
) -> Result<()> {
    *config = AppConfig::load(paths)?;
    apply_session_model_override(state, config);
    *client = OpenAiCompatibleClient::from_config(config, paths)?;
    Ok(())
}

/// The footer/status display must reflect the session's pinned model pool,
/// not just the global config.
pub(crate) fn footer_config_for_session(
    paths: &GQYPaths,
    config: &AppConfig,
    session_id: &str,
) -> AppConfig {
    let mut config = config.clone();
    if let Ok(Some(models)) =
        StateStore::new(paths).and_then(|store| store.session_model_override(session_id))
    {
        config.active_provider_models = Some(models);
    }
    config
}

/// Direct (daemon-less) sessions read their pinned model pool straight from
/// the state store; daemon-run turns get the same treatment in the turn task.
pub(crate) fn apply_session_model_override(state: &StateStore, config: &mut AppConfig) {
    match state.session_model_override(&state.session_id()) {
        Ok(Some(models)) => config.active_provider_models = Some(models),
        Ok(None) => {}
        Err(error) => tracing::warn!(
            error = %error,
            "{}",
            t(
                "loading the session model override failed",
                "读取会话模型覆盖失败"
            )
        ),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VariantMenuItem {
    pub(crate) provider_id: String,
    pub(crate) model: String,
    pub(crate) options: Vec<VariantMenuOption>,
    pub(crate) selected: usize,
    pub(crate) cursor: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VariantMenuOption {
    pub(crate) label: String,
    pub(crate) value: Option<String>,
}

impl VariantMenuItem {
    pub(crate) fn from_options(options: &ThinkingVariantOptions) -> Self {
        let mut variants = vec![VariantMenuOption {
            label: "default".to_string(),
            value: None,
        }];
        variants.extend(options.variants.iter().map(|variant| VariantMenuOption {
            label: if variant == "default" {
                "default (variant)".to_string()
            } else {
                variant.clone()
            },
            value: Some(variant.clone()),
        }));
        let selected = options
            .selected
            .as_ref()
            .and_then(|selected| {
                variants
                    .iter()
                    .position(|variant| variant.value.as_ref() == Some(selected))
            })
            .unwrap_or(0);
        Self {
            provider_id: options.provider_id.clone(),
            model: options.model.clone(),
            options: variants,
            selected,
            cursor: selected,
        }
    }

    pub(crate) fn selection(&self) -> (String, String, Option<String>) {
        (
            self.provider_id.clone(),
            self.model.clone(),
            self.options[self.selected].value.clone(),
        )
    }

    pub(crate) fn check_cursor(&mut self) {
        self.selected = self.cursor;
    }
}

pub(crate) fn inline_variant_select(
    options: &[ThinkingVariantOptions],
) -> Result<Option<Vec<(String, String, Option<String>)>>> {
    let mut items = options
        .iter()
        .map(VariantMenuItem::from_options)
        .collect::<Vec<_>>();
    if items.is_empty() {
        return Ok(None);
    }
    if items.len() == 1 {
        return inline_single_variant_select(items.remove(0));
    }
    let max_options = items
        .iter()
        .map(|item| item.options.len())
        .max()
        .unwrap_or(1);
    let menu_lines = inline_fuzzy_lines(items.len().max(max_options));
    reserve_inline_fuzzy_space(menu_lines)?;
    let mut session = InlineRawMode::start()?;
    let mut active_column = 0usize;
    let mut model_index = 0usize;
    let mut model_scroll = 0usize;
    let mut variant_scroll = 0usize;
    let (_, cursor_y) = cursor::position().unwrap_or((0, menu_lines.saturating_sub(1)));
    let anchor_y = cursor_y.saturating_sub(menu_lines.saturating_sub(1));
    loop {
        let visible = menu_lines.saturating_sub(2) as usize;
        model_scroll = inline_fuzzy_scroll(model_index, model_scroll, visible.min(items.len()));
        let item = &items[model_index];
        variant_scroll =
            inline_fuzzy_scroll(item.cursor, variant_scroll, visible.min(item.options.len()));
        draw_inline_variant(
            &mut session.stdout,
            anchor_y,
            menu_lines,
            &items,
            active_column,
            model_index,
            model_scroll,
            variant_scroll,
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
                KeyCode::Esc | KeyCode::Char('q') => {
                    clear_inline_fuzzy(&mut session.stdout, anchor_y, menu_lines)?;
                    return Ok(None);
                }
                KeyCode::Enter => {
                    clear_inline_fuzzy(&mut session.stdout, anchor_y, menu_lines)?;
                    return Ok(Some(items.iter().map(VariantMenuItem::selection).collect()));
                }
                KeyCode::Left | KeyCode::Char('h') => active_column = 0,
                KeyCode::Right | KeyCode::Char('l') => active_column = 1,
                KeyCode::Up | KeyCode::Char('k') if active_column == 0 => {
                    model_index = model_index.saturating_sub(1);
                    variant_scroll = 0;
                }
                KeyCode::Down | KeyCode::Char('j') if active_column == 0 => {
                    model_index = (model_index + 1).min(items.len() - 1);
                    variant_scroll = 0;
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    items[model_index].cursor = items[model_index].cursor.saturating_sub(1);
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    let last = items[model_index].options.len() - 1;
                    items[model_index].cursor = (items[model_index].cursor + 1).min(last);
                }
                KeyCode::Tab if active_column == 1 => {
                    items[model_index].check_cursor();
                }
                _ => {}
            }
        }
    }
}

pub(crate) fn inline_single_variant_select(
    mut item: VariantMenuItem,
) -> Result<Option<Vec<(String, String, Option<String>)>>> {
    let menu_lines = inline_fuzzy_lines(item.options.len());
    reserve_inline_fuzzy_space(menu_lines)?;
    let mut session = InlineRawMode::start()?;
    let mut scroll = 0usize;
    let (_, cursor_y) = cursor::position().unwrap_or((0, menu_lines.saturating_sub(1)));
    let anchor_y = cursor_y.saturating_sub(menu_lines.saturating_sub(1));
    loop {
        let visible = menu_lines.saturating_sub(2) as usize;
        scroll = inline_fuzzy_scroll(item.cursor, scroll, visible.min(item.options.len()));
        draw_inline_single_variant(&mut session.stdout, anchor_y, menu_lines, &item, scroll)?;
        if let Event::Key(KeyEvent {
            code, modifiers, ..
        }) = event::read()?
        {
            match code {
                KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => {
                    clear_inline_fuzzy(&mut session.stdout, anchor_y, menu_lines)?;
                    return Ok(None);
                }
                KeyCode::Esc | KeyCode::Char('q') => {
                    clear_inline_fuzzy(&mut session.stdout, anchor_y, menu_lines)?;
                    return Ok(None);
                }
                KeyCode::Enter => {
                    clear_inline_fuzzy(&mut session.stdout, anchor_y, menu_lines)?;
                    return Ok(Some(vec![item.selection()]));
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    item.cursor = item.cursor.saturating_sub(1);
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    item.cursor = (item.cursor + 1).min(item.options.len() - 1);
                }
                KeyCode::Tab => item.check_cursor(),
                _ => {}
            }
        }
    }
}
