//! state — 自 src/web.rs 拆分。

#![allow(clippy::doc_lazy_continuation)]
pub(crate) use super::*;

pub(crate) fn rebuild_for_config(
    agent: &mut Option<Agent>,
    config: &mut AppConfig,
    paths: &GQYPaths,
    state_store: &StateStore,
    manager: &Arc<Mutex<ManagerState>>,
    events: &EventHub,
    next_config: AppConfig,
    prompts: &PromptDocuments,
    reset_conversation: bool,
) -> std::result::Result<(), AdminFailure> {
    let _ = reset_conversation;
    let mut next_config = next_config;
    // Models removed from the text models must leave the tier pools too.
    next_config.prune_subagent_tiers();
    let previous_prompts = read_prompt_documents(config, paths)
        .map_err(|error| AdminFailure::Internal(safe_error_message(error)))?;
    let persona_changes = persona_document_changes(&previous_prompts, prompts);
    let mut persona_db_guard = PersonaDbRenameGuard::new(state_store.clone(), &persona_changes)
        .map_err(|error| AdminFailure::Internal(safe_error_message(error)))?;
    let previous_scope = config.active_persona_scope();
    let next_scope = next_config.active_persona_scope();
    let migrated_previous_scope = persona_changes
        .iter()
        .find_map(|(old_name, new_name)| {
            (crate::config::persona_scope_name(old_name) == previous_scope)
                .then(|| new_name.as_deref().map(crate::config::persona_scope_name))
                .flatten()
        })
        .unwrap_or_else(|| previous_scope.clone());
    let persona_changed = migrated_previous_scope != next_scope;
    let previous_session_id = state_store.session_id().to_string();
    let target_session_id = if persona_changed {
        session_for_persona(state_store, manager, &next_scope)
            .map_err(|error| AdminFailure::Internal(safe_error_message(error)))?
    } else {
        previous_session_id.clone()
    };
    if persona_changed {
        state_store
            .set_persona_current_session(&migrated_previous_scope, &previous_session_id)
            .map_err(|error| AdminFailure::Internal(safe_error_message(error)))?;
    }
    let target_state_store = if persona_changed {
        state_store.pinned(&target_session_id)
    } else {
        state_store.clone()
    };
    let prompt_backups =
        apply_prompt_documents(config, &next_config, &previous_prompts, prompts, paths)
            .map_err(|error| AdminFailure::Internal(safe_error_message(error)))?;
    let scope_backups = match apply_persona_scope_changes(
        config,
        &next_config,
        &previous_prompts,
        prompts,
        paths,
    ) {
        Ok(backups) => backups,
        Err(error) => {
            restore_file_backups(&prompt_backups);
            return Err(AdminFailure::Internal(safe_error_message(error)));
        }
    };
    let config_backup = FileBackup {
        path: paths.config_file.clone(),
        content: std::fs::read(&paths.config_file).ok(),
    };
    let system_prompt_backup = next_config.system_prompt.as_ref().map(|_| FileBackup {
        path: next_config.system_prompt_path(paths),
        content: std::fs::read(next_config.system_prompt_path(paths)).ok(),
    });

    let build_agent = || -> Result<Agent> {
        crate::models_cache::ensure_active_metadata(paths, &next_config);
        let client = OpenAiCompatibleClient::from_config(&next_config, paths)?;
        let registry = build_tool_registry(&next_config, paths, AgentMode::Normal, true)?;
        Agent::new(
            next_config.clone(),
            paths,
            target_state_store.clone(),
            client,
            registry,
            AgentMode::Normal,
        )
    };
    let next_agent = if agent.is_some() {
        match build_agent() {
            Ok(agent) => Some(agent),
            Err(error) => {
                restore_file_backups(&prompt_backups);
                restore_persona_scope_backups(&scope_backups);
                return Err(AdminFailure::Invalid(safe_error_message(error)));
            }
        }
    } else {
        None
    };
    let context = match next_agent.as_ref().map_or_else(
        || cold_context(&next_config, &target_state_store),
        current_context,
    ) {
        Ok(context) => context,
        Err(error) => {
            restore_file_backups(&prompt_backups);
            restore_persona_scope_backups(&scope_backups);
            return Err(AdminFailure::Invalid(safe_error_message(error)));
        }
    };
    if let Err(error) = next_config.save(paths) {
        restore_file_backups(&prompt_backups);
        restore_persona_scope_backups(&scope_backups);
        restore_file_backups(std::slice::from_ref(&config_backup));
        if let Some(backup) = &system_prompt_backup {
            restore_file_backups(std::slice::from_ref(backup));
        }
        return Err(AdminFailure::Internal(safe_error_message(error)));
    }

    if persona_changed {
        if let Err(error) = state_store.switch_session(&target_session_id) {
            restore_file_backups(&prompt_backups);
            restore_persona_scope_backups(&scope_backups);
            restore_file_backups(std::slice::from_ref(&config_backup));
            if let Some(backup) = &system_prompt_backup {
                restore_file_backups(std::slice::from_ref(backup));
            }
            return Err(AdminFailure::Internal(safe_error_message(error)));
        }
        if let Err(error) = state_store.set_persona_current_session(&next_scope, &target_session_id)
        {
            let _ = state_store.switch_session(&previous_session_id);
            restore_file_backups(&prompt_backups);
            restore_persona_scope_backups(&scope_backups);
            restore_file_backups(std::slice::from_ref(&config_backup));
            if let Some(backup) = &system_prompt_backup {
                restore_file_backups(std::slice::from_ref(backup));
            }
            return Err(AdminFailure::Internal(safe_error_message(error)));
        }
    }

    *agent = next_agent;
    *config = next_config.clone();
    let mut manager = manager.lock().unwrap();
    let migrated_session_ids = persona_changes
        .iter()
        .filter_map(|(old_name, new_name)| {
            let old_scope = crate::config::persona_scope_name(old_name);
            let new_scope = new_name.as_deref().map(crate::config::persona_scope_name)?;
            manager
                .persona_session_ids
                .remove(&old_scope)
                .map(|session_id| (new_scope, session_id))
        })
        .collect::<Vec<_>>();
    manager.persona_session_ids.extend(migrated_session_ids);
    if persona_changed {
        manager
            .persona_session_ids
            .insert(migrated_previous_scope, previous_session_id);
        manager
            .persona_session_ids
            .insert(next_scope, target_session_id.clone());
    }
    manager.config = next_config;
    manager.context = context;
    drop(manager);
    if persona_changed {
        events.publish(
            "session.current_changed",
            json!({ "session_id": target_session_id }),
        );
    }
    persona_db_guard.commit();
    finalize_persona_scope_backups(&scope_backups);
    for (old_name, new_name) in &persona_changes {
        if new_name.is_none() {
            if let Err(error) =
                state_store.delete_persona_scope(&crate::config::persona_scope_name(old_name))
            {
                tracing::warn!(
                    %error,
                    %old_name,
                    "{}",
                    t(
                        "deleted persona state cleanup failed",
                        "已删除角色的状态清理失败"
                    )
                );
            }
        }
    }
    Ok(())
}

