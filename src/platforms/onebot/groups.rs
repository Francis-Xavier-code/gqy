//! groups — 自 src/platforms/onebot.rs 拆分。

use super::*;

/// its per-conversation route (私聊/群聊专属配置), creating the route if needed.
/// `/models` lists the globally configured models; `/models <index|provider/model>`
/// switches this conversation's text model by writing a single-model pool into
pub(crate) fn execute_models_command(
    state: &DaemonState,
    target: Target,
    argument: Option<&str>,
) -> String {
    let kind = match target {
        Target::Private { .. } => PlatformConversationKind::Private,
        Target::Group { .. } => PlatformConversationKind::Group,
    };
    let conversation_id = target.conversation_id().to_string();
    let mut manager = state.manager.lock().unwrap();
    let choices = manager.config.text_provider_model_choices();
    if choices.is_empty() {
        return t("No models are configured.", "尚未配置任何模型。").to_string();
    }
    let Some(argument) = argument else {
        let effective = manager
            .config
            .qq_text_model_pool(kind, &conversation_id, false)
            .unwrap_or(&[])
            .to_vec();
        // Plain numbered lines read best in QQ: no alignment padding (IM
        // fonts are proportional) and no empty checkbox noise — only the
        // effective models carry a marker.
        let mut lines = vec![t("Available models:", "可用模型：").to_string()];
        for (index, choice) in choices.iter().enumerate() {
            let active = effective.iter().any(|active| {
                active.provider_id == choice.provider_id && active.model == choice.model
            });
            let marker = if active {
                t(" ✅current", " ✅当前")
            } else {
                ""
            };
            lines.push(format!("{}. {}{marker}", index + 1, choice.label()));
        }
        lines.push(format!(
            "{}{}",
            t("Switch with: ", "切换模型："),
            commands::models_switch_hint(&manager.config.platforms)
        ));
        return lines.join("\n");
    };
    let selected = match crate::config::resolve_provider_model_argument(&choices, argument) {
        Ok(choice) => choice.clone(),
        Err(message) => return message,
    };
    if manager.admin_busy {
        return t(
            "GQY is busy with another admin operation. Try again shortly.",
            "GQY 正忙于其他管理操作，请稍后再试。",
        )
        .to_string();
    }
    let mut next_config = manager.config.clone();
    let mut route = next_config
        .platforms
        .model_route(kind, &conversation_id)
        .cloned()
        .unwrap_or_else(|| crate::config::PlatformModelRoute {
            conversation: crate::config::PlatformConversationConfig {
                kind,
                id: conversation_id.clone(),
            },
            persona: crate::config::PlatformPersonaOverride::default(),
            text_models_inheritance: crate::config::PlatformModelPoolInheritance::default(),
            text_models: None,
            multimodal_models_inheritance: crate::config::PlatformModelPoolInheritance::default(),
            multimodal_models: None,
            extra_prompt: String::new(),
            session_limits: None,
        });
    route.text_models = Some(vec![crate::config::ActiveProviderModelConfig {
        provider_id: selected.provider_id.clone(),
        model: selected.model.clone(),
    }]);
    next_config.platforms.upsert_model_route(route);
    if let Err(error) = next_config.save(&state.paths) {
        tracing::warn!(
            target: "gqy::qq",
            error = %error,
            "{}",
            t(
                "saving the conversation model override failed",
                "保存会话专属模型配置失败"
            )
        );
        return t(
            "The model could not be saved. Check the daemon logs for details.",
            "模型切换保存失败，请查看 daemon 日志。",
        )
        .to_string();
    }
    manager.config = next_config;
    format!(
        "{}{}",
        t(
            "This conversation now uses (saved to its dedicated settings): ",
            "本会话已切换模型（已写入私聊/群聊专属配置）："
        ),
        selected.label()
    )
}

pub(crate) fn stop_response_message(cancelled: usize, queued: usize) -> String {
    if crate::i18n::is_zh() {
        match (cancelled, queued) {
            (0, 0) => "当前会话没有正在运行的任务。".to_string(),
            (_, 0) => format!("已打断 {cancelled} 个运行中的任务。"),
            (0, _) => format!("已丢弃 {queued} 个排队中的任务。"),
            _ => format!("已打断 {cancelled} 个运行中的任务、{queued} 个排队中的任务。"),
        }
    } else {
        match (cancelled, queued) {
            (0, 0) => "No running tasks to stop in the current conversation.".to_string(),
            (_, 0) => format!("Interrupted {cancelled} running task(s)."),
            (0, _) => format!("Discarded {queued} queued task(s)."),
            _ => format!(
                "Interrupted {cancelled} running task(s) and discarded {queued} queued task(s)."
            ),
        }
    }
}

pub(crate) fn cancel_session_runs(state: &DaemonState, session_id: &str) -> usize {
    let manager = state.manager.lock().unwrap();
    let mut cancelled = 0;
    for run in manager
        .active_runs
        .values()
        .filter(|run| &*run.session_id == session_id)
    {
        run.request_cancel();
        cancelled += 1;
    }
    cancelled
}

pub(crate) fn platform_turn_context(
    state: &DaemonState,
    conn: ConnectionHandle,
    target: Target,
    event: &Value,
    config: crate::config::AppConfig,
    inbound_event: Option<PlatformInboundEvent>,
) -> Result<PlatformTurnContext> {
    platform_turn_context_with_activity(state, conn, target, event, config, inbound_event, None)
}

