//! agent_impl — 自 src/agent/mod.rs 拆分。

use super::*;

mod agent_impl2;
use agent_impl2::*;
impl Agent {
    pub fn new(
        config: AppConfig,
        paths: &GQYPaths,
        state: StateStore,
        client: OpenAiCompatibleClient,
        tools: ToolRegistry,
        mode: AgentMode,
    ) -> Result<Self> {
        Self::new_for_audience(
            config,
            paths,
            state,
            client,
            tools,
            mode,
            PromptAudience::Owner,
        )
    }

    pub(crate) fn new_for_audience(
        config: AppConfig,
        paths: &GQYPaths,
        state: StateStore,
        client: OpenAiCompatibleClient,
        tools: ToolRegistry,
        mode: AgentMode,
        prompt_audience: PromptAudience,
    ) -> Result<Self> {
        // Construction is side-effect free (aside from idempotent memory
        // init) so concurrent turns can each build their own Agent; startup
        // maintenance (prompt-change reset, stale-turn recovery) lives in
        // `prepare_for_turn`.
        // dev 有自己的记忆/技能(=切人格语义):把 config 的人格指针换成
        // 保留人格 "dev",此后 MemoryStore/skills 派生目录全部随之隔离。
        let config = if mode == AgentMode::Dev {
            config.dev_scoped()
        } else {
            config
        };
        let base_system_prompt = mode_system_prompt(&config, paths, mode, prompt_audience)?;
        let system_prompt = with_host_environment(
            with_mode_reminder(base_system_prompt, mode),
            prompt_audience,
            paths,
            mode,
        );
        let tools_enabled = config.tools.enabled;
        let max_tool_rounds = config.tools.max_rounds;
        // Dev 无人格:预设对话整套跳过。
        let preset_dialogs = if mode == AgentMode::Dev {
            Vec::new()
        } else {
            persona_hint::load_dialogs(&config, paths, &config.active_persona_scope())
        };
        let memory = MemoryStore::new(&config, paths);
        memory.init()?;
        let (memory_database_id, memory_generation) = memory.identity()?;
        let memory_origin = MemoryOrigin::local(state.session_id().to_string());
        let on_overflow = config.context.on_overflow.clone();
        Ok(Self {
            state,
            client,
            system_prompt,
            runtime_system_context: Vec::new(),
            turn_system_context: Vec::new(),
            memory_content: None,
            suppress_session_history: false,
            trim_at_ratio: config.context.trim_at_ratio,
            trim_batch_ratio: config.context.trim_batch_ratio,
            tools_enabled,
            max_tool_rounds,
            tools: Arc::new(Mutex::new(tools)),
            memory,
            memory_organizer: None,
            memory_origin,
            memory_database_id,
            memory_generation,
            mode,
            prompt_audience,
            config,
            paths: paths.clone(),
            on_overflow,
            turn_display_content: None,
            attachment_run_id: None,
            image_platform: None,
            image_platform_label: None,
            platform_context: None,
            context_images: Vec::new(),
            persona_reminder: None,
            repeat_chain: crate::tools::repeat_reminder::RepeatChain::default(),
            preset_dialogs,
            last_request_snapshot: None,
            last_request_endpoint: None,
            keepalive_cancel: None,
            consecutive_compacts: std::sync::atomic::AtomicU32::new(0),
            compact_stuck: std::sync::atomic::AtomicBool::new(false),
            last_compact_max_seq: std::sync::atomic::AtomicI64::new(-1),
            rapid_compacts: std::sync::atomic::AtomicU32::new(0),
            soft_notice_sent: std::sync::atomic::AtomicBool::new(false),
        })
    }

    /// Stops the idle cache-keepalive loop (called whenever a new request is
    /// about to change the context, and before dropping the agent).
    pub fn cancel_cache_keepalive(&mut self) {
        if let Some(cancel) = self.keepalive_cancel.take() {
            cancel.store(true, std::sync::atomic::Ordering::Release);
        }
    }

