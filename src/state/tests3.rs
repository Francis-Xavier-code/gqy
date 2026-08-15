//! tests3 — 自 src/state/mod.rs 外移。
#![cfg(test)]

use super::*;

#[test]
fn queued_prompts_survive_prompt_changes_but_not_a_new_store_session() {
    let (temp, store) = test_store();
    store.reset_if_prompt_changed("system prompt one").unwrap();
    store
        .enqueue_prompt("q1", "queued content", "queued", &[])
        .unwrap();
    store.reset_if_prompt_changed("system prompt two").unwrap();
    assert_eq!(store.load_queued_prompts().unwrap().len(), 1);
    drop(store);

    let paths = GQYPaths {
        root_dir: temp.path().to_path_buf(),
        config_dir: temp.path().join("config"),
        config_file: temp.path().join("config/config.jsonc"),
        skills_dir: temp.path().join("config/skills"),
        data_dir: temp.path().join("data"),
        cache_dir: temp.path().join("cache"),
        state_dir: temp.path().join("state"),
        pictures_dir: temp.path().join("pictures"),
        fish_hook_file: temp.path().join("fish/gqy.fish"),
        bash_hook_file: temp.path().join("shell/bash-hook.sh"),
        zsh_hook_file: temp.path().join("shell/zsh-hook.zsh"),
        scripts_dir: temp.path().join("config/scripts"),
        system_scripts_dir: PathBuf::new(),
    };
    let reopened = StateStore::new(&paths).unwrap();
    assert!(reopened.load_queued_prompts().unwrap().is_empty());
}

#[test]
fn prompt_fingerprint_changes_never_delete_history() {
    let (_temp, store) = test_store();
    store
        .reset_if_prompt_changed("persona plus owner identity")
        .unwrap();
    store
        .start_turn("turn", "hello", std::process::id())
        .unwrap();
    store.complete_turn("turn", "reply", None).unwrap();

    store
        .reset_if_prompt_changed_with_compatible(
            "persona only",
            Some("persona plus owner identity"),
        )
        .unwrap();
    assert_eq!(store.load_visible_turns().unwrap().len(), 1);

    // v7 Release 3: a prompt text change is a planned cache cold start and
    // must never destroy conversation data.
    store.reset_if_prompt_changed("different persona").unwrap();
    assert_eq!(store.load_visible_turns().unwrap().len(), 1);
}

#[test]
fn prompt_fingerprints_are_isolated_per_session() {
    let (_temp, store) = test_store();
    let first = store
        .create_session("first", "first", "user", None)
        .unwrap();
    let second = store
        .create_session("second", "second", "user", None)
        .unwrap();
    let first_store = store.pinned(&first.session_id);
    let second_store = store.pinned(&second.session_id);
    first_store.reset_if_prompt_changed("prompt A").unwrap();
    second_store.reset_if_prompt_changed("prompt B").unwrap();
    first_store
        .start_turn("first-turn", "hello", std::process::id())
        .unwrap();
    first_store
        .complete_turn("first-turn", "first reply", None)
        .unwrap();
    second_store
        .start_turn("second-turn", "hello", std::process::id())
        .unwrap();
    second_store
        .complete_turn("second-turn", "second reply", None)
        .unwrap();

    first_store.reset_if_prompt_changed("prompt A").unwrap();
    second_store.reset_if_prompt_changed("prompt B").unwrap();

    assert_eq!(first_store.load_visible_turns().unwrap().len(), 1);
    assert_eq!(second_store.load_visible_turns().unwrap().len(), 1);
}

