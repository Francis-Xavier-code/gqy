//! tests — 自 src/cli.rs 外移。
#![cfg(test)]

use super::*;

mod repl_input_tests {
    use super::*;

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
                command: Some(DaemonCommand::Logs(DaemonLogsArgs {
                    lines: Some(25),
                    ..
                })),
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

        assert!(parse_args(["gqy", "daemon", "--password"].map(OsString::from).to_vec(),).is_err());
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
}
