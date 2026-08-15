//! tests — 自 src/state/mod.rs 外移。
#![cfg(test)]
#![allow(clippy::op_ref)]

pub(crate) use super::*;

#[cfg(test)]
#[test]
fn turn_lifecycle() {
    let temp = tempfile::tempdir().unwrap();
    let store = StateStore::new(&GQYPaths {
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
    })
    .unwrap();

    store.init_files().unwrap();
    assert!(!temp.path().join("state/gqy.log").exists());

    store.start_turn("turn_1", "hello", 999999).unwrap();
    let turns = store.load_turns().unwrap();
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].status, TurnStatus::Running);
    assert_eq!(turns[0].assistant_content, pending_placeholder());

    store.complete_turn("turn_1", "hi there", None).unwrap();
    let turns = store.load_turns().unwrap();
    assert_eq!(turns[0].status, TurnStatus::Completed);
    assert_eq!(turns[0].assistant_content, "hi there");
}

#[test]
fn question_exchange_persists_with_user_role_history() {
    let temp = tempfile::tempdir().unwrap();
    let store = StateStore::new(&GQYPaths {
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
    })
    .unwrap();
    store.start_turn("turn_1", "配置它", 999999).unwrap();
    let request = crate::question::QuestionRequest {
        questions: vec![crate::question::QuestionPrompt {
            header: "范围".to_string(),
            question: "修改哪些部分？".to_string(),
            options: vec![crate::question::QuestionOption {
                label: "全部".to_string(),
                description: String::new(),
            }],
            multiple: false,
            custom: true,
        }],
    };
    let exchange =
        crate::question::QuestionExchange::new(request, vec![vec!["全部".to_string()]]).unwrap();
    store.append_question_exchange("turn_1", &exchange).unwrap();
    store.complete_turn("turn_1", "已经配置。", None).unwrap();

    let turns = store.load_turns().unwrap();
    assert_eq!(turns[0].question_exchanges, vec![exchange]);
    let history = store.load_conversation().unwrap();
    assert_eq!(history[1].role, "assistant_clarification");
    assert_eq!(history[2].role, "user_clarification");
    assert!(history[2].content.contains("全部"));
}

#[test]
fn interrupt_turn() {
    let temp = tempfile::tempdir().unwrap();
    let store = StateStore::new(&GQYPaths {
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
    })
    .unwrap();

    store.start_turn("turn_1", "do something", 999999).unwrap();
    store.interrupt_turn("turn_1").unwrap();
    let turns = store.load_turns().unwrap();
    assert_eq!(turns[0].status, TurnStatus::Interrupted);
    assert_eq!(turns[0].assistant_content, interrupted_text());
}

