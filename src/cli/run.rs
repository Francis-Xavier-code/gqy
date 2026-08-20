//! run — CLI 命令执行（自 src/cli/defs.rs 拆分）。

pub(crate) use super::*;

pub(crate) fn run_history(paths: &GQYPaths, args: HistoryArgs) -> Result<()> {
    let state = StateStore::new(paths)?;
    run_history_with_state(&state, args)
}

pub(crate) fn run_history_with_state(state: &StateStore, args: HistoryArgs) -> Result<()> {
    for entry in state.history(args.limit)? {
        if args.raw {
            println!("{}", serde_json::to_string(&entry)?);
            continue;
        }
        let display_role = if entry.role.ends_with("_clarification") {
            entry.role.trim_end_matches("_clarification")
        } else {
            entry.role.as_str()
        };
        println!("{} {display_role}", entry.timestamp);
        if entry.role.starts_with("assistant") {
            let response = crate::llm::ChatResult {
                content: entry.content,
                reasoning: if args.no_thinking {
                    None
                } else {
                    entry.reasoning
                },
                usage: None,
                usage_estimated: false,
                tool_calls: Vec::new(),
                provider_id: None,
                model: None,
                finish_reason: None,
                thinking_signature: None,
                last_request_usage: None,
                responses_continuation: None,
            };
            render::print_assistant_response(&response, !args.no_thinking)?;
        } else {
            println!("{}", entry.content);
        }
        println!();
    }
    Ok(())
}

pub(crate) async fn run_kb(paths: &GQYPaths, args: KbArgs) -> Result<()> {
    let config = AppConfig::load(paths)?;
    let kb = tools::knowledge_base::KnowledgeBase::new(config, paths.clone())?;
    match args.command {
        KbCommand::Add(args) => {
            let added = kb.add_path(&args.path).await?;
            for path in added {
                println!("{} {path}", t("added", "已添加"));
            }
        }
        KbCommand::List => {
            for file in kb.list()? {
                println!("{}\t{} {}", file.name, file.size_bytes, t("bytes", "字节"));
            }
        }
        KbCommand::Search(args) => {
            let query = args.query.join(" ");
            println!("{}", kb.search(&query, args.limit).await?);
        }
        KbCommand::Find(args) => {
            let query = args.query.join(" ");
            println!("{}", kb.find_by_name(&query, args.limit)?);
        }
        KbCommand::Read(args) => {
            println!("{}", kb.read_file(&args.file, args.start, args.lines)?);
        }
        KbCommand::Remove(args) => {
            kb.remove(&args.file)?;
            println!("{} {}", t("removed", "已移除"), args.file);
        }
        KbCommand::Reindex => {
            let files = kb.list()?;
            println!(
                "{}: {}",
                t(
                    "keyword index is rebuilt on demand; files tracked",
                    "关键词索引会按需重建；已跟踪文件数",
                ),
                files.len()
            );
        }
        KbCommand::Stats => {
            let mut stats = kb.stats()?;
            if let Some(object) = stats.as_object_mut() {
                if let Ok(status) = crate::default_kb::status(paths) {
                    object.insert(
                        "default_kb_update_available".to_string(),
                        serde_json::json!(status.has_update_notice),
                    );
                }
            }
            println!("{}", stats);
        }
        KbCommand::Embed(args) => match args.command {
            KbEmbedCommand::Reindex(args) => {
                kb.reindex_embeddings(args.quiet).await?;
            }
        },
    }
    Ok(())
}

pub(crate) async fn run_update_default_kb(paths: &GQYPaths) -> Result<()> {
    let config = AppConfig::load_or_default(paths)?;
    let state = crate::default_kb::update(paths, &config, |stage| {
        let mut stderr = io::stderr().lock();
        let _ = write_default_kb_update_progress(&mut stderr, stage);
    })?;
    println!(
        "{}: {}",
        t("updated default knowledge base", "已更新默认知识库"),
        state.shorin_wiki_commit
    );
    Ok(())
}