#[test]
fn stale_queue_cleanup_preserves_another_live_process_session() {
    let (_temp, store) = test_store();
    let live_owner = std::process::id();
    store
        .conv_db
        .enqueue_prompt(
            &store.session_id(),
            None,
            "other-q",
            "content",
            "display",
            &[],
            &[],
            "other-session",
            live_owner,
        )
        .unwrap();
    let different_pid = live_owner.wrapping_add(1).max(1);

    assert_eq!(
        store
            .conv_db
            .discard_stale_queued_prompts("new-session", different_pid)
            .unwrap(),
        0
    );
    assert_eq!(
        store
            .conv_db
            .load_queued_prompts(&store.session_id(), "other-session")
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn normal_session_cleanup_discards_unsent_prompts() {
    let (_temp, store) = test_store();
    store
        .enqueue_prompt("q1", "content", "display", &[])
        .unwrap();

    assert_eq!(store.discard_queued_prompts().unwrap(), 1);
    assert!(store.load_queued_prompts().unwrap().is_empty());
}

#[test]
fn hidden_turns_excluded_from_visible() {
    let (_temp, store) = test_store();
    store.start_turn("t1", "first", 999999).unwrap();
    store.complete_turn("t1", "reply1", None).unwrap();
    store.start_turn("t2", "second", 999999).unwrap();
    store.complete_turn("t2", "reply2", None).unwrap();

    let visible = store.load_visible_turns().unwrap();
    assert_eq!(visible.len(), 2);

    let hidden_count = store.hide_turns_before_seq(visible[0].seq).unwrap();
    assert_eq!(hidden_count, 1);

    let visible_after = store.load_visible_turns().unwrap();
    assert_eq!(visible_after.len(), 1);
    assert_eq!(visible_after[0].turn_id, "t2");

    let all = store.load_turns().unwrap();
    assert_eq!(all.len(), 2);
    assert!(all[0].hidden);
    assert!(!all[1].hidden);
}

#[test]
fn summary_turn_insert_and_load() {
    let (_temp, store) = test_store();
    store.start_turn("t1", "hello", 999999).unwrap();
    store.complete_turn("t1", "hi", None).unwrap();

    store
        .insert_summary_turn(
            "## Task Goal\nDo stuff",
            TurnTokens {
                total: 12,
                ..Default::default()
            },
            true,
        )
        .unwrap();

    let summary = store.load_last_summary().unwrap();
    assert!(summary.is_some());
    let summary = summary.unwrap();
    assert!(summary.is_summary);
    assert!(!summary.hidden);
    assert_eq!(summary.assistant_content, "## Task Goal\nDo stuff");
    assert_eq!(summary.token_total, 12);
    assert!(summary.token_usage_estimated);

    let visible = store.load_visible_turns().unwrap();
    assert_eq!(visible.len(), 2);
    assert!(visible.iter().any(|t| t.is_summary));
    assert!(visible.iter().any(|t| !t.is_summary));
}

#[test]
fn session_loaded_tools_persist_until_reset() {
    let (_temp, store) = test_store();
    store
        .add_session_loaded_tools(&["web_search".to_string()], Some("t1"))
        .unwrap();
    store
        .add_session_loaded_targets(&["group:gaming".to_string()], Some("t1"))
        .unwrap();

    let loaded = store.load_session_loaded_tools().unwrap();
    assert!(loaded.contains("web_search"));

    store.reset_conversation().unwrap();
    assert!(store.load_session_loaded_tools().unwrap().is_empty());
}

#[test]
fn hide_before_seq_hides_old_summary_too() {
    let (_temp, store) = test_store();
    store.start_turn("t1", "old", 999999).unwrap();
    store.complete_turn("t1", "old reply", None).unwrap();
    store
        .insert_summary_turn(
            "summary of old",
            TurnTokens {
                total: 8,
                ..Default::default()
            },
            true,
        )
        .unwrap();
    store.start_turn("t2", "new", 999999).unwrap();
    store.complete_turn("t2", "new reply", None).unwrap();

    let visible = store.load_visible_turns().unwrap();
    assert_eq!(visible.len(), 3);

    let t2_seq = visible.last().unwrap().seq;
    let hidden = store.hide_turns_before_seq(t2_seq).unwrap();
    assert_eq!(hidden, 3);

    let visible_after = store.load_visible_turns().unwrap();
    assert!(visible_after.is_empty());
}

#[test]
fn evictable_turns_are_deleted_only_after_explicit_commit() {
    let (_temp, store) = test_store();
    for i in 0..10 {
        let id = format!("t{i}");
        let content = "x".repeat(1000);
        store.start_turn(&id, &content, 999999).unwrap();
        store.complete_turn(&id, &content, None).unwrap();
    }

    let evicted = store.oldest_evictable_visible_turns(3).unwrap();
    assert_eq!(evicted.len(), 3);
    assert_eq!(store.load_visible_turns().unwrap().len(), 10);

    let ids = evicted
        .iter()
        .map(|turn| turn.turn_id.clone())
        .collect::<Vec<_>>();
    store.delete_visible_turns(&ids).unwrap();

    let visible = store.load_visible_turns().unwrap();
    assert_eq!(visible.len(), 7);
}

#[test]
fn deleting_no_visible_turns_is_a_noop() {
    let (_temp, store) = test_store();
    store.start_turn("t1", "short", 999999).unwrap();
    store.complete_turn("t1", "reply", None).unwrap();

    assert_eq!(store.delete_visible_turns(&[]).unwrap(), 0);

    let visible = store.load_visible_turns().unwrap();
    assert_eq!(visible.len(), 1);
}

#[test]
fn deleting_visible_turns_rolls_back_when_any_id_changed() {
    let (_temp, store) = test_store();
    for id in ["t1", "t2"] {
        store.start_turn(id, id, 999999).unwrap();
        store.complete_turn(id, "reply", None).unwrap();
    }
    store
        .add_session_loaded_tools(&["from_t1".to_string()], Some("t1"))
        .unwrap();
    store
        .add_session_loaded_tools(&["from_t2".to_string()], Some("t2"))
        .unwrap();

    assert!(store
        .delete_visible_turns(&["t1".to_string(), "missing".to_string()])
        .is_err());
    assert_eq!(store.load_visible_turns().unwrap().len(), 2);
    assert_eq!(
        store.load_session_loaded_tools().unwrap(),
        BTreeSet::from(["from_t1".to_string(), "from_t2".to_string()])
    );
}

#[test]
fn checked_pop_rolls_back_when_loaded_tool_sources_change() {
    let (_temp, store) = test_store();
    for id in ["t1", "t2"] {
        store.start_turn(id, id, 999999).unwrap();
        store.complete_turn(id, "reply", None).unwrap();
    }
    store
        .add_session_loaded_tools(&["dynamic_tool".to_string()], Some("t1"))
        .unwrap();
    let expected = store.load_session_loaded_tools_with_sources().unwrap();
    store
        .add_session_loaded_tools(&["dynamic_tool".to_string()], Some("t2"))
        .unwrap();

    assert!(store
        .delete_visible_turns_checked(&["t1".to_string()], Some(&expected))
        .is_err());

    assert_eq!(store.load_visible_turns().unwrap().len(), 2);
    assert_eq!(
        store.load_session_loaded_tools_with_sources().unwrap(),
        vec![("dynamic_tool".to_string(), Some("t2".to_string()))]
    );
}

#[test]
fn deleting_visible_turns_unloads_only_items_sourced_from_deleted_turns() {
    let (_temp, store) = test_store();
    for id in ["t1", "t2"] {
        store.start_turn(id, id, 999999).unwrap();
        store.complete_turn(id, "reply", None).unwrap();
    }
    store
        .add_session_loaded_tools(&["popped_tool".to_string()], Some("t1"))
        .unwrap();
    store
        .add_session_loaded_tools(&["kept_tool".to_string()], Some("t2"))
        .unwrap();
    store
        .add_session_loaded_tools(&["global_tool".to_string()], None)
        .unwrap();
    store
        .add_session_loaded_targets(&["popped_target".to_string()], Some("t1"))
        .unwrap();
    store
        .add_session_loaded_targets(&["kept_target".to_string()], Some("t2"))
        .unwrap();

    assert_eq!(store.delete_visible_turns(&["t1".to_string()]).unwrap(), 1);

    assert_eq!(
        store.load_session_loaded_tools().unwrap(),
        BTreeSet::from(["global_tool".to_string(), "kept_tool".to_string()])
    );
    assert_eq!(
        store
            .conv_db
            .load_session_loaded_items(&store.session_id(), "target")
            .unwrap(),
        BTreeSet::from(["kept_target".to_string()])
    );
}

#[test]
fn interrupted_turn_is_evictable_but_summary_and_running_turn_are_not() {
    let (_temp, store) = test_store();
    store
        .insert_summary_turn(
            "summary",
            TurnTokens {
                total: 1,
                ..Default::default()
            },
            false,
        )
        .unwrap();
    store.start_turn("completed", "completed", 999999).unwrap();
    store.complete_turn("completed", "reply", None).unwrap();
    store
        .start_turn("interrupted", "interrupted", 999999)
        .unwrap();
    store.interrupt_turn("interrupted").unwrap();
    store
        .start_turn("running", "pending", std::process::id())
        .unwrap();

    let evicted = store.oldest_evictable_visible_turns(10).unwrap();
    assert_eq!(
        evicted
            .iter()
            .map(|turn| turn.turn_id.as_str())
            .collect::<Vec<_>>(),
        vec!["completed", "interrupted"]
    );
    assert_eq!(evicted[1].status, TurnStatus::Interrupted);
}

#[test]
fn compact_is_reversible_with_undo() {
    let (_temp, store) = test_store();
    for id in ["t1", "t2"] {
        store.start_turn(id, id, 999999).unwrap();
        store.complete_turn(id, "reply", None).unwrap();
    }
    let (fold_ids, turn_ids) = visible_snapshot(&store);

    store
        .replace_visible_with_summary(
            &fold_ids,
            &turn_ids,
            "summary",
            TurnTokens {
                total: 10,
                ..Default::default()
            },
            true,
            None,
        )
        .unwrap();

    let all = store.load_turns().unwrap();
    assert_eq!(all.len(), 3);
    assert!(all[0].hidden && all[1].hidden);
    assert_eq!(store.load_visible_turns().unwrap().len(), 1);
    assert_eq!(
        store
            .load_conversation()
            .unwrap()
            .into_iter()
            .filter(|entry| entry.role == "user")
            .map(|entry| entry.content)
            .collect::<Vec<_>>(),
        vec!["t1", "t2"]
    );

    let (removed, prompt) = store.undo_last_turn().unwrap();
    assert_eq!(removed, 1);
    assert!(prompt.is_none());
    let visible = store.load_visible_turns().unwrap();
    assert_eq!(
        visible
            .iter()
            .map(|turn| turn.turn_id.as_str())
            .collect::<Vec<_>>(),
        vec!["t1", "t2"]
    );
}

#[test]
fn nested_compact_undo_restores_one_layer_at_a_time() {
    let (_temp, store) = test_store();
    for id in ["t1", "t2"] {
        store.start_turn(id, id, 999999).unwrap();
        store.complete_turn(id, "reply", None).unwrap();
    }
    let (fold_ids, turn_ids) = visible_snapshot(&store);
    store
        .replace_visible_with_summary(
            &fold_ids,
            &turn_ids,
            "summary one",
            TurnTokens::default(),
            false,
            None,
        )
        .unwrap();
    store.start_turn("t3", "third", 999999).unwrap();
    store.complete_turn("t3", "reply", None).unwrap();
    let (fold_ids, turn_ids) = visible_snapshot(&store);
    store
        .replace_visible_with_summary(
            &fold_ids,
            &turn_ids,
            "summary two",
            TurnTokens::default(),
            false,
            None,
        )
        .unwrap();

    assert_eq!(
        store
            .load_last_summary()
            .unwrap()
            .unwrap()
            .assistant_content,
        "summary two"
    );
    assert_eq!(store.undo_last_turn().unwrap(), (1, None));
    let visible = store.load_visible_turns().unwrap();
    assert_eq!(visible.len(), 2);
    assert_eq!(visible[0].assistant_content, "summary one");
    assert_eq!(visible[1].turn_id, "t3");

    assert_eq!(store.undo_last_turn().unwrap().1.as_deref(), Some("third"));
    assert_eq!(store.undo_last_turn().unwrap(), (1, None));
    let visible = store.load_visible_turns().unwrap();
    assert_eq!(
        visible
            .iter()
            .map(|turn| turn.turn_id.as_str())
            .collect::<Vec<_>>(),
        vec!["t1", "t2"]
    );
}

#[test]
fn tail_retention_compact_folds_only_the_selected_turns() {
    let (_temp, store) = test_store();
    for id in ["t1", "t2", "t3", "t4"] {
        store.start_turn(id, id, 999999).unwrap();
        store.complete_turn(id, "reply", None).unwrap();
    }
    let (_, all_ids) = visible_snapshot(&store);
    store
        .replace_visible_with_summary(
            &["t1".to_string(), "t2".to_string()],
            &all_ids,
            "summary",
            TurnTokens::default(),
            false,
            None,
        )
        .unwrap();

    let visible = store.load_visible_turns().unwrap();
    let ids: Vec<&str> = visible.iter().map(|t| t.turn_id.as_str()).collect();
    assert_eq!(&ids[..2], &["t3", "t4"]);
    assert_eq!(visible.len(), 3);
    assert!(visible[2].is_summary);
    assert_eq!(
        store
            .load_last_summary()
            .unwrap()
            .unwrap()
            .assistant_content,
        "summary"
    );

    // Undo restores exactly the folded set and deletes the summary.
    assert_eq!(store.undo_last_turn().unwrap(), (1, None));
    let visible = store.load_visible_turns().unwrap();
    assert_eq!(
        visible
            .iter()
            .map(|t| t.turn_id.as_str())
            .collect::<Vec<_>>(),
        vec!["t1", "t2", "t3", "t4"]
    );
}

#[test]
fn second_tail_compact_supersedes_the_previous_summary() {
    let (_temp, store) = test_store();
    for id in ["t1", "t2", "t3"] {
        store.start_turn(id, id, 999999).unwrap();
        store.complete_turn(id, "reply", None).unwrap();
    }
    let (_, all_ids) = visible_snapshot(&store);
    store
        .replace_visible_with_summary(
            &["t1".to_string()],
            &all_ids,
            "summary one",
            TurnTokens::default(),
            false,
            None,
        )
        .unwrap();
    store.start_turn("t4", "fourth", 999999).unwrap();
    store.complete_turn("t4", "reply", None).unwrap();

    // Second compaction folds t2 (oldest visible non-summary turn); the
    // superseded summary must be hidden together with it even though its
    // seq is higher than the tail turns'.
    let (_, all_ids) = visible_snapshot(&store);
    store
        .replace_visible_with_summary(
            &["t2".to_string()],
            &all_ids,
            "summary two",
            TurnTokens::default(),
            false,
            None,
        )
        .unwrap();

    let visible = store.load_visible_turns().unwrap();
    let ids: Vec<&str> = visible.iter().map(|t| t.turn_id.as_str()).collect();
    assert_eq!(&ids[..2], &["t3", "t4"]);
    assert_eq!(visible.len(), 3);
    assert_eq!(
        store
            .load_last_summary()
            .unwrap()
            .unwrap()
            .assistant_content,
        "summary two"
    );
    assert_eq!(
        visible.iter().filter(|t| t.is_summary).count(),
        1,
        "the superseded summary must not stay visible"
    );

    // Undo restores t2 and summary one, drops summary two.
    assert_eq!(store.undo_last_turn().unwrap(), (1, None));
    assert_eq!(
        store
            .load_last_summary()
            .unwrap()
            .unwrap()
            .assistant_content,
        "summary one"
    );
    let visible = store.load_visible_turns().unwrap();
    assert!(visible.iter().any(|t| t.turn_id == "t2" && !t.hidden));
}

#[test]
fn prune_folds_old_tool_reports_behind_the_harvest_gate() {
    let (_temp, store) = test_store();
    let big_report = "x".repeat(4096);
    for id in ["t1", "t2", "t3", "t4"] {
        store.start_turn(id, id, 999999).unwrap();
        store
            .conv_db
            .append_tool_reports(id, &[big_report.clone()])
            .unwrap();
        store.complete_turn(id, "reply", None).unwrap();
    }

    // Harvest gate: potential savings (~8KB from t1+t2) below the
    // threshold → nothing is rewritten.
    let stats = store.prune_stale_tool_reports(2, 1_000_000).unwrap();
    assert_eq!(stats.turns, 0);
    let turns = store.load_visible_turns().unwrap();
    assert_eq!(turns[0].tool_reports[0], big_report);

    // Gate passes: the two oldest turns fold, newest two are protected.
    let stats = store.prune_stale_tool_reports(2, 1024).unwrap();
    assert_eq!(stats.turns, 2);
    assert!(stats.saved_chars > 6000);
    let turns = store.load_visible_turns().unwrap();
    assert!(turns[0].tool_reports[0].contains("已折叠"));
    assert!(turns[1].tool_reports[0].contains("已折叠"));
    assert_eq!(turns[2].tool_reports[0], big_report);
    assert_eq!(turns[3].tool_reports[0], big_report);

    // Monotonic: a second pass finds nothing new to rewrite (the
    // archived turns are never re-pruned, so the cache is not re-hit).
    let stats = store.prune_stale_tool_reports(2, 1024).unwrap();
    assert_eq!(stats.turns, 0);
}

#[test]
fn empty_summary_leaves_visible_turns_unchanged() {
    let (_temp, store) = test_store();
    store.start_turn("t1", "hello", 999999).unwrap();
    store.complete_turn("t1", "reply", None).unwrap();
    let (fold_ids, turn_ids) = visible_snapshot(&store);

    assert!(store
        .replace_visible_with_summary(
            &fold_ids,
            &turn_ids,
            "  ",
            TurnTokens::default(),
            false,
            None
        )
        .is_err());

    let visible = store.load_visible_turns().unwrap();
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].turn_id, "t1");
}

