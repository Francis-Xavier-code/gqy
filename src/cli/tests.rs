//! tests — 自 src/cli.rs 外移。
#![cfg(test)]

use super::*;

mod repl_input_tests {
    use super::*;
    use crate::llm::ChatStreamKind;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn sample_pop_turn(status: TurnStatus) -> Turn {
        Turn {
            turn_id: "turn-1".to_string(),
            seq: 1,
            user_content: "first prompt line\nsecond prompt line".to_string(),
            display_content: "first prompt line\nsecond prompt line".to_string(),
            user_timestamp: "2026-07-19 10:42".to_string(),
            assistant_content: "first answer line\nsecond answer line".to_string(),
            assistant_reasoning: Some("private reasoning".to_string()),
            assistant_provider_id: None,
            assistant_model: None,
            assistant_timestamp: Some("2026-07-19 10:43".to_string()),
            status,
            tool_reports: vec!["hidden tool report".to_string()],
            tool_flow: Vec::new(),
            question_exchanges: Vec::new(),
            followups: Vec::new(),
            attachments: Vec::new(),
            hidden: false,
            is_summary: false,
            owner_pid: None,
            token_total: 0,
            token_prompt: 0,
            token_cache_read: 0,
            token_usage_estimated: false,
            revision: 0,
            journal_events: Vec::new(),
            context_messages: Vec::new(),
        }
    }

