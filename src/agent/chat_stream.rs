//! chat_stream — Agent 对话/redo 流式处理（原 agent_impl2）。

pub(crate) use super::*;

impl Agent {
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
        let (
            replay_start,
            fossil_start,
            base_tool_reports,
            initial_tool_rounds,
            initial_question_rounds,
        ) = match candidate.input_kind {
            RedoInputKind::Initial => {
                let (_, input) = prepared.pop().context("redo input is empty")?;
                messages.push(input.message);
                let fossil_start = messages.len();
                messages.extend(tail_fossils);
                let replay_start = messages.len();
                messages.extend(input.hints);
                (replay_start, fossil_start, Vec::new(), 0, 0)
            }
            RedoInputKind::Followup => {
                let checkpoint = redo.checkpoint.context("redo checkpoint is unavailable")?;
                messages.push(self.turn_user_message(&current_turn));
                let fossil_start = messages.len();
                messages.extend(tail_fossils);
                let replay_start = messages.len();
                messages.extend(checkpoint.replay_messages);
                for (_, input) in prepared {
                    messages.push(input.message);
                    messages.extend(input.hints);
                }
                (
                    replay_start,
                    fossil_start,
                    checkpoint.prefix_tool_reports,
                    checkpoint.tool_rounds,
                    checkpoint.question_rounds,
                )
            }
        };
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
            self.state
                .set_turn_tool_flow(&candidate.turn_id, &tool_flow)?;
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
        let association_exclusion =
            self.state
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
        if self.tools_enabled && self.config.plugins.vision.enabled && self.image_platform.is_some()
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
        self.config
            .context
            .compact_tail_tokens
            .unwrap_or(16384.min(context_window / 4))
    }
}
