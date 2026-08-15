//! tests4 — 自 src/platforms/onebot.rs 外移。
#![cfg(test)]

use super::*;

#[tokio::test]
async fn adapter_uses_the_new_connection_after_reconnect() {
    let (old_handle, mut old_frames) = test_connection(None);
    let adapter = Arc::new(test_adapter(old_handle, Target::Private { user_id: 42 }));
    let (new_handle, mut new_frames) = test_connection(None);
    adapter
        .registry
        .lock()
        .unwrap()
        .register(adapter.self_id, new_handle.clone());

    let send = {
        let adapter = adapter.clone();
        tokio::spawn(async move {
            adapter
                .send_message_segments(vec![text_segment("hello")])
                .await
        })
    };
    let frame: Value = serde_json::from_str(&new_frames.recv().await.unwrap()).unwrap();
    assert!(old_frames.try_recv().is_err());
    route_api_response(
        &new_handle,
        json!({
            "status": "ok",
            "retcode": 0,
            "data": { "message_id": 1 },
            "echo": frame["echo"],
        }),
    );
    assert!(send.await.unwrap().is_ok());
}

#[test]
fn group_mute_cache_expires_and_isolates_bot_accounts() {
    let start = Instant::now();
    let mut cache = GroupMuteCache::default();
    cache.insert(
        (10_001, 42),
        BotSendAvailability::Muted,
        Duration::from_secs(5),
        start,
    );
    cache.insert(
        (10_002, 42),
        BotSendAvailability::Available,
        Duration::from_secs(5),
        start,
    );
    assert_eq!(
        cache.get((10_001, 42), start),
        Some(BotSendAvailability::Muted)
    );
    assert_eq!(
        cache.get((10_002, 42), start),
        Some(BotSendAvailability::Available)
    );
    assert_eq!(
        cache.get((10_001, 42), start + Duration::from_secs(5)),
        None
    );
}

#[test]
fn ingress_order_is_strictly_monotonic() {
    let first = next_ingress_order();
    let second = next_ingress_order();
    assert!(second > first);
}

#[test]
fn group_ban_notices_update_bot_and_whole_group_mute_state() {
    let self_id = 91_001;
    let group_id = 92_001;
    group_mute_cache().lock().unwrap().remove_account(self_id);

    update_group_ban_notice(&json!({
        "post_type": "notice",
        "notice_type": "group_ban",
        "sub_type": "ban",
        "self_id": self_id,
        "group_id": group_id,
        "user_id": self_id,
        "duration": 120
    }));
    assert_eq!(
        group_mute_cache()
            .lock()
            .unwrap()
            .get((self_id, group_id), Instant::now()),
        Some(BotSendAvailability::Muted)
    );

    update_group_ban_notice(&json!({
        "post_type": "notice",
        "notice_type": "group_ban",
        "sub_type": "lift_ban",
        "self_id": self_id,
        "group_id": group_id,
        "user_id": self_id,
        "duration": 0
    }));
    assert_eq!(
        group_mute_cache()
            .lock()
            .unwrap()
            .get((self_id, group_id), Instant::now()),
        Some(BotSendAvailability::Available)
    );

    update_group_ban_notice(&json!({
        "post_type": "notice",
        "notice_type": "group_ban",
        "sub_type": "ban",
        "self_id": self_id,
        "group_id": group_id,
        "user_id": 0,
        "duration": 0
    }));
    assert_eq!(
        group_mute_cache()
            .lock()
            .unwrap()
            .get((self_id, group_id), Instant::now()),
        Some(BotSendAvailability::Muted)
    );
    group_mute_cache().lock().unwrap().remove_account(self_id);
}

#[tokio::test]
async fn bot_send_availability_queries_self_once_and_uses_the_cache() {
    let (handle, mut frames) = test_connection(None);
    let adapter = Arc::new(test_adapter(handle.clone(), Target::Group { group_id: 42 }));
    group_mute_cache()
        .lock()
        .unwrap()
        .remove_account(adapter.self_id);
    let lookup = {
        let adapter = adapter.clone();
        tokio::spawn(async move { adapter.bot_send_availability().await })
    };
    let frame: Value = serde_json::from_str(&frames.recv().await.unwrap()).unwrap();
    assert_eq!(frame["action"], "get_group_member_info");
    assert_eq!(frame["params"]["group_id"], 42);
    assert_eq!(frame["params"]["user_id"], adapter.self_id);
    route_api_response(
        &handle,
        json!({
            "status": "ok",
            "retcode": 0,
            "data": {
                "group_id": 42,
                "user_id": adapter.self_id,
                "shut_up_timestamp": unix_now() + 60
            },
            "echo": frame["echo"]
        }),
    );
    assert_eq!(lookup.await.unwrap().unwrap(), BotSendAvailability::Muted);
    assert_eq!(
        adapter.bot_send_availability().await.unwrap(),
        BotSendAvailability::Muted
    );
    assert!(frames.try_recv().is_err());
    group_mute_cache()
        .lock()
        .unwrap()
        .remove_account(adapter.self_id);
}