pub(crate) fn platform_turn_context_with_activity(
    state: &DaemonState,
    conn: ConnectionHandle,
    target: Target,
    event: &Value,
    mut config: crate::config::AppConfig,
    inbound_event: Option<PlatformInboundEvent>,
    activity: Option<crate::platforms::MessageActivityHandle>,
) -> Result<PlatformTurnContext> {
    let self_id = event.get("self_id").and_then(Value::as_i64).unwrap_or(0);
    let user_id = event.get("user_id").and_then(Value::as_i64).unwrap_or(0);
    let user_id_text = user_id.to_string();
    let conversation = platform_conversation(target, self_id);
    let conversation_kind = match target {
        Target::Private { .. } => PlatformConversationKind::Private,
        Target::Group { .. } => PlatformConversationKind::Group,
    };
    config.apply_qq_conversation_persona(conversation_kind, &conversation.conversation_id);
    if !config.prompt.active_persona.trim().is_empty()
        && !config
            .persona_path(&state.paths, config.prompt.active_persona.trim())
            .is_file()
    {
        bail!(
            "QQ conversation persona does not exist: {}",
            config.prompt.active_persona
        );
    }
    let sender_display_name = event_sender_display_name(event);
    let is_admin = config.platforms.qq.admin_users.contains(&user_id)
        || has_dynamic_access(
            &state.state_store,
            &conversation.account_id,
            AccessPermission::Administrator,
            &user_id_text,
        );
    let adapter = Arc::new(OneBotAdapter {
        conn,
        registry: state.platforms.onebot.clone(),
        http: state.platforms.http_client()?,
        self_id,
        target,
        max_reply_chars: config.platforms.qq.max_reply_chars,
    });
    let mut context = PlatformTurnContext::new(
        conversation,
        user_id_text,
        sender_display_name,
        is_admin,
        config,
        state.paths.clone(),
        state.state_store.clone(),
        adapter,
        state.platforms.plugins()?,
    )
    .with_config_manager(state.manager.clone());
    if let Some(activity) = activity {
        context = context.with_message_activity(activity);
    }
    Ok(match inbound_event {
        Some(event) => context.with_inbound_event(event),
        None => context,
    })
}

pub(crate) fn value_id_string(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

pub(crate) fn value_i64(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_str().and_then(|value| value.trim().parse().ok()))
}

pub(crate) async fn get_message_data(
    conn: &ConnectionHandle,
    message_id: &str,
    timeout: Duration,
) -> Result<Value> {
    let message_id = message_id.trim();
    if message_id.is_empty() || message_id.len() > MAX_ONEBOT_ID_BYTES {
        bail!("invalid OneBot message id");
    }
    conn.call_api_with_timeout(
        "get_msg",
        json!({ "message_id": onebot_id_value(message_id) }),
        timeout,
    )
    .await
}

/// Adds images from exactly one quoted message. A nested `reply` segment in
/// the fetched message is intentionally ignored, preventing recursive lookup.
pub(crate) async fn merge_quoted_message_images(
    conn: &ConnectionHandle,
    current_message_id: &str,
    parsed: &mut InboundMessage,
    quoted_message_data: Option<&Value>,
) -> Result<usize> {
    let Some(quoted_message_id) = parsed.reply_to_message_id.clone() else {
        return Ok(0);
    };
    if quoted_message_id == current_message_id
        || parsed.images.len() >= MAX_INBOUND_IMAGES
        || parsed
            .images
            .iter()
            .map(MediaRef::inline_bytes)
            .sum::<usize>()
            >= MAX_INBOUND_IMAGE_TOTAL_BYTES
    {
        return Ok(0);
    }

    let fetched;
    let data = if let Some(data) = quoted_message_data {
        data
    } else {
        fetched = get_message_data(conn, &quoted_message_id, QUOTED_MESSAGE_LOOKUP_TIMEOUT).await?;
        &fetched
    };
    if data
        .get("message_id")
        .and_then(value_id_string)
        .is_some_and(|returned_id| returned_id != quoted_message_id)
    {
        bail!("OneBot get_msg returned a different message id");
    }
    let before = parsed.images.len();
    let unresolved =
        append_message_image_sources(parsed, data.get("message"), data.get("raw_message"));
    let lookups = unresolved.into_iter().map(|file| async move {
        let result = conn.call_api("get_image", json!({ "file": &file })).await;
        (file, result)
    });
    for (file, result) in join_all(lookups).await {
        match result {
            Ok(data) => {
                append_resolved_quoted_image(parsed, &data);
            }
            Err(error) => {
                tracing::warn!(
                    target: "gqy::qq",
                    error = %error,
                    image_file = %file,
                    "{}",
                    t("OneBot get_image lookup for a quoted image failed", "OneBot 查询引用图片的 get_image 失败")
                );
            }
        }
    }
    Ok(parsed.images.len().saturating_sub(before))
}

pub(crate) async fn resolve_current_message_images(
    conn: &ConnectionHandle,
    parsed: &mut InboundMessage,
) {
    let unresolved = std::mem::take(&mut parsed.unresolved_image_files);
    let lookups = unresolved.into_iter().map(|file| async move {
        let result = conn
            .call_api_with_timeout(
                "get_image",
                json!({ "file": &file }),
                QUOTED_MESSAGE_LOOKUP_TIMEOUT,
            )
            .await;
        (file, result)
    });
    for (file, result) in join_all(lookups).await {
        match result {
            Ok(data) => {
                append_resolved_quoted_image(parsed, &data);
            }
            Err(error) => {
                tracing::warn!(
                    target: "gqy::qq",
                    error = %error,
                    image_file = %file,
                    "{}",
                    t("OneBot get_image lookup for an inbound image failed", "OneBot 查询传入图片的 get_image 失败")
                );
            }
        }
    }
}

pub(crate) fn append_resolved_quoted_image(parsed: &mut InboundMessage, data: &Value) -> bool {
    let before = parsed.images.len();
    push_inbound_image_source(
        parsed,
        data.get("file").and_then(Value::as_str).unwrap_or(""),
        data.get("url").and_then(Value::as_str),
    );
    if parsed.images.len() == before {
        if let Some(encoded) = data.get("base64").and_then(Value::as_str) {
            push_inbound_base64(parsed, encoded);
        }
    }
    parsed.images.len() > before
}

pub(crate) struct PreparedInboundImages {
    pub(crate) attachments: Vec<Option<ImageAttachment>>,
    pub(crate) attempted: usize,
    pub(crate) failed: usize,
    pub(crate) duplicates: usize,
    pub(crate) total_bytes: usize,
}

