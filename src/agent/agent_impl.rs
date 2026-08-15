//! agent_impl — 自 src/agent/mod.rs 拆分。

use super::*;

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

    pub async fn chat_stream_with_images<F>(
        &mut self,
        input: &str,
        images: &[Option<PastedImage>],
        on_event: F,
    ) -> Result<ChatResult>
    where
        F: FnMut(AgentEvent) -> Result<()>,
    {
        self.chat_stream_with_images_inner(input, images, None, on_event)
            .await
    }

    pub async fn chat_stream_with_control<F>(
        &mut self,
        input: &str,
        images: &[Option<PastedImage>],
        control: &AgentTurnControl,
        on_event: F,
    ) -> Result<ChatResult>
    where
        F: FnMut(AgentEvent) -> Result<()>,
    {
        self.chat_stream_with_images_inner(input, images, Some(control), on_event)
            .await
    }

    pub async fn redo_stream_with_control<F>(
        &mut self,
        candidate: &RedoCandidate,
        prompts: Vec<RedoPromptInput>,
        control: &AgentTurnControl,
        on_event: F,
    ) -> Result<ChatResult>
    where
        F: FnMut(AgentEvent) -> Result<()>,
    {
        let session = self.state.session_id();
        crate::tools::workspace::with_session(
            session,
            self.redo_stream_turn(candidate, prompts, control, on_event),
        )
        .await
    }

    pub async fn redo_stream_turn<F>(
        &mut self,
        candidate: &RedoCandidate,
        prompts: Vec<RedoPromptInput>,
        control: &AgentTurnControl,
        on_event: F,
    ) -> Result<ChatResult>
    where
        F: FnMut(AgentEvent) -> Result<()>,
    {
        self.cancel_cache_keepalive();
        self.state.recover_stale_turns()?;
        self.trim_visible_context()?;
        if prompts.is_empty()
            || prompts.last().map(|prompt| prompt.prompt_id.as_str())
                != Some(candidate.input_id.as_str())
        {
            bail!("redo prompts no longer match the selected input");
        }
        let current_turn = self
            .state
            .load_turns()?
            .into_iter()
            .find(|turn| turn.turn_id == candidate.turn_id)
            .context("redo turn no longer exists")?;

        let mut prepared = Vec::with_capacity(prompts.len());
        for prompt in prompts {
            let input = self
                .prepare_user_input(&prompt.content, &prompt.images)
                .await?;
            prepared.push((prompt, input));
        }
        let (last_prompt, last_input) = prepared.last().context("redo input is empty")?;
        let last_content = last_input.content.clone();
        let last_display_content = last_prompt.display_content.clone();
        let diary_input = last_content.clone();
        let redo = self.state.begin_redo(
            &candidate.turn_id,
            &candidate.input_id,
            candidate.input_kind,
            candidate.revision,
            &last_content,
            &last_display_content,
            std::process::id(),
        )?;
        let guard =
            PendingRedoGuard::new(self.state.clone(), candidate.turn_id.clone(), redo.revision);
        let mut on_event = on_event;
        on_event(AgentEvent::TurnStarted {
            turn_id: candidate.turn_id.clone(),
        })?;

        let (mut messages, redo_user_index) = self.chat_messages(&candidate.turn_id, "")?;
        // 按下标摘下 [占位用户, 瞬态尾巴...]:重放輸入接回后尾巴原样跟上,
        // 保持"瞬态永远在用户消息之后"。
        let tail_fossils = messages.split_off(redo_user_index + 1);
        let _ = messages.pop();
        let replay_start;
        let fossil_start;
        let base_tool_reports;
        let initial_tool_rounds;
        let initial_question_rounds;
        match candidate.input_kind {
            RedoInputKind::Initial => {
                let (_, input) = prepared.pop().context("redo input is empty")?;
                messages.push(input.message);
                fossil_start = messages.len();
                messages.extend(tail_fossils);
                replay_start = messages.len();
                messages.extend(input.hints);
                base_tool_reports = Vec::new();
                initial_tool_rounds = 0;
                initial_question_rounds = 0;
            }
            RedoInputKind::Followup => {
                let checkpoint = redo.checkpoint.context("redo checkpoint is unavailable")?;
                messages.push(self.turn_user_message(&current_turn));
                fossil_start = messages.len();
                messages.extend(tail_fossils);
                replay_start = messages.len();
                messages.extend(checkpoint.replay_messages);
                for (_, input) in prepared {
                    messages.push(input.message);
                    messages.extend(input.hints);
                }
                base_tool_reports = checkpoint.prefix_tool_reports;
                initial_tool_rounds = checkpoint.tool_rounds;
                initial_question_rounds = checkpoint.question_rounds;
            }
        }
        // Redo rewrites the turn, so refresh its fossilized tail to match what
        // this generation actually sends (new runtime stamp + replayed tail).
        self.state.set_turn_context_messages(
            &candidate.turn_id,
            &fossil_context_messages(&messages[fossil_start..]),
        )?;

        let mut used_tools = Vec::new();
        let mut persisted_tool_reports = Vec::new();
        let mut journal =
            TurnJournalSink::new(self.state.clone(), candidate.turn_id.clone(), redo.revision);
        let stream_result = {
            let mut journaled_event = |event| journal.emit(event, &mut on_event);
            self.chat_with_tools(
                &candidate.turn_id,
                &mut messages,
                &mut used_tools,
                &mut persisted_tool_reports,
                replay_start,
                &base_tool_reports,
                initial_tool_rounds,
                initial_question_rounds,
                Some(control),
                &mut journaled_event,
            )
            .await
        };
        journal.finish(&mut on_event)?;
        let result = stream_result?;
        let reports = persisted_tool_reports
            .into_iter()
            .map(|(_, report)| report)
            .collect::<Vec<_>>();
        self.state
            .append_persisted_contexts(&candidate.turn_id, &reports)?;
        let tokens = TurnTokens::from_usage(result.usage.as_ref());
        guard.complete_with_model(
            &result.content,
            result.reasoning.as_deref(),
            result.provider_id.as_deref(),
            result.model.as_deref(),
            tokens,
            result.usage_estimated,
        )?;
        if let (Some(provider), Some(model)) = (&result.provider_id, &result.model) {
            self.last_request_endpoint = Some((provider.clone(), model.clone()));
        }
        let tool_flow = derive_tool_flow(&messages, replay_start);
        if !tool_flow.is_empty() {
            self.state.set_turn_tool_flow(&candidate.turn_id, &tool_flow)?;
        }
        if self.memory.process_after_turn(
            &diary_input,
            &result.content,
            &self.memory_origin,
            &self.memory_database_id,
            self.memory_generation,
        )? {
            self.wake_memory_organizer();
        }
        if let Some(usage) = result.usage.clone() {
            let meta = crate::state::UsageMeta {
                source: self.usage_source(),
                provider: result.provider_id.as_deref(),
                model: result.model.as_deref(),
            };
            self.state.add_usage(&usage, meta)?;
        }
        self.start_cache_keepalive();
        Ok(result)
    }

    /// Publishes the turn's session as the ambient scope before running it.
    /// Subagents launched inside read it to hang their audit sessions off this
    /// one, and those audits now count toward the session's Σ — without the
    /// scope a subagent bills to nobody. The daemon actor sets the same scope;
    /// re-scoping to the same id is harmless, and the direct/local paths had
    /// no scope at all.
    pub async fn chat_stream_with_images_inner<F>(
        &mut self,
        input: &str,
        images: &[Option<PastedImage>],
        control: Option<&AgentTurnControl>,
        on_event: F,
    ) -> Result<ChatResult>
    where
        F: FnMut(AgentEvent) -> Result<()>,
    {
        let session = self.state.session_id();
        crate::tools::workspace::with_session(
            session,
            self.chat_stream_turn(input, images, control, on_event),
        )
        .await
    }

    /// 浮动尾部人格提醒,所有会话形态(终端/WebUI/平台)一致生效:命中
    /// 缓存时只是一次小文件读,缓存未建时对同一 client 蒸馏一次(每份
    /// 人格内容一生只发生一次)。蒸馏失败降级为无提醒,绝不阻断回合。
    pub async fn resolve_persona_reminder(&self) -> Option<String> {
        // Dev 无人格,自然无防失忆提醒。
        if self.mode == AgentMode::Dev {
            return None;
        }
        if !self.config.prompt.persona_reminder {
            return None;
        }
        match persona_hint::resolve(&self.config, &self.paths, &self.client).await {
            Ok(reminder) => reminder,
            Err(error) => {
                tracing::warn!(error = %error, "persona reminder distillation failed");
                None
            }
        }
    }

    pub async fn chat_stream_turn<F>(
        &mut self,
        input: &str,
        images: &[Option<PastedImage>],
        control: Option<&AgentTurnControl>,
        on_event: F,
    ) -> Result<ChatResult>
    where
        F: FnMut(AgentEvent) -> Result<()>,
    {
        // A new turn is about to mutate the context; stop pinging the stale
        // prefix (the turn's own requests refresh the cache anyway).
        self.cancel_cache_keepalive();
        self.state.recover_stale_turns()?;
        self.trim_visible_context()?;
        self.persona_reminder = self.resolve_persona_reminder().await;
        // 人类新回合:重复链语境重置。goal 自动续轮/job 唤醒不算语境
        // 变化——跨自动轮的原样重复正是最需要打断的死循环(dsh 同款:
        // 只有 user 来源消息重置链)。
        if matches!(
            crate::tools::workspace::current_turn_origin(),
            crate::tools::workspace::TurnOrigin::Human
        ) {
            self.repeat_chain.reset();
        }
        let prepared = self.prepare_user_input(input, images).await?;
        let input = prepared.content.clone();
        let turn_id = format!(
            "turn_{}_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0),
            rand::random::<u16>()
        );
        let display_content = self
            .turn_display_content
            .take()
            .unwrap_or_else(|| input.clone());
        let attachment_run_id = self.attachment_run_id.take();
        self.state.start_turn_with_display(
            &turn_id,
            &input,
            &display_content,
            std::process::id(),
            attachment_run_id.as_deref(),
        )?;
        let guard = PendingTurnGuard::new(self.state.clone(), turn_id.clone());
        let mut on_event = on_event;
        on_event(AgentEvent::TurnStarted {
            turn_id: turn_id.clone(),
        })?;
        let (mut messages, user_index) = self.chat_messages(&turn_id, &input)?;
        // 按显式下标把占位用户消息换成带附件的成品;瞬态尾巴保持原位。
        if let Some(user) = messages.get_mut(user_index) {
            *user = prepared.message;
        }
        let replay_start = messages.len();
        if !self.turn_system_context.is_empty() {
            // Trusted transport/control tail (v7 §三): host-derived per-message
            // context lands after the user message, before untrusted blocks.
            // Standing advisories (the `[SystemInfo:` class, e.g. long-reply
            // conversion records) repeat identical text turn after turn; when
            // the exact bytes are already visible in a replayed fossil the
            // repeat adds nothing and is skipped — the associative-memory
            // dedup reasoning. Everything else ("this turn is system
            // triggered", identity warnings, moderation prechecks) refers to
            // the CURRENT turn, so an identical old fossil is no substitute
            // and those blocks are always sent.
            let fresh = self
                .turn_system_context
                .iter()
                .filter(|block| {
                    !(block.starts_with(STANDING_ADVISORY_PREFIX)
                        && turn_context_block_visible(&messages, block))
                })
                .cloned()
                .collect::<Vec<_>>();
            if !fresh.is_empty() {
                messages.push(ChatMessage::turn_context(fresh.join("\n\n")));
            }
        }
        messages.extend(prepared.hints);
        // 记忆联想不再按模式关断:dev 的 MemoryStore 指向保留人格 "dev"
        // 的独立库(构造时作用域化),联想/落库都发生在自己的命名空间里。
        let association_exclusion = self
            .state
            .oldest_visible_turn_timestamp(&turn_id)?
            .map(|since| crate::memory::AssociationExclusion {
                session_id: self.state.session_id().to_string(),
                since,
            });
        if let Some(mut association) = self
            .memory
            .association(&input, association_exclusion.as_ref())?
        {
            if association.organization_due {
                self.wake_memory_organizer();
            }
            if self.memory.association_dedup_enabled() {
                // Cross-turn dedup: fossils replay earlier associative
                // blocks byte-for-byte, so a line already visible in this
                // request adds nothing but tokens. Filtering only shrinks
                // the block being built this turn; once a carrying turn is
                // hidden by compact or trim, its lines leave the request
                // and the memory becomes eligible for injection again.
                let seen = visible_association_lines(&messages);
                self.memory
                    .retain_unseen_association(&mut association, &seen);
            }
            if !association.facts.is_empty() || !association.episodes.is_empty() {
                // v7 Phase 1.1: the associative-memory block rides the turn
                // tail instead of `insert(1)`, so the stable history prefix
                // in front stays byte-identical for provider prefix caches.
                // It lands after `replay_start`, so redo checkpoints freeze
                // the recalled snapshot (decision 6).
                messages.push(ChatMessage::turn_context(
                    self.memory.format_association(&association),
                ));
            }
        }
        // dev 目录里没有表情包工具,提醒只会指向不存在的工具——不发。
        if self.mode != AgentMode::Dev {
            if let Some(reminder) =
                memes::auto_meme_reminder(&self.config, &input, self.platform_context.is_some())
            {
                messages.push(ChatMessage::turn_context(reminder));
            }
        }
        // v7 append-only fossilization ("注入了就别删"): archive the transient
        // system tail exactly as sent — runtime stamp, trusted transport
        // context, hints, associative memory, meme reminder — so future
        // history replay is a byte-exact extension of this request and the
        // provider prefix cache never sees a divergence at this turn.
        self.state.set_turn_context_messages(
            &turn_id,
            &fossil_context_messages(&messages[user_index + 1..]),
        )?;
        let mut used_tools = Vec::new();
        let mut persisted_tool_reports = Vec::new();
        let mut journal = TurnJournalSink::new(self.state.clone(), turn_id.clone(), 0);
        let stream_result = {
            let mut journaled_event = |event| journal.emit(event, &mut on_event);
            self.chat_with_tools(
                &turn_id,
                &mut messages,
                &mut used_tools,
                &mut persisted_tool_reports,
                replay_start,
                &[],
                0,
                0,
                control,
                &mut journaled_event,
            )
            .await
        };
        journal.finish(&mut on_event)?;
        let result = stream_result?;
        let reports = persisted_tool_reports
            .into_iter()
            .map(|(_, report)| report)
            .collect::<Vec<_>>();
        self.state.append_persisted_contexts(&turn_id, &reports)?;
        let tokens = TurnTokens::from_usage(result.usage.as_ref());
        guard.complete_with_model(
            &result.content,
            result.reasoning.as_deref(),
            result.provider_id.as_deref(),
            result.model.as_deref(),
            tokens,
            result.usage_estimated,
        )?;
        if let (Some(provider), Some(model)) = (&result.provider_id, &result.model) {
            self.last_request_endpoint = Some((provider.clone(), model.clone()));
        }
        let tool_flow = derive_tool_flow(&messages, replay_start);
        if !tool_flow.is_empty() {
            self.state.set_turn_tool_flow(&turn_id, &tool_flow)?;
        }
        if self.memory.process_after_turn(
            // C10 三份内容分离(最小实现):日记读平台包装前的原文快照,
            // 而不是带指令样板和群聊记录块的完整 prompt 内容。
            self.memory_content.as_deref().unwrap_or(&input),
            &result.content,
            &self.memory_origin,
            &self.memory_database_id,
            self.memory_generation,
        )? {
            self.wake_memory_organizer();
        }
        if let Some(usage) = result.usage.clone() {
            let meta = crate::state::UsageMeta {
                source: self.usage_source(),
                provider: result.provider_id.as_deref(),
                model: result.model.as_deref(),
            };
            self.state.add_usage(&usage, meta)?;
        }
        self.start_cache_keepalive();
        Ok(result)
    }

    pub fn wake_memory_organizer(&self) {
        if let Some(organizer) = &self.memory_organizer {
            organizer.wake(self.config.clone(), self.paths.clone(), self.state.clone());
        }
    }

    pub async fn prepare_user_input(
        &self,
        input: &str,
        images: &[Option<PastedImage>],
    ) -> Result<PreparedUserInput> {
        let input = clean_user_visible_text(input);
        let binary_images = images
            .iter()
            .filter_map(|image| match image {
                Some(PastedImage::Binary(image)) => Some(image),
                _ => None,
            })
            .collect::<Vec<_>>();
        let path_images = images
            .iter()
            .filter_map(|image| match image {
                Some(PastedImage::Path(path)) => Some(path.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        let absolute_image_paths =
            resolve_pasted_image_paths(images, &self.paths, self.image_platform.as_deref());
        let binary_paths = images
            .iter()
            .zip(&absolute_image_paths)
            .filter_map(|(image, path)| {
                matches!(image, Some(PastedImage::Binary(_)))
                    .then(|| path.clone())
                    .flatten()
            })
            .collect::<Vec<_>>();
        // v7 Phase 1.3-b: register the scoped vision tool whenever the platform
        // path is active, even with no images this turn. A conditional
        // registration made the tools array appear/disappear between turns,
        // invalidating the provider prefix cache from token 0; an empty scope
        // simply rejects analysis requests with a clear message instead.
        if self.tools_enabled
            && self.config.plugins.vision.enabled
            && self.image_platform.is_some()
        {
            let mut tools = self.tools.lock().unwrap();
            if let Some(platform_context) = self.platform_context.clone() {
                vision::register_scoped_platform(
                    &mut tools,
                    self.config.clone(),
                    self.paths.clone(),
                    binary_paths.iter().map(PathBuf::from).collect(),
                    self.context_images.clone(),
                    platform_context,
                );
            } else if !tools.contains("vision_analyze") {
                vision::register_scoped_local(
                    &mut tools,
                    self.config.clone(),
                    self.paths.clone(),
                    binary_paths.iter().map(PathBuf::from).collect(),
                );
            }
        }
        let vision_tool_available =
            self.tools_enabled && self.tools.lock().unwrap().contains("vision_analyze");
        let input = rewrite_image_placeholders_with_paths(&input, &absolute_image_paths);
        let current_model_supports_vision = self.current_model_supports_vision();
        let content = if !binary_images.is_empty() && !current_model_supports_vision {
            self.describe_images_with_vision_provider(&input, &binary_images)
                .await?
        } else {
            input
        };

        let message = if !binary_images.is_empty() && current_model_supports_vision {
            let mut parts = vec![ChatContentPart::Text {
                text: content.clone(),
            }];
            parts.extend(binary_images.iter().map(|image| ChatContentPart::ImageUrl {
                image_url: ImageUrlContent {
                    url: image.data_url().to_string(),
                },
            }));
            ChatMessage::user_parts(parts)
        } else {
            ChatMessage::plain("user", &content)
        };

        let mut hints = Vec::new();
        if !binary_paths.is_empty() {
            let source = self
                .image_platform_label
                .as_deref()
                .or(self.image_platform.as_deref())
                .map(|platform| format!("通过 {platform} 发送"))
                .unwrap_or_else(|| "粘贴".to_string());
            let tool_hint = if vision_tool_available {
                "\n你可以使用 vision_analyze 工具对此图片进行更详细的分析。"
            } else {
                ""
            };
            let hint = if binary_paths.len() == 1 {
                format!(
                    "用户{source}了 1 张图片，已保存到临时文件：{}{}",
                    binary_paths[0], tool_hint
                )
            } else {
                let list = binary_paths
                    .iter()
                    .enumerate()
                    .map(|(index, path)| format!("  [Image {}] {}", index + 1, path))
                    .collect::<Vec<_>>()
                    .join("\n");
                format!(
                    "用户{source}了 {} 张图片，已保存到临时文件：\n{}{}",
                    binary_paths.len(),
                    list,
                    if vision_tool_available {
                        "\n你可以使用 vision_analyze 工具对这些图片进行更详细的分析。"
                    } else {
                        ""
                    }
                )
            };
            hints.push(ChatMessage::turn_context(hint));
        }
        if !path_images.is_empty() && vision_tool_available {
            let list = path_images
                .iter()
                .enumerate()
                .map(|(index, path)| format!("  [Image {}] {}", index + 1, path))
                .collect::<Vec<_>>()
                .join("\n");
            hints.push(ChatMessage::turn_context(format!(
                "用户粘贴了 {} 张本地图片路径：\n{}\n你可以使用 vision_analyze 工具读取并分析这些图片。",
                path_images.len(),
                list
            )));
        }
        if !self.context_images.is_empty() && vision_tool_available {
            let ids = self
                .context_images
                .iter()
                .map(|image| image.id.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            hints.push(ChatMessage::turn_context(format!(
                "此前群聊记录中有可按需查看的历史图片：{ids}。你尚未看到这些图片的实际内容；只有回答确实依赖图片时，才使用 vision_analyze，并把对应 ID 作为 image 参数。不得根据图片占位符猜测内容。"
            )));
        }

        Ok(PreparedUserInput {
            content,
            message,
            hints,
        })
    }

    pub async fn handle_overflow_after_turn<F>(
        &self,
        context_tokens: u64,
        on_event: F,
    ) -> Result<Option<ChatResult>>
    where
        F: FnMut(AgentEvent) -> Result<()>,
    {
        let mut on_event = on_event;
        let Some(compact) = self.handle_overflow(context_tokens, &mut on_event).await? else {
            return Ok(None);
        };
        self.state.add_auxiliary_usage(
            &compact.usage,
            crate::state::UsageMeta {
                source: self.usage_source(),
                provider: compact.provider_id.as_deref(),
                model: None,
            },
        )?;
        Ok(Some(ChatResult {
            content: String::new(),
            reasoning: None,
            usage: Some(compact.usage),
            usage_estimated: compact.usage_estimated,
            tool_calls: Vec::new(),
            provider_id: None,
            model: None,
            finish_reason: None,
            thinking_signature: None,
            last_request_usage: None,
            responses_continuation: None,
        }))
    }

    pub async fn compact_now<F>(&self, on_event: F) -> Result<Option<ChatResult>>
    where
        F: FnMut(AgentEvent) -> Result<()>,
    {
        let mut on_event = on_event;
        let context_window = self.context_window().or_else(|| {
            if crate::models_cache::is_loaded() {
                return None;
            }
            crate::models_cache::refresh_blocking(&self.paths).ok()?;
            self.context_window()
        });
        let Some(context_window) = context_window else {
            let missing = self.client.models_without_context_window(&self.config);
            if missing.is_empty() {
                bail!(
                    "{}",
                    crate::i18n::text(
                        "The current model's context window is not loaded or configured, so the context cannot be compacted",
                        "当前模型的上下文窗口尚未加载或未配置，无法压缩上下文"
                    )
                );
            }
            bail!(
                "{}{}",
                crate::i18n::text(
                    "The context windows for these active models are not loaded or configured, so the context cannot be compacted: ",
                    "以下活动模型的上下文窗口尚未加载或未配置，无法压缩上下文："
                ),
                missing.join(", ")
            );
        };
        let visible_count = self.state.load_visible_turns()?.len();
        if visible_count == 0 {
            return Ok(None);
        }
        let check = overflow::OverflowCheck::new(Some(context_window), self.trim_at_ratio, None);
        on_event(AgentEvent::CompactStart)?;
        let compactor = compact::Compactor::new(
            self.client.clone(),
            self.state.clone(),
            context_window,
            check.reserved_tokens,
            self.compact_tail_budget(context_window),
            self.preset_dialogs.len(),
        );
        let mut on_chunk = |chunk: ChatStreamChunk| on_event(AgentEvent::CompactChunk(chunk));
        let fork_builder = |fold_ids: &[String]| -> Result<compact::CompactForkParts> {
            Ok((
                self.compact_fork_prefix(fold_ids)?,
                self.live_tool_definitions()?,
            ))
        };
        let fork_builder: Option<compact::CompactForkBuilder<'_>> = self
            .config
            .context
            .compact_cache_reuse
            .then_some(&fork_builder);
        // Manual /compact is an explicit user request: bypass the
        // fold-economics gate (but tail retention still applies).
        let compact = match compactor
            .perform_compact(true, false, fork_builder, &mut on_chunk)
            .await
        {
            Ok(result) => {
                on_event(AgentEvent::CompactEnd)?;
                result
            }
            Err(err) => {
                on_event(AgentEvent::CompactEnd)?;
                return Err(err);
            }
        };
        let Some(compact) = compact else {
            return Ok(None);
        };
        self.state.add_auxiliary_usage(
            &compact.usage,
            crate::state::UsageMeta {
                source: self.usage_source(),
                provider: compact.provider_id.as_deref(),
                model: None,
            },
        )?;
        Ok(Some(ChatResult {
            content: String::new(),
            reasoning: None,
            usage: Some(compact.usage),
            usage_estimated: compact.usage_estimated,
            tool_calls: Vec::new(),
            provider_id: None,
            model: None,
            finish_reason: None,
            thinking_signature: None,
            last_request_usage: None,
            responses_continuation: None,
        }))
    }

    /// Cold-resume prune: after idling past the provider cache TTL the next
    /// request is a full-price cold start anyway, so a history rewrite right
    /// now is free cache-wise and only shrinks that first request. Uses a
    /// minimal harvest gate for the same reason.
    pub fn maybe_cold_resume_prune(&self) -> Result<()> {
        if !self.config.context.prune_stale_tool_reports {
            return Ok(());
        }
        let minutes = self.config.context.cold_prune_after_minutes;
        if minutes == 0 {
            return Ok(());
        }
        let Some(last) = self.state.session_last_request_at()? else {
            return Ok(());
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        if now.saturating_sub(last) < (minutes as i64).saturating_mul(60) {
            return Ok(());
        }
        let stats = self.state.prune_stale_tool_reports(2, 1024)?;
        if stats.turns > 0 {
            tracing::info!(
                turns = stats.turns,
                saved_chars = stats.saved_chars,
                idle_minutes = now.saturating_sub(last) / 60,
                "context_rewrite reason=cold_resume_prune"
            );
        }
        Ok(())
    }

    /// Mechanical prune behind the harvest gate: rewriting history is a
    /// prefix-cache reset, so the batch must save at least ~window/64 tokens
    /// (~window/16 chars) to pay for it. Protects the newest 2 turns.
    pub fn prune_stale_history(&self, context_window: usize) -> Result<crate::state::PruneStats> {
        let min_saved_chars = (context_window / 16).max(8192);
        let stats = self.state.prune_stale_tool_reports(2, min_saved_chars)?;
        if stats.turns > 0 {
            tracing::info!(
                turns = stats.turns,
                saved_chars = stats.saved_chars,
                "context_rewrite reason=prune"
            );
        }
        Ok(stats)
    }

    /// Derives the verbatim tail budget for compaction. Fixed token count by
    /// design (the trigger scales with the window, the tail does not — that
    /// geometry is what stops the re-compaction loop); chat sessions default
    /// smaller because casual history has less verbatim value.
    pub fn compact_tail_budget(&self, context_window: usize) -> usize {
        self.config.context.compact_tail_tokens.unwrap_or(16384.min(context_window / 4))
    }

    pub async fn handle_overflow<F>(
        &self,
        context_tokens: u64,
        on_event: &mut F,
    ) -> Result<Option<compact::CompactResult>>
    where
        F: FnMut(AgentEvent) -> Result<()>,
    {
        use std::sync::atomic::Ordering;
        let context_window = self.context_window();
        let check = overflow::OverflowCheck::new(context_window, self.trim_at_ratio, None);
        let context_tokens = usize::try_from(context_tokens).unwrap_or(usize::MAX);
        if !check.is_enabled() {
            return Ok(None);
        }
        if !check.check_tokens(context_tokens) {
            // Breathing room below the trigger is what a healthy compaction
            // buys; clear the stuck latch and the run counters here, before
            // any other branch can return, so a compaction that settles the
            // context anywhere under the trigger fully re-arms
            // auto-compaction (a stale count would latch the next one off).
            self.consecutive_compacts.store(0, Ordering::Relaxed);
            self.rapid_compacts.store(0, Ordering::Relaxed);
            self.compact_stuck.store(false, Ordering::Relaxed);
            // Below-trigger watermarks: each tier does only the cheapest
            // thing that helps. snip prunes stale tool reports mechanically
            // (no LLM call); soft just says the context is growing, once.
            if let Some(window) = context_window {
                let snip_threshold = (window as f32
                    * self.config.context.compact_snip_ratio)
                    .max(1.0) as usize;
                let soft_threshold = (window as f32
                    * self.config.context.compact_soft_ratio)
                    .max(1.0) as usize;
                if context_tokens >= snip_threshold {
                    if self.config.context.prune_stale_tool_reports {
                        let stats = self.prune_stale_history(window)?;
                        if stats.turns > 0 {
                            on_event(AgentEvent::Notice {
                                text: format!(
                                    "{} {} · ~{} chars",
                                    crate::i18n::text(
                                        "Folded stale tool records from turns:",
                                        "已机械折叠旧轮次的工具记录："
                                    ),
                                    stats.turns,
                                    stats.saved_chars,
                                ),
                            })?;
                        }
                    }
                } else if context_tokens >= soft_threshold
                    && !self.soft_notice_sent.swap(true, Ordering::Relaxed)
                {
                    on_event(AgentEvent::Notice {
                        text: crate::i18n::text(
                            "Context is getting large; older tool records will fold first, then the history will be compacted automatically.",
                            "上下文渐大；将先机械折叠旧工具记录，随后才会自动压缩历史。",
                        )
                        .to_string(),
                    })?;
                }
            }
            return Ok(None);
        }
        if self.compact_stuck.load(Ordering::Relaxed) {
            return Ok(None);
        }
        let compact_result = match self.on_overflow.as_str() {
            "compact" => {
                let visible_count = self.state.load_visible_turns()?.len();
                if visible_count == 0 {
                    return Ok(None);
                }
                let window = context_window.unwrap();
                let force_threshold = (window as f32
                    * self.config.context.compact_force_ratio)
                    .max(1.0) as usize;
                let force = context_tokens >= force_threshold;
                // Prune first: it is free, and when it alone lands the
                // context back under the trigger the paid summary call (and
                // its cache reset) is skipped entirely.
                if self.config.context.prune_stale_tool_reports {
                    let stats = self.prune_stale_history(window)?;
                    if stats.turns > 0 && !force {
                        let post_tokens = usize::try_from(self.effective_context_tokens()?)
                            .unwrap_or(usize::MAX);
                        if !check.check_tokens(post_tokens) {
                            on_event(AgentEvent::Notice {
                                text: crate::i18n::text(
                                    "Folded stale tool records; context is back under the compaction threshold.",
                                    "已机械折叠旧工具记录；上下文已回落到压缩阈值之下。",
                                )
                                .to_string(),
                            })?;
                            return Ok(None);
                        }
                    }
                }
                on_event(AgentEvent::CompactStart)?;
                let compactor = compact::Compactor::new(
                    self.client.clone(),
                    self.state.clone(),
                    window,
                    check.reserved_tokens,
                    self.compact_tail_budget(window),
                    self.preset_dialogs.len(),
                );
                let mut on_chunk =
                    |chunk: ChatStreamChunk| on_event(AgentEvent::CompactChunk(chunk));
                let fork_builder = |fold_ids: &[String]| -> Result<compact::CompactForkParts> {
                    Ok((
                        self.compact_fork_prefix(fold_ids)?,
                        self.live_tool_definitions()?,
                    ))
                };
                let fork_builder: Option<compact::CompactForkBuilder<'_>> = self
                    .config
                    .context
                    .compact_cache_reuse
                    .then_some(&fork_builder);
                let result = match compactor
                    .perform_compact(force, true, fork_builder, &mut on_chunk)
                    .await
                {
                    Ok(result) => {
                        on_event(AgentEvent::CompactEnd)?;
                        result
                    }
                    Err(e) => {
                        on_event(AgentEvent::CompactEnd)?;
                        return Err(e);
                    }
                };
                if let Some(result) = result.as_ref() {
                    on_event(AgentEvent::Notice {
                        text: format!(
                            "{} {} → {} {}",
                            crate::i18n::text("Compacted: folded turns", "压缩完成：折叠轮次"),
                            result.folded_turns,
                            crate::i18n::text("kept verbatim", "逐字保留最近轮次"),
                            result.kept_turns,
                        ),
                    })?;
                }
                if result.is_some() {
                    // Post-compaction check: still over the trigger means the
                    // verbatim floor plus system prompt alone exceed it.
                    // Twice in a row would re-fire every turn (cratering the
                    // prefix cache each time), so latch auto-compaction off
                    // and say why, once.
                    let post_tokens =
                        usize::try_from(self.effective_context_tokens()?).unwrap_or(usize::MAX);
                    if check.check_tokens(post_tokens) {
                        let runs = self.consecutive_compacts.fetch_add(1, Ordering::Relaxed) + 1;
                        if runs >= 2 && !self.compact_stuck.swap(true, Ordering::Relaxed) {
                            on_event(AgentEvent::Notice {
                                text: crate::i18n::text(
                                    "Automatic context compaction paused: the context window is too small for compaction to help (the system prompt plus the verbatim tail already exceed the trigger). Raise context window or reduce tool output; compaction resumes once the context drops.",
                                    "自动上下文压缩已暂停：上下文窗口太小，压缩无法奏效（system prompt 加逐字尾巴已超过触发线）。请调大上下文窗口或减小工具输出；上下文回落后自动恢复。",
                                )
                                .to_string(),
                            })?;
                        }
                    } else {
                        self.consecutive_compacts.store(0, Ordering::Relaxed);
                    }
                    // Thrashing check: a healthy compaction buys many turns
                    // of breathing room. Refilling within ~3 turns, three
                    // times in a row, means a single oversized item refills
                    // the window and each compaction only craters the cache.
                    let max_seq = self
                        .state
                        .load_visible_turns()?
                        .last()
                        .map(|turn| turn.seq)
                        .unwrap_or(-1);
                    let previous = self.last_compact_max_seq.swap(max_seq, Ordering::Relaxed);
                    // Each turn advances seq by 1 and the compaction summary
                    // itself takes one, so "within 3 turns" is a delta <= 4.
                    if previous >= 0 && max_seq.saturating_sub(previous) <= 4 {
                        let rapid = self.rapid_compacts.fetch_add(1, Ordering::Relaxed) + 1;
                        if rapid >= 3 && !self.compact_stuck.swap(true, Ordering::Relaxed) {
                            on_event(AgentEvent::Notice {
                                text: crate::i18n::text(
                                    "Automatic context compaction paused: the context refills within a few turns of each compaction. A single message or tool output is likely too large for the window — read in smaller chunks, or /clear to start fresh.",
                                    "自动上下文压缩已暂停：每次压缩后几轮内上下文就再次填满。可能有单条消息或工具输出对窗口而言过大——请分块读取，或使用 /clear 重新开始。",
                                )
                                .to_string(),
                            })?;
                        }
                    } else {
                        self.rapid_compacts.store(0, Ordering::Relaxed);
                    }
                }
                result
            }
            "pop" => {
                on_event(AgentEvent::PopStart)?;
                self.trim_visible_context()?;
                on_event(AgentEvent::PopEnd)?;
                None
            }
            _ => None,
        };
        Ok(compact_result)
    }

    pub fn current_model_supports_vision(&self) -> bool {
        should_use_active_text_pool_for_images(&self.config)
    }

    pub async fn describe_images_with_vision_provider(
        &self,
        input: &str,
        images: &[&ClipboardImage],
    ) -> Result<String> {
        let vision_cfg = &self.config.plugins.vision;
        if !vision_cfg.enabled {
            bail!(
                "{}",
                crate::i18n::text(
                    "the active text model cannot read images and the vision plugin is disabled",
                    "当前文本模型无法读取图片，并且视觉插件已禁用"
                )
            );
        }
        let strict_pool = self
            .config
            .active_multimodal_provider_models
            .as_ref()
            .is_some_and(|pool| !pool.is_empty());
        let mut descriptions = Vec::new();
        for (i, img) in images.iter().enumerate() {
            let prompt = if input.trim().is_empty() {
                "请简洁描述这张图片，并指出重要细节。".to_string()
            } else {
                format!("用户消息：{input}\n\n请基于图片内容回答或描述图片，不要编造看不见的信息。")
            };
            match vision::analyze_image_url_with_prompt(
                &self.config,
                &self.paths,
                img.data_url(),
                &prompt,
            )
            .await
            {
                Ok(desc) => {
                    descriptions.push(format!("[Image {} 的描述]\n{}", i + 1, desc.trim()));
                }
                Err(error) if strict_pool => {
                    return Err(error).with_context(|| {
                        format!(
                            "configured multimodal model pool failed for image {}",
                            i + 1
                        )
                    });
                }
                Err(error) => {
                    descriptions.push(format!("[Image {} 识图失败: {}]", i + 1, error));
                }
            }
        }
        let combined = descriptions.join("\n\n");
        if input.trim().is_empty() {
            Ok(combined)
        } else {
            Ok(format!("{input}\n\n{combined}"))
        }
    }

    pub async fn chat_with_tools<F>(
        &mut self,
        current_turn_id: &str,
        messages: &mut Vec<ChatMessage>,
        used_tools: &mut Vec<String>,
        persisted_tool_reports: &mut Vec<(String, String)>,
        replay_start: usize,
        base_tool_reports: &[String],
        initial_tool_rounds: usize,
        initial_question_rounds: usize,
        control: Option<&AgentTurnControl>,
        on_event: &mut F,
    ) -> Result<ChatResult>
    where
        F: FnMut(AgentEvent) -> Result<()>,
    {
        let mut tool_round = initial_tool_rounds;
        let mut question_rounds = initial_question_rounds;
        let mut replay_start = replay_start;
        // Passive overflow recovery is a one-shot barrier per turn: the
        // post-compaction retry must not recover another overflow (pi /
        // opencode / Claude Code all converge on exactly one attempt).
        let mut overflow_recovery_attempted = false;
        let mut loaded_tools = self.initial_loaded_tools(messages)?;
        let mut usage_accumulator = UsageAccumulator::default();
        // v7 cache write-grace: provider prefix-cache writes are async, so a
        // follow-up fired within ~2s can miss the prefix the previous round
        // just computed (measured on DeepSeek). Track round completion time.
        let mut last_round_completed_at: Option<Instant> = None;
        let mut responses_continuation = None;
        let mut continuation_input_start = messages.len();
        let mut continuation_context: Option<(usize, Vec<ChatMessage>)> = None;
        let artifact_auto_publish = self.mode == AgentMode::Normal
            && self.prompt_audience == PromptAudience::External
            && artifact_delivery_requested(messages)
            && self
                .tools
                .lock()
                .unwrap()
                .tool_names()
                .iter()
                .any(|name| name == "create_artifact");
        let mut artifact_candidates = Vec::<AutoArtifactCandidate>::new();
        let mut artifact_published = false;
        loop {
            let tool_limit_reached = self.max_tool_rounds > 0 && tool_round >= self.max_tool_rounds;

            if self.config.skills.enabled {
                if self.mode == AgentMode::Normal {
                    let mut registry = self.tools.lock().unwrap();
                    tools::rescan_scripts(&mut registry, &self.paths);
                    tools::register_script_display_names(&registry);
                }
                let current_fingerprint = {
                    let registry = self.tools.lock().unwrap();
                    registry
                        .contains("load_skill")
                        .then(|| registry.skill_catalog_fingerprint())
                };
                if let Some(current_fingerprint) = current_fingerprint {
                    let config = self.config.clone();
                    let paths = self.paths.clone();
                    let refresh = tokio::task::spawn_blocking(move || {
                        tools::prepare_skill_refresh(current_fingerprint, &config, &paths)
                            .map(|snapshot| (snapshot, config, paths))
                    })
                    .await;
                    match refresh {
                        Ok(Ok((Some(snapshot), config, paths))) => {
                            let mut registry = self.tools.lock().unwrap();
                            tools::apply_skill_refresh(&mut registry, &config, &paths, snapshot);
                        }
                        Ok(Ok((None, _, _))) => {}
                        Ok(Err(error)) => {
                            tracing::warn!(error = %error, "failed to refresh GQY skill catalog")
                        }
                        Err(error) => {
                            tracing::warn!(error = %error, "GQY skill catalog worker stopped")
                        }
                    }
                }
            }

            let definitions = if self.tools_enabled && !tool_limit_reached {
                let tools = self.tools.lock().unwrap();
                if tools::is_stub_loading_mode(&self.config.tools.loading_mode) {
                    tools.stub_definitions()
                } else if tools::is_hybrid_loading_mode(&self.config.tools.loading_mode) {
                    tools.lazy_definitions(&loaded_tools)
                } else {
                    tools.definitions()
                }
            } else {
                Vec::new()
            };

            on_event(AgentEvent::ReasoningStart {
                received_at: Instant::now(),
            })?;
            let (chunk_tx, mut chunk_rx) =
                tokio::sync::mpsc::unbounded_channel::<(ChatStreamChunk, Instant)>();
            let mut request_messages = if responses_continuation.is_some() {
                messages
                    .get(continuation_input_start..)
                    .context("Responses continuation input cursor is out of bounds")?
                    .to_vec()
            } else {
                messages.clone()
            };
            if let Some((context_index, context_messages)) = continuation_context.as_ref() {
                let offset = context_index
                    .checked_sub(continuation_input_start)
                    .context("Responses continuation context cursor is out of bounds")?;
                if offset > request_messages.len() {
                    bail!("Responses continuation context cursor is out of bounds");
                }
                request_messages.splice(offset..offset, context_messages.clone());
            }
            let mut reasoning_filter = ReasoningTitleFilter::default();
            if self.config.cache.write_grace_ms > 0 {
                if let Some(previous) = last_round_completed_at {
                    let grace = std::time::Duration::from_millis(self.config.cache.write_grace_ms);
                    let elapsed = previous.elapsed();
                    if elapsed < grace {
                        tokio::time::sleep(grace - elapsed).await;
                    }
                }
            }
            if self.config.cache.keepalive_seconds > 0 && responses_continuation.is_none() {
                self.last_request_snapshot =
                    Some((request_messages.clone(), definitions.clone()));
            }
            let round_streamed = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let round = {
                let streamed_flag = round_streamed.clone();
                let llm_future = self.client.chat_stream_with_continuation(
                    request_messages.clone(),
                    definitions,
                    responses_continuation.as_deref(),
                    move |chunk| {
                        streamed_flag.store(true, Ordering::Relaxed);
                        let _ = chunk_tx.send((chunk, Instant::now()));
                        Ok(())
                    },
                );
                tokio::pin!(llm_future);
                let mut spinner_interval = tokio::time::interval(SPINNER_INTERVAL);
                spinner_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                spinner_interval.tick().await;
                let supersede = control.and_then(|control| control.supersede.as_deref());
                let supersede_generation = control.and_then(|control| {
                    supersede.map(|_| control.supersede_seen.load(Ordering::Acquire))
                });
                loop {
                    tokio::select! {
                        biased;
                        _ = async {
                            match (supersede, supersede_generation) {
                                (Some(signal), Some(generation)) => signal.wait_after(generation).await,
                                _ => std::future::pending::<()>().await,
                            }
                        } => {
                            break None;
                        }
                        result = &mut llm_future => {
                            break Some(result);
                        }
                        Some((chunk, received_at)) = chunk_rx.recv() => {
                            emit_model_chunk_at(
                                chunk,
                                received_at,
                                &mut reasoning_filter,
                                on_event,
                            )?;
                        }
                        _ = spinner_interval.tick() => {
                            on_event(AgentEvent::SpinnerTick)?;
                        }
                    }
                }
            };
            let round = match round {
                Some(Err(error)) => {
                    // Responses 续传自愈(任务#16):上游不支持
                    // previous_response_id 时,工具轮第二步只发增量会撞
                    // "No tool call found for tool output" 类 400。此时清
                    // 续传重发全量(messages 里工具结果已齐,无状态回放
                    // 完整),并让客户端持久记该供应商不可续传——本会话
                    // 与后续会话都不再发增量。
                    if responses_continuation.is_some()
                        && crate::llm::is_responses_continuation_unsupported_error(&error)
                    {
                        tracing::warn!(
                            error = %error,
                            "responses continuation rejected; retrying this round with full stateless input"
                        );
                        self.client.mark_responses_continuation_unsupported();
                        responses_continuation = None;
                        continue;
                    }
                    // Passive overflow trigger (compact-and-retry). Only at
                    // the turn's initial request, before any assistant output
                    // was streamed: mid-loop the live tool exchange is not
                    // rebuildable from the DB, and a partially shown answer
                    // must not be silently retried (opencode's
                    // hasAssistantStarted guard).
                    let initial_request = tool_round == initial_tool_rounds
                        && question_rounds == initial_question_rounds
                        && responses_continuation.is_none()
                        && !round_streamed.load(Ordering::Relaxed);
                    let window = self.context_window();
                    if initial_request
                        && !overflow_recovery_attempted
                        && window.is_some()
                        && crate::llm::is_context_overflow_error(&error)
                    {
                        overflow_recovery_attempted = true;
                        let window = window.unwrap();
                        let check = overflow::OverflowCheck::new(
                            Some(window),
                            self.trim_at_ratio,
                            None,
                        );
                        on_event(AgentEvent::CompactStart)?;
                        let compactor = compact::Compactor::new(
                            self.client.clone(),
                            self.state.clone(),
                            window,
                            check.reserved_tokens,
                            self.compact_tail_budget(window),
                            self.preset_dialogs.len(),
                        );
                        let mut on_compact_chunk =
                            |chunk: ChatStreamChunk| on_event(AgentEvent::CompactChunk(chunk));
                        // No fork here: a fork of an overflowing conversation
                        // overflows identically — recovery must use the
                        // isolated serialized path.
                        let compacted = compactor
                            .perform_compact(true, true, None, &mut on_compact_chunk)
                            .await;
                        on_event(AgentEvent::CompactEnd)?;
                        if let Ok(Some(compact_result)) = compacted {
                            self.state.add_auxiliary_usage(
                                &compact_result.usage,
                                crate::state::UsageMeta {
                                    source: self.usage_source(),
                                    provider: compact_result.provider_id.as_deref(),
                                    model: None,
                                },
                            )?;
                            // Splice the rebuilt (compacted) history prefix in
                            // front of the current turn's user message; the
                            // live tail (user input, runtime stamp, hints)
                            // is preserved byte-for-byte.
                            let user_index = live_user_index(messages, replay_start)
                                .unwrap_or_else(|| replay_start.min(messages.len()));
                            let (rebuilt, rebuilt_user_index) =
                                self.chat_messages(current_turn_id, "")?;
                            let tail = messages.split_off(user_index);
                            messages.clear();
                            messages.extend(rebuilt.into_iter().take(rebuilt_user_index));
                            messages.extend(tail);
                            // 活跃轮边界随尾巴整体平移:新前缀长 + 尾内偏移。
                            replay_start = rebuilt_user_index + (replay_start - user_index);
                            continuation_input_start = messages.len();
                            tracing::info!(
                                folded = compact_result.folded_turns,
                                kept = compact_result.kept_turns,
                                "context overflow recovered by compact-and-retry"
                            );
                            continue;
                        }
                        if let Err(compact_error) = compacted {
                            tracing::warn!(
                                error = %compact_error,
                                "compact-and-retry failed; surfacing the original overflow"
                            );
                        }
                    }
                    return Err(error);
                }
                Some(Ok(result)) => Some(result),
                None => None,
            };
            let Some(result) = round else {
                if let Some(control) = control {
                    if let Some(generation) = control.pending_supersede_generation() {
                        control.mark_supersede_seen(generation);
                    }
                }
                let queued = self.state.load_queued_prompts()?;
                if queued.is_empty() {
                    continue;
                }
                let prompt_ids = queued
                    .iter()
                    .map(|prompt| prompt.prompt_id.clone())
                    .collect::<Vec<_>>();
                on_event(AgentEvent::GenerationSuperseded { prompt_ids })?;
                let checkpoint = redo_checkpoint_payload(
                    messages,
                    replay_start,
                    base_tool_reports,
                    persisted_tool_reports,
                    tool_round,
                    question_rounds,
                );
                let continuation_context_index = responses_continuation.as_ref().map(|_| {
                    continuation_context
                        .as_ref()
                        .map(|(index, _)| *index)
                        .unwrap_or(messages.len())
                });
                self.consume_queued_prompts(
                    current_turn_id,
                    messages,
                    queued,
                    (None, None, None, None),
                    checkpoint,
                    control.expect("supersede requires turn control"),
                    on_event,
                )
                .await?;
                if let Some(index) = continuation_context_index {
                    continuation_context = Some((
                        index,
                        vec![
                            ChatMessage::turn_context(continuation_system_prompt(
                                &self.system_prompt,
                                self.mode,
                            )),
                            ChatMessage::turn_context(runtime_context(
                                self.mode,
                                self.platform_context.is_some(),
                            )),
                        ],
                    ));
                }
                continue;
            };
            while let Ok((chunk, received_at)) = chunk_rx.try_recv() {
                emit_model_chunk_at(chunk, received_at, &mut reasoning_filter, on_event)?;
            }
            let (title, text) = reasoning_filter.finish();
            if let Some(title) = title {
                on_event(AgentEvent::ReasoningTitle(title))?;
            }
            if let Some(text) = text {
                on_event(AgentEvent::Chunk(ChatStreamChunk {
                    kind: ChatStreamKind::Reasoning,
                    text,
                }))?;
            }
            usage_accumulator.add_result(&result, messages);
            if let Some(turn_usage) = usage_accumulator.usage() {
                let round = result.usage.clone().unwrap_or_else(|| {
                    let prompt = overflow::estimate_messages_tokens(&request_messages) as u64;
                    let completion = estimate_result_tokens(&result) as u64;
                    Usage {
                        prompt_tokens: prompt,
                        completion_tokens: completion,
                        total_tokens: prompt.saturating_add(completion),
                        ..Usage::default()
                    }
                });
                on_event(AgentEvent::RoundUsage {
                    round: Box::new(round),
                    turn: TurnTokens::from_usage(Some(&turn_usage)),
                    estimated: usage_accumulator.estimated,
                })?;
            }
            last_round_completed_at = Some(Instant::now());
            if result.tool_calls.is_empty() || !self.tools_enabled {
                responses_continuation = None;
                continuation_input_start = messages.len();
                continuation_context = None;
                if let Some(control) = control {
                    let queued = self.state.load_queued_prompts()?;
                    if !queued.is_empty() {
                        if let Some(generation) = control.pending_supersede_generation() {
                            let prompt_ids = queued
                                .iter()
                                .map(|prompt| prompt.prompt_id.clone())
                                .collect();
                            on_event(AgentEvent::GenerationSuperseded { prompt_ids })?;
                            let checkpoint = redo_checkpoint_payload(
                                messages,
                                replay_start,
                                base_tool_reports,
                                persisted_tool_reports,
                                tool_round,
                                question_rounds,
                            );
                            self.consume_queued_prompts(
                                current_turn_id,
                                messages,
                                queued,
                                (None, None, None, None),
                                checkpoint,
                                control,
                                on_event,
                            )
                            .await?;
                            control.mark_supersede_seen(generation);
                            continue;
                        }
                        push_assistant_context_messages(
                            messages,
                            &result.content,
                            result.reasoning.as_deref(),
                            true,
                        );
                        let checkpoint = redo_checkpoint_payload(
                            messages,
                            replay_start,
                            base_tool_reports,
                            persisted_tool_reports,
                            tool_round,
                            question_rounds,
                        );
                        self.consume_queued_prompts(
                            current_turn_id,
                            messages,
                            queued,
                            (
                                Some(&result.content),
                                result.reasoning.as_deref(),
                                result.provider_id.as_deref(),
                                result.model.as_deref(),
                            ),
                            checkpoint,
                            control,
                            on_event,
                        )
                        .await?;
                        continue;
                    }
                }
                let mut result = result;
                if artifact_auto_publish && !artifact_published {
                    publish_auto_artifact_candidates(&artifact_candidates, on_event)?;
                }
                if let Some(usage) = usage_accumulator.usage() {
                    result.last_request_usage = result.usage.take();
                    result.usage = Some(usage);
                    result.usage_estimated = usage_accumulator.estimated;
                }
                return Ok(result);
            }
            if tool_limit_reached {
                let mut result = result;
                let warning = format!(
                    "工具调用已达到上限 {} 轮，未执行后续工具调用。可将 `tools.max_rounds` 设为 0 以允许无限工具调用。",
                    self.max_tool_rounds
                );
                let warning_chunk = if result.content.trim().is_empty() {
                    warning.clone()
                } else {
                    format!("\n\n{warning}")
                };
                result.content.push_str(&warning_chunk);
                on_event(AgentEvent::Chunk(ChatStreamChunk {
                    kind: ChatStreamKind::Content,
                    text: warning_chunk,
                }))?;
                result.tool_calls.clear();
                if let Some(usage) = usage_accumulator.usage() {
                    result.last_request_usage = result.usage.take();
                    result.usage = Some(usage);
                    result.usage_estimated = usage_accumulator.estimated;
                }
                return Ok(result);
            }
            tool_round += 1;
            let next_responses_continuation = result.responses_continuation.clone();
            push_assistant_message_with_reasoning(
                messages,
                result.content.clone(),
                result.reasoning.as_deref(),
                result.thinking_signature.as_deref(),
                Some(result.tool_calls.clone()),
                true,
            );
            if result
                .finish_reason
                .as_deref()
                .is_some_and(|reason| reason.eq_ignore_ascii_case("length"))
                && !result.tool_calls.is_empty()
            {
                // A "length" stop means the output hit the token limit, so every
                // tool call in this message may carry silently truncated
                // arguments. Refuse to execute any of them and let the model
                // re-issue the calls with complete arguments.
                for call in &result.tool_calls {
                    messages.push(ChatMessage::tool(
                        call.id.clone(),
                        "error: 本次回复因输出 token 上限被截断，工具调用参数可能不完整。请重新发起该工具调用并给出完整参数。",
                    ));
                }
                continue;
            }
            if next_responses_continuation.is_some() {
                continuation_input_start = messages.len();
            }
            responses_continuation = next_responses_continuation;
            continuation_context = None;
            let ask_question_enabled = self
                .tools
                .lock()
                .unwrap()
                .tool_names()
                .iter()
                .any(|name| name == "ask_question");
            let question_call_count = result
                .tool_calls
                .iter()
                .filter(|call| ask_question_enabled && call.function.name == "ask_question")
                .count();
            if question_call_count == 1 {
                question_rounds += 1;
            }
            let question_round_allowed =
                question_call_count == 1 && question_rounds <= MAX_QUESTION_ROUNDS_PER_TURN;
            let defer_sibling_tools = question_call_count == 1 && result.tool_calls.len() > 1;
            // Multiple `task` calls in one batch run concurrently (subagents
            // are independent by design); everything else stays serial.
            let mut parallel_task_outputs =
                if defer_sibling_tools {
                    std::collections::HashMap::new()
                } else {
                    self.execute_parallel_task_calls(&result.tool_calls, &loaded_tools, on_event)
                        .await?
                };
            for (call_index, call) in result.tool_calls.into_iter().enumerate() {
                if let Some(group_output) = parallel_task_outputs.remove(&call_index) {
                    // Executed in the parallel group; events already emitted.
                    used_tools.push(call.function.name.clone());
                    if let Some(report) = group_output.report {
                        persisted_tool_reports.push((call.function.name.clone(), report));
                    }
                    let model_output = self
                        .spill_tool_output(
                            current_turn_id,
                            &call.id,
                            &call.function.name,
                            &group_output.output,
                        )
                        .unwrap_or(group_output.output);
                    messages.push(ChatMessage::tool(call.id, model_output));
                    continue;
                }
                let call_id = call.id.clone();
                let event_name = tool_event_name(&call.function.name, &call.function.arguments);
                on_event(AgentEvent::ToolCall {
                    call_id: call_id.clone(),
                    name: event_name.clone(),
                    arguments: call.function.arguments.clone(),
                })?;
                if question_call_count > 1 {
                    let output = "tool error: only one ask_question call is allowed per tool batch; combine all questions into one call".to_string();
                    on_event(AgentEvent::ToolResult {
                        call_id: call_id.clone(),
                        name: event_name.clone(),
                        ok: false,
                        output: output.clone(),
                    })?;
                    messages.push(ChatMessage::tool(call.id, output));
                    continue;
                }
                if defer_sibling_tools && call.function.name != "ask_question" {
                    let output = "tool error: deferred until the user answers ask_question; reissue this tool call after receiving the answer".to_string();
                    on_event(AgentEvent::ToolResult {
                        call_id: call_id.clone(),
                        name: event_name.clone(),
                        ok: false,
                        output: output.clone(),
                    })?;
                    messages.push(ChatMessage::tool(call.id, output));
                    continue;
                }
                if ask_question_enabled && call.function.name == "ask_question" {
                    if !question_round_allowed {
                        let output = format!(
                            "tool error: ask_question exceeded the per-turn limit of {MAX_QUESTION_ROUNDS_PER_TURN}"
                        );
                        on_event(AgentEvent::ToolResult {
                            call_id: call_id.clone(),
                            name: event_name.clone(),
                            ok: false,
                            output: output.clone(),
                        })?;
                        messages.push(ChatMessage::tool(call.id, output));
                        continue;
                    }
                    let request = match QuestionRequest::parse(&call.function.arguments) {
                        Ok(request) => request,
                        Err(err) => {
                            let output = format!("tool error: invalid ask_question request: {err}");
                            on_event(AgentEvent::ToolResult {
                                call_id: call_id.clone(),
                                name: event_name.clone(),
                                ok: false,
                                output: output.clone(),
                            })?;
                            messages.push(ChatMessage::tool(call.id, output));
                            continue;
                        }
                    };
                    let (response_tx, response_rx) = oneshot::channel();
                    on_event(AgentEvent::AskQuestion {
                        call_id: call_id.clone(),
                        request: request.clone(),
                        responder: response_tx,
                    })?;
                    let response = response_rx.await.unwrap_or(QuestionResponse::Cancelled);
                    let output = match response {
                        QuestionResponse::Answered(answers) => {
                            let exchange = QuestionExchange::new(request, answers)?;
                            self.state
                                .append_question_exchange(current_turn_id, &exchange)?;
                            answered_tool_output(&exchange)
                        }
                        QuestionResponse::Closed => closed_tool_output(),
                        QuestionResponse::Cancelled => return Err(QuestionCancelled.into()),
                        QuestionResponse::Unavailable(reason) => unavailable_tool_output(&reason),
                    };
                    messages.push(ChatMessage::tool(call.id, output.clone()));
                    on_event(AgentEvent::ToolResult {
                        call_id: call_id.clone(),
                        name: event_name,
                        ok: true,
                        output,
                    })?;
                    continue;
                }
                used_tools.push(call.function.name.clone());
                // 模式级 ReadOnly 权限门随闲聊模式一并删除:拒绝层现在是
                // registry 的单调 guard(软失败),不可用工具靠 registry 组合
                // 不注册(平台 restricted 同理),未知工具在分发处软失败。
                {
                    let tools = self.tools.lock().unwrap();
                    if tools::is_hybrid_loading_mode(&self.config.tools.loading_mode)
                        && call.function.name != "load_tools"
                        && tools.requires_lazy_load(&call.function.name, &loaded_tools)
                    {
                        if tools.can_auto_load_direct_call(&call.function.name) {
                            loaded_tools.insert(call.function.name.clone());
                            if self.config.tools.persist_loaded_tools {
                                self.state.add_session_loaded_tools(
                                    &[call.function.name.clone()],
                                    Some(current_turn_id),
                                )?;
                            }
                        } else {
                            let output = format!(
                                "tool error: 工具 `{}` 尚未加载。请先调用 load_tools，参数为 {{\"names\":[\"{}\"]}}。",
                                call.function.name,
                                call.function.name,
                            );
                            on_event(AgentEvent::ToolResult {
                                call_id: call_id.clone(),
                                name: event_name.clone(),
                                ok: false,
                                output: output.clone(),
                            })?;
                            messages.push(ChatMessage::tool(call.id, output));
                            continue;
                        }
                    }
                }
                let (progress_tx, mut progress_rx) = mpsc::unbounded_channel();
                let tool_future = {
                    let tools = self.tools.lock().unwrap();
                    // Homebrew review/install 互斥等回合级规则已迁入 guard 层,凭 used_tools 上下文判定。
                    tools.call_with_progress_future(
                        &call.function.name,
                        &call.function.arguments,
                        progress_tx,
                        &crate::tools::GuardCtx {
                            used_tools: &used_tools,
                        },
                    )
                };
                let tool_future = match tool_future {
                    Ok(f) => f,
                    Err(err) => {
                        let output = format!("tool error: {err}");
                        on_event(AgentEvent::ToolResult {
                            call_id: call_id.clone(),
                            name: event_name.clone(),
                            ok: false,
                            output: output.clone(),
                        })?;
                        messages.push(ChatMessage::tool(call.id, output));
                        continue;
                    }
                };
                tokio::pin!(tool_future);
                let mut spinner_interval = tokio::time::interval(SPINNER_INTERVAL);
                spinner_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                spinner_interval.tick().await;
                let (output, tool_succeeded) = loop {
                    tokio::select! {
                        result = &mut tool_future => {
                            break match result {
                                Ok(output) => {
                                    while let Ok(progress) = progress_rx.try_recv() {
                                        emit_tool_progress(on_event, &call_id, &event_name, progress)?;
                                    }
                                    (output, true)
                                }
                                Err(err) => {
                                    while let Ok(progress) = progress_rx.try_recv() {
                                        emit_tool_progress(on_event, &call_id, &event_name, progress)?;
                                    }
                                    on_event(AgentEvent::ToolResult {
                                        call_id: call_id.clone(),
                                        name: event_name.clone(),
                                        ok: false,
                                        output: format!("tool error: {err}"),
                                    })?;
                                    (format!("tool error: {err}"), false)
                                }
                            };
                        }
                        Some(progress) = progress_rx.recv() => {
                            emit_tool_progress(on_event, &call_id, &event_name, progress)?;
                        }
                        _ = spinner_interval.tick() => {
                            on_event(AgentEvent::SpinnerTick)?;
                        }
                    }
                };
                let clipboard_image = if tool_succeeded {
                    clipboard_binary_image_from_tool_result(&call.function.name, &output)
                } else {
                    None
                };
                let mut model_output = self
                    .spill_tool_output(current_turn_id, &call.id, &call.function.name, &output)
                    .unwrap_or_else(|| output.clone());
                // 重复调用观察:成功/失败/被拒都计数(反复撞拒绝正是要打断
                // 的循环)。提醒**折进工具结果字节**而不是独立消息——
                // derive_tool_flow 只持久化 assistant/tool 消息,独立提醒
                // 下一轮回放即消失,前缀在此掰断(缓存调研 08-16,deepseek
                // 报告 P0-2 实证同一处)。folded 形态活体=回放,永远同源。
                if let Some(reminder) = self
                    .repeat_chain
                    .observe(&call.function.name, &call.function.arguments)
                {
                    model_output.push_str("\n\n");
                    model_output.push_str(&reminder);
                }
                messages.push(ChatMessage::tool(call.id.clone(), model_output));
                if tool_succeeded && call.function.name == "load_tools" {
                    let loaded = loaded_items_from_output(&output);
                    for name in &loaded.tools {
                        loaded_tools.insert(name.clone());
                    }
                    if self.config.tools.persist_loaded_tools {
                        self.state
                            .add_session_loaded_tools(&loaded.tools, Some(current_turn_id))?;
                        self.state
                            .add_session_loaded_targets(&loaded.targets, Some(current_turn_id))?;
                    }
                }
                if let Some(img) = clipboard_image {
                    let supports_vision = self.current_model_supports_vision();
                    let uses_vision_fallback =
                        !supports_vision && self.config.plugins.vision.enabled;
                    if !supports_vision {
                        let message = if self.config.plugins.vision.enabled {
                            if crate::i18n::is_zh() {
                                "视觉分析."
                            } else {
                                "Vision analysis."
                            }
                        } else if crate::i18n::is_zh() {
                            "当前模型不支持图片，且未启用视觉模型，无法分析剪贴板图片。"
                        } else {
                            "The current model does not support images and the vision plugin is disabled, so the clipboard image cannot be analyzed."
                        };
                        on_event(AgentEvent::ToolProgress {
                            call_id: call_id.clone(),
                            name: event_name.clone(),
                            message: message.to_string(),
                        })?;
                    }
                    let image_message = if uses_vision_fallback {
                        let image_future = self.clipboard_image_message(img);
                        tokio::pin!(image_future);
                        let mut spinner_interval = tokio::time::interval(SPINNER_INTERVAL);
                        spinner_interval
                            .set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                        spinner_interval.tick().await;
                        let mut progress_interval =
                            tokio::time::interval(Duration::from_millis(900));
                        progress_interval
                            .set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                        progress_interval.tick().await;
                        let mut progress_tick = 0usize;
                        loop {
                            tokio::select! {
                                result = &mut image_future => {
                                    break result?;
                                }
                                _ = progress_interval.tick() => {
                                    progress_tick = progress_tick.wrapping_add(1);
                                    on_event(AgentEvent::ToolProgress {
                                        call_id: call_id.clone(),
                                        name: event_name.clone(),
                                        message: vision_analysis_progress(progress_tick),
                                    })?;
                                }
                                _ = spinner_interval.tick() => {
                                    on_event(AgentEvent::SpinnerTick)?;
                                }
                            }
                        }
                    } else {
                        self.clipboard_image_message(img).await?
                    };
                    if let Some(message) = image_message {
                        messages.push(message);
                    }
                }
                if tool_succeeded {
                    let result_ok = tool_output_succeeded(&output);
                    if result_ok {
                        if let Some(delta) =
                            tool_call_footprint(&call.function.name, &call.function.arguments)
                        {
                            self.state.merge_turn_footprint(current_turn_id, &delta)?;
                        }
                        if matches!(
                            call.function.name.as_str(),
                            "create_artifact" | "apply_artifact_patch" | "present_artifact"
                        ) {
                            artifact_published = true;
                        } else if artifact_auto_publish {
                            for path in artifact_candidate_paths(&call.function.name, &output) {
                                artifact_candidates.push(AutoArtifactCandidate {
                                    call_id: call_id.clone(),
                                    tool_name: event_name.clone(),
                                    path,
                                });
                            }
                        }
                    }
                    on_event(AgentEvent::ToolResult {
                        call_id,
                        name: event_name.clone(),
                        ok: result_ok,
                        output: output.clone(),
                    })?;
                    if let Some(report) =
                        extract_persistable_tool_report(&call.function.name, &output)
                    {
                        persisted_tool_reports.push((call.function.name.clone(), report));
                    }
                }
            }
            if question_round_allowed {
                tool_round = tool_round.saturating_sub(1);
            }
            if let Some(control) = control {
                if let Some(queue_ingress) = control.queue_ingress.as_ref() {
                    queue_ingress.wait_for_reserved_ingress().await;
                }
                let queued = self.state.load_queued_prompts()?;
                if !queued.is_empty() {
                    let supersede_generation = control.pending_supersede_generation();
                    if supersede_generation.is_some() {
                        let prompt_ids = queued
                            .iter()
                            .map(|prompt| prompt.prompt_id.clone())
                            .collect();
                        on_event(AgentEvent::GenerationSuperseded { prompt_ids })?;
                    }
                    let checkpoint = redo_checkpoint_payload(
                        messages,
                        replay_start,
                        base_tool_reports,
                        persisted_tool_reports,
                        tool_round,
                        question_rounds,
                    );
                    let preceding_assistant = if supersede_generation.is_some() {
                        (None, None, None, None)
                    } else {
                        (
                            Some(result.content.as_str()),
                            result.reasoning.as_deref(),
                            result.provider_id.as_deref(),
                            result.model.as_deref(),
                        )
                    };
                    let continuation_context_index = responses_continuation.as_ref().map(|_| {
                        continuation_context
                            .as_ref()
                            .map(|(index, _)| *index)
                            .unwrap_or(messages.len())
                    });
                    self.consume_queued_prompts(
                        current_turn_id,
                        messages,
                        queued,
                        preceding_assistant,
                        checkpoint,
                        control,
                        on_event,
                    )
                    .await?;
                    if let Some(index) = continuation_context_index {
                        continuation_context = Some((
                            index,
                            vec![
                                ChatMessage::turn_context(continuation_system_prompt(
                                    &self.system_prompt,
                                    self.mode,
                                )),
                                ChatMessage::turn_context(runtime_context(
                                    self.mode,
                                    self.platform_context.is_some(),
                                )),
                            ],
                        ));
                    }
                    if let Some(generation) = supersede_generation {
                        control.mark_supersede_seen(generation);
                    }
                }
            }
        }
    }

    pub fn initial_loaded_tools(&self, messages: &[ChatMessage]) -> Result<BTreeSet<String>> {
        if !self.config.tools.persist_loaded_tools {
            return Ok(BTreeSet::new());
        }
        let mut loaded = self.state.load_session_loaded_tools()?;
        if loaded.is_empty() {
            loaded = loaded_tools_from_messages(messages);
            if !loaded.is_empty() {
                let names = loaded.iter().cloned().collect::<Vec<_>>();
                self.state.add_session_loaded_tools(&names, None)?;
            }
        }
        if !loaded.is_empty() {
            let tools = self.tools.lock().unwrap();
            let available = tools.tool_names().into_iter().collect::<BTreeSet<_>>();
            loaded.retain(|name| available.contains(name));
        }
        Ok(loaded)
    }

    pub async fn clipboard_image_message(&self, img: ClipboardImage) -> Result<Option<ChatMessage>> {
        if self.current_model_supports_vision() {
            return Ok(Some(ChatMessage::user_parts(vec![
                ChatContentPart::ImageUrl {
                    image_url: ImageUrlContent {
                        url: img.data_url().to_string(),
                    },
                },
            ])));
        }

        let images = vec![&img];
        let description = self
            .describe_images_with_vision_provider("", &images)
            .await?;
        if description.trim().is_empty() {
            return Ok(None);
        }
        Ok(Some(ChatMessage::plain("user", description)))
    }

    /// 返回 (消息序列, 当前用户消息下标)。用户消息之后是数量可变的
    /// 瞬态尾巴(runtime 投影可跳、防失忆提醒隔轮注入),调用方必须用
    /// 下标定位,绝不能再按"倒数第二条"猜(缓存调研 08-16 的复位地雷)。
    pub fn chat_messages(
        &self,
        current_turn_id: &str,
        current_input: &str,
    ) -> Result<(Vec<ChatMessage>, usize)> {
        let mut messages = vec![ChatMessage::system(self.system_prompt.clone())];
        // 预设对话(begin_dialogs):system 之后、历史之前,每请求注入、
        // 永不落库。模型把它当普通聊天记录,学的是轮次里的语气;作为
        // 常量前缀只在编辑时断一次缓存。compact_fork_prefix 同步注入,
        // 保持折叠请求与实况字节一致。
        for (user, assistant) in &self.preset_dialogs {
            messages.push(ChatMessage::plain("user", user.clone()));
            messages.push(ChatMessage::assistant(assistant.clone(), None));
        }
        if !self.suppress_session_history {
            if let Some(summary) = self.state.load_last_summary()? {
                messages.push(summary_checkpoint_message(&summary.assistant_content));
            }
            let turns = self.state.load_visible_turns_excluding(current_turn_id)?;
            for turn in &turns {
                if turn.is_summary {
                    continue;
                }
                // A turn still running holds a placeholder that gets overwritten
                // with the real reply once it finishes, so replaying it would
                // put two different byte sequences at the same position and
                // drop the prefix cache for everyone after it. Roughly a fifth
                // of this group's turns overlap. The placeholder only ever said
                // "ignore me" anyway.
                if turn.status == crate::state::TurnStatus::Running {
                    continue;
                }
                self.push_history_turn(&mut messages, turn);
            }
        }
        // v7 §三: the runtime stamp is transient tail and must sit AFTER the
        // current user message. When it preceded the user message, every next
        // turn's replayed history diverged from the provider's cached prefix
        // exactly at this position (verified byte-level against DeepSeek
        // prefix caching).
        let user_index = messages.len();
        messages.push(ChatMessage::plain("user", current_input));
        // dsh 式投影(08-16 缓存调研):运行时上下文"变了才注入"。终端面
        // 时间已降到小时级,同一小时内 cwd/环境不变 → 与历史里最近一份
        // 化石逐字节相同 → 本轮零新增;平台面保留分钟级,人格报时靠它。
        let runtime = runtime_context(self.mode, self.platform_context.is_some());
        if last_fossil_with_prefix(&messages, "<runtime ") != Some(runtime.as_str()) {
            messages.push(ChatMessage::turn_context(runtime));
        }
        // 防失忆提醒(08-16 起):不再浮动,每隔 interval 轮以化石身份进
        // 历史——纯追加,不掰前缀。计数以历史里最近一份提醒化石所在的
        // 轮为锚。
        if let Some(reminder) = self.persona_reminder.as_deref() {
            let interval = self.config.prompt.persona_reminder_interval.max(1) as usize;
            if turns_since_reminder_fossil(&self.state, current_turn_id)?
                .map_or(true, |since| since >= interval)
            {
                messages.push(ChatMessage::turn_context(format!(
                    "<persona-reminder>{reminder}</persona-reminder>"
                )));
            }
        }
        Ok((messages, user_index))
    }

    /// Renders one stored turn exactly as the live request rendered it
    /// (byte-identical replay incl. the fossilized transient tail), shared by
    /// the main request path and the compaction fork prefix.
    pub fn push_history_turn(&self, messages: &mut Vec<ChatMessage>, turn: &crate::state::Turn) {
        messages.push(self.turn_user_message(turn));
        // Fossilized transient tail (v7 append-only): replay the
        // system messages that followed the user message in the live
        // request, byte-identical and in order, so this turn renders
        // as a pure extension of what the provider already cached.
        messages.extend(turn.context_messages.iter().map(replay_fossil));
        if turn.status == crate::state::TurnStatus::Interrupted && !turn.journal_events.is_empty() {
            messages.extend(interrupted_turn_replay_messages(self, turn));
        } else {
            // 问答只回放一种形态:有结构化 tool_flow 的回合,ask_question
            // 已作为原生 tool_calls+tool 输出在 flow 里逐字节回放;再补
            // 纯文本问答对=同一轮发两遍且字节不同于活体,前缀在此掰断
            // (缓存调研 08-16,deepseek 报告 P0-2③实证)。纯文本对只给
            // 无 flow 的老回合兜底。
            if turn.tool_flow.is_empty() {
                for exchange in &turn.question_exchanges {
                    messages.push(ChatMessage::plain(
                        "assistant",
                        crate::question::assistant_exchange_text(exchange),
                    ));
                    messages.push(ChatMessage::plain(
                        "user",
                        crate::question::user_exchange_text(exchange),
                    ));
                }
            }
            for followup in &turn.followups {
                push_assistant_context_messages(
                    messages,
                    followup
                        .preceding_assistant_content
                        .as_deref()
                        .unwrap_or_default(),
                    followup.preceding_assistant_reasoning.as_deref(),
                    false,
                );
                messages.push(self.followup_user_message(followup));
            }
            // dsh 形态回放:每轮 assistant 带原生 tool_calls(参数原样字节),
            // 随后各 call 的 role:"tool" 输出;最终回复照旧收尾。老回合
            // (无结构化流)退回 private_tool_memory 压扁兜底。
            for round in &turn.tool_flow {
                push_assistant_message_with_reasoning(
                    messages,
                    round.assistant_content.clone(),
                    round.assistant_reasoning.as_deref(),
                    None,
                    Some(
                        round
                            .calls
                            .iter()
                            .map(|call| ToolCall {
                                id: call.id.clone(),
                                kind: "function".to_string(),
                                function: ToolCallFunction {
                                    name: call.name.clone(),
                                    arguments: call.arguments.clone(),
                                },
                            })
                            .collect(),
                    ),
                    false,
                );
                for call in &round.calls {
                    messages.push(ChatMessage::tool(call.id.clone(), call.output.clone()));
                }
            }
            push_assistant_context_messages(
                messages,
                &turn.assistant_content,
                turn.assistant_reasoning.as_deref(),
                true,
            );
            if turn.tool_flow.is_empty() && !turn.tool_reports.is_empty() {
                messages.push(ChatMessage::turn_context(private_tool_memory(
                    &turn.tool_reports,
                )));
            }
        }
    }

    /// Byte-identical prefix of the live conversation covering exactly the
    /// turns about to fold: `[system][checkpoint][fold turns...]`. A fork
    /// summarization request built on this prefix re-reads the history at
    /// cached price instead of full price (the serialized fallback shares no
    /// bytes with the provider's cache).
    pub fn compact_fork_prefix(&self, fold_turn_ids: &[String]) -> Result<Vec<ChatMessage>> {
        let fold: std::collections::HashSet<&str> =
            fold_turn_ids.iter().map(|id| id.as_str()).collect();
        let mut messages = vec![ChatMessage::system(self.system_prompt.clone())];
        // 与 chat_messages 的实况前缀字节一致:预设对话也在折叠前缀里。
        for (user, assistant) in &self.preset_dialogs {
            messages.push(ChatMessage::plain("user", user.clone()));
            messages.push(ChatMessage::assistant(assistant.clone(), None));
        }
        if let Some(summary) = self.state.load_last_summary()? {
            messages.push(summary_checkpoint_message(&summary.assistant_content));
        }
        for turn in self.state.load_visible_turns()? {
            if turn.is_summary || !fold.contains(turn.turn_id.as_str()) {
                continue;
            }
            self.push_history_turn(&mut messages, &turn);
        }
        Ok(messages)
    }

    pub fn live_tool_definitions(&self) -> Result<Vec<crate::llm::ToolDefinition>> {
        if !self.tools_enabled {
            return Ok(Vec::new());
        }
        let loaded = self.initial_loaded_tools(&[])?;
        let tools = self.tools.lock().unwrap();
        Ok(
            if tools::is_stub_loading_mode(&self.config.tools.loading_mode) {
                tools.stub_definitions()
            } else if tools::is_hybrid_loading_mode(&self.config.tools.loading_mode) {
                tools.lazy_definitions(&loaded)
            } else {
                tools.definitions()
            },
        )
    }

    pub fn followup_user_message(&self, followup: &crate::state::TurnFollowup) -> ChatMessage {
        if !self.current_model_supports_vision() {
            return ChatMessage::plain("user", &followup.content);
        }
        let mut images = followup
            .attachments
            .iter()
            .filter_map(|attachment| match attachment {
                QueuedPromptAttachment::Binary { mime, data_base64 } => {
                    Some(ChatContentPart::ImageUrl {
                        image_url: ImageUrlContent {
                            url: format!("data:{mime};base64,{data_base64}"),
                        },
                    })
                }
                QueuedPromptAttachment::Path { .. } => None,
            })
            .collect::<Vec<_>>();
        images.extend(self.uploaded_attachment_image_parts(&followup.uploaded_attachments));
        if images.is_empty() {
            return ChatMessage::plain("user", &followup.content);
        }
        let mut parts = vec![ChatContentPart::Text {
            text: followup.content.clone(),
        }];
        parts.extend(images);
        ChatMessage::user_parts(parts)
    }

    pub fn turn_user_message(&self, turn: &crate::state::Turn) -> ChatMessage {
        if !self.current_model_supports_vision() {
            return ChatMessage::plain("user", &turn.user_content);
        }
        let images = self.uploaded_attachment_image_parts(&turn.attachments);
        if images.is_empty() {
            return ChatMessage::plain("user", &turn.user_content);
        }
        let mut parts = vec![ChatContentPart::Text {
            text: turn.user_content.clone(),
        }];
        parts.extend(images);
        ChatMessage::user_parts(parts)
    }

    pub fn uploaded_attachment_image_parts(
        &self,
        attachments: &[crate::state::UserAttachment],
    ) -> Vec<ChatContentPart> {
        attachments
            .iter()
            .filter(|attachment| attachment.kind == "image")
            .filter_map(|attachment| {
                self.state
                    .load_user_attachment(&attachment.attachment_id)
                    .ok()
                    .flatten()
            })
            .map(|attachment| ChatContentPart::ImageUrl {
                image_url: ImageUrlContent {
                    url: ClipboardImage::new(attachment.attachment.mime, attachment.bytes)
                        .data_url()
                        .to_string(),
                },
            })
            .collect()
    }

    pub fn queued_prompt_images(&self, prompt: &QueuedPrompt) -> Result<Vec<Option<PastedImage>>> {
        let mut images = queued_prompt_images(prompt)?;
        for attachment in &prompt.uploaded_attachments {
            if attachment.kind != "image" {
                continue;
            }
            if let Some(data) = self.state.load_user_attachment(&attachment.attachment_id)? {
                images.push(Some(PastedImage::Binary(ClipboardImage::new(
                    data.attachment.mime,
                    data.bytes,
                ))));
            }
        }
        Ok(images)
    }
}