    /// Starts the idle keepalive loop for the last request prefix. No-op when
    /// disabled or when no snapshot exists.
    pub fn start_cache_keepalive(&mut self) {
        self.cancel_cache_keepalive();
        let interval = self.config.cache.keepalive_seconds;
        if interval == 0 {
            return;
        }
        let Some((messages, tools)) = self.last_request_snapshot.clone() else {
            return;
        };
        let endpoint_hint = self.last_request_endpoint.clone();
        let max_pings = self.config.cache.keepalive_max_pings;
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        self.keepalive_cancel = Some(cancel.clone());
        let client = self.client.clone();
        let state = self.state.clone();
        let usage_source = self.usage_source().to_string();
        tokio::spawn(async move {
            for ping in 0..max_pings {
                tokio::time::sleep(std::time::Duration::from_secs(interval)).await;
                if cancel.load(std::sync::atomic::Ordering::Acquire) {
                    return;
                }
                match client
                    .cache_keepalive(messages.clone(), tools.clone(), endpoint_hint.as_ref())
                    .await
                {
                    Ok(Some(usage)) => {
                        tracing::info!(
                            ping = ping + 1,
                            prompt_tokens = usage.prompt_tokens,
                            cache_read = usage.cache_read_tokens,
                            "cache keepalive ping"
                        );
                        let meta = crate::state::UsageMeta {
                            source: &usage_source,
                            provider: Some(client.provider_id()),
                            model: None,
                        };
                        let _ = state.add_auxiliary_usage(&usage, meta);
                    }
                    Ok(None) => return, // protocol without keepalive support
                    Err(error) => {
                        tracing::warn!(error = %error, "cache keepalive ping failed");
                        return;
                    }
                }
            }
        });
    }