    fn pop_test_paths(root: &std::path::Path) -> GQYPaths {
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
            system_scripts_dir: root.join("system/scripts"),
        }
    }

    #[test]
    fn terminal_frame_tracks_ansi_and_wide_graphemes() {
        let layout = terminal_frame_layout("\x1b[32mAB\x1b[0m\n中👨‍👩‍👧‍👦".as_bytes(), (3, 2), 12, None);

        assert_eq!(layout.cursor, (4, 3));
        assert_eq!(layout.occupied_bottom, Some(3));
    }

    #[test]
    fn terminal_frame_wraps_before_the_next_wide_grapheme() {
        let layout = terminal_frame_layout("中🙂".as_bytes(), (8, 1), 10, None);

        assert_eq!(layout.cursor, (2, 2));
        assert_eq!(layout.occupied_bottom, Some(2));
    }

    #[test]
    fn terminal_frame_applies_cursor_motion_without_losing_bottom_occupancy() {
        let layout = terminal_frame_layout(b"first\nsecond\x1b[1A\x1b[3G!", (0, 4), 20, None);

        assert_eq!(layout.cursor, (3, 4));
        assert_eq!(layout.occupied_bottom, Some(5));
    }

    #[test]
    fn terminal_frame_scroll_margin_keeps_cursor_above_live_input() {
        let layout = terminal_frame_layout("one\n二\nthree".as_bytes(), (0, 5), 20, Some(5));

        assert_eq!(layout.cursor, (5, 5));
        assert_eq!(layout.occupied_bottom, Some(5));
    }

    #[test]
    fn live_frame_uses_the_gap_only_for_a_terminating_newline() {
        let content = terminal_frame_layout(b"answer", (0, 5), 20, None);
        assert_eq!(live_frame_output_bottom(6, content), Some(5));

        let terminated = terminal_frame_layout(b"answer\n", (0, 5), 20, None);
        assert_eq!(live_frame_output_bottom(6, terminated), Some(6));
        let bounded = terminal_frame_layout(
            b"answer\n",
            (0, 5),
            20,
            live_frame_output_bottom(6, terminated),
        );
        assert_eq!(bounded.cursor, (0, 6));
        assert_eq!(bounded.occupied_bottom, Some(5));
    }

    #[test]
    fn models_is_the_cli_model_selector() {
        let matches = localized_command()
            .try_get_matches_from(["gqy", "models", "1"])
            .unwrap();
        let cli = Cli::from_arg_matches(&matches).unwrap();

        assert!(matches!(
            cli.command,
            Some(Command::Models(ModelsArgs { target: Some(ref target) })) if target == "1"
        ));
        let old_matches = localized_command()
            .try_get_matches_from(["gqy", "providers"])
            .unwrap();
        let old_cli = Cli::from_arg_matches(&old_matches).unwrap();
        assert!(old_cli.command.is_none());
        assert_eq!(old_cli.message, ["providers"]);
    }

    #[test]
    fn variant_is_a_cli_subcommand_with_an_optional_name() {
        let cli = parse_args(["gqy", "variant"].map(OsString::from).to_vec()).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Variant(VariantArgs { name: None }))
        ));

        let cli = parse_args(["gqy", "variant", "high"].map(OsString::from).to_vec()).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Variant(VariantArgs { name })) if name.as_deref() == Some("high")
        ));

        assert!(parse_args(
            ["gqy", "variant", "high", "extra"]
                .map(OsString::from)
                .to_vec()
        )
        .is_err());
    }

    #[tokio::test]
    async fn one_shot_turns_default_to_a_throwaway_session() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let paths = GQYPaths {
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
        };

        // Neither flag: `gqy ask` / `gqy '<message>'` must not touch a real
        // conversation. `--continue` opts back into the terminal session.
        // Both resolve without contacting the daemon.
        assert_eq!(
            one_shot_session(&paths, None, false).await.unwrap(),
            TurnSession::Ephemeral
        );
        assert_eq!(
            one_shot_session(&paths, None, true).await.unwrap(),
            TurnSession::Current
        );
    }

    #[test]
    fn continue_and_session_flags_are_mutually_exclusive() {
        let cli = parse_args(["gqy", "-c", "hello"].map(OsString::from).to_vec()).unwrap();
        assert!(cli.continue_session);
        assert_eq!(cli.message, vec!["hello".to_string()]);

        let cli = parse_args(
            ["gqy", "--session", "2", "hello"]
                .map(OsString::from)
                .to_vec(),
        )
        .unwrap();
        assert!(!cli.continue_session);
        assert_eq!(cli.session.as_deref(), Some("2"));

        assert!(parse_args(
            ["gqy", "-c", "--session", "2", "hello"]
                .map(OsString::from)
                .to_vec()
        )
        .is_err());
    }

    #[test]
    fn replayed_job_wake_turns_are_not_drawn_as_user_prompts() {
        let config = AppConfig::default();
        let wake = crate::state::TurnReplay {
            display_content: "[后台任务完成] 子代理完成 82bea3 · 后台测试A".to_string(),
            assistant_content: "跑完了。".to_string(),
            entries: Vec::new(),
            is_job_wake: true,
        };
        let typed = crate::state::TurnReplay {
            display_content: "帮我改一下 README".to_string(),
            assistant_content: "改好了。".to_string(),
            entries: Vec::new(),
            is_job_wake: false,
        };

        let frame = session_replay_frame(&[wake], AgentMode::Normal, &config, 80).unwrap();
        let frame = String::from_utf8_lossy(&frame);
        // Dim ⚙ notice with the bracketed prefix stripped, exactly like the
        // live path — never the user bubble's bar.
        assert!(frame.contains("⚙ 子代理完成 82bea3 · 后台测试A"));
        assert!(!frame.contains("[后台任务完成]"));
        assert!(!frame.contains(&submitted_echo_bar(AgentMode::Normal)));

        let frame = session_replay_frame(&[typed], AgentMode::Normal, &config, 80).unwrap();
        let frame = String::from_utf8_lossy(&frame);
        assert!(frame.contains(&submitted_echo_bar(AgentMode::Normal)));
        assert!(!frame.contains('⚙'));
    }

    #[test]
    fn picker_keys_reach_delete_only_through_a_modifier() {
        use crossterm::event::{KeyCode, KeyModifiers};
        let plain = KeyModifiers::NONE;
        let control = KeyModifiers::CONTROL;

        // Every printable character is search input, so a bare `d` must never
        // be a shortcut — deletion needs Ctrl+D (or the Delete key).
        assert_eq!(
            inline_select_key(KeyCode::Char('d'), plain, true),
            InlineSelectKey::Char('d')
        );
        assert_eq!(
            inline_select_key(KeyCode::Char('d'), control, true),
            InlineSelectKey::DeleteRequest
        );
        assert_eq!(
            inline_select_key(KeyCode::Delete, plain, true),
            InlineSelectKey::DeleteRequest
        );

        // Pickers that did not opt in stay exactly as they were.
        assert_eq!(
            inline_select_key(KeyCode::Char('d'), control, false),
            InlineSelectKey::Ignore
        );
        assert_eq!(
            inline_select_key(KeyCode::Delete, plain, false),
            InlineSelectKey::Ignore
        );

        assert_eq!(
            inline_select_key(KeyCode::Char('c'), control, true),
            InlineSelectKey::Cancel
        );
        assert_eq!(
            inline_select_key(KeyCode::Esc, plain, true),
            InlineSelectKey::Cancel
        );
        assert_eq!(
            inline_select_key(KeyCode::Enter, plain, true),
            InlineSelectKey::Accept
        );
        assert_eq!(
            inline_select_key(KeyCode::Char('j'), plain, true),
            InlineSelectKey::Down
        );
        assert_eq!(
            inline_select_key(KeyCode::Char('k'), plain, true),
            InlineSelectKey::Up
        );
    }

    #[test]
    fn web_is_a_cli_subcommand_with_local_server_options() {
        let cli = parse_args(
            ["gqy", "web", "--port", "4100"]
                .map(OsString::from)
                .to_vec(),
        )
        .unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Web(WebArgs {
                port: 4100,
                bind: None,
                password: None,
                password_file: None,
                port_explicit: true,
            }))
        ));

        for arg in ["stop", "status", "restart", "--status", "--stop"] {
            assert!(parse_args(["gqy", "web", arg].map(OsString::from).to_vec()).is_err());
        }

        let cli = parse_args(["gqy", "web"].map(OsString::from).to_vec()).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Web(WebArgs {
                port: 8300,
                bind: None,
                password: None,
                password_file: None,
                port_explicit: false,
            }))
        ));

        let cli = parse_args(["gqy", "web", "-p"].map(OsString::from).to_vec()).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Web(WebArgs {
                password: Some(password),
                ..
            })) if password.is_empty()
        ));
        for args in [
            vec!["gqy", "web", "-p", "secret"],
            vec!["gqy", "web", "--password=secret"],
            vec!["gqy", "web", "-psecret"],
        ] {
            assert!(parse_args(args.into_iter().map(OsString::from).collect()).is_err());
        }

        let cli = parse_args(
            ["gqy", "web", "--password-file", "/tmp/gqy-password"]
                .map(OsString::from)
                .to_vec(),
        )
        .unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Web(WebArgs {
                password: None,
                password_file: Some(path),
                ..
            })) if path == PathBuf::from("/tmp/gqy-password")
        ));

        assert!(parse_args(["gqy", "web", "--public"].map(OsString::from).to_vec(),).is_err());
    }

    #[test]
    fn web_password_is_materialized_as_a_private_file() {
        let temp = tempfile::tempdir().unwrap();
        let paths = pop_test_paths(temp.path());
        let args = WebArgs {
            port: 9400,
            bind: None,
            password: Some("very-secret".to_string()),
            password_file: None,
            port_explicit: false,
        };

        let launch = web_launch_config(&paths, &args).unwrap().unwrap();

        assert_eq!(launch.port, 9400);
        let password_file = launch.password_file.unwrap();
        let password_dir = paths.managed_web_password_dir();
        assert_eq!(password_file.parent(), Some(password_dir.as_path()));
        assert_eq!(
            std::fs::read_to_string(&password_file).unwrap(),
            "very-secret"
        );
        assert_eq!(
            std::fs::metadata(password_file)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[test]
    fn bare_web_does_not_override_the_persisted_launch_config() {
        let temp = tempfile::tempdir().unwrap();
        let paths = pop_test_paths(temp.path());
        let args = WebArgs {
            port: ipc::DEFAULT_WEB_PORT,
            bind: None,
            password: None,
            password_file: None,
            port_explicit: false,
        };

        assert!(web_launch_config(&paths, &args).unwrap().is_none());
    }

    #[test]
    fn explicit_password_file_is_copied_into_private_gqy_state() {
        let temp = tempfile::tempdir().unwrap();
        let paths = pop_test_paths(temp.path());
        let external = temp.path().join("external-password");
        std::fs::write(&external, "file-secret\n").unwrap();
        let args = WebArgs {
            port: ipc::DEFAULT_WEB_PORT,
            bind: None,
            password: None,
            password_file: Some(external.clone()),
            port_explicit: false,
        };

        let launch = web_launch_config(&paths, &args).unwrap().unwrap();
        let managed = launch.password_file.unwrap();
        assert_ne!(managed, external);
        let password_dir = paths.managed_web_password_dir();
        assert_eq!(managed.parent(), Some(password_dir.as_path()));
        assert_eq!(std::fs::read_to_string(managed).unwrap(), "file-secret");
    }

    #[test]
    fn daemon_owns_lifecycle_and_log_commands() {
        for (arg, expected) in [
            ("start", "start"),
            ("stop", "stop"),
            ("restart", "restart"),
            ("status", "status"),
        ] {
            let cli = parse_args(["gqy", "daemon", arg].map(OsString::from).to_vec()).unwrap();
            let actual = match cli.command {
                Some(Command::Daemon(DaemonArgs {
                    command: Some(DaemonCommand::Start),
                    ..
                })) => "start",
                Some(Command::Daemon(DaemonArgs {
                    command: Some(DaemonCommand::Stop),
                    ..
                })) => "stop",
                Some(Command::Daemon(DaemonArgs {
                    command: Some(DaemonCommand::Restart),
                    ..
                })) => "restart",
                Some(Command::Daemon(DaemonArgs {
                    command: Some(DaemonCommand::Status),
                    ..
                })) => "status",
                other => panic!("unexpected command: {other:?}"),
            };
            assert_eq!(actual, expected);
        }

        let cli = parse_args(["gqy", "daemon", "logs"].map(OsString::from).to_vec()).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Daemon(DaemonArgs {
                command: Some(DaemonCommand::Logs(DaemonLogsArgs { lines: None, .. })),
                ..
            }))
        ));

        let cli = parse_args(
            ["gqy", "daemon", "logs", "-n", "25"]
                .map(OsString::from)
                .to_vec(),
        )
        .unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Daemon(DaemonArgs {
                command: Some(DaemonCommand::Logs(DaemonLogsArgs { lines: Some(25), .. })),
                ..
            }))
        ));
    }

    #[test]
    fn reload_is_a_top_level_command() {
        let cli = parse_args(["gqy", "reload"].map(OsString::from).to_vec()).unwrap();
        assert!(matches!(cli.command, Some(Command::Reload)));
        assert!(parse_args(["gqy", "reload", "extra"].map(OsString::from).to_vec()).is_err());
    }

    #[test]
    fn config_reload_response_uses_codes_and_supports_legacy_busy_errors() {
        assert_eq!(
            validate_config_reload_response(Some(IpcFrame::coded_error(
                ipc::ErrorCode::Busy,
                "localized busy message",
            )))
            .unwrap(),
            ConfigReloadResponse::Busy
        );
        assert_eq!(
            validate_config_reload_response(Some(IpcFrame::error(ipc::ADMIN_BUSY_MESSAGE)))
                .unwrap(),
            ConfigReloadResponse::Busy
        );
        assert!(
            validate_config_reload_response(Some(IpcFrame::error("invalid configuration")))
                .is_err()
        );
    }

    #[tokio::test]
    async fn config_reload_retries_busy_responses_until_success() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let observed = attempts.clone();

        retry_config_reload(4, Duration::ZERO, move || {
            let attempts = attempts.clone();
            async move {
                let attempt = attempts.fetch_add(1, Ordering::SeqCst) + 1;
                Ok(if attempt < 3 {
                    ConfigReloadResponse::Busy
                } else {
                    ConfigReloadResponse::Reloaded
                })
            }
        })
        .await
        .unwrap();

        assert_eq!(observed.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn config_reload_stops_after_the_attempt_limit() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let observed = attempts.clone();

        let error = retry_config_reload(3, Duration::ZERO, move || {
            let attempts = attempts.clone();
            async move {
                attempts.fetch_add(1, Ordering::SeqCst);
                Ok(ConfigReloadResponse::Busy)
            }
        })
        .await
        .unwrap_err();

        assert_eq!(observed.load(Ordering::SeqCst), 3);
        assert!(error.to_string().contains('3'));
    }

    #[tokio::test]
    async fn config_reload_retries_coded_busy_frames_over_ipc() {
        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("reload.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let server = tokio::spawn(async move {
            for attempt in 1..=3 {
                let (mut stream, _) = listener.accept().await.unwrap();
                let request = ipc::receive::<IpcRequest>(&mut stream)
                    .await
                    .unwrap()
                    .unwrap();
                assert!(matches!(request.command, IpcCommand::ReloadConfig));
                let response = if attempt < 3 {
                    IpcFrame::coded_error(ipc::ErrorCode::Busy, ipc::ADMIN_BUSY_MESSAGE)
                } else {
                    IpcFrame::Ack
                };
                ipc::send(&mut stream, &response).await.unwrap();
            }
        });

        retry_config_reload(4, Duration::ZERO, || {
            request_config_reload_at(&socket, Duration::from_secs(1))
        })
        .await
        .unwrap();
        server.await.unwrap();
    }

    #[tokio::test]
    async fn config_reload_request_times_out_when_daemon_does_not_respond() {
        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("reload-timeout.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let request = ipc::receive::<IpcRequest>(&mut stream)
                .await
                .unwrap()
                .unwrap();
            assert!(matches!(request.command, IpcCommand::ReloadConfig));
            std::future::pending::<()>().await;
        });

        let error = request_config_reload_at(&socket, Duration::from_millis(100))
            .await
            .unwrap_err();
        assert!(error
            .downcast_ref::<tokio::time::error::Elapsed>()
            .is_some());
        server.abort();
        let _ = server.await;
    }

    #[test]
    fn daemon_accepts_a_port_and_defaults_to_start() {
        let cli = parse_args(
            ["gqy", "daemon", "--port", "9412"]
                .map(OsString::from)
                .to_vec(),
        )
        .unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Daemon(DaemonArgs {
                port: Some(9412),
                command: None,
            }))
        ));

        let cli = parse_args(
            ["gqy", "daemon", "--port", "9412", "restart"]
                .map(OsString::from)
                .to_vec(),
        )
        .unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Daemon(DaemonArgs {
                port: Some(9412),
                command: Some(DaemonCommand::Restart),
            }))
        ));

        let cli = parse_args(
            ["gqy", "daemon", "start", "--port", "9412"]
                .map(OsString::from)
                .to_vec(),
        )
        .unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Daemon(DaemonArgs {
                port: Some(9412),
                command: Some(DaemonCommand::Start),
            }))
        ));

        assert!(parse_args(
            ["gqy", "daemon", "--password"]
                .map(OsString::from)
                .to_vec(),
        )
        .is_err());
    }

    #[test]
    fn daemon_web_urls_are_rendered_on_separate_aligned_lines() {
        let urls = vec![
            "http://127.0.0.1:8300".to_string(),
            "http://192.168.1.2:8300".to_string(),
        ];
        assert_eq!(
            daemon_web_status_lines("WebUI:", &urls),
            [
                "WebUI: http://127.0.0.1:8300",
                "       http://192.168.1.2:8300",
            ]
        );
        assert_eq!(
            daemon_web_status_lines("WebUI：", &urls),
            [
                "WebUI： http://127.0.0.1:8300",
                "        http://192.168.1.2:8300",
            ]
        );
    }

    #[test]
    fn daemon_log_formatter_parses_targets_and_preserves_multiline_content() {
        let parsed = parse_daemon_log_line(
            "2026-07-29T12:34:56.789Z  INFO gqy::qq: listener ready port=8090",
        )
        .unwrap();
        assert_eq!(parsed.level, "INFO");
        assert_eq!(parsed.module, "gqy::qq");
        assert_eq!(parsed.message, "listener ready port=8090");

        let rendered = format_daemon_log_line(
            "2026-07-29T12:34:56.789Z  INFO gqy::qq: listener ready port=8090",
            false,
        );
        assert!(!rendered.contains('\x1b'));
        assert!(rendered.ends_with("[INFO] [gqy::qq] listener ready port=8090"));
        assert_eq!(
            format_daemon_log_line("判断原因：保留这一行原有的内容", true),
            "判断原因：保留这一行原有的内容"
        );
    }

    #[test]
    fn daemon_log_formatter_supports_legacy_lines_and_tty_colors() {
        let legacy = "2026-07-29T12:34:56.789Z  WARN OneBot connection closed reason=timeout";
        let parsed = parse_daemon_log_line(legacy).unwrap();
        assert_eq!(parsed.module, "gqy");
        assert_eq!(parsed.message, "OneBot connection closed reason=timeout");

        let rendered = format_daemon_log_line(legacy, true);
        assert!(rendered.contains('\x1b'));
        assert!(rendered.contains("[WARN]"));
        assert!(rendered.ends_with("OneBot connection closed reason=timeout"));
    }

    #[test]
    fn daemon_log_formatter_colors_entire_active_reply_decisions() {
        let mut formatter = DaemonLogStreamFormatter::default();
        let mut reply = Vec::new();
        formatter
            .push(
                b"2026-07-29T12:34:56.789Z  INFO gqy::qq: \xe3\x80\x90\xe7\xbb\xad\xe8\x81\x8a\xe7\xaa\x97\xe5\x8f\xa3\xe5\x88\xa4\xe6\x96\xad\xef\xbc\x9a\xe5\x9b\x9e\xe5\xa4\x8d\xe3\x80\x91\n\xe7\xbb\x93\xe6\x9e\x9c\xef\xbc\x9a\xe5\x9b\x9e\xe5\xa4\x8d\n",
                true,
                &mut reply,
            )
            .unwrap();
        let reply = String::from_utf8(reply).unwrap();
        assert!(reply.lines().all(|line| line.contains('\x1b')));

        let mut no_reply = Vec::new();
        formatter
            .push(
                b"2026-07-29T12:34:57.789Z  INFO gqy::qq: \xe3\x80\x90\xe4\xb8\xbb\xe5\x8a\xa8\xe5\x9b\x9e\xe5\xa4\x8d\xe5\x88\xa4\xe6\x96\xad\xef\xbc\x9a\xe4\xb8\x8d\xe5\x9b\x9e\xe5\xa4\x8d\xe3\x80\x91\n\xe7\xbb\x93\xe6\x9e\x9c\xef\xbc\x9a\xe4\xb8\x8d\xe5\x9b\x9e\xe5\xa4\x8d\n",
                true,
                &mut no_reply,
            )
            .unwrap();
        let no_reply = String::from_utf8(no_reply).unwrap();
        assert!(no_reply.lines().all(|line| line.contains('\x1b')));
        assert_ne!(reply, no_reply);

        let mut reset = Vec::new();
        formatter
            .push(
                b"2026-07-29T12:34:58.789Z  INFO gqy::qq: listener ready\nplain continuation\n",
                false,
                &mut reset,
            )
            .unwrap();
        let reset = String::from_utf8(reset).unwrap();
        assert!(reset.ends_with("[INFO] [gqy::qq] listener ready\nplain continuation\n"));
    }

    #[test]
    fn daemon_log_formatter_recognizes_english_active_reply_decisions() {
        assert_eq!(
            active_reply_log_color("[Active reply decision: reply]\nResult: reply"),
            Some(Color::Green)
        );
        assert_eq!(
            active_reply_log_color("[Continuation decision: no reply]\nResult: no reply"),
            Some(Color::DarkGrey)
        );

        let mut color = None;
        let timestamp = format_daemon_log_line_with_state(
            "2026-07-29T12:34:56.789Z  INFO gqy::qq: ",
            true,
            &mut color,
        );
        assert!(timestamp.contains("[INFO]"));
        assert_eq!(color, None);
        let title =
            format_daemon_log_line_with_state("[Active reply decision: reply]", true, &mut color);
        assert_eq!(color, Some(Color::Green));
        assert!(title.contains('\x1b'));
        assert!(
            format_daemon_log_line_with_state("Result: reply", true, &mut color).contains('\x1b')
        );
    }

    #[test]
    fn daemon_log_stream_formatter_waits_for_complete_lines() {
        let mut formatter = DaemonLogStreamFormatter::default();
        let mut output = Vec::new();
        formatter
            .push(
                b"2026-07-29T12:34:56.789Z  INFO gqy::qq: part",
                false,
                &mut output,
            )
            .unwrap();
        assert!(output.is_empty());

        formatter
            .push(b"ial\n  continuation\nlast", false, &mut output)
            .unwrap();
        let rendered = String::from_utf8(output).unwrap();
        assert!(rendered.contains("[INFO] [gqy::qq] partial\n"));
        assert!(rendered.ends_with("  continuation\n"));

        let mut tail = Vec::new();
        formatter.finish(false, &mut tail).unwrap();
        assert_eq!(tail, b"last\n");
    }

    #[test]
    fn recent_daemon_logs_keep_multiline_order_across_rotated_files() {
        let temp = tempfile::tempdir().unwrap();
        let paths = pop_test_paths(temp.path());
        let logs_dir = paths.logs_dir();
        std::fs::create_dir_all(&logs_dir).unwrap();
        std::fs::write(
            logs_dir.join("gqy.2026-07-28.log"),
            "2026-07-28T12:00:00Z  INFO gqy::qq: old event\n  old continuation\n",
        )
        .unwrap();
        std::fs::write(
            logs_dir.join("gqy.2026-07-29.log"),
            "2026-07-29T12:00:00Z  WARN gqy::qq: new event\n  new continuation\n判断原因：保持多行\n",
        )
        .unwrap();

        let lines = recent_daemon_log_lines(&paths, 4).unwrap();
        assert_eq!(
            lines,
            [
                "  old continuation",
                "2026-07-29T12:00:00Z  WARN gqy::qq: new event",
                "  new continuation",
                "判断原因：保持多行",
            ]
        );
        let rendered = lines
            .iter()
            .map(|line| format_daemon_log_line(line, false))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!rendered.contains('\x1b'));
        assert!(rendered.contains("[WARN] [gqy::qq] new event"));
        assert!(rendered.ends_with("  new continuation\n判断原因：保持多行"));
    }

    #[test]
    fn recent_daemon_logs_include_unstructured_daemon_stream_before_rotating_logs() {
        let temp = tempfile::tempdir().unwrap();
        let paths = pop_test_paths(temp.path());
        let logs_dir = paths.logs_dir();
        std::fs::create_dir_all(&logs_dir).unwrap();
        std::fs::write(logs_dir.join("daemon.log"), "startup banner\npanic: boom\n").unwrap();
        std::fs::write(
            logs_dir.join("gqy.2026-07-29.log"),
            "2026-07-29T12:00:00Z  INFO gqy::qq: listener ready\n",
        )
        .unwrap();

        let lines = recent_daemon_log_lines(&paths, 3).unwrap();
        assert_eq!(
            lines,
            [
                "startup banner",
                "panic: boom",
                "2026-07-29T12:00:00Z  INFO gqy::qq: listener ready",
            ]
        );
    }

    #[test]
    fn daemon_log_follow_cursor_starts_after_the_snapshot_for_each_source() {
        let temp = tempfile::tempdir().unwrap();
        let paths = pop_test_paths(temp.path());
        let logs_dir = paths.logs_dir();
        std::fs::create_dir_all(&logs_dir).unwrap();
        let fallback = logs_dir.join("daemon.log");
        let rotating = logs_dir.join("gqy.2026-07-29.log");
        std::fs::write(&fallback, b"before fallback\n").unwrap();
        std::fs::write(&rotating, b"before rotating\n").unwrap();

        let snapshot = recent_daemon_log_snapshot(&paths, 10).unwrap();
        assert_eq!(snapshot.lines, ["before fallback", "before rotating"]);
        let cursor = snapshot.cursor;
        assert_eq!(cursor.current, Some(rotating.clone()));
        assert_eq!(cursor.fallback, Some(fallback.clone()));
        assert_eq!(cursor.current_offset, 16);
        assert_eq!(cursor.fallback_offset, 16);

        let mut formatter = DaemonLogStreamFormatter::default();
        let mut output = Vec::new();
        std::fs::OpenOptions::new()
            .append(true)
            .open(&rotating)
            .unwrap()
            .write_all(b"after rotating\n")
            .unwrap();
        let mut offset = cursor.current_offset;
        assert!(
            write_daemon_log_delta(&rotating, &mut offset, &mut formatter, false, &mut output,)
                .unwrap()
        );
        formatter.finish(false, &mut output).unwrap();
        assert_eq!(String::from_utf8(output).unwrap(), "after rotating\n");
    }

    #[test]
    fn daemon_log_delta_avoids_duplicates_across_append_rotation_and_truncation() {
        fn append(path: &Path, bytes: &[u8]) {
            let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
            file.write_all(bytes).unwrap();
        }

        let temp = tempfile::tempdir().unwrap();
        let old = temp.path().join("gqy.2026-07-28.log");
        let current = temp.path().join("gqy.2026-07-29.log");
        std::fs::write(&old, b"old partial").unwrap();

        let mut formatter = DaemonLogStreamFormatter::default();
        let mut output = Vec::new();
        let mut old_offset = 0;
        assert!(
            write_daemon_log_delta(&old, &mut old_offset, &mut formatter, false, &mut output,)
                .unwrap()
        );
        assert!(output.is_empty());
        append(&old, b" completed\nold tail\n");
        assert!(
            write_daemon_log_delta(&old, &mut old_offset, &mut formatter, false, &mut output,)
                .unwrap()
        );
        formatter.finish(false, &mut output).unwrap();

        std::fs::write(
            &current,
            b"2026-07-29T12:00:00Z  INFO gqy::qq: first\n  continuation\n",
        )
        .unwrap();
        let mut offset = 0;
        assert!(
            write_daemon_log_delta(&current, &mut offset, &mut formatter, false, &mut output,)
                .unwrap()
        );
        assert!(
            !write_daemon_log_delta(&current, &mut offset, &mut formatter, false, &mut output,)
                .unwrap()
        );

        append(&current, b"2026-07-29T12:00:01Z  INFO gqy::qq: \xe7\xbe");
        assert!(
            write_daemon_log_delta(&current, &mut offset, &mut formatter, false, &mut output,)
                .unwrap()
        );
        append(&current, b"\xa4\xe8\x81\x8a\n");
        assert!(
            write_daemon_log_delta(&current, &mut offset, &mut formatter, false, &mut output,)
                .unwrap()
        );

        append(&current, b"dangling");
        assert!(
            write_daemon_log_delta(&current, &mut offset, &mut formatter, false, &mut output,)
                .unwrap()
        );
        std::fs::write(&current, b"reset\n").unwrap();
        assert!(
            write_daemon_log_delta(&current, &mut offset, &mut formatter, false, &mut output,)
                .unwrap()
        );
        assert_eq!(offset, 6);

        let rendered = String::from_utf8(output).unwrap();
        assert!(!rendered.contains('\x1b'));
        assert_eq!(rendered.matches("old partial completed").count(), 1);
        assert_eq!(rendered.matches("old tail").count(), 1);
        assert_eq!(rendered.matches("[INFO] [gqy::qq] first").count(), 1);
        assert_eq!(rendered.matches("  continuation").count(), 1);
        assert_eq!(rendered.matches("[INFO] [gqy::qq] 群聊").count(), 1);
        assert_eq!(rendered.matches("dangling").count(), 1);
        assert_eq!(rendered.matches("reset").count(), 1);
    }

    #[test]
    fn pop_is_a_cli_subcommand_with_an_optional_count() {
        let cli = parse_args(["gqy", "pop"].map(OsString::from).to_vec()).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Pop(PopArgs { count: None }))
        ));

        let cli = parse_args(["gqy", "pop", "3"].map(OsString::from).to_vec()).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Pop(PopArgs { count: Some(3) }))
        ));
        assert!(parse_args(["gqy", "pop", "0"].map(OsString::from).to_vec()).is_err());
        assert!(parse_args(["gqy", "pop", "nope"].map(OsString::from).to_vec()).is_err());
    }

    #[test]
    fn repl_pop_accepts_zero_or_one_positive_integer() {
        assert_eq!(parse_repl_pop_count("").unwrap(), None);
        assert_eq!(parse_repl_pop_count(" 3 ").unwrap(), Some(3));
        assert!(parse_repl_pop_count("0").is_err());
        assert!(parse_repl_pop_count("nope").is_err());
        assert!(parse_repl_pop_count("1 2").is_err());
    }

    #[test]
    fn counted_pop_removes_oldest_turns_and_caps_at_available_count() {
        let temp = tempfile::tempdir().unwrap();
        let paths = pop_test_paths(temp.path());
        let config = AppConfig::default();
        let state = StateStore::new(&paths).unwrap();
        for id in ["t1", "t2", "t3"] {
            state.start_turn(id, id, 999999).unwrap();
            state.complete_turn(id, "reply", None).unwrap();
        }

        let first = execute_pop(&paths, &config, &state, Some(2))
            .unwrap()
            .unwrap();
        assert_eq!(first.turns, 2);
        assert_eq!(
            state
                .load_visible_turns()
                .unwrap()
                .into_iter()
                .map(|turn| turn.turn_id)
                .collect::<Vec<_>>(),
            vec!["t3"]
        );

        let second = execute_pop(&paths, &config, &state, Some(99))
            .unwrap()
            .unwrap();
        assert_eq!(second.turns, 1);
        assert!(state.load_visible_turns().unwrap().is_empty());
    }

    #[test]
    fn pop_menu_uses_three_lines_without_context_metadata() {
        let turn = sample_pop_turn(TurnStatus::Completed);
        let lines = pop_menu_turn_lines(&turn, true, false, 80)
            .map(|line| strip_terminal_control_sequences(&line));

        assert_eq!(lines[0], "› [ ] 2026-07-19 10:42");
        assert!(lines[1].contains("first prompt line"));
        assert!(!lines[1].contains("second prompt line"));
        assert!(lines[2].contains("first answer line"));
        assert!(!lines[2].contains("second answer line"));
        let joined = lines.join(" ");
        assert!(!joined.contains("hidden tool report"));
        assert!(!joined.contains("private reasoning"));
        assert!(lines.iter().all(|line| visible_width(line) <= 80));
    }

    #[test]
    fn pop_menu_labels_an_interrupted_reply_without_showing_the_reminder() {
        let mut turn = sample_pop_turn(TurnStatus::Interrupted);
        turn.assistant_content = crate::state::interrupted_text().to_string();
        let lines = pop_menu_turn_lines(&turn, false, true, 80)
            .map(|line| strip_terminal_control_sequences(&line));

        assert!(lines[2].contains("中断") || lines[2].contains("interrupted"));
        assert!(!lines[2].contains("system-reminder"));
    }

    #[test]
    fn pop_menu_footer_has_controls_but_no_position_counter() {
        let help = strip_terminal_control_sequences(&pop_menu_help_line(120));
        assert!(help.contains("Tab"));
        assert!(help.contains("Enter"));
        assert!(!help.contains("3 / 8"));

        let header = strip_terminal_control_sequences(&pop_menu_header("", 2, 8, 80));
        assert!(header.contains("2 / 8"));
    }

    #[test]
    fn filtered_pop_turns_keep_oldest_first_order() {
        let matcher = SkimMatcherV2::default();
        let items = vec![
            "old matching prompt".to_string(),
            "middle unrelated".to_string(),
            "new matching prompt".to_string(),
        ];

        assert_eq!(pop_matches(&matcher, &items, "matching"), vec![0, 2]);
    }

    #[test]
    fn debug_is_a_global_cli_option() {
        for args in [
            &["gqy", "--debug", "models", "1"][..],
            &["gqy", "models", "--debug", "1"][..],
            &["gqy", "hello", "--debug"][..],
            &["gqy", "ask", "hello", "--debug"][..],
        ] {
            let cli = parse_args(args.iter().map(OsString::from).collect()).unwrap();
            assert!(cli.debug);
        }

        let cli = parse_args(["gqy", "hello", "--debug"].map(OsString::from).to_vec()).unwrap();
        assert_eq!(cli.message, ["hello"]);

        let cli = parse_args(["gqy", "--", "--debug"].map(OsString::from).to_vec()).unwrap();
        assert!(!cli.debug);
        assert_eq!(cli.message, ["--debug"]);
    }

    #[test]
    fn footer_reset_clears_turn_and_cumulative_tokens() {
        let config = AppConfig::default();
        let mut footer = ReplFooterStatus::from_config(
            &config,
            100,
            TurnTokens {
                total: 250,
                ..Default::default()
            },
        );
        footer.set_token_usage(
            50,
            100,
            Some(200_000),
            TurnTokens {
                total: 250,
                ..Default::default()
            },
        );

        footer.reset_token_usage(0, Some(200_000));

        assert_eq!(footer.token_usage.turn_tokens, 0);
        assert_eq!(footer.token_usage.session_tokens, 0);
        assert_eq!(footer.token_usage.context_window, Some(200_000));
        assert_eq!(footer.token_usage.cumulative_tokens, None);

        footer.reset_token_usage(0, None);
        assert_eq!(footer.token_usage.context_window, None);
    }

    #[test]
    fn footer_turn_completion_updates_the_rendered_token_accounting() {
        let config = AppConfig::default();
        let mut footer = ReplFooterStatus::from_config(&config, 0, TurnTokens::default());
        let result = ChatResult {
            content: "reply".to_string(),
            reasoning: None,
            usage: Some(Usage {
                prompt_tokens: 80,
                completion_tokens: 20,
                total_tokens: 100,
                ..Usage::default()
            }),
            usage_estimated: false,
            tool_calls: Vec::new(),
            provider_id: None,
            model: None,
            finish_reason: None,
            thinking_signature: None,
            last_request_usage: None,
            responses_continuation: None,
        };

        footer.update_token_usage(
            &result,
            240,
            Some(200_000),
            TurnTokens {
                total: 100,
                ..Default::default()
            },
        );

        assert_eq!(footer.token_usage.turn_tokens, 100);
        assert_eq!(footer.token_usage.session_tokens, 240);
        assert_eq!(footer.token_usage.cumulative_tokens, Some(100));
        assert_eq!(
            strip_terminal_control_sequences(&repl_footer_line(AgentMode::Normal, &footer, 80))
                .split_whitespace()
                .last(),
            Some("Σ100")
        );
    }

    #[test]
    fn an_idle_tick_only_redraws_when_the_cumulative_actually_moved() {
        let config = AppConfig::default();
        let mut footer = ReplFooterStatus::from_config(&config, 0, TurnTokens::default());
        let totals = TurnTokens {
            total: 32_808,
            prompt: 29_035,
            cache_read: 17_664,
        };
        assert!(footer.update_cumulative_tokens(totals));
        // The jobs poll republishes the same Σ every second; redrawing the
        // whole tail on each of those would fight the strip animation.
        assert!(!footer.update_cumulative_tokens(totals));

        // A background subagent finishing moves only the cache halves — the
        // total can stay put when its usage was estimated, so equality has to
        // consider all three.
        assert!(footer.update_cumulative_tokens(TurnTokens {
            cache_read: 20_000,
            ..totals
        }));
    }

    #[test]
    fn the_footer_leaves_the_per_turn_figure_to_the_token_line() {
        let config = AppConfig::default();
        let mut footer = ReplFooterStatus::from_config(&config, 0, TurnTokens::default());
        footer.set_token_usage_with_cache(
            TurnTokens {
                total: 21_224,
                prompt: 16_139,
                cache_read: 6_528,
            },
            21_700,
            Some(1_000_000),
            TurnTokens {
                total: 180_100,
                prompt: 47_538,
                cache_read: 11_392,
            },
        );

        let line =
            strip_terminal_control_sequences(&repl_footer_line(AgentMode::Normal, &footer, 80));
        // Two standing gauges only. Carrying the turn figure as well cost 14
        // columns and pushed the whole footer past 80.
        assert!(line.contains("21.7k/1M(2.2%)"), "{line}");
        assert!(line.contains("Σ180.1k(C24%)"), "{line}");
        assert!(!line.contains("21.2k"), "{line}");
        assert!(!line.contains("C40%"), "{line}");
        assert!(
            visible_width(&line) <= 80,
            "footer must fit 80 columns: {} — {line}",
            visible_width(&line)
        );
    }

    #[test]
    fn footer_variant_always_uses_the_fixed_primary_color() {
        let config = AppConfig::default();
        let mut footer = ReplFooterStatus::from_config(&config, 0, TurnTokens::default());
        footer.update_thinking_variant(Some("high"));

        for mode in [AgentMode::Normal, AgentMode::Dev] {
            let line = repl_footer_left(mode, &footer, 120);
            assert!(line.contains("\x1b[1m\x1b[34mhigh\x1b[0m"));
            assert_eq!(
                strip_terminal_control_sequences(&line),
                format!(
                    "{} · {} {} · high",
                    mode.label(),
                    footer.model,
                    footer.provider
                )
            );
        }
    }

    #[test]
    fn mixed_footer_uses_dim_provider_and_hides_global_variant() {
        let mut config = AppConfig::default();
        let provider = config
            .providers
            .iter_mut()
            .find(|provider| !provider.models.is_empty())
            .unwrap();
        let provider_id = provider.id.clone();
        let first_model = provider.models[0].clone();
        let second_model = "footer-second-model".to_string();
        provider.models.push(second_model.clone());
        config.active_provider_models = Some(vec![
            ActiveProviderModelConfig {
                provider_id: provider_id.clone(),
                model: first_model,
            },
            ActiveProviderModelConfig {
                provider_id,
                model: second_model,
            },
        ]);
        let mut footer = ReplFooterStatus::from_config(&config, 0, TurnTokens::default());
        footer.update_thinking_variant(Some("mixed"));

        let line = repl_footer_left(AgentMode::Normal, &footer, 120);

        assert_eq!(footer.provider, "mixed");
        assert!(footer.thinking.is_none());
        assert_eq!(
            strip_terminal_control_sequences(&line),
            format!(
                "{} · {} mixed",
                AgentMode::Normal.label(),
                t("Mixed", "混合")
            )
        );
        assert!(line.contains("\x1b[2mmixed\x1b[0m"));
        assert!(!line.contains(&primary_footer_text("mixed")));
    }

    #[test]
    fn committed_user_message_keeps_one_blank_line_before_output() {
        let output = committed_user_messages_text(&[("hello", AgentMode::Normal)], true, 80);

        assert_eq!(
            strip_terminal_control_sequences(&output),
            "\n┃\n┃ hello\n┃\n\n"
        );
    }

    #[test]
    fn queued_message_uses_full_height_bar_and_primary_status() {
        let prompt = QueuedPrompt {
            prompt_id: "q1".to_string(),
            seq: 1,
            content: "follow up".to_string(),
            display_content: "follow up".to_string(),
            attachments: Vec::new(),
            uploaded_attachments: Vec::new(),
            submitted_at: String::new(),
        };

        let normal = queued_prompt_lines(std::slice::from_ref(&prompt), AgentMode::Normal, 80);
        let chat = queued_prompt_lines(&[prompt], AgentMode::Dev, 80);

        assert_eq!(normal.len(), 4);
        assert_eq!(normal[0], submitted_echo_bar(AgentMode::Normal));
        assert_eq!(normal[2], submitted_echo_bar(AgentMode::Normal));
        assert!(normal[3].starts_with(&submitted_echo_bar(AgentMode::Normal)));
        assert!(normal[3].contains(&primary_footer_text(t("Queued", "排队中"))));
        assert!(chat
            .iter()
            .filter(|line| !line.is_empty())
            .all(|line| line.starts_with(&submitted_echo_bar(AgentMode::Dev))));
        assert_ne!(normal[0], chat[0]);
    }

    #[test]
    fn live_tail_moves_naturally_and_releases_after_output_shrinks() {
        assert_eq!(max_live_tail_start(6, 5), 0);
        assert_eq!(max_live_tail_start(24, 5), 18);
        assert_eq!(
            live_tail_placement(0, 4, 5, 24, false),
            LiveTailPlacement {
                output_row: 4,
                tail_start: 4,
                overflow: 0,
                anchored: false,
            }
        );
        assert_eq!(
            live_tail_placement(0, 20, 5, 24, false),
            LiveTailPlacement {
                output_row: 18,
                tail_start: 18,
                overflow: 2,
                anchored: true,
            }
        );
        assert_eq!(
            live_tail_placement(0, 6, 5, 24, false),
            LiveTailPlacement {
                output_row: 6,
                tail_start: 6,
                overflow: 0,
                anchored: false,
            }
        );
        assert_eq!(live_tail_placement(0, 6, 5, 30, false).tail_start, 6);
    }

    #[test]
    fn anchored_tail_stays_at_the_bottom_when_it_shrinks() {
        // A job strip pushed a bottom-anchored 5-row tail to 7 rows, scrolling
        // the screen twice, so output now ends at row 16. The strip goes away.
        let shrunk = live_tail_placement(0, 16, 5, 24, true);
        assert_eq!(
            shrunk,
            LiveTailPlacement {
                // Stays where the output really ended: the renderer's spinner
                // erases itself relative to this cursor.
                output_row: 16,
                tail_start: 18,
                overflow: 0,
                anchored: true,
            }
        );
        // Bottom edge back on the last usable row, where it was before.
        assert_eq!(shrunk.tail_start + 5, 24 - 1);

        // Without the anchor the tail hugs the output cursor as before: a
        // conversation that has not filled the screen is untouched.
        assert_eq!(
            live_tail_placement(0, 16, 5, 24, false),
            LiveTailPlacement {
                output_row: 16,
                tail_start: 16,
                overflow: 0,
                anchored: false,
            }
        );

        // Growing while anchored still scrolls rather than double-counting.
        assert_eq!(
            live_tail_placement(0, 18, 7, 24, true),
            LiveTailPlacement {
                output_row: 16,
                tail_start: 16,
                overflow: 2,
                anchored: true,
            }
        );
    }

    #[test]
    fn streaming_output_never_drags_an_anchored_tail_back_up() {
        // 24 rows, 5-row tail → anchored at 18. Output ends two rows above it
        // because a job strip just went away; the frame must leave the tail
        // alone and fill the gap instead of reclaiming those rows.
        let max_tail = max_live_tail_start(24, 5);
        assert_eq!(max_tail, 18);
        assert_eq!(live_tail_next_start(18, 16, max_tail), 18);
        // Still pinned once the gap is closed.
        assert_eq!(live_tail_next_start(18, 18, max_tail), 18);
        // And it never runs past the anchor.
        assert_eq!(live_tail_next_start(18, 21, max_tail), 18);

        // A tail that had not reached the bottom keeps following the output.
        assert_eq!(live_tail_next_start(10, 12, max_tail), 12);
        assert_eq!(live_tail_next_start(10, 8, max_tail), 8);
        assert_eq!(live_tail_next_start(10, 30, max_tail), 18);
    }

    #[test]
    fn live_editor_restores_clear_screen_and_double_escape_controls() {
        let temp = tempfile::tempdir().unwrap();
        let paths = pop_test_paths(temp.path());
        let escape = || Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        let mut editor = LiveReplEditor::new(AgentMode::Normal, Vec::new());
        editor.input = "draft".to_string();
        assert!(matches!(
            editor.handle_event(escape(), &paths, true).unwrap(),
            LiveEditorAction::Redraw
        ));
        // Arming the interrupt must not clear the typed draft.
        assert_eq!(editor.input, "draft");
        assert!(matches!(
            editor.handle_event(escape(), &paths, true).unwrap(),
            LiveEditorAction::Interrupt
        ));
        assert_eq!(editor.input, "draft");

        let clear = Event::Key(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL));
        assert!(matches!(
            editor.handle_event(clear, &paths, true).unwrap(),
            LiveEditorAction::ClearScreen
        ));

        assert!(matches!(
            editor.handle_event(escape(), &paths, false).unwrap(),
            LiveEditorAction::Redraw
        ));
        assert!(matches!(
            editor.handle_event(escape(), &paths, false).unwrap(),
            LiveEditorAction::Redraw
        ));
        // Esc no longer clears drafts anywhere; empty the editor manually
        // before asserting the empty-submit path.
        assert_eq!(editor.input, "draft");
        editor.clear();

        assert!(matches!(
            editor
                .handle_event(
                    Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
                    &paths,
                    false,
                )
                .unwrap(),
            LiveEditorAction::EmptySubmit
        ));
        assert!(editor.history.is_empty());

        editor.input = "/help".to_string();
        editor.cursor = editor.input.chars().count();
        assert!(matches!(
            editor
                .handle_event(
                    Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
                    &paths,
                    false,
                )
                .unwrap(),
            LiveEditorAction::Submit(_)
        ));
        assert!(editor.history.is_empty());
        editor.record_history("ordinary prompt");
        assert_eq!(editor.history, ["ordinary prompt"]);
    }

    #[test]
    fn live_editor_shift_enter_inserts_newline_without_submit() {
        let temp = tempfile::tempdir().unwrap();
        let paths = pop_test_paths(temp.path());
        let mut editor = LiveReplEditor::new(AgentMode::Normal, Vec::new());
        editor.input = "hello".to_string();
        editor.cursor = 5;
        assert!(matches!(
            editor
                .handle_event(
                    Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT)),
                    &paths,
                    false,
                )
                .unwrap(),
            LiveEditorAction::Redraw
        ));
        assert_eq!(editor.input, "hello\n");
        assert_eq!(editor.cursor, 6);

        assert!(matches!(
            editor
                .handle_event(
                    Event::Key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL)),
                    &paths,
                    false,
                )
                .unwrap(),
            LiveEditorAction::Redraw
        ));
        assert_eq!(editor.input, "hello\n\n");
        assert_eq!(editor.cursor, 7);
    }

    #[test]
    fn spinner_does_not_resume_tail_during_external_output() {
        let config = AppConfig::default();
        let mut live = LiveReplTail {
            editor: LiveReplEditor::new(AgentMode::Normal, Vec::new()),
            queued: Vec::new(),
            pending_chunks: Vec::new(),
            footer: ReplFooterStatus::from_config(&config, 0, TurnTokens::default()),
            round_base_footer: None,
            output_cursor: (0, 0),
            tail_start: 0,
            tail_rows: 0,
            input_cursor: (0, 0),
            rendered: false,
            external_output_active: true,
            raw_mode_handoff: false,
            jobs: Vec::new(),
            job_spinner: 0,
        };
        let mut renderer = render::StreamRenderer::new(
            render::ReasoningDisplayMode::Hidden,
            render::ToolCallDisplayMode::Hidden,
            true,
            true,
            10,
        );

        handle_live_agent_event(&mut live, &mut renderer, AgentEvent::SpinnerTick).unwrap();

        assert!(live.external_output_active);
        assert!(!live.rendered);
    }

    #[test]
    fn live_tail_coalesces_adjacent_stream_chunks_and_can_discard_them() {
        let config = AppConfig::default();
        let mut live = LiveReplTail {
            editor: LiveReplEditor::new(AgentMode::Normal, Vec::new()),
            queued: Vec::new(),
            pending_chunks: Vec::new(),
            footer: ReplFooterStatus::from_config(&config, 0, TurnTokens::default()),
            round_base_footer: None,
            output_cursor: (0, 0),
            tail_start: 0,
            tail_rows: 0,
            input_cursor: (0, 0),
            rendered: false,
            external_output_active: false,
            raw_mode_handoff: false,
            jobs: Vec::new(),
            job_spinner: 0,
        };

        for (kind, text) in [
            (ChatStreamKind::Reasoning, "one"),
            (ChatStreamKind::Reasoning, " two"),
            (ChatStreamKind::Content, "answer"),
            (ChatStreamKind::Content, " text"),
        ] {
            live.queue_stream_chunk(ChatStreamChunk {
                kind,
                text: text.to_string(),
            });
        }

        assert_eq!(live.pending_chunks.len(), 2);
        assert_eq!(live.pending_chunks[0].text, "one two");
        assert_eq!(live.pending_chunks[1].text, "answer text");
        live.discard_pending_chunks();
        assert!(live.pending_chunks.is_empty());
    }

    #[test]
    fn prompt_rows_wrap_at_terminal_width() {
        assert_eq!(repl_prompt_rows_for_cols("", &["1234567".into()], 10), 1);
        assert_eq!(repl_prompt_rows_for_cols("", &["1234567890".into()], 10), 2);
        assert_eq!(
            repl_prompt_rows_for_cols("", &["123".into(), "456".into()], 10),
            2
        );
    }

    #[test]
    fn cursor_position_wraps_at_terminal_width() {
        assert_eq!(repl_cursor_position_for_cols("", "1234567", 7, 10), (7, 0));
        assert_eq!(
            repl_cursor_position_for_cols("", "1234567890", 10, 10),
            (0, 1)
        );
        assert_eq!(repl_cursor_position_for_cols("", "123\n456", 7, 10), (3, 1));
        assert_eq!(repl_cursor_position_for_cols("", "1234567", 3, 10), (3, 0));
    }

    #[test]
    fn cursor_position_keeps_prefix_after_newline() {
        assert_eq!(repl_cursor_position_for_cols("  ", "123\n", 4, 10), (2, 1));
        assert_eq!(
            repl_cursor_position_for_cols("  ", "123\n456", 7, 10),
            (5, 1)
        );
    }

    #[test]
    fn prompt_rows_include_prefix_on_each_line() {
        assert_eq!(
            repl_prompt_rows_for_cols("  ", &["12".into(), "34".into()], 5),
            2
        );
        assert_eq!(
            repl_prompt_rows_for_cols("  ", &["123".into(), "34".into()], 5),
            3
        );
    }

    #[test]
    fn wrapped_input_rows_keep_prefix_outside_content_width() {
        assert_eq!(
            repl_wrapped_input_rows_for_cols("  ", &["123456789".into()], 10),
            vec!["12345678".to_string(), "9".to_string()]
        );
        assert_eq!(
            repl_wrapped_input_rows_for_cols("  ", &["12345678".into()], 10),
            vec!["12345678".to_string(), String::new()]
        );
        assert_eq!(
            repl_cursor_position_for_cols("  ", "12345678", 8, 10),
            (2, 1)
        );
    }

    #[test]
    fn history_browsing_requires_empty_or_clean_history_input() {
        let history = vec!["first".to_string(), "second".to_string()];

        assert!(repl_should_browse_history("", &history, None));
        assert!(repl_should_browse_history("second", &history, Some(1)));
        assert!(!repl_should_browse_history("draft", &history, None));
        assert!(!repl_should_browse_history(
            "second edited",
            &history,
            Some(1)
        ));
    }

    #[test]
    fn vertical_cursor_move_uses_soft_wrapped_rows() {
        assert_eq!(
            repl_move_cursor_vertical_for_cols("  ", "123456789", 9, -1, 10),
            1
        );
        assert_eq!(
            repl_move_cursor_vertical_for_cols("  ", "123456789", 1, 1, 10),
            9
        );
    }

    #[test]
    fn vertical_cursor_move_handles_explicit_newlines() {
        assert_eq!(
            repl_move_cursor_vertical_for_cols("  ", "abc\ndef", 6, -1, 20),
            2
        );
        assert_eq!(
            repl_move_cursor_vertical_for_cols("  ", "abc\ndef", 2, 1, 20),
            6
        );
    }

    #[test]
    fn vertical_cursor_move_handles_wide_chars_near_wrap() {
        assert_eq!(
            repl_cursor_position_for_cols("  ", "1234567你", 8, 11),
            (2, 1)
        );
        assert_eq!(
            repl_cursor_position_for_cols("  ", "12345678你", 9, 11),
            (4, 1)
        );
        assert_eq!(
            repl_move_cursor_vertical_for_cols("  ", "12345678你好", 9, -1, 11),
            2
        );
    }

    #[test]
    fn reset_is_a_repl_command() {
        assert!(repl_commands().contains(&"/reset"));
    }

    #[test]
    fn compact_is_a_repl_command() {
        assert!(repl_commands().contains(&"/compact"));
    }

    #[test]
    fn pop_is_a_repl_command_with_an_optional_count() {
        assert!(repl_commands().contains(&"/pop"));
        assert_eq!(split_repl_command("/pop 3"), ("/pop", "3"));
        // "/p" became ambiguous once /persona joined the table.
        assert_eq!(resolve_repl_command("/po"), "/pop");
        assert_eq!(resolve_repl_command("/pe"), "/persona");
    }

    #[test]
    fn usage_and_persona_are_repl_commands() {
        assert!(repl_commands().contains(&"/usage"));
        assert!(repl_commands().contains(&"/persona"));
        assert_eq!(resolve_repl_command("/us"), "/usage");
        assert_eq!(split_repl_command("/persona Alice.md"), ("/persona", "Alice.md"));
    }

    #[test]
    fn variant_is_a_repl_command_with_arguments() {
        assert!(repl_commands().contains(&"/variant"));
        assert_eq!(split_repl_command("/variant high"), ("/variant", "high"));
        assert_eq!(split_repl_command("/reset all"), ("/reset", "all"));
        assert_eq!(resolve_repl_command("/var"), "/variant");
    }

    #[test]
    fn variant_menu_checks_pending_selection_before_confirming() {
        let options = ThinkingVariantOptions {
            provider_id: "ririxin".to_string(),
            model: "deepseek-v4-flash".to_string(),
            variants: vec!["high".to_string(), "max".to_string()],
            selected: Some("high".to_string()),
        };
        let mut item = VariantMenuItem::from_options(&options);
        assert_eq!(
            item.options
                .iter()
                .map(|option| option.label.as_str())
                .collect::<Vec<_>>(),
            vec!["default", "high", "max"]
        );
        assert_eq!(item.selection().2.as_deref(), Some("high"));

        item.cursor = 2;
        assert_eq!(item.selection().2.as_deref(), Some("high"));
        item.check_cursor();
        assert_eq!(item.selection().2.as_deref(), Some("max"));
    }

    #[test]
    fn single_variant_menu_uses_content_width() {
        let item = VariantMenuItem::from_options(&ThinkingVariantOptions {
            provider_id: "ririxin".to_string(),
            model: "deepseek-v4-flash".to_string(),
            variants: vec!["high".to_string(), "max".to_string()],
            selected: None,
        });

        assert!(single_variant_content_width(&item) < 30);
    }

    #[test]
    fn mixed_variant_columns_do_not_fill_wide_terminal() {
        let items = ["myopencode", "myopencode6"]
            .into_iter()
            .map(|provider_id| {
                VariantMenuItem::from_options(&ThinkingVariantOptions {
                    provider_id: provider_id.to_string(),
                    model: "deepseek-v4-flash-free".to_string(),
                    variants: vec!["high".to_string(), "max".to_string()],
                    selected: None,
                })
            })
            .collect::<Vec<_>>();

        let (left, right) = variant_menu_column_widths(&items, 120);
        assert!(left + right < 80);
        assert!(left >= visible_width("myopencode6 / deepseek-v4-flash-free") + 2);
        assert!(right >= visible_width("[*] default") + 2);
    }

    #[test]
    fn mixed_endpoint_label_only_omits_unset_variant() {
        assert_eq!(
            mixed_model_endpoint_label("provider", "model", None),
            "provider / model"
        );
        assert_eq!(
            mixed_model_endpoint_label("provider", "model", Some("default")),
            "provider / model · default"
        );
        assert_eq!(
            mixed_model_endpoint_label("provider", "model", Some("high")),
            "provider / model · high"
        );
    }

    #[test]
    fn variant_menu_distinguishes_unset_from_default_effort() {
        let options = ThinkingVariantOptions {
            provider_id: "groq".to_string(),
            model: "qwen/qwen3-32b".to_string(),
            variants: vec!["none".to_string(), "default".to_string()],
            selected: Some("default".to_string()),
        };
        let item = VariantMenuItem::from_options(&options);

        assert_eq!(item.options[0].label, "default");
        assert_eq!(item.options[0].value, None);
        assert_eq!(item.options[2].label, "default (variant)");
        assert_eq!(item.options[2].value.as_deref(), Some("default"));
        assert_eq!(item.selected, 2);
        assert_eq!(item.selection().2.as_deref(), Some("default"));
    }

    #[test]
    fn explicit_variant_prefix_can_select_default_effort() {
        let argument = "variant:default";
        assert_eq!(argument.strip_prefix("variant:"), Some("default"));
        assert_ne!(argument, "default");
    }

    #[test]
    fn variant_name_resolution_handles_default_and_case_insensitive_names() {
        let available = vec!["low".to_string(), "high".to_string(), "default".to_string()];

        assert_eq!(
            resolve_variant_name("HIGH", &available).unwrap(),
            Some("high".into())
        );
        assert_eq!(resolve_variant_name("default", &available).unwrap(), None);
        assert_eq!(
            resolve_variant_name("variant:default", &available).unwrap(),
            Some("default".into())
        );
        assert!(resolve_variant_name("unknown", &available).is_err());
        assert!(resolve_variant_name("Variant:default", &available).is_err());
    }

    #[test]
    fn command_suggestions_are_prefixed_and_truncated() {
        let suggestions = repl_command_suggestions("/");
        let line = repl_command_suggestions_line(&suggestions, 24);
        assert!(line.starts_with("/new"));
        assert!(visible_width(&line) <= 24);

        let line = repl_command_suggestions_line(&["/compact"], 40);
        assert_eq!(line, "/compact");
    }

    #[test]
    fn truncation_respects_very_narrow_widths() {
        assert_eq!(truncate_visible_width("abcdef", 0), "");
        assert_eq!(truncate_visible_width("abcdef", 1), ".");
        assert_eq!(truncate_visible_width("abcdef", 2), "..");
        assert_eq!(truncate_visible_width("abcdef", 3), "...");
    }

    #[test]
    fn shortcut_hint_line_is_bar_aligned_and_truncated() {
        // Tab 切换模式已随闲聊模式删除,提示行首个词条现在是换行快捷键。
        let line = repl_shortcut_hint_line(AgentMode::Normal, 24);
        assert!(strip_terminal_control_sequences(&line).contains("Shift+Enter"));
        assert!(visible_width(&line) <= 24);
    }

    #[test]
    fn inline_fuzzy_lines_are_bar_aligned_and_truncated() {
        let header = inline_fuzzy_header("big", 12);
        assert!(strip_terminal_control_sequences(&header).contains(t("Select", "选择模型")));
        assert!(visible_width(&header) <= 12);

        let item = inline_fuzzy_item_line("opencode Zen / big-pickle", true, false, 16);
        let item_plain = strip_terminal_control_sequences(&item);
        assert!(item_plain.starts_with("› [ ]"));
        assert!(item_plain.contains("open"));
        assert!(visible_width(&item) <= 16);

        let item = inline_fuzzy_item_line("opencode Zen / big-pickle", false, true, 18);
        let item_plain = strip_terminal_control_sequences(&item);
        assert!(item_plain.starts_with("  [*]"));
        assert!(item_plain.contains("opencode"));
        assert!(visible_width(&item) <= 18);

        let help = inline_fuzzy_help_line(40);
        let help_plain = strip_terminal_control_sequences(&help);
        assert!(help_plain.contains("j/k"));
        assert!(visible_width(&help) <= 40);
    }

    #[test]
    fn session_selection_defaults_to_the_current_entry() {
        let entry = |id: &str, is_current: bool| SessionListEntry {
            id: id.to_string(),
            name: id.to_string(),
            is_current,
            turns: 0,
            snippet: String::new(),
            workspace: None,
            mode: "normal".to_string(),
        };
        let entries = vec![entry("default", true), entry("active", false)];

        assert_eq!(session_initial_selection(&entries, Some("active")), 1);
        assert_eq!(session_initial_selection(&entries, None), 0);
        assert!(matches!(
            session_ref_from_index(&entries, 2),
            Some(crate::ipc::SessionRef::Id { id }) if id == "active"
        ));
        assert_eq!(session_initial_selection(&[entry("only", false)], None), 0);
    }

    #[test]
    fn wipe_is_its_own_command_not_a_suffix_on_reset() {
        // `/reset` and `/reset all` differed by one word and by everything
        // else: one starts a conversation over, the other erased memory, every
        // session and the generated skills. They answer under separate names
        // now, and `/wipe` is far enough from `/w…` prefixes to be typed on
        // purpose.
        assert!(matches!(
            parse_repl_input("/wipe"),
            ReplInput::Slash(ReplSlashCommand::Wipe, "")
        ));
        assert!(matches!(
            parse_repl_input("/reset"),
            ReplInput::Slash(ReplSlashCommand::Reset, "")
        ));
        assert!(matches!(
            parse_repl_input("/reset all"),
            ReplInput::Slash(ReplSlashCommand::Reset, "all")
        ));
    }

    #[test]
    fn partial_slash_command_resolves_unique_match() {
        assert_eq!(resolve_repl_command("/model"), "/models");
        assert_eq!(resolve_repl_command("/compa"), "/compact");
        assert_eq!(resolve_repl_command("/co"), "/co");
        assert_eq!(resolve_repl_command("hello"), "hello");
    }

    #[test]
    fn parse_repl_input_dispatches_by_table() {
        assert!(matches!(parse_repl_input("hello"), ReplInput::Chat));
        assert!(matches!(
            parse_repl_input("/models"),
            ReplInput::Slash(ReplSlashCommand::Models, "")
        ));
        // Unique prefix resolves.
        assert!(matches!(
            parse_repl_input("/compa"),
            ReplInput::Slash(ReplSlashCommand::Compact, "")
        ));
        // Exact match wins over ambiguous prefixes of longer names.
        assert!(matches!(
            parse_repl_input("/reset all"),
            ReplInput::Slash(ReplSlashCommand::Reset, "all")
        ));
        // Case-insensitive.
        assert!(matches!(
            parse_repl_input("/POP 3"),
            ReplInput::Slash(ReplSlashCommand::Pop, "3")
        ));
        // Ambiguous prefix stays unknown.
        assert!(matches!(
            parse_repl_input("/co"),
            ReplInput::UnknownSlash("/co")
        ));
        assert!(matches!(
            parse_repl_input("/nope"),
            ReplInput::UnknownSlash("/nope")
        ));
    }

    #[test]
    fn every_repl_slash_command_has_a_table_entry() {
        // repl_command_spec panics on a missing entry; touch every variant.
        for spec in REPL_COMMAND_TABLE {
            assert_eq!(repl_command_spec(spec.command).command, spec.command);
        }
    }

    #[test]
    fn drain_stdin_does_not_panic() {
        drain_stdin();
    }

    #[test]
    fn input_helpers_edit_at_cursor() {
        let mut input = "abcd".to_string();
        let mut cursor = 2;
        insert_char_at_cursor(&mut input, &mut cursor, '中');
        assert_eq!(input, "ab中cd");
        assert_eq!(cursor, 3);

        remove_char_before_cursor(&mut input, &mut cursor);
        assert_eq!(input, "abcd");
        assert_eq!(cursor, 2);

        remove_char_at_cursor(&mut input, cursor);
        assert_eq!(input, "abd");
        assert_eq!(cursor, 2);
    }

    #[test]
    fn input_helpers_remove_word_before_cursor() {
        let mut input = "hello world  ".to_string();
        let mut cursor = input.chars().count();
        remove_word_before_cursor(&mut input, &mut cursor);
        assert_eq!(input, "hello ");
        assert_eq!(cursor, 6);

        let mut input = "前面 中间 后面".to_string();
        let mut cursor = 6;
        remove_word_before_cursor(&mut input, &mut cursor);
        assert_eq!(input, "前面 后面");
        assert_eq!(cursor, 3);
    }

    #[test]
    fn input_helpers_insert_paste_at_cursor() {
        let mut input = "前后".to_string();
        let mut cursor = 1;
        insert_str_at_cursor(&mut input, &mut cursor, "中间");
        assert_eq!(input, "前中间后");
        assert_eq!(cursor, 3);
    }

    #[test]
    fn input_helpers_insert_newline_at_cursor() {
        let mut input = "前后".to_string();
        let mut cursor = 1;
        insert_newline_at_cursor(&mut input, &mut cursor);
        assert_eq!(input, "前\n后");
        assert_eq!(cursor, 2);
    }

    #[test]
    fn long_paste_visible_lines_are_collapsed() {
        let lines = (0..20)
            .map(|index| format!("line {index}"))
            .collect::<Vec<_>>();
        let visible = repl_visible_input_lines("[NORMAL] > ", &lines, 12, true);

        assert_eq!(visible.len(), 3);
        assert_eq!(visible[0], "line 0");
        assert!(visible[1].contains("18") || visible[1].contains("已隐藏 18"));
        assert_eq!(visible[2], "line 19");
        assert_eq!(lines.len(), 20);
    }

    #[test]
    fn long_paste_is_replaced_with_placeholder_and_expanded() {
        let text = "alpha\nbeta\ngamma".to_string();
        let placeholder = pasted_text_placeholder(1, pasted_text_line_count(&text));
        let input = format!("请分析 {placeholder}谢谢");
        let pasted_texts = vec![Some(PastedText { text: text.clone() })];

        assert!(should_summarize_pasted_text(&text));
        assert_eq!(
            expand_pasted_text_placeholders(&input, &pasted_texts),
            "请分析 alpha\nbeta\ngamma谢谢"
        );
    }

    #[test]
    fn short_paste_is_not_summarized() {
        assert!(!should_summarize_pasted_text("short paste"));
    }

    #[test]
    fn insert_pasted_text_summarizes_long_clipboard_text() {
        let mut input = "前后".to_string();
        let mut cursor = 1;
        let mut pasted_texts = Vec::new();

        insert_pasted_text_at_cursor(
            &mut input,
            &mut cursor,
            "alpha\nbeta\ngamma".to_string(),
            &mut pasted_texts,
        );

        assert!(
            input == "前[Pasted 1: ~3 lines]后" || input == "前[粘贴 1: ~3 行]后",
            "unexpected localized placeholder: {input}"
        );
        assert_eq!(pasted_texts.len(), 1);
        assert_eq!(cursor, input.chars().count() - 1);
    }

    #[test]
    fn pasted_placeholder_is_treated_as_atomic_token() {
        let input = "前[Pasted 1: ~3 lines] 后";
        assert_eq!(placeholder_at_cursor(input, 3), Some((1, 21)));
        assert_eq!(placeholder_before_cursor(input, 21), Some((1, 21)));
        assert_eq!(placeholder_after_cursor(input, 1), Some((1, 21)));
        assert_eq!(placeholder_before_or_at_cursor(input, 3), Some((1, 21)));
        assert_eq!(placeholder_after_or_at_cursor(input, 3), Some((1, 21)));
    }

    #[test]
    fn chinese_pasted_placeholder_is_supported() {
        let input = "前[粘贴 1: ~3 行] 后";
        let placeholder = find_pasted_text_placeholders(input);

        assert_eq!(placeholder, vec![(1, 13, 1)]);
        assert_eq!(placeholder_at_cursor(input, 3), Some((1, 13)));
        assert_eq!(placeholder_before_cursor(input, 13), Some((1, 13)));
        assert_eq!(placeholder_after_cursor(input, 1), Some((1, 13)));
    }

    #[test]
    fn colorizes_image_and_pasted_placeholders() {
        let colored = colorize_repl_placeholders("[Image 1] [Pasted 1: ~3 lines]");
        assert!(colored.contains("\x1b[35m[Image 1]\x1b[0m"));
        assert!(colored.contains("\x1b[35m[Pasted 1: ~3 lines]\x1b[0m"));
    }

    #[test]
    fn placeholder_text_near_cursor_expands_pasted_placeholder() {
        let input = "前[Pasted 1: ~3 lines]后";
        let pasted_texts = vec![Some(PastedText {
            text: "alpha\nbeta\ngamma".to_string(),
        })];

        assert_eq!(
            placeholder_text_near_cursor(input, 3, &pasted_texts),
            Some("alpha\nbeta\ngamma".to_string())
        );
    }

    #[test]
    fn strips_terminal_control_sequences_from_repl_text() {
        assert_eq!(
            strip_terminal_control_sequences("\x1b[E表情包\x1b[0m\x07 ok"),
            "表情包 ok"
        );
        assert_eq!(
            strip_terminal_control_sequences("line1\nline2\tend"),
            "line1\nline2\tend"
        );
    }

    #[test]
    fn repl_history_loads_user_messages_from_state() {
        let temp = tempfile::tempdir().unwrap();
        let paths = GQYPaths {
            root_dir: PathBuf::new(),
            config_dir: PathBuf::new(),
            config_file: PathBuf::new(),
            skills_dir: PathBuf::new(),
            data_dir: PathBuf::new(),
            cache_dir: PathBuf::new(),
            state_dir: temp.path().to_path_buf(),
            pictures_dir: PathBuf::new(),
            fish_hook_file: PathBuf::new(),
            bash_hook_file: PathBuf::new(),
            zsh_hook_file: PathBuf::new(),
            scripts_dir: PathBuf::new(),
            system_scripts_dir: PathBuf::new(),
        };
        let state = StateStore::new(&paths).unwrap();
        state.start_turn("turn_1", "first", 999999).unwrap();
        state.complete_turn("turn_1", "reply", None).unwrap();
        state.start_turn("turn_2", "second", 999999).unwrap();

        assert_eq!(
            load_repl_input_history(&state, &paths).unwrap(),
            vec!["first".to_string(), "second".to_string()]
        );
    }
}