#[tokio::test]
async fn quoted_images_are_fetched_once_merged_and_bounded() {
    let (handle, mut frames) = test_connection(None);
    let mut parsed = InboundMessage {
        images: vec![MediaRef::Url("https://img.example/current.png".to_string())],
        reply_to_message_id: Some("91".to_string()),
        ..Default::default()
    };
    let lookup_handle = handle.clone();
    let lookup = tokio::spawn(async move {
        let added = merge_quoted_message_images(&lookup_handle, "90", &mut parsed, None).await?;
        Result::<_>::Ok((added, parsed))
    });

    let frame: Value = serde_json::from_str(&frames.recv().await.unwrap()).unwrap();
    assert_eq!(frame["action"], "get_msg");
    assert_eq!(frame["params"]["message_id"], 91);
    route_api_response(
        &handle,
        json!({
            "status": "ok",
            "retcode": 0,
            "data": {
                "message_id": 91,
                "message": [
                    { "type": "reply", "data": { "id": 80 } },
                    { "type": "image", "data": { "url": "https://img.example/current.png" } },
                    { "type": "image", "data": { "file": "base64://AQ==" } },
                    { "type": "image", "data": { "file": "base64://Ag==" } },
                    { "type": "image", "data": { "file": "base64://Aw==" } },
                    { "type": "image", "data": { "file": "base64://BA==" } }
                ]
            },
            "echo": frame["echo"],
        }),
    );
    let (added, parsed) = lookup.await.unwrap().unwrap();
    assert_eq!(added, 3);
    assert_eq!(parsed.images.len(), MAX_INBOUND_IMAGES);
    assert!(matches!(&parsed.images[0], MediaRef::Url(url) if url.ends_with("current.png")));
    assert!(matches!(&parsed.images[1], MediaRef::Bytes(bytes) if bytes == &[1]));
    assert!(matches!(&parsed.images[3], MediaRef::Bytes(bytes) if bytes == &[3]));
    assert!(
        frames.try_recv().is_err(),
        "nested replies must not be fetched"
    );

    let mut self_reply = InboundMessage {
        reply_to_message_id: Some("90".to_string()),
        ..Default::default()
    };
    assert_eq!(
        merge_quoted_message_images(&handle, "90", &mut self_reply, None)
            .await
            .unwrap(),
        0
    );
    assert!(frames.try_recv().is_err());
}

#[tokio::test]
async fn preloaded_quoted_metadata_avoids_a_second_message_lookup() {
    let (handle, mut frames) = test_connection(None);
    let mut parsed = InboundMessage {
        reply_to_message_id: Some("91".to_string()),
        ..Default::default()
    };
    let data = json!({
        "message_id": 91,
        "sender": { "user_id": 8, "nickname": "eight" },
        "message": [{ "type": "image", "data": { "file": "base64://AQ==" } }]
    });

    assert_eq!(
        merge_quoted_message_images(&handle, "90", &mut parsed, Some(&data))
            .await
            .unwrap(),
        1
    );
    assert!(frames.try_recv().is_err());
}