pub(crate) async fn prepare_inbound_images(
    state: &DaemonState,
    media_refs: Vec<MediaRef>,
) -> Result<PreparedInboundImages> {
    let attempted = media_refs.len().min(MAX_INBOUND_IMAGES);
    let mut attachments = Vec::with_capacity(attempted);
    let mut failed = 0usize;
    let mut duplicates = 0usize;
    let mut total_bytes = 0usize;
    let mut seen_content = HashSet::<[u8; 32]>::with_capacity(attempted);

    for media in media_refs.into_iter().take(MAX_INBOUND_IMAGES) {
        let remaining = MAX_INBOUND_IMAGE_TOTAL_BYTES.saturating_sub(total_bytes);
        if remaining == 0 {
            failed += 1;
            continue;
        }
        let maximum = MAX_INBOUND_IMAGE_BYTES.min(remaining);
        let bytes = match media {
            MediaRef::Bytes(bytes) if bytes.len() <= maximum => bytes,
            MediaRef::Bytes(_) => {
                failed += 1;
                continue;
            }
            MediaRef::Url(url) => {
                let http = state.platforms.http_client()?;
                match download_capped(&http, &url, maximum, IMAGE_DOWNLOAD_TIMEOUT).await {
                    Ok((bytes, _)) => bytes,
                    Err(error) => {
                        failed += 1;
                        tracing::warn!(error = %error, "{}", t("OneBot image download failed", "OneBot 图片下载失败"));
                        continue;
                    }
                }
            }
        };
        let digest: [u8; 32] = Sha256::digest(&bytes).into();
        if !seen_content.insert(digest) {
            duplicates += 1;
            continue;
        }
        total_bytes += bytes.len();
        let mime = sniff_image_mime(&bytes).to_string();
        attachments.push(Some(ImageAttachment::Binary { mime, data: bytes }));
    }

    Ok(PreparedInboundImages {
        attachments,
        attempted,
        failed,
        duplicates,
        total_bytes,
    })
}

/// Turns a parsed inbound message into agent input (downloading media),
/// resolves the dedicated session and runs the turn. `Ok(None)` means
/// the message needs no reply (e.g. sticker-only).
pub(crate) fn platform_update_target(
    state: &DaemonState,
    session_id: &str,
    conversation: &PlatformConversation,
    sender_id: &str,
) -> Option<(String, String, Arc<PlatformFollowupRun>)> {
    let manager = state.manager.lock().unwrap();
    manager
        .active_runs
        .iter()
        .filter(|(_, run)| &*run.session_id == session_id)
        .filter_map(|(run_id, run)| {
            let followup = run.platform_followup.as_ref()?;
            if followup.conversation != *conversation || followup.sender_id != sender_id {
                return None;
            }
            Some((
                followup.started(),
                run_id.clone(),
                run.turn_id.clone()?,
                followup.clone(),
            ))
        })
        .max_by_key(|(started, _, _, _)| *started)
        .map(|(_, run_id, turn_id, followup)| (run_id, turn_id, followup))
}

