//! config — 自 src/web.rs 拆分。

use super::*;

pub(crate) async fn get_config(
    State(state): State<DaemonState>,
    headers: HeaderMap,
) -> std::result::Result<Response, ApiError> {
    require_auth(&headers, &state)?;
    let (config, context) = {
        let manager = state.manager.lock().unwrap();
        (manager.config.clone(), manager.context)
    };
    let mut response = Json(config_response(&config, context, &state.paths)?).into_response();
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

pub(crate) async fn update_config(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Json(request): Json<UpdateConfigRequest>,
) -> std::result::Result<Json<ConfigResponse>, ApiError> {
    require_mutation(&headers, &state)?;

    let current = state.manager.lock().unwrap().config.clone();
    let current_prompts =
        read_prompt_documents(&current, &state.paths).map_err(ApiError::internal)?;
    let mut candidate: AppConfig = serde_json::from_value(request.config).map_err(|error| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            format!("invalid configuration: {}", safe_error_message(error)),
        )
    })?;
    reconcile_qq_persona_references(&mut candidate, &request.prompts);
    candidate.normalize_platform_model_routes();
    restore_config_secrets(&mut candidate, &current, &request.secrets)?;
    validate_config_candidate(&candidate)?;
    validate_prompt_documents(&candidate, &request.prompts)?;
    let qq_listener = state
        .platforms
        .qq_listener
        .prepare(&state, Some(&current.platforms.qq), &candidate.platforms.qq)
        .await
        .map_err(|error| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                format!(
                    "Tencent QQ listener configuration failed: {}",
                    safe_error_message(error)
                ),
            )
        })?;
    let requested_prompts = request.prompts.clone();
    // Allowed while turns run: the ApplyConfig handler interrupts running
    // turns only for persona layout changes; everything else hot-applies.
    reserve_admin_light(&state.manager)?;
    let (reply, receiver) = oneshot::channel();
    if state
        .actor_tx
        .send(ActorCommand::ApplyConfig {
            config: Box::new(candidate),
            prompts: request.prompts,
            reset_conversation: false,
            reply,
        })
        .is_err()
    {
        release_admin(&state.manager);
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "agent worker is unavailable",
        ));
    }
    match receiver.await {
        Ok(Ok(())) => qq_listener.commit(),
        Ok(Err(AdminFailure::Invalid(message))) => {
            return Err(ApiError::new(StatusCode::BAD_REQUEST, message));
        }
        Ok(Err(AdminFailure::Internal(message))) => {
            tracing::error!(
                error = %message,
                "{}",
                t("WebUI configuration update failed", "WebUI 配置更新失败")
            );
            return Err(ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                safe_error_message(&message),
            ));
        }
        Err(_) => {
            release_admin(&state.manager);
            return Err(ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "agent worker stopped before updating the configuration",
            ));
        }
    }
    cleanup_persona_assets(&state.paths, &current_prompts, &requested_prompts);
    let manager = state.manager.lock().unwrap();
    Ok(Json(config_response(
        &manager.config,
        manager.context,
        &state.paths,
    )?))
}

pub(crate) fn cleanup_persona_assets(
    paths: &GQYPaths,
    previous: &PromptDocuments,
    current: &PromptDocuments,
) {
    let directory = paths.persona_avatars_dir();
    let Ok(entries) = std::fs::read_dir(&directory) else {
        return;
    };
    let referenced = |prompts: &PromptDocuments| {
        prompts
            .personas
            .iter()
            .flat_map(|document| {
                [
                    document.avatar_path.as_deref(),
                    document.board_image_path.as_deref(),
                ]
            })
            .flatten()
            .filter_map(|path| resolve_persona_asset_path(paths, path))
            .filter_map(|path| {
                path.strip_prefix(&directory)
                    .ok()
                    .map(|relative| relative.to_string_lossy().to_string())
            })
            .collect::<HashSet<_>>()
    };
    let previous = referenced(previous);
    let current = referenced(current);
    let now = std::time::SystemTime::now();
    for entry in entries.flatten() {
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(_) => continue,
        };
        if !file_type.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        let stale = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age >= std::time::Duration::from_secs(24 * 60 * 60));
        if name.starts_with(".upload-") {
            if stale {
                let _ = std::fs::remove_file(entry.path());
            }
            continue;
        }
        let bytes = name.as_bytes();
        let managed_name = bytes.len() >= 68
            && bytes[64] == b'.'
            && bytes[..64].iter().all(u8::is_ascii_hexdigit)
            && matches!(&bytes[65..], b"png" | b"jpg" | b"gif" | b"webp" | b"bmp");
        if !managed_name || current.contains(&name) {
            continue;
        }
        let old_reference = previous.contains(&name);
        if old_reference || stale {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

pub(crate) async fn image_asset(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Path(asset_id): Path<String>,
) -> std::result::Result<Response, ApiError> {
    require_auth(&headers, &state)?;
    if asset_id.len() > 96
        || asset_id.is_empty()
        || !asset_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
    {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "image asset not found",
        ));
    }
    let Some(asset) = state
        .state_store
        .load_image_asset(&asset_id)
        .map_err(ApiError::internal)?
    else {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "image asset not found",
        ));
    };
    let mut response = asset.bytes.into_response();
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_str(&asset.asset.mime).map_err(ApiError::internal)?,
    );
    response.headers_mut().insert(
        CACHE_CONTROL,
        HeaderValue::from_static("private, max-age=86400"),
    );
    response
        .headers_mut()
        .insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    Ok(response)
}

