//! messages — 自 src/platforms/onebot.rs 拆分。

use super::*;

pub(crate) fn parse_cq_string(raw: &str, self_id: i64) -> InboundMessage {
    let mut parsed = InboundMessage::default();
    let mut remaining = raw;
    let mut segment_count = 0usize;
    while let Some(start) = remaining.find("[CQ:") {
        push_cq_text(&mut parsed, &remaining[..start]);
        if parsed.rejected_reason.is_some() {
            return parsed;
        }
        segment_count += 1;
        if segment_count > MAX_INBOUND_SEGMENTS {
            parsed.rejected_reason = Some("message has too many OneBot segments");
            return parsed;
        }
        let segment = &remaining[start + 4..];
        let Some(end) = segment.find(']') else {
            push_cq_text(&mut parsed, &remaining[start..]);
            return parsed;
        };
        let body = &segment[..end];
        let mut fields = body.split(',');
        let kind = fields.next().unwrap_or_default();
        let parameters = fields
            .take(MAX_CQ_FIELDS)
            .filter_map(|field| field.split_once('='))
            .collect::<HashMap<_, _>>();
        match kind {
            "at" => {
                if let Some(qq) = parameters.get("qq").map(|value| decode_cq_text(value)) {
                    parsed.at_self |= qq == self_id.to_string();
                    push_mention(&mut parsed, qq);
                }
            }
            "reply" => {
                parsed.reply_to_message_id = parameters
                    .get("id")
                    .map(|value| decode_cq_text(value))
                    .and_then(bounded_onebot_id);
            }
            "image" | "file" | "record" | "video" | "face"
                if parsed.media.len() < MAX_INBOUND_MEDIA_RECORDS =>
            {
                let media_kind = match kind {
                    "image" => PlatformMediaKind::Image,
                    "file" => PlatformMediaKind::File,
                    "record" => PlatformMediaKind::Audio,
                    "video" => PlatformMediaKind::Video,
                    "face" => PlatformMediaKind::Emoji,
                    _ => PlatformMediaKind::Other,
                };
                parsed.media.push(PlatformInboundMedia {
                    kind: media_kind,
                    id: parameters
                        .get("id")
                        .or_else(|| parameters.get("file_id"))
                        .map(|value| decode_cq_text(value))
                        .and_then(bounded_onebot_id),
                    name: parameters
                        .get("name")
                        .or_else(|| parameters.get("file_name"))
                        .map(|value| {
                            bounded_chars(&decode_cq_text(value), MAX_INBOUND_FILE_NAME_CHARS)
                        }),
                    url: parameters
                        .get("url")
                        .map(|value| decode_cq_text(value))
                        .filter(|url| url.starts_with("http") && url.len() <= 4096),
                });
            }
            _ => {}
        }
        if kind == "image" {
            let file = parameters
                .get("file")
                .map(|value| decode_cq_text(value))
                .unwrap_or_default();
            let url = parameters.get("url").map(|value| decode_cq_text(value));
            if !push_inbound_image_source(&mut parsed, &file, url.as_deref()) {
                push_unresolved_image_file(
                    parsed.images.len(),
                    &mut parsed.unresolved_image_files,
                    (!file.is_empty()).then_some(file),
                );
            }
        }
        remaining = &segment[end + 1..];
    }
    push_cq_text(&mut parsed, remaining);
    parsed
}