pub(crate) fn reserve_tool_followup(
    state: &DaemonState,
    session_id: &str,
    conversation: &PlatformConversation,
    sender_id: &str,
) -> Option<(
    String,
    String,
    Arc<PlatformFollowupRun>,
    crate::agent::QueueIngressReservation,
)> {
    let (run_id, turn_id, followup) =
        platform_update_target(state, session_id, conversation, sender_id)?;
    let reservation = followup.try_reserve()?;
    Some((run_id, turn_id, followup, reservation))
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn enqueue_tool_followup(
    state: &DaemonState,
    conn: &ConnectionHandle,
    target: Target,
    event: &Value,
    mut parsed: InboundMessage,
    inbound_event: &PlatformInboundEvent,
    context: &PlatformTurnContext,
    followup: &PlatformFollowupRun,
    session_id: &str,
    run_id: &str,
    turn_id: &str,
    mode: TurnUpdateMode,
) -> Result<()> {
    if !parsed.unresolved_image_files.is_empty() {
        resolve_current_message_images(conn, &mut parsed).await;
    }
    let current_message_id = event
        .get("message_id")
        .and_then(value_id_string)
        .unwrap_or_default();
    let quoted_message_data = parsed.quoted_message_data.take();
    let quoted_images = merge_quoted_message_images(
        conn,
        &current_message_id,
        &mut parsed,
        quoted_message_data.as_ref(),
    )
    .await
    .unwrap_or_else(|error| {
        tracing::warn!(
            target: "gqy::qq",
            error = %error,
            message_id = %current_message_id,
            "{}",
            t("OneBot follow-up quoted images could not be prepared", "无法准备 OneBot 后续消息的引用图片")
        );
        0
    });
    let mut content = parsed.text.trim().to_string();
    let prepared_images = prepare_inbound_images(state, parsed.images).await?;
    let attempted_images = prepared_images.attempted;
    let failed_images = prepared_images.failed;
    let mut attachments = Vec::with_capacity(prepared_images.attachments.len());
    for image in prepared_images.attachments.into_iter().flatten() {
        match image {
            ImageAttachment::Binary { mime, data } => {
                attachments.push(QueuedPromptAttachment::Binary {
                    mime,
                    data_base64: BASE64.encode(data),
                });
            }
            ImageAttachment::Path { path } => {
                attachments.push(QueuedPromptAttachment::Path { path });
            }
        }
    }
    for file in &parsed.files {
        match fetch_inbound_file(state, conn, target, file).await {
            Ok(path) => {
                if !content.is_empty() {
                    content.push('\n');
                }
                content.push_str(&format!(
                    "[{} {} {} {}]",
                    t("the user sent a file", "用户发来文件"),
                    file.name,
                    t("saved at", "已保存于"),
                    path.display()
                ));
            }
            Err(error) => {
                tracing::warn!(error = %error, file = %file.name, "{}", t("OneBot follow-up file download failed", "OneBot 后续消息文件下载失败"));
                if !content.is_empty() {
                    content.push('\n');
                }
                content.push_str(&format!(
                    "[{}: {}]",
                    t("file download failed", "文件接收失败"),
                    file.name
                ));
            }
        }
    }
    if content.is_empty() {
        if !attachments.is_empty() {
            content = image_only_prompt(attachments.len());
        } else if attempted_images > 0 {
            bail!("the follow-up image could not be downloaded");
        } else if parsed.at_self {
            content = t(
                "(they @-mentioned you without any text)",
                "（对方@了你，但没有其他内容）",
            )
            .to_string();
        } else {
            bail!("the follow-up message had no model-visible content");
        }
    }
    if failed_images > 0 {
        content.push_str(t(
            "\n(the message also contained an image that could not be downloaded; do not claim to have seen it)",
            "\n（消息还附带了未能下载的图片；不要声称已经看到了它）",
        ));
    }
    if quoted_images > 0 {
        content.push_str(&quoted_image_prompt(quoted_images));
    }
    let display_content = content.clone();
    content.push_str("\n\nQQ 后续消息可信元数据：");
    content.push_str(&format!(
        "发送者 QQ={}; 消息 ID={}",
        inbound_event.sender_id, inbound_event.message_id
    ));
    if let Some(reply) = inbound_event.replied_message.as_ref() {
        content.push_str(&format!(
            "; 回复消息 ID={}; 被回复者 QQ={}",
            reply.message_id, reply.sender_id
        ));
    }
    if !inbound_event.mentioned_user_ids.is_empty() {
        let mentions = if inbound_event.mentioned_users.is_empty() {
            inbound_event
                .mentioned_user_ids
                .iter()
                .map(|user_id| format!("QQ:{user_id}"))
                .collect::<Vec<_>>()
        } else {
            inbound_event
                .mentioned_users
                .iter()
                .map(|mention| match mention.display_name.as_deref() {
                    Some(name) => format!("{}(QQ:{})", qq_metadata_string(name), mention.user_id),
                    None => format!("QQ:{}", mention.user_id),
                })
                .collect::<Vec<_>>()
        };
        content.push_str(&format!("; @对象={}", mentions.join("、")));
    }

    context.observe_inbound(inbound_event).await;
    enqueue_turn_update(
        state,
        TurnUpdateRequest {
            run_id: run_id.to_string(),
            turn_id: turn_id.to_string(),
            session_id: Some(session_id.into()),
            audience: crate::config::PromptAudience::External,
            content,
            display_content,
            attachments,
            uploaded_attachment_ids: Vec::new(),
            mode,
        },
    )?;
    followup.context.accept_followup(inbound_event);
    Ok(())
}

pub(crate) async fn build_and_run_turn(
    state: &DaemonState,
    conn: &ConnectionHandle,
    target: Target,
    event: &Value,
    mut parsed: InboundMessage,
    context: Arc<PlatformTurnContext>,
    session_id: Arc<str>,
) -> Result<Option<TurnDispatch>> {
    if context.turn_is_superseded() {
        return Ok(None);
    }
    if !parsed.unresolved_image_files.is_empty() {
        resolve_current_message_images(conn, &mut parsed).await;
    }
    let current_message_id = event
        .get("message_id")
        .and_then(value_id_string)
        .unwrap_or_default();
    let quoted_message_data = parsed.quoted_message_data.take();
    let quoted_images = match merge_quoted_message_images(
        conn,
        &current_message_id,
        &mut parsed,
        quoted_message_data.as_ref(),
    )
    .await
    {
        Ok(added) => {
            if added > 0 {
                tracing::info!(
                    target: "gqy::qq",
                    quoted_message_id = parsed.reply_to_message_id.as_deref().unwrap_or_default(),
                    images = added,
                    "{}",
                    t("OneBot quoted-message images added to the model input", "OneBot 引用消息图片已加入模型输入")
                );
            }
            added
        }
        Err(error) => {
            tracing::warn!(
                target: "gqy::qq",
                error = %error,
                quoted_message_id = parsed.reply_to_message_id.as_deref().unwrap_or_default(),
                "{}",
                t("OneBot quoted-message lookup failed", "OneBot 引用消息查询失败")
            );
            0
        }
    };
    let mut content = parsed.text.trim().to_string();

    let prepared_images = prepare_inbound_images(state, parsed.images).await?;
    let attempted_images = prepared_images.attempted;
    let failed_images = prepared_images.failed;
    let images = prepared_images.attachments;
    if attempted_images > 0 {
        tracing::info!(
            target: "gqy::qq",
            attempted = attempted_images,
            prepared = images.len(),
            failed = failed_images,
            duplicates = prepared_images.duplicates,
            total_bytes = prepared_images.total_bytes,
            "{}",
            t("OneBot inbound images prepared for the model", "OneBot 传入图片已为模型准备完成")
        );
    }

    for file in &parsed.files {
        match fetch_inbound_file(state, conn, target, file).await {
            Ok(path) => {
                if !content.is_empty() {
                    content.push('\n');
                }
                content.push_str(&format!(
                    "[{} {} {} {}]",
                    t("the user sent a file", "用户发来文件"),
                    file.name,
                    t("saved at", "已保存于"),
                    path.display()
                ));
            }
            Err(error) => {
                tracing::warn!(error = %error, file = %file.name, "{}", t("OneBot file download failed", "OneBot 文件下载失败"));
                let _ = context
                    .send_bypass_plugins(OutboundMessage::text(
                        OutboundOrigin::Command,
                        format!(
                            "{}{}",
                            t("Couldn't fetch the file: ", "文件接收失败："),
                            file.name
                        ),
                    ))
                    .await;
            }
        }
    }

    if content.is_empty() {
        if !images.is_empty() {
            content = image_only_prompt(images.len());
        } else if attempted_images > 0 {
            context
                .send_bypass_plugins(OutboundMessage::text(
                    OutboundOrigin::Command,
                    t(
                        "I couldn't read that image. Please send it again.",
                        "图片接收失败了，请重新发送一次。",
                    ),
                ))
                .await?;
            return Ok(None);
        } else if parsed.at_self {
            content = t(
                "(they @-mentioned you without any text)",
                "（对方@了你，但没有其他内容）",
            )
            .to_string();
        } else {
            return Ok(None);
        }
    }
    if failed_images > 0 && !content.is_empty() {
        content.push_str(t(
            "\n(the message also contained an image that could not be downloaded; do not claim to have seen it)",
            "\n（消息还附带了未能下载的图片；不要声称已经看到了它）",
        ));
    }
    if quoted_images > 0 {
        content.push_str(&quoted_image_prompt(quoted_images));
    }

    if context.turn_is_superseded() {
        return Ok(None);
    }
    let prepared = context.prepare_turn(content).await;
    let content = prepared.content;
    let group_name = context
        .inbound_event()
        .and_then(|event| event.conversation_display_name.as_deref());
    let conversation_kind = match context.conversation.kind {
        ConversationKind::Private => crate::config::PlatformConversationKind::Private,
        ConversationKind::Group => crate::config::PlatformConversationKind::Group,
    };
    let route = context
        .config
        .platforms
        .model_route(conversation_kind, &context.conversation.conversation_id);
    // v7 Phase 2.1: the per-message transport block (sender identity JSON,
    // message ids, mentions) changes on every inbound message. It rides the
    // turn tail via `turn_system_context`; only stable policy text stays in the
    // system prompt so the provider prefix cache survives across messages.
    let mut turn_system_context = vec![qq_turn_system_context(
        &context.config.platforms.qq,
        &context.conversation,
        &context.sender_id,
        &context.sender_display_name,
        context.is_admin,
        context.inbound_event(),
        group_name,
    )];
    turn_system_context.extend(prepared.turn_system_context);
    let mut system_context = Vec::new();
    if let Some(prompt) = route
        .map(|route| route.extra_prompt.trim())
        .filter(|prompt| !prompt.is_empty())
    {
        system_context.push(format!("QQ 会话附加规则：\n{prompt}"));
    }
    system_context.extend(prepared.system_context);
    let profile = TurnProfile {
        active_persona: Some(context.config.prompt.active_persona.clone()),
        text_models: context.config.active_provider_models.clone(),
        multimodal_models: context
            .config
            .qq_multimodal_model_pool(conversation_kind, &context.conversation.conversation_id)
            .map(<[_]>::to_vec),
        system_context,
        turn_system_context,
        memory_content: Some(prepared.memory_content),
        context_images: prepared.context_images,
        image_cache_namespace: Some("qq".to_string()),
        image_source_label: Some("QQ".to_string()),
        memory_write_enabled: context.config.platforms.qq.memory.write_enabled,
        // Groups keep their own turn history now. The structured log still
        // carries who said what — the protocol offers no third role and drops
        // `name`, so identity can only live in the text — but the log is
        // additive: each turn appends what arrived since the last one, and
        // earlier turns replay verbatim. GQY's own turns become real
        // assistant messages instead of one `[你]` line in a rolling window.
        suppress_session_history: false,
        group_context: (context.conversation.kind == ConversationKind::Group)
            .then(|| context.config.platforms.qq.group_context.clone()),
        platform: Some(context),
        followup: None,
    };
    let dispatch = run_platform_turn(state, session_id, content, images, profile).await?;
    Ok(Some(dispatch))
}

pub(crate) fn image_only_prompt(count: usize) -> String {
    if crate::i18n::is_zh() {
        format!("（对方发送了 {count} 张图片。请查看图片内容并自然回应。）")
    } else if count == 1 {
        "(The user sent 1 image. Inspect it and respond naturally.)".to_string()
    } else {
        format!("(The user sent {count} images. Inspect them and respond naturally.)")
    }
}

pub(crate) fn quoted_image_prompt(count: usize) -> String {
    if crate::i18n::is_zh() {
        format!("\n（输入图片中有 {count} 张来自对方引用的消息。）")
    } else if count == 1 {
        "\n(1 input image came from the message the user quoted.)".to_string()
    } else {
        format!("\n({count} input images came from the message the user quoted.)")
    }
}

pub(crate) fn resolve_onebot_session(
    state: &DaemonState,
    context: &PlatformTurnContext,
    target: Target,
    event: &Value,
) -> Result<Arc<str>> {
    let session_name = session_name_for(target, event);
    let legacy_name = legacy_session_name_for(target);
    resolve_platform_session(
        state,
        &context.conversation,
        &context.config.active_persona_scope(),
        None,
        &session_name,
        Some(&legacy_name),
    )
}

/// Session-name key for this conversation. Group history is always shared by
/// the whole group; the bot account still isolates multiple QQ adapters.
pub(crate) fn session_name_for(target: Target, event: &Value) -> String {
    let self_id = event.get("self_id").and_then(Value::as_i64).unwrap_or(0);
    match target {
        Target::Private { user_id } => format!("qq:{self_id}:private:{user_id}"),
        Target::Group { group_id } => format!("qq:{self_id}:group:{group_id}"),
    }
}

pub(crate) fn legacy_session_name_for(target: Target) -> String {
    match target {
        Target::Private { user_id } => format!("qq:private:{user_id}"),
        Target::Group { group_id } => format!("qq:group:{group_id}"),
    }
}

/// Resolves a download URL for an inbound file (direct, or via the
/// NapCat file-URL APIs), downloads it capped and saves it under the
/// data dir. Returns the saved path.
pub(crate) async fn fetch_inbound_file(
    state: &DaemonState,
    conn: &ConnectionHandle,
    target: Target,
    file: &FileRef,
) -> Result<PathBuf> {
    let url = match &file.url {
        Some(url) => url.clone(),
        None => {
            let file_id = file
                .file_id
                .as_deref()
                .context("the file has no url and no file_id")?;
            let data = match target {
                Target::Group { group_id } => {
                    conn.call_api(
                        "get_group_file_url",
                        json!({ "file_id": file_id, "group_id": group_id }),
                    )
                    .await?
                }
                Target::Private { .. } => {
                    conn.call_api("get_private_file_url", json!({ "file_id": file_id }))
                        .await?
                }
            };
            data.get("url")
                .and_then(Value::as_str)
                .context("the file-url API returned no url")?
                .to_string()
        }
    };
    let _file_store_guard = state.platforms.file_store_lock.lock().await;
    ensure_platform_file_capacity(
        &state.paths.data_dir,
        MAX_INBOUND_FILE_BYTES as u64,
        PLATFORM_FILE_STORAGE_BYTES,
        PLATFORM_FILE_STORAGE_ENTRIES,
        PLATFORM_FILE_TTL,
    )
    .await?;
    let http = state.platforms.http_client()?;
    download_platform_file_capped(
        &http,
        &url,
        &state.paths.data_dir,
        &file.name,
        MAX_INBOUND_FILE_BYTES,
        FILE_DOWNLOAD_TIMEOUT,
    )
    .await
}

pub(crate) async fn ensure_platform_file_capacity(
    data_dir: &std::path::Path,
    reserve: u64,
    max_bytes: u64,
    max_entries: usize,
    ttl: Duration,
) -> Result<()> {
    let dir = data_dir.join("platform_files");
    tokio::fs::create_dir_all(&dir).await?;
    let mut entries = tokio::fs::read_dir(&dir).await?;
    let mut bytes = 0_u64;
    let mut count = 0usize;
    while let Some(entry) = entries.next_entry().await? {
        let metadata = match entry.metadata().await {
            Ok(metadata) if metadata.is_file() => metadata,
            _ => continue,
        };
        let expired = metadata
            .modified()
            .ok()
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age > ttl);
        if expired {
            let _ = tokio::fs::remove_file(entry.path()).await;
            continue;
        }
        bytes = bytes
            .checked_add(metadata.len())
            .context("platform file storage size overflow")?;
        count = count.saturating_add(1);
    }
    if count >= max_entries || bytes.saturating_add(reserve) > max_bytes {
        bail!("platform file storage quota is full");
    }
    Ok(())
}