/// Auto-names a still-unnamed session from its first prompt once a turn has
/// run in it. Explicit names (given at creation or via rename) are never
/// overwritten.
pub(crate) fn maybe_auto_name_session(
    state_store: &StateStore,
    events: &EventHub,
    seed: &str,
) -> Option<String> {
    let session_id = state_store.session_id();
    let record = state_store.session_record(&session_id).ok().flatten()?;
    if !record.name.trim().is_empty() {
        return None;
    }
    let title = session_title_from_prompt(seed);
    if title.is_empty() {
        return None;
    }
    if state_store
        .rename_session(&record.session_id, &title)
        .is_ok()
    {
        events.publish(
            "session.renamed",
            json!({ "session_id": record.session_id, "name": title }),
        );
        return Some(title);
    }
    None
}

pub(crate) fn session_title_from_prompt(prompt: &str) -> String {
    let cleaned = prompt
        .trim()
        .lines()
        .next()
        .unwrap_or("")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let mut title: String = cleaned.chars().take(20).collect();
    if cleaned.chars().count() > 20 {
        title.push('…');
    }
    title
}

pub(crate) fn switch_actor_session(
    agent: Option<&Agent>,
    config: &AppConfig,
    state_store: &StateStore,
    manager: &Arc<Mutex<ManagerState>>,
    events: &EventHub,
    session_id: &str,
) -> std::result::Result<(), AdminFailure> {
    // Notes: switching deliberately does not touch updated_at (viewing must
    // not reorder the session list), and runs no turn-entry maintenance —
    // switching is allowed while turns are running, so a prompt-change reset
    // here could wipe a session mid-turn.
    let switch = || -> Result<ContextSnapshot> {
        state_store.switch_session(session_id)?;
        agent.map_or_else(|| cold_context(config, state_store), current_context)
    };
    let context = switch().map_err(|error| AdminFailure::Internal(safe_error_message(&error)))?;
    let mut manager_state = manager.lock().unwrap();
    manager_state.context = context;
    let persona_scope = manager_state.config.active_persona_scope();
    manager_state
        .persona_session_ids
        .insert(persona_scope.clone(), session_id.to_string());
    drop(manager_state);
    state_store
        .set_persona_current_session(&persona_scope, session_id)
        .map_err(|error| AdminFailure::Internal(safe_error_message(error)))?;
    events.publish(
        "session.current_changed",
        json!({ "session_id": session_id }),
    );
    Ok(())
}

pub(crate) fn reset_actor_conversation(
    agent: &mut Option<Agent>,
    config: &AppConfig,
    paths: &GQYPaths,
    state_store: &StateStore,
    manager: &Arc<Mutex<ManagerState>>,
    events: &EventHub,
    session_id: &str,
) -> std::result::Result<(), AdminFailure> {
    // "Reset" means the conversation starts over, so everything scoped to it
    // goes: history, per-session usage, and the recall caches that only make
    // sense against that history. This used to be gated on a flag that was
    // really asking "did the caller address the session as `Current`?" — an
    // implementation detail of each frontend, which left `/reset` and the
    // WebUI clearing strictly less than `gqy reset`. Platform sessions never
    // reach this command (both entry points reject them) and clear themselves
    // through `ClearSessionContent`, so there is nothing left for a flag to
    // protect.
    let mut reset = || -> Result<Option<ContextSnapshot>> {
        let store = state_store.pinned(session_id);
        store.clear_session_content()?;
        store.reset_conversation_usage()?;
        let memory = MemoryStore::new(config, paths);
        memory.clear_evicted_context()?;
        memory.clear_pending_events()?;
        tools::clear_brew_review_state(paths)?;
        if &*state_store.session_id() == session_id {
            if let Some(agent) = agent.as_mut() {
                agent.reset_memory()?;
                agent.prepare_for_turn()?;
                current_context(agent).map(Some)
            } else {
                cold_context(config, &store).map(Some)
            }
        } else {
            Ok(None)
        }
    };
    let context = reset().map_err(|error| AdminFailure::Internal(safe_error_message(&error)))?;
    if let Some(context) = context {
        manager.lock().unwrap().context = context;
    }
    events.publish("conversation.reset", json!({ "session_id": session_id }));
    Ok(())
}

pub(crate) fn reset_actor_persona_state(
    agent: &mut Option<Agent>,
    daemon_config: &AppConfig,
    reset_config: &AppConfig,
    paths: &GQYPaths,
    state_store: &StateStore,
    manager: &Arc<Mutex<ManagerState>>,
    events: &EventHub,
) -> std::result::Result<(), AdminFailure> {
    let mut reset = || -> Result<ContextSnapshot> {
        let persona = reset_config.active_persona_scope();
        state_store.reset_persona_contexts(&persona, "onebot")?;
        MemoryStore::new(reset_config, paths).reset_all(true)?;
        if persona != daemon_config.active_persona_scope() {
            return Ok(manager.lock().unwrap().context);
        }
        tools::clear_brew_review_state(paths)?;
        state_store.reset_conversation_usage()?;
        if let Some(agent) = agent.as_mut() {
            agent.reset_memory()?;
            agent.prepare_for_turn()?;
            current_context(agent)
        } else {
            cold_context(daemon_config, state_store)
        }
    };
    let context = reset().map_err(|error| AdminFailure::Internal(safe_error_message(&error)))?;
    manager.lock().unwrap().context = context;
    events.publish(
        "conversation.reset",
        json!({ "scope": "persona", "persona": reset_config.active_persona_scope() }),
    );
    Ok(())
}