/// Parses the OneBot `message` field (segment array, or raw string as a
/// fallback when NapCat isn't configured for array format).
pub(crate) fn parse_message(
    message: Option<&Value>,
    raw_message: Option<&Value>,
    self_id: i64,
) -> InboundMessage {
    let mut parsed = InboundMessage::default();
    let Some(Value::Array(segments)) = message else {
        if let Some(raw) = message
            .and_then(Value::as_str)
            .or_else(|| raw_message.and_then(Value::as_str))
        {
            return parse_cq_string(raw, self_id);
        }
        return parsed;
    };
    if segments.len() > MAX_INBOUND_SEGMENTS {
        parsed.rejected_reason = Some("message has too many OneBot segments");
        return parsed;
    }
    for segment in segments.iter().take(MAX_INBOUND_SEGMENTS) {
        let kind = segment.get("type").and_then(Value::as_str).unwrap_or("");
        let data = segment.get("data").unwrap_or(&Value::Null);
        match kind {
            "text" => {
                if let Some(text) = data.get("text").and_then(Value::as_str) {
                    push_inbound_text(&mut parsed, text);
                    if parsed.rejected_reason.is_some() {
                        return parsed;
                    }
                }
            }
            "image" => {
                if parsed.media.len() < MAX_INBOUND_MEDIA_RECORDS {
                    let file = data.get("file").and_then(Value::as_str).unwrap_or("");
                    parsed.media.push(PlatformInboundMedia {
                        kind: PlatformMediaKind::Image,
                        id: data
                            .get("file_id")
                            .and_then(value_id_string)
                            .and_then(bounded_onebot_id)
                            .or_else(|| {
                                (!file.is_empty() && !file.starts_with("base64://"))
                                    .then(|| file.to_string())
                                    .and_then(bounded_onebot_id)
                            }),
                        name: None,
                        url: data
                            .get("url")
                            .and_then(Value::as_str)
                            .filter(|url| url.starts_with("http") && url.len() <= 4096)
                            .map(str::to_string),
                    });
                }
                let file = data.get("file").and_then(Value::as_str).unwrap_or("");
                if !push_inbound_image_source(
                    &mut parsed,
                    file,
                    data.get("url").and_then(Value::as_str),
                ) {
                    let file_id = data.get("file_id").and_then(value_id_string);
                    push_unresolved_image_file(
                        parsed.images.len(),
                        &mut parsed.unresolved_image_files,
                        (!file.is_empty()).then(|| file.to_string()).or(file_id),
                    );
                }
            }
            "at" => {
                let qq = data.get("qq").and_then(|qq| match qq {
                    Value::String(qq) => Some(qq.clone()),
                    Value::Number(qq) => Some(qq.to_string()),
                    _ => None,
                });
                if qq.as_deref() == Some(self_id.to_string().as_str()) {
                    parsed.at_self = true;
                }
                if let Some(qq) = qq {
                    push_mention(&mut parsed, qq);
                }
            }
            "reply" => {
                parsed.reply_to_message_id = data
                    .get("id")
                    .and_then(value_id_string)
                    .and_then(bounded_onebot_id);
            }
            "file" => {
                if parsed.media.len() < MAX_INBOUND_MEDIA_RECORDS {
                    parsed.media.push(PlatformInboundMedia {
                        kind: PlatformMediaKind::File,
                        id: data
                            .get("file_id")
                            .and_then(value_id_string)
                            .or_else(|| data.get("file").and_then(value_id_string))
                            .and_then(bounded_onebot_id),
                        name: data
                            .get("file_name")
                            .and_then(Value::as_str)
                            .or_else(|| data.get("name").and_then(Value::as_str))
                            .map(|name| bounded_chars(name, MAX_INBOUND_FILE_NAME_CHARS)),
                        url: data
                            .get("url")
                            .and_then(Value::as_str)
                            .filter(|url| url.starts_with("http") && url.len() <= 4096)
                            .map(str::to_string),
                    });
                }
                if parsed.files.len() >= MAX_INBOUND_FILES {
                    continue;
                }
                let name = bounded_chars(
                    data.get("file_name")
                        .and_then(Value::as_str)
                        .or_else(|| data.get("name").and_then(Value::as_str))
                        .or_else(|| data.get("file").and_then(Value::as_str))
                        .unwrap_or("file"),
                    MAX_INBOUND_FILE_NAME_CHARS,
                );
                parsed.files.push(FileRef {
                    file_id: data
                        .get("file_id")
                        .and_then(Value::as_str)
                        .and_then(|id| bounded_onebot_id(id.to_string())),
                    name,
                    url: data
                        .get("url")
                        .and_then(Value::as_str)
                        .filter(|url| url.starts_with("http") && url.len() <= 4096)
                        .map(str::to_string),
                });
            }
            "face" | "record" | "video" if parsed.media.len() < MAX_INBOUND_MEDIA_RECORDS => {
                parsed.media.push(PlatformInboundMedia {
                    kind: match kind {
                        "face" => PlatformMediaKind::Emoji,
                        "record" => PlatformMediaKind::Audio,
                        "video" => PlatformMediaKind::Video,
                        _ => PlatformMediaKind::Other,
                    },
                    id: data
                        .get("id")
                        .and_then(value_id_string)
                        .or_else(|| data.get("file_id").and_then(value_id_string))
                        .and_then(bounded_onebot_id),
                    name: data
                        .get("name")
                        .and_then(Value::as_str)
                        .map(|name| bounded_chars(name, MAX_INBOUND_FILE_NAME_CHARS)),
                    url: data
                        .get("url")
                        .and_then(Value::as_str)
                        .filter(|url| url.starts_with("http") && url.len() <= 4096)
                        .map(str::to_string),
                });
            }
            // Other OneBot segments carry no turn input.
            _ => {}
        }
    }
    parsed
}

// ---------------------------------------------------------------------------
// Outbound
// ---------------------------------------------------------------------------

pub(crate) struct OneBotAdapter {
    conn: ConnectionHandle,
    registry: Arc<Mutex<ConnectionRegistry>>,
    http: reqwest::Client,
    self_id: i64,
    target: Target,
    max_reply_chars: usize,
}

pub(crate) fn onebot_id_value(value: &str) -> Value {
    value
        .trim()
        .parse::<i64>()
        .map(Value::from)
        .unwrap_or_else(|_| Value::String(value.trim().to_string()))
}

pub(crate) fn parse_message_info(data: &Value, self_id: i64) -> Option<PlatformMessageInfo> {
    let message_id = data.get("message_id").and_then(value_id_string)?;
    let parsed = parse_message(data.get("message"), data.get("raw_message"), self_id);
    let sender = data.get("sender");
    let sender_id = sender
        .and_then(|sender| sender.get("user_id"))
        .and_then(value_id_string)
        .or_else(|| data.get("user_id").and_then(value_id_string))
        .unwrap_or_default();
    let sender_display_name = sender
        .and_then(|sender| sender.get("card"))
        .and_then(Value::as_str)
        .filter(|name| !name.trim().is_empty())
        .or_else(|| {
            sender
                .and_then(|sender| sender.get("nickname"))
                .and_then(Value::as_str)
        })
        .unwrap_or("?")
        .to_string();
    let conversation_kind = match data.get("message_type").and_then(Value::as_str) {
        Some("group") => Some(ConversationKind::Group),
        Some("private") => Some(ConversationKind::Private),
        _ => None,
    };
    let conversation_id = data
        .get("group_id")
        .and_then(value_id_string)
        .or_else(|| data.get("target_id").and_then(value_id_string))
        .or_else(|| data.get("peer_id").and_then(value_id_string))
        .or_else(|| {
            data.get("user_id")
                .and_then(value_id_string)
                .filter(|id| id != &self_id.to_string())
        })
        .or_else(|| {
            (conversation_kind == Some(ConversationKind::Private)
                && sender_id != self_id.to_string())
            .then(|| sender_id.clone())
        });
    Some(PlatformMessageInfo {
        message_id,
        sender_id,
        sender_display_name,
        timestamp: data.get("time").and_then(Value::as_i64).unwrap_or(0),
        text: parsed.text,
        reply_to_message_id: parsed.reply_to_message_id,
        mentioned_user_ids: parsed.mentioned_user_ids,
        mentioned_users: Vec::new(),
        media: parsed.media,
        conversation_kind,
        conversation_id,
    })
}

