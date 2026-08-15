//! tests — 自 src/agent/mod.rs 外移。
#![cfg(test)]

use super::*;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ActiveProviderModelConfig, AppConfig, ProviderConfig};
    use crate::paths::GQYPaths;
    use crate::platforms::{
        ConversationKind, OutboundMessage, PlatformAdapter, PlatformConversation, SendReceipt,
    };
    use crate::tools::{empty_parameters, ToolSpec};
    use futures_util::future::BoxFuture;
    use std::path::PathBuf;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    struct NoopPlatformAdapter;

    #[test]
    fn artifact_delivery_detection_is_conservative() {
        assert!(artifact_delivery_requested(&[ChatMessage::plain(
            "user",
            "生成一个 macOS 游戏兼容性报告，保存为 Markdown 文件",
        )]));
        assert!(artifact_delivery_requested(&[ChatMessage::plain(
            "user",
            "create a standalone HTML file",
        )]));
        assert!(!artifact_delivery_requested(&[ChatMessage::plain(
            "user",
            "修改 src/main.rs 修复这个错误",
        )]));
    }

    #[test]
    fn artifact_candidates_only_include_new_files() {
        let created = artifact_candidate_paths(
            "write_file",
            r#"{"ok":true,"created":true,"path":"report.md"}"#,
        );
        assert_eq!(created.len(), 1);
        assert!(artifact_candidate_paths(
            "write_file",
            r#"{"ok":true,"created":false,"path":"src/main.rs"}"#,
        )
        .is_empty());
        assert!(artifact_candidate_paths(
            "apply_patch",
            r#"{"ok":true,"files":[{"path":"report.md","operation":"update"}]}"#,
        )
        .is_empty());
    }

    #[test]
    fn tool_call_stream_announces_preparation_for_slow_argument_tools() {
        let mut filter = ReasoningTitleFilter::default();
        let mut prepared = Vec::new();
        let mut streamed = Vec::new();
        let mut on_event = |event| {
            match event {
                AgentEvent::ToolPreparing { name } => prepared.push(name),
                AgentEvent::Chunk(chunk) if chunk.kind == ChatStreamKind::ToolCall => {
                    streamed.push(chunk.text)
                }
                _ => {}
            }
            Ok(())
        };
        let names = [
            "apply_patch",
            "apply_artifact_patch",
            "write_file",
            "edit_string",
            "run_command",
            "task",
            "ask_question",
            // Arguments arrive in one chunk: a hint here would only flicker.
            "read_file",
        ];
        for name in names {
            emit_filtered_chunk(
                ChatStreamChunk {
                    kind: ChatStreamKind::ToolCall,
                    text: name.to_string(),
                },
                &mut filter,
                &mut on_event,
            )
            .unwrap();
        }
        assert_eq!(
            prepared,
            [
                "apply_patch",
                "apply_artifact_patch",
                "write_file",
                "edit_string",
                "run_command",
                "task",
                "ask_question"
            ]
        );
        assert_eq!(streamed, names);
    }

    #[test]
    fn artifact_tool_report_keeps_cross_turn_filename_memory() {
        let report = extract_persistable_tool_report(
            "apply_artifact_patch",
            r#"{"ok":true,"files":[{"path":"report.md","operation":"update"}]}"#,
        )
        .unwrap();
        assert!(report.contains("report.md"));
        assert!(!report.contains("/home/test"));
    }

    impl PlatformAdapter for NoopPlatformAdapter {
        fn send<'a>(&'a self, _message: OutboundMessage) -> BoxFuture<'a, Result<SendReceipt>> {
            Box::pin(async { bail!("send is not used in this test") })
        }

        fn bot_display_name<'a>(&'a self) -> BoxFuture<'a, Result<String>> {
            Box::pin(async { Ok("GQY".to_string()) })
        }
    }

    #[test]
    fn strips_pasted_system_reminder_from_user_input() {
        let input = "继续<system-reminder>hidden</system-reminder> ok";

        assert_eq!(clean_user_visible_text(input), "继续 ok");
    }

    #[test]
    fn strips_unclosed_system_reminder_from_user_input() {
        let input = "继续<system_reminder>hidden";

        assert_eq!(clean_user_visible_text(input), "继续");
    }

    #[test]
    fn formats_dynamic_load_tool_names() {
        assert_eq!(
            tool_event_name("load_skill", r#"{"name":"web-search"}"#),
            "load_skill:web-search"
        );
        assert_eq!(
            tool_event_name("load_tools", r#"{"names":["get_weather","todoupdate"]}"#),
            "load_tools:get_weather,todoupdate"
        );
    }

    #[test]
    fn restores_loaded_tools_from_previous_tool_report() {
        let messages = vec![ChatMessage::plain(
            "assistant",
            "<previous_tool_report name=\"load_tools\">\n{\"loaded_tools\":[\"get_weather\",\"todoupdate\"]}\n</previous_tool_report>",
        )];
        let loaded = loaded_tools_from_messages(&messages);
        assert!(loaded.contains("get_weather"));
        assert!(loaded.contains("todoupdate"));
    }

    #[test]
    fn persists_loaded_tools_with_previous_tool_report_wrapper() {
        let output = serde_json::json!({
            "loaded_tools": [
                {"name": "get_weather"},
                {"name": "todoupdate"}
            ]
        })
        .to_string();

        assert_eq!(
            extract_persistable_tool_report("load_tools", &output).as_deref(),
            Some("<previous_tool_report name=\"load_tools\">\n{\"loaded_tools\":[\"get_weather\",\"todoupdate\"]}\n</previous_tool_report>")
        );
    }

    #[test]
    fn tool_footprint_extracts_paths_and_memories() {
        let fp = tool_call_footprint("read_file", r#"{"path":"/tmp/a.txt"}"#).unwrap();
        assert!(fp.read.contains("/tmp/a.txt"));
        let fp = tool_call_footprint(
            "edit_string",
            r#"{"path":"b.rs","old_string":"x","new_string":"y"}"#,
        )
        .unwrap();
        assert!(fp.modified.contains("b.rs"));
        // stub-mode wrapped arguments unwrap
        let fp = tool_call_footprint(
            "write_file",
            r#"{"arguments":{"path":"c.md","content":"hi"}}"#,
        )
        .unwrap();
        assert!(fp.modified.contains("c.md"));
        let fp = tool_call_footprint("remember_fact", r#"{"content":"用户住在杭州"}"#).unwrap();
        assert!(fp.memories.contains("用户住在杭州"));
        assert!(tool_call_footprint("bash", r#"{"command":"ls"}"#).is_none());
    }

    #[test]
    fn persists_compact_sent_meme_report() {
        let output = serde_json::json!({
            "success": true,
            "id": "sha256:abc123",
            "description": "猫猫\n开心 & <得意>",
            "unused": "ignored",
        })
        .to_string();

        assert_eq!(
            extract_persistable_tool_report("show_meme", &output).as_deref(),
            Some("<sent_meme>发送了一个表情包：id=sha256:abc123；description=猫猫 开心 &amp; &lt;得意&gt;</sent_meme>")
        );
    }

    #[test]
    fn sent_meme_report_allows_missing_description() {
        let output = serde_json::json!({
            "success": true,
            "id": "sha256:abc123",
        })
        .to_string();

        assert_eq!(
            extract_persistable_tool_report("show_meme", &output).as_deref(),
            Some("<sent_meme>发送了一个表情包：id=sha256:abc123</sent_meme>")
        );
    }

    #[test]
    fn sent_meme_report_skips_failed_result() {
        let output = serde_json::json!({
            "success": false,
            "id": "sha256:abc123",
            "description": "猫猫",
        })
        .to_string();

        assert!(extract_persistable_tool_report("show_meme", &output).is_none());
    }

    #[test]
    fn mode_reminder_does_not_inject_a_reasoning_title_protocol() {
        let prompt = with_mode_reminder("base".to_string(), AgentMode::Normal);
        assert_eq!(prompt, "base");
        assert!(!prompt.contains("<runtime"));

        // Dev 遵循极简原则:与 Normal 一样零模式提醒。
        let prompt = with_mode_reminder("base".to_string(), AgentMode::Dev);
        assert_eq!(prompt, "base");
    }

    #[test]
    fn reasoning_title_filter_emits_completed_markdown_title_immediately() {
        let mut filter = ReasoningTitleFilter::default();
        assert_eq!(filter.push("**Preparing to"), (None, None));
        assert_eq!(
            filter.push(" call tools**"),
            (Some("Preparing to call tools".to_string()), None)
        );
        assert_eq!(filter.finish(), (None, None));
    }

    #[test]
    fn reasoning_title_filter_strips_delayed_blank_line_before_body() {
        let mut filter = ReasoningTitleFilter::default();
        assert_eq!(
            filter.push("**Preparing to call tools**\n"),
            (Some("Preparing to call tools".to_string()), None)
        );
        assert_eq!(
            filter.push("\nInspect the arguments."),
            (None, Some("Inspect the arguments.".to_string()))
        );
    }

    #[test]
    fn reasoning_title_filter_streams_plain_body_without_inventing_title() {
        let mut filter = ReasoningTitleFilter::default();
        assert_eq!(
            filter.push("The user is"),
            (None, Some("The user is".to_string()))
        );
        assert_eq!(
            filter.push(" asking what changed."),
            (None, Some(" asking what changed.".to_string()))
        );
        assert_eq!(
            filter.push(" Continue analysis."),
            (None, Some(" Continue analysis.".to_string()))
        );
        assert_eq!(filter.finish(), (None, None));
    }

    #[test]
    fn reasoning_title_filter_keeps_long_markdown_heading_text() {
        let title = "heading ".repeat(12);
        let text = format!("# {title}\n\nBody reasoning.");
        let mut filter = ReasoningTitleFilter::default();
        let (parsed_title, body) = filter.push(&text);

        assert!(parsed_title.is_some());
        assert_eq!(body.as_deref(), Some("Body reasoning."));
        assert_eq!(filter.finish(), (None, None));
    }

    #[test]
    fn reasoning_title_filter_extracts_markdown_action_heading() {
        assert_eq!(
            parse_reasoning_title(
                "**Planning response approach and title clipping**\n\nInspect the renderer."
            ),
            (
                Some("Planning response approach and title clipping".to_string()),
                "Inspect the renderer.".to_string()
            )
        );
    }

    #[test]
    fn reasoning_title_filter_keeps_ordinary_bold_text_in_body() {
        assert_eq!(
            parse_reasoning_title("**Important:** keep this in the body."),
            (None, "**Important:** keep this in the body.".to_string())
        );
    }

    #[test]
    fn reasoning_title_filter_matches_unsplit_input_at_every_character_boundary() {
        for text in [
            "**检查参数**\n\n\n继续分析。",
            "## 检查参数\n\n\n继续分析。",
            "**Checking arguments**\r\n\r\nContinue analysis.",
            "#include <stdio.h>",
        ] {
            let expected = parse_reasoning_title(text);
            for split in text
                .char_indices()
                .map(|(index, _)| index)
                .chain(std::iter::once(text.len()))
            {
                assert_eq!(
                    parse_reasoning_title_chunks([&text[..split], &text[split..]]),
                    expected,
                    "different result when split at byte {split} in {text:?}"
                );
            }
        }
    }

    #[test]
    fn reasoning_title_filter_does_not_show_incomplete_bold_title() {
        assert_eq!(
            parse_reasoning_title("**Incomplete title"),
            (None, "**Incomplete title".to_string())
        );
    }

    #[test]
    fn reasoning_title_filter_does_not_use_first_sentence_as_title() {
        assert_eq!(
            parse_reasoning_title("Designing the clipping helper. Keep the rest."),
            (
                None,
                "Designing the clipping helper. Keep the rest.".to_string()
            )
        );
    }

    #[test]
    fn reasoning_part_start_reopens_title_detection() {
        let mut filter = ReasoningTitleFilter::default();
        let mut titles = Vec::new();
        let mut reasoning = Vec::new();
        let mut on_event = |event| {
            match event {
                AgentEvent::ReasoningTitle(title) => titles.push(title),
                AgentEvent::Chunk(chunk) if chunk.kind == ChatStreamKind::Reasoning => {
                    reasoning.push(chunk.text);
                }
                _ => {}
            }
            Ok(())
        };

        emit_filtered_chunk(
            ChatStreamChunk {
                kind: ChatStreamKind::ReasoningPartStart,
                text: String::new(),
            },
            &mut filter,
            &mut on_event,
        )
        .unwrap();
        emit_filtered_chunk(
            ChatStreamChunk {
                kind: ChatStreamKind::Reasoning,
                text: "**First title**\n\nFirst body.".to_string(),
            },
            &mut filter,
            &mut on_event,
        )
        .unwrap();
        emit_filtered_chunk(
            ChatStreamChunk {
                kind: ChatStreamKind::ReasoningPartEnd,
                text: String::new(),
            },
            &mut filter,
            &mut on_event,
        )
        .unwrap();
        emit_filtered_chunk(
            ChatStreamChunk {
                kind: ChatStreamKind::ReasoningPartStart,
                text: String::new(),
            },
            &mut filter,
            &mut on_event,
        )
        .unwrap();
        emit_filtered_chunk(
            ChatStreamChunk {
                kind: ChatStreamKind::Reasoning,
                text: "**Second title**".to_string(),
            },
            &mut filter,
            &mut on_event,
        )
        .unwrap();

        assert_eq!(titles, vec!["First title", "Second title"]);
        assert_eq!(reasoning, vec!["First body."]);
    }

    #[test]
    fn reasoning_summary_finishes_before_answer_content() {
        let mut filter = ReasoningTitleFilter::default();
        let mut events = Vec::new();
        let mut on_event = |event| {
            events.push(match event {
                AgentEvent::ReasoningPartStart { .. } => "part-start".to_string(),
                AgentEvent::ReasoningTitle(title) => format!("title:{title}"),
                AgentEvent::Chunk(chunk) => format!("{:?}:{}", chunk.kind, chunk.text),
                AgentEvent::ReasoningPartEnd { .. } => "part-end".to_string(),
                _ => "other".to_string(),
            });
            Ok(())
        };

        for chunk in [
            ChatStreamChunk {
                kind: ChatStreamKind::ReasoningPartStart,
                text: String::new(),
            },
            ChatStreamChunk {
                kind: ChatStreamKind::Reasoning,
                text: "**Checking event order**\n\nSummary body.".to_string(),
            },
            ChatStreamChunk {
                kind: ChatStreamKind::ReasoningPartEnd,
                text: String::new(),
            },
            ChatStreamChunk {
                kind: ChatStreamKind::Content,
                text: "Answer.".to_string(),
            },
        ] {
            emit_filtered_chunk(chunk, &mut filter, &mut on_event).unwrap();
        }

        assert_eq!(
            events,
            [
                "part-start",
                "title:Checking event order",
                "Reasoning:Summary body.",
                "part-end",
                "Content:Answer.",
            ]
        );
    }

    #[test]
    fn reasoning_boundaries_preserve_chunk_receive_timestamps() {
        let mut filter = ReasoningTitleFilter::default();
        let started_at = Instant::now();
        let ended_at = started_at + Duration::from_millis(725);
        let mut boundaries = Vec::new();
        let mut on_event = |event| {
            match event {
                AgentEvent::ReasoningPartStart { received_at } => {
                    boundaries.push(("start", received_at));
                }
                AgentEvent::ReasoningPartEnd { received_at } => {
                    boundaries.push(("end", received_at));
                }
                _ => {}
            }
            Ok(())
        };

        emit_filtered_chunk_at(
            ChatStreamChunk {
                kind: ChatStreamKind::ReasoningPartStart,
                text: String::new(),
            },
            started_at,
            &mut filter,
            &mut on_event,
        )
        .unwrap();
        emit_filtered_chunk_at(
            ChatStreamChunk {
                kind: ChatStreamKind::ReasoningPartEnd,
                text: String::new(),
            },
            ended_at,
            &mut filter,
            &mut on_event,
        )
        .unwrap();

        assert_eq!(boundaries, [("start", started_at), ("end", ended_at)]);
    }

    #[test]
    fn reasoning_title_filter_does_not_treat_hash_include_as_heading() {
        assert_eq!(
            parse_reasoning_title("#include <stdio.h>"),
            (None, "#include <stdio.h>".to_string())
        );
    }

    #[test]
    fn runtime_context_contains_dynamic_runtime_only() {
        let context = runtime_context(AgentMode::Normal, false);
        assert!(context.starts_with("<runtime "));
        assert!(context.contains("now=\""));
        assert!(context.contains("cwd=\""));
    }

    #[test]
    fn a_platform_runtime_stamp_carries_nothing_a_chat_message_cannot_use() {
        // A QQ turn has no working directory, no shell and no terminal. Those
        // attributes were re-sent at full price on every single turn — 285
        // chars where a timestamp needs about 45.
        let platform = runtime_context(AgentMode::Normal, true);
        assert!(platform.contains("now=\""), "{platform}");
        for noise in ["cwd=", "shell=", "terminal=", "env=", "note="] {
            assert!(!platform.contains(noise), "{noise} in {platform}");
        }
        assert!(
            platform.len() * 3 < runtime_context(AgentMode::Normal, false).len(),
            "{platform}"
        );
    }

    #[test]
    fn host_environment_rides_the_system_prompt_for_owners_only() {
        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths(temp.path());

        let owner = with_host_environment(
            "base".to_string(),
            PromptAudience::Owner,
            &paths,
            AgentMode::Normal,
        );
        assert!(owner.starts_with("base\n\n<host-environment os=\""));
        assert!(owner.contains("/>"));
        assert!(owner.contains("LaTeX"), "渲染能力说明应跟随 owner 提示词");
        assert!(owner.contains(&format!(" gqy_home=\"{}\"", paths.root_dir.display())));
        // The static block must not be mistaken for the per-turn stamp, and
        // `mode_reminder_does_not_inject_a_reasoning_title_protocol` asserts the
        // system prompt never carries a `<runtime` tag.
        assert!(!owner.contains("<runtime"));

        // Platform and judge sessions come out byte-identical to today's prompt,
        // so they take no prefix-cache cold start from this change at all.
        for audience in [PromptAudience::External, PromptAudience::Internal] {
            assert_eq!(
                with_host_environment("base".to_string(), audience, &paths, AgentMode::Normal),
                "base",
                "{audience:?} must be untouched"
            );
        }
    }

    #[test]
    fn host_environment_is_byte_stable_across_prompt_rebuilds() {
        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths(temp.path());
        // Rebuilt on every turn by `prepare_for_turn`; a value that drifted
        // between rebuilds would move the prefix and cost a cache miss a turn.
        let first = with_host_environment(
            String::new(),
            PromptAudience::Owner,
            &paths,
            AgentMode::Normal,
        );
        let second = with_host_environment(
            String::new(),
            PromptAudience::Owner,
            &paths,
            AgentMode::Normal,
        );
        assert_eq!(first, second);
    }

    #[test]
    fn user_identity_is_limited_to_owner_prompts() {
        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths(temp.path());
        std::fs::create_dir_all(&paths.config_dir).unwrap();
        let mut config = AppConfig::default();
        std::fs::create_dir_all(config.identities_dir_path(&paths)).unwrap();
        std::fs::write(config.user_identity_path(&paths), "legacy-owner-marker").unwrap();

        let owner = config
            .system_prompt_for(&paths, PromptAudience::Owner)
            .unwrap();
        let external = config
            .system_prompt_for(&paths, PromptAudience::External)
            .unwrap();
        let internal = config
            .system_prompt_for(&paths, PromptAudience::Internal)
            .unwrap();
        assert!(owner.contains("legacy-owner-marker"));
        assert!(!external.contains("legacy-owner-marker"));
        assert!(!internal.contains("legacy-owner-marker"));

        config.prompt.active_identity = "owner.md".to_string();
        std::fs::write(
            config.identity_path(&paths, "owner.md"),
            "active-owner-marker",
        )
        .unwrap();
        assert!(config
            .system_prompt_for(&paths, PromptAudience::Owner)
            .unwrap()
            .contains("active-owner-marker"));
        assert!(!config
            .system_prompt_for(&paths, PromptAudience::External)
            .unwrap()
            .contains("active-owner-marker"));
    }

    #[test]
    fn runtime_system_context_refreshes_the_effective_prompt_immediately() {
        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths(temp.path());
        let config = AppConfig::default();
        let state = StateStore::new(&paths).unwrap();
        let client =
            OpenAiCompatibleClient::new(config.provider(None).unwrap(), &config, &paths).unwrap();
        let mut agent = Agent::new(
            config,
            &paths,
            state,
            client,
            ToolRegistry::new(),
            AgentMode::Normal,
        )
        .unwrap();

        agent
            .set_runtime_system_context(vec!["  platform-only notice  ".to_string()])
            .unwrap();
        assert!(agent.system_prompt.contains("platform-only notice"));
        assert_eq!(
            agent.runtime_system_context,
            vec!["platform-only notice".to_string()]
        );
    }

    #[test]
    fn structured_platform_context_can_suppress_ambiguous_session_replay() {
        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths(temp.path());
        let config = AppConfig::default();
        let state = StateStore::new(&paths).unwrap();
        state
            .start_turn("old", "anonymous old user", 999_999)
            .unwrap();
        state.complete_turn("old", "old assistant", None).unwrap();
        let client =
            OpenAiCompatibleClient::new(config.provider(None).unwrap(), &config, &paths).unwrap();
        let mut agent = Agent::new(
            config,
            &paths,
            state,
            client,
            ToolRegistry::new(),
            AgentMode::Normal,
        )
        .unwrap();

        assert!(agent
            .chat_messages("current", "new user")
            .unwrap()
            .0
            .iter()
            .any(|message| format!("{:?}", message.content).contains("anonymous old user")));
        agent.set_session_history_suppressed(true);
        let messages = agent.chat_messages("current", "new user").unwrap().0;
        assert!(!messages
            .iter()
            .any(|message| format!("{:?}", message.content).contains("anonymous old user")));
        // [.., user, runtime tail]: the current user message sits right before
        // the transient runtime stamp.
        assert!(format!("{:?}", messages[messages.len() - 2].content).contains("new user"));
        assert!(format!("{:?}", messages.last().unwrap().content).contains("<runtime now="));
    }

    #[test]
    fn fossilized_transient_tail_replays_between_user_and_assistant() {
        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths(temp.path());
        let config = AppConfig::default();
        let state = StateStore::new(&paths).unwrap();
        state.start_turn("old", "old question", 999_999).unwrap();
        state
            .set_turn_context_messages(
                "old",
                &[
                    ChatMessage::turn_context("<runtime now=\"frozen stamp\"/>"),
                    ChatMessage::turn_context(
                        "<associative-memory>frozen recall</associative-memory>",
                    ),
                ],
            )
            .unwrap();
        state.complete_turn("old", "old answer", None).unwrap();
        let client =
            OpenAiCompatibleClient::new(config.provider(None).unwrap(), &config, &paths).unwrap();
        let agent = Agent::new(
            config,
            &paths,
            state,
            client,
            ToolRegistry::new(),
            AgentMode::Normal,
        )
        .unwrap();

        let messages = agent.chat_messages("current", "next question").unwrap().0;
        let text = |message: &ChatMessage| format!("{:?}", message.content);
        let user = messages
            .iter()
            .position(|m| text(m).contains("old question"))
            .unwrap();
        let assistant = messages
            .iter()
            .position(|m| text(m).contains("old answer"))
            .unwrap();
        // The fossils sit, in order, strictly between the user message and the
        // assistant reply — byte-for-byte what the live request sent.
        assert_eq!(messages[user + 1].role, "user");
        assert!(text(&messages[user + 1]).contains("frozen stamp"));
        assert_eq!(messages[user + 2].role, "user");
        assert!(text(&messages[user + 2]).contains("frozen recall"));
        assert!(user + 2 < assistant);
    }

    #[test]
    fn a_still_running_turn_stays_out_of_everyone_elses_history() {
        // A running turn holds a placeholder that is overwritten with the real
        // reply when it finishes, so replaying it puts two different byte
        // sequences at the same position and drops the prefix cache for every
        // turn behind it. About a fifth of this group's turns overlap.
        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths(temp.path());
        let state = StateStore::new(&paths).unwrap();
        state
            .start_turn("t1", "第一条", std::process::id())
            .unwrap();
        state
            .complete_turn_with_usage_and_model(
                "t1",
                "答复一",
                None,
                None,
                None,
                TurnTokens::default(),
                false,
            )
            .unwrap();
        state
            .start_turn("t2", "并发的一条", std::process::id())
            .unwrap();

        let visible = state.load_visible_turns_excluding("t3").unwrap();
        let running: Vec<&str> = visible
            .iter()
            .filter(|turn| turn.status == crate::state::TurnStatus::Running)
            .map(|turn| turn.turn_id.as_str())
            .collect();
        assert_eq!(running, ["t2"], "the store still hands them over");
        assert_eq!(
            visible
                .iter()
                .filter(|turn| turn.status != crate::state::TurnStatus::Running)
                .count(),
            1,
            "and exactly one is replayable"
        );
    }

    #[test]
    fn nothing_after_the_leading_prompt_may_carry_the_system_role() {
        // Provider chat templates gather every `system` message to the front of
        // the rendered prompt, so one appearing mid-conversation shifts that
        // block and drops the prefix cache to zero. Measured on DeepSeek with a
        // byte-identical prefix: appending `assistant + user` hit 99%, the same
        // append with one `system` in front of it hit 0%, and moving that
        // `system` to the very end still hit 0%.
        let messages = vec![
            ChatMessage::system("persona"),
            ChatMessage::plain("user", "问题"),
            ChatMessage::turn_context("<runtime now=\"x\"/>"),
            ChatMessage::turn_context("<associative-memory>x</associative-memory>"),
            ChatMessage::assistant("答案", None),
        ];
        let stray: Vec<usize> = messages
            .iter()
            .enumerate()
            .skip(1)
            .filter(|(_, message)| message.role == "system")
            .map(|(index, _)| index)
            .collect();
        assert!(
            stray.is_empty(),
            "system role at {stray:?} would reset the prefix cache"
        );
    }

    #[test]
    fn a_fossil_written_before_the_role_change_replays_as_a_user_block() {
        // Old turns stored the transient tail as `system`. Replaying that
        // verbatim would keep poisoning the prefix for the rest of the
        // session's life, so it is re-roled on the way out.
        let stored = ChatMessage::system("<runtime now=\"old\"/>");
        let replayed = replay_fossil(&stored);
        assert_eq!(replayed.role, "user");
        assert!(replayed.transient_context);
        assert!(matches!(
            replayed.content.as_ref(),
            Some(ChatContent::Text(content)) if content == "<runtime now=\"old\"/>"
        ));

        // Already-converted fossils pass through untouched.
        let fresh = ChatMessage::turn_context("<runtime now=\"new\"/>");
        assert_eq!(replay_fossil(&fresh).role, "user");
    }

    #[test]
    fn fossil_capture_stops_at_the_first_non_context_message() {
        let tail = vec![
            ChatMessage::turn_context("<runtime now=\"x\"/>"),
            ChatMessage::turn_context("hint"),
            ChatMessage::plain("assistant", "loop starts here"),
            ChatMessage::turn_context("after loop — must not be captured"),
        ];
        let fossil = fossil_context_messages(&tail);
        assert_eq!(fossil.len(), 2);
        assert!(format!("{:?}", fossil[1].content).contains("hint"));
    }
}