fn run_history(paths: &GQYPaths, args: HistoryArgs) -> Result<()> {
    let state = StateStore::new(paths)?;
    run_history_with_state(&state, args)
}

fn run_history_with_state(state: &StateStore, args: HistoryArgs) -> Result<()> {
    for entry in state.history(args.limit)? {
        if args.raw {
            println!("{}", serde_json::to_string(&entry)?);
            continue;
        }
        let display_role = if entry.role.ends_with("_clarification") {
            entry.role.trim_end_matches("_clarification")
        } else {
            entry.role.as_str()
        };
        println!("{} {display_role}", entry.timestamp);
        if entry.role.starts_with("assistant") {
            let response = crate::llm::ChatResult {
                content: entry.content,
                reasoning: if args.no_thinking {
                    None
                } else {
                    entry.reasoning
                },
                usage: None,
                usage_estimated: false,
                tool_calls: Vec::new(),
                provider_id: None,
                model: None,
                finish_reason: None,
                thinking_signature: None,
                last_request_usage: None,
                responses_continuation: None,
            };
            render::print_assistant_response(&response, !args.no_thinking)?;
        } else {
            println!("{}", entry.content);
        }
        println!();
    }
    Ok(())
}

async fn run_kb(paths: &GQYPaths, args: KbArgs) -> Result<()> {
    let config = AppConfig::load(paths)?;
    let kb = tools::knowledge_base::KnowledgeBase::new(config, paths.clone())?;
    match args.command {
        KbCommand::Add(args) => {
            let added = kb.add_path(&args.path).await?;
            for path in added {
                println!("{} {path}", t("added", "已添加"));
            }
        }
        KbCommand::List => {
            for file in kb.list()? {
                println!("{}\t{} {}", file.name, file.size_bytes, t("bytes", "字节"));
            }
        }
        KbCommand::Search(args) => {
            let query = args.query.join(" ");
            println!("{}", kb.search(&query, args.limit).await?);
        }
        KbCommand::Find(args) => {
            let query = args.query.join(" ");
            println!("{}", kb.find_by_name(&query, args.limit)?);
        }
        KbCommand::Read(args) => {
            println!("{}", kb.read_file(&args.file, args.start, args.lines)?);
        }
        KbCommand::Remove(args) => {
            kb.remove(&args.file)?;
            println!("{} {}", t("removed", "已移除"), args.file);
        }
        KbCommand::Reindex => {
            let files = kb.list()?;
            println!(
                "{}: {}",
                t(
                    "keyword index is rebuilt on demand; files tracked",
                    "关键词索引会按需重建；已跟踪文件数",
                ),
                files.len()
            );
        }
        KbCommand::Stats => {
            let mut stats = kb.stats()?;
            if let Some(object) = stats.as_object_mut() {
                if let Ok(status) = crate::default_kb::status(paths) {
                    object.insert(
                        "default_kb_update_available".to_string(),
                        serde_json::json!(status.has_update_notice),
                    );
                }
            }
            println!("{}", stats);
        }
        KbCommand::Embed(args) => match args.command {
            KbEmbedCommand::Reindex(args) => {
                kb.reindex_embeddings(args.quiet).await?;
            }
        },
    }
    Ok(())
}