pub(crate) fn clear_actor_session_content(
    agent: &mut Option<Agent>,
    config: &AppConfig,
    state_store: &StateStore,
    manager: &Arc<Mutex<ManagerState>>,
    session_id: &str,
) -> std::result::Result<(), AdminFailure> {
    let store = state_store.pinned(session_id);
    store
        .clear_session_content()
        .map_err(|error| AdminFailure::Internal(safe_error_message(error)))?;

    // Platform sessions normally never become the daemon's current local
    // session. Keep the in-memory context coherent if a legacy binding points
    // at that session, without clearing persona-wide memory or usage totals.
    if &*state_store.session_id() == session_id {
        let context = if let Some(agent) = agent.as_mut() {
            agent
                .reset_memory()
                .and_then(|()| agent.prepare_for_turn())
                .and_then(|()| current_context(agent))
        } else {
            cold_context(config, &store)
        }
        .map_err(|error| AdminFailure::Internal(safe_error_message(error)))?;
        manager.lock().unwrap().context = context;
    }
    Ok(())
}

/// Background-job completions wake the model so it can follow up on the
/// result autonomously. Local sessions get a real turn (or a queued
/// followup when the session is mid-turn); platform-bound sessions get a
/// plain-text broadcast into the conversation — a self-initiated platform
/// turn would need synthetic sender semantics the plugins aren't built for.
/// goal 续轮驱动器(任务#10,dsh goal-round-driver 的 daemon 化)。
/// 订阅 run 生命周期事件,在会话空闲检查点推进 armed 的 active 目标:
/// - run.completed → 尝试认领下一轮(四道栅栏见 maybe_continue_goal)
/// - run.failed → disarm(异常不自动重试,dsh 同款;等人 resume)
/// 取消→pause 的语义在 ipc Cancel 处理器里(那里能拿到被取消 run 的来源)。
pub(crate) fn install_background_job_hook(state: &DaemonState) {
    let started_state = state.clone();
    tools::jobs::set_started_hook(Arc::new(move |overview| {
        started_state
            .events
            .publish("job.started", json!({ "job": overview }));
    }));
    let hook_state = state.clone();
    tools::jobs::set_completion_hook(Arc::new(move |completion| {
        let state = hook_state.clone();
        tokio::spawn(async move {
            handle_job_completion(state, completion).await;
        });
    }));
}

