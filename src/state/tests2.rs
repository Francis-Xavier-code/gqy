//! tests2 — 自 src/state/mod.rs 外移。
#![cfg(test)]

use super::tests::*;

#[test]
fn local_session_listing_excludes_platform_owned_history() {
    let (_temp, store) = test_store();
    let local = store
        .create_session("gqy", "shared name", "user", None)
        .unwrap();
    let platform = store
        .create_session("gqy", "shared name", "user", None)
        .unwrap();
    let key = platform_binding_key("20000", None, "gqy");
    store
        .bind_platform_session(&key, &platform.session_id)
        .unwrap();

    let all_ids = store
        .list_sessions("gqy")
        .unwrap()
        .into_iter()
        .map(|overview| overview.record.session_id)
        .collect::<Vec<_>>();
    assert!(all_ids.contains(&local.session_id));
    assert!(all_ids.contains(&platform.session_id));

    let local_ids = store
        .list_local_sessions("gqy")
        .unwrap()
        .into_iter()
        .map(|overview| overview.record.session_id)
        .collect::<Vec<_>>();
    assert!(local_ids.contains(&local.session_id));
    assert!(!local_ids.contains(&platform.session_id));
    assert!(!store.is_platform_session(&local.session_id).unwrap());
    assert!(store.is_platform_session(&platform.session_id).unwrap());
    assert_eq!(
        store
            .find_local_session_by_name("gqy", "SHARED NAME")
            .unwrap()
            .unwrap()
            .session_id,
        local.session_id
    );
}

#[test]
fn platform_binding_overwrite_and_conflict_are_atomic() {
    let (_temp, store) = test_store();
    let session_a = store.create_session("gqy", "a", "user", None).unwrap();
    let session_b = store.create_session("gqy", "b", "user", None).unwrap();
    let session_c = store.create_session("gqy", "c", "user", None).unwrap();
    let key_a = platform_binding_key("group-a", None, "gqy");
    let key_b = platform_binding_key("group-b", None, "gqy");

    store
        .bind_platform_session(&key_a, &session_a.session_id)
        .unwrap();
    store
        .bind_platform_session(&key_b, &session_b.session_id)
        .unwrap();

    let error = store
        .bind_platform_session(&key_a, &session_b.session_id)
        .unwrap_err();
    assert!(error.to_string().contains("already bound"));
    assert_eq!(
        store.find_platform_session_binding(&key_a).unwrap(),
        Some(session_a.session_id)
    );
    assert_eq!(
        store.find_platform_session_binding(&key_b).unwrap(),
        Some(session_b.session_id)
    );

    store
        .bind_platform_session(&key_a, &session_c.session_id)
        .unwrap();
    assert_eq!(
        store.find_platform_session_binding(&key_a).unwrap(),
        Some(session_c.session_id)
    );
    assert!(store.unbind_platform_session(&key_a).unwrap());
    assert!(!store.unbind_platform_session(&key_a).unwrap());
}

#[test]
fn concurrent_platform_bind_rejects_session_sharing() {
    let (temp, store) = test_store();
    let second_store = StateStore::new(&test_paths(temp.path())).unwrap();
    let session = store
        .create_session("gqy", "shared target", "user", None)
        .unwrap();
    let barrier = Arc::new(std::sync::Barrier::new(3));
    let handles = [store.clone(), second_store]
        .into_iter()
        .zip(["group-a", "group-b"])
        .map(|(store, conversation_id)| {
            let barrier = barrier.clone();
            let session_id = session.session_id.clone();
            let key = platform_binding_key(conversation_id, None, "gqy");
            std::thread::spawn(move || {
                barrier.wait();
                let result = store.bind_platform_session(&key, &session_id);
                (key, result)
            })
        })
        .collect::<Vec<_>>();
    barrier.wait();
    let results = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect::<Vec<_>>();

    assert_eq!(
        results.iter().filter(|(_, result)| result.is_ok()).count(),
        1
    );
    assert_eq!(
        results.iter().filter(|(_, result)| result.is_err()).count(),
        1
    );
    assert_eq!(
        results
            .iter()
            .filter(|(key, _)| store.find_platform_session_binding(key).unwrap().is_some())
            .count(),
        1
    );
}