async fn run_update_default_kb(paths: &GQYPaths) -> Result<()> {
    let config = AppConfig::load_or_default(paths)?;
    let state = crate::default_kb::update(paths, &config, |stage| {
        let mut stderr = io::stderr().lock();
        let _ = write_default_kb_update_progress(&mut stderr, stage);
    })?;
    println!(
        "{}: {}",
        t("updated default knowledge base", "已更新默认知识库"),
        state.shorin_wiki_commit
    );
    Ok(())
}

fn write_default_kb_update_progress(
    output: &mut impl Write,
    stage: crate::default_kb::UpdateStage,
) -> io::Result<()> {
    writeln!(output, "[default-kb] {}", stage.message())?;
    output.flush()
}

#[cfg(test)]
mod default_kb_progress_tests {
    use super::*;

    #[test]
    fn progress_is_emitted_as_a_complete_line() {
        let stage = crate::default_kb::UpdateStage::FetchingRepository;
        let mut output = Vec::new();

        write_default_kb_update_progress(&mut output, stage).unwrap();

        assert_eq!(
            String::from_utf8(output).unwrap(),
            format!("[default-kb] {}\n", stage.message())
        );
    }
}

fn run_memory(paths: &GQYPaths, args: MemoryArgs) -> Result<()> {
    let config = AppConfig::load_or_default(paths)?;
    let store = MemoryStore::new(&config, paths);
    match args.command {
        MemoryCommand::Stats => println!("{}", store.stats()?),
        MemoryCommand::Reset(args) => {
            store.reset_all(args.include_skills)?;
            println!("{}", t("cleared assistant memory", "已清空助手记忆"));
        }
        MemoryCommand::Search(args) => {
            let query = join_message(args.query);
            let limit = args.limit.unwrap_or(10);
            println!("{}", store.recall_memories(&query, limit, args.forgotten)?);
        }
        MemoryCommand::Remember(args) => {
            let content = join_message(args.content);
            let id = store.remember_fact(&content, &args.source)?;
            println!("{}: {id}", t("remembered fact", "已记住事实"));
        }
    }
    Ok(())
}