pub(crate) async fn artifact_asset(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Path(asset_id): Path<String>,
) -> std::result::Result<Response, ApiError> {
    require_auth(&headers, &state)?;
    if asset_id.len() > 96
        || asset_id.is_empty()
        || !asset_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
    {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "artifact not found"));
    }
    let Some(artifact) = state
        .state_store
        .load_artifact_asset(&asset_id)
        .map_err(ApiError::internal)?
    else {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "artifact not found"));
    };
    let inline = matches!(
        artifact.asset.kind.as_str(),
        "markdown" | "text" | "code" | "json" | "pdf" | "html"
    );
    let disposition = format!(
        "{}; filename*=UTF-8''{}",
        if inline { "inline" } else { "attachment" },
        urlencoding::encode(&artifact.asset.file_name)
    );
    let mut response = artifact.bytes.into_response();
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_str(&artifact.asset.mime).map_err(ApiError::internal)?,
    );
    response.headers_mut().insert(
        CONTENT_DISPOSITION,
        HeaderValue::from_str(&disposition).map_err(ApiError::internal)?,
    );
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("private, no-cache"));
    response
        .headers_mut()
        .insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    if artifact.asset.kind == "html" {
        response.headers_mut().insert(
            CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(
                "sandbox; default-src 'none'; style-src 'unsafe-inline'; img-src data: blob:",
            ),
        );
    }
    Ok(response)
}

pub(crate) async fn upload_user_attachment(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Query(query): Query<AttachmentQuery>,
    body: Bytes,
) -> std::result::Result<Json<SafeUserAttachment>, ApiError> {
    require_mutation(&headers, &state)?;
    let session_id =
        resolve_turn_session(&state, Some(query.session_id)).map_err(session_api_error)?;
    if body.is_empty() || body.len() > ATTACHMENT_BODY_LIMIT {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "attachment must be between 1 byte and 10 MiB",
        ));
    }
    let encoded_name = headers
        .get("x-gqy-filename")
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| ApiError::new(StatusCode::BAD_REQUEST, "attachment filename is required"))?;
    let decoded_name = urlencoding::decode(encoded_name)
        .map_err(|_| ApiError::new(StatusCode::BAD_REQUEST, "attachment filename is invalid"))?;
    let file_name = sanitize_attachment_file_name(&decoded_name)?;
    let (kind, mime, width, height) = inspect_user_attachment(&file_name, &body)?;
    let attachment = UserAttachment {
        attachment_id: random_id("att", 24),
        file_name,
        mime,
        kind,
        size_bytes: body.len() as u64,
        width,
        height,
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    let store = state.state_store.pinned(&session_id);
    store
        .purge_stale_user_attachments()
        .map_err(ApiError::internal)?;
    store
        .save_user_attachment(&attachment, &body)
        .map_err(ApiError::internal)?;
    Ok(Json(SafeUserAttachment::from(attachment)))
}

pub(crate) async fn user_attachment(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Path(attachment_id): Path<String>,
) -> std::result::Result<Response, ApiError> {
    require_auth(&headers, &state)?;
    validate_attachment_id(&attachment_id)?;
    let Some(attachment) = state
        .state_store
        .load_user_attachment_by_id(&attachment_id)
        .map_err(ApiError::internal)?
    else {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "attachment not found"));
    };
    let inline = attachment.attachment.kind == "image";
    let mut response = attachment.bytes.into_response();
    let content_type = if inline {
        attachment.attachment.mime.as_str()
    } else {
        "application/octet-stream"
    };
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_str(content_type).map_err(ApiError::internal)?,
    );
    response.headers_mut().insert(
        CONTENT_LENGTH,
        HeaderValue::from_str(&attachment.attachment.size_bytes.to_string())
            .map_err(ApiError::internal)?,
    );
    response.headers_mut().insert(
        CONTENT_DISPOSITION,
        attachment_content_disposition(&attachment.attachment.file_name, inline)?,
    );
    response.headers_mut().insert(
        CACHE_CONTROL,
        HeaderValue::from_static("private, max-age=86400"),
    );
    response
        .headers_mut()
        .insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    Ok(response)
}

pub(crate) async fn delete_user_attachment(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Query(query): Query<AttachmentQuery>,
    Path(attachment_id): Path<String>,
) -> std::result::Result<StatusCode, ApiError> {
    require_mutation(&headers, &state)?;
    validate_attachment_id(&attachment_id)?;
    let session_id =
        resolve_turn_session(&state, Some(query.session_id)).map_err(session_api_error)?;
    let deleted = state
        .state_store
        .pinned(&session_id)
        .delete_staged_user_attachment(&attachment_id)
        .map_err(ApiError::internal)?;
    if !deleted {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "attachment not found"));
    }
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) fn validate_attachment_id(attachment_id: &str) -> std::result::Result<(), ApiError> {
    if attachment_id.len() <= 96
        && !attachment_id.is_empty()
        && attachment_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
    {
        return Ok(());
    }
    Err(ApiError::new(StatusCode::NOT_FOUND, "attachment not found"))
}