pub(crate) fn parse_group_member(
    data: &Value,
    fallback_group_id: i64,
) -> Option<PlatformGroupMember> {
    Some(PlatformGroupMember {
        group_id: data
            .get("group_id")
            .and_then(value_id_string)
            .unwrap_or_else(|| fallback_group_id.to_string()),
        user_id: data.get("user_id").and_then(value_id_string)?,
        nickname: data
            .get("nickname")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        card: data
            .get("card")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        role: data
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("member")
            .to_string(),
        title: data
            .get("title")
            .and_then(Value::as_str)
            .or_else(|| data.get("special_title").and_then(Value::as_str))
            .unwrap_or_default()
            .to_string(),
        joined_at: data.get("join_time").and_then(Value::as_i64).unwrap_or(0),
        last_active_at: data
            .get("last_sent_time")
            .and_then(Value::as_i64)
            .unwrap_or(0),
    })
}

pub(crate) fn group_member_mute_until(data: &Value) -> Option<i64> {
    data.get("shut_up_timestamp").and_then(|value| {
        value
            .as_i64()
            .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
            .or_else(|| value.as_str().and_then(|value| value.trim().parse().ok()))
    })
}

pub(crate) fn prepend_response_target(segments: &mut Vec<Value>, target: &ResponseTarget) {
    let mut index = 0;
    if target.quote && !target.message_id.is_empty() {
        segments.insert(
            index,
            json!({ "type": "reply", "data": { "id": target.message_id } }),
        );
        index += 1;
    }
    let mut seen = HashSet::new();
    let mut mention_user_ids = Vec::new();
    if target.mention && !target.user_id.is_empty() {
        seen.insert(target.user_id.as_str());
        mention_user_ids.push(target.user_id.as_str());
    }
    for user_id in &target.explicit_mention_user_ids {
        let user_id = user_id.trim();
        if !user_id.is_empty() && seen.insert(user_id) {
            mention_user_ids.push(user_id);
        }
    }
    for user_id in mention_user_ids {
        segments.insert(index, json!({ "type": "at", "data": { "qq": user_id } }));
        index += 1;
        // OneBot renders an `at` segment adjacent to the following text.
        // Keep the generated target readable on clients that do not add
        // visual separation themselves.
        segments.insert(index, text_segment(" "));
        index += 1;
    }
}