fn run_skills(paths: &GQYPaths, args: SkillsArgs) -> Result<()> {
    std::fs::create_dir_all(&paths.skills_dir)?;
    match args.command {
        SkillsCommand::List => {
            for name in skill_names(paths)? {
                let disabled = paths.skills_dir.join(&name).join(".disabled").exists();
                println!(
                    "{}{}",
                    name,
                    if disabled {
                        t(" [disabled]", " [已禁用]")
                    } else {
                        ""
                    }
                );
            }
        }
        SkillsCommand::Show(args) => {
            let path = skill_dir(paths, &args.name)?.join("SKILL.md");
            println!("{}", std::fs::read_to_string(path)?);
        }
        SkillsCommand::Enable(args) => {
            let marker = skill_dir(paths, &args.name)?.join(".disabled");
            if marker.exists() {
                std::fs::remove_file(marker)?;
            }
            println!("{}: {}", t("enabled skill", "已启用 skill"), args.name);
        }
        SkillsCommand::Disable(args) => {
            let marker = skill_dir(paths, &args.name)?.join(".disabled");
            std::fs::write(marker, "disabled\n")?;
            println!("{}: {}", t("disabled skill", "已禁用 skill"), args.name);
        }
        SkillsCommand::Remove(args) => {
            let dir = skill_dir(paths, &args.name)?;
            std::fs::remove_dir_all(dir)?;
            println!("{}: {}", t("removed skill", "已移除 skill"), args.name);
        }
        SkillsCommand::Stats => {
            let names = skill_names(paths)?;
            let disabled = names
                .iter()
                .filter(|name| paths.skills_dir.join(name).join(".disabled").exists())
                .count();
            println!(
                "{}",
                serde_json::json!({
                    "ok": true,
                    "skills_dir": paths.skills_dir.display().to_string(),
                    "skills": names.len(),
                    "disabled": disabled,
                    "enabled": names.len().saturating_sub(disabled),
                })
            );
        }
        SkillsCommand::Prune => {
            let mut removed = 0usize;
            for name in skill_names(paths)? {
                let dir = paths.skills_dir.join(&name);
                let raw = std::fs::read_to_string(dir.join("SKILL.md")).unwrap_or_default();
                if crate::skills::is_generated_skill(&raw) && dir.join(".disabled").exists() {
                    std::fs::remove_dir_all(dir)?;
                    removed += 1;
                }
            }
            println!("{}: {removed}", t("pruned skills", "已清理 skills"));
        }
    }
    Ok(())
}