    /// 用量历史的来源标签:平台回合记平台 id(如 "qq"),其余一律 "agent"。
    /// dsh 式工具输出外溢(spill):模型侧内联超过 context.tool_output_spill_bytes
    /// 的纯文本结果全文存进会话级文件,内联替换为头尾预览+定位提示。read_file
    /// 不外溢(避免 读→溢→再读 循环);存盘失败保留原文,绝不把成功调用变错误。
    /// 只约束进模型的消息,程序侧(报告提取/load_tools 解析等)继续用完整值。
    pub fn spill_tool_output(
        &self,
        turn_id: &str,
        call_id: &str,
        tool_name: &str,
        output: &str,
    ) -> Option<String> {
        let cap = self.config.context.tool_output_spill_bytes;
        if cap == 0 || tool_name == "read_file" || output.len() <= cap {
            return None;
        }
        fn safe_segment(raw: &str) -> String {
            raw.chars()
                .map(|ch| {
                    if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
                        ch
                    } else {
                        '_'
                    }
                })
                .take(48)
                .collect()
        }
        let dir = self
            .paths
            .state_dir
            .join("spill")
            .join(safe_segment(&self.state.session_id()));
        if std::fs::create_dir_all(&dir).is_err() {
            return None;
        }
        let file = dir.join(format!(
            "{}-{}-{}.txt",
            safe_segment(turn_id),
            safe_segment(call_id),
            safe_segment(tool_name)
        ));
        let replacement = spill_replacement(output, cap, &file.display().to_string())?;
        if let Err(error) = std::fs::write(&file, output) {
            tracing::warn!(%error, path = %file.display(), "tool output spill failed; keeping inline");
            return None;
        }
        tracing::info!(
            tool = tool_name,
            bytes = output.len(),
            path = %file.display(),
            "oversized tool output spilled"
        );
        Some(replacement)
    }

    pub fn usage_source(&self) -> &str {
        self.platform_context
            .as_ref()
            .map(|context| context.conversation.platform.as_str())
            .unwrap_or("agent")
    }

    pub fn prepare_for_turn(&mut self) -> Result<()> {
        let effective_system_prompt =
            mode_system_prompt(&self.config, &self.paths, self.mode, self.prompt_audience)?;
        {
            let fingerprint_prompt = match self.mode {
                AgentMode::Dev => effective_system_prompt.clone(),
                AgentMode::Normal => self.config.base_system_prompt(&self.paths)?,
            };
            let compatible_previous = matches!(self.prompt_audience, PromptAudience::Owner)
                .then_some(effective_system_prompt.as_str());
            self.state.reset_if_prompt_changed_with_compatible(
                &fingerprint_prompt,
                compatible_previous,
            )?;
            self.state.recover_stale_turns()?;
            self.maybe_cold_resume_prune()?;
        }
        self.system_prompt = with_host_environment(
            with_runtime_system_context(
                with_mode_reminder(effective_system_prompt, self.mode),
                &self.runtime_system_context,
            ),
            self.prompt_audience,
            &self.paths,
            self.mode,
        );
        Ok(())
    }

    pub fn set_runtime_system_context(&mut self, context: Vec<String>) -> Result<()> {
        self.runtime_system_context = context
            .into_iter()
            .map(|item| item.trim().to_string())
            .filter(|item| !item.is_empty())
            .collect();
        self.refresh_system_prompt()
    }

    /// Per-message transport blocks that ride the turn tail (after the user
    /// message) instead of the system prompt. No prompt refresh needed: they
    /// are consumed at message-assembly time.
    /// Raw input for the memory diary; `None` falls back to the turn content.
    pub fn set_memory_content(&mut self, content: Option<String>) {
        self.memory_content = content.filter(|text| !text.trim().is_empty());
    }

    pub fn set_turn_system_context(&mut self, context: Vec<String>) {
        self.turn_system_context = context
            .into_iter()
            .map(|item| item.trim().to_string())
            .filter(|item| !item.is_empty())
            .collect();
    }

    pub(crate) fn set_memory_writes_enabled(&mut self, enabled: bool) {
        self.memory.set_writes_enabled(enabled);
    }

    pub(crate) fn set_memory_organizer(&mut self, organizer: MemoryOrganizerHandle) {
        self.memory_organizer = Some(organizer);
    }

    pub(crate) fn set_memory_origin(&mut self, origin: MemoryOrigin) {
        self.memory_origin = origin;
    }

    pub(crate) fn set_memory_request_context(
        &mut self,
        access: MemoryAccess,
        writer_principal: Option<String>,
        writer_display_name: impl Into<String>,
    ) {
        self.memory
            .set_request_context(access, writer_principal, writer_display_name);
    }

    pub(crate) fn set_image_platform(&mut self, platform: &str, display_name: &str) {
        let platform = platform
            .chars()
            .filter(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
            .collect::<String>();
        self.image_platform = (!platform.is_empty()).then_some(platform);
        self.image_platform_label = self.image_platform.as_ref().and_then(|_| {
            (!display_name.trim().is_empty()).then(|| display_name.trim().to_string())
        });
    }

    pub(crate) fn set_platform_context_images(
        &mut self,
        context: Arc<PlatformTurnContext>,
        images: Vec<PlatformContextImageRef>,
    ) {
        self.platform_context = Some(context);
        self.context_images = images;
    }

    pub fn set_turn_persistence(
        &mut self,
        display_content: String,
        attachment_run_id: Option<String>,
    ) {
        self.turn_display_content = Some(display_content);
        self.attachment_run_id = attachment_run_id;
    }

    pub fn set_session_history_suppressed(&mut self, suppressed: bool) {
        self.suppress_session_history = suppressed;
    }

    /// Runs a batch's `task` tool calls concurrently, in waves bounded by
    /// `tools.subagent_concurrency`. Subagents are independent by design, so
    /// fanning them out preserves semantics while collapsing wall-clock time.
    /// Batches with fewer than two task calls — or a not-yet-loaded task tool
    /// (hybrid lazy loading) — return an empty map and take the serial path.
    pub async fn execute_parallel_task_calls<F>(
        &self,
        calls: &[crate::llm::ToolCall],
        loaded_tools: &std::collections::BTreeSet<String>,
        on_event: &mut F,
    ) -> Result<std::collections::HashMap<usize, GroupTaskOutput>>
    where
        F: FnMut(AgentEvent) -> Result<()>,
    {
        let mut outputs = std::collections::HashMap::new();
        let eligible: Vec<usize> = calls
            .iter()
            .enumerate()
            .filter(|(_, call)| call.function.name == "task")
            .map(|(index, _)| index)
            .collect();
        if eligible.len() < 2 {
            return Ok(outputs);
        }
        {
            let tools = self.tools.lock().unwrap();
            if tools::is_hybrid_loading_mode(&self.config.tools.loading_mode)
                && tools.requires_lazy_load("task", loaded_tools)
            {
                return Ok(outputs);
            }
        }

        struct Slot {
            call_index: usize,
            call_id: String,
            event_name: String,
            future: Option<tools::ToolFuture>,
            progress: mpsc::UnboundedReceiver<tools::ToolProgressEvent>,
        }
        enum WaveEvent {
            Done(usize, Result<String>),
            Progress(usize, tools::ToolProgressEvent),
            Spinner,
        }

        let limit = self.config.tools.subagent_concurrency.max(1);
        for wave in eligible.chunks(limit) {
            let mut slots: Vec<Slot> = Vec::new();
            {
                let tools = self.tools.lock().unwrap();
                for &call_index in wave {
                    let call = &calls[call_index];
                    let event_name = tool_event_name(&call.function.name, &call.function.arguments);
                    on_event(AgentEvent::ToolCall {
                        call_id: call.id.clone(),
                        name: event_name.clone(),
                        arguments: call.function.arguments.clone(),
                    })?;
                    let (progress_tx, progress_rx) = mpsc::unbounded_channel();
                    match tools.call_with_progress_future(
                        &call.function.name,
                        &call.function.arguments,
                        progress_tx,
                        &crate::tools::GuardCtx::default(),
                    ) {
                        Ok(future) => slots.push(Slot {
                            call_index,
                            call_id: call.id.clone(),
                            event_name,
                            future: Some(future),
                            progress: progress_rx,
                        }),
                        Err(err) => {
                            let output = format!("tool error: {err}");
                            on_event(AgentEvent::ToolResult {
                                call_id: call.id.clone(),
                                name: event_name,
                                ok: false,
                                output: output.clone(),
                            })?;
                            outputs.insert(
                                call_index,
                                GroupTaskOutput {
                                    output,
                                    report: None,
                                },
                            );
                        }
                    }
                }
            }
            let mut remaining = slots.iter().filter(|slot| slot.future.is_some()).count();
            let mut spinner_interval = tokio::time::interval(SPINNER_INTERVAL);
            spinner_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            spinner_interval.tick().await;
            while remaining > 0 {
                let event = {
                    let poll_slots = std::future::poll_fn(|context| {
                        for (position, slot) in slots.iter_mut().enumerate() {
                            if let std::task::Poll::Ready(Some(progress)) =
                                slot.progress.poll_recv(context)
                            {
                                return std::task::Poll::Ready(WaveEvent::Progress(
                                    position, progress,
                                ));
                            }
                            if let Some(future) = slot.future.as_mut() {
                                if let std::task::Poll::Ready(result) =
                                    future.as_mut().poll(context)
                                {
                                    slot.future = None;
                                    return std::task::Poll::Ready(WaveEvent::Done(
                                        position, result,
                                    ));
                                }
                            }
                        }
                        std::task::Poll::Pending
                    });
                    tokio::select! {
                        event = poll_slots => event,
                        _ = spinner_interval.tick() => WaveEvent::Spinner,
                    }
                };
                match event {
                    WaveEvent::Spinner => on_event(AgentEvent::SpinnerTick)?,
                    WaveEvent::Progress(position, progress) => {
                        emit_tool_progress(
                            on_event,
                            &slots[position].call_id,
                            &slots[position].event_name,
                            progress,
                        )?;
                    }
                    WaveEvent::Done(position, result) => {
                        remaining -= 1;
                        while let Ok(progress) = slots[position].progress.try_recv() {
                            emit_tool_progress(
                                on_event,
                                &slots[position].call_id,
                                &slots[position].event_name,
                                progress,
                            )?;
                        }
                        let call_index = slots[position].call_index;
                        let call_id = slots[position].call_id.clone();
                        let event_name = slots[position].event_name.clone();
                        match result {
                            Ok(output) => {
                                on_event(AgentEvent::ToolResult {
                                    call_id,
                                    name: event_name,
                                    ok: true,
                                    output: output.clone(),
                                })?;
                                let report = extract_persistable_tool_report("task", &output);
                                outputs.insert(call_index, GroupTaskOutput { output, report });
                            }
                            Err(err) => {
                                let output = format!("tool error: {err}");
                                on_event(AgentEvent::ToolResult {
                                    call_id,
                                    name: event_name,
                                    ok: false,
                                    output: output.clone(),
                                })?;
                                outputs.insert(
                                    call_index,
                                    GroupTaskOutput {
                                        output,
                                        report: None,
                                    },
                                );
                            }
                        }
                    }
                }
            }
        }
        Ok(outputs)
    }

    /// Rebuilds the system prompt for the current mode without running
    /// turn-entry maintenance. Used for mid-turn mode switches, where
    /// `reset_if_prompt_changed` must never fire (it would wipe the very
    /// turn that is running).
    pub fn refresh_system_prompt(&mut self) -> Result<()> {
        let base_system_prompt =
            mode_system_prompt(&self.config, &self.paths, self.mode, self.prompt_audience)?;
        self.system_prompt = with_host_environment(
            with_runtime_system_context(
                with_mode_reminder(base_system_prompt, self.mode),
                &self.runtime_system_context,
            ),
            self.prompt_audience,
            &self.paths,
            self.mode,
        );
        Ok(())
    }

    pub fn mode(&self) -> AgentMode {
        self.mode
    }

    pub fn context_window(&self) -> Option<usize> {
        self.client.context_window(&self.config).ok().flatten()
    }

    pub fn effective_context_tokens(&self) -> Result<u64> {
        let (messages, _) = self.chat_messages("", "")?;
        let mut tokens = overflow::estimate_messages_tokens(&messages) as u64;
        if self.tools_enabled {
            let loaded_tools = self.initial_loaded_tools(&messages)?;
            tokens = tokens.saturating_add(self.tool_definition_tokens(&loaded_tools) as u64);
        }
        Ok(tokens)
    }

    /// Session-scoped lifetime token total (Σ in the footer): keeps growing
    /// across compactions, resets to zero with the session history. The old
    /// global usage.json figure lives on in /usage as the global overview.
    pub fn conversation_usage_tokens(&self) -> Result<u64> {
        self.state.session_cumulative_tokens()
    }

    /// Same Σ with the prompt and cache-read halves its cache rate needs.
    pub fn conversation_usage_token_totals(&self) -> Result<TurnTokens> {
        self.state.session_cumulative_token_totals()
    }

    pub fn tool_definition_tokens(&self, loaded_tools: &BTreeSet<String>) -> usize {
        let tools = self.tools.lock().unwrap();
        let definitions = if tools::is_stub_loading_mode(&self.config.tools.loading_mode) {
            tools.stub_definitions()
        } else if tools::is_hybrid_loading_mode(&self.config.tools.loading_mode) {
            tools.lazy_definitions(loaded_tools)
        } else {
            tools.definitions()
        };
        estimate_tool_definition_tokens(&definitions)
    }

    pub async fn consume_queued_prompts<F>(
        &mut self,
        current_turn_id: &str,
        messages: &mut Vec<ChatMessage>,
        queued: Vec<QueuedPrompt>,
        preceding_assistant: (Option<&str>, Option<&str>, Option<&str>, Option<&str>),
        checkpoint: TurnRedoCheckpointPayload,
        control: &AgentTurnControl,
        on_event: &mut F,
    ) -> Result<()>
    where
        F: FnMut(AgentEvent) -> Result<()>,
    {
        on_event(AgentEvent::FlushJournal)?;
        // 排队插话=人类语境变化,重复链重置。
        self.repeat_chain.reset();
        let mut prepared = Vec::with_capacity(queued.len());
        for prompt in queued {
            let images = self.queued_prompt_images(&prompt)?;
            let input = self.prepare_user_input(&prompt.content, &images).await?;
            prepared.push((prompt, input));
        }

        let mode = control.mode();
        if self.mode != mode {
            self.switch_mode(mode, control.tools(mode));
            self.refresh_system_prompt()?;
        }
        replace_request_mode_context(
            messages,
            &self.system_prompt,
            mode,
            self.platform_context.is_some(),
        );

        let consumed = prepared
            .iter()
            .map(|(prompt, input)| (prompt.prompt_id.clone(), input.content.clone()))
            .collect::<Vec<_>>();
        self.state.consume_queued_prompts_with_checkpoint(
            current_turn_id,
            &consumed,
            preceding_assistant
                .0
                .filter(|content| !content.trim().is_empty()),
            preceding_assistant
                .1
                .filter(|reasoning| !reasoning.trim().is_empty()),
            preceding_assistant
                .2
                .filter(|provider_id| !provider_id.trim().is_empty()),
            preceding_assistant
                .3
                .filter(|model| !model.trim().is_empty()),
            checkpoint,
        )?;
        on_event(AgentEvent::QueuedPromptsConsumed {
            prompt_ids: consumed.iter().map(|(id, _)| id.clone()).collect(),
            mode,
            provider_id: preceding_assistant.2.map(str::to_string),
            model: preceding_assistant.3.map(str::to_string),
        })?;

        for (_, input) in prepared {
            messages.push(input.message);
            messages.extend(input.hints);
        }
        Ok(())
    }

    pub fn trim_visible_context(&self) -> Result<Vec<crate::state::StoredConversationEntry>> {
        let Some(context_window) = self.context_window() else {
            return Ok(Vec::new());
        };
        let track_loaded_tool_sources = self.tools_enabled
            && self.config.tools.persist_loaded_tools
            && tools::is_hybrid_loading_mode(&self.config.tools.loading_mode);
        if track_loaded_tool_sources {
            self.effective_context_tokens()?;
        }
        let mut loaded_tool_sources = if track_loaded_tool_sources {
            Some(self.state.load_session_loaded_tools_with_sources()?)
        } else {
            None
        };
        let expected_loaded_tools = loaded_tool_sources.clone();
        let mut total = usize::try_from(self.effective_context_tokens()?).unwrap_or(usize::MAX);
        let trigger = (context_window as f32 * self.trim_at_ratio).max(1.0) as usize;
        if total < trigger {
            return Ok(Vec::new());
        }

        let target = (context_window as f32 * (1.0 - self.trim_batch_ratio)).max(1.0) as usize;
        let turns = self.state.load_visible_turns()?;
        let mut loaded_tool_tokens = loaded_tool_sources
            .as_ref()
            .map(|items| {
                self.tool_definition_tokens(
                    &items
                        .iter()
                        .map(|(name, _)| name.clone())
                        .collect::<BTreeSet<_>>(),
                )
            })
            .unwrap_or(0);
        let mut count = 0usize;
        for turn in turns
            .iter()
            .filter(|turn| !turn.is_summary && turn.status != crate::state::TurnStatus::Running)
        {
            if total <= target {
                break;
            }
            let turn_tokens = if turn.status == crate::state::TurnStatus::Interrupted
                && !turn.journal_events.is_empty()
            {
                let mut replay = vec![self.turn_user_message(turn)];
                replay.extend(interrupted_turn_replay_messages(self, turn));
                overflow::estimate_messages_tokens(&replay)
            } else {
                turn_context_tokens(turn)
            };
            total = total.saturating_sub(turn_tokens);
            if let Some(items) = loaded_tool_sources.as_mut() {
                items.retain(|(_, source)| source.as_deref() != Some(turn.turn_id.as_str()));
                let remaining = items
                    .iter()
                    .map(|(name, _)| name.clone())
                    .collect::<BTreeSet<_>>();
                let remaining_tokens = self.tool_definition_tokens(&remaining);
                if remaining_tokens <= loaded_tool_tokens {
                    total = total.saturating_sub(loaded_tool_tokens - remaining_tokens);
                } else {
                    total = total.saturating_add(remaining_tokens - loaded_tool_tokens);
                }
                loaded_tool_tokens = remaining_tokens;
            }
            count += 1;
        }
        let turns = self.state.oldest_evictable_visible_turns(count)?;
        archive_and_delete_visible_turns_checked(
            &self.state,
            &self.memory,
            &turns,
            expected_loaded_tools.as_deref(),
        )
    }

    pub fn switch_mode(&mut self, mode: AgentMode, tools: ToolRegistry) {
        self.mode = mode;
        self.tools = Arc::new(Mutex::new(tools));
    }

    pub fn replace_client(&mut self, client: OpenAiCompatibleClient) {
        self.client = client;
    }

    pub(crate) fn cloned_client(&self) -> OpenAiCompatibleClient {
        self.client.clone()
    }

    pub fn reload_config(
        &mut self,
        config: AppConfig,
        client: OpenAiCompatibleClient,
    ) -> Result<()> {
        self.config = config;
        self.client = client;
        self.tools_enabled = self.config.tools.enabled;
        self.max_tool_rounds = self.config.tools.max_rounds;
        self.trim_at_ratio = self.config.context.trim_at_ratio;
        self.trim_batch_ratio = self.config.context.trim_batch_ratio;
        self.on_overflow = self.config.context.on_overflow.clone();
        let (access, writer_principal, writer_display_name) = self.memory.request_context();
        self.memory = MemoryStore::new(&self.config, &self.paths).with_request_context(
            access,
            writer_principal,
            writer_display_name,
        );
        self.memory.init()?;
        (self.memory_database_id, self.memory_generation) = self.memory.identity()?;
        self.prepare_for_turn()
    }

    /// /reset-memory:清空本模式人格的长期记忆(会话历史/技能不动),
    /// 然后重建句柄。dev 作用域由构造期的 dev_scoped 配置自动继承。
    pub fn wipe_memory(&mut self) -> Result<()> {
        self.memory.reset_all(false)?;
        self.reset_memory()
    }

    pub fn reset_memory(&mut self) -> Result<()> {
        let (access, writer_principal, writer_display_name) = self.memory.request_context();
        self.memory = MemoryStore::new(&self.config, &self.paths).with_request_context(
            access,
            writer_principal,
            writer_display_name,
        );
        self.memory.init()?;
        (self.memory_database_id, self.memory_generation) = self.memory.identity()?;
        Ok(())
    }

    pub async fn chat_stream<F>(&mut self, input: &str, on_event: F) -> Result<ChatResult>
    where
        F: FnMut(AgentEvent) -> Result<()>,
    {
        self.chat_stream_with_images(input, &[], on_event).await
    }
}