impl PlatformAdapter for OneBotAdapter {
    fn send<'a>(&'a self, message: OutboundMessage) -> BoxFuture<'a, Result<SendReceipt>> {
        Box::pin(async move { self.send_message(message).await })
    }

    fn bot_display_name<'a>(&'a self) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            let conn = self.connection();
            if let Some(name) = conn.bot_name.lock().unwrap().clone() {
                return Ok(name);
            }
            let data = conn.call_api("get_login_info", json!({})).await?;
            let name = data
                .get("nickname")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .unwrap_or("Bot")
                .to_string();
            *conn.bot_name.lock().unwrap() = Some(name.clone());
            Ok(name)
        })
    }

    fn message_images<'a>(
        &'a self,
        message_id: &'a str,
    ) -> BoxFuture<'a, Result<Vec<PlatformImageData>>> {
        Box::pin(async move {
            let data = get_message_data(
                &self.connection(),
                message_id,
                QUOTED_MESSAGE_LOOKUP_TIMEOUT,
            )
            .await?;
            let info = parse_message_info(&data, self.self_id)
                .context("OneBot image message metadata is unavailable")?;
            let expected_kind = match self.target {
                Target::Private { .. } => ConversationKind::Private,
                Target::Group { .. } => ConversationKind::Group,
            };
            let expected_id = self.target.conversation_id().to_string();
            if info.conversation_kind != Some(expected_kind)
                || info.conversation_id.as_deref() != Some(expected_id.as_str())
            {
                bail!("the requested image message belongs to another conversation")
            }
            let mut images = Vec::new();
            let mut total_bytes = 0usize;
            let sources =
                ordered_message_image_sources(data.get("message"), data.get("raw_message"));
            for source in sources {
                let remaining = MAX_INBOUND_IMAGE_TOTAL_BYTES.saturating_sub(total_bytes);
                if remaining == 0 {
                    break;
                }
                let maximum = MAX_INBOUND_IMAGE_BYTES.min(remaining);
                let media = match source {
                    OrderedMessageImageSource::Media(media) => media,
                    OrderedMessageImageSource::File(file) => {
                        let Ok(data) = self
                            .connection()
                            .call_api_with_timeout(
                                "get_image",
                                json!({ "file": file }),
                                QUOTED_MESSAGE_LOOKUP_TIMEOUT,
                            )
                            .await
                        else {
                            continue;
                        };
                        let mut parsed = InboundMessage::default();
                        if !append_resolved_quoted_image(&mut parsed, &data) {
                            continue;
                        }
                        let Some(media) = parsed.images.into_iter().next() else {
                            continue;
                        };
                        media
                    }
                };
                let bytes = match media {
                    MediaRef::Bytes(bytes) if bytes.len() <= maximum => bytes,
                    MediaRef::Bytes(_) => continue,
                    MediaRef::Url(url) => {
                        match download_capped(&self.http, &url, maximum, IMAGE_DOWNLOAD_TIMEOUT)
                            .await
                        {
                            Ok((bytes, _)) => bytes,
                            Err(error) => {
                                tracing::debug!(%error, "{}", t("meme collector image download failed", "表情包收集器图片下载失败"));
                                continue;
                            }
                        }
                    }
                };
                total_bytes += bytes.len();
                images.push(PlatformImageData {
                    mime: sniff_image_mime(&bytes).to_string(),
                    data: Arc::from(bytes),
                });
            }
            Ok(images)
        })
    }

    fn bot_send_availability<'a>(&'a self) -> BoxFuture<'a, Result<BotSendAvailability>> {
        Box::pin(async move {
            let Target::Group { group_id } = self.target else {
                return Ok(BotSendAvailability::Available);
            };
            let key = (self.self_id, group_id);
            let now = Instant::now();
            if let Some(availability) = group_mute_cache().lock().unwrap().get(key, now) {
                return Ok(availability);
            }

            let result = self
                .connection()
                .call_api_with_timeout(
                    "get_group_member_info",
                    json!({
                        "group_id": group_id,
                        "user_id": self.self_id,
                        "no_cache": false,
                    }),
                    GROUP_MUTE_LOOKUP_TIMEOUT,
                )
                .await;
            let now_unix = unix_now();
            let (availability, ttl) = match result {
                Ok(data) => match group_member_mute_until(&data) {
                    Some(muted_until) if muted_until > now_unix => (
                        BotSendAvailability::Muted,
                        Duration::from_secs((muted_until - now_unix) as u64)
                            .min(GROUP_MUTE_MAX_TTL),
                    ),
                    Some(_) => (BotSendAvailability::Available, GROUP_MUTE_AVAILABLE_TTL),
                    None => (BotSendAvailability::Unknown, GROUP_MUTE_UNKNOWN_TTL),
                },
                Err(error) => {
                    tracing::debug!(
                        target: "gqy::qq",
                        error = %error,
                        self_id = self.self_id,
                        group_id,
                        "{}",
                        t("OneBot bot mute-state lookup failed", "OneBot 机器人禁言状态查询失败")
                    );
                    (BotSendAvailability::Unknown, GROUP_MUTE_UNKNOWN_TTL)
                }
            };
            group_mute_cache()
                .lock()
                .unwrap()
                .insert(key, availability, ttl, now);
            Ok(availability)
        })
    }

    fn set_message_reaction<'a>(
        &'a self,
        message_id: &'a str,
        reaction_id: &'a str,
        active: bool,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            if message_id.trim().is_empty() || reaction_id.trim().is_empty() {
                bail!("message_id and reaction_id are required");
            }
            self.connection()
                .call_api(
                    "set_msg_emoji_like",
                    json!({
                        "message_id": onebot_id_value(message_id),
                        "emoji_id": onebot_id_value(reaction_id),
                        "emoji_type": "1",
                        "set": active,
                    }),
                )
                .await?;
            Ok(())
        })
    }

    fn message_info<'a>(
        &'a self,
        message_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<PlatformMessageInfo>>> {
        Box::pin(async move {
            if message_id.trim().is_empty() {
                return Ok(None);
            }
            let data = get_message_data(&self.connection(), message_id, API_CALL_TIMEOUT).await?;
            Ok(parse_message_info(&data, self.self_id))
        })
    }

    fn group_members<'a>(&'a self) -> BoxFuture<'a, Result<Vec<PlatformGroupMember>>> {
        Box::pin(async move {
            let Target::Group { group_id } = self.target else {
                bail!("group member lookup requires a group conversation");
            };
            let data = self
                .connection()
                .call_api(
                    "get_group_member_list",
                    json!({ "group_id": group_id, "no_cache": false }),
                )
                .await?;
            let members = data
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|member| parse_group_member(member, group_id))
                .collect();
            Ok(members)
        })
    }

    fn group_member<'a>(
        &'a self,
        user_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<PlatformGroupMember>>> {
        self.group_member_lookup(user_id, false)
    }

    fn group_member_fresh<'a>(
        &'a self,
        user_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<PlatformGroupMember>>> {
        self.group_member_lookup(user_id, true)
    }

    fn bot_group_role<'a>(&'a self) -> BoxFuture<'a, Result<BotGroupRole>> {
        Box::pin(async move {
            let Target::Group { group_id } = self.target else {
                return Ok(BotGroupRole::Unknown);
            };
            let key = (self.self_id, group_id);
            let now = Instant::now();
            if let Some(role) = group_role_cache().lock().unwrap().get(key, now) {
                return Ok(role);
            }
            let data = self
                .connection()
                .call_api(
                    "get_group_member_info",
                    json!({
                        "group_id": group_id,
                        "user_id": self.self_id,
                        "no_cache": false,
                    }),
                )
                .await?;
            let role = match data.get("role").and_then(Value::as_str) {
                Some("owner") => BotGroupRole::Owner,
                Some("admin") => BotGroupRole::Admin,
                Some("member") => BotGroupRole::Member,
                _ => BotGroupRole::Unknown,
            };
            group_role_cache().lock().unwrap().insert(key, role, now);
            Ok(role)
        })
    }

    fn delete_message<'a>(&'a self, message_id: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let message_id = message_id.trim();
            if message_id.is_empty() || message_id.len() > MAX_ONEBOT_ID_BYTES {
                bail!("invalid OneBot message id");
            }
            let numeric = message_id
                .parse::<i32>()
                .context("OneBot message id is outside the supported numeric range")?;
            self.connection()
                .call_api("delete_msg", json!({ "message_id": numeric }))
                .await?;
            Ok(())
        })
    }

    fn set_group_ban<'a>(
        &'a self,
        user_id: &'a str,
        duration_seconds: u64,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let Target::Group { group_id } = self.target else {
                bail!("group ban requires a group conversation");
            };
            self.connection()
                .call_api(
                    "set_group_ban",
                    json!({
                        "group_id": group_id,
                        "user_id": onebot_id_value(user_id),
                        "duration": duration_seconds,
                    }),
                )
                .await?;
            Ok(())
        })
    }

    fn set_group_kick<'a>(
        &'a self,
        user_id: &'a str,
        reject_add_request: bool,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let Target::Group { group_id } = self.target else {
                bail!("group kick requires a group conversation");
            };
            self.connection()
                .call_api(
                    "set_group_kick",
                    json!({
                        "group_id": group_id,
                        "user_id": onebot_id_value(user_id),
                        "reject_add_request": reject_add_request,
                    }),
                )
                .await?;
            Ok(())
        })
    }

    fn set_group_special_title<'a>(
        &'a self,
        user_id: &'a str,
        special_title: &'a str,
        duration_seconds: i64,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let Target::Group { group_id } = self.target else {
                bail!("group title requires a group conversation");
            };
            self.connection()
                .call_api(
                    "set_group_special_title",
                    json!({
                        "group_id": group_id,
                        "user_id": onebot_id_value(user_id),
                        "special_title": special_title,
                        "duration": duration_seconds,
                    }),
                )
                .await?;
            Ok(())
        })
    }
}