fn skill_names(paths: &GQYPaths) -> Result<Vec<String>> {
    let mut names = Vec::new();
    if !paths.skills_dir.exists() {
        return Ok(names);
    }
    for entry in std::fs::read_dir(&paths.skills_dir)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() && entry.path().join("SKILL.md").is_file() {
            names.push(entry.file_name().to_string_lossy().to_string());
        }
    }
    names.sort();
    Ok(names)
}

fn skill_dir(paths: &GQYPaths, name: &str) -> Result<PathBuf> {
    let clean = name.trim();
    if clean.is_empty()
        || clean.contains('/')
        || clean.contains('\\')
        || clean == "."
        || clean == ".."
    {
        bail!("{}: {name}", t("invalid skill name", "无效 skill 名称"));
    }
    let dir = paths.skills_dir.join(clean);
    if !dir.join("SKILL.md").is_file() {
        bail!("{}: {name}", t("skill not found", "未找到 skill"));
    }
    Ok(dir)
}

async fn run_reset(paths: &GQYPaths) -> Result<()> {
    let config = AppConfig::load_or_default(paths)?;
    let state = StateStore::new(paths)?;
    let memory = MemoryStore::new(&config, paths);
    state.reset_conversation()?;
    memory.clear_evicted_context()?;
    memory.clear_pending_events()?;
    tools::clear_brew_review_state(paths)?;
    Ok(())
}