pub(crate) fn sanitize_attachment_file_name(value: &str) -> std::result::Result<String, ApiError> {
    let name = value
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("")
        .trim()
        .chars()
        .filter(|character| !character.is_control())
        .take(180)
        .collect::<String>();
    if name.is_empty() || name == "." || name == ".." {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "attachment filename is invalid",
        ));
    }
    Ok(name)
}

pub(crate) fn inspect_user_attachment(
    file_name: &str,
    bytes: &[u8],
) -> std::result::Result<(String, String, u32, u32), ApiError> {
    if let Ok(reader) = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format() {
        if let Some(format) = reader.format() {
            if matches!(
                format,
                image::ImageFormat::Png
                    | image::ImageFormat::Jpeg
                    | image::ImageFormat::WebP
                    | image::ImageFormat::Gif
            ) {
                let (width, height) = reader.into_dimensions().map_err(|_| {
                    ApiError::new(StatusCode::BAD_REQUEST, "attachment image is invalid")
                })?;
                if width == 0
                    || height == 0
                    || width > 40_000
                    || height > 40_000
                    || u64::from(width) * u64::from(height) > 40_000_000
                {
                    return Err(ApiError::new(
                        StatusCode::BAD_REQUEST,
                        "attachment image dimensions are outside the safety limit",
                    ));
                }
                return Ok((
                    "image".to_string(),
                    format.to_mime_type().to_string(),
                    width,
                    height,
                ));
            }
        }
    }
    if bytes.len() > MAX_TEXT_ATTACHMENT_BYTES {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "text attachment exceeds the 1 MiB limit",
        ));
    }
    std::str::from_utf8(bytes).map_err(|_| {
        ApiError::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "attachment is not UTF-8 text",
        )
    })?;
    let extension = FilePath::new(file_name)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    pub(crate) const TEXT_EXTENSIONS: &[&str] = &[
        "txt", "md", "markdown", "json", "jsonl", "csv", "tsv", "log", "rs", "js", "jsx", "ts",
        "tsx", "py", "go", "java", "c", "cc", "cpp", "h", "hpp", "cs", "rb", "php", "swift", "kt",
        "kts", "sh", "bash", "zsh", "fish", "toml", "yaml", "yml", "xml", "html", "css", "scss",
        "sql",
    ];
    if !TEXT_EXTENSIONS.contains(&extension.as_str()) {
        return Err(ApiError::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported attachment type",
        ));
    }
    let mime = match extension.as_str() {
        "md" | "markdown" => "text/markdown",
        "json" | "jsonl" => "application/json",
        "csv" => "text/csv",
        "html" => "text/html",
        "css" => "text/css",
        _ => "text/plain",
    };
    Ok(("text".to_string(), mime.to_string(), 0, 0))
}

pub(crate) fn attachment_content_disposition(
    file_name: &str,
    inline: bool,
) -> std::result::Result<HeaderValue, ApiError> {
    let fallback = file_name
        .chars()
        .filter(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_')
        })
        .take(80)
        .collect::<String>();
    let fallback = if fallback.is_empty() {
        "attachment"
    } else {
        &fallback
    };
    let disposition = if inline { "inline" } else { "attachment" };
    let value = format!(
        "{disposition}; filename=\"{fallback}\"; filename*=UTF-8''{}",
        urlencoding::encode(file_name)
    );
    HeaderValue::from_str(&value).map_err(ApiError::internal)
}

pub(crate) async fn events(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Query(query): Query<EventsQuery>,
) -> std::result::Result<Sse<impl Stream<Item = std::result::Result<Event, Infallible>>>, ApiError>
{
    require_auth(&headers, &state)?;
    let header_after = headers
        .get("last-event-id")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
    let after = query.after.max(header_after);
    let subscription = state.events.subscribe_after(after);
    let stream_state = SseStreamState {
        pending: subscription.pending,
        receiver: subscription.receiver,
        events: state.events,
        last_id: after,
    };
    let events = stream::unfold(stream_state, |mut state| async move {
        loop {
            if let Some(record) = state.pending.pop_front() {
                if record.kind == "resync_required" {
                    state.last_id = record.id;
                    return Some((Ok(record_to_sse(record)), state));
                }
                if record.id <= state.last_id {
                    continue;
                }
                state.last_id = record.id;
                return Some((Ok(record_to_sse(record)), state));
            }
            match state.receiver.recv().await {
                Ok(record) if record.id > state.last_id => {
                    state.last_id = record.id;
                    return Some((Ok(record_to_sse(record)), state));
                }
                Ok(_) => {}
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    state.pending = state.events.replay_after(state.last_id);
                }
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    });
    let ready =
        stream::once(async { Ok::<Event, Infallible>(Event::default().comment("connected")) });
    let stream = ready.chain(events);
    Ok(Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keepalive"),
    ))
}