pub(crate) async fn download_platform_file_capped(
    client: &reqwest::Client,
    url: &str,
    data_dir: &std::path::Path,
    name: &str,
    max_bytes: usize,
    timeout: Duration,
) -> Result<PathBuf> {
    let response = client
        .get(url)
        .timeout(timeout)
        .send()
        .await
        .with_context(|| format!("requesting {url}"))?
        .error_for_status()
        .with_context(|| format!("downloading {url}"))?;
    if response
        .content_length()
        .is_some_and(|length| length > max_bytes as u64)
    {
        bail!(
            "the file is larger than the {}MB limit",
            max_bytes / 1024 / 1024
        );
    }
    let (path, mut output) = create_platform_file(data_dir, name).await?;
    let result = async {
        let mut total = 0usize;
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.with_context(|| format!("reading {url}"))?;
            total = total
                .checked_add(chunk.len())
                .context("platform file size overflow")?;
            if total > max_bytes {
                bail!(
                    "the file is larger than the {}MB limit",
                    max_bytes / 1024 / 1024
                );
            }
            output.write_all(&chunk).await?;
        }
        output.flush().await?;
        Ok::<(), anyhow::Error>(())
    }
    .await;
    if let Err(error) = result {
        drop(output);
        let _ = tokio::fs::remove_file(&path).await;
        return Err(error);
    }
    Ok(path)
}