/// `gqy reset-memory`:清空当前人格的长期记忆。daemon 在跑走 IPC,
/// 否则本地直清;终端确认后执行。
async fn run_reset_memory_command(paths: &GQYPaths) -> Result<()> {
    if !io::stdin().is_terminal() {
        bail!(
            "{}",
            t(
                "reset-memory needs a terminal to confirm",
                "reset-memory 需要在终端确认"
            )
        );
    }
    if !confirm_stdin(t(
        "erase this persona's long-term memory (facts, diary, episodes)?",
        "确认清空长期记忆（事实/日记/经历）？",
    ))? {
        println!("{}", t("cancelled", "已取消"));
        return Ok(());
    }
    if ipc::daemon_info(paths).await.is_some() {
        send_ipc_admin(paths, IpcCommand::ResetMemory { mode: None }).await?;
    } else {
        let config = AppConfig::load_or_default(paths)?;
        MemoryStore::new(&config, paths).reset_all(false)?;
    }
    println!("{}", t("long-term memory erased", "长期记忆已清空"));
    Ok(())
}

fn wipe_summary() -> &'static str {
    t(
        "This erases everything GQY has accumulated: memory, every conversation's contents, group-chat contexts, and auto-generated skills. It cannot be undone.",
        "这会抹掉 GQY 积累的一切：记忆、所有会话的内容、群聊上下文、自动生成的技能。不可撤销。",
    )
}