#[test]
fn compact_insert_failure_rolls_back_hidden_turns() {
    let (temp, store) = test_store();
    store.start_turn("t1", "hello", 999999).unwrap();
    store.complete_turn("t1", "reply", None).unwrap();
    let (fold_ids, turn_ids) = visible_snapshot(&store);
    let conn = rusqlite::Connection::open(temp.path().join("state/conversation.db")).unwrap();
    conn.execute_batch(
        "CREATE TRIGGER fail_summary_insert
             BEFORE INSERT ON turns WHEN NEW.is_summary = 1
             BEGIN SELECT RAISE(ABORT, 'injected summary failure'); END;",
    )
    .unwrap();

    assert!(store
        .replace_visible_with_summary(
            &fold_ids,
            &turn_ids,
            "summary",
            TurnTokens::default(),
            false,
            None
        )
        .is_err());
    let visible = store.load_visible_turns().unwrap();
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].turn_id, "t1");
    assert!(!visible[0].hidden);
}

#[test]
fn irreversible_legacy_summary_is_not_deleted_by_undo() {
    let (_temp, store) = test_store();
    store
        .insert_summary_turn("legacy summary", TurnTokens::default(), false)
        .unwrap();

    assert_eq!(store.undo_last_turn().unwrap(), (0, None));
    assert_eq!(
        store
            .load_last_summary()
            .unwrap()
            .unwrap()
            .assistant_content,
        "legacy summary"
    );
}