pub(crate) fn write_default_kb_update_progress(
    output: &mut impl Write,
    stage: crate::default_kb::UpdateStage,
) -> io::Result<()> {
    writeln!(output, "[default-kb] {}", stage.message())?;
    output.flush()
}

pub(crate) fn run_memory(paths: &GQYPaths, args: MemoryArgs) -> Result<()> {
    let config = AppConfig::load_or_default(paths)?;
    let store = MemoryStore::new(&config, paths);
    match args.command {
        MemoryCommand::Stats => println!("{}", store.stats()?),
        MemoryCommand::Reset(args) => {
            store.reset_all(args.include_skills)?;
            println!("{}", t("cleared assistant memory", "已清空助手记忆"));
        }
        MemoryCommand::Search(args) => {
            let query = join_message(args.query);
            let limit = args.limit.unwrap_or(10);
            println!("{}", store.recall_memories(&query, limit, args.forgotten)?);
        }
        MemoryCommand::Remember(args) => {
            let content = join_message(args.content);
            let id = store.remember_fact(&content, &args.source)?;
            println!("{}: {id}", t("remembered fact", "已记住事实"));
        }
    }
    Ok(())
}

pub(crate) fn run_skills(paths: &GQYPaths, args: SkillsArgs) -> Result<()> {
    std::fs::create_dir_all(&paths.skills_dir)?;
    match args.command {
        SkillsCommand::List => {
            for name in skill_names(paths)? {
                let disabled = paths.skills_dir.join(&name).join(".disabled").exists();
                println!(
                    "{}{}",
                    name,
                    if disabled {
                        t(" [disabled]", " [已禁用]")
                    } else {
                        ""
                    }
                );
            }
        }
        SkillsCommand::Show(args) => {
            let path = skill_dir(paths, &args.name)?.join("SKILL.md");
            println!("{}", std::fs::read_to_string(path)?);
        }
        SkillsCommand::Enable(args) => {
            let marker = skill_dir(paths, &args.name)?.join(".disabled");
            if marker.exists() {
                std::fs::remove_file(marker)?;
            }
            println!("{}: {}", t("enabled skill", "已启用 skill"), args.name);
        }
        SkillsCommand::Disable(args) => {
            let marker = skill_dir(paths, &args.name)?.join(".disabled");
            std::fs::write(marker, "disabled\n")?;
            println!("{}: {}", t("disabled skill", "已禁用 skill"), args.name);
        }
        SkillsCommand::Remove(args) => {
            let dir = skill_dir(paths, &args.name)?;
            std::fs::remove_dir_all(dir)?;
            println!("{}: {}", t("removed skill", "已移除 skill"), args.name);
        }
        SkillsCommand::Stats => {
            let names = skill_names(paths)?;
            let disabled = names
                .iter()
                .filter(|name| paths.skills_dir.join(name).join(".disabled").exists())
                .count();
            println!(
                "{}",
                serde_json::json!({
                    "ok": true,
                    "skills_dir": paths.skills_dir.display().to_string(),
                    "skills": names.len(),
                    "disabled": disabled,
                    "enabled": names.len().saturating_sub(disabled),
                })
            );
        }
        SkillsCommand::Prune => {
            let mut removed = 0usize;
            for name in skill_names(paths)? {
                let dir = paths.skills_dir.join(&name);
                let raw = std::fs::read_to_string(dir.join("SKILL.md")).unwrap_or_default();
                if crate::skills::is_generated_skill(&raw) && dir.join(".disabled").exists() {
                    std::fs::remove_dir_all(dir)?;
                    removed += 1;
                }
            }
            println!("{}: {removed}", t("pruned skills", "已清理 skills"));
        }
        SkillsCommand::Import(args) => run_skill_import(paths, &args)?,
    }
    Ok(())
}