pub(crate) async fn handle_job_completion(
    state: DaemonState,
    completion: tools::jobs::JobCompletion,
) {
    state.events.publish(
        "job.finished",
        json!({
            "job_id": completion.job_id,
            "title": completion.title,
            "status": completion.state_label,
            "runtime_seconds": completion.runtime_seconds,
        }),
    );
    tracing::info!(
        job_id = %completion.job_id,
        wake_requested = completion.wake_requested,
        has_session = completion.session_id.is_some(),
        has_origin_tty = completion.origin_tty.is_some(),
        "background job finished"
    );
    if !completion.wake_requested {
        // The model stopped this command itself; clean the strips quietly.
        tools::jobs::acknowledge(&completion.job_id);
        state
            .events
            .publish("job.acknowledged", json!({ "job_id": completion.job_id }));
        return;
    }
    let command_short = completion.command.chars().take(120).collect::<String>();
    let mut pending_wake_run: Option<JobWakeRun> = None;
    if let Some(session_id) = completion.session_id.clone() {
        match state.state_store.is_platform_session(&session_id) {
            Ok(true) => {
                wake_platform_session_for_job(&state, &session_id, &completion).await;
            }
            Ok(false) => {
                pending_wake_run =
                    wake_local_session_for_job(&state, session_id, &completion, &command_short);
            }
            Err(error) => {
                tracing::warn!(
                    job_id = %completion.job_id,
                    error = %error,
                    "failed to resolve the session of a finished background command"
                );
            }
        }
    }
    // Keep the finished job visible in UI strips until its wake turn is done
    // (the report is what replaces the strip line); everything else clears
    // right away.
    if let Some(wake) = pending_wake_run {
        // 流式回写与等待循环并行:回合一开跑就把思考/工具/正文追加进触发
        // 终端,acknowledge 只关心回合何时结束。
        if completion.origin_tty.is_some() {
            let stream_state = state.clone();
            let stream_completion = completion.clone();
            let stream_wake = wake.clone();
            tokio::spawn(async move {
                stream_job_wake_to_origin_tty(stream_state, stream_completion, stream_wake).await;
            });
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(600);
        while std::time::Instant::now() < deadline {
            let still_running = state
                .manager
                .lock()
                .unwrap()
                .active_runs
                .contains_key(&wake.run_id);
            if !still_running {
                break;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }
    tools::jobs::acknowledge(&completion.job_id);
    state
        .events
        .publish("job.acknowledged", json!({ "job_id": completion.job_id }));
}

/// 本地会话唤醒回合的标识:run id + 事件订阅起点(在回合入队前取,保证
/// 从 turn.started 起一帧不漏)。
#[derive(Clone)]
pub(crate) struct JobWakeRun {
    run_id: String,
    events_after: u64,
}

pub(crate) enum TtyWriteOp {
    Write(String),
    /// 正常收尾:flush 后给 shell 发 SIGWINCH 促使重绘提示符。
    Finish,
    /// 中途收笔(前台被占/超时):已写的留在屏上,不再动那个终端。
    Abort,
}

/// 把唤醒回合流式渲染进当初触发 shellhook/单次 CLI 的终端:思考(暗色,按
/// display.reasoning 配置)、工具行、正文逐行 Markdown。触发端进程早已退出,
/// 由 daemon 直接写 tty 设备。三道闸全过才动笔:
/// 1. `notifications.job_writeback_to_terminal` 开关(默认开);
/// 2. 触发 shell 还活着且 stdin 仍指向记录的 tty——终端关闭、pid 复用都拦下;
/// 3. shell 空闲在前台提示符(tpgid==pgrp)——正开着 vim/htop 时绝不能撕屏。
/// 追加式输出,无光标控制;每次落笔前重查第 3 道闸,中途被占立即收笔并补
/// 桌面通知。物理写入走专职线程,^S 流控卡死也只占一根线程。
pub(crate) async fn stream_job_wake_to_origin_tty(
    state: DaemonState,
    completion: tools::jobs::JobCompletion,
    wake: JobWakeRun,
) {
    let Some(origin) = completion.origin_tty.clone() else {
        return;
    };
    let config = crate::config::AppConfig::load_or_default(&state.paths).unwrap_or_default();
    if !config.notifications.job_writeback_to_terminal {
        return;
    }
    let notify_fallback = |reason: &str| {
        tracing::info!(job_id = %completion.job_id, reason, "job wake writeback fell back to a notification");
        if config.notifications.enabled {
            crate::notify::notify(
                &format!("GQY 后台任务跟进 · {}", completion.title),
                "任务已完成,跟进回复在会话里(终端不在提示符,没有直接写入)。",
            );
        }
    };
    if !origin_shell_at_prompt(&origin) {
        notify_fallback("shell not at prompt");
        return;
    }
    use std::os::unix::fs::OpenOptionsExt;
    let tty = match std::fs::OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_NOCTTY)
        .open(&origin.path)
    {
        Ok(tty) => tty,
        Err(error) => {
            tracing::debug!(job_id = %completion.job_id, %error, "origin tty open failed");
            notify_fallback("tty open failed");
            return;
        }
    };
    tracing::info!(
        job_id = %completion.job_id,
        run_id = %wake.run_id,
        tty = %origin.path.display(),
        shell_pid = origin.shell_pid,
        "streaming job wake reply to the originating terminal"
    );

    let (ops_tx, ops_rx) = std::sync::mpsc::channel::<TtyWriteOp>();
    let shell_pid = origin.shell_pid;
    let writer = std::thread::Builder::new()
        .name("gqy-tty-writeback".to_string())
        .spawn(move || origin_tty_writer(tty, shell_pid, ops_rx));
    if writer.is_err() {
        notify_fallback("writer thread spawn failed");
        return;
    }

    let reasoning_mode =
        crate::render::ReasoningDisplayMode::from_config(&config.display.reasoning);
    // 落笔即有反馈:头部先行,正文随事件到达逐行追加。
    let _ = ops_tx.send(TtyWriteOp::Write(format!(
        "\r\n\x1b[1m✦ GQY 后台任务跟进\x1b[0m \x1b[2m· {}\x1b[0m\r\n\r\n",
        completion.title
    )));

    let mut subscription = state.events.subscribe_after(wake.events_after);
    let deadline = std::time::Instant::now() + Duration::from_secs(900);
    let mut reasoning_buf = String::new();
    let mut content_buf = String::new();
    let mut wrote_reasoning = false;
    let mut reasoning_open = false;
    let mut last_id = wake.events_after;
    let mut aborted = false;
    loop {
        if std::time::Instant::now() > deadline {
            aborted = true;
            break;
        }
        let record = if let Some(record) = subscription.pending.pop_front() {
            record
        } else {
            match tokio::time::timeout(Duration::from_secs(30), subscription.receiver.recv()).await
            {
                Ok(Ok(record)) => record,
                Ok(Err(broadcast::error::RecvError::Lagged(_))) => {
                    subscription.pending = state.events.replay_after(last_id);
                    continue;
                }
                Ok(Err(broadcast::error::RecvError::Closed)) => break,
                Err(_) => {
                    // 静默期顺手确认回合还活着,免得错过终态事件后干等。
                    if !state
                        .manager
                        .lock()
                        .unwrap()
                        .active_runs
                        .contains_key(&wake.run_id)
                    {
                        break;
                    }
                    continue;
                }
            }
        };
        last_id = record.id;
        let Ok(data) = serde_json::from_str::<Value>(&record.data) else {
            continue;
        };
        if data.get("run_id").and_then(Value::as_str) != Some(wake.run_id.as_str()) {
            if !state
                .manager
                .lock()
                .unwrap()
                .active_runs
                .contains_key(&wake.run_id)
            {
                break;
            }
            continue;
        }

        let mut chunk_out = String::new();
        match record.kind.as_str() {
            "reasoning.title" => {
                if matches!(reasoning_mode, crate::render::ReasoningDisplayMode::Summary) {
                    if let Some(title) = data.get("title").and_then(Value::as_str) {
                        flush_line_buf(
                            &mut reasoning_buf,
                            WriteLineStyle::Reasoning,
                            &mut chunk_out,
                        );
                        push_rendered_line(
                            &format!("∴ {title}"),
                            WriteLineStyle::Reasoning,
                            &mut chunk_out,
                        );
                        wrote_reasoning = true;
                        reasoning_open = true;
                    }
                }
            }
            "reasoning.delta" => {
                if matches!(reasoning_mode, crate::render::ReasoningDisplayMode::Full) {
                    if let Some(delta) = data.get("delta").and_then(Value::as_str) {
                        reasoning_buf.push_str(delta);
                        drain_line_buf(
                            &mut reasoning_buf,
                            WriteLineStyle::Reasoning,
                            &mut chunk_out,
                        );
                        wrote_reasoning = true;
                        reasoning_open = true;
                    }
                }
            }
            "reasoning.part_end" | "reasoning.reset" => {
                flush_line_buf(
                    &mut reasoning_buf,
                    WriteLineStyle::Reasoning,
                    &mut chunk_out,
                );
            }
            "tool.started" => {
                flush_line_buf(
                    &mut reasoning_buf,
                    WriteLineStyle::Reasoning,
                    &mut chunk_out,
                );
                if reasoning_open {
                    chunk_out.push_str("\r\n");
                    reasoning_open = false;
                }
                let name = data
                    .get("display_name")
                    .or_else(|| data.get("name"))
                    .and_then(Value::as_str)
                    .unwrap_or("工具");
                push_rendered_line(&format!("⚙ {name} …"), WriteLineStyle::Note, &mut chunk_out);
            }
            "tool.finished" => {
                if data.get("ok").and_then(Value::as_bool) == Some(false) {
                    let name = data
                        .get("display_name")
                        .or_else(|| data.get("name"))
                        .and_then(Value::as_str)
                        .unwrap_or("工具");
                    push_rendered_line(
                        &format!("⚙ {name} 失败"),
                        WriteLineStyle::Note,
                        &mut chunk_out,
                    );
                }
            }
            "assistant.delta" => {
                flush_line_buf(
                    &mut reasoning_buf,
                    WriteLineStyle::Reasoning,
                    &mut chunk_out,
                );
                if reasoning_open
                    || (wrote_reasoning && content_buf.is_empty() && chunk_out.is_empty())
                {
                    chunk_out.push_str("\r\n");
                    reasoning_open = false;
                    wrote_reasoning = false;
                }
                if let Some(delta) = data.get("delta").and_then(Value::as_str) {
                    content_buf.push_str(delta);
                    drain_line_buf(&mut content_buf, WriteLineStyle::Content, &mut chunk_out);
                }
            }
            "run.completed" | "run.failed" | "run.cancelled" => {
                flush_line_buf(
                    &mut reasoning_buf,
                    WriteLineStyle::Reasoning,
                    &mut chunk_out,
                );
                flush_line_buf(&mut content_buf, WriteLineStyle::Content, &mut chunk_out);
                if record.kind != "run.completed" {
                    push_rendered_line("(跟进中断)", WriteLineStyle::Note, &mut chunk_out);
                }
                // fish/zsh 收到 SIGWINCH 重绘提示符时,会从光标行向上清掉
                // 自家提示符高度的行数再画(starship 双行提示符实测清 2 行)。
                // 垫两行空白当牺牲品,免得清到正文末行。
                chunk_out.push_str("\r\n\r\n\r\n");
                let _ = ops_tx.send(TtyWriteOp::Write(chunk_out));
                let _ = ops_tx.send(TtyWriteOp::Finish);
                tracing::info!(
                    job_id = %completion.job_id,
                    outcome = %record.kind,
                    "job wake reply streamed to the originating terminal"
                );
                return;
            }
            _ => {}
        }
        if !chunk_out.is_empty() {
            // 落笔前重查前台闸:用户开了全屏程序就立即收笔,已写的留在屏上。
            if !origin_shell_at_prompt(&origin) {
                aborted = true;
                break;
            }
            let _ = ops_tx.send(TtyWriteOp::Write(chunk_out));
        }
    }
    let _ = ops_tx.send(TtyWriteOp::Abort);
    if aborted {
        notify_fallback("interrupted mid-stream");
    }
}

/// 回写行的三种笔触:正文走 Markdown 渲染;思考用与 REPL 正常思考一致的
/// 绿色(write_full_reasoning_chunk 同款 ANSI 10);注记(工具行/中断标记)暗色。
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum WriteLineStyle {
    Content,
    Reasoning,
    Note,
}

/// 行缓冲落盘:凑满整行才渲染。
pub(crate) fn drain_line_buf(buf: &mut String, style: WriteLineStyle, out: &mut String) {
    while let Some(index) = buf.find('\n') {
        let line: String = buf.drain(..=index).collect();
        let line = line.trim_end_matches(['\n', '\r']);
        push_rendered_line(line, style, out);
    }
}

pub(crate) fn flush_line_buf(buf: &mut String, style: WriteLineStyle, out: &mut String) {
    if buf.trim().is_empty() {
        buf.clear();
        return;
    }
    let line = std::mem::take(buf);
    push_rendered_line(line.trim_end(), style, out);
}

pub(crate) fn push_rendered_line(line: &str, style: WriteLineStyle, out: &mut String) {
    match style {
        WriteLineStyle::Content => out.push_str(&crate::render::render_markdown_line(line)),
        WriteLineStyle::Reasoning => {
            if !line.is_empty() {
                out.push_str(&format!("\x1b[38;5;10m{line}\x1b[0m"));
            }
        }
        WriteLineStyle::Note => {
            if !line.is_empty() {
                out.push_str(&format!("\x1b[2m{line}\x1b[0m"));
            }
        }
    }
    out.push_str("\r\n");
}

/// 三道闸的第 2、3 道:shell 活着、还挂在记录的 tty 上、且自己就是终端前台
/// 进程组(即停在提示符,没在跑别的程序)。
pub(crate) fn origin_shell_at_prompt(origin: &crate::ipc::OriginTty) -> bool {
    let pid = origin.shell_pid;
    let Ok(stdin_target) = std::fs::read_link(format!("/proc/{pid}/fd/0")) else {
        return false;
    };
    if stdin_target != origin.path {
        return false;
    }
    let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return false;
    };
    matches!(parse_stat_pgrp_tpgid(&stat), Some((pgrp, tpgid)) if pgrp == tpgid)
}

/// /proc/pid/stat 的 comm 字段可含空格和括号,必须从最后一个 \')\' 之后再按空白
/// 切:其后第 3 个字段是 pgrp,第 6 个是 tpgid。
pub(crate) fn parse_stat_pgrp_tpgid(stat: &str) -> Option<(i64, i64)> {
    let (_, rest) = stat.rsplit_once(')')?;
    let mut fields = rest.split_whitespace();
    let pgrp = fields.nth(2)?.parse().ok()?;
    let tpgid = fields.nth(2)?.parse().ok()?;
    Some((pgrp, tpgid))
}

/// 专职写线程:tty 是同步阻塞设备(^S 流控可以永久卡住 write),隔离在自己
/// 的线程里,卡死也只占一根线程,不拖累 daemon 的 async runtime。
pub(crate) fn origin_tty_writer(
    mut tty: std::fs::File,
    shell_pid: u32,
    ops: std::sync::mpsc::Receiver<TtyWriteOp>,
) {
    use std::io::Write;
    for op in ops {
        match op {
            TtyWriteOp::Write(text) => {
                if tty.write_all(text.as_bytes()).is_err() {
                    return;
                }
            }
            TtyWriteOp::Finish => {
                let _ = tty.flush();
                // 提示符被我们的输出推到半空,SIGWINCH 让 shell(fish/zsh/新
                // bash 的 readline 都处理)原地重绘一行干净的提示符。
                unsafe {
                    libc::kill(shell_pid as i32, libc::SIGWINCH);
                }
                return;
            }
            TtyWriteOp::Abort => return,
        }
    }
}

pub(crate) fn wake_local_session_for_job(
    state: &DaemonState,
    session_id: Arc<str>,
    completion: &tools::jobs::JobCompletion,
    command_short: &str,
) -> Option<JobWakeRun> {
    let noun = if completion.is_subagent {
        "后台子代理"
    } else {
        "后台命令"
    };
    // 结果直接附在唤醒里,不再让模型「先去查一次再汇报」。子代理给完整结论
    // (它就是交付物),命令给日志尾部;剩下的自己判断——只给事实和日志路径,
    // 不给动作指示。
    let result_block = tools::jobs::completion_result(
        &completion.log_path,
        completion.is_subagent,
        completion.exit_code == Some(0),
    )
    .map(|(label, body)| format!("- {label}:\n{body}\n"))
    .unwrap_or_default();
    let content = format!(
        "<background-job-report>{noun}「{}」已执行完毕：\n\
         - job_id: {}\n- 任务: {}\n- 状态: {}（运行 {} 秒）\n\
         - 日志: {}\n{result_block}\
         这是系统自动触发的跟进，不是用户消息。\
         </background-job-report>",
        completion.title,
        completion.job_id,
        command_short,
        completion.state_label,
        completion.runtime_seconds,
        completion.log_path.display(),
    );
    let display_content = format!(
        "[后台任务完成] {}完成 {} · {}",
        if completion.is_subagent {
            "子代理"
        } else {
            "命令"
        },
        completion.job_id,
        completion.title
    );

    // Mid-turn session: ride the queue so the model reacts within the
    // running reply instead of colliding with it.
    let queued = {
        let manager = state.manager.lock().unwrap();
        manager
            .active_runs
            .iter()
            .find(|(_, info)| *info.session_id == *session_id)
            .map(|(run_id, info)| (run_id.clone(), info.queue_target.clone(), info.audience))
    };
    if let Some((run_id, queue_target, audience)) = queued {
        tracing::info!(
            job_id = %completion.job_id,
            run_id = %run_id,
            has_queue_target = queue_target.is_some(),
            "job wake joining the session's active run"
        );
        let Some(target) = queue_target else {
            // Turn is still starting; report on the next completion poll
            // rather than racing its queue setup.
            tracing::debug!(job_id = %completion.job_id, "job wake skipped: turn starting");
            return None;
        };
        let request = TurnUpdateRequest {
            run_id,
            turn_id: target.turn_id,
            session_id: Some(session_id.clone()),
            audience,
            content,
            display_content,
            attachments: Vec::new(),
            uploaded_attachment_ids: Vec::new(),
            mode: TurnUpdateMode::Followup,
        };
        if let Err(error) = enqueue_turn_update(state, request) {
            tracing::debug!(
                job_id = %completion.job_id,
                error = %error,
                "job wake could not join the running turn"
            );
        }
        return None;
    }

    let run_id = random_id("run", 18);
    let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
    {
        let mut manager = state.manager.lock().unwrap();
        if manager.admin_busy {
            tracing::debug!(job_id = %completion.job_id, "job wake skipped: admin busy");
            return None;
        }
        manager.active_runs.insert(
            run_id.clone(),
            RunInfo {
                session_id: session_id.clone(),
                mode: AgentMode::Normal,
                audience: PromptAudience::Owner,
                cancel: cancel_tx,
                turn_id: None,
                queue_target: None,
                supersede: Arc::new(crate::agent::TurnSupersedeSignal::default()),
                platform_followup: None,
                operation: RunOperation::Create,
                job_wake: true,
                turn_origin: crate::tools::workspace::TurnOrigin::JobWake,
                job_wake_label: Some(format!(
                    "{}完成 {} · {}",
                    if completion.is_subagent {
                        "子代理"
                    } else {
                        "命令"
                    },
                    completion.job_id,
                    completion.title
                )),
            },
        );
    }
    // 订阅起点在入队前取:回合的 turn.started 起所有事件都不漏给流式回写。
    let events_after = state.events.latest_id();
    if state
        .actor_tx
        .send(ActorCommand::StartTurn {
            run_id: run_id.clone(),
            session_id,
            content,
            display_content,
            attachment_run_id: None,
            mode: AgentMode::Normal,
            images: Vec::new(),
            cwd: Some(completion.workspace.clone()),
            origin_tty: completion.origin_tty.clone(),
            audience: PromptAudience::Owner,
            profile: None,
            cancel: cancel_rx,
            turn_origin: Box::new(crate::tools::workspace::TurnOrigin::JobWake),
        })
        .is_err()
    {
        finish_run(&state.manager, &run_id, None);
        return None;
    }
    Some(JobWakeRun {
        run_id,
        events_after,
    })
}

pub(crate) async fn wake_platform_session_for_job(
    state: &DaemonState,
    session_id: &Arc<str>,
    completion: &tools::jobs::JobCompletion,
) {
    let persona = state.manager.lock().unwrap().config.active_persona_scope();
    let binding = state
        .state_store
        .platform_session_bindings(&persona, "onebot")
        .ok()
        .and_then(|bindings| {
            bindings
                .into_iter()
                .find(|binding| binding.session_id == **session_id)
        });
    let Some(binding) = binding else {
        tracing::debug!(job_id = %completion.job_id, "job wake skipped: no platform binding");
        return;
    };
    let noun = if completion.is_subagent {
        "后台子代理"
    } else {
        "后台命令"
    };
    // 与本地唤醒同款:结果直接附在唤醒里(子代理给完整结论,命令给日志尾部),
    // 只给事实,不再指示模型「先去查一次再汇报」。
    let result_block = tools::jobs::completion_result(
        &completion.log_path,
        completion.is_subagent,
        completion.exit_code == Some(0),
    )
    .map(|(label, body)| format!("- {label}:\n{body}\n"))
    .unwrap_or_default();
    let content = format!(
        "<background-job-report>{noun}「{}」已执行完毕：\n- job_id: {}\n- 任务: {}\n- 状态: {}（运行 {} 秒）\n\
         {result_block}这是系统自动触发的跟进，不是用户消息。\
         </background-job-report>",
        completion.title,
        completion.job_id,
        completion.command.chars().take(200).collect::<String>(),
        completion.state_label,
        completion.runtime_seconds
    );
    if let Err(error) = crate::platforms::onebot::wake_conversation_for_job(
        state,
        &binding.key.account_id,
        &binding.key.conversation_kind,
        &binding.key.conversation_id,
        completion.platform_sender.as_deref(),
        content,
    )
    .await
    {
        tracing::warn!(
            job_id = %completion.job_id,
            error = %error,
            "failed to wake the model for a background command in QQ"
        );
    }
}

/// An explicit cancel withdraws the follow-ups still queued behind the
/// reply: the user aborted the exchange, so folding them into context would
/// keep answering messages they no longer want processed. Published before
/// `run.cancelled` so clients still draining the event stream can clear
/// their queue bubbles.
pub(crate) fn drop_cancelled_queue(
    store: &StateStore,
    events: &EventHub,
    run_id: &str,
    session_id: &str,
) {
    match store.delete_queued_prompts() {
        Ok(prompt_ids) => {
            for prompt_id in prompt_ids {
                events.publish(
                    "queue.removed",
                    json!({
                        "session_id": session_id,
                        "run_id": run_id,
                        "prompt_id": prompt_id,
                    }),
                );
            }
        }
        Err(error) => {
            tracing::warn!(
                run_id,
                error = %error,
                "{}",
                t(
                    "failed to drop queued prompts for a cancelled turn",
                    "无法丢弃已取消回复的排队消息"
                )
            );
        }
    }
}

pub(crate) fn finish_cancelled_run(
    manager: &Arc<Mutex<ManagerState>>,
    events: &EventHub,
    agent: &Agent,
    run_id: &str,
    session_id: &str,
    updates_context: bool,
) {
    let context = current_context(agent).ok().filter(|_| updates_context);
    let mut payload = json!({ "run_id": run_id, "session_id": session_id });
    if let Some(context) = &context {
        // The interrupted turn is persisted into the context; keep the client
        // context meters honest instead of leaving them at the pre-turn value.
        payload["context_tokens"] = json!(context.tokens);
        payload["context_window"] = json!(context.window);
        payload["cumulative_tokens"] = json!(context.cumulative_tokens);
        payload["cumulative_prompt_tokens"] = json!(context.cumulative_prompt_tokens);
        payload["cumulative_cache_read_tokens"] = json!(context.cumulative_cache_read_tokens);
    }
    finish_run(manager, run_id, context);
    events.publish("run.cancelled", payload);
}

pub(crate) fn finish_failed_run(
    manager: &Arc<Mutex<ManagerState>>,
    events: &EventHub,
    questions: &QuestionBroker,
    agent: &Agent,
    run_id: &str,
    session_id: &str,
    updates_context: bool,
    error: &anyhow::Error,
) {
    questions.cancel_run(run_id);
    let context = current_context(agent).ok().filter(|_| updates_context);
    finish_run(manager, run_id, context);
    let message = safe_error_message(error);
    tracing::error!(
        run_id,
        error = %error,
        "{}",
        t("WebUI agent run failed", "WebUI 智能体运行失败")
    );
    events.publish(
        "run.failed",
        json!({ "run_id": run_id, "session_id": session_id, "message": message }),
    );
}

pub(crate) fn finish_completed_with_context_error(
    manager: &Arc<Mutex<ManagerState>>,
    events: &EventHub,
    agent: &Agent,
    run_id: &str,
    session_id: &str,
    updates_context: bool,
    result: &ChatResult,
    error: &anyhow::Error,
) {
    let message = safe_error_message(error);
    tracing::error!(
        run_id,
        error = %error,
        "{}",
        t(
            "WebUI post-turn context maintenance failed",
            "WebUI 回合后上下文维护失败"
        )
    );
    events.publish(
        "context.error",
        json!({ "run_id": run_id, "session_id": session_id, "message": message }),
    );
    let context = current_context(agent).unwrap_or_else(|_| manager.lock().unwrap().context);
    finish_run(manager, run_id, updates_context.then_some(context));
    publish_completed(events, run_id, session_id, result, context);
}

pub(crate) fn finish_run(
    manager: &Arc<Mutex<ManagerState>>,
    run_id: &str,
    context: Option<ContextSnapshot>,
) {
    let mut manager = manager.lock().unwrap();
    if let Some(context) = context {
        manager.context = context;
    }
    if let Some(run) = manager.active_runs.remove(run_id) {
        if let Some(followup) = run.platform_followup {
            followup.close();
        }
    }
}

pub(crate) fn publish_completed(
    events: &EventHub,
    run_id: &str,
    session_id: &str,
    result: &ChatResult,
    context: ContextSnapshot,
) {
    // Always the local estimate of the persisted context: provider-reported
    // request usage measures what this turn consumed, not what the context
    // holds now — the two diverge after post-turn compaction/pruning, and
    // the footer meter must refresh with those rewrites.
    let context_tokens = context.tokens;
    events.publish(
        "run.completed",
        json!({
            "run_id": run_id,
            "session_id": session_id,
            "usage": result.usage,
            "usage_estimated": result.usage_estimated,
            "provider_id": result.provider_id,
            "model": result.model,
            "context_tokens": context_tokens,
            "context_window": context.window,
            "cumulative_tokens": context.cumulative_tokens,
            "cumulative_prompt_tokens": context.cumulative_prompt_tokens,
            "cumulative_cache_read_tokens": context.cumulative_cache_read_tokens,
        }),
    );
}

pub(crate) fn current_context(agent: &Agent) -> Result<ContextSnapshot> {
    let cumulative = agent.conversation_usage_token_totals()?;
    Ok(ContextSnapshot {
        tokens: agent.effective_context_tokens()?,
        window: agent.context_window(),
        cumulative_tokens: cumulative.total,
        cumulative_prompt_tokens: cumulative.prompt,
        cumulative_cache_read_tokens: cumulative.cache_read,
    })
}

pub(crate) fn build_actor_agent(
    config: &AppConfig,
    paths: &GQYPaths,
    state: &StateStore,
) -> Result<Agent> {
    let mut agent = build_session_agent(config, paths, state)?;
    agent.prepare_for_turn()?;
    Ok(agent)
}

pub(crate) fn build_session_agent(
    config: &AppConfig,
    paths: &GQYPaths,
    state: &StateStore,
) -> Result<Agent> {
    crate::models_cache::ensure_active_metadata(paths, config);
    let client = OpenAiCompatibleClient::from_config(config, paths)?;
    let registry = build_tool_registry(config, paths, AgentMode::Normal, true)?;
    Agent::new(
        config.clone(),
        paths,
        state.clone(),
        client,
        registry,
        AgentMode::Normal,
    )
}

pub(crate) fn ensure_actor_agent<'a>(
    agent: &'a mut Option<Agent>,
    config: &AppConfig,
    paths: &GQYPaths,
    state: &StateStore,
    turn_engine: &TurnEngineState,
) -> std::result::Result<&'a mut Agent, AdminFailure> {
    if agent.is_none() {
        turn_engine.set(TurnEngineState::INITIALIZING);
        match build_actor_agent(config, paths, state) {
            Ok(next) => {
                *agent = Some(next);
                turn_engine.set(TurnEngineState::READY);
            }
            Err(error) => {
                turn_engine.set(TurnEngineState::FAILED);
                return Err(AdminFailure::Internal(safe_error_message(error)));
            }
        }
    }
    Ok(agent.as_mut().expect("actor agent was initialized"))
}