impl OneBotAdapter {
    /// `no_cache` asks NapCat to re-read the roster from the server instead of
    /// answering from its own copy, which can still list members who left.
    pub(crate) fn group_member_lookup<'a>(
        &'a self,
        user_id: &'a str,
        no_cache: bool,
    ) -> BoxFuture<'a, Result<Option<PlatformGroupMember>>> {
        Box::pin(async move {
            let Target::Group { group_id } = self.target else {
                bail!("group member lookup requires a group conversation");
            };
            if user_id.trim().is_empty() {
                return Ok(None);
            }
            let data = self
                .connection()
                .call_api(
                    "get_group_member_info",
                    json!({
                        "group_id": group_id,
                        "user_id": onebot_id_value(user_id),
                        "no_cache": no_cache,
                    }),
                )
                .await?;
            Ok(parse_group_member(&data, group_id))
        })
    }

    pub(crate) fn connection(&self) -> ConnectionHandle {
        self.registry
            .lock()
            .unwrap()
            .handle(self.self_id)
            .unwrap_or_else(|| self.conn.clone())
    }

    pub(crate) async fn send_message(&self, message: OutboundMessage) -> Result<SendReceipt> {
        let response_target = message.response_target;
        match message.body {
            OutboundBody::Segments(segments) => {
                self.send_segments(segments, response_target.as_ref()).await
            }
            OutboundBody::Forward(nodes) => {
                let mut receipt = self.send_forward(nodes).await?;
                if let Some(target) = response_target.filter(ResponseTarget::is_effective) {
                    match self.send_response_marker(&target).await {
                        Ok(message_id) => {
                            receipt.delivered_parts += 1;
                            receipt.response_target_delivered = true;
                            if let Some(message_id) = message_id {
                                receipt.message_ids.push(message_id);
                            }
                        }
                        Err(error) => return Err(partial_send_error(error, receipt)),
                    }
                }
                Ok(receipt)
            }
        }
    }

    pub(crate) async fn send_response_marker(
        &self,
        target: &ResponseTarget,
    ) -> Result<Option<String>> {
        if !matches!(self.target, Target::Group { .. }) || !target.is_effective() {
            return Ok(None);
        }
        let mut segments = vec![text_segment("\u{200b}")];
        prepend_response_target(&mut segments, target);
        let data = self.send_message_segments(segments).await?;
        Ok(data.get("message_id").and_then(value_id_string))
    }

    pub(crate) async fn send_segments(
        &self,
        segments: Vec<OutboundSegment>,
        response_target: Option<&ResponseTarget>,
    ) -> Result<SendReceipt> {
        let mut frames = Vec::new();
        let mut current = Vec::new();
        let mut current_image_digests = Vec::new();
        let mut files = Vec::new();
        for segment in segments {
            match segment {
                OutboundSegment::Markdown(text) => {
                    append_text_chunks(
                        &mut frames,
                        &mut current,
                        &mut current_image_digests,
                        &markdown_to_plain(&text),
                        self.max_reply_chars,
                    );
                }
                OutboundSegment::Text(text) => append_text_chunks(
                    &mut frames,
                    &mut current,
                    &mut current_image_digests,
                    &text,
                    self.max_reply_chars,
                ),
                OutboundSegment::Mention(user_id) => current.push(json!({
                    "type": "at",
                    "data": { "qq": user_id },
                })),
                OutboundSegment::ImageBytes { data, .. } => {
                    if data.len() > MAX_OUTBOUND_IMAGE_BYTES {
                        bail!("outbound image exceeds the 20 MiB limit");
                    }
                    current_image_digests.push(blake3::hash(&data));
                    current.push(image_segment(&data));
                }
                OutboundSegment::ImagePath { path, .. } => {
                    let bytes = read_file_capped(&path, MAX_OUTBOUND_IMAGE_BYTES).await?;
                    // Decode dimensions before giving untrusted/generated bytes
                    // to the adapter, matching WebUI image safety expectations.
                    image::load_from_memory(&bytes)
                        .with_context(|| format!("decoding image {}", path.display()))?;
                    current_image_digests.push(blake3::hash(&bytes));
                    current.push(image_segment(&bytes));
                }
                OutboundSegment::FilePath { path, name } => {
                    push_message_frame(&mut frames, &mut current, &mut current_image_digests);
                    files.push((path, name));
                }
            }
        }
        push_message_frame(&mut frames, &mut current, &mut current_image_digests);

        let has_message_frames = !frames.is_empty();
        let target_on_first_frame = has_message_frames
            && matches!(self.target, Target::Group { .. })
            && response_target.is_some_and(ResponseTarget::is_effective);
        let mut receipt = SendReceipt::default();
        for (index, frame) in frames.into_iter().enumerate() {
            let MessageFrame {
                mut segments,
                image_digests,
            } = frame;
            let has_image = !image_digests.is_empty();
            if index == 0 && target_on_first_frame {
                prepend_response_target(
                    &mut segments,
                    response_target.expect("effective response target exists"),
                );
            }
            let data = match self.send_message_segments(segments).await {
                Ok(data) => data,
                Err(error) => return Err(partial_send_error(error, receipt)),
            };
            receipt.delivered_parts += 1;
            if index == 0 && target_on_first_frame {
                receipt.response_target_delivered = true;
            }
            receipt.image_digests.extend(image_digests);
            if let Some(id) = data.get("message_id").and_then(value_id_string) {
                if has_image {
                    receipt.image_message_ids.push(id.clone());
                }
                receipt.message_ids.push(id);
            }
        }
        for (path, name) in files {
            let id = match self.upload_file(&path, name.as_deref()).await {
                Ok(id) => id,
                Err(error) => return Err(partial_send_error(error, receipt)),
            };
            receipt.delivered_parts += 1;
            if let Some(id) = id {
                receipt.message_ids.push(id);
            }
        }
        if !has_message_frames {
            if let Some(target) = response_target.filter(|target| target.is_effective()) {
                let message_id = match self.send_response_marker(target).await {
                    Ok(message_id) => message_id,
                    Err(error) => return Err(partial_send_error(error, receipt)),
                };
                receipt.delivered_parts += 1;
                receipt.response_target_delivered = true;
                if let Some(message_id) = message_id {
                    receipt.message_ids.push(message_id);
                }
            }
        }
        Ok(receipt)
    }

    pub(crate) async fn send_forward(&self, nodes: Vec<ForwardNode>) -> Result<SendReceipt> {
        if nodes.is_empty() {
            bail!("a forward message needs at least one node");
        }
        let mut messages = Vec::with_capacity(nodes.len());
        let mut image_digests = Vec::new();
        for node in nodes {
            let mut content = Vec::new();
            for segment in node.segments {
                match segment {
                    OutboundSegment::Markdown(text) => {
                        content.push(text_segment(&markdown_to_plain(&text)));
                    }
                    OutboundSegment::Text(text) => content.push(text_segment(&text)),
                    OutboundSegment::Mention(user_id) => content.push(json!({
                        "type": "at",
                        "data": { "qq": user_id },
                    })),
                    OutboundSegment::ImageBytes { data, .. } => {
                        if data.len() > MAX_OUTBOUND_IMAGE_BYTES {
                            bail!("outbound image exceeds the 20 MiB limit");
                        }
                        image_digests.push(blake3::hash(&data));
                        content.push(image_segment(&data));
                    }
                    OutboundSegment::ImagePath { path, .. } => {
                        let bytes = read_file_capped(&path, MAX_OUTBOUND_IMAGE_BYTES).await?;
                        image::load_from_memory(&bytes)
                            .with_context(|| format!("decoding image {}", path.display()))?;
                        image_digests.push(blake3::hash(&bytes));
                        content.push(image_segment(&bytes));
                    }
                    OutboundSegment::FilePath { .. } => {
                        bail!("files cannot be embedded in a OneBot forward node")
                    }
                }
            }
            messages.push(json!({
                "type": "node",
                "data": {
                    "uin": node.user_id,
                    "name": node.display_name,
                    "content": content,
                }
            }));
        }
        let (action, params) = match self.target {
            Target::Private { user_id } => (
                "send_private_forward_msg",
                json!({ "user_id": user_id, "messages": messages }),
            ),
            Target::Group { group_id } => (
                "send_group_forward_msg",
                json!({ "group_id": group_id, "messages": messages }),
            ),
        };
        let data = self.connection().call_api(action, params).await?;
        Ok(SendReceipt {
            message_ids: data
                .get("message_id")
                .and_then(value_id_string)
                .into_iter()
                .collect(),
            image_message_ids: Vec::new(),
            delivered_parts: 1,
            image_digests,
            response_target_delivered: false,
        })
    }

    pub(crate) async fn send_message_segments(&self, segments: Vec<Value>) -> Result<Value> {
        let timeout = send_timeout_for(&segments);
        let (action, params) = match self.target {
            Target::Private { user_id } => (
                "send_private_msg",
                json!({ "user_id": user_id, "message": segments }),
            ),
            Target::Group { group_id } => (
                "send_group_msg",
                json!({ "group_id": group_id, "message": segments }),
            ),
        };
        self.connection()
            .call_api_with_timeout(action, params, timeout)
            .await
    }

    pub(crate) async fn upload_file(
        &self,
        path: &std::path::Path,
        name: Option<&str>,
    ) -> Result<Option<String>> {
        let metadata = tokio::fs::metadata(path)
            .await
            .with_context(|| format!("reading outbound file metadata: {}", path.display()))?;
        if !metadata.is_file() {
            bail!(
                "outbound attachment is not a regular file: {}",
                path.display()
            );
        }
        if metadata.len() > MAX_OUTBOUND_FILE_BYTES as u64 {
            bail!("outbound attachment exceeds the 50 MiB limit");
        }
        let name = name
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .or_else(|| path.file_name().and_then(|name| name.to_str()))
            .unwrap_or("file");
        let name = sanitize_file_name(name);
        let conn = self.connection();
        if let Some(base_url) = conn.asset_base_url.as_deref() {
            let lease = conn.assets.create(base_url, path, &name).await?;
            match self.upload_file_source(&lease.url, &name).await {
                Ok(id) => return Ok(id),
                Err(error) => tracing::warn!(
                    error = %error,
                    "{}",
                    t("NapCat could not fetch streamed file; considering base64 fallback", "NapCat 无法获取流式文件，尝试使用 base64 回退")
                ),
            }
        }
        if metadata.len() > MAX_BASE64_FILE_BYTES as u64 {
            bail!(
                "NapCat could not fetch the temporary file URL and the file exceeds the 16 MiB base64 fallback limit"
            );
        }
        let bytes = read_file_capped(path, MAX_BASE64_FILE_BYTES).await?;
        self.upload_file_source(&format!("base64://{}", BASE64.encode(bytes)), &name)
            .await
    }

    pub(crate) async fn upload_file_source(
        &self,
        source: &str,
        name: &str,
    ) -> Result<Option<String>> {
        let (action, params) = match self.target {
            Target::Private { user_id } => (
                "upload_private_file",
                json!({ "user_id": user_id, "file": source, "name": name }),
            ),
            Target::Group { group_id } => (
                "upload_group_file",
                json!({ "group_id": group_id, "file": source, "name": name }),
            ),
        };
        let data = self
            .conn
            .call_api_with_timeout(action, params, FILE_DOWNLOAD_TIMEOUT)
            .await?;
        Ok(data.get("file_id").and_then(value_id_string))
    }
}

