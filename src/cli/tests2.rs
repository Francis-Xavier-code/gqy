//! tests2 — 自 src/cli/tests.rs 拆分。
#![cfg(test)]

use super::*;

mod repl_input_tests2 {
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
        assert_eq!(
            split_repl_command("/persona Alice.md"),
            ("/persona", "Alice.md")
        );
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