pub(crate) fn actor_context(
    agent: &Option<Agent>,
    config: &AppConfig,
    state: &StateStore,
) -> Result<ContextSnapshot> {
    agent
        .as_ref()
        .map_or_else(|| cold_context(config, state), current_context)
}

pub(crate) fn cold_context(
    config: &AppConfig,
    state_store: &StateStore,
) -> Result<ContextSnapshot> {
    let cumulative = state_store.session_cumulative_token_totals()?;
    Ok(ContextSnapshot {
        tokens: 0,
        window: config.active_context_window()?,
        cumulative_tokens: cumulative.total,
        cumulative_prompt_tokens: cumulative.prompt,
        cumulative_cache_read_tokens: cumulative.cache_read,
    })
}

pub(crate) fn session_state(
    manager: &Arc<Mutex<ManagerState>>,
    state_store: &StateStore,
) -> Result<ipc::SessionState> {
    let context = manager.lock().unwrap().context;
    let session_id = state_store.session_id();
    let record = state_store.session_record(&session_id)?;
    Ok(ipc::SessionState {
        context_tokens: context.tokens,
        context_window: context.window,
        cumulative_tokens: context.cumulative_tokens,
        cumulative_prompt_tokens: context.cumulative_prompt_tokens,
        cumulative_cache_read_tokens: context.cumulative_cache_read_tokens,
        session_id: session_id.to_string(),
        session_name: record
            .as_ref()
            .map(|record| record.name.clone())
            .unwrap_or_default(),
        workspace: record.and_then(|record| record.workspace),
    })
}