/// Saves inbound bytes under `<data_dir>/platform_files/`, keeping only
/// the basename (no path traversal) and suffixing on collision.
pub(crate) async fn save_platform_file(
    data_dir: &std::path::Path,
    name: &str,
    bytes: &[u8],
) -> Result<PathBuf> {
    let (path, mut output) = create_platform_file(data_dir, name).await?;
    if let Err(error) = output.write_all(bytes).await {
        drop(output);
        let _ = tokio::fs::remove_file(&path).await;
        return Err(error).context("writing the inbound platform file");
    }
    Ok(path)
}

pub(crate) async fn create_platform_file(
    data_dir: &std::path::Path,
    name: &str,
) -> Result<(PathBuf, tokio::fs::File)> {
    let dir = data_dir.join("platform_files");
    tokio::fs::create_dir_all(&dir).await?;
    let safe = sanitize_file_name(name);
    for counter in 0..=1000 {
        let path = std::path::Path::new(&safe);
        let stem = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("file");
        let file_name = match (counter, path.extension().and_then(|ext| ext.to_str())) {
            (0, _) => safe.clone(),
            (_, Some(ext)) => format!("{stem}-{counter}.{ext}"),
            (_, None) => format!("{stem}-{counter}"),
        };
        let candidate = dir.join(file_name);
        let output = match tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
            .await
        {
            Ok(output) => output,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error).context("creating the inbound platform file"),
        };
        return Ok((candidate, output));
    }
    bail!("too many files with the same name")
}

pub(crate) fn sanitize_file_name(name: &str) -> String {
    let base = name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("file")
        .replace(['\0', '\n', '\r'], "");
    let trimmed = base.trim();
    if trimmed.is_empty() || trimmed == "." || trimmed == ".." {
        return "file".to_string();
    }
    trimmed.chars().take(120).collect()
}

/// Group wake check. `Some(text)` = triggered, with any wake prefix
/// already stripped; `None` = stay silent.
pub(crate) fn group_trigger_text(
    config: &OneBotConfig,
    parsed: &InboundMessage,
    replied_message: Option<&PlatformMessageInfo>,
    self_id: i64,
) -> Option<String> {
    if parsed.at_self
        || replied_message
            .is_some_and(|message| message.sender_id.parse::<i64>().ok() == Some(self_id))
    {
        return Some(parsed.text.clone());
    }
    let text = parsed.text.trim_start();
    let keyword = config
        .group_chats
        .trigger_keywords
        .iter()
        .filter(|keyword| text.starts_with(keyword.as_str()))
        .max_by_key(|keyword| keyword.chars().count())?;
    let rest = &text[keyword.len()..];
    Some(
        rest.trim_start_matches(|ch: char| {
            ch.is_whitespace() || matches!(ch, ':' | '：' | ',' | '，')
        })
        .to_string(),
    )
}