#[test]
fn irreversible_nested_legacy_summary_is_not_downgraded_by_undo() {
    let (_temp, store) = test_store();
    store
        .insert_summary_turn("legacy summary one", TurnTokens::default(), false)
        .unwrap();
    let first_seq = store.load_visible_turns().unwrap()[0].seq;
    store.hide_turns_before_seq(first_seq).unwrap();
    store
        .insert_summary_turn("legacy summary two", TurnTokens::default(), false)
        .unwrap();

    assert_eq!(store.undo_last_turn().unwrap(), (0, None));
    assert_eq!(
        store
            .load_last_summary()
            .unwrap()
            .unwrap()
            .assistant_content,
        "legacy summary two"
    );
}

#[test]
fn undo_does_not_remove_a_running_turn() {
    let (_temp, store) = test_store();
    store.start_turn("t1", "completed", 999999).unwrap();
    store.complete_turn("t1", "reply", None).unwrap();
    store
        .start_turn("running", "active", std::process::id())
        .unwrap();

    assert_eq!(store.undo_last_turn().unwrap(), (0, None));
    assert_eq!(store.load_visible_turns().unwrap().len(), 2);
}

#[test]
fn compact_rejects_a_changed_snapshot() {
    let (_temp, store) = test_store();
    store.start_turn("t1", "first", 999999).unwrap();
    store.complete_turn("t1", "reply", None).unwrap();
    let (fold_ids, turn_ids) = visible_snapshot(&store);
    store.undo_last_turn().unwrap();

    assert!(store
        .replace_visible_with_summary(
            &fold_ids,
            &turn_ids,
            "stale",
            TurnTokens::default(),
            false,
            None
        )
        .is_err());
    assert!(store.load_visible_turns().unwrap().is_empty());
}