pub(crate) fn session_state_for(
    state: &DaemonState,
    session_id: &str,
) -> Result<ipc::SessionState> {
    let record = state
        .state_store
        .session_record(session_id)?
        .with_context(|| format!("session not found: {session_id}"))?;
    let current_session_id = state.state_store.session_id();
    let context = if &*current_session_id == session_id {
        state.manager.lock().unwrap().context
    } else {
        let config = state.manager.lock().unwrap().config.clone();
        let store = state.state_store.pinned(session_id);
        current_context(&build_session_agent(&config, &state.paths, &store)?)?
    };
    Ok(ipc::SessionState {
        context_tokens: context.tokens,
        context_window: context.window,
        cumulative_tokens: context.cumulative_tokens,
        cumulative_prompt_tokens: context.cumulative_prompt_tokens,
        cumulative_cache_read_tokens: context.cumulative_cache_read_tokens,
        session_id: record.session_id,
        session_name: record.name,
        workspace: record.workspace,
    })
}

/// Global admin reservation (config/model changes): requires that no turn is
/// running in any session.
pub(crate) fn reserve_admin(
    manager: &Arc<Mutex<ManagerState>>,
) -> std::result::Result<(), ApiError> {
    let mut manager = manager.lock().unwrap();
    if !manager.active_runs.is_empty() || manager.admin_busy {
        return Err(ApiError::new(StatusCode::CONFLICT, ipc::ADMIN_BUSY_MESSAGE));
    }
    manager.admin_busy = true;
    Ok(())
}

