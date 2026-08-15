//! handlers — 自 src/platforms/onebot.rs 拆分。

use super::*;
use crate::platforms::SessionTurnAcquireError;

pub(crate) async fn handle_message_with_activity(
    state: DaemonState,
    conn: ConnectionHandle,
    event: Value,
    ingress_order: i64,
    activity: Option<InboundMessageActivity>,
) {
    let mut app_config = state.manager.lock().unwrap().config.clone();
    let config = app_config.platforms.qq.clone();
    if !config.enabled {
        return;
    }
    let user_id = event.get("user_id").and_then(Value::as_i64).unwrap_or(0);
    let self_id = event.get("self_id").and_then(Value::as_i64).unwrap_or(0);
    if user_id == 0 || user_id == self_id {
        return;
    }
    let message_type = event
        .get("message_type")
        .and_then(Value::as_str)
        .unwrap_or("");
    let target = match message_type {
        "private" => Target::Private { user_id },
        "group" => {
            let group_id = event.get("group_id").and_then(Value::as_i64).unwrap_or(0);
            if group_id == 0 {
                return;
            }
            Target::Group { group_id }
        }
        _ => return,
    };
    let admission = admission_for_with_state(&config, &state.state_store, target, self_id, user_id);
    if !admission.allowed {
        return;
    }
    apply_admission_text_model_pool(&mut app_config, target, &admission);

    let mut parsed = parse_message(event.get("message"), event.get("raw_message"), self_id);
    if let Some(reason) = parsed.rejected_reason {
        tracing::warn!(
            target: "gqy::qq",
            self_id,
            sender_id = user_id,
            conversation_kind = target.kind(),
            conversation_id = target.conversation_id(),
            %reason,
            "{}",
            t("OneBot message rejected before plugin processing", "OneBot 消息在插件处理前被拒绝")
        );
        return;
    }
    let parsed_command = commands::parse(&app_config.platforms, parsed.text.trim());
    let mut inbound_event = message_event_at(
        target,
        &event,
        &parsed,
        activity
            .as_ref()
            .map(|activity| activity.received_at)
            .unwrap_or_else(Instant::now),
        activity.as_ref().map(|activity| activity.position),
    );
    inbound_event.ingress_order = Some(ingress_order);
    if parsed_command.is_none() && matches!(target, Target::Group { .. }) && config.show_group_name
    {
        inbound_event.conversation_display_name =
            resolve_group_name(&conn, self_id, target.conversation_id(), &event).await;
    }
    if parsed_command.is_none() && !parsed.mentioned_user_ids.is_empty() {
        inbound_event.mentioned_users =
            resolve_mentioned_users(&conn, self_id, target, &parsed.mentioned_user_ids).await;
    }
    let quoted_message_id = parsed_command
        .is_none()
        .then(|| {
            parsed.reply_to_message_id.as_deref().filter(|id| {
                event.get("message_id").and_then(value_id_string).as_deref() != Some(*id)
            })
        })
        .flatten();
    parsed.quoted_message_data = if let Some(quoted_message_id) = quoted_message_id {
        match get_message_data(&conn, quoted_message_id, QUOTED_MESSAGE_LOOKUP_TIMEOUT).await {
            Ok(data) => {
                let info = parse_message_info(&data, self_id)
                    .filter(|info| info.message_id == quoted_message_id)
                    .filter(|info| message_info_matches_target(info, target));
                if info.is_none() {
                    tracing::warn!(
                        target: "gqy::qq",
                        quoted_message_id,
                        "{}",
                        t("OneBot quoted-message metadata was missing or mismatched", "OneBot 引用消息元数据缺失或不匹配")
                    );
                }
                if info.is_some() {
                    inbound_event.replied_message = info;
                    Some(data)
                } else {
                    // Prevent the image merge stage from repeating an
                    // unscoped lookup for a cross-conversation message id.
                    parsed.reply_to_message_id = None;
                    None
                }
            }
            Err(error) => {
                tracing::warn!(
                    target: "gqy::qq",
                    error = %error,
                    quoted_message_id,
                    "{}",
                    t("OneBot quoted-message metadata lookup failed", "OneBot 引用消息元数据查询失败")
                );
                None
            }
        }
    } else {
        None
    };
    let context = match platform_turn_context_with_activity(
        &state,
        conn.clone(),
        target,
        &event,
        app_config,
        Some(inbound_event.clone()),
        activity.map(|activity| activity.handle),
    ) {
        Ok(context) => Arc::new(context),
        Err(error) => {
            tracing::warn!(target: "gqy::qq", error = %error, "{}", t("OneBot platform runtime initialization failed", "OneBot 平台运行时初始化失败"));
            return;
        }
    };

    // Classify group traffic before charging rate limits. Busy groups often
    // produce many messages that do not wake GQY and must not starve actual
    // mentions or prefix commands.
    // Built-in commands own only their registered names. Other prefixed input
    // remains ordinary chat after plugins have had a chance to claim it.
    let plugin_command_response = if parsed_command.is_some() {
        None
    } else {
        context.handle_command(parsed.text.trim()).await
    };
    let builtin_command = if plugin_command_response.is_none() {
        parsed_command
    } else {
        None
    };

    // Plugins may supersede same-sender work before this message enters the
    // shared judgement/turn admission queue.
    let session_id = if plugin_command_response.is_none() && builtin_command.is_none() {
        match resolve_onebot_session(&state, &context, target, &event) {
            Ok(session_id) => Some(session_id),
            Err(error) => {
                tracing::warn!(target: "gqy::qq", error = %error, "{}", t("resolving the QQ session failed", "解析 QQ 会话失败"));
                if matches!(target, Target::Private { .. }) {
                    let _ = context
                        .send_bypass_plugins(OutboundMessage::text(
                            OutboundOrigin::Command,
                            t(
                                "Something went wrong while opening this conversation.",
                                "打开当前会话时出错了。",
                            ),
                        ))
                        .await;
                }
                return;
            }
        }
    } else {
        None
    };
    let core_trigger_content = (plugin_command_response.is_none() && builtin_command.is_none())
        .then(|| match target {
            Target::Private { .. } => Some(parsed.text.clone()),
            Target::Group { .. } => group_trigger_text(
                &config,
                &parsed,
                inbound_event.replied_message.as_ref(),
                self_id,
            ),
        })
        .flatten();
    if let Some(session_id) = session_id.as_deref() {
        // Group chats only accept follow-ups while a tool is executing (the
        // reservation guarantees same-round consumption); outside that window
        // group messages go through supersede/new-turn admission because other
        // people may be talking to each other. Private chats behave like the
        // REPL/WebUI instead: any message while a turn is active becomes a
        // follow-up to that turn, with the ingress reservation held when one
        // is available.
        let followup_target = if matches!(target, Target::Group { .. }) {
            reserve_tool_followup(
                &state,
                session_id,
                &context.conversation,
                &context.sender_id,
            )
            .map(|(run_id, turn_id, followup, reservation)| {
                (run_id, turn_id, followup, Some(reservation))
            })
        } else {
            platform_update_target(
                &state,
                session_id,
                &context.conversation,
                &context.sender_id,
            )
            .map(|(run_id, turn_id, followup)| {
                let reservation = followup.try_reserve();
                (run_id, turn_id, followup, reservation)
            })
        };
        if let Some((run_id, turn_id, followup, reservation)) = followup_target {
            let _ingress_reservation = reservation;
            let _enqueue_order = followup.lock_enqueue().await;
            let rate_decision = admission
                .rate_key
                .as_deref()
                .map_or(RateDecision::Allow, |key| {
                    state
                        .platforms
                        .rate
                        .lock()
                        .unwrap()
                        .check(key, admission.rate_limit)
                });
            if rate_decision != RateDecision::Allow {
                if rate_decision == RateDecision::DropWithNotice {
                    let _ = context
                        .send_bypass_plugins(OutboundMessage::text(
                            OutboundOrigin::Command,
                            t(
                                "Too many messages — please slow down a little.",
                                "消息太频繁了，请稍候再发。",
                            ),
                        ))
                        .await;
                }
                return;
            }
            match enqueue_tool_followup(
                &state,
                &conn,
                target,
                &event,
                parsed,
                &inbound_event,
                &context,
                &followup,
                session_id,
                &run_id,
                &turn_id,
                TurnUpdateMode::Followup,
            )
            .await
            {
                Ok(()) => tracing::info!(
                    target: "gqy::qq",
                    session_id,
                    sender_id = user_id,
                    message_id = %inbound_event.message_id,
                    "{}",
                    t("OneBot message queued as a follow-up to the active turn", "OneBot 消息已加入当前回合的后续队列")
                ),
                Err(error) => tracing::warn!(
                    target: "gqy::qq",
                    session_id,
                    sender_id = user_id,
                    error = %error,
                    "{}",
                    t("OneBot follow-up could not be queued", "OneBot 后续消息无法入队")
                ),
            }
            return;
        }
    }
    if let Some(session_id) = session_id.as_deref() {
        if context.preempt_inbound(&inbound_event) {
            if let Some((run_id, turn_id, followup)) = platform_update_target(
                &state,
                session_id,
                &context.conversation,
                &context.sender_id,
            ) {
                let _enqueue_order = followup.lock_enqueue().await;
                let result = enqueue_tool_followup(
                    &state,
                    &conn,
                    target,
                    &event,
                    parsed,
                    &inbound_event,
                    &context,
                    &followup,
                    session_id,
                    &run_id,
                    &turn_id,
                    TurnUpdateMode::Supersede,
                )
                .await;
                match result {
                    Ok(()) => {
                        // 覆盖成功:表情从旧消息转移到新消息,补救窗口从
                        // 新消息重新起算(链式覆盖)。
                        context.confirm_supersede(&inbound_event).await;
                        tracing::info!(
                            target: "gqy::qq",
                            session_id,
                            sender_id = user_id,
                            message_id = %inbound_event.message_id,
                            "{}",
                            t("OneBot message superseded the active generation", "OneBot 消息已取代当前生成")
                        )
                    }
                    Err(error) => tracing::warn!(
                        target: "gqy::qq",
                        session_id,
                        sender_id = user_id,
                        error = %error,
                        "{}",
                        t("OneBot active generation could not be superseded", "无法取代 OneBot 当前生成")
                    ),
                }
                return;
            }
            let manager = state.manager.lock().unwrap();
            for run in manager
                .active_runs
                .values()
                .filter(|run| &*run.session_id == session_id)
                .filter(|run| {
                    run.platform_followup.as_ref().is_some_and(|followup| {
                        followup.conversation == context.conversation
                            && followup.sender_id == context.sender_id
                    })
                })
            {
                run.request_cancel();
            }
        }
    }
    let session_limits = config.session_limits(
        match target {
            Target::Private { .. } => PlatformConversationKind::Private,
            Target::Group { .. } => PlatformConversationKind::Group,
        },
        &target.conversation_id().to_string(),
    );
    let session_turn_ticket = session_id.as_deref().map(|session_id| {
        state
            .platforms
            .session_turn_ticket(session_id, session_limits)
    });
    let session_turn = match session_turn_ticket {
        Some(ticket) => match ticket.acquire().await {
            Ok(lease) => Some(lease),
            // Dropped in silence. Announcing a full queue told the group
            // nothing it could act on — the backlog clears on its own — and
            // the apology itself cost a message at the exact moment the
            // conversation was already saturated. The log keeps it visible to
            // whoever runs the bot.
            Err(super::SessionTurnAcquireError::Full) => {
                tracing::debug!(
                    target: "gqy::qq",
                    session_id = ?session_id,
                    sender_id = user_id,
                    message_id = %inbound_event.message_id,
                    "{}",
                    t(
                        "OneBot message discarded: the conversation queue is full",
                        "OneBot 消息已丢弃：当前会话等待队列已满"
                    )
                );
                return;
            }
            Err(super::SessionTurnAcquireError::Closed) => return,
        },
        None => None,
    };
    if session_turn
        .as_ref()
        .is_some_and(|session_turn| !session_turn.is_valid())
    {
        context.after_turn_aborted().await;
        return;
    }
    let message_id = inbound_event.message_id.clone();
    if plugin_command_response.is_none() && builtin_command.is_none() {
        let trigger_content = core_trigger_content;
        let mut trigger = TriggerDecision {
            should_reply: trigger_content.is_some(),
            content: trigger_content.unwrap_or_else(|| parsed.text.clone()),
            // Reply targeting is owned by the real-context plugin. Keeping
            // the transport core neutral makes its quote/mention switches
            // authoritative and avoids an invisible default quote.
            response_target: None,
        };
        let rate_available = admission.rate_key.as_deref().is_none_or(|key| {
            state
                .platforms
                .rate
                .lock()
                .unwrap()
                .available(key, admission.rate_limit)
        });
        context.set_reply_rate_available(rate_available);
        context.observe_inbound(&inbound_event).await;
        context.decide_trigger(&inbound_event, &mut trigger).await;
        if !trigger.should_reply {
            return;
        }
        parsed.text = trigger.content;
        context.set_response_target(trigger.response_target);
    }

    tracing::info!(
        target: "gqy::qq",
        self_id,
        sender_id = user_id,
        conversation_kind = target.kind(),
        conversation_id = target.conversation_id(),
        %message_id,
        text_chars = parsed.text.chars().count(),
        images = parsed
            .images
            .len()
            .saturating_add(parsed.unresolved_image_files.len()),
        files = parsed.files.len(),
        command = plugin_command_response.is_some() || builtin_command.is_some(),
        "{}",
        t("OneBot message accepted", "OneBot 消息已接受")
    );

    // Built-in control commands bypass chat rate limits and preempt the
    // target session's active and queued work after authorization.
    if let Some(command) = builtin_command {
        if let Some(response) =
            execute_builtin_command(&state, &context, target, &event, command).await
        {
            if let Err(error) = context.send_bypass_plugins(response).await {
                tracing::warn!(target: "gqy::qq", error = %error, "{}", t("OneBot built-in command response failed", "OneBot 内置命令响应失败"));
            } else {
                tracing::info!(target: "gqy::qq", self_id, sender_id = user_id, "{}", t("OneBot built-in command response sent", "OneBot 内置命令响应已发送"));
            }
        }
        return;
    }

    let decision = admission
        .rate_key
        .as_deref()
        .map_or(RateDecision::Allow, |key| {
            state
                .platforms
                .rate
                .lock()
                .unwrap()
                .check(key, admission.rate_limit)
        });
    match decision {
        RateDecision::Allow => {}
        RateDecision::DropSilently => {
            tracing::info!(
                target: "gqy::qq",
                self_id,
                sender_id = user_id,
                conversation_kind = target.kind(),
                conversation_id = target.conversation_id(),
                "{}",
                t("OneBot message rate-limited", "OneBot 消息已被限流")
            );
            context.after_turn_aborted().await;
            return;
        }
        RateDecision::DropWithNotice => {
            let notice_sent = sends_rate_limit_notice(target);
            tracing::info!(
                target: "gqy::qq",
                self_id,
                sender_id = user_id,
                conversation_kind = target.kind(),
                conversation_id = target.conversation_id(),
                notice_sent,
                "{}",
                t("OneBot message rate-limited", "OneBot 消息已被限流")
            );
            if notice_sent {
                let _ = context
                    .send_bypass_plugins(OutboundMessage::text(
                        OutboundOrigin::Command,
                        t(
                            "Too many messages — please slow down a little.",
                            "消息太频繁了，请稍候再发。",
                        ),
                    ))
                    .await;
            }
            context.after_turn_aborted().await;
            return;
        }
    }

    // Platform commands are independent of the LLM group wake trigger.
    if let Some(response) = plugin_command_response {
        if let Err(error) = context.send_bypass_plugins(response).await {
            tracing::warn!(target: "gqy::qq", error = %error, "{}", t("OneBot plugin command response failed", "OneBot 插件命令响应失败"));
        } else {
            tracing::info!(target: "gqy::qq", self_id, sender_id = user_id, "{}", t("OneBot plugin command response sent", "OneBot 插件命令响应已发送"));
        }
        return;
    }
    let session_id = session_id.expect("non-command message has a resolved session");
    let session_turn = session_turn.expect("non-command message owns a session turn");
    let turn = build_and_run_turn(
        &state,
        &conn,
        target,
        &event,
        parsed,
        context.clone(),
        session_id,
    )
    .await;
    if !session_turn.is_valid() {
        context.after_turn_aborted().await;
        return;
    }
    match turn {
        Ok(Some(dispatch)) => match deliver_dispatch(&state, &context, dispatch).await {
            Err(error) => {
                tracing::warn!(target: "gqy::qq", error = %error, "{}", t("OneBot reply delivery failed", "OneBot 回复投递失败"));
                context.after_turn_aborted().await;
            }
            Ok(true) => {
                tracing::info!(
                    target: "gqy::qq",
                    self_id,
                    sender_id = user_id,
                    conversation_kind = target.kind(),
                    conversation_id = target.conversation_id(),
                    "{}",
                    t("OneBot reply delivered", "OneBot 回复已投递")
                );
            }
            Ok(false) => {}
        },
        Ok(None) => {
            if !context.turn_is_superseded() {
                context.after_turn_aborted().await;
            }
        }
        Err(error) => {
            tracing::warn!(target: "gqy::qq", error = %error, "{}", t("OneBot message handling failed", "OneBot 消息处理失败"));
            context.after_turn_aborted().await;
            if matches!(target, Target::Private { .. }) {
                let _ = context
                    .send_bypass_plugins(OutboundMessage::text(
                        OutboundOrigin::Command,
                        format!(
                            "{}{}",
                            t("Something went wrong: ", "出错了："),
                            safe_error_message(&error)
                        ),
                    ))
                    .await;
            }
        }
    }
}