#[test]
fn compact_rejects_a_new_turn_after_snapshot() {
    let (_temp, store) = test_store();
    store.start_turn("t1", "first", 999999).unwrap();
    store.complete_turn("t1", "reply", None).unwrap();
    let (fold_ids, turn_ids) = visible_snapshot(&store);
    store.start_turn("t2", "second", 999999).unwrap();
    store.complete_turn("t2", "reply", None).unwrap();

    assert!(store
        .replace_visible_with_summary(
            &fold_ids,
            &turn_ids,
            "stale",
            TurnTokens::default(),
            false,
            None
        )
        .is_err());
    assert_eq!(store.load_visible_turns().unwrap().len(), 2);
}

#[test]
fn initial_prompt_redo_reuses_the_turn_with_a_new_revision() {
    let (_temp, store) = test_store();
    store
        .start_turn_with_display("t1", "original", "original", 999999, None)
        .unwrap();
    store.complete_turn("t1", "old answer", None).unwrap();

    let candidate = store.redo_candidate().unwrap().unwrap();
    assert_eq!(candidate.input_kind, RedoInputKind::Initial);
    let redo = store
        .begin_redo(
            "t1",
            "t1",
            RedoInputKind::Initial,
            candidate.revision,
            "edited internal",
            "edited",
            std::process::id(),
        )
        .unwrap();
    assert_eq!(redo.revision, 1);
    assert!(redo.checkpoint.is_none());

    let turn = store.load_turns().unwrap().remove(0);
    assert_eq!(turn.revision, 1);
    assert_eq!(turn.status, TurnStatus::Running);
    assert_eq!(turn.user_content, "edited internal");
    assert_eq!(turn.display_content, "edited");
    assert!(store
        .begin_redo(
            "t1",
            "t1",
            RedoInputKind::Initial,
            candidate.revision,
            "stale",
            "stale",
            std::process::id(),
        )
        .is_err());

    store
        .complete_turn_revision_with_usage_and_model(
            "t1",
            1,
            "new answer",
            None,
            None,
            None,
            TurnTokens::default(),
            false,
        )
        .unwrap();
    assert_eq!(
        store.load_turns().unwrap()[0].assistant_content,
        "new answer"
    );
}