/// Per-session admin reservation (reset/undo/pop/compact/delete/archive):
/// only the target session must be idle; turns in other sessions keep
/// running.
pub(crate) fn reserve_admin_for_session(
    manager: &Arc<Mutex<ManagerState>>,
    session_id: &str,
) -> std::result::Result<(), ApiError> {
    let mut manager = manager.lock().unwrap();
    if manager.admin_busy || manager.session_has_runs(session_id) {
        return Err(ApiError::new(StatusCode::CONFLICT, ipc::ADMIN_BUSY_MESSAGE));
    }
    manager.admin_busy = true;
    Ok(())
}

pub(crate) async fn clear_platform_session_content(
    state: &DaemonState,
    session_id: Arc<str>,
) -> std::result::Result<(), PlatformSessionResetError> {
    state
        .state_store
        .recover_stale_turns()
        .map_err(|error| PlatformSessionResetError::Internal(safe_error_message(error)))?;
    {
        let mut manager = state.manager.lock().unwrap();
        if manager.admin_busy || manager.session_has_runs(&session_id) {
            return Err(PlatformSessionResetError::Busy);
        }
        manager.admin_busy = true;
    }

    let target = state.state_store.pinned(&session_id);
    match target.has_running_turns() {
        Ok(false) => {}
        Ok(true) => {
            release_admin(&state.manager);
            return Err(PlatformSessionResetError::Busy);
        }
        Err(error) => {
            release_admin(&state.manager);
            return Err(PlatformSessionResetError::Internal(safe_error_message(
                error,
            )));
        }
    }

    let (reply, receiver) = oneshot::channel();
    if state
        .actor_tx
        .send(ActorCommand::ClearSessionContent { session_id, reply })
        .is_err()
    {
        release_admin(&state.manager);
        return Err(PlatformSessionResetError::Unavailable);
    }
    match receiver.await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(AdminFailure::Invalid(message) | AdminFailure::Internal(message))) => {
            Err(PlatformSessionResetError::Internal(message))
        }
        Err(_) => {
            release_admin(&state.manager);
            Err(PlatformSessionResetError::Unavailable)
        }
    }
}