/// 并发回合完成序追加:与已完成回合重叠的回合在完成/中断时移到
/// 会话末尾,已完成历史跨请求 append-only,不再出现插入型缓存
/// 断点;无重叠回合与 redo 修订保持原位。
#[test]
fn overlapping_turns_reorder_to_completion_order() {
    let (_temp, store) = test_store();
    // A 先开跑,B 后开但先答完(群聊并发形态)——回放顺序按完成序。
    store.start_turn("turn_a", "先来的", 999999).unwrap();
    store.start_turn("turn_b", "后来的", 999999).unwrap();
    store.complete_turn("turn_b", "B 先答完", None).unwrap();
    store.complete_turn("turn_a", "A 后答完", None).unwrap();
    let turns = store.load_turns().unwrap();
    let order = turns
        .iter()
        .map(|turn| turn.turn_id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(order, ["turn_b", "turn_a"]);

    // 无重叠的后续回合不发生无谓跳位。
    store.start_turn("turn_c", "单独回合", 999999).unwrap();
    store.complete_turn("turn_c", "顺序完成", None).unwrap();
    let turns = store.load_turns().unwrap();
    assert_eq!(turns[2].turn_id, "turn_c");
    assert_eq!(turns[2].seq, turns[1].seq + 1);

    // 中断同样是"首次变为可回放",一样追加到末尾。
    store.start_turn("turn_d", "被打断的", 999999).unwrap();
    store.start_turn("turn_e", "插队的", 999999).unwrap();
    store.complete_turn("turn_e", "插队先完", None).unwrap();
    store.interrupt_turn("turn_d").unwrap();
    let turns = store.load_turns().unwrap();
    let order = turns
        .iter()
        .map(|turn| turn.turn_id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(order, ["turn_b", "turn_a", "turn_c", "turn_e", "turn_d"]);

    // redo 修订原位改写:turn_d 重跑完成后位置不动。
    let candidate = store.redo_candidate().unwrap().unwrap();
    assert_eq!(candidate.turn_id, "turn_d");
    let redo = store
        .begin_redo(
            "turn_d",
            "turn_d",
            RedoInputKind::Initial,
            candidate.revision,
            "重打的输入",
            "重打的输入",
            std::process::id(),
        )
        .unwrap();
    store
        .complete_turn_revision_with_usage_and_model(
            "turn_d",
            redo.revision,
            "重答",
            None,
            None,
            None,
            TurnTokens::default(),
            false,
        )
        .unwrap();
    let turns = store.load_turns().unwrap();
    let order = turns
        .iter()
        .map(|turn| turn.turn_id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(order, ["turn_b", "turn_a", "turn_c", "turn_e", "turn_d"]);
}

#[test]
fn interrupted_turn_materializes_persisted_journal_output() {
    let temp = tempfile::tempdir().unwrap();
    let store = StateStore::new(&GQYPaths {
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
    })
    .unwrap();
    store
        .start_turn("turn_journal", "long task", 999999)
        .unwrap();
    store
        .append_turn_journal_event(
            "turn_journal",
            0,
            0,
            "assistant_content",
            None,
            None,
            Some("first persisted part"),
            None,
            None,
        )
        .unwrap();
    store
        .append_turn_journal_event(
            "turn_journal",
            0,
            0,
            "assistant_reasoning",
            None,
            None,
            Some("private reasoning"),
            None,
            None,
        )
        .unwrap();
    store.interrupt_turn("turn_journal").unwrap();

    let turn = store.load_turns().unwrap().remove(0);
    assert_eq!(turn.status, TurnStatus::Interrupted);
    assert!(turn.assistant_content.contains("first persisted part"));
    assert!(turn.assistant_content.contains(interrupted_text()));
    assert_eq!(
        turn.assistant_reasoning.as_deref(),
        Some("private reasoning")
    );
    assert_eq!(turn.journal_events.len(), 2);
}

#[test]
fn superseded_journal_keeps_completed_tool_events_without_partial_text() {
    let (_temp, store) = test_store();
    store.start_turn("superseded", "long task", 999999).unwrap();
    store
        .append_turn_journal_event(
            "superseded",
            0,
            0,
            "assistant_content",
            None,
            None,
            Some("discarded partial answer"),
            None,
            None,
        )
        .unwrap();
    store
        .append_turn_journal_event(
            "superseded",
            0,
            0,
            "tool_call",
            Some("call-1"),
            Some("read_file"),
            Some("{\"path\":\"README.md\"}"),
            None,
            None,
        )
        .unwrap();
    store
        .append_turn_journal_event(
            "superseded",
            0,
            0,
            "tool_result",
            Some("call-1"),
            Some("read_file"),
            Some("completed tool output"),
            None,
            Some(true),
        )
        .unwrap();
    store
        .supersede_turn_journal_segment("superseded", 0, 0)
        .unwrap();

    let turn = store.load_turns().unwrap().remove(0);
    assert!(!turn
        .journal_events
        .iter()
        .any(|event| event.kind == "assistant_content"));
    assert!(turn
        .journal_events
        .iter()
        .any(|event| event.kind == "tool_call"));
    assert!(turn
        .journal_events
        .iter()
        .any(|event| event.kind == "tool_result"));
}

#[test]
fn recover_stale_running() {
    let temp = tempfile::tempdir().unwrap();
    let store = StateStore::new(&GQYPaths {
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
    })
    .unwrap();

    store.start_turn("turn_1", "task a", 999999).unwrap();
    store.start_turn("turn_2", "task b", 999999).unwrap();
    assert!(store.has_running_turns().unwrap());

    let recovered = store.recover_stale_turns().unwrap();
    assert_eq!(recovered, 2);

    let turns = store.load_turns().unwrap();
    assert_eq!(turns.len(), 2);
    assert!(turns.iter().all(|t| t.status == TurnStatus::Interrupted));
}

#[test]
fn recover_stale_skips_alive_owner() {
    let (_temp, store) = test_store();

    let current_pid = std::process::id();
    store
        .start_turn("turn_1", "终端1的prompt", current_pid)
        .unwrap();
    store.start_turn("turn_dead", "孤儿turn", 999999).unwrap();

    let recovered = store.recover_stale_turns().unwrap();
    assert_eq!(recovered, 1);

    let turns = store.load_turns().unwrap();
    let turn1 = turns.iter().find(|t| t.turn_id == "turn_1").unwrap();
    assert_eq!(turn1.status, TurnStatus::Running);
    assert_eq!(turn1.assistant_content, pending_placeholder());

    let dead = turns.iter().find(|t| t.turn_id == "turn_dead").unwrap();
    assert_eq!(dead.status, TurnStatus::Interrupted);
}

#[test]
fn interrupt_keeps_consumed_prompts_attached_to_the_interrupted_turn() {
    let (_temp, store) = test_store();
    store
        .enqueue_prompt("q1", "followup", "followup", &[])
        .unwrap();
    store.start_turn("turn_1", "initial", 999999).unwrap();
    store
        .consume_queued_prompts(
            "turn_1",
            &[("q1".to_string(), "followup".to_string())],
            None,
            None,
        )
        .unwrap();

    store.interrupt_turn("turn_1").unwrap();

    assert!(store.load_queued_prompts().unwrap().is_empty());
    let turns = store.load_turns().unwrap();
    assert_eq!(turns[0].status, TurnStatus::Interrupted);
    assert_eq!(turns[0].followups.len(), 1);
    assert_eq!(turns[0].followups[0].prompt_id, "q1");
}

#[test]
fn stale_turn_recovery_keeps_consumed_prompts_consumed() {
    let (_temp, store) = test_store();
    store
        .enqueue_prompt("q1", "followup", "followup", &[])
        .unwrap();
    store.start_turn("turn_1", "initial", 999999).unwrap();
    store
        .consume_queued_prompts(
            "turn_1",
            &[("q1".to_string(), "followup".to_string())],
            None,
            None,
        )
        .unwrap();

    assert_eq!(store.recover_stale_turns().unwrap(), 1);
    assert!(store.load_queued_prompts().unwrap().is_empty());
    let turns = store.load_turns().unwrap();
    assert_eq!(turns[0].status, TurnStatus::Interrupted);
    assert_eq!(turns[0].followups[0].prompt_id, "q1");
}

#[test]
fn stale_turn_recovery_consumes_accepted_queued_prompts() {
    let (_temp, store) = test_store();
    store.start_turn("turn_1", "initial", 999999).unwrap();
    store
        .append_turn_journal_event(
            "turn_1",
            0,
            0,
            "assistant_content",
            None,
            None,
            Some("partial answer"),
            None,
            None,
        )
        .unwrap();
    let target = store.running_turn_queue_target().unwrap().unwrap();
    store
        .enqueue_prompt_for_target(&target, "q1", "followup", "followup", &[])
        .unwrap();

    assert_eq!(store.recover_stale_turns().unwrap(), 1);
    assert!(store
        .load_queued_prompts_for_target(&target)
        .unwrap()
        .is_empty());
    let turn = store.load_turns().unwrap().remove(0);
    assert_eq!(turn.status, TurnStatus::Interrupted);
    assert_eq!(turn.followups.len(), 1);
    assert_eq!(turn.followups[0].prompt_id, "q1");
    assert_eq!(
        turn.followups[0].preceding_assistant_content.as_deref(),
        Some("partial answer")
    );
    assert!(turn
        .journal_events
        .iter()
        .any(|event| event.kind == "queued_prompts_consumed"));
}

#[test]
fn finished_turn_cleanup_preserves_a_late_queued_prompt() {
    let (_temp, store) = test_store();
    store
        .start_turn("turn_1", "initial", std::process::id())
        .unwrap();
    store.complete_turn("turn_1", "answer", None).unwrap();
    store
        .enqueue_prompt("late", "followup", "followup", &[])
        .unwrap();

    assert_eq!(store.discard_queued_prompts().unwrap(), 1);
    let turn = store.load_turns().unwrap().remove(0);
    assert_eq!(turn.followups.len(), 1);
    assert_eq!(turn.followups[0].prompt_id, "late");
    assert_eq!(
        turn.followups[0].preceding_assistant_content.as_deref(),
        Some("answer")
    );
}

#[test]
fn cancelled_turn_cleanup_deletes_queued_prompts_without_folding() {
    let (_temp, store) = test_store();
    store
        .start_turn("turn_1", "initial", std::process::id())
        .unwrap();
    store
        .enqueue_prompt("q1", "排队消息", "排队消息", &[])
        .unwrap();
    store.interrupt_turn("turn_1").unwrap();

    let dropped = store.delete_queued_prompts().unwrap();
    assert_eq!(dropped, vec!["q1".to_string()]);
    // Neither still queued nor folded into the turn as a follow-up.
    assert!(store.load_queued_prompts().unwrap().is_empty());
    let turn = store.load_turns().unwrap().remove(0);
    assert!(turn.followups.is_empty());
    // Idempotent on an already-empty queue.
    assert!(store.delete_queued_prompts().unwrap().is_empty());
}

#[test]
fn undo_removes_last_turn() {
    let temp = tempfile::tempdir().unwrap();
    let store = StateStore::new(&GQYPaths {
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
    })
    .unwrap();

    store.start_turn("turn_1", "hello", 999999).unwrap();
    store.complete_turn("turn_1", "hi", None).unwrap();
    store.start_turn("turn_2", "bye", 999999).unwrap();
    store.complete_turn("turn_2", "goodbye", None).unwrap();

    let (removed, prompt) = store.undo_last_turn().unwrap();
    assert_eq!(removed, 1);
    assert_eq!(prompt.as_deref(), Some("bye"));

    let turns = store.load_turns().unwrap();
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].turn_id, "turn_1");
}

pub(crate) fn test_paths(root: &Path) -> GQYPaths {
    GQYPaths {
        root_dir: root.to_path_buf(),
        config_dir: root.join("config"),
        config_file: root.join("config/config.jsonc"),
        skills_dir: root.join("config/skills"),
        data_dir: root.join("data"),
        cache_dir: root.join("cache"),
        state_dir: root.join("state"),
        pictures_dir: root.join("pictures"),
        fish_hook_file: root.join("fish/gqy.fish"),
        bash_hook_file: root.join("shell/bash-hook.sh"),
        zsh_hook_file: root.join("shell/zsh-hook.zsh"),
        scripts_dir: root.join("config/scripts"),
        system_scripts_dir: PathBuf::new(),
    }
}

pub(crate) fn test_store() -> (tempfile::TempDir, StateStore) {
    let temp = tempfile::tempdir().unwrap();
    let store = StateStore::new(&test_paths(temp.path())).unwrap();
    (temp, store)
}

#[test]
fn platform_access_grants_are_cached_persisted_and_audited() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let store = StateStore::new(&paths).unwrap();
    let peer = StateStore::new(&paths).unwrap();
    let key = PlatformAccessGrantKey {
        platform: "onebot".to_string(),
        account_scope: GLOBAL_PLATFORM_ACCOUNT_SCOPE.to_string(),
        permission: "private_whitelist".to_string(),
        subject_kind: "user".to_string(),
        subject_id: "2477342916".to_string(),
    };
    let actor = PlatformAccessActor {
        platform: "onebot".to_string(),
        account_id: "10000".to_string(),
        user_id: "42".to_string(),
        conversation_kind: "private".to_string(),
        conversation_id: "42".to_string(),
        message_id: "message-1".to_string(),
    };

    assert!(store.add_platform_access_grant(&key, &actor).unwrap());
    assert!(!store.add_platform_access_grant(&key, &actor).unwrap());
    assert!(store.has_platform_access_grant(
        "onebot",
        "10000",
        "private_whitelist",
        "user",
        "2477342916"
    ));
    assert!(peer.has_platform_access_grant(
        "onebot",
        "10000",
        "private_whitelist",
        "user",
        "2477342916"
    ));
    assert!(store.has_platform_access_grant(
        "onebot",
        "another-bot",
        "private_whitelist",
        "user",
        "2477342916"
    ));
    assert_eq!(store.platform_access_grants("onebot").unwrap().len(), 1);

    let reopened = StateStore::new(&paths).unwrap();
    assert!(reopened.has_platform_access_grant(
        "onebot",
        "20000",
        "private_whitelist",
        "user",
        "2477342916"
    ));
    assert!(reopened.remove_platform_access_grant(&key, &actor).unwrap());
    assert!(!reopened.remove_platform_access_grant(&key, &actor).unwrap());
    assert!(!reopened.has_platform_access_grant(
        "onebot",
        "10000",
        "private_whitelist",
        "user",
        "2477342916"
    ));
    assert!(!store.has_platform_access_grant(
        "onebot",
        "10000",
        "private_whitelist",
        "user",
        "2477342916"
    ));
    assert!(!peer.has_platform_access_grant(
        "onebot",
        "10000",
        "private_whitelist",
        "user",
        "2477342916"
    ));

    let denied_key = PlatformAccessGrantKey {
        subject_id: "99".to_string(),
        ..key.clone()
    };
    let denied = store
        .mutate_platform_access_grant_if_authorized(
            &denied_key,
            &actor,
            PlatformAccessMutation::Grant,
            &PlatformAccessAuthorization {
                statically_authorized: false,
                dynamic_key: PlatformAccessGrantKey {
                    platform: "onebot".to_string(),
                    account_scope: "10000".to_string(),
                    permission: "administrator".to_string(),
                    subject_kind: "user".to_string(),
                    subject_id: "42".to_string(),
                },
            },
        )
        .unwrap();
    assert_eq!(denied, PlatformAccessMutationResult::Unauthorized);
    assert!(!store.has_platform_access_grant("onebot", "10000", "private_whitelist", "user", "99"));

    let conn = rusqlite::Connection::open(paths.state_dir.join("conversation.db")).unwrap();
    let audit_count: i64 = conn
        .query_row("SELECT count(*) FROM platform_access_audit", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(audit_count, 2);
}

#[test]
fn user_attachment_moves_from_staged_to_turn_and_cascades() {
    let (_temp, store) = test_store();
    let attachment = UserAttachment {
        attachment_id: "att_test".to_string(),
        file_name: "notes.md".to_string(),
        mime: "text/markdown".to_string(),
        kind: "text".to_string(),
        size_bytes: 7,
        width: 0,
        height: 0,
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    store.save_user_attachment(&attachment, b"content").unwrap();
    assert_eq!(
        store
            .load_staged_user_attachments(std::slice::from_ref(&attachment.attachment_id))
            .unwrap()[0]
            .bytes,
        b"content"
    );

    store
        .reserve_user_attachments(std::slice::from_ref(&attachment.attachment_id), "run_test")
        .unwrap();
    store
        .start_turn_with_display(
            "turn_test",
            "visible\n\n<user-attachment>content</user-attachment>",
            "visible",
            std::process::id(),
            Some("run_test"),
        )
        .unwrap();
    let turns = store.load_turns().unwrap();
    assert_eq!(turns[0].display_content, "visible");
    assert_eq!(turns[0].attachments, vec![attachment.clone()]);
    assert!(store
        .load_staged_user_attachments(std::slice::from_ref(&attachment.attachment_id))
        .is_err());

    store.reset_conversation().unwrap();
    assert!(store
        .load_user_attachment_by_id(&attachment.attachment_id)
        .unwrap()
        .is_none());
}

#[test]
fn session_crud_switching_and_persona_adoption() {
    let temp = tempfile::tempdir().unwrap();
    let store = StateStore::new(&test_paths(temp.path())).unwrap();
    // Migrated/default rows start persona-less and are claimed on adoption.
    store.adopt_sessions_for_persona("gqy").unwrap();
    let default_id = store.session_id();
    let default = store.session_record(&default_id).unwrap().unwrap();
    assert_eq!(default.persona, "gqy");

    store.start_turn("t1", "hello", std::process::id()).unwrap();
    store.complete_turn("t1", "hi", None).unwrap();

    let created = store
        .create_session("gqy", "旅行计划", "user", None)
        .unwrap();
    store.switch_session(&created.session_id).unwrap();
    assert_eq!(&*store.session_id(), created.session_id.as_str());
    // The new session starts empty; history stays in the old session.
    assert!(store.load_visible_turns().unwrap().is_empty());

    // The pointer is persisted: an independent store resolves to it.
    let reopened = StateStore::new(&test_paths(temp.path())).unwrap();
    assert_eq!(&*reopened.session_id(), created.session_id.as_str());

    let listed = store.list_sessions("gqy").unwrap();
    assert_eq!(listed.len(), 2);
    let default_overview = listed
        .iter()
        .find(|overview| overview.record.session_id == *default_id)
        .unwrap();
    assert_eq!(default_overview.turn_count, 1);
    assert_eq!(default_overview.last_user_content.as_deref(), Some("hello"));

    assert!(store
        .find_session_by_name("gqy", "旅行计划")
        .unwrap()
        .is_some());
    store.rename_session(&created.session_id, "新名字").unwrap();
    assert!(store
        .find_session_by_name("gqy", "旅行计划")
        .unwrap()
        .is_none());

    // Deleting a session cascades its turns away.
    store.delete_session(&default_id).unwrap();
    assert!(store.session_record(&default_id).unwrap().is_none());
    assert_eq!(store.list_sessions("gqy").unwrap().len(), 1);

    // A dangling pointer self-heals back to a default session.
    store.delete_session(&created.session_id).unwrap();
    let healed = StateStore::new(&test_paths(temp.path())).unwrap();
    assert!(healed
        .session_record(&healed.session_id())
        .unwrap()
        .is_some());
}

#[test]
fn persona_reset_clears_active_local_and_onebot_contexts_only() {
    let (_temp, store) = test_store();
    store.adopt_sessions_for_persona("gqy").unwrap();
    let current = store.session_id().to_string();
    let local = store.create_session("gqy", "local", "user", None).unwrap();
    let second = store.create_session("gqy", "second", "user", None).unwrap();
    let other_persona = store
        .create_session("other", "other", "user", None)
        .unwrap();
    let qq = store.create_session("gqy", "qq", "user", None).unwrap();
    store
        .bind_platform_session(
            &PlatformSessionBindingKey {
                platform: "onebot".to_string(),
                account_id: "10000".to_string(),
                conversation_kind: "group".to_string(),
                conversation_id: "42".to_string(),
                participant_id: None,
                persona: "gqy".to_string(),
            },
            &qq.session_id,
        )
        .unwrap();
    let subagent = store
        .create_session("gqy", "child", "subagent", Some(&local.session_id))
        .unwrap();
    let second_child = store
        .create_session("gqy", "second-child", "subagent", Some(&second.session_id))
        .unwrap();

    let sessions = [
        current.clone(),
        local.session_id.clone(),
        second.session_id.clone(),
        other_persona.session_id.clone(),
        qq.session_id.clone(),
        subagent.session_id.clone(),
        second_child.session_id.clone(),
    ];
    for (index, session_id) in sessions.iter().enumerate() {
        let pinned = store.pinned(session_id);
        let turn_id = format!("reset-scope-{index}");
        pinned
            .start_turn(&turn_id, "before", std::process::id())
            .unwrap();
        pinned.complete_turn(&turn_id, "after", None).unwrap();
    }

    let targets = store.persona_reset_session_ids("gqy", "onebot").unwrap();
    assert!(targets.contains(&current));
    assert!(targets.contains(&local.session_id));
    assert!(targets.contains(&qq.session_id));
    assert!(targets.contains(&subagent.session_id));
    // 归档豁免已随功能移除:普通本地会话及其子代理一并进重置范围。
    assert!(targets.contains(&second.session_id));
    assert!(targets.contains(&second_child.session_id));
    assert!(!targets.contains(&other_persona.session_id));

    let cleared = store.reset_persona_contexts("gqy", "onebot").unwrap();
    assert_eq!(cleared, targets);
    for session_id in [
        &current,
        &local.session_id,
        &qq.session_id,
        &subagent.session_id,
        &second.session_id,
        &second_child.session_id,
    ] {
        assert!(store.pinned(session_id).load_turns().unwrap().is_empty());
    }
    for session_id in [&other_persona.session_id] {
        assert_eq!(store.pinned(session_id).load_turns().unwrap().len(), 1);
    }
    assert_eq!(
        store.platform_session_bindings("gqy", "onebot").unwrap()[0].session_id,
        qq.session_id
    );
}

pub(crate) fn platform_binding_key(
    conversation_id: &str,
    participant_id: Option<&str>,
    persona: &str,
) -> PlatformSessionBindingKey {
    PlatformSessionBindingKey {
        platform: "onebot".to_string(),
        account_id: "10000".to_string(),
        conversation_kind: "group".to_string(),
        conversation_id: conversation_id.to_string(),
        participant_id: participant_id.map(str::to_string),
        persona: persona.to_string(),
    }
}

pub(crate) fn plugin_scope(conversation_id: &str) -> PlatformPluginScopeKey {
    PlatformPluginScopeKey {
        plugin_id: "reply_processor".to_string(),
        platform: "onebot".to_string(),
        account_id: "10000".to_string(),
        conversation_kind: "group".to_string(),
        conversation_id: conversation_id.to_string(),
    }
}

#[test]
fn platform_bindings_survive_rename_and_isolate_personas() {
    let (_temp, store) = test_store();
    let gqy_session = store
        .create_session("gqy", "old display name", "user", None)
        .unwrap();
    let other_session = store
        .create_session("other", "another display name", "user", None)
        .unwrap();
    let gqy_key = platform_binding_key("20000", None, "gqy");
    let other_key = platform_binding_key("20000", None, "other");

    store
        .bind_platform_session(&gqy_key, &gqy_session.session_id)
        .unwrap();
    store
        .bind_platform_session(&other_key, &other_session.session_id)
        .unwrap();
    store
        .rename_session(&gqy_session.session_id, "new display name")
        .unwrap();

    assert_eq!(
        store.find_platform_session_binding(&gqy_key).unwrap(),
        Some(gqy_session.session_id.clone())
    );
    // `None` and an empty participant are the same database identity.
    let empty_participant_key = platform_binding_key("20000", Some(""), "gqy");
    assert_eq!(
        store
            .find_platform_session_binding(&empty_participant_key)
            .unwrap(),
        Some(gqy_session.session_id.clone())
    );
    assert_eq!(
        store.find_platform_session_binding(&other_key).unwrap(),
        Some(other_session.session_id)
    );

    store.delete_session(&gqy_session.session_id).unwrap();
    assert_eq!(store.find_platform_session_binding(&gqy_key).unwrap(), None);
}

#[test]
fn persona_scope_rename_migrates_sessions_bindings_and_affection() {
    let (_temp, store) = test_store();
    let session = store
        .create_session("old", "QQ group", "user", None)
        .unwrap();
    let old_binding = platform_binding_key("20000", None, "old");
    store
        .bind_platform_session(&old_binding, &session.session_id)
        .unwrap();
    store
        .set_persona_current_session("old", &session.session_id)
        .unwrap();
    let scope = PlatformPluginScopeKey {
        plugin_id: "real_context".to_string(),
        ..plugin_scope("20000")
    };
    store
        .plugin_put_json(
            &scope,
            "affection_profile:old",
            &serde_json::json!({"score": 42}),
        )
        .unwrap();

    store.rename_persona_scope("old", "new").unwrap();

    assert_eq!(
        store
            .session_record(&session.session_id)
            .unwrap()
            .unwrap()
            .persona,
        "new"
    );
    assert!(store
        .find_platform_session_binding(&old_binding)
        .unwrap()
        .is_none());
    let new_binding = platform_binding_key("20000", None, "new");
    assert_eq!(
        store.find_platform_session_binding(&new_binding).unwrap(),
        Some(session.session_id.clone())
    );
    assert_eq!(
        store.persona_current_session("new").unwrap(),
        Some(session.session_id)
    );
    assert!(store
        .plugin_get_json::<serde_json::Value>(&scope, "affection_profile:old")
        .unwrap()
        .is_none());
    assert_eq!(
        store
            .plugin_get_json::<serde_json::Value>(&scope, "affection_profile:new")
            .unwrap()
            .unwrap()["score"],
        42
    );
}
