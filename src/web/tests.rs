//! tests — 自 src/web.rs 拆分。

#![cfg(test)]

use super::*;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::question::{QuestionOption, QuestionPrompt};
    use crate::state::PlatformSessionBindingKey;

    #[test]
    pub(crate) fn artifact_tools_are_scoped_to_local_webui_requests() {
        assert!(is_local_webui_request(PromptAudience::External, false));
        assert!(!is_local_webui_request(PromptAudience::Owner, false));
        assert!(!is_local_webui_request(PromptAudience::External, true));
    }

    pub(crate) fn test_paths(root: &FilePath) -> GQYPaths {
        GQYPaths {
            root_dir: root.to_path_buf(),
            config_dir: root.join("config"),
            config_file: root.join("config/config.jsonc"),
            skills_dir: root.join("config/skills"),
            data_dir: root.join("data"),
            cache_dir: root.join("cache"),
            state_dir: root.join("state"),
            pictures_dir: root.join("pictures"),
            fish_hook_file: root.join("fish"),
            bash_hook_file: root.join("bash"),
            zsh_hook_file: root.join("zsh"),
            scripts_dir: root.join("scripts"),
            system_scripts_dir: root.join("system-scripts"),
        }
    }

    #[test]
    pub(crate) fn managed_persona_assets_use_the_resource_directory_and_reject_escape() {
        let temp = tempfile::tempdir().unwrap();
        let mut paths = test_paths(temp.path());
        paths.skills_dir = paths.data_dir.join("skills");
        paths.scripts_dir = paths.data_dir.join("scripts");

        assert_eq!(
            managed_persona_asset_path(&paths, "persona-avatars/avatar.png"),
            Some(paths.data_dir.join("persona-avatars/avatar.png"))
        );
        assert!(managed_persona_asset_path(&paths, "/etc/passwd").is_none());
        assert!(managed_persona_asset_path(&paths, "persona-avatars/../secret").is_none());
        assert_eq!(
            managed_persona_asset_path(&paths, "persona-avatars/nested/file.png"),
            Some(paths.data_dir.join("persona-avatars/nested/file.png"))
        );
        assert_eq!(
            resolve_persona_asset_path(&paths, "./persona-avatars/avatar.png"),
            Some(paths.data_dir.join("persona-avatars/avatar.png"))
        );
        assert!(resolve_persona_asset_path(&paths, "persona-avatars/../../secret").is_none());
        assert_eq!(
            resolve_persona_asset_path(&paths, "avatars/custom.png"),
            Some(paths.config_dir.join("avatars/custom.png"))
        );
        assert_eq!(
            resolve_persona_asset_path(&paths, "scripts/images/custom.png"),
            Some(paths.data_dir.join("scripts/images/custom.png"))
        );
        assert_eq!(
            resolve_persona_asset_path(
                &paths,
                &paths
                    .config_dir
                    .join("persona-avatars/absolute.png")
                    .display()
                    .to_string(),
            ),
            Some(paths.data_dir.join("persona-avatars/absolute.png"))
        );
    }

    #[tokio::test]
    pub(crate) async fn persona_asset_store_is_atomic_and_rejects_corrupt_cache_entries() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("persona-avatars");
        std::fs::create_dir_all(&directory).unwrap();
        let body = b"persona asset";
        let hash = format!("{:x}", Sha256::digest(body));
        let destination = directory.join(format!("{hash}.png"));

        store_persona_asset(&directory, &destination, &hash, body)
            .await
            .unwrap();
        store_persona_asset(&directory, &destination, &hash, body)
            .await
            .unwrap();
        assert_eq!(std::fs::read(&destination).unwrap(), body);

        std::fs::write(&destination, b"corrupt").unwrap();
        store_persona_asset(&directory, &destination, &hash, body)
            .await
            .unwrap();
        assert_eq!(std::fs::read(&destination).unwrap(), body);
    }

    #[test]
    pub(crate) fn persona_asset_cleanup_normalizes_managed_reference_paths() {
        pub(crate) fn prompts(path: String) -> PromptDocuments {
            PromptDocuments {
                personas: vec![PromptDocument {
                    name: "Persona.md".to_string(),
                    content: String::new(),
                    avatar_path: Some(path),
                    board_image_path: None,
                    board_title: None,
                    board_subtitle: None,
                    starter_prompts: None,
                    original_name: None,
                }],
                identities: Vec::new(),
            }
        }

        let temp = tempfile::tempdir().unwrap();
        let mut paths = test_paths(temp.path());
        paths.skills_dir = paths.data_dir.join("skills");
        let directory = paths.persona_avatars_dir();
        std::fs::create_dir_all(&directory).unwrap();
        let name = format!("{}.png", "a".repeat(64));
        let asset = directory.join(&name);
        std::fs::write(&asset, "image").unwrap();

        cleanup_persona_assets(
            &paths,
            &prompts(format!("persona-avatars/{name}")),
            &prompts(format!("./persona-avatars/{name}")),
        );
        assert!(asset.is_file());
    }

    #[test]
    pub(crate) fn system_prompt_resource_is_not_exposed_as_a_persona_document() {
        let temp = tempfile::tempdir().unwrap();
        let mut paths = test_paths(temp.path());
        paths.skills_dir = paths.data_dir.join("skills");
        let prompts = paths.prompts_dir();
        std::fs::create_dir_all(&prompts).unwrap();
        std::fs::write(prompts.join("system-prompt.md"), "fallback").unwrap();
        std::fs::write(prompts.join("Persona.md"), "persona").unwrap();

        let documents = read_prompt_documents(&AppConfig::default(), &paths).unwrap();
        assert_eq!(documents.personas.len(), 1);
        assert_eq!(documents.personas[0].name, "Persona.md");
    }

    #[cfg(unix)]
    #[test]
    pub(crate) fn managed_persona_asset_validation_rejects_symlink_escape() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let mut paths = test_paths(temp.path());
        paths.skills_dir = paths.data_dir.join("skills");
        let directory = paths.persona_avatars_dir();
        std::fs::create_dir_all(&directory).unwrap();
        let outside = temp.path().join("outside.png");
        std::fs::write(&outside, "image").unwrap();
        let managed = directory.join("avatar.png");
        symlink(&outside, &managed).unwrap();

        assert!(validate_managed_persona_asset_file(&paths, &managed).is_err());
    }

    pub(crate) fn test_daemon_with_actor(
        root: &FilePath,
    ) -> (DaemonState, std::thread::JoinHandle<Result<()>>) {
        DaemonState::for_test_with_actor(test_paths(root), 8300).unwrap()
    }

    #[tokio::test]
    pub(crate) async fn one_shot_sessions_are_mintable_runnable_and_deletable_but_nothing_else() {
        let temp = tempfile::tempdir().unwrap();
        let state = DaemonState::for_test(test_paths(temp.path()), 8300).unwrap();
        let persona = active_persona_scope(&state);
        state
            .state_store
            .adopt_sessions_for_persona(&persona)
            .unwrap();
        let terminal = state.state_store.session_id().to_string();

        let data = handle_session_command(
            &state,
            IpcCommand::CreateSession {
                name: Some("一次性对话".to_string()),
                switch: false,
                kind: Some(crate::state::ASK_SESSION_KIND.to_string()),
                mode: None,
            },
        )
        .await
        .unwrap();
        let ask_id = data["session"]["session_id"].as_str().unwrap().to_string();

        // Minting it must not move the terminal lane, and it must not surface
        // in the session list.
        assert_eq!(&*state.state_store.session_id(), terminal.as_str());
        let listed = handle_session_command(
            &state,
            IpcCommand::ListSessions {
                mode: None,
            },
        )
        .await
        .unwrap();
        assert!(listed["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .all(|session| session["session_id"] != ask_id.as_str()));

        // A turn may target it. (SwitchSession 已随「终端集成会话不可改」
        // 整体移除,外部再无切换全局指针的入口。)
        assert_eq!(
            resolve_turn_session(&state, Some(ask_id.clone())).unwrap(),
            ask_id.clone().into()
        );

        // Other kinds are not mintable over IPC, and `ask` may not be created
        // as the session to switch into.
        assert!(handle_session_command(
            &state,
            IpcCommand::CreateSession {
                name: None,
                switch: false,
                kind: Some("subagent".to_string()),
                mode: None,
            },
        )
        .await
        .is_err());
        assert!(handle_session_command(
            &state,
            IpcCommand::CreateSession {
                name: None,
                switch: true,
                kind: Some(crate::state::ASK_SESSION_KIND.to_string()),
                mode: None,
            },
        )
        .await
        .is_err());

        // Deleting it is the teardown a one-shot turn performs.
        handle_session_command(
            &state,
            IpcCommand::DeleteSession {
                target: ipc::SessionRef::Id { id: ask_id.clone() },
            },
        )
        .await
        .unwrap();
        assert!(state.state_store.session_record(&ask_id).unwrap().is_none());
        assert!(resolve_turn_session(&state, Some(ask_id)).is_err());
    }

    #[tokio::test]
    pub(crate) async fn repl_session_lane_resumes_and_heals_without_moving_the_terminal_lane() {
        let temp = tempfile::tempdir().unwrap();
        let state = DaemonState::for_test(test_paths(temp.path()), 8300).unwrap();
        let persona = active_persona_scope(&state);
        state
            .state_store
            .adopt_sessions_for_persona(&persona)
            .unwrap();
        let terminal = state.state_store.session_id().to_string();
        let repl = state
            .state_store
            .create_session(&persona, "repl lane", crate::state::USER_SESSION_KIND, None)
            .unwrap();

        handle_session_command(
            &state,
            IpcCommand::SetReplSession {
                target: ipc::SessionRef::Id {
                    id: repl.session_id.clone(),
                },
            },
        )
        .await
        .unwrap();
        assert_eq!(
            state.state_store.repl_session(&persona).unwrap().as_deref(),
            Some(repl.session_id.as_str())
        );
        assert_eq!(&*state.state_store.session_id(), terminal.as_str());

        // A deleted REPL session must not strand the next REPL: the pointer
        // falls back to the terminal session and is healed in place.
        state.state_store.delete_session(&repl.session_id).unwrap();
        assert!(state.state_store.repl_session(&persona).unwrap().is_none());

        // One-shot sessions are not a valid REPL lane either.
        let ask = state
            .state_store
            .create_session(&persona, "一次性对话", crate::state::ASK_SESSION_KIND, None)
            .unwrap();
        assert!(handle_session_command(
            &state,
            IpcCommand::SetReplSession {
                target: ipc::SessionRef::Id {
                    id: ask.session_id.clone(),
                },
            },
        )
        .await
        .is_err());
    }

    #[tokio::test]
    pub(crate) async fn ipc_session_list_excludes_platform_owned_sessions() {
        let temp = tempfile::tempdir().unwrap();
        let state = DaemonState::for_test(test_paths(temp.path()), 8300).unwrap();
        let persona = active_persona_scope(&state);
        state
            .state_store
            .adopt_sessions_for_persona(&persona)
            .unwrap();
        let local = state
            .state_store
            .create_session(&persona, "local", "user", None)
            .unwrap();
        let platform = state
            .state_store
            .create_session(&persona, "platform", "user", None)
            .unwrap();
        state
            .state_store
            .bind_platform_session(
                &PlatformSessionBindingKey {
                    platform: "onebot".to_string(),
                    account_id: "10000".to_string(),
                    conversation_kind: "group".to_string(),
                    conversation_id: "20000".to_string(),
                    participant_id: None,
                    persona: persona.clone(),
                },
                &platform.session_id,
            )
            .unwrap();

        let data = handle_session_command(
            &state,
            IpcCommand::ListSessions {
                mode: None,
            },
        )
        .await
        .unwrap();
        let ids = data["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|session| session["session_id"].as_str())
            .collect::<Vec<_>>();

        assert!(ids.contains(&local.session_id.as_str()));
        assert!(!ids.contains(&platform.session_id.as_str()));
    }

    #[test]
    pub(crate) fn target_session_state_does_not_move_the_default_session() {
        let temp = tempfile::tempdir().unwrap();
        let state = DaemonState::for_test(test_paths(temp.path()), 8300).unwrap();
        let persona = active_persona_scope(&state);
        state
            .state_store
            .adopt_sessions_for_persona(&persona)
            .unwrap();
        let default_session_id = state.state_store.session_id();
        let local = state
            .state_store
            .create_session(&persona, "repl local", "user", None)
            .unwrap();

        let snapshot = session_state_for(&state, &local.session_id).unwrap();

        assert_eq!(snapshot.session_id, local.session_id);
        assert_eq!(&*state.state_store.session_id(), &*default_session_id);
    }

    #[tokio::test]
    pub(crate) async fn tool_bridge_executes_with_session_scope_and_depth_guard() {
        let temp = tempfile::tempdir().unwrap();
        let state = DaemonState::for_test(test_paths(temp.path()), 8300).unwrap();
        let persona = active_persona_scope(&state);
        let session = state
            .state_store
            .create_session(&persona, "bridge", crate::state::USER_SESSION_KIND, None)
            .unwrap()
            .session_id;
        // 会话作用域生效:get_goal 在指定会话身份下执行,拿到 goal:null。
        let data = handle_session_command(
            &state,
            IpcCommand::ToolCall {
                session: Some(session.clone()),
                name: "job_status".to_string(),
                arguments: "{}".to_string(),
                origin: None,
                depth: 0,
            },
        )
        .await
        .unwrap();
        let output = data["output"].as_str().unwrap();
        assert!(!output.is_empty(), "unexpected: {output}");
        // 深度护栏。
        let denied = handle_session_command(
            &state,
            IpcCommand::ToolCall {
                session: Some(session),
                name: "job_status".to_string(),
                arguments: "{}".to_string(),
                origin: None,
                depth: crate::tools::workspace::MAX_BRIDGE_DEPTH,
            },
        )
        .await
        .unwrap_err();
        assert!(denied.contains("recursion limit"));
    }

    #[tokio::test]
    pub(crate) async fn dev_sessions_live_under_the_reserved_persona_and_pin_dev_mode() {
        let temp = tempfile::tempdir().unwrap();
        let state = DaemonState::for_test(test_paths(temp.path()), 8300).unwrap();
        // mode:"dev" 建到保留人格 dev 名下。
        let data = handle_session_command(
            &state,
            IpcCommand::CreateSession {
                name: Some("dev work".to_string()),
                switch: false,
                kind: None,
                mode: Some("dev".to_string()),
            },
        )
        .await
        .unwrap();
        let session_id = data["session"]["session_id"].as_str().unwrap().to_string();
        let record = state
            .state_store
            .session_record(&session_id)
            .unwrap()
            .unwrap();
        assert_eq!(record.persona, crate::state::DEV_PERSONA);
        // 会话模式由记录强制:dev 会话怎么请求都是 Dev,普通会话反之。
        assert_eq!(
            turn_mode_for_session(&state.state_store, &session_id, AgentMode::Normal),
            AgentMode::Dev
        );
        let normal_id = state.state_store.session_id().to_string();
        assert_eq!(
            turn_mode_for_session(&state.state_store, &normal_id, AgentMode::Dev),
            AgentMode::Normal
        );
    }

    #[tokio::test]
    pub(crate) async fn creating_a_repl_session_does_not_move_the_default_session() {
        let temp = tempfile::tempdir().unwrap();
        let state = DaemonState::for_test(test_paths(temp.path()), 8300).unwrap();
        let persona = active_persona_scope(&state);
        state
            .state_store
            .adopt_sessions_for_persona(&persona)
            .unwrap();
        let default_session_id = state.state_store.session_id();

        let data = handle_session_command(
            &state,
            IpcCommand::CreateSession {
                name: Some("repl local".to_string()),
                switch: false,
                kind: None,
                mode: None,
            },
        )
        .await
        .unwrap();

        assert_ne!(
            data["session"]["session_id"].as_str(),
            Some(default_session_id.as_ref())
        );
        assert_eq!(&*state.state_store.session_id(), &*default_session_id);
    }

    #[tokio::test]
    pub(crate) async fn actor_undo_is_scoped_to_the_requested_session() {
        let temp = tempfile::tempdir().unwrap();
        let (state, actor_join) = test_daemon_with_actor(temp.path());
        let persona = active_persona_scope(&state);
        state
            .state_store
            .adopt_sessions_for_persona(&persona)
            .unwrap();
        let default_session_id = state.state_store.session_id();
        let default_store = state.state_store.pinned(&default_session_id);
        default_store
            .start_turn("default-turn", "default", std::process::id())
            .unwrap();
        default_store
            .complete_turn("default-turn", "default reply", None)
            .unwrap();
        let local = state
            .state_store
            .create_session(&persona, "repl local", "user", None)
            .unwrap();
        let local_store = state.state_store.pinned(&local.session_id);
        local_store
            .start_turn("local-turn", "local", std::process::id())
            .unwrap();
        local_store
            .complete_turn("local-turn", "local reply", None)
            .unwrap();

        let (reply, receiver) = oneshot::channel();
        state
            .actor_tx
            .send(ActorCommand::Undo {
                session_id: local.session_id.clone().into(),
                reply,
            })
            .unwrap();
        receiver.await.unwrap().unwrap();

        assert!(local_store.load_turns().unwrap().is_empty());
        assert_eq!(default_store.load_turns().unwrap().len(), 1);
        assert_eq!(&*state.state_store.session_id(), &*default_session_id);
        state.actor_tx.send(ActorCommand::Shutdown).unwrap();
        actor_join.join().unwrap().unwrap();
    }

    #[test]
    pub(crate) fn local_session_resolution_rejects_platform_ids_and_prefers_local_names() {
        let temp = tempfile::tempdir().unwrap();
        let state = DaemonState::for_test(test_paths(temp.path()), 8300).unwrap();
        let persona = active_persona_scope(&state);
        state
            .state_store
            .adopt_sessions_for_persona(&persona)
            .unwrap();
        let local = state
            .state_store
            .create_session(&persona, "shared", "user", None)
            .unwrap();
        let platform = state
            .state_store
            .create_session(&persona, "shared", "user", None)
            .unwrap();
        state
            .state_store
            .bind_platform_session(
                &PlatformSessionBindingKey {
                    platform: "onebot".to_string(),
                    account_id: "10000".to_string(),
                    conversation_kind: "private".to_string(),
                    conversation_id: "20000".to_string(),
                    participant_id: Some("20000".to_string()),
                    persona,
                },
                &platform.session_id,
            )
            .unwrap();

        let resolved = resolve_local_session_ref(
            &state,
            &ipc::SessionRef::Name {
                name: "SHARED".to_string(),
            },
        )
        .unwrap();
        assert_eq!(resolved.session_id, local.session_id);
        assert!(resolve_local_session_ref(
            &state,
            &ipc::SessionRef::Id {
                id: platform.session_id,
            },
        )
        .is_err());
    }

    #[tokio::test]
    pub(crate) async fn reset_memory_bumps_generation_in_the_requested_scope_only() {
        let temp = tempfile::tempdir().unwrap();
        let state = DaemonState::for_test(test_paths(temp.path()), 8300).unwrap();
        let config = state.manager.lock().unwrap().config.clone();
        let dev_store = crate::memory::MemoryStore::new(&config.dev_scoped(), &state.paths);
        dev_store.init().unwrap();
        let normal_store = crate::memory::MemoryStore::new(&config, &state.paths);
        normal_store.init().unwrap();
        let (_, dev_gen_before) = dev_store.identity().unwrap();
        let (_, normal_gen_before) = normal_store.identity().unwrap();

        handle_session_command(
            &state,
            IpcCommand::ResetMemory {
                mode: Some("dev".to_string()),
            },
        )
        .await
        .unwrap();

        // 只有 dev 命名空间的代数被抬升,普通人格纹丝不动。
        let (_, dev_gen_after) = dev_store.identity().unwrap();
        let (_, normal_gen_after) = normal_store.identity().unwrap();
        assert_eq!(dev_gen_after, dev_gen_before + 1);
        assert_eq!(normal_gen_after, normal_gen_before);
    }

    #[tokio::test]
    pub(crate) async fn dev_sessions_resolve_by_id_and_list_under_dev_mode() {
        let temp = tempfile::tempdir().unwrap();
        let state = DaemonState::for_test(test_paths(temp.path()), 8300).unwrap();
        let persona = active_persona_scope(&state);
        state
            .state_store
            .adopt_sessions_for_persona(&persona)
            .unwrap();
        let dev = state
            .state_store
            .create_session(crate::state::DEV_PERSONA, "编译修复", "user", None)
            .unwrap();

        // 验收问题二:显式 id 寻址必须穿过人格过滤,否则 dev REPL 的
        // 起回合/切换全部 404 并落回默认会话。
        let resolved = resolve_local_session_ref(
            &state,
            &ipc::SessionRef::Id {
                id: dev.session_id.clone(),
            },
        )
        .unwrap();
        assert_eq!(resolved.session_id, dev.session_id);
        assert_eq!(resolved.persona, crate::state::DEV_PERSONA);

        // dev 会话不进普通人格的名字空间;dev 模式列表只见 dev 会话。
        assert!(resolve_local_session_ref(
            &state,
            &ipc::SessionRef::Name {
                name: "编译修复".to_string(),
            },
        )
        .is_err());
        let listed = handle_session_command(
            &state,
            IpcCommand::ListSessions {
                mode: Some("dev".to_string()),
            },
        )
        .await
        .unwrap();
        let ids: Vec<&str> = listed["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|session| session["session_id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, vec![dev.session_id.as_str()]);
        let normal = handle_session_command(
            &state,
            IpcCommand::ListSessions {
                mode: None,
            },
        )
        .await
        .unwrap();
        assert!(normal["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .all(|session| session["session_id"].as_str() != Some(dev.session_id.as_str())));
    }

    #[test]
    pub(crate) fn attachment_validation_accepts_utf8_code_and_rejects_unknown_binary() {
        let (kind, mime, width, height) =
            inspect_user_attachment("main.rs", b"fn main() {}\n").unwrap();
        assert_eq!(kind, "text");
        assert_eq!(mime, "text/plain");
        assert_eq!((width, height), (0, 0));
        assert!(inspect_user_attachment("payload.bin", &[0xff, 0xfe, 0xfd]).is_err());
        assert!(inspect_user_attachment("notes.exe", b"plain text").is_err());
    }

    #[test]
    pub(crate) fn attachment_download_header_preserves_utf8_filename() {
        let value = attachment_content_disposition("报告 1.md", false)
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        assert!(value.starts_with("attachment;"));
        assert!(value.contains("filename*=UTF-8''%E6%8A%A5%E5%91%8A%201.md"));
    }

    #[tokio::test]
    pub(crate) async fn config_reload_applies_disk_config() {
        let temp = tempfile::tempdir().unwrap();
        let (state, actor_join) = test_daemon_with_actor(temp.path());
        let mut next_config = state.manager.lock().unwrap().config.clone();
        next_config.display.show_token_usage = !next_config.display.show_token_usage;
        let expected = next_config.display.show_token_usage;
        next_config.save(&state.paths).unwrap();

        let (mut client, server) = tokio::net::UnixStream::pair().unwrap();
        let server_state = state.clone();
        let task = tokio::spawn(async move { handle_ipc_connection(server_state, server).await });
        ipc::send(&mut client, &IpcRequest::new(IpcCommand::ReloadConfig))
            .await
            .unwrap();
        let response = ipc::receive::<IpcFrame>(&mut client)
            .await
            .unwrap()
            .unwrap();

        assert!(matches!(response, IpcFrame::AdminResult { .. }));
        task.await.unwrap().unwrap();
        let manager = state.manager.lock().unwrap();
        assert_eq!(manager.config.display.show_token_usage, expected);
        assert!(!manager.admin_busy);
        drop(manager);

        state.actor_tx.send(ActorCommand::Shutdown).unwrap();
        actor_join.join().unwrap().unwrap();
    }

    #[tokio::test]
    pub(crate) async fn failed_config_reload_preserves_the_candidate_file() {
        let temp = tempfile::tempdir().unwrap();
        let state = DaemonState::for_test(test_paths(temp.path()), 8300).unwrap();
        let runtime_value = state
            .manager
            .lock()
            .unwrap()
            .config
            .display
            .show_token_usage;
        let mut candidate = state.manager.lock().unwrap().config.clone();
        candidate.display.show_token_usage = !runtime_value;
        candidate.save(&state.paths).unwrap();

        let (mut client, server) = tokio::net::UnixStream::pair().unwrap();
        let server_state = state.clone();
        let task = tokio::spawn(async move { handle_ipc_connection(server_state, server).await });
        ipc::send(&mut client, &IpcRequest::new(IpcCommand::ReloadConfig))
            .await
            .unwrap();
        let response = ipc::receive::<IpcFrame>(&mut client)
            .await
            .unwrap()
            .unwrap();

        assert!(matches!(
            response,
            IpcFrame::Error {
                code: None,
                message,
            } if message.contains("worker is unavailable")
        ));
        task.await.unwrap().unwrap();
        assert_eq!(
            AppConfig::load(&state.paths)
                .unwrap()
                .display
                .show_token_usage,
            !runtime_value
        );
        let manager = state.manager.lock().unwrap();
        assert_eq!(manager.config.display.show_token_usage, runtime_value);
        assert!(!manager.admin_busy);
    }

    #[tokio::test]
    pub(crate) async fn busy_config_reload_returns_an_error_frame() {
        let temp = tempfile::tempdir().unwrap();
        let (state, actor_join) = test_daemon_with_actor(temp.path());
        state
            .manager
            .lock()
            .unwrap()
            .config
            .save(&state.paths)
            .unwrap();
        // Running turns no longer block a reload (they keep their own config
        // snapshot); only a concurrent admin operation does.
        state.manager.lock().unwrap().admin_busy = true;

        let (mut client, server) = tokio::net::UnixStream::pair().unwrap();
        let server_state = state.clone();
        let task = tokio::spawn(async move { handle_ipc_connection(server_state, server).await });
        ipc::send(&mut client, &IpcRequest::new(IpcCommand::ReloadConfig))
            .await
            .unwrap();
        let response = ipc::receive::<IpcFrame>(&mut client)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            response,
            IpcFrame::Error {
                code: Some(ipc::ErrorCode::Busy),
                message,
            } if message.contains("busy with another operation")
        ));
        task.await.unwrap().unwrap();

        state.manager.lock().unwrap().admin_busy = false;
        state.actor_tx.send(ActorCommand::Shutdown).unwrap();
        actor_join.join().unwrap().unwrap();
    }

    #[tokio::test]
    pub(crate) async fn config_reload_succeeds_and_keeps_turns_running() {
        let temp = tempfile::tempdir().unwrap();
        let (state, actor_join) = test_daemon_with_actor(temp.path());
        state
            .manager
            .lock()
            .unwrap()
            .config
            .save(&state.paths)
            .unwrap();
        let (cancel, cancel_rx) = tokio::sync::watch::channel(false);
        state.manager.lock().unwrap().active_runs.insert(
            "hot-reload-run".to_string(),
            RunInfo {
                session_id: state.state_store.session_id().into(),
                mode: AgentMode::Normal,
                audience: PromptAudience::External,
                cancel,
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

        let (mut client, server) = tokio::net::UnixStream::pair().unwrap();
        let server_state = state.clone();
        let task = tokio::spawn(async move { handle_ipc_connection(server_state, server).await });
        ipc::send(&mut client, &IpcRequest::new(IpcCommand::ReloadConfig))
            .await
            .unwrap();
        let response = ipc::receive::<IpcFrame>(&mut client)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(response, IpcFrame::AdminResult { .. }));
        task.await.unwrap().unwrap();

        // A turn-safe reload neither cancels nor waits out the running turn.
        assert!(!*cancel_rx.borrow());
        {
            let manager = state.manager.lock().unwrap();
            assert!(manager.active_runs.contains_key("hot-reload-run"));
            assert!(!manager.admin_busy);
        }

        state.manager.lock().unwrap().active_runs.clear();
        state.actor_tx.send(ActorCommand::Shutdown).unwrap();
        actor_join.join().unwrap().unwrap();
    }

    #[tokio::test]
    pub(crate) async fn set_session_models_ipc_pins_and_clears_the_override() {
        let temp = tempfile::tempdir().unwrap();
        let state = DaemonState::for_test(test_paths(temp.path()), 8300).unwrap();
        let choice = state
            .manager
            .lock()
            .unwrap()
            .config
            .text_provider_model_choices()
            .first()
            .cloned()
            .expect("the default config configures at least one model");
        let persona = active_persona_scope(&state);
        let record = state
            .state_store
            .create_session(&persona, "", "user", None)
            .unwrap();
        let target = ipc::SessionRef::Id {
            id: record.session_id.clone(),
        };

        handle_session_command(
            &state,
            IpcCommand::SetSessionModels {
                target: target.clone(),
                models: vec![crate::config::ActiveProviderModelConfig {
                    provider_id: choice.provider_id.clone(),
                    model: choice.model.clone(),
                }],
            },
        )
        .await
        .unwrap();
        let session_id = record.session_id.clone();
        let stored = state
            .state_store
            .session_model_override(&session_id)
            .unwrap()
            .expect("the override is stored");
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].provider_id, choice.provider_id);
        assert_eq!(stored[0].model, choice.model);

        // Unknown models are rejected and leave the override untouched.
        let error = handle_session_command(
            &state,
            IpcCommand::SetSessionModels {
                target: target.clone(),
                models: vec![crate::config::ActiveProviderModelConfig {
                    provider_id: "no-such-provider".to_string(),
                    model: "no-such-model".to_string(),
                }],
            },
        )
        .await
        .unwrap_err();
        assert!(error.contains("no-such-provider"));
        assert!(state
            .state_store
            .session_model_override(&session_id)
            .unwrap()
            .is_some());

        // An empty list clears the override (follow the global pool again).
        handle_session_command(
            &state,
            IpcCommand::SetSessionModels {
                target,
                models: Vec::new(),
            },
        )
        .await
        .unwrap();
        assert!(state
            .state_store
            .session_model_override(&session_id)
            .unwrap()
            .is_none());
    }

    #[test]
    pub(crate) fn qq_group_history_scope_and_offender_deletion_are_isolated() {
        let temp = tempfile::tempdir().unwrap();
        let store = StateStore::new(&test_paths(temp.path())).unwrap();
        let scope = qq_group_scope("123456", "234567").unwrap();
        store
            .plugin_put_json(
                &scope,
                "offender_history",
                &json!({
                    "345678": { "user_id": "345678", "ban_count": 2 },
                    "456789": { "user_id": "456789", "ban_count": 1 }
                }),
            )
            .unwrap();
        store
            .plugin_update_json::<HashMap<String, Value>, _>(
                &scope,
                "offender_history",
                |current| {
                    let mut records = current.unwrap_or_default();
                    records.remove("345678");
                    Ok(Some(records))
                },
            )
            .unwrap();
        let remaining = store
            .plugin_get_json::<HashMap<String, Value>>(&scope, "offender_history")
            .unwrap()
            .unwrap();
        assert!(!remaining.contains_key("345678"));
        assert!(remaining.contains_key("456789"));
        assert_eq!(scope.platform, "onebot");
        assert_eq!(scope.conversation_kind, "group");
    }

    #[tokio::test]
    pub(crate) async fn platform_session_reset_is_serialized_per_target_session() {
        let temp = tempfile::tempdir().unwrap();
        let (state, actor_join) = test_daemon_with_actor(temp.path());
        let target = state
            .state_store
            .create_session("gqy", "qq target", "user", None)
            .unwrap();
        let other = state
            .state_store
            .create_session("gqy", "other", "user", None)
            .unwrap();
        let target_store = state.state_store.pinned(&target.session_id);
        target_store
            .start_turn("before_reset", "hello", std::process::id())
            .unwrap();
        target_store
            .complete_turn("before_reset", "world", None)
            .unwrap();

        let (other_cancel, _other_cancel_rx) = tokio::sync::watch::channel(false);
        state.manager.lock().unwrap().active_runs.insert(
            "other_run".to_string(),
            RunInfo {
                session_id: other.session_id.clone().into(),
                mode: AgentMode::Normal,
                audience: PromptAudience::Internal,
                cancel: other_cancel,
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
        assert!(
            clear_platform_session_content(&state, target.session_id.clone().into())
                .await
                .is_ok()
        );
        assert!(target_store.load_turns().unwrap().is_empty());
        assert!(!state.manager.lock().unwrap().admin_busy);

        target_store
            .start_turn("must_survive", "still here", std::process::id())
            .unwrap();
        target_store
            .complete_turn("must_survive", "answer", None)
            .unwrap();
        let (target_cancel, _target_cancel_rx) = tokio::sync::watch::channel(false);
        state.manager.lock().unwrap().active_runs.insert(
            "target_run".to_string(),
            RunInfo {
                session_id: target.session_id.clone().into(),
                mode: AgentMode::Normal,
                audience: PromptAudience::External,
                cancel: target_cancel,
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
        assert!(matches!(
            clear_platform_session_content(&state, target.session_id.clone().into()).await,
            Err(PlatformSessionResetError::Busy)
        ));
        assert_eq!(target_store.load_turns().unwrap().len(), 1);
        assert!(!state.manager.lock().unwrap().admin_busy);

        state.manager.lock().unwrap().active_runs.clear();
        target_store
            .start_turn("database_running", "working", std::process::id())
            .unwrap();
        assert!(matches!(
            clear_platform_session_content(&state, target.session_id.clone().into()).await,
            Err(PlatformSessionResetError::Busy)
        ));
        assert!(!state.manager.lock().unwrap().admin_busy);
        target_store.interrupt_turn("database_running").unwrap();

        state.actor_tx.send(ActorCommand::Shutdown).unwrap();
        actor_join.join().unwrap().unwrap();
        assert!(matches!(
            clear_platform_session_content(&state, target.session_id.into()).await,
            Err(PlatformSessionResetError::Unavailable)
        ));
        assert!(!state.manager.lock().unwrap().admin_busy);
    }

}

