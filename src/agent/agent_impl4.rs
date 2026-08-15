//! agent_impl4 — 自 src/agent/agent_impl2.rs 拆分。

#![allow(
    clippy::cloned_ref_to_slice_refs,
    clippy::needless_borrow,
    clippy::unnecessary_map_or
)]
pub(crate) use super::*;

impl Agent {
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
                self.last_request_snapshot = Some((request_messages.clone(), definitions.clone()));
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
                        let check =
                            overflow::OverflowCheck::new(Some(window), self.trim_at_ratio, None);
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
            let mut parallel_task_outputs = if defer_sibling_tools {
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

    pub async fn clipboard_image_message(
        &self,
        img: ClipboardImage,
    ) -> Result<Option<ChatMessage>> {
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