pub(crate) struct SseStreamState {
    pending: VecDeque<EventRecord>,
    receiver: broadcast::Receiver<EventRecord>,
    events: EventHub,
    last_id: u64,
}

pub(crate) fn record_to_sse(record: EventRecord) -> Event {
    Event::default()
        .id(record.id.to_string())
        .event(record.kind)
        .data(record.data)
}

pub(crate) fn enqueue_turn_update(
    state: &DaemonState,
    request: TurnUpdateRequest,
) -> Result<TurnUpdateReceipt> {
    let manager = state.manager.lock().unwrap();
    if manager.admin_busy {
        bail!("{}", ipc::ADMIN_BUSY_MESSAGE);
    }
    let run = manager
        .active_runs
        .get(&request.run_id)
        .context("active run not found")?;
    if run.audience != request.audience {
        bail!("the active reply belongs to a different request source");
    }
    if request
        .session_id
        .as_deref()
        .is_some_and(|session_id| session_id != &*run.session_id)
    {
        bail!("the active reply belongs to a different conversation");
    }
    if run.turn_id.as_deref() != Some(request.turn_id.as_str()) {
        bail!("the active run no longer owns the requested turn");
    }
    let target = run
        .queue_target
        .clone()
        .context("the active turn is not ready to accept follow-up messages")?;
    if target.turn_id != request.turn_id {
        bail!("the active run queue target changed");
    }
    let session_id = run.session_id.clone();
    let supersede = run.supersede.clone();
    let prompt_id = random_id("queued", 18);
    let store = state.state_store.pinned(&session_id);
    store.recover_stale_turns()?;
    let prompt = store.enqueue_prompt_for_target_with_uploads(
        &target,
        &prompt_id,
        &request.content,
        &request.display_content,
        &request.attachments,
        &request.uploaded_attachment_ids,
    )?;
    if request.mode == TurnUpdateMode::Supersede {
        supersede.trigger();
    }
    state.events.publish(
        "queue.added",
        json!({
            "session_id": &*session_id,
            "run_id": request.run_id,
            "turn_id": request.turn_id,
            "mode": match request.mode {
                TurnUpdateMode::Followup => "followup",
                TurnUpdateMode::Supersede => "supersede",
            },
            "prompt": SafeQueuedPrompt::from(prompt.clone()),
        }),
    );
    Ok(TurnUpdateReceipt {
        run_id: request.run_id,
        turn_id: request.turn_id,
        session_id,
        prompt,
    })
}

pub(crate) struct PreparedWebAttachments {
    content: String,
    images: Vec<Option<ImageAttachment>>,
}

pub(crate) struct RedoWebPrompt {
    pub(crate) prompt_id: String,
    pub(crate) content: String,
    pub(crate) display_content: String,
    pub(crate) images: Vec<Option<ImageAttachment>>,
}

pub(crate) fn prepare_web_attachments(
    store: &StateStore,
    display_content: &str,
    attachment_ids: &[String],
) -> std::result::Result<PreparedWebAttachments, ApiError> {
    if attachment_ids.len() > MAX_ATTACHMENTS_PER_MESSAGE {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            format!("a message can include at most {MAX_ATTACHMENTS_PER_MESSAGE} attachments"),
        ));
    }
    let unique = attachment_ids.iter().collect::<HashSet<_>>();
    if unique.len() != attachment_ids.len()
        || attachment_ids
            .iter()
            .any(|id| validate_attachment_id(id).is_err())
    {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "attachment ids are invalid",
        ));
    }
    let attachments = store
        .load_staged_user_attachments(attachment_ids)
        .map_err(|error| ApiError::new(StatusCode::BAD_REQUEST, error.to_string()))?;
    prepare_web_attachment_data(display_content, attachments)
}

pub(crate) fn prepare_web_attachment_data(
    display_content: &str,
    attachments: Vec<crate::state::UserAttachmentData>,
) -> std::result::Result<PreparedWebAttachments, ApiError> {
    if attachments.len() > MAX_ATTACHMENTS_PER_MESSAGE {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            format!("a message can include at most {MAX_ATTACHMENTS_PER_MESSAGE} attachments"),
        ));
    }
    let total_bytes = attachments
        .iter()
        .map(|attachment| attachment.attachment.size_bytes)
        .sum::<u64>();
    if total_bytes > MAX_ATTACHMENT_TOTAL_BYTES {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "attachments exceed the 32 MiB per-message limit",
        ));
    }
    let mut content = if display_content.is_empty() {
        "请查看附件。".to_string()
    } else {
        display_content.to_string()
    };
    let mut images = Vec::new();
    for attachment in attachments {
        if attachment.attachment.kind == "image" {
            images.push(Some(ImageAttachment::Binary {
                mime: attachment.attachment.mime,
                data: attachment.bytes,
            }));
            continue;
        }
        let text = std::str::from_utf8(&attachment.bytes)
            .map_err(|_| ApiError::new(StatusCode::BAD_REQUEST, "text attachment is not UTF-8"))?;
        let name = escape_attachment_attribute(&attachment.attachment.file_name);
        let mime = escape_attachment_attribute(&attachment.attachment.mime);
        content.push_str(&format!(
            "\n\n<user-attachment name=\"{name}\" mime=\"{mime}\">\n{text}\n</user-attachment>"
        ));
    }
    Ok(PreparedWebAttachments { content, images })
}