pub(crate) struct MessageFrame {
    segments: Vec<Value>,
    image_digests: Vec<blake3::Hash>,
}

pub(crate) fn push_message_frame(
    frames: &mut Vec<MessageFrame>,
    current: &mut Vec<Value>,
    current_image_digests: &mut Vec<blake3::Hash>,
) {
    if current.is_empty() {
        return;
    }
    frames.push(MessageFrame {
        segments: std::mem::take(current),
        image_digests: std::mem::take(current_image_digests),
    });
}

pub(crate) fn append_text_chunks(
    frames: &mut Vec<MessageFrame>,
    current: &mut Vec<Value>,
    current_image_digests: &mut Vec<blake3::Hash>,
    text: &str,
    max_reply_chars: usize,
) {
    let chunks = split_reply(text, max_reply_chars);
    let count = chunks.len();
    for (index, chunk) in chunks.into_iter().enumerate() {
        current.push(text_segment(&chunk));
        if index + 1 < count {
            push_message_frame(frames, current, current_image_digests);
        }
    }
}

pub(crate) fn partial_send_error(error: anyhow::Error, receipt: SendReceipt) -> anyhow::Error {
    if receipt.has_delivery() {
        anyhow::Error::new(PartialSendError::new(error, receipt))
    } else {
        error
    }
}