#[tokio::test]
async fn quoted_napcat_file_image_uses_get_image_fallback() {
    let (handle, mut frames) = test_connection(None);
    let mut parsed = InboundMessage {
        reply_to_message_id: Some("91".to_string()),
        ..Default::default()
    };
    let lookup_handle = handle.clone();
    let lookup = tokio::spawn(async move {
        let added = merge_quoted_message_images(&lookup_handle, "90", &mut parsed, None).await?;
        Result::<_>::Ok((added, parsed))
    });

    let get_msg: Value = serde_json::from_str(&frames.recv().await.unwrap()).unwrap();
    assert_eq!(get_msg["action"], "get_msg");
    route_api_response(
        &handle,
        json!({
            "status": "ok",
            "retcode": 0,
            "data": {
                "message_id": 91,
                // NapCat get_msg disables URL resolution and normally
                // exposes only the registered image file identifier.
                "message": [{
                    "type": "image",
                    "data": { "file": "napcat-image.jpg", "url": "" }
                }]
            },
            "echo": get_msg["echo"],
        }),
    );

    let get_image: Value = serde_json::from_str(&frames.recv().await.unwrap()).unwrap();
    assert_eq!(get_image["action"], "get_image");
    assert_eq!(get_image["params"]["file"], "napcat-image.jpg");
    route_api_response(
        &handle,
        json!({
            "status": "ok",
            "retcode": 0,
            "data": {
                "file": "/tmp/napcat-image.jpg",
                "url": "https://img.example/quoted.jpg"
            },
            "echo": get_image["echo"],
        }),
    );

    let (added, parsed) = lookup.await.unwrap().unwrap();
    assert_eq!(added, 1);
    assert!(matches!(
        &parsed.images[0],
        MediaRef::Url(url) if url == "https://img.example/quoted.jpg"
    ));
    assert!(frames.try_recv().is_err());
}

#[tokio::test]
async fn current_napcat_file_image_uses_get_image_fallback() {
    let (handle, mut frames) = test_connection(None);
    let message = json!([{
        "type": "image",
        "data": { "file": "current-napcat-image.jpg", "url": "" }
    }]);
    let mut parsed = parse_message(Some(&message), None, 10001);
    assert!(parsed.images.is_empty());
    assert_eq!(parsed.unresolved_image_files, ["current-napcat-image.jpg"]);
    let lookup_handle = handle.clone();
    let lookup = tokio::spawn(async move {
        resolve_current_message_images(&lookup_handle, &mut parsed).await;
        parsed
    });

    let get_image: Value = serde_json::from_str(&frames.recv().await.unwrap()).unwrap();
    assert_eq!(get_image["action"], "get_image");
    assert_eq!(get_image["params"]["file"], "current-napcat-image.jpg");
    route_api_response(
        &handle,
        json!({
            "status": "ok",
            "retcode": 0,
            "data": { "base64": "AQID" },
            "echo": get_image["echo"],
        }),
    );
    let parsed = lookup.await.unwrap();
    assert!(parsed.unresolved_image_files.is_empty());
    assert!(matches!(&parsed.images[0], MediaRef::Bytes(bytes) if bytes == &[1, 2, 3]));
}

#[tokio::test]
async fn adapter_history_images_preserve_order_and_reject_other_groups() {
    let (handle, mut frames) = test_connection(None);
    let adapter = Arc::new(test_adapter(handle.clone(), Target::Group { group_id: 42 }));
    let lookup = {
        let adapter = adapter.clone();
        tokio::spawn(async move { adapter.message_images("90").await })
    };
    let get_msg: Value = serde_json::from_str(&frames.recv().await.unwrap()).unwrap();
    route_api_response(
        &handle,
        json!({
            "status": "ok",
            "retcode": 0,
            "data": {
                "message_id": 90,
                "message_type": "group",
                "group_id": 42,
                "sender": { "user_id": 7, "nickname": "sender" },
                "message": [
                    { "type": "image", "data": { "file": "base64://AQID" } },
                    { "type": "image", "data": { "file": "base64://AQID" } }
                ]
            },
            "echo": get_msg["echo"],
        }),
    );
    let images = lookup.await.unwrap().unwrap();
    assert_eq!(images.len(), 2);
    assert_eq!(&*images[0].data, &[1, 2, 3]);
    assert_eq!(&*images[1].data, &[1, 2, 3]);

    let rejected = {
        let adapter = adapter.clone();
        tokio::spawn(async move { adapter.message_images("91").await })
    };
    let get_msg: Value = serde_json::from_str(&frames.recv().await.unwrap()).unwrap();
    route_api_response(
        &handle,
        json!({
            "status": "ok",
            "retcode": 0,
            "data": {
                "message_id": 91,
                "message_type": "group",
                "group_id": 99,
                "sender": { "user_id": 8, "nickname": "other" },
                "message": [{ "type": "image", "data": { "file": "base64://BAUG" } }]
            },
            "echo": get_msg["echo"],
        }),
    );
    let error = rejected.await.unwrap().unwrap_err();
    assert!(error
        .to_string()
        .contains("belongs to another conversation"));
}