pub(crate) fn escape_attachment_attribute(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

pub(crate) fn validate_message_content(
    content: String,
    has_attachments: bool,
) -> std::result::Result<String, ApiError> {
    if content.trim().is_empty() && has_attachments {
        return Ok(String::new());
    }
    validate_content(content)
}

pub(crate) async fn redo_turn(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Path((session_id, turn_id)): Path<(String, String)>,
    Json(request): Json<RedoTurnRequest>,
) -> std::result::Result<Response, ApiError> {
    require_mutation(&headers, &state)?;
    require_local_web_session(&state, &session_id)?;
    let mode = parse_mode(request.mode.as_deref().unwrap_or("normal"))?;
    let store = state.state_store.pinned_for_turn(&session_id);
    let candidate = store
        .redo_candidate()
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::new(StatusCode::CONFLICT, "the last input cannot be redone"))?;
    if candidate.turn_id != turn_id
        || candidate.input_id != request.input_id
        || candidate.revision != request.expected_revision
    {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "the conversation changed before redo could start",
        ));
    }

    let mut prompts = Vec::new();
    match candidate.input_kind {
        crate::state::RedoInputKind::Initial => {
            let attachments = store
                .load_user_attachment_data_for_turn(&turn_id)
                .map_err(ApiError::internal)?;
            let display_content = validate_message_content(
                request
                    .content
                    .unwrap_or_else(|| candidate.display_content.clone()),
                !attachments.is_empty(),
            )?;
            let prepared = prepare_web_attachment_data(&display_content, attachments)?;
            prompts.push(RedoWebPrompt {
                prompt_id: candidate.input_id.clone(),
                content: prepared.content,
                display_content,
                images: prepared.images,
            });
        }
        crate::state::RedoInputKind::Followup => {
            let batch = store
                .load_redo_batch_prompts(&turn_id, &candidate.batch_prompt_ids)
                .map_err(ApiError::internal)?;
            for prompt in batch {
                if !prompt.attachments.is_empty() {
                    return Err(ApiError::new(
                        StatusCode::CONFLICT,
                        "this follow-up uses non-durable attachments and cannot be redone",
                    ));
                }
                let attachments = store
                    .load_user_attachment_data_for_prompt(&prompt.prompt_id)
                    .map_err(ApiError::internal)?;
                let display_content = if prompt.prompt_id == candidate.input_id {
                    validate_message_content(
                        request
                            .content
                            .clone()
                            .unwrap_or_else(|| prompt.display_content.clone()),
                        !attachments.is_empty(),
                    )?
                } else {
                    prompt.display_content
                };
                let prepared = prepare_web_attachment_data(&display_content, attachments)?;
                prompts.push(RedoWebPrompt {
                    prompt_id: prompt.prompt_id,
                    content: prepared.content,
                    display_content,
                    images: prepared.images,
                });
            }
        }
    }

    let run_id = random_id("run", 18);
    let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
    {
        let mut manager = state.manager.lock().unwrap();
        if manager.admin_busy || manager.session_has_runs(&session_id) {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "GQY is busy in this conversation",
            ));
        }
        manager.active_runs.insert(
            run_id.clone(),
            RunInfo {
                session_id: session_id.clone().into(),
                mode,
                audience: PromptAudience::External,
                cancel: cancel_tx,
                turn_id: Some(turn_id.clone()),
                queue_target: None,
                supersede: Arc::new(crate::agent::TurnSupersedeSignal::default()),
                platform_followup: None,
                operation: RunOperation::Redo {
                    turn_id: turn_id.clone(),
                    input_id: candidate.input_id.clone(),
                },
                job_wake: false,
                turn_origin: crate::tools::workspace::TurnOrigin::Human,
                job_wake_label: None,
            },
        );
    }
    if state
        .actor_tx
        .send(ActorCommand::RedoTurn {
            run_id: run_id.clone(),
            session_id: session_id.into(),
            candidate,
            prompts,
            mode,
            cancel: cancel_rx,
        })
        .is_err()
    {
        finish_run(&state.manager, &run_id, None);
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "agent worker is unavailable",
        ));
    }
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({
            "run_id": run_id,
            "turn_id": turn_id,
            "operation": "redo",
        })),
    )
        .into_response())
}

pub(crate) fn unique_run_target(
    manager: &ManagerState,
    session_id: &str,
    audience: PromptAudience,
) -> Option<(String, String)> {
    let mut runs = manager.active_runs.iter().filter(|(_, run)| {
        &*run.session_id == session_id && run.audience == audience && run.turn_id.is_some()
    });
    let (run_id, run) = runs.next()?;
    if runs.next().is_some() {
        return None;
    }
    Some((run_id.clone(), run.turn_id.clone()?))
}