/// Sends carrying base64 images need far longer than a plain text call: a
/// 2 MiB picture is ~2.9 MB of JSON that NapCat has to receive, decode and
/// upload to QQ. Timing out early is worse than waiting — the message is
/// still delivered, but GQY treats the send as failed and posts the plain
/// text fallback, so the group gets the picture *and* the text.
///
/// Size-scaling the budget only moved the cliff, and it moved it unevenly: the
/// old `div_ceil` step gave 0.99 MiB the same 30s as 64 KiB, so payloads just
/// under a megabyte boundary had the tightest work-to-budget ratio of all. An
/// attachment send now simply waits for NapCat instead of guessing how long it
/// should take.
///
/// `MAX_SEND_TIMEOUT` stays as a backstop rather than a budget. Losing the
/// connection already frees an in-flight call — `pending` hangs off the
/// per-connection `ConnectionHandle`, which `connection_loop` drops on exit,
/// so every waiting `oneshot` resolves immediately. The backstop only covers
/// a NapCat that stays connected but never answers this one echo, which would
/// otherwise wedge the conversation forever (same-conversation turns are
/// serialized and each in-flight message holds one of `MAX_IN_FLIGHT_MESSAGES`).
pub(crate) fn send_timeout_for(segments: &[Value]) -> Duration {
    let carries_attachment = segments.iter().any(|segment| {
        segment
            .get("data")
            .and_then(|data| data.get("file"))
            .and_then(Value::as_str)
            .is_some_and(|file| !file.is_empty())
    });
    if carries_attachment {
        MAX_SEND_TIMEOUT
    } else {
        API_CALL_TIMEOUT
    }
}