#[tokio::test]
async fn adapter_exposes_reactions_message_details_and_group_members() {
    let (handle, mut frames) = test_connection(None);
    let adapter = Arc::new(test_adapter(handle.clone(), Target::Group { group_id: 42 }));

    let reaction = {
        let adapter = adapter.clone();
        tokio::spawn(async move { adapter.set_message_reaction("90", "289", true).await })
    };
    let frame: Value = serde_json::from_str(&frames.recv().await.unwrap()).unwrap();
    assert_eq!(frame["action"], "set_msg_emoji_like");
    assert_eq!(frame["params"]["message_id"], 90);
    assert_eq!(frame["params"]["emoji_id"], 289);
    assert_eq!(frame["params"]["set"], true);
    route_api_response(
        &handle,
        json!({ "status": "ok", "retcode": 0, "data": null, "echo": frame["echo"] }),
    );
    reaction.await.unwrap().unwrap();

    let members = {
        let adapter = adapter.clone();
        tokio::spawn(async move { adapter.group_members().await })
    };
    let frame: Value = serde_json::from_str(&frames.recv().await.unwrap()).unwrap();
    assert_eq!(frame["action"], "get_group_member_list");
    assert_eq!(frame["params"]["group_id"], 42);
    route_api_response(
        &handle,
        json!({
            "status": "ok",
            "retcode": 0,
            "data": [{
                "group_id": 42,
                "user_id": 7,
                "nickname": "nick",
                "card": "card",
                "role": "admin",
                "join_time": 10,
                "last_sent_time": 20
            }],
            "echo": frame["echo"],
        }),
    );
    let members = members.await.unwrap().unwrap();
    assert_eq!(members.len(), 1);
    assert_eq!(members[0].user_id, "7");
    assert_eq!(members[0].display_name(), "card");
    assert_eq!(members[0].role, "admin");

    let member = {
        let adapter = adapter.clone();
        tokio::spawn(async move { adapter.group_member("8").await })
    };
    let frame: Value = serde_json::from_str(&frames.recv().await.unwrap()).unwrap();
    assert_eq!(frame["action"], "get_group_member_info");
    assert_eq!(frame["params"]["user_id"], 8);
    route_api_response(
        &handle,
        json!({
            "status": "ok",
            "retcode": 0,
            "data": { "group_id": 42, "user_id": 8, "nickname": "eight" },
            "echo": frame["echo"],
        }),
    );
    assert_eq!(member.await.unwrap().unwrap().unwrap().nickname, "eight");

    let info = {
        let adapter = adapter.clone();
        tokio::spawn(async move { adapter.message_info("91").await })
    };
    let frame: Value = serde_json::from_str(&frames.recv().await.unwrap()).unwrap();
    assert_eq!(frame["action"], "get_msg");
    route_api_response(
        &handle,
        json!({
            "status": "ok",
            "retcode": 0,
            "data": {
                "message_id": 91,
                "time": 123,
                "sender": { "user_id": 8, "nickname": "eight" },
                "message": [
                    { "type": "reply", "data": { "id": 80 } },
                    { "type": "at", "data": { "qq": 9 } },
                    { "type": "text", "data": { "text": "hello" } }
                ]
            },
            "echo": frame["echo"],
        }),
    );
    let info = info.await.unwrap().unwrap().unwrap();
    assert_eq!(info.message_id, "91");
    assert_eq!(info.sender_id, "8");
    assert_eq!(info.text, "hello");
    assert_eq!(info.reply_to_message_id.as_deref(), Some("80"));
    assert_eq!(info.mentioned_user_ids, vec!["9"]);
}