pub(crate) fn decode_cq_text(text: &str) -> String {
    text.replace("&#91;", "[")
        .replace("&#93;", "]")
        .replace("&#44;", ",")
        .replace("&amp;", "&")
}

pub(crate) fn push_inbound_text(parsed: &mut InboundMessage, text: &str) {
    if parsed.rejected_reason.is_some() {
        return;
    }
    let remaining = MAX_INBOUND_TEXT_CHARS.saturating_sub(parsed.text_chars);
    let mut chars = text.chars();
    let before = parsed.text.len();
    parsed.text.extend(chars.by_ref().take(remaining));
    parsed.text_chars += parsed.text[before..].chars().count();
    if chars.next().is_some() {
        parsed.rejected_reason = Some("message text exceeds the 20,000 character limit");
    }
}

pub(crate) fn push_cq_text(parsed: &mut InboundMessage, text: &str) {
    if parsed.rejected_reason.is_some() {
        return;
    }
    let remaining = MAX_INBOUND_TEXT_CHARS.saturating_sub(parsed.text_chars);
    // The longest supported CQ entity is five characters for one decoded
    // character. Bound the temporary decode even when a raw frame is large.
    let raw_limit = remaining.saturating_mul(5).saturating_add(1);
    let bounded = text.chars().take(raw_limit).collect::<String>();
    push_inbound_text(parsed, &decode_cq_text(&bounded));
    if bounded.chars().count() == raw_limit && text.chars().nth(raw_limit).is_some() {
        parsed.rejected_reason = Some("message text exceeds the 20,000 character limit");
    }
}

pub(crate) fn bounded_onebot_id(value: impl Into<String>) -> Option<String> {
    let value = value.into();
    (!value.is_empty() && value.len() <= MAX_ONEBOT_ID_BYTES).then_some(value)
}

pub(crate) fn push_mention(parsed: &mut InboundMessage, qq: String) {
    if parsed.mentioned_user_ids.len() >= MAX_INBOUND_MENTIONS
        || qq.len() > MAX_ONEBOT_ID_BYTES
        || !qq.bytes().all(|byte| byte.is_ascii_digit())
        || qq == "0"
        || parsed.mentioned_user_ids.contains(&qq)
    {
        return;
    }
    parsed.mentioned_user_ids.push(qq);
}

pub(crate) fn bounded_chars(value: &str, maximum: usize) -> String {
    value.chars().take(maximum).collect()
}

pub(crate) fn push_image_ref_with_limits(
    images: &mut Vec<MediaRef>,
    candidate: MediaRef,
    maximum_images: usize,
    maximum_inline_bytes: usize,
) -> bool {
    if images
        .iter()
        .any(|existing| existing.same_source(&candidate))
    {
        return false;
    }
    if images.len() >= maximum_images {
        return false;
    }
    let candidate_bytes = candidate.inline_bytes();
    if candidate_bytes > MAX_INBOUND_IMAGE_BYTES
        || images
            .iter()
            .map(MediaRef::inline_bytes)
            .sum::<usize>()
            .saturating_add(candidate_bytes)
            > maximum_inline_bytes
    {
        return false;
    }
    images.push(candidate);
    true
}

pub(crate) fn push_inbound_base64(parsed: &mut InboundMessage, encoded: &str) -> bool {
    // Refuse before decoding once the shared count budget is full.
    if parsed.images.len() >= MAX_INBOUND_IMAGES {
        return false;
    }
    let encoded = encoded.strip_prefix("base64://").unwrap_or(encoded);
    let remaining = MAX_INBOUND_IMAGE_TOTAL_BYTES.saturating_sub(
        parsed
            .images
            .iter()
            .map(MediaRef::inline_bytes)
            .sum::<usize>(),
    );
    let maximum_decoded = MAX_INBOUND_IMAGE_BYTES.min(remaining);
    if maximum_decoded == 0 {
        return false;
    }
    let maximum_encoded = maximum_decoded
        .saturating_add(2)
        .div_ceil(3)
        .saturating_mul(4);
    if encoded.len() > maximum_encoded {
        return false;
    }
    let Ok(bytes) = BASE64.decode(encoded) else {
        return false;
    };
    if bytes.len() > maximum_decoded {
        return false;
    }
    push_image_ref_with_limits(
        &mut parsed.images,
        MediaRef::Bytes(bytes),
        MAX_INBOUND_IMAGES,
        MAX_INBOUND_IMAGE_TOTAL_BYTES,
    )
}

pub(crate) fn http_image_source<'a>(file: &'a str, url: Option<&'a str>) -> Option<&'a str> {
    url.filter(|url| {
        (url.starts_with("http://") || url.starts_with("https://")) && url.len() <= 4096
    })
    .or_else(|| {
        Some(file).filter(|file| {
            (file.starts_with("http://") || file.starts_with("https://")) && file.len() <= 4096
        })
    })
}

pub(crate) fn push_inbound_image_source(
    parsed: &mut InboundMessage,
    file: &str,
    url: Option<&str>,
) -> bool {
    if let Some(encoded) = file.strip_prefix("base64://") {
        return push_inbound_base64(parsed, encoded);
    }

    http_image_source(file, url).is_some_and(|source| {
        push_image_ref_with_limits(
            &mut parsed.images,
            MediaRef::Url(source.to_string()),
            MAX_INBOUND_IMAGES,
            MAX_INBOUND_IMAGE_TOTAL_BYTES,
        )
    })
}

pub(crate) fn push_unresolved_image_file(
    resolved_images: usize,
    unresolved: &mut Vec<String>,
    file: Option<String>,
) {
    if resolved_images.saturating_add(unresolved.len()) >= MAX_INBOUND_IMAGES {
        return;
    }
    let Some(file) = file else { return };
    let file = file.trim();
    if file.is_empty()
        || file.len() > 4096
        || file.starts_with("base64://")
        || file.starts_with("http://")
        || file.starts_with("https://")
        || unresolved.iter().any(|existing| existing == file)
    {
        return;
    }
    unresolved.push(file.to_string());
}