pub(crate) fn image_segment(bytes: &[u8]) -> Value {
    json!({
        "type": "image",
        "data": { "file": format!("base64://{}", BASE64.encode(bytes)) },
    })
}

pub(crate) async fn read_file_capped(path: &std::path::Path, cap: usize) -> Result<Vec<u8>> {
    let file = tokio::fs::File::open(path)
        .await
        .with_context(|| format!("opening attachment: {}", path.display()))?;
    let metadata = file
        .metadata()
        .await
        .with_context(|| format!("reading attachment metadata: {}", path.display()))?;
    if !metadata.is_file() {
        bail!("attachment is not a regular file: {}", path.display());
    }
    if metadata.len() > cap as u64 {
        bail!("attachment exceeds the {} MiB limit", cap / 1024 / 1024);
    }
    let limit = u64::try_from(cap.saturating_add(1)).unwrap_or(u64::MAX);
    let mut reader = file.take(limit);
    let mut bytes = Vec::with_capacity(metadata.len().min(cap as u64) as usize);
    reader
        .read_to_end(&mut bytes)
        .await
        .with_context(|| format!("reading attachment: {}", path.display()))?;
    if bytes.len() > cap {
        bail!("attachment exceeds the {} MiB limit", cap / 1024 / 1024);
    }
    Ok(bytes)
}

pub(crate) async fn deliver_dispatch(
    state: &DaemonState,
    context: &Arc<PlatformTurnContext>,
    dispatch: TurnDispatch,
) -> Result<bool> {
    match dispatch {
        TurnDispatch::Failed(message) => {
            context.after_turn_aborted().await;
            if context.conversation.kind == ConversationKind::Group {
                tracing::info!(
                    target: "gqy::qq",
                    error = %message,
                    "{}",
                    t("suppressed an internal OneBot group error", "已抑制 OneBot 群聊内部错误")
                );
                return Ok(false);
            }
            context
                .send_bypass_plugins(OutboundMessage::text(
                    OutboundOrigin::Command,
                    format!("{}{message}", t("Something went wrong: ", "出错了：")),
                ))
                .await?;
        }
        TurnDispatch::Completed(mut outcome) => {
            if context.turn_is_superseded() {
                context.after_turn_aborted().await;
                return Ok(false);
            }
            let mut segments = Vec::new();
            let reply_text = final_reply_text(&outcome);
            let delivered_image_digests = context.delivered_image_digests();
            let mut image_digests = delivered_image_digests.clone();
            let mut matched_delivered_image = false;
            let mut unresolved_image_count = 0usize;
            let mut image_count = 0usize;
            for asset_id in &outcome.image_assets {
                match state.state_store.load_image_asset(asset_id) {
                    Ok(Some(asset)) => {
                        let digest = blake3::hash(&asset.bytes);
                        if !image_digests.insert(digest) {
                            let already_delivered = delivered_image_digests.contains(&digest);
                            if already_delivered {
                                matched_delivered_image = true;
                            }
                            tracing::debug!(
                                target: "gqy::qq",
                                asset_id,
                                "{}",
                                if already_delivered {
                                    t(
                                        "suppressed a OneBot reply image already delivered to this conversation",
                                        "已抑制本会话中先前已投递的 OneBot 回复图片",
                                    )
                                } else {
                                    t(
                                        "suppressed a duplicate OneBot reply image",
                                        "已抑制重复的 OneBot 回复图片",
                                    )
                                }
                            );
                            continue;
                        }
                        segments.push(OutboundSegment::ImageBytes {
                            mime: asset.asset.mime,
                            data: Arc::from(asset.bytes),
                            alt: asset.asset.alt,
                        });
                        image_count += 1;
                    }
                    Ok(None) => {
                        unresolved_image_count += 1;
                        tracing::warn!(
                            target: "gqy::qq",
                            asset_id,
                            "{}",
                            t(
                                "a OneBot reply image asset was not found",
                                "未找到 OneBot 回复图片资源",
                            )
                        );
                    }
                    Err(error) => {
                        unresolved_image_count += 1;
                        tracing::warn!(error = %error, asset_id, "{}", t("loading an image asset for OneBot failed", "为 OneBot 加载图片资源失败"));
                    }
                }
            }
            if matched_delivered_image && image_count == 0 && unresolved_image_count == 0 {
                outcome.final_reply_already_sent = true;
            }
            let readable = crate::platforms::adapters::format_platform_final_reply_log(
                &outcome,
                context,
                &reply_text,
                image_count,
            );
            if !reply_text.trim().is_empty() {
                segments.insert(0, OutboundSegment::Markdown(reply_text));
            }
            if segments.is_empty() {
                if outcome.final_reply_already_sent {
                    tracing::info!(target: "gqy::qq", "\n{readable}");
                    return Ok(true);
                }
                tracing::info!(
                    target: "gqy::qq",
                    "{}",
                    t("suppressed an empty OneBot model reply", "已抑制空的 OneBot 模型回复")
                );
                return Ok(false);
            }
            context
                .send(OutboundMessage::segments(
                    OutboundOrigin::FinalReply,
                    segments,
                ))
                .await?;
            tracing::info!(target: "gqy::qq", "\n{readable}");
        }
    }
    Ok(true)
}

pub(crate) fn final_reply_text(outcome: &crate::web::TurnOutcome) -> String {
    crate::platforms::adapters::cut_suppressed_ranges(
        &outcome.text,
        &outcome.suppressed_reply_ranges,
    )
}

pub(crate) fn text_segment(text: &str) -> Value {
    json!({ "type": "text", "data": { "text": text } })
}