#[test]
fn followup_redo_restores_the_last_batch_checkpoint() {
    let (_temp, store) = test_store();
    store
        .start_turn("t1", "initial", std::process::id())
        .unwrap();
    store
        .enqueue_prompt("q1", "followup", "followup", &[])
        .unwrap();
    let checkpoint = TurnRedoCheckpointPayload {
        replay_messages: vec![crate::llm::ChatMessage::plain("assistant", "prefix answer")],
        prefix_tool_reports: vec!["prefix report".to_string()],
        tool_rounds: 1,
        question_rounds: 0,
        loaded_items: Vec::new(),
        prefix_question_count: 0,
        prefix_image_asset_ids: Vec::new(),
        prefix_artifact_asset_ids: Vec::new(),
    };
    store
        .consume_queued_prompts_with_checkpoint(
            "t1",
            &[("q1".to_string(), "followup".to_string())],
            Some("prefix answer"),
            None,
            None,
            None,
            checkpoint,
        )
        .unwrap();
    store.complete_turn("t1", "old final", None).unwrap();

    let candidate = store.redo_candidate().unwrap().unwrap();
    assert_eq!(candidate.input_kind, RedoInputKind::Followup);
    assert_eq!(candidate.input_id, "q1");
    let redo = store
        .begin_redo(
            "t1",
            "q1",
            RedoInputKind::Followup,
            candidate.revision,
            "edited followup",
            "edited followup",
            std::process::id(),
        )
        .unwrap();
    let redo_revision = redo.revision;
    let checkpoint = redo.checkpoint.unwrap();
    assert_eq!(checkpoint.replay_messages.len(), 1);
    assert_eq!(checkpoint.prefix_tool_reports, vec!["prefix report"]);
    let turn = store.load_turns().unwrap().remove(0);
    assert_eq!(turn.followups[0].content, "edited followup");
    assert_eq!(turn.tool_reports, vec!["prefix report"]);
    store
        .enqueue_prompt("q2", "new during redo", "new during redo", &[])
        .unwrap();
    store
        .consume_queued_prompts_with_checkpoint(
            "t1",
            &[("q2".to_string(), "new during redo".to_string())],
            None,
            None,
            None,
            None,
            TurnRedoCheckpointPayload {
                replay_messages: Vec::new(),
                prefix_tool_reports: Vec::new(),
                tool_rounds: 0,
                question_rounds: 0,
                loaded_items: Vec::new(),
                prefix_question_count: 0,
                prefix_image_asset_ids: Vec::new(),
                prefix_artifact_asset_ids: Vec::new(),
            },
        )
        .unwrap();
    store.interrupt_turn_revision("t1", redo_revision).unwrap();
    let restored = store.load_turns().unwrap().remove(0);
    assert_eq!(restored.revision, 0);
    assert_eq!(restored.status, TurnStatus::Completed);
    assert_eq!(restored.assistant_content, "old final");
    assert_eq!(restored.followups[0].content, "followup");
    assert_eq!(restored.followups.len(), 1);
    assert_eq!(store.redo_candidate().unwrap().unwrap().input_id, "q1");
}