async fn run_wipe(paths: &GQYPaths, assume_yes: bool) -> Result<()> {
    if !assume_yes {
        if !io::stdin().is_terminal() {
            bail!(
                "{}",
                t(
                    "wipe needs a terminal to confirm; pass --yes to run it unattended",
                    "wipe 需要在终端确认；非交互场景请加 --yes"
                )
            );
        }
        println!("{}", wipe_summary());
        if !confirm_stdin(t("wipe everything?", "确认全部抹掉？"))? {
            println!("{}", t("cancelled", "已取消"));
            return Ok(());
        }
    }
    if ipc::daemon_info(paths).await.is_some() {
        send_ipc_admin(paths, IpcCommand::WipePersona).await?;
    } else {
        let config = AppConfig::load_or_default(paths)?;
        let state = StateStore::new(paths)?;
        let persona = config.active_persona_scope();
        let bindings = state.platform_session_bindings(&persona, "onebot")?;
        let plugins = crate::platforms::plugins::PlatformPluginRegistry::built_in()?;
        plugins
            .after_persona_reset(&crate::platforms::plugins::PlatformPersonaResetContext {
                config: &config,
                paths,
                bindings: &bindings,
            })
            .await?;
        state.reset_persona_contexts(&persona, "onebot")?;
        state.reset_conversation_usage()?;
        MemoryStore::new(&config, paths).reset_all(true)?;
        tools::clear_brew_review_state(paths)?;
    }
    println!("{}", print_wipe_message());
    Ok(())
}

fn print_wipe_message() -> &'static str {
    t(
        "erased all conversations, QQ contexts, memory, and generated skills for the current persona",
        "已抹掉当前人格的全部会话内容、QQ 上下文、记忆和自动技能",
    )
}

fn print_reset_message() {
    let message = t("cleared current conversation history", "已清空当前会话历史");
    println!("\x1b[2m{message}\x1b[0m\n");
}

fn join_message(parts: Vec<String>) -> String {
    parts.join(" ").trim().to_string()
}

pub(crate) fn build_tool_registry(
    config: &AppConfig,
    paths: &GQYPaths,
    mode: AgentMode,
    interactive_questions: bool,
) -> Result<tools::ToolRegistry> {
    let mut registry = if config.tools.enabled {
        match mode {
            AgentMode::Normal => tools::builtin_registry(config, paths),
            AgentMode::Dev => tools::dev_registry(config, paths),
        }
    } else {
        tools::ToolRegistry::new()
    };
    if config.tools.enabled && config.skills.enabled {
        tools::register_skills(&mut registry, config, paths)?;
        if mode == AgentMode::Normal {
            tools::register_skill_authoring(&mut registry, config.clone(), paths.clone());
        }
    }
    if config.tools.enabled && interactive_questions {
        tools::register_ask_question(&mut registry);
    }
    tools::register_script_display_names(&registry);
    Ok(registry)
}

fn handle_agent_event(renderer: &mut render::StreamRenderer, event: AgentEvent) -> Result<()> {
    match event {
        AgentEvent::TurnStarted { .. } => Ok(()),
        AgentEvent::RawReasoning(_) => Ok(()),
        AgentEvent::FlushJournal => Ok(()),
        // 单次输出模式没有常驻 footer,逐请求计量快照无处可画。
        AgentEvent::RoundUsage { .. } => Ok(()),
        AgentEvent::Chunk(chunk) => {
            renderer.write_chunk(chunk)?;
            renderer.tick_spinner()
        }
        AgentEvent::ReasoningStart { received_at } => renderer.start_reasoning_phase(received_at),
        AgentEvent::ReasoningReset { received_at } => renderer.reset_reasoning_phase(received_at),
        AgentEvent::ReasoningPartStart { received_at } => {
            renderer.start_reasoning_part(received_at)
        }
        AgentEvent::ReasoningPartEnd { received_at } => renderer.finish_reasoning_part(received_at),
        AgentEvent::ReasoningTitle(title) => {
            renderer.write_reasoning_title(&title)?;
            renderer.tick_spinner()
        }
        AgentEvent::ToolCall {
            name, arguments, ..
        } => {
            renderer.write_tool_call(&name, &arguments)?;
            renderer.tick_spinner()
        }
        AgentEvent::ToolPreparing { name } => {
            renderer.write_tool_preparing(&name)?;
            renderer.tick_spinner()
        }
        AgentEvent::ToolResult {
            name, ok, output, ..
        } => {
            renderer.write_tool_result(&name, ok, &output)?;
            renderer.tick_spinner()
        }
        AgentEvent::ToolProgress { name, message, .. } => {
            renderer.write_tool_progress(&name, &message)?;
            renderer.tick_spinner()
        }
        AgentEvent::CommandOutput {
            name,
            stream,
            chunk,
            ..
        } => {
            renderer.write_command_output(&name, stream, &chunk)?;
            renderer.tick_spinner()
        }
        AgentEvent::PrepareForExternalOutput { ready } => {
            renderer.prepare_for_external_output()?;
            let _ = ready.send(true);
            Ok(())
        }
        AgentEvent::Image { .. } | AgentEvent::Artifact { .. } => Ok(()),
        AgentEvent::AskQuestion {
            request, responder, ..
        } => {
            renderer.prepare_for_external_output()?;
            let response = crate::question_tui::ask(&request).unwrap_or_else(|err| {
                crate::question::QuestionResponse::Unavailable(err.to_string())
            });
            if !matches!(&response, crate::question::QuestionResponse::Cancelled) {
                renderer.start_waiting()?;
            }
            let _ = responder.send(response);
            Ok(())
        }
        AgentEvent::QueuedPromptsConsumed { .. } => Ok(()),
        AgentEvent::GenerationSuperseded { .. } => Ok(()),
        AgentEvent::SpinnerTick => renderer.tick_spinner(),
        AgentEvent::CompactStart => {
            renderer.write_system_message(t("Compacting context...", "正在压缩上下文..."))?;
            renderer.tick_spinner()
        }
        AgentEvent::CompactChunk(chunk) => {
            renderer.write_compact_chunk(&chunk)?;
            renderer.tick_spinner()
        }
        AgentEvent::CompactEnd => {
            renderer.finish_compact()?;
            renderer.tick_spinner()
        }
        AgentEvent::PopStart => renderer.tick_spinner(),
        AgentEvent::PopEnd => renderer.tick_spinner(),
        AgentEvent::Notice { text } => {
            renderer.write_system_message(&text)?;
            renderer.tick_spinner()
        }
    }
}

#[cfg(test)]
mod remote_tool_image_tests {
    use super::*;
    use image::{Delay, Frame, Rgba, RgbaImage};

    #[test]
    fn web_tool_image_event_exposes_asset_id_to_remote_cli() {
        let event = serde_json::json!({
            "run_id": "run-1",
            "name": "show_meme",
            "asset": { "id": "img-1", "mime": "image/gif" }
        });
        assert_eq!(remote_tool_image_asset_id(&event), Some("img-1"));
        assert_eq!(remote_tool_image_asset_id(&serde_json::json!({})), None);
    }

    #[test]
    fn ipc_command_response_distinguishes_errors_and_closed_connections() {
        assert!(validate_ipc_command_response(Some(IpcFrame::Ack)).is_ok());
        let rejected = validate_ipc_command_response(Some(IpcFrame::Error {
            code: None,
            message: "GQY is busy with another operation".to_string(),
        }))
        .unwrap_err();
        assert!(rejected.to_string().contains("busy with another operation"));

        let closed = validate_ipc_command_response(None).unwrap_err();
        assert!(closed
            .to_string()
            .contains("closed the connection without a response"));

        let unexpected = validate_ipc_command_response(Some(IpcFrame::Accepted {
            turn_id: None,
            run_id: "run-test".to_string(),
        }))
        .unwrap_err();
        assert!(unexpected.to_string().contains("unexpected response"));
    }

    #[test]
    fn remote_gif_asset_is_converted_to_static_png() {
        let mut gif = Vec::new();
        {
            let mut encoder = image::codecs::gif::GifEncoder::new(&mut gif);
            encoder
                .encode_frames((0..2).map(|value| {
                    Frame::from_parts(
                        RgbaImage::from_pixel(32, 32, Rgba([value, 20, 40, 255])),
                        0,
                        0,
                        Delay::from_numer_denom_ms(100, 1),
                    )
                }))
                .unwrap();
        }
        let asset = crate::state::ImageAssetData {
            asset: crate::state::ImageAsset {
                asset_id: "img-gif".to_string(),
                turn_id: "turn-1".to_string(),
                tool_id: Some("tool-1".to_string()),
                mime: "image/gif".to_string(),
                width: 32,
                height: 32,
                alt: "animated meme".to_string(),
                created_at: "2026-01-01T00:00:00Z".to_string(),
            },
            bytes: gif,
        };
        let preview = remote_image_preview(&asset).unwrap();
        let bytes = std::fs::read(preview.path()).unwrap();
        assert!(bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
        assert_eq!(image::load_from_memory(&bytes).unwrap().width(), 32);
    }
}