pub(crate) fn message_info_matches_target(info: &PlatformMessageInfo, target: Target) -> bool {
    let expected_kind = match target {
        Target::Private { .. } => ConversationKind::Private,
        Target::Group { .. } => ConversationKind::Group,
    };
    info.conversation_kind == Some(expected_kind)
        && info.conversation_id.as_deref() == Some(target.conversation_id().to_string().as_str())
}

pub(crate) async fn execute_builtin_command(
    state: &DaemonState,
    context: &PlatformTurnContext,
    target: Target,
    event: &Value,
    command: commands::ParsedPlatformCommand,
) -> Option<OutboundMessage> {
    let response = match command {
        commands::ParsedPlatformCommand::Reset { scope } => {
            let descriptor = commands::descriptor(commands::RESET_COMMAND_ID)
                .expect("the reset command descriptor is registered");
            if !commands::is_allowed(&context.config.platforms, descriptor, context.is_admin) {
                return None;
            } else if scope.is_none() {
                commands::reset_usage_message(&context.config.platforms)
            } else {
                match resolve_onebot_session(state, context, target, event) {
                    Err(error) => {
                        tracing::warn!(target: "gqy::qq", error = %error, "{}", t("resolving the QQ session for reset failed", "解析待重置的 QQ 会话失败"));
                        t(
                            "The conversation could not be reset. Check the daemon logs for details.",
                            "无法重置当前会话，请查看 daemon 日志。",
                        )
                        .to_string()
                    }
                    Ok(session_id) => {
                        let ticket = state.platforms.preempt_session_turns(&session_id);
                        cancel_session_runs(state, &session_id);
                        let _session_turn = ticket.acquire().await.ok();
                        match clear_platform_session_content(state, session_id.clone()).await {
                            Ok(()) => match context.after_session_reset().await {
                                Ok(()) => {
                                tracing::info!(
                                    target: "gqy::qq",
                                    session_id = %session_id,
                                    sender_id = %context.sender_id,
                                    "{}",
                                    t("QQ conversation reset", "QQ 会话已重置")
                                );
                                t(
                                    "The current conversation has been reset.",
                                    "当前会话已重置。",
                                )
                                .to_string()
                                }
                                Err(error) => {
                                    tracing::warn!(
                                        target: "gqy::qq",
                                        session_id = %session_id,
                                        error = %error,
                                        "{}",
                                        t("QQ conversation reset but plugin state update failed", "QQ 会话已重置，但插件状态更新失败")
                                    );
                                    t(
                                        "The conversation was cleared, but its platform history boundary could not be updated. Run /reset again.",
                                        "会话内容已清空，但通讯平台历史边界更新失败，请再次执行 /reset。",
                                    )
                                    .to_string()
                                }
                            },
                            Err(PlatformSessionResetError::Busy) => t(
                                "This conversation is replying right now. Try resetting it again after the reply finishes.",
                                "当前会话正在回复，请在回复结束后重试。",
                            )
                            .to_string(),
                            Err(PlatformSessionResetError::Unavailable) => t(
                                "The GQY core is unavailable, so the conversation was not reset.",
                                "GQY 核心当前不可用，会话未重置。",
                            )
                            .to_string(),
                            Err(PlatformSessionResetError::Internal(error)) => {
                                tracing::warn!(target: "gqy::qq", session_id = %session_id, error = %error, "{}", t("resetting the QQ conversation failed", "重置 QQ 会话失败"));
                                t(
                                    "The conversation could not be reset. Check the daemon logs for details.",
                                    "无法重置当前会话，请查看 daemon 日志。",
                                )
                                .to_string()
                            }
                        }
                    }
                }
            }
        }
        commands::ParsedPlatformCommand::Wipe { confirmed } => {
            let descriptor = commands::descriptor(commands::WIPE_COMMAND_ID)
                .expect("the wipe command descriptor is registered");
            if !commands::is_allowed(&context.config.platforms, descriptor, context.is_admin) {
                return None;
            }
            if !confirmed {
                commands::wipe_confirm_message(&context.config.platforms)
            } else {
                match reset_platform_persona_state(state, &context.config).await {
                    Ok(_) => t(
                        "Memory, every conversation's contents, group-chat contexts and generated skills for the current persona have been erased.",
                        "当前人格的记忆、全部会话内容、群聊上下文和自动技能已抹掉。",
                    )
                    .to_string(),
                    Err(PlatformPersonaResetError::Busy) => t(
                        "GQY is busy. Try again shortly.",
                        "GQY 正忙，请稍后重试。",
                    )
                    .to_string(),
                    Err(PlatformPersonaResetError::Unavailable) => t(
                        "The wipe service is temporarily unavailable.",
                        "抹除服务暂时不可用。",
                    )
                    .to_string(),
                    Err(PlatformPersonaResetError::Internal(error)) => {
                        tracing::warn!(target: "gqy::qq", %error, "{}", t("wiping the QQ persona state failed", "抹除 QQ 人格状态失败"));
                        t(
                            "The wipe could not be completed. Check the daemon logs for details.",
                            "抹除未能完成，请查看 daemon 日志。",
                        )
                        .to_string()
                    }
                }
            }
        }
        commands::ParsedPlatformCommand::ResetMemory { confirmed } => {
            let descriptor = commands::descriptor(commands::RESET_MEMORY_COMMAND_ID)
                .expect("the reset-memory command descriptor is registered");
            if !commands::is_allowed(&context.config.platforms, descriptor, context.is_admin) {
                return None;
            }
            if !confirmed {
                commands::reset_memory_confirm_message(&context.config.platforms)
            } else {
                // context.config 已按平台人格覆盖作用域化:清的就是这个
                // 会话所属人格的记忆命名空间;会话历史与技能不动。
                match crate::memory::MemoryStore::new(&context.config, &state.paths)
                    .reset_all(false)
                {
                    Ok(()) => t("Long-term memory erased.", "长期记忆已清空。").to_string(),
                    Err(error) => {
                        tracing::warn!(target: "gqy::qq", %error, "{}", t("resetting platform memory failed", "平台记忆清空失败"));
                        t(
                            "The memory reset could not be completed. Check the daemon logs for details.",
                            "记忆清空未能完成，请查看 daemon 日志。",
                        )
                        .to_string()
                    }
                }
            }
        }
        commands::ParsedPlatformCommand::Stop { has_arguments } => {
            let descriptor = commands::descriptor(commands::STOP_COMMAND_ID)
                .expect("the stop command descriptor is registered");
            if !commands::is_allowed(&context.config.platforms, descriptor, context.is_admin) {
                commands::permission_denied_message(&context.config.platforms, descriptor)
            } else if has_arguments {
                commands::stop_usage_message(&context.config.platforms)
            } else {
                match resolve_onebot_session(state, context, target, event) {
                    Err(error) => {
                        tracing::warn!(target: "gqy::qq", error = %error, "{}", t("resolving the QQ session for stop failed", "解析待停止的 QQ 会话失败"));
                        t(
                            "The current conversation could not be stopped. Check the daemon logs for details.",
                            "无法停止当前会话，请查看 daemon 日志。",
                        )
                        .to_string()
                    }
                    Ok(session_id) => {
                        let queued = state.platforms.queued_session_turns(&session_id);
                        let ticket = state.platforms.preempt_session_turns(&session_id);
                        let cancelled = cancel_session_runs(state, &session_id);
                        let _session_turn = ticket.acquire().await.ok();
                        tracing::info!(
                            target: "gqy::qq",
                            session_id = %session_id,
                            sender_id = %context.sender_id,
                            cancelled,
                            queued,
                            "{}",
                            t("QQ conversation stopped", "QQ 会话已停止")
                        );
                        stop_response_message(cancelled, queued)
                    }
                }
            }
        }
        commands::ParsedPlatformCommand::Models { argument } => {
            let descriptor = commands::descriptor(commands::MODELS_COMMAND_ID)
                .expect("the models command descriptor is registered");
            if !commands::is_allowed(&context.config.platforms, descriptor, context.is_admin) {
                // Deliberately silent for non-admins, like /reset: no reply
                // and no log line.
                return None;
            }
            execute_models_command(state, target, argument.as_deref())
        }
    };
    Some(OutboundMessage::text(OutboundOrigin::Command, response))
}