/// `gqy skills import <path>`：把本地 skill 目录导入为草稿；草稿会在
/// `gqy` 会话中被 AI 审查并最终发布。`--force` 直接安装（危险，不推荐）。
pub(crate) fn run_skill_import(paths: &GQYPaths, args: &SkillImportArgs) -> Result<()> {
    use crate::skills::SkillScope;
    let source = args
        .path
        .canonicalize()
        .with_context(|| format!("cannot resolve skill path: {}", args.path.display()))?;
    if !source.join("SKILL.md").is_file() {
        bail!(
            "{}: {}",
            t("not a skill directory", "不是有效的 skill 目录"),
            source.display()
        );
    }
    let raw = std::fs::read_to_string(source.join("SKILL.md"))?;
    let metadata = crate::skills::parse_skill_metadata(&raw, None)?;
    let name = metadata.name.clone();
    let config = AppConfig::load_or_default(paths)?;

    if args.force {
        let target = paths.skills_dir.join(&name);
        if target.exists() {
            bail!("skill already exists: {name}");
        }
        std::fs::create_dir_all(&paths.skills_dir)?;
        crate::skills::copy_tree(&source, &target)?;
        println!("{}: {name}", t("force-imported skill", "已强制导入 skill"));
        return Ok(());
    }

    // 先建草稿，随后进入 gqy 会话由 AI 审查并发布。
    let draft = crate::skills::create_draft(
        &config,
        paths,
        &name,
        &metadata.description,
        SkillScope::Global,
    )?;
    let skill_dir = PathBuf::from(&draft.skill_dir);
    // 用源目录内容替换草稿的模板文件（SKILL.md 与资源）。
    std::fs::remove_file(skill_dir.join("SKILL.md"))?;
    for entry in std::fs::read_dir(&skill_dir)? {
        let entry = entry?;
        if entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        if entry.path().is_file() {
            std::fs::remove_file(entry.path())?;
        }
    }
    crate::skills::copy_tree_into(&source, &skill_dir)?;
    println!(
        "{}: {name}\n{}",
        t("imported skill draft", "已导入 skill 草稿"),
        t(
            "run `gqy` and ask the assistant to review and publish this skill draft",
            "运行 gqy 让助手审查并发布该 skill 草稿"
        )
    );
    Ok(())
}

/// `gqy resources`：查看/清理脚本与 Skill 的审查、安装记录。
pub(crate) fn run_resources(paths: &GQYPaths, args: ResourcesArgs) -> Result<()> {
    match args.command {
        ResourcesCommand::Status => {
            println!("{}", crate::skills::status_summary(paths)?);
        }
        ResourcesCommand::Prune => {
            let removed = crate::skills::prune_review_state(paths)?;
            println!(
                "{}: {removed}",
                t("pruned review records", "已清理审查记录")
            );
        }
    }
    Ok(())
}

pub(crate) fn skill_names(paths: &GQYPaths) -> Result<Vec<String>> {
    let mut names = Vec::new();
    if !paths.skills_dir.exists() {
        return Ok(names);
    }
    for entry in std::fs::read_dir(&paths.skills_dir)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() && entry.path().join("SKILL.md").is_file() {
            names.push(entry.file_name().to_string_lossy().to_string());
        }
    }
    names.sort();
    Ok(names)
}

pub(crate) fn skill_dir(paths: &GQYPaths, name: &str) -> Result<PathBuf> {
    let clean = name.trim();
    if clean.is_empty()
        || clean.contains('/')
        || clean.contains('\\')
        || clean == "."
        || clean == ".."
    {
        bail!("{}: {name}", t("invalid skill name", "无效 skill 名称"));
    }
    let dir = paths.skills_dir.join(clean);
    if !dir.join("SKILL.md").is_file() {
        bail!("{}: {name}", t("skill not found", "未找到 skill"));
    }
    Ok(dir)
}

pub(crate) async fn run_reset(paths: &GQYPaths) -> Result<()> {
    let config = AppConfig::load_or_default(paths)?;
    let state = StateStore::new(paths)?;
    let memory = MemoryStore::new(&config, paths);
    state.reset_conversation()?;
    memory.clear_evicted_context()?;
    memory.clear_pending_events()?;
    tools::clear_brew_review_state(paths)?;
    Ok(())
}