pub(crate) async fn create_turn(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Json(request): Json<CreateTurnRequest>,
) -> std::result::Result<Response, ApiError> {
    require_mutation(&headers, &state)?;
    let attachment_ids = request.attachment_ids;
    let display_content = validate_message_content(request.content, !attachment_ids.is_empty())?;
    let mode = parse_mode(request.mode.as_deref().unwrap_or("normal"))?;
    let session_id = resolve_turn_session(&state, request.session_id).map_err(session_api_error)?;
    state
        .state_store
        .recover_stale_turns()
        .map_err(ApiError::internal)?;
    // A running turn in the *target* session gets the message as a queued
    // follow-up (composer tray UX); other sessions run in parallel.
    let target_store = state.state_store.pinned(&session_id);
    let prepared = prepare_web_attachments(&target_store, &display_content, &attachment_ids)?;
    if target_store
        .has_running_turns()
        .map_err(ApiError::internal)?
        && state
            .manager
            .lock()
            .unwrap()
            .session_runs_match_audience(&session_id, PromptAudience::External)
    {
        let (run_id, turn_id) = unique_run_target(
            &state.manager.lock().unwrap(),
            &session_id,
            PromptAudience::External,
        )
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::CONFLICT,
                "the running turn is not ready or is ambiguous",
            )
        })?;
        let receipt = enqueue_turn_update(
            &state,
            TurnUpdateRequest {
                run_id: run_id.clone(),
                turn_id: turn_id.clone(),
                session_id: Some(session_id.clone()),
                audience: PromptAudience::External,
                content: prepared.content,
                display_content,
                attachments: Vec::new(),
                uploaded_attachment_ids: attachment_ids,
                mode: TurnUpdateMode::Followup,
            },
        )
        .map_err(|error| ApiError::new(StatusCode::CONFLICT, error.to_string()))?;
        let prompt = SafeQueuedPrompt::from(receipt.prompt);
        return Ok((
            StatusCode::ACCEPTED,
            Json(json!({
                "queued": true,
                "prompt": prompt,
                "run_id": receipt.run_id,
                "running_turn_id": receipt.turn_id,
            })),
        )
            .into_response());
    }
    let run_id = random_id("run", 18);
    let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
    {
        let mut manager = state.manager.lock().unwrap();
        if manager.admin_busy || manager.session_has_runs(&session_id) {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "GQY is busy in this conversation",
            ));
        }
        manager.active_runs.insert(
            run_id.clone(),
            RunInfo {
                session_id: session_id.clone(),
                mode,
                audience: PromptAudience::External,
                cancel: cancel_tx,
                turn_id: None,
                queue_target: None,
                supersede: Arc::new(crate::agent::TurnSupersedeSignal::default()),
                platform_followup: None,
                operation: RunOperation::Create,
                job_wake: false,
                turn_origin: crate::tools::workspace::TurnOrigin::Human,
                job_wake_label: None,
            },
        );
    }
    if let Err(error) = target_store.reserve_user_attachments(&attachment_ids, &run_id) {
        finish_run(&state.manager, &run_id, None);
        return Err(ApiError::new(StatusCode::BAD_REQUEST, error.to_string()));
    }
    if state
        .actor_tx
        .send(ActorCommand::StartTurn {
            run_id: run_id.clone(),
            session_id,
            display_content,
            content: prepared.content,
            attachment_run_id: (!attachment_ids.is_empty()).then_some(run_id.clone()),
            mode,
            images: prepared.images,
            cwd: None,
            origin_tty: None,
            audience: PromptAudience::External,
            profile: None,
            cancel: cancel_rx,
            turn_origin: Box::new(crate::tools::workspace::TurnOrigin::Human),
        })
        .is_err()
    {
        let _ = target_store.release_user_attachments_for_run(&run_id);
        finish_run(&state.manager, &run_id, None);
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "agent worker is unavailable",
        ));
    }
    Ok((StatusCode::ACCEPTED, Json(json!({ "run_id": run_id }))).into_response())
}

pub(crate) async fn queue_prompt(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Json(request): Json<QueuePromptRequest>,
) -> std::result::Result<Response, ApiError> {
    require_mutation(&headers, &state)?;
    let attachment_ids = request.attachment_ids;
    let display_content = validate_message_content(request.content, !attachment_ids.is_empty())?;
    let session_id = resolve_turn_session(&state, request.session_id).map_err(session_api_error)?;
    let store = state.state_store.pinned(&session_id);
    let prepared = prepare_web_attachments(&store, &display_content, &attachment_ids)?;
    let receipt = enqueue_turn_update(
        &state,
        TurnUpdateRequest {
            run_id: request.run_id,
            turn_id: request.turn_id,
            session_id: Some(session_id),
            audience: PromptAudience::External,
            content: prepared.content,
            display_content,
            attachments: Vec::new(),
            uploaded_attachment_ids: attachment_ids,
            mode: TurnUpdateMode::Followup,
        },
    )
    .map_err(|error| ApiError::new(StatusCode::CONFLICT, error.to_string()))?;
    let safe = SafeQueuedPrompt::from(receipt.prompt);
    Ok((StatusCode::ACCEPTED, Json(safe)).into_response())
}