#[test]
fn cancelled_initial_redo_restores_the_previous_turn() {
    let (_temp, store) = test_store();
    store
        .start_turn_with_display("t1", "internal", "visible", 999999, None)
        .unwrap();
    store
        .complete_turn("t1", "old answer", Some("old reasoning"))
        .unwrap();
    let candidate = store.redo_candidate().unwrap().unwrap();
    let redo = store
        .begin_redo(
            "t1",
            "t1",
            RedoInputKind::Initial,
            candidate.revision,
            "edited internal",
            "edited visible",
            std::process::id(),
        )
        .unwrap();

    store.interrupt_turn_revision("t1", redo.revision).unwrap();
    let restored = store.load_turns().unwrap().remove(0);
    assert_eq!(restored.revision, 0);
    assert_eq!(restored.status, TurnStatus::Completed);
    assert_eq!(restored.user_content, "internal");
    assert_eq!(restored.display_content, "visible");
    assert_eq!(restored.assistant_content, "old answer");
    assert_eq!(
        restored.assistant_reasoning.as_deref(),
        Some("old reasoning")
    );
}

#[test]
fn cancelled_redo_restores_artifact_versions() {
    let (temp, store) = test_store();
    let artifact_dir = temp.path().join("data/artifacts/default");
    std::fs::create_dir_all(&artifact_dir).unwrap();
    let path = artifact_dir.join("report.md");
    std::fs::write(&path, "old artifact").unwrap();
    store
        .start_turn("t1", "create report", std::process::id())
        .unwrap();
    let old = store
        .save_artifact_asset("t1", Some("tool-old"), &path, "Report")
        .unwrap();
    store.complete_turn("t1", "old answer", None).unwrap();

    let candidate = store.redo_candidate().unwrap().unwrap();
    let redo = store
        .begin_redo(
            "t1",
            "t1",
            RedoInputKind::Initial,
            candidate.revision,
            "redo report",
            "redo report",
            std::process::id(),
        )
        .unwrap();
    assert!(store.load_artifact_assets().unwrap().is_empty());
    std::fs::write(&path, "new artifact").unwrap();
    store
        .save_artifact_asset("t1", Some("tool-new"), &path, "Report")
        .unwrap();
    store.interrupt_turn_revision("t1", redo.revision).unwrap();

    let restored = store.load_artifact_asset(&old.asset_id).unwrap().unwrap();
    assert_eq!(restored.asset.tool_id.as_deref(), Some("tool-old"));
    assert_eq!(restored.bytes, b"old artifact");
    assert_eq!(std::fs::read_to_string(path).unwrap(), "old artifact");
}