pub(crate) async fn run_reset_memory_command(paths: &GQYPaths) -> Result<()> {
    if !io::stdin().is_terminal() {
        bail!(
            "{}",
            t(
                "reset-memory needs a terminal to confirm",
                "reset-memory 需要在终端确认"
            )
        );
    }
    if !confirm_stdin(t(
        "erase this persona's long-term memory (facts, diary, episodes)?",
        "确认清空长期记忆（事实/日记/经历）？",
    ))? {
        println!("{}", t("cancelled", "已取消"));
        return Ok(());
    }
    if ipc::daemon_info(paths).await.is_some() {
        send_ipc_admin(paths, IpcCommand::ResetMemory { mode: None }).await?;
    } else {
        let config = AppConfig::load_or_default(paths)?;
        MemoryStore::new(&config, paths).reset_all(false)?;
    }
    println!("{}", t("long-term memory erased", "长期记忆已清空"));
    Ok(())
}

pub(crate) fn wipe_summary() -> &'static str {
    t(
        "This erases everything GQY has accumulated: memory, every conversation's contents, group-chat contexts, and auto-generated skills. It cannot be undone.",
        "这会抹掉 GQY 积累的一切：记忆、所有会话的内容、群聊上下文、自动生成的技能。不可撤销。",
    )
}

pub(crate) async fn run_wipe(paths: &GQYPaths, assume_yes: bool) -> Result<()> {
    if !assume_yes {
        if !io::stdin().is_terminal() {
            bail!(
                "{}",
                t(
                    "wipe needs a terminal to confirm; pass --yes to run it unattended",
                    "wipe 需要在终端确认；非交互场景请加 --yes"
                )
            );
        }
        println!("{}", wipe_summary());
        if !confirm_stdin(t("wipe everything?", "确认全部抹掉？"))? {
            println!("{}", t("cancelled", "已取消"));
            return Ok(());
        }
    }
    if ipc::daemon_info(paths).await.is_some() {
        send_ipc_admin(paths, IpcCommand::WipePersona).await?;
    } else {
        let config = AppConfig::load_or_default(paths)?;
        let state = StateStore::new(paths)?;
        let persona = config.active_persona_scope();
        let bindings = state.platform_session_bindings(&persona, "onebot")?;
        let plugins = crate::platforms::plugins::PlatformPluginRegistry::built_in()?;
        plugins
            .after_persona_reset(&crate::platforms::plugins::PlatformPersonaResetContext {
                config: &config,
                paths,
                bindings: &bindings,
            })
            .await?;
        state.reset_persona_contexts(&persona, "onebot")?;
        state.reset_conversation_usage()?;
        MemoryStore::new(&config, paths).reset_all(true)?;
        tools::clear_brew_review_state(paths)?;
    }
    println!("{}", print_wipe_message());
    Ok(())
}

pub(crate) fn print_wipe_message() -> &'static str {
    t(
        "erased all conversations, QQ contexts, memory, and generated skills for the current persona",
        "已抹掉当前人格的全部会话内容、QQ 上下文、记忆和自动技能",
    )
}

pub(crate) fn print_reset_message() {
    let message = t("cleared current conversation history", "已清空当前会话历史");
    println!("\x1b[2m{message}\x1b[0m\n");
}