pub(crate) fn append_cq_image_sources(
    parsed: &mut InboundMessage,
    raw: &str,
    unresolved: &mut Vec<String>,
) {
    let mut remaining = raw;
    for _ in 0..MAX_INBOUND_SEGMENTS {
        let Some(start) = remaining.find("[CQ:") else {
            return;
        };
        let segment = &remaining[start + 4..];
        let Some(end) = segment.find(']') else {
            return;
        };
        let body = &segment[..end];
        let mut fields = body.split(',');
        if fields.next() == Some("image") {
            let parameters = fields
                .take(MAX_CQ_FIELDS)
                .filter_map(|field| field.split_once('='))
                .collect::<HashMap<_, _>>();
            let file = parameters
                .get("file")
                .map(|value| decode_cq_text(value))
                .unwrap_or_default();
            let url = parameters.get("url").map(|value| decode_cq_text(value));
            if http_image_source(&file, url.as_deref()).is_some() || file.starts_with("base64://") {
                push_inbound_image_source(parsed, &file, url.as_deref());
            } else {
                let file_id = parameters.get("file_id").map(|value| decode_cq_text(value));
                push_unresolved_image_file(
                    parsed.images.len(),
                    unresolved,
                    (!file.is_empty()).then_some(file).or(file_id),
                );
            }
        }
        if parsed.images.len().saturating_add(unresolved.len()) >= MAX_INBOUND_IMAGES {
            return;
        }
        remaining = &segment[end + 1..];
    }
}

pub(crate) fn append_message_image_sources(
    parsed: &mut InboundMessage,
    message: Option<&Value>,
    raw_message: Option<&Value>,
) -> Vec<String> {
    let mut unresolved = Vec::new();
    if let Some(Value::Array(segments)) = message {
        for segment in segments.iter().take(MAX_INBOUND_SEGMENTS) {
            if segment.get("type").and_then(Value::as_str) != Some("image") {
                continue;
            }
            let data = segment.get("data").unwrap_or(&Value::Null);
            let file = data.get("file").and_then(Value::as_str).unwrap_or("");
            let url = data.get("url").and_then(Value::as_str);
            if http_image_source(file, url).is_some() || file.starts_with("base64://") {
                push_inbound_image_source(parsed, file, url);
            } else {
                let file_id = data.get("file_id").and_then(value_id_string);
                push_unresolved_image_file(
                    parsed.images.len(),
                    &mut unresolved,
                    (!file.is_empty()).then(|| file.to_string()).or(file_id),
                );
            }
            if parsed.images.len().saturating_add(unresolved.len()) >= MAX_INBOUND_IMAGES {
                break;
            }
        }
        return unresolved;
    }
    if let Some(raw) = message
        .and_then(Value::as_str)
        .or_else(|| raw_message.and_then(Value::as_str))
    {
        append_cq_image_sources(parsed, raw, &mut unresolved);
    }
    unresolved
}

pub(crate) fn ordered_image_source(
    file: &str,
    url: Option<&str>,
) -> Option<OrderedMessageImageSource> {
    if let Some(encoded) = file.strip_prefix("base64://") {
        let maximum_encoded = MAX_INBOUND_IMAGE_BYTES
            .saturating_add(2)
            .div_ceil(3)
            .saturating_mul(4);
        if encoded.len() > maximum_encoded {
            return None;
        }
        let bytes = BASE64.decode(encoded).ok()?;
        return (bytes.len() <= MAX_INBOUND_IMAGE_BYTES)
            .then_some(OrderedMessageImageSource::Media(MediaRef::Bytes(bytes)));
    }
    if let Some(source) = http_image_source(file, url) {
        return Some(OrderedMessageImageSource::Media(MediaRef::Url(
            source.to_string(),
        )));
    }
    let file = file.trim();
    (!file.is_empty() && file.len() <= 4096)
        .then(|| OrderedMessageImageSource::File(file.to_string()))
}

pub(crate) fn ordered_message_image_sources(
    message: Option<&Value>,
    raw_message: Option<&Value>,
) -> Vec<OrderedMessageImageSource> {
    let mut sources = Vec::new();
    if let Some(Value::Array(segments)) = message {
        for segment in segments.iter().take(MAX_INBOUND_SEGMENTS) {
            if sources.len() >= MAX_INBOUND_IMAGES
                || segment.get("type").and_then(Value::as_str) != Some("image")
            {
                continue;
            }
            let data = segment.get("data").unwrap_or(&Value::Null);
            let file = data.get("file").and_then(Value::as_str).unwrap_or_default();
            let file_id = data.get("file_id").and_then(value_id_string);
            if let Some(source) = ordered_image_source(
                if file.is_empty() {
                    file_id.as_deref().unwrap_or_default()
                } else {
                    file
                },
                data.get("url").and_then(Value::as_str),
            ) {
                sources.push(source);
            }
        }
        return sources;
    }

    let Some(raw) = message
        .and_then(Value::as_str)
        .or_else(|| raw_message.and_then(Value::as_str))
    else {
        return sources;
    };
    let mut remaining = raw;
    for _ in 0..MAX_INBOUND_SEGMENTS {
        let Some(start) = remaining.find("[CQ:") else {
            break;
        };
        let segment = &remaining[start + 4..];
        let Some(end) = segment.find(']') else {
            break;
        };
        let body = &segment[..end];
        let mut fields = body.split(',');
        if fields.next() == Some("image") && sources.len() < MAX_INBOUND_IMAGES {
            let parameters = fields
                .take(MAX_CQ_FIELDS)
                .filter_map(|field| field.split_once('='))
                .collect::<HashMap<_, _>>();
            let file = parameters
                .get("file")
                .or_else(|| parameters.get("file_id"))
                .map(|value| decode_cq_text(value))
                .unwrap_or_default();
            let url = parameters.get("url").map(|value| decode_cq_text(value));
            if let Some(source) = ordered_image_source(&file, url.as_deref()) {
                sources.push(source);
            }
        }
        remaining = &segment[end + 1..];
    }
    sources
}