#[test]
fn concurrent_platform_claim_converges_on_one_session() {
    let (temp, store) = test_store();
    let second_store = StateStore::new(&test_paths(temp.path())).unwrap();
    let session_a = store.create_session("gqy", "a", "user", None).unwrap();
    let session_b = store.create_session("gqy", "b", "user", None).unwrap();
    let key = platform_binding_key("same-group", None, "gqy");
    let barrier = Arc::new(std::sync::Barrier::new(3));
    let handles = [
        (store.clone(), session_a.session_id.clone()),
        (second_store, session_b.session_id.clone()),
    ]
    .into_iter()
    .map(|(store, candidate)| {
        let barrier = barrier.clone();
        let key = key.clone();
        std::thread::spawn(move || {
            barrier.wait();
            store.claim_platform_session(&key, &candidate).unwrap()
        })
    })
    .collect::<Vec<_>>();
    barrier.wait();
    let winners = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect::<Vec<_>>();

    assert_eq!(winners[0], winners[1]);
    assert_eq!(
        store.find_platform_session_binding(&key).unwrap(),
        Some(winners[0].clone())
    );
    assert!(winners[0] == session_a.session_id || winners[0] == session_b.session_id);
}

#[test]
fn platform_session_creation_is_bound_atomically() {
    let (_temp, store) = test_store();
    let key = platform_binding_key("atomic-group", None, "gqy");
    let (platform, created) = store
        .create_or_get_platform_session(&key, "platform")
        .unwrap();
    assert!(created);
    assert_eq!(
        store.find_platform_session_binding(&key).unwrap(),
        Some(platform.session_id.clone())
    );
    assert!(!store
        .list_local_sessions("gqy")
        .unwrap()
        .iter()
        .any(|entry| entry.record.session_id == platform.session_id));

    let (same, created) = store
        .create_or_get_platform_session(&key, "ignored")
        .unwrap();
    assert!(!created);
    assert_eq!(same.session_id, platform.session_id);
}

#[test]
fn platform_plugin_json_is_shared_across_personas_and_supports_deletion() {
    let (_temp, store) = test_store();
    let scope = plugin_scope("20000");
    let value = vec!["image-a".to_string(), "image-b".to_string()];
    store
        .plugin_put_json(&scope, "recent_images", &value)
        .unwrap();
    let replacement = vec!["image-c".to_string()];
    store
        .plugin_put_json(&scope, "recent_images", &replacement)
        .unwrap();

    // Pinned stores represent independent persona sessions but share the
    // external-conversation plugin scope.
    let gqy_session = store.create_session("gqy", "gqy", "user", None).unwrap();
    let other_session = store
        .create_session("other", "other", "user", None)
        .unwrap();
    let gqy_store = store.pinned(&gqy_session.session_id);
    let other_store = store.pinned(&other_session.session_id);
    let from_gqy: Option<Vec<String>> = gqy_store.plugin_get_json(&scope, "recent_images").unwrap();
    let from_other: Option<Vec<String>> = other_store
        .plugin_get_json(&scope, "recent_images")
        .unwrap();
    assert_eq!(from_gqy, Some(replacement.clone()));
    assert_eq!(from_other, Some(replacement));

    store.plugin_put_json(&scope, "mode", &"image").unwrap();
    assert!(store.plugin_delete_key(&scope, "recent_images").unwrap());
    let deleted: Option<Vec<String>> = store.plugin_get_json(&scope, "recent_images").unwrap();
    assert_eq!(deleted, None);
    assert_eq!(store.plugin_delete_scope(&scope).unwrap(), 1);
    assert!(!store.plugin_delete_key(&scope, "mode").unwrap());
}

#[test]
fn concurrent_platform_plugin_updates_do_not_lose_values() {
    let (temp, first) = test_store();
    let second = StateStore::new(&test_paths(temp.path())).unwrap();
    let scope = plugin_scope("atomic-group");
    let barrier = Arc::new(std::sync::Barrier::new(9));
    let handles = (0..8)
        .map(|value| {
            let store = if value % 2 == 0 {
                first.clone()
            } else {
                second.clone()
            };
            let scope = scope.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                store
                    .plugin_update_json(&scope, "values", |current: Option<Vec<usize>>| {
                        let mut values = current.unwrap_or_default();
                        values.push(value);
                        Ok(Some(values))
                    })
                    .unwrap();
            })
        })
        .collect::<Vec<_>>();
    barrier.wait();
    for handle in handles {
        handle.join().unwrap();
    }

    let mut values: Vec<usize> = first.plugin_get_json(&scope, "values").unwrap().unwrap();
    values.sort_unstable();
    assert_eq!(values, (0..8).collect::<Vec<_>>());
}