pub(crate) fn join_message(parts: Vec<String>) -> String {
    parts.join(" ").trim().to_string()
}
pub(crate) fn handle_agent_event(
    renderer: &mut render::StreamRenderer,
    event: AgentEvent,
) -> Result<()> {
    match event {
        AgentEvent::TurnStarted { .. } => Ok(()),
        AgentEvent::RawReasoning(_) => Ok(()),
        AgentEvent::FlushJournal => Ok(()),
        // 单次输出模式没有常驻 footer,逐请求计量快照无处可画。
        AgentEvent::RoundUsage { .. } => Ok(()),
        AgentEvent::Chunk(chunk) => {
            renderer.write_chunk(chunk)?;
            renderer.tick_spinner()
        }
        AgentEvent::ReasoningStart { received_at } => renderer.start_reasoning_phase(received_at),
        AgentEvent::ReasoningReset { received_at } => renderer.reset_reasoning_phase(received_at),
        AgentEvent::ReasoningPartStart { received_at } => {
            renderer.start_reasoning_part(received_at)
        }
        AgentEvent::ReasoningPartEnd { received_at } => renderer.finish_reasoning_part(received_at),
        AgentEvent::ReasoningTitle(title) => {
            renderer.write_reasoning_title(&title)?;
            renderer.tick_spinner()
        }
        AgentEvent::ToolCall {
            name, arguments, ..
        } => {
            renderer.write_tool_call(&name, &arguments)?;
            renderer.tick_spinner()
        }
        AgentEvent::ToolPreparing { name } => {
            renderer.write_tool_preparing(&name)?;
            renderer.tick_spinner()
        }
        AgentEvent::ToolResult {
            name, ok, output, ..
        } => {
            renderer.write_tool_result(&name, ok, &output)?;
            renderer.tick_spinner()
        }
        AgentEvent::ToolProgress { name, message, .. } => {
            renderer.write_tool_progress(&name, &message)?;
            renderer.tick_spinner()
        }
        AgentEvent::CommandOutput {
            name,
            stream,
            chunk,
            ..
        } => {
            renderer.write_command_output(&name, stream, &chunk)?;
            renderer.tick_spinner()
        }
        AgentEvent::PrepareForExternalOutput { ready } => {
            renderer.prepare_for_external_output()?;
            let _ = ready.send(true);
            Ok(())
        }
        AgentEvent::Image { .. } | AgentEvent::Artifact { .. } => Ok(()),
        AgentEvent::AskQuestion {
            request, responder, ..
        } => {
            renderer.prepare_for_external_output()?;
            let response = crate::question_tui::ask(&request).unwrap_or_else(|err| {
                crate::question::QuestionResponse::Unavailable(err.to_string())
            });
            if !matches!(&response, crate::question::QuestionResponse::Cancelled) {
                renderer.start_waiting()?;
            }
            let _ = responder.send(response);
            Ok(())
        }
        AgentEvent::QueuedPromptsConsumed { .. } => Ok(()),
        AgentEvent::GenerationSuperseded { .. } => Ok(()),
        AgentEvent::SpinnerTick => renderer.tick_spinner(),
        AgentEvent::CompactStart => {
            renderer.write_system_message(t("Compacting context...", "正在压缩上下文..."))?;
            renderer.tick_spinner()
        }
        AgentEvent::CompactChunk(chunk) => {
            renderer.write_compact_chunk(&chunk)?;
            renderer.tick_spinner()
        }
        AgentEvent::CompactEnd => {
            renderer.finish_compact()?;
            renderer.tick_spinner()
        }
        AgentEvent::PopStart => renderer.tick_spinner(),
        AgentEvent::PopEnd => renderer.tick_spinner(),
        AgentEvent::Notice { text } => {
            renderer.write_system_message(&text)?;
            renderer.tick_spinner()
        }
    }
}

pub(crate) fn build_tool_registry(
    config: &AppConfig,
    paths: &GQYPaths,
    mode: AgentMode,
    interactive_questions: bool,
) -> Result<tools::ToolRegistry> {
    let mut registry = if config.tools.enabled {
        match mode {
            AgentMode::Normal => tools::builtin_registry(config, paths),
            AgentMode::Dev => tools::dev_registry(config, paths),
        }
    } else {
        tools::ToolRegistry::new()
    };
    if config.tools.enabled && config.skills.enabled {
        tools::register_skills(&mut registry, config, paths)?;
        if mode == AgentMode::Normal {
            tools::register_skill_authoring(&mut registry, config.clone(), paths.clone());
        }
    }
    if config.tools.enabled && interactive_questions {
        tools::register_ask_question(&mut registry);
    }
    tools::register_script_display_names(&registry);
    Ok(registry)
}