pub(crate) async fn remove_queue_prompt(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Path((run_id, turn_id, prompt_id)): Path<(String, String, String)>,
) -> std::result::Result<StatusCode, ApiError> {
    require_mutation(&headers, &state)?;
    if prompt_id.len() > 96
        || prompt_id.is_empty()
        || !prompt_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
    {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "queued prompt not found",
        ));
    }
    let manager = state.manager.lock().unwrap();
    let run = manager
        .active_runs
        .get(&run_id)
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "queued prompt target not found"))?;
    if run.audience != PromptAudience::External || run.turn_id.as_deref() != Some(&turn_id) {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "queued prompt target not found",
        ));
    }
    let target = run
        .queue_target
        .clone()
        .ok_or_else(|| ApiError::new(StatusCode::CONFLICT, "the active turn is not ready"))?;
    let session_id = run.session_id.clone();
    drop(manager);
    let removed = state
        .state_store
        .pinned(&session_id)
        .remove_queued_prompt_for_target(&target, &prompt_id)
        .map_err(ApiError::internal)?;
    if !removed {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "queued prompt not found",
        ));
    };
    state.events.publish(
        "queue.removed",
        json!({
            "session_id": &*session_id,
            "run_id": run_id,
            "turn_id": turn_id,
            "prompt_id": prompt_id,
        }),
    );
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) async fn list_jobs_http(
    State(state): State<DaemonState>,
    headers: HeaderMap,
) -> std::result::Result<Response, ApiError> {
    require_auth(&headers, &state)?;
    Ok(Json(json!({ "jobs": tools::jobs::overview() })).into_response())
}

#[derive(Deserialize)]
pub(crate) struct UsageStatsQuery {
    #[serde(default)]
    range: Option<String>,
}

/// 控制台「数据统计」数据源:选定范围的汇总/环比基线 + 364 天日序列 +
/// 按来源(agent/各平台)分组的模型明细。
pub(crate) async fn usage_stats_web(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Query(query): Query<UsageStatsQuery>,
) -> std::result::Result<Response, ApiError> {
    require_auth(&headers, &state)?;
    let range = crate::state::UsageRange::parse(query.range.as_deref().unwrap_or("1d"));
    let config = state.manager.lock().unwrap().config.clone();
    crate::models_cache::ensure_active_metadata(&state.paths, &config);
    let stats = state
        .state_store
        .usage_stats(range, Some(&config))
        .map_err(ApiError::internal)?;
    Ok(Json(json!({ "ok": true, "stats": stats })).into_response())
}

#[derive(Deserialize)]
pub(crate) struct UsageDetailsQuery {
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    src: Option<String>,
    #[serde(default)]
    model: Option<String>,
}

pub(crate) async fn usage_details_web(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Query(query): Query<UsageDetailsQuery>,
) -> std::result::Result<Response, ApiError> {
    require_auth(&headers, &state)?;
    let limit = query.limit.unwrap_or(50).clamp(1, 500);
    let config = state.manager.lock().unwrap().config.clone();
    crate::models_cache::ensure_active_metadata(&state.paths, &config);
    let records = state
        .state_store
        .usage_details(
            limit,
            query.src.as_deref(),
            query.model.as_deref(),
            Some(&config),
        )
        .map_err(ApiError::internal)?;
    Ok(Json(json!({ "ok": true, "records": records })).into_response())
}

pub(crate) async fn stop_job_http(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Path(job_id): Path<String>,
) -> std::result::Result<Response, ApiError> {
    require_mutation(&headers, &state)?;
    tools::jobs::stop_job(&job_id)
        .await
        .map_err(|error| ApiError::new(StatusCode::NOT_FOUND, safe_error_message(&error)))?;
    tools::jobs::acknowledge(&job_id);
    state
        .events
        .publish("job.acknowledged", json!({ "job_id": job_id }));
    Ok(StatusCode::NO_CONTENT.into_response())
}

pub(crate) async fn cancel_run(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Path(run_id): Path<String>,
) -> std::result::Result<Response, ApiError> {
    require_mutation(&headers, &state)?;
    let cancelled = {
        let manager = state.manager.lock().unwrap();
        manager
            .active_runs
            .get(&run_id)
            .map(RunInfo::request_cancel)
    };
    if cancelled.is_none() {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "active run not found"));
    }
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({
            "run_id": run_id,
            "cancellation_requested": true,
        })),
    )
        .into_response())
}

