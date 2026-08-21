//! overflow_handling — 上下文溢出处理与视觉描述（原 agent_impl3）。

pub(crate) use super::*;

impl Agent {
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
                let snip_threshold =
                    (window as f32 * self.config.context.compact_snip_ratio).max(1.0) as usize;
                let soft_threshold =
                    (window as f32 * self.config.context.compact_soft_ratio).max(1.0) as usize;
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
                let force_threshold =
                    (window as f32 * self.config.context.compact_force_ratio).max(1.0) as usize;
                let force = context_tokens >= force_threshold;
                // Prune first: it is free, and when it alone lands the
                // context back under the trigger the paid summary call (and
                // its cache reset) is skipped entirely.
                if self.config.context.prune_stale_tool_reports {
                    let stats = self.prune_stale_history(window)?;
                    if stats.turns > 0 && !force {
                        let post_tokens =
                            usize::try_from(self.effective_context_tokens()?).unwrap_or(usize::MAX);
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
}