#[tokio::test]
async fn file_upload_falls_back_to_base64_after_url_failure() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("sample.txt");
    tokio::fs::write(&path, b"hello").await.unwrap();
    let (handle, mut frames) = test_connection(Some("http://gqy.test:8300".to_string()));
    let adapter = test_adapter(handle.clone(), Target::Private { user_id: 42 });
    let upload = tokio::spawn(async move { adapter.upload_file(&path, None).await });

    let first: Value = serde_json::from_str(&frames.recv().await.unwrap()).unwrap();
    assert_eq!(first["action"], "upload_private_file");
    assert!(first["params"]["file"]
        .as_str()
        .unwrap()
        .starts_with("http://gqy.test:8300/api/platform-assets/"));
    route_api_response(
        &handle,
        json!({
            "status": "failed",
            "retcode": 100,
            "data": null,
            "echo": first["echo"],
        }),
    );

    let second: Value = serde_json::from_str(&frames.recv().await.unwrap()).unwrap();
    assert_eq!(second["action"], "upload_private_file");
    assert_eq!(second["params"]["file"], "base64://aGVsbG8=");
    route_api_response(
        &handle,
        json!({
            "status": "ok",
            "retcode": 0,
            "data": { "file_id": "file-1" },
            "echo": second["echo"],
        }),
    );
    assert_eq!(upload.await.unwrap().unwrap().as_deref(), Some("file-1"));
}

#[tokio::test]
async fn adapter_reports_confirmed_images_on_later_attachment_failure() {
    let temp = tempfile::tempdir().unwrap();
    let missing_file = temp.path().join("missing.txt");
    let (handle, mut frames) = test_connection(None);
    let adapter = Arc::new(test_adapter(handle.clone(), Target::Private { user_id: 7 }));
    let send = {
        let adapter = adapter.clone();
        tokio::spawn(async move {
            adapter
                .send_message(OutboundMessage::segments(
                    OutboundOrigin::Tool,
                    vec![
                        OutboundSegment::ImageBytes {
                            mime: "image/png".to_string(),
                            data: Arc::from([1_u8, 2, 3]),
                            alt: "sample".to_string(),
                        },
                        OutboundSegment::FilePath {
                            path: missing_file,
                            name: None,
                        },
                    ],
                ))
                .await
        })
    };

    let frame: Value = serde_json::from_str(
        &tokio::time::timeout(Duration::from_secs(1), frames.recv())
            .await
            .expect("image send timed out")
            .expect("image frame channel closed"),
    )
    .unwrap();
    route_api_response(
        &handle,
        json!({
            "status": "ok",
            "retcode": 0,
            "data": { "message_id": 122 },
            "echo": frame["echo"],
        }),
    );

    let error = send.await.unwrap().unwrap_err();
    let partial = error
        .downcast_ref::<PartialSendError>()
        .expect("partial send error");
    assert_eq!(partial.receipt().delivered_parts, 1);
    assert_eq!(partial.receipt().message_ids, vec!["122"]);
    assert_eq!(
        partial.receipt().image_digests,
        vec![blake3::hash(&[1_u8, 2, 3])]
    );
    assert!(frames.try_recv().is_err());
}