pub(crate) async fn answer_question(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Path(question_id): Path<String>,
    Json(request): Json<AnswerQuestionRequest>,
) -> std::result::Result<StatusCode, ApiError> {
    require_mutation(&headers, &state)?;
    match state
        .questions
        .answer(&question_id, request.answers, |run_id, answers| {
            state.events.publish(
                "question.answered",
                json!({
                    "run_id": run_id,
                    "question_id": question_id,
                    "answers": answers,
                }),
            );
        }) {
        Ok(()) => {}
        Err(AnswerFailure::NotFound) => {
            return Err(ApiError::new(
                StatusCode::NOT_FOUND,
                "pending question not found",
            ));
        }
        Err(AnswerFailure::Invalid(message)) => {
            return Err(ApiError::new(StatusCode::BAD_REQUEST, message));
        }
        Err(AnswerFailure::Gone) => {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "the question is no longer awaiting an answer",
            ));
        }
    }
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) async fn close_question(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Path(question_id): Path<String>,
) -> std::result::Result<StatusCode, ApiError> {
    require_mutation(&headers, &state)?;
    match state.questions.close(&question_id, |run_id| {
        state.events.publish(
            "question.closed",
            json!({
                "run_id": run_id,
                "question_id": question_id,
            }),
        );
    }) {
        Ok(()) => {}
        Err(AnswerFailure::NotFound) => {
            return Err(ApiError::new(
                StatusCode::NOT_FOUND,
                "pending question not found",
            ));
        }
        Err(AnswerFailure::Gone) => {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "the question is no longer awaiting an answer",
            ));
        }
        Err(AnswerFailure::Invalid(_)) => unreachable!("closing a question has no answer payload"),
    }
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) async fn get_thinking_variants(
    State(state): State<DaemonState>,
    headers: HeaderMap,
) -> std::result::Result<Response, ApiError> {
    require_auth(&headers, &state)?;
    let config = state.manager.lock().unwrap().config.clone();
    let options =
        active_thinking_variant_options(&config, &state.paths).map_err(ApiError::internal)?;
    let mut response = Json(ThinkingVariantsResponse { options }).into_response();
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

pub(crate) async fn set_thinking_variants(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Json(request): Json<SetThinkingVariantsRequest>,
) -> std::result::Result<Json<ThinkingVariantsResponse>, ApiError> {
    require_mutation(&headers, &state)?;
    let updates = validate_thinking_variant_updates(request.updates)?;
    reserve_admin(&state.manager)?;
    let (reply, receiver) = oneshot::channel();
    if state
        .actor_tx
        .send(ActorCommand::SetThinkingVariants { updates, reply })
        .is_err()
    {
        release_admin(&state.manager);
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "agent worker is unavailable",
        ));
    }
    match receiver.await {
        Ok(Ok(())) => {}
        Ok(Err(AdminFailure::Invalid(message))) => {
            return Err(ApiError::new(StatusCode::BAD_REQUEST, message));
        }
        Ok(Err(AdminFailure::Internal(message))) => {
            tracing::error!(
                error = %message,
                "{}",
                t(
                    "WebUI thinking variant update failed",
                    "WebUI 思考程度更新失败"
                )
            );
            return Err(ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                safe_error_message(&message),
            ));
        }
        Err(_) => {
            release_admin(&state.manager);
            return Err(ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "agent worker stopped before updating the thinking variant",
            ));
        }
    }
    let config = state.manager.lock().unwrap().config.clone();
    let options =
        active_thinking_variant_options(&config, &state.paths).map_err(ApiError::internal)?;
    Ok(Json(ThinkingVariantsResponse { options }))
}

#[derive(Deserialize)]
pub(crate) struct SetSessionModelsRequest {
    /// Empty clears the override so the session follows the global pool.
    #[serde(default)]
    models: Vec<ActiveProviderModelConfig>,
}

#[derive(Serialize)]
pub(crate) struct SessionModelsResponse {
    model_override: Option<Vec<ActiveProviderModelConfig>>,
}

pub(crate) async fn get_session_models_http(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Path(session_id): Path<String>,
) -> std::result::Result<Json<SessionModelsResponse>, ApiError> {
    require_auth(&headers, &state)?;
    let record = require_local_web_session(&state, &session_id)?;
    let model_override = state
        .state_store
        .session_model_override(&record.session_id)
        .map_err(ApiError::internal)?;
    Ok(Json(SessionModelsResponse { model_override }))
}

pub(crate) async fn set_session_models_http(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Path(session_id): Path<String>,
    Json(request): Json<SetSessionModelsRequest>,
) -> std::result::Result<Json<SessionModelsResponse>, ApiError> {
    require_mutation(&headers, &state)?;
    let record = require_local_web_session(&state, &session_id)?;
    let models = (!request.models.is_empty()).then(|| request.models);
    if let Some(models) = &models {
        let choices = {
            let manager = state.manager.lock().unwrap();
            manager.config.text_provider_model_choices()
        };
        for model in models {
            if !choices.iter().any(|choice| {
                choice.provider_id == model.provider_id && choice.model == model.model
            }) {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    format!("unknown model: {}/{}", model.provider_id, model.model),
                ));
            }
        }
    }
    state
        .state_store
        .set_session_model_override(&record.session_id, models.as_deref())
        .map_err(ApiError::internal)?;
    state.events.publish(
        "session.updated",
        json!({ "session_id": record.session_id, "model_override": models }),
    );
    Ok(Json(SessionModelsResponse {
        model_override: models,
    }))
}