fn platform_meme_ref(
    conversation_id: &str,
    message_id: &str,
    library: &str,
    meme_id: &str,
    direction: &str,
    created_at: &str,
) -> PlatformMemeRefRecord {
    PlatformMemeRefRecord {
        platform: "onebot".to_string(),
        account_id: "10000".to_string(),
        conversation_kind: "group".to_string(),
        conversation_id: conversation_id.to_string(),
        message_id: message_id.to_string(),
        library: library.to_string(),
        meme_id: meme_id.to_string(),
        direction: direction.to_string(),
        created_at: created_at.to_string(),
    }
}

#[test]
fn platform_meme_refs_are_ordered_isolated_upserted_and_cleaned_by_ref() {
    let (_temp, store) = test_store();
    let later = platform_meme_ref(
        "group-a",
        "message-1",
        "secondary",
        "meme-b",
        "outbound",
        "2026-01-02T00:00:00Z",
    );
    let earlier = platform_meme_ref(
        "group-a",
        "message-1",
        "default",
        "meme-a",
        "inbound",
        "2026-01-01T00:00:00Z",
    );
    let other_conversation = platform_meme_ref(
        "group-b",
        "message-1",
        "default",
        "meme-a",
        "inbound",
        "2026-01-03T00:00:00Z",
    );
    store.put_platform_meme_ref(&later).unwrap();
    store.put_platform_meme_ref(&earlier).unwrap();
    store.put_platform_meme_ref(&other_conversation).unwrap();

    assert_eq!(
        store
            .platform_meme_refs_for_message("onebot", "10000", "group", "group-a", "message-1")
            .unwrap(),
        vec![earlier.clone(), later]
    );

    let mut updated = earlier;
    updated.direction = "outbound".to_string();
    updated.created_at = "2026-01-04T00:00:00Z".to_string();
    store.put_platform_meme_ref(&updated).unwrap();
    let records = store
        .platform_meme_refs_for_message("onebot", "10000", "group", "group-a", "message-1")
        .unwrap();
    assert_eq!(records.len(), 2);
    assert_eq!(records[1], updated);

    assert_eq!(
        store.delete_platform_meme_ref("default", "meme-a").unwrap(),
        2
    );
    assert!(store
        .platform_meme_refs_for_message("onebot", "10000", "group", "group-b", "message-1")
        .unwrap()
        .is_empty());
    assert_eq!(
        store
            .platform_meme_refs_for_message("onebot", "10000", "group", "group-a", "message-1")
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn platform_meme_ref_rejects_invalid_direction() {
    let (_temp, store) = test_store();
    let record = platform_meme_ref(
        "group-a",
        "message-1",
        "default",
        "meme-a",
        "sideways",
        "2026-01-01T00:00:00Z",
    );
    assert!(store.put_platform_meme_ref(&record).is_err());
    assert!(store
        .platform_meme_refs_for_message("onebot", "10000", "group", "group-a", "message-1")
        .unwrap()
        .is_empty());
}

#[test]
fn wiping_the_persona_takes_the_subagent_rows_with_it() {
    let temp = tempfile::tempdir().unwrap();
    let store = StateStore::new(&test_paths(temp.path())).unwrap();
    store.adopt_sessions_for_persona("gqy").unwrap();
    let parent = store.session_id();
    let audit = store
        .create_session("gqy", "深挖", "subagent", Some(&parent))
        .unwrap();
    store
        .record_subagent_usage(&audit.session_id, None, None, None, 400, 100, 500, 200)
        .unwrap();
    assert_eq!(store.session_cumulative_token_totals().unwrap().total, 500);

    // Subagent usage lives on the session row, not in `turns` — clearing
    // the turns alone left every Σ still carrying it.
    store.reset_persona_contexts("gqy", "onebot").unwrap();
    assert_eq!(
        store.session_cumulative_token_totals().unwrap(),
        TurnTokens::default()
    );
}

#[test]
fn a_subagents_tokens_land_in_the_launching_sessions_total() {
    let temp = tempfile::tempdir().unwrap();
    let store = StateStore::new(&test_paths(temp.path())).unwrap();
    store.adopt_sessions_for_persona("gqy").unwrap();
    let parent = store.session_id();

    let turn_id = "turn_parent_1";
    store
        .start_turn(turn_id, "问题", std::process::id())
        .unwrap();
    store
        .complete_turn_with_usage_and_model(
            turn_id,
            "答案",
            None,
            None,
            None,
            TurnTokens {
                total: 1_000,
                prompt: 900,
                cache_read: 300,
            },
            false,
        )
        .unwrap();
    assert_eq!(
        store.session_cumulative_token_totals().unwrap(),
        TurnTokens {
            total: 1_000,
            prompt: 900,
            cache_read: 300
        }
    );

    let audit = store
        .create_session("gqy", "深挖", "subagent", Some(&parent))
        .unwrap();
    store
        .record_subagent_usage(&audit.session_id, None, None, None, 400, 100, 500, 200)
        .unwrap();

    // A subagent bills to the session that launched it, cache hits and all
    // — otherwise the most expensive thing a turn can do is invisible.
    assert_eq!(
        store.session_cumulative_token_totals().unwrap(),
        TurnTokens {
            total: 1_500,
            prompt: 1_300,
            cache_read: 500
        }
    );

    // A reset that left the audit sessions behind would zero the history
    // and still report a running total.
    store.reset_conversation().unwrap();
    assert_eq!(
        store.session_cumulative_token_totals().unwrap(),
        TurnTokens::default()
    );
}

#[test]
fn a_subagent_run_recorded_before_the_cache_column_stays_out_of_the_rate() {
    let temp = tempfile::tempdir().unwrap();
    let store = StateStore::new(&test_paths(temp.path())).unwrap();
    store.adopt_sessions_for_persona("gqy").unwrap();
    let parent = store.session_id();
    let audit = store
        .create_session("gqy", "升级前的一次", "subagent", Some(&parent))
        .unwrap();
    // Exactly what the v19 migration leaves behind: usage recorded, cache
    // unknown (NULL). Counting its prompt with no hits to match turned a
    // measured 24% into 1% on the real database.
    store
        .conv_db()
        .record_legacy_subagent_usage_for_test(&audit.session_id, 1_111_360, 1_222_121)
        .unwrap();
    let totals = store.session_cumulative_token_totals().unwrap();
    assert_eq!(totals.total, 1_222_121);
    assert_eq!(
        totals.prompt, 0,
        "unknown cache must not claim a denominator"
    );
    assert_eq!(totals.cache_read, 0);
}

#[test]
fn an_estimated_subagent_run_never_reaches_the_cache_denominator() {
    let temp = tempfile::tempdir().unwrap();
    let store = StateStore::new(&test_paths(temp.path())).unwrap();
    store.adopt_sessions_for_persona("gqy").unwrap();
    let parent = store.session_id();
    let audit = store
        .create_session("gqy", "估算的一次", "subagent", Some(&parent))
        .unwrap();
    // The provider reported nothing, so only the char estimate is known:
    // it inflates the total but must not pretend to be measured prompt.
    store
        .record_subagent_usage(&audit.session_id, None, None, None, 0, 0, 9_000, 0)
        .unwrap();
    let totals = store.session_cumulative_token_totals().unwrap();
    assert_eq!(totals.total, 9_000);
    assert_eq!(totals.prompt, 0);
    assert_eq!(totals.cache_read, 0);
}

#[test]
fn subagent_audit_sessions_are_hidden_and_expire() {
    let temp = tempfile::tempdir().unwrap();
    let store = StateStore::new(&test_paths(temp.path())).unwrap();
    store.adopt_sessions_for_persona("gqy").unwrap();
    let parent = store.session_id();
    let audit = store
        .create_session("gqy", "探索代码库", "subagent", Some(&parent))
        .unwrap();
    let pinned = store.pinned(&audit.session_id);
    pinned
        .start_turn("sat_1", "task prompt", std::process::id())
        .unwrap();
    pinned
        .complete_turn("sat_1", "{\"ok\":true}", None)
        .unwrap();
    store
        .record_subagent_usage(
            &audit.session_id,
            Some("opencode"),
            Some("big-pickle"),
            Some(168000),
            100,
            50,
            150,
            40,
        )
        .unwrap();

    // Hidden from the user-facing session list.
    assert!(store
        .list_sessions("gqy")
        .unwrap()
        .iter()
        .all(|overview| overview.record.session_id != audit.session_id));
    let record = store.session_record(&audit.session_id).unwrap().unwrap();
    assert_eq!(record.kind, "subagent");
    assert_eq!(record.parent_session_id.as_deref(), Some(&*parent));

    // Fresh audit survives cleanup; a backdated one is removed with its
    // turns (FK cascade).
    assert_eq!(store.delete_subagent_sessions_older_than(7).unwrap(), 0);
    store
        .conv_db()
        .record_subagent_usage(&audit.session_id, None, None, None, 0, 0, 0, 0)
        .unwrap();
    // Backdate updated_at directly.
    store.conv_db().touch_session(&audit.session_id).unwrap();
    let backdated = (chrono::Utc::now() - chrono::Duration::days(10)).to_rfc3339();
    // No public API to backdate; use a raw update via the test-only conv_db handle.
    {
        use rusqlite::params;
        let db_path = temp.path().join("state").join("conversation.db");
        let conn = rusqlite::Connection::open(db_path).unwrap();
        conn.execute(
            "UPDATE sessions SET updated_at = ?1 WHERE session_id = ?2",
            params![backdated, audit.session_id],
        )
        .unwrap();
    }
    assert_eq!(store.delete_subagent_sessions_older_than(7).unwrap(), 1);
    assert!(store.session_record(&audit.session_id).unwrap().is_none());
}

#[test]
fn finished_turns_keep_a_replayable_transcript() {
    let (_temp, store) = test_store();
    store.init_files().unwrap();
    store.start_turn("t1", "改一下 README", 999_999).unwrap();
    let db = store.conv_db();
    for (kind, call_id, name, payload, ok) in [
        ("assistant_content", None, None, Some("这就去改。"), None),
        (
            "tool_call",
            Some("c1"),
            Some("edit_string"),
            Some("{\"path\":\"README.md\"}"),
            None,
        ),
        (
            "tool_result",
            Some("c1"),
            None,
            Some("1 处替换"),
            Some(true),
        ),
        ("tool_progress", Some("c1"), None, Some("忽略我"), None),
        ("assistant_content", None, None, Some("改好了。"), None),
    ] {
        db.append_turn_journal_event("t1", 0, 0, kind, call_id, name, payload, None, ok)
            .unwrap();
    }
    store.complete_turn("t1", "改好了。", None).unwrap();

    let replays = store.session_replay(5).unwrap();
    assert_eq!(replays.len(), 1);
    let entries = &replays[0].entries;
    assert_eq!(replays[0].display_content, "改一下 README");
    // Prose and tool blocks keep their original interleaving, and the
    // live-only progress ticks are gone.
    assert_eq!(
        entries,
        &vec![
            ReplayEntry::Text {
                text: "这就去改。".to_string()
            },
            ReplayEntry::ToolCall {
                name: "edit_string".to_string(),
                arguments: "{\"path\":\"README.md\"}".to_string(),
            },
            ReplayEntry::ToolResult {
                name: "edit_string".to_string(),
                ok: true,
                output: "1 处替换".to_string(),
            },
            ReplayEntry::Text {
                text: "改好了。".to_string()
            },
        ]
    );

    // A turn without a stored transcript still replays its reply.
    store.start_turn("t2", "再问一句", 999_999).unwrap();
    store.complete_turn("t2", "好的。", None).unwrap();
    let replays = store.session_replay(5).unwrap();
    assert_eq!(replays.len(), 2);
    assert!(replays[1].entries.is_empty());
    assert_eq!(replays[1].assistant_content, "好的。");
    // Oldest first, so the caller can print them top to bottom.
    assert_eq!(replays[0].display_content, "改一下 README");
    assert!(replays.iter().all(|replay| !replay.is_job_wake));

    // A background-job wake turn is daemon-synthesized: the replay must be
    // able to tell it apart so it is not drawn as something the user typed.
    store
        .start_turn_with_display(
            "t3",
            "<background-job-report>子代理「后台测试A」已执行完毕</background-job-report>",
            "[后台任务完成] 子代理完成 82bea3 · 后台测试A",
            999_999,
            None,
        )
        .unwrap();
    store.complete_turn("t3", "跑完了。", None).unwrap();
    let replays = store.session_replay(5).unwrap();
    assert_eq!(replays.len(), 3);
    assert!(replays[2].is_job_wake);
    assert_eq!(
        replays[2].display_content,
        "[后台任务完成] 子代理完成 82bea3 · 后台测试A"
    );
}

#[test]
fn one_shot_sessions_stay_invisible_and_stale_ones_are_swept() {
    let (temp, store) = test_store();
    store.init_files().unwrap();
    let user = store
        .create_session("gqy", "real", USER_SESSION_KIND, None)
        .unwrap();
    let ask = store
        .create_session("gqy", "一次性对话", ASK_SESSION_KIND, None)
        .unwrap();

    // Never listed, never findable by name — only the client holding the
    // freshly minted id can address it.
    let listed = store.list_sessions("gqy").unwrap();
    assert!(listed
        .iter()
        .any(|overview| overview.record.session_id == user.session_id));
    assert!(listed
        .iter()
        .all(|overview| overview.record.session_id != ask.session_id));
    assert!(store
        .find_local_session_by_name("gqy", "一次性对话")
        .unwrap()
        .is_none());

    // Fresh one-shot survives the sweep; an hour-old orphan does not.
    assert_eq!(store.delete_ask_sessions_older_than(1).unwrap(), 0);
    {
        use rusqlite::params;
        let backdated = (chrono::Utc::now() - chrono::Duration::hours(4)).to_rfc3339();
        let db_path = temp.path().join("state").join("conversation.db");
        let conn = rusqlite::Connection::open(db_path).unwrap();
        conn.execute("UPDATE sessions SET updated_at = ?1", params![backdated])
            .unwrap();
    }
    assert_eq!(store.delete_ask_sessions_older_than(1).unwrap(), 1);
    assert!(store.session_record(&ask.session_id).unwrap().is_none());
    // The equally backdated user session is untouched.
    assert!(store.session_record(&user.session_id).unwrap().is_some());
}

#[test]
fn repl_session_pointer_is_separate_and_drops_when_stale() {
    let (_temp, store) = test_store();
    store.init_files().unwrap();
    let terminal = store.session_id().to_string();
    let repl = store
        .create_session("gqy", "repl lane", USER_SESSION_KIND, None)
        .unwrap();

    assert!(store.repl_session("gqy").unwrap().is_none());
    store.set_repl_session("gqy", &repl.session_id).unwrap();
    assert_eq!(
        store.repl_session("gqy").unwrap().as_deref(),
        Some(repl.session_id.as_str())
    );
    // Moving the REPL lane must not drag the terminal lane along.
    assert_eq!(&*store.session_id(), terminal.as_str());

    // Deleted: the pointer goes stale rather than returning a session
    // the REPL must not land on.
    store.delete_session(&repl.session_id).unwrap();
    assert!(store.repl_session("gqy").unwrap().is_none());
}

#[test]
fn image_assets_persist_with_metadata_and_are_removed_with_history() {
    let (temp, store) = test_store();
    store.init_files().unwrap();
    store.start_turn("turn_image", "show it", 999999).unwrap();
    let path = temp.path().join("sample.png");
    image::RgbaImage::from_pixel(3, 2, image::Rgba([30, 120, 210, 255]))
        .save(&path)
        .unwrap();

    let saved = store
        .save_image_asset("turn_image", Some("tool_1"), &path, "sample image")
        .unwrap();
    assert_eq!(saved.mime, "image/png");
    assert_eq!((saved.width, saved.height), (3, 2));
    assert_eq!(store.load_image_assets().unwrap(), vec![saved.clone()]);
    let loaded = store.load_image_asset(&saved.asset_id).unwrap().unwrap();
    assert_eq!(loaded.asset, saved);
    assert!(!loaded.bytes.is_empty());

    store.reset_conversation().unwrap();
    assert!(store.load_image_assets().unwrap().is_empty());
    assert!(store
        .load_image_asset(&loaded.asset.asset_id)
        .unwrap()
        .is_none());
}

#[test]
fn artifact_assets_update_in_place_and_are_removed_with_history() {
    let (temp, store) = test_store();
    store.init_files().unwrap();
    store
        .start_turn("turn_artifact", "build it", 999999)
        .unwrap();
    let path = temp.path().join("report.md");
    std::fs::write(&path, "# First\n").unwrap();
    let managed_dir = temp
        .path()
        .join("data/artifacts")
        .join(store.session_id().as_ref());
    std::fs::create_dir_all(&managed_dir).unwrap();
    std::fs::write(managed_dir.join("managed.md"), "# Managed\n").unwrap();

    let first = store
        .save_artifact_asset("turn_artifact", Some("tool_1"), &path, "Report")
        .unwrap();
    assert_eq!(first.kind, "markdown");
    assert_eq!(first.file_name, "Report");

    std::fs::write(&path, "# Updated\n").unwrap();
    let updated = store
        .save_artifact_asset("turn_artifact", Some("tool_2"), &path, "Updated report")
        .unwrap();
    assert_eq!(updated.asset_id, first.asset_id);
    assert_eq!(store.load_artifact_assets().unwrap(), vec![updated.clone()]);
    let loaded = store
        .load_artifact_asset(&updated.asset_id)
        .unwrap()
        .unwrap();
    assert_eq!(loaded.bytes, b"# Updated\n");

    store.reset_conversation().unwrap();
    assert!(!managed_dir.exists());
    assert!(store.load_artifact_assets().unwrap().is_empty());
    assert!(store
        .load_artifact_asset(&updated.asset_id)
        .unwrap()
        .is_none());
}

#[test]
fn managed_artifact_keeps_its_identity_across_turns() {
    let (temp, store) = test_store();
    store.init_files().unwrap();
    let managed_dir = temp
        .path()
        .join("data/artifacts")
        .join(store.session_id().as_ref());
    std::fs::create_dir_all(&managed_dir).unwrap();
    let path = managed_dir.join("report.md");

    store.start_turn("turn_one", "first", 999999).unwrap();
    std::fs::write(&path, "# First\n").unwrap();
    let first = store
        .save_artifact_asset("turn_one", Some("tool_one"), &path, "Report")
        .unwrap();
    store.complete_turn("turn_one", "done", None).unwrap();

    store.start_turn("turn_two", "update", 999999).unwrap();
    std::fs::write(&path, "# Updated\n").unwrap();
    let updated = store
        .save_artifact_asset("turn_two", Some("tool_two"), &path, "Report")
        .unwrap();

    assert_eq!(updated.asset_id, first.asset_id);
    assert_eq!(updated.turn_id, "turn_two");
    assert_eq!(store.load_artifact_assets().unwrap(), vec![updated.clone()]);
    assert_eq!(
        store
            .load_artifact_asset(&updated.asset_id)
            .unwrap()
            .unwrap()
            .bytes,
        b"# Updated\n"
    );
}

#[test]
fn clearing_pinned_session_content_is_isolated_and_preserves_usage_and_binding() {
    let (_temp, store) = test_store();
    let current_session = store.session_id();
    store
        .start_turn("local_turn", "local prompt", std::process::id())
        .unwrap();
    store
        .complete_turn("local_turn", "local answer", None)
        .unwrap();

    let target_record = store
        .create_session("gqy", "qq:10000:private:42", "user", None)
        .unwrap();
    let target = store.pinned(&target_record.session_id);
    target
        .start_turn("qq_turn", "QQ prompt", std::process::id())
        .unwrap();
    target.complete_turn("qq_turn", "QQ answer", None).unwrap();
    target
        .enqueue_prompt("qq_queue", "queued", "queued", &[])
        .unwrap();
    let binding = platform_binding_key("42", None, "gqy");
    store
        .bind_platform_session(&binding, &target_record.session_id)
        .unwrap();

    store
        .add_usage(
            &Usage {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
                ..Usage::default()
            },
            UsageMeta {
                source: "agent",
                provider: Some("prov"),
                model: Some("model"),
            },
        )
        .unwrap();
    let usage_before = store.usage_snapshot().unwrap();

    target.clear_session_content().unwrap();

    assert!(target.load_turns().unwrap().is_empty());
    assert!(target.load_queued_prompts().unwrap().is_empty());
    assert_eq!(store.load_turns().unwrap().len(), 1);
    assert_eq!(store.session_id(), current_session);
    assert!(store
        .session_record(&target_record.session_id)
        .unwrap()
        .is_some());
    assert_eq!(
        store.find_platform_session_binding(&binding).unwrap(),
        Some(target_record.session_id)
    );
    let usage_after = store.usage_snapshot().unwrap();
    assert_eq!(usage_after.total_tokens, usage_before.total_tokens);
    assert_eq!(
        usage_after.conversation_tokens,
        usage_before.conversation_tokens
    );
}

/// Returns (non-summary fold ids, all visible ids) mirroring what the
/// compactor passes for a full fold of the current history.
pub(crate) fn visible_snapshot(store: &StateStore) -> (Vec<String>, Vec<String>) {
    let turns = store.load_visible_turns().unwrap();
    let fold_ids = turns
        .iter()
        .filter(|turn| !turn.is_summary)
        .map(|turn| turn.turn_id.clone())
        .collect();
    let turn_ids = turns.into_iter().map(|turn| turn.turn_id).collect();
    (fold_ids, turn_ids)
}

#[test]
fn queued_prompts_persist_and_attach_to_a_turn_in_order() {
    let (_temp, store) = test_store();
    let first = store
        .enqueue_prompt(
            "q1",
            "first expanded",
            "first",
            &[QueuedPromptAttachment::Path {
                path: "/tmp/image.png".to_string(),
            }],
        )
        .unwrap();
    let second = store
        .enqueue_prompt("q2", "second expanded", "second", &[])
        .unwrap();

    assert!(first.seq < second.seq);
    assert_eq!(
        store.load_queued_prompts().unwrap(),
        vec![first.clone(), second]
    );

    store.start_turn("t1", "initial", 999999).unwrap();
    store
        .consume_queued_prompts(
            "t1",
            &[
                ("q1".to_string(), "first context".to_string()),
                ("q2".to_string(), "second context".to_string()),
            ],
            Some("before followup"),
            Some("reasoning before followup"),
        )
        .unwrap();
    store.complete_turn("t1", "final answer", None).unwrap();

    assert!(store.load_queued_prompts().unwrap().is_empty());
    let turns = store.load_turns().unwrap();
    assert_eq!(turns[0].followups.len(), 2);
    assert_eq!(turns[0].followups[0].content, "first context");
    assert_eq!(turns[0].followups[0].attachments, first.attachments);
    assert_eq!(
        turns[0].followups[0]
            .preceding_assistant_reasoning
            .as_deref(),
        Some("reasoning before followup")
    );
    assert!(turns[0].followups[1].preceding_assistant_content.is_none());

    let history = store.load_conversation().unwrap();
    assert_eq!(
        history
            .iter()
            .map(|entry| (entry.role.as_str(), entry.content.as_str()))
            .collect::<Vec<_>>(),
        vec![
            ("user", "initial"),
            ("assistant", "before followup"),
            ("user", "first context"),
            ("user", "second context"),
            ("assistant", "final answer"),
        ]
    );

    store
        .enqueue_prompt("q3", "still queued", "still queued", &[])
        .unwrap();
    store.reset_conversation().unwrap();
    assert!(store.load_queued_prompts().unwrap().is_empty());
}

#[test]
fn running_turn_exposes_its_queue_as_a_cross_process_target() {
    let (temp, owner_store) = test_store();
    owner_store
        .start_turn("running", "still working", std::process::id())
        .unwrap();
    let web_store = StateStore::new(&test_paths(temp.path())).unwrap();

    let target = web_store.running_turn_queue_target().unwrap().unwrap();
    assert_eq!(target.turn_id, "running");
    assert!(target.queue_session_id.is_some());
    assert_eq!(target.owner_pid, Some(std::process::id()));

    let queued = web_store
        .enqueue_prompt_for_target(&target, "followup", "next", "next", &[])
        .unwrap();
    assert_eq!(owner_store.load_queued_prompts().unwrap(), vec![queued]);
}

#[test]
fn independent_process_stores_can_append_and_read_running_turns() {
    let (temp, first_store) = test_store();
    let second_store = StateStore::new(&test_paths(temp.path())).unwrap();

    first_store
        .start_turn("first", "first prompt", std::process::id())
        .unwrap();
    second_store
        .start_turn("second", "second prompt", std::process::id())
        .unwrap();

    let turns = first_store.load_visible_turns().unwrap();
    assert_eq!(turns.len(), 2);
    assert!(turns.iter().all(|turn| turn.status == TurnStatus::Running));
    assert!(turns
        .iter()
        .all(|turn| turn.assistant_content == pending_placeholder()));
}