#[tokio::test]
async fn adapter_smoke_test_sends_replies_images_and_forward_nodes() {
    let (handle, mut frames) = test_connection(None);
    let adapter = Arc::new(test_adapter(handle.clone(), Target::Group { group_id: 42 }));
    let mut message = OutboundMessage::segments(
        OutboundOrigin::FinalReply,
        vec![
            OutboundSegment::Text("hello".to_string()),
            OutboundSegment::ImageBytes {
                mime: "image/png".to_string(),
                data: Arc::from([1_u8, 2, 3]),
                alt: "sample".to_string(),
            },
        ],
    );
    message.response_target = Some(ResponseTarget {
        message_id: "99".to_string(),
        user_id: "77".to_string(),
        quote: true,
        mention: true,
        explicit_mention_user_ids: Vec::new(),
    });
    let send = {
        let adapter = adapter.clone();
        tokio::spawn(async move { adapter.send_message(message).await })
    };
    let frame: Value = serde_json::from_str(&frames.recv().await.unwrap()).unwrap();
    assert_eq!(frame["action"], "send_group_msg");
    assert_eq!(frame["params"]["group_id"], 42);
    assert_eq!(frame["params"]["message"][0]["type"], "reply");
    assert_eq!(frame["params"]["message"][1]["type"], "at");
    assert_eq!(frame["params"]["message"][1]["data"]["qq"], "77");
    assert_eq!(frame["params"]["message"][2]["data"]["text"], " ");
    assert_eq!(frame["params"]["message"][3]["data"]["text"], "hello");
    assert_eq!(
        frame["params"]["message"][4]["data"]["file"],
        "base64://AQID"
    );
    route_api_response(
        &handle,
        json!({
            "status": "ok",
            "retcode": 0,
            "data": { "message_id": 123 },
            "echo": frame["echo"],
        }),
    );
    let receipt = send.await.unwrap().unwrap();
    assert_eq!(receipt.message_ids, vec!["123"]);
    assert_eq!(receipt.image_message_ids, vec!["123"]);
    assert_eq!(receipt.delivered_parts, 1);
    assert_eq!(receipt.image_digests, vec![blake3::hash(&[1_u8, 2, 3])]);

    let forward = OutboundMessage {
        body: OutboundBody::Forward(vec![ForwardNode {
            user_id: "10000".to_string(),
            display_name: "GQY".to_string(),
            segments: vec![OutboundSegment::Markdown("**long**".to_string())],
        }]),
        response_target: Some(ResponseTarget {
            message_id: "98".to_string(),
            user_id: "76".to_string(),
            quote: true,
            mention: true,
            explicit_mention_user_ids: Vec::new(),
        }),
        origin: OutboundOrigin::Plugin,
        metadata: Default::default(),
    };
    let send = {
        let adapter = adapter.clone();
        tokio::spawn(async move { adapter.send_message(forward).await })
    };
    let frame: Value = serde_json::from_str(&frames.recv().await.unwrap()).unwrap();
    assert_eq!(frame["action"], "send_group_forward_msg");
    assert_eq!(frame["params"]["messages"][0]["type"], "node");
    assert_eq!(
        frame["params"]["messages"][0]["data"]["content"][0]["data"]["text"],
        "long"
    );
    route_api_response(
        &handle,
        json!({
            "status": "ok",
            "retcode": 0,
            "data": { "message_id": "forward-1" },
            "echo": frame["echo"],
        }),
    );
    let marker: Value = serde_json::from_str(&frames.recv().await.unwrap()).unwrap();
    assert_eq!(marker["action"], "send_group_msg");
    assert_eq!(marker["params"]["message"][0]["type"], "reply");
    assert_eq!(marker["params"]["message"][0]["data"]["id"], "98");
    assert_eq!(marker["params"]["message"][1]["type"], "at");
    assert_eq!(marker["params"]["message"][1]["data"]["qq"], "76");
    assert_eq!(marker["params"]["message"][2]["data"]["text"], " ");
    route_api_response(
        &handle,
        json!({
            "status": "ok",
            "retcode": 0,
            "data": { "message_id": "marker-1" },
            "echo": marker["echo"],
        }),
    );
    assert_eq!(
        send.await.unwrap().unwrap().message_ids,
        vec!["forward-1", "marker-1"]
    );
}

#[tokio::test]
async fn split_replies_encode_the_response_target_only_on_the_first_frame() {
    let (handle, mut frames) = test_connection(None);
    let mut adapter = test_adapter(handle.clone(), Target::Group { group_id: 42 });
    adapter.max_reply_chars = 3;
    let adapter = Arc::new(adapter);
    let mut message = OutboundMessage::text(OutboundOrigin::FinalReply, "abcdef");
    message.response_target = Some(ResponseTarget {
        message_id: "99".to_string(),
        user_id: "7".to_string(),
        quote: true,
        mention: true,
        explicit_mention_user_ids: Vec::new(),
    });
    let send = {
        let adapter = adapter.clone();
        tokio::spawn(async move { adapter.send_message(message).await })
    };

    let first: Value = serde_json::from_str(&frames.recv().await.unwrap()).unwrap();
    assert_eq!(first["params"]["message"][0]["type"], "reply");
    assert_eq!(first["params"]["message"][1]["type"], "at");
    assert_eq!(first["params"]["message"][2]["data"]["text"], " ");
    route_api_response(
        &handle,
        json!({
            "status": "ok",
            "retcode": 0,
            "data": { "message_id": 1 },
            "echo": first["echo"],
        }),
    );

    let second: Value = serde_json::from_str(&frames.recv().await.unwrap()).unwrap();
    assert_eq!(second["params"]["message"][0]["type"], "text");
    assert!(second["params"]["message"]
        .as_array()
        .unwrap()
        .iter()
        .all(|segment| !matches!(segment["type"].as_str(), Some("reply" | "at"))));
    route_api_response(
        &handle,
        json!({
            "status": "ok",
            "retcode": 0,
            "data": { "message_id": 2 },
            "echo": second["echo"],
        }),
    );
    let receipt = send.await.unwrap().unwrap();
    assert_eq!(receipt.message_ids, vec!["1", "2"]);
    assert!(receipt.response_target_delivered);
}

#[tokio::test]
async fn split_failure_reports_that_the_response_target_was_delivered() {
    let (handle, mut frames) = test_connection(None);
    let mut adapter = test_adapter(handle.clone(), Target::Group { group_id: 42 });
    adapter.max_reply_chars = 3;
    let adapter = Arc::new(adapter);
    let mut message = OutboundMessage::text(OutboundOrigin::FinalReply, "abcdef");
    message.response_target = Some(ResponseTarget {
        message_id: String::new(),
        user_id: String::new(),
        quote: false,
        mention: false,
        explicit_mention_user_ids: vec!["30000".to_string(), "40000".to_string()],
    });
    let send = {
        let adapter = adapter.clone();
        tokio::spawn(async move { adapter.send_message(message).await })
    };

    let first: Value = serde_json::from_str(&frames.recv().await.unwrap()).unwrap();
    assert_eq!(first["params"]["message"][0]["data"]["qq"], "30000");
    assert_eq!(first["params"]["message"][2]["data"]["qq"], "40000");
    route_api_response(
        &handle,
        json!({
            "status": "ok",
            "retcode": 0,
            "data": { "message_id": 1 },
            "echo": first["echo"],
        }),
    );

    let second: Value = serde_json::from_str(&frames.recv().await.unwrap()).unwrap();
    route_api_response(
        &handle,
        json!({
            "status": "failed",
            "retcode": 100,
            "data": null,
            "echo": second["echo"],
        }),
    );
    let error = send.await.unwrap().unwrap_err();
    let partial = error.downcast_ref::<PartialSendError>().unwrap();
    assert_eq!(partial.receipt().delivered_parts, 1);
    assert!(partial.receipt().response_target_delivered);
}

#[tokio::test]
async fn forward_marker_failure_is_reported_as_partial_delivery() {
    let (handle, mut frames) = test_connection(None);
    let adapter = Arc::new(test_adapter(handle.clone(), Target::Group { group_id: 42 }));
    let message = OutboundMessage {
        body: OutboundBody::Forward(vec![ForwardNode {
            user_id: "10000".to_string(),
            display_name: "GQY".to_string(),
            segments: vec![OutboundSegment::Text("forward".to_string())],
        }]),
        response_target: Some(ResponseTarget {
            message_id: String::new(),
            user_id: String::new(),
            quote: false,
            mention: false,
            explicit_mention_user_ids: vec!["30000".to_string()],
        }),
        origin: OutboundOrigin::FinalReply,
        metadata: Default::default(),
    };
    let send = {
        let adapter = adapter.clone();
        tokio::spawn(async move { adapter.send_message(message).await })
    };

    let forward: Value = serde_json::from_str(&frames.recv().await.unwrap()).unwrap();
    assert_eq!(forward["action"], "send_group_forward_msg");
    route_api_response(
        &handle,
        json!({
            "status": "ok",
            "retcode": 0,
            "data": { "message_id": "forward-1" },
            "echo": forward["echo"],
        }),
    );

    let marker: Value = serde_json::from_str(&frames.recv().await.unwrap()).unwrap();
    assert_eq!(marker["action"], "send_group_msg");
    route_api_response(
        &handle,
        json!({
            "status": "failed",
            "retcode": 100,
            "data": null,
            "echo": marker["echo"],
        }),
    );

    let error = send.await.unwrap().unwrap_err();
    let partial = error.downcast_ref::<PartialSendError>().unwrap();
    assert_eq!(partial.receipt().delivered_parts, 1);
    assert!(!partial.receipt().response_target_delivered);
}

#[tokio::test]
async fn invalid_attachment_does_not_send_a_bare_response_marker() {
    let temp = tempfile::tempdir().unwrap();
    let missing = temp.path().join("missing.txt");
    let (handle, mut frames) = test_connection(None);
    let adapter = test_adapter(handle, Target::Group { group_id: 42 });
    let message = OutboundMessage::segments(
        OutboundOrigin::FinalReply,
        vec![OutboundSegment::FilePath {
            path: missing,
            name: None,
        }],
    );
    let mut message = message;
    message.response_target = Some(ResponseTarget {
        message_id: String::new(),
        user_id: String::new(),
        quote: false,
        mention: false,
        explicit_mention_user_ids: vec!["30000".to_string()],
    });

    assert!(adapter.send_message(message).await.is_err());
    assert!(frames.try_recv().is_err());
}
