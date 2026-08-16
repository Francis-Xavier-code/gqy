//! tests2 — 自 src/render/mod.rs 外移。
#![cfg(test)]

use super::tests::*;

#[test]
fn tool_summary_suppresses_subagent_reasoning_even_when_reasoning_is_full() {
    let mut renderer = StreamRenderer::new(
        ReasoningDisplayMode::Full,
        ToolCallDisplayMode::Summary,
        false,
        true,
        10,
    );
    renderer.live_summary = false;
    renderer
        .write_tool_call("task", r#"{"description":"分析问题","prompt":"details"}"#)
        .unwrap();

    renderer
        .write_tool_progress("task", "__subagent_reasoning__Inspecting state")
        .unwrap();

    let stats = renderer.tool_stats.get("task").unwrap();
    assert_eq!(stats.calls, 1);
    assert!(stats.started_at.is_some());
    assert_eq!(renderer.subagent_mode, None);
}

#[test]
fn tool_summary_keeps_final_subagent_stats() {
    let mut renderer = StreamRenderer::new(
        ReasoningDisplayMode::Summary,
        ToolCallDisplayMode::Summary,
        true,
        true,
        10,
    );
    renderer.tool_stats.insert(
        "deep_research".to_string(),
        ToolStats {
            calls: 1,
            ok: 1,
            error: 0,
            subject: None,
            progress: None,
            final_progress: Some("工具调用 1 次　消耗词元 2.3K".to_string()),
            ..ToolStats::default()
        },
    );

    assert_eq!(
        renderer.tool_summary_text(),
        format!(
            "~ {}×1 ok\n  ✓ 工具调用 1 次　消耗词元 2.3K",
            t("Deep research", "深度研究")
        )
    );
}

#[test]
fn task_summary_omits_tool_prefix() {
    let mut renderer = StreamRenderer::new(
        ReasoningDisplayMode::Summary,
        ToolCallDisplayMode::Summary,
        true,
        true,
        10,
    );
    renderer.tool_stats.insert(
        "task".to_string(),
        ToolStats {
            calls: 1,
            ok: 0,
            error: 0,
            subject: Some("定位活动摘要渲染链路".to_string()),
            progress: None,
            final_progress: None,
            ..ToolStats::default()
        },
    );

    let header = format!("~ {}×1 {}", t("Subagent", "子代理"), t("running", "运行中"));
    assert_eq!(renderer.tool_summary_header(), header);
    assert_eq!(
        renderer.tool_summary_text(),
        format!("{header}\n  ↳ 定位活动摘要渲染链路")
    );
}

#[test]
fn parallel_subagents_render_stacked_blocks() {
    let mut renderer = StreamRenderer::new(
        ReasoningDisplayMode::Summary,
        ToolCallDisplayMode::Summary,
        true,
        true,
        10,
    );
    for (name, subject, progress) in [
        ("task:任务A", "任务A", Some("工具 #1: 运行命令")),
        ("task:任务B", "任务B", None),
        ("task:任务C", "任务C", Some("正在搜索")),
    ] {
        renderer.tool_stats.insert(
            name.to_string(),
            ToolStats {
                calls: 1,
                ok: 0,
                error: 0,
                subject: Some(subject.to_string()),
                progress: progress.map(str::to_string),
                final_progress: None,
                ..ToolStats::default()
            },
        );
    }
    let (phase, sub) = renderer.tool_summary_live();
    // Block mode: no shared phase line — every subagent is its own block.
    assert_eq!(phase, "");
    let sub = sub.expect("stacked blocks present");
    let marker = wait_spinner::BLOCK_MARKER;
    let lines: Vec<&str> = sub.lines().collect();
    // Each running block header carries the spinner marker; its own
    // progress follows; blank lines separate blocks. The redundant
    // subject line (same as the description in the header) is dropped.
    assert!(lines[0].starts_with(marker) && lines[0].contains("任务A"));
    assert_eq!(lines[1], "  ↳ 工具 #1: 运行命令");
    assert_eq!(lines[2], "");
    assert!(lines[3].starts_with(marker) && lines[3].contains("任务B"));
    assert_eq!(lines[4], "");
    assert!(lines[5].starts_with(marker) && lines[5].contains("任务C"));
    assert_eq!(lines[6], "  ↳ 正在搜索");
    assert_eq!(lines.len(), 7);
}

#[test]
fn live_blocks_freeze_settled_subagents_in_place() {
    let mut renderer = StreamRenderer::new(
        ReasoningDisplayMode::Summary,
        ToolCallDisplayMode::Summary,
        true,
        true,
        10,
    );
    renderer.tool_stats.insert(
        "task:任务A".to_string(),
        ToolStats {
            calls: 1,
            subject: Some("任务A".to_string()),
            progress: Some("正在搜索".to_string()),
            ..ToolStats::default()
        },
    );
    renderer.tool_stats.insert(
        "task:任务B".to_string(),
        ToolStats {
            calls: 1,
            ok: 1,
            subject: Some("任务B".to_string()),
            final_progress: Some("工具调用 1 次".to_string()),
            ..ToolStats::default()
        },
    );
    let (phase, sub) = renderer.tool_summary_live();
    assert_eq!(phase, "");
    let sub = sub.expect("blocks present");
    let marker = wait_spinner::BLOCK_MARKER;
    let lines: Vec<&str> = sub.lines().collect();
    // Running block keeps its animated marker + indented live progress…
    assert!(lines[0].starts_with(marker) && lines[0].contains("任务A"));
    assert_eq!(lines[1], "  ↳ 正在搜索");
    assert_eq!(lines[2], "");
    // …while the settled block drops the spinner glyph from its header;
    // detail lines stay two columns in, matching the committed layout.
    assert!(lines[3].starts_with("~ ") && lines[3].contains("任务B"));
    assert!(lines[3].contains("ok"));
    assert_eq!(lines[4], "  ✓ 工具调用 1 次");
    assert_eq!(lines.len(), 5);
}

#[test]
fn committed_summary_keeps_block_headers_when_one_subagent_finishes() {
    let mut renderer = StreamRenderer::new(
        ReasoningDisplayMode::Summary,
        ToolCallDisplayMode::Summary,
        true,
        true,
        10,
    );
    renderer.tool_stats.insert(
        "task:任务A".to_string(),
        ToolStats {
            calls: 1,
            subject: Some("任务A".to_string()),
            ..ToolStats::default()
        },
    );
    renderer.tool_stats.insert(
        "task:任务B".to_string(),
        ToolStats {
            calls: 1,
            ok: 1,
            subject: Some("任务B".to_string()),
            final_progress: Some("工具调用 1 次".to_string()),
            ..ToolStats::default()
        },
    );
    let text = renderer.tool_summary_text();
    let lines: Vec<&str> = text.lines().collect();
    // Each block keeps its own "~" header; a blank line separates blocks.
    assert!(lines[0].starts_with("~ ") && lines[0].contains("任务A"));
    assert_eq!(lines[1], "");
    assert!(lines[2].starts_with("~ ") && lines[2].contains("任务B"));
    assert_eq!(lines[3], "  ✓ 工具调用 1 次");
    assert_eq!(lines.len(), 4);
}

#[test]
fn all_subagent_summaries_use_activity_prefix() {
    for name in ["task", "deep_research"] {
        let mut renderer = StreamRenderer::new(
            ReasoningDisplayMode::Summary,
            ToolCallDisplayMode::Summary,
            true,
            true,
            10,
        );
        renderer.tool_stats.insert(
            name.to_string(),
            ToolStats {
                calls: 1,
                ok: 0,
                error: 0,
                subject: None,
                progress: None,
                final_progress: None,
                ..ToolStats::default()
            },
        );

        assert_eq!(
            renderer.tool_summary_header(),
            format!(
                "~ {}×1 {}",
                readable_tool_name(name),
                t("running", "运行中")
            )
        );
    }
}

#[test]
fn load_tools_keeps_targets_on_the_status_line() {
    let mut renderer = StreamRenderer::new(
        ReasoningDisplayMode::Summary,
        ToolCallDisplayMode::Summary,
        false,
        true,
        10,
    );
    renderer.tool_stats.insert(
        "load_tools:web_search,get_weather".to_string(),
        ToolStats {
            calls: 1,
            ok: 1,
            subject: Some("网络搜索、天气查询".to_string()),
            ..ToolStats::default()
        },
    );

    assert_eq!(
        renderer.tool_summary_text(),
        format!("~ {}×1 ok · 网络搜索、天气查询", t("Load", "加载"))
    );
    assert!(!renderer.tool_summary_text().contains("\n↳"));
}

#[test]
fn tool_status_counts_mixed_multiple_calls() {
    let stats = ToolStats {
        calls: 3,
        ok: 1,
        error: 1,
        subject: None,
        progress: None,
        final_progress: None,
        ..ToolStats::default()
    };
    assert_eq!(
        tool_status_text("grep", &stats, false),
        format!("grep×3 {}:1 ok:1 err:1", t("running", "运行中"))
    );
}

#[test]
fn trash_subject_counts_items_but_names_a_lone_path() {
    assert_eq!(
        tool_subject("trash_path", r#"{"paths":["/tmp/a","/tmp/b","/tmp/c"]}"#).as_deref(),
        Some(t("3 items", "3 项"))
    );
    // 只删一个时路径比「1 项」有用。
    assert_eq!(
        tool_subject("trash_path", r#"{"paths":["/tmp/only.txt"]}"#).as_deref(),
        Some("/tmp/only.txt")
    );
    assert_eq!(tool_subject("trash_path", r#"{"paths":[]}"#), None);
}

/// 失败清单自带 `✗`,不能再被套一个 `✓` 变成「✓ ✗ 权限不足」。
#[test]
fn final_progress_keeps_lines_that_carry_their_own_marker() {
    let renderer = StreamRenderer::new(
        ReasoningDisplayMode::Summary,
        ToolCallDisplayMode::Summary,
        false,
        true,
        10,
    );
    let stats = ToolStats {
        calls: 1,
        ok: 1,
        final_progress: Some("✗ /etc/hosts  权限不足\n收尾完成".to_string()),
        ..Default::default()
    };
    let lines = renderer.tool_block_lines("trash_path", &stats, false);
    assert!(
        lines
            .iter()
            .any(|line| line.trim() == "✗ /etc/hosts  权限不足"),
        "失败行应原样保留：{lines:?}"
    );
    assert!(
        lines.iter().any(|line| line.trim() == "✓ 收尾完成"),
        "普通行仍要套 ✓：{lines:?}"
    );
}

#[test]
fn tool_subject_extracts_safe_operation_targets() {
    assert_eq!(
        tool_subject("web_search", r#"{"query":"OpenCode 工具摘要"}"#).as_deref(),
        Some("OpenCode 工具摘要")
    );
    assert_eq!(
        tool_subject(
            "task",
            r#"{"description":"定位渲染链路","prompt":"private details"}"#
        )
        .as_deref(),
        Some("定位渲染链路")
    );
    assert_eq!(
        tool_subject("grep", r#"{"pattern":"ToolStats","path":"src"}"#).as_deref(),
        Some("ToolStats · src")
    );
    assert_eq!(
        tool_subject("run_command", r#"{"command":"du -sh /home/shorin/*"}"#).as_deref(),
        Some("du -sh /home/shorin/*")
    );
    let expected_load_tools_subject = format!(
        "{}{}{}",
        t("Web search", "网络搜索"),
        t(", ", "、"),
        t("Weather", "天气查询")
    );
    assert_eq!(
        tool_subject(
            "load_tools:web_search,get_weather",
            r#"{"names":["web_search","get_weather"]}"#
        )
        .as_deref(),
        Some(expected_load_tools_subject.as_str())
    );
}

#[test]
fn read_file_subject_shows_the_page_range() {
    assert_eq!(
        tool_subject("read_file", r#"{"path":"/tmp/a.rs"}"#).as_deref(),
        Some("/tmp/a.rs")
    );
    assert_eq!(
        tool_subject(
            "read_file",
            r#"{"path":"/tmp/a.rs","offset":2001,"limit":2000}"#
        )
        .as_deref(),
        Some("/tmp/a.rs (L2001-4000)")
    );
    assert_eq!(
        tool_subject("read_file", r#"{"path":"/tmp/a.rs","limit":500}"#).as_deref(),
        Some("/tmp/a.rs (L1-500)")
    );
    assert_eq!(
        tool_subject("read_file", r#"{"path":"/tmp/a.rs","offset":300}"#).as_deref(),
        Some("/tmp/a.rs (L300+)")
    );
}

#[test]
fn tool_subject_redacts_urls_and_ignores_unknown_arguments() {
    let subject = tool_subject(
        "web_fetch",
        r#"{"url":"https://user:secret@example.com/path?token=hidden#fragment"}"#,
    )
    .unwrap();
    assert_eq!(subject, "https://example.com/path");
    assert!(!subject.contains("secret"));
    assert!(!subject.contains("token"));
    assert_eq!(
        tool_subject("mcp_unknown", r#"{"password":"hidden","query":"private"}"#),
        None
    );
    assert_eq!(
        tool_subject(
            "web_search",
            r#"{"query":"查找 token=super-secret, Rust 文档"}"#
        )
        .as_deref(),
        Some("查找 token=[redacted], Rust 文档")
    );
    assert_eq!(
        safe_inline_subject(r#"请求 {"token":"super-secret"}"#).as_deref(),
        Some(r#"请求 {"token":"[redacted]"}"#)
    );
    assert_eq!(
        safe_inline_subject("Authorization Bearer super-secret").as_deref(),
        Some("Authorization [redacted]")
    );
    assert_eq!(
        safe_inline_subject("curl --password hunter2 https://example.com").as_deref(),
        Some("curl --password [redacted] https://example.com")
    );
    assert_eq!(
        safe_inline_subject("Bearer ghp_super-secret next").as_deref(),
        Some("Bearer [redacted] next")
    );
    assert_eq!(
        safe_inline_subject("curl --password\nhunter2 https://example.com").as_deref(),
        Some("curl --password [redacted] https://example.com")
    );
    assert_eq!(
        safe_inline_subject("Bearer\nghp_super-secret next").as_deref(),
        Some("Bearer [redacted] next")
    );
    assert_eq!(
        safe_inline_subject("AWS_SECRET_ACCESS_KEY=super-secret command").as_deref(),
        Some("AWS_SECRET_ACCESS_KEY=[redacted]")
    );
    assert_eq!(
        safe_inline_subject("AWS_ACCESS_KEY_ID=AKIAEXAMPLE command").as_deref(),
        Some("AWS_ACCESS_KEY_ID=[redacted]")
    );
    assert_eq!(
        safe_inline_subject("password hunter2").as_deref(),
        Some("password [redacted]")
    );
}

#[test]
fn tool_subject_is_single_line_and_terminal_safe() {
    let subject = tool_subject("web_search", "{\"query\":\"safe\\ntext\\u001b[2J\"}").unwrap();
    assert_eq!(subject, "safe text");
}

#[test]
fn show_meme_is_a_silent_tool() {
    assert!(is_silent_tool("show_meme"));
    assert!(!is_silent_tool("search_meme"));
}

#[test]
fn readable_tool_names_translate_known_tools_and_fallback_unknown() {
    for (name, english, chinese) in [
        ("deep_research", "Deep research", "深度研究"),
        ("read_file", "Read file", "读取文件"),
        ("check_issue", "Check issue", "检查问题"),
        ("check_os_info", "System information", "查看系统信息"),
        ("get_weather", "Weather", "天气查询"),
        ("get_exchange_rate", "Exchange rates", "汇率查询"),
        ("draw_zhouyi_hexagram", "Draw I Ching hexagram", "周易起卦"),
        ("draw_tarot_card", "Draw tarot card", "抽塔罗牌"),
        ("draw_fortune_lot", "Draw fortune", "吉凶占"),
        ("vision_analyze", "Analyze image", "分析图片"),
        ("search_meme", "Search memes", "搜索表情包"),
        ("show_meme", "Send meme", "发送表情"),
        ("add_meme", "Add meme", "添加表情包"),
        ("task", "Subagent", "子代理"),
        (
            "upload_text_to_knowledge_base",
            "Import knowledge base",
            "导入知识库",
        ),
        (
            "search_evicted_context",
            "Search old context",
            "搜索旧上下文",
        ),
        ("recall_past_events", "Recall past events", "回忆往事"),
        (
            "brew_check_status",
            "Check Homebrew status",
            "查询 Homebrew 状态",
        ),
        ("online_man_search", "Search online manuals", "搜索在线手册"),
        ("online_man_get_page", "Read online manual", "读取在线手册"),
        (
            "install_brew_package",
            "Install Homebrew package",
            "安装 Homebrew 包",
        ),
        (
            "search_knowledge_base_by_name",
            "Search knowledge base by name",
            "按名称搜索知识库",
        ),
        ("recall_memories", "Recall memories", "召回记忆"),
    ] {
        assert_eq!(readable_tool_name(name), t(english, chinese), "{name}");
    }
    assert_eq!(readable_tool_name("custom_skill"), "custom_skill");
}

#[test]
fn summary_styles_distinguish_reasoning_from_tools() {
    assert_eq!(
        style_summary_text("工具", SummaryStyle::Tool),
        "\x1b[2m工具\x1b[0m"
    );
    assert_eq!(
        style_summary_text("思考", SummaryStyle::Reasoning),
        "\x1b[38;5;10m思考\x1b[0m"
    );
}

#[test]
fn ordinary_activity_summaries_have_one_blank_line_without_leading_gap() {
    let mut output = Vec::new();
    write_activity_summary(&mut output, "思考摘要", SummaryStyle::Reasoning).unwrap();
    write_activity_summary(&mut output, "~ 工具×1 ok", SummaryStyle::Tool).unwrap();
    let output = strip_ansi_for_test(&String::from_utf8(output).unwrap());

    assert_eq!(output, "思考摘要\n\n~ 工具×1 ok\n\n");
    assert!(!output.starts_with('\n'));
}

#[test]
fn reasoning_summary_reserves_one_blank_line_before_subagent_activity() {
    let mut output = Vec::new();
    write_activity_summary(
        &mut output,
        "思考 · 59 词元 · 2.5s",
        SummaryStyle::Reasoning,
    )
    .unwrap();
    write!(output, "~ 游戏兼容性调查×1 运行中").unwrap();
    let output = strip_ansi_for_test(&String::from_utf8(output).unwrap());

    assert_eq!(output, "思考 · 59 词元 · 2.5s\n\n~ 游戏兼容性调查×1 运行中");
}

#[test]
fn external_cursor_control_suppresses_renderer_visibility_changes() {
    let mut renderer = StreamRenderer::new(
        ReasoningDisplayMode::Summary,
        ToolCallDisplayMode::Summary,
        false,
        true,
        10,
    );
    renderer.use_external_cursor_control();

    renderer.hide_cursor().unwrap();
    assert!(!renderer.cursor_hidden);
    renderer.cursor_hidden = true;
    renderer.show_cursor().unwrap();
    assert!(renderer.cursor_hidden);
}

#[test]
fn pending_summary_reasoning_does_not_add_a_leading_newline_on_finish() {
    assert!(!stream_needs_terminating_newline(
        Some(ChatStreamKind::Reasoning),
        ReasoningDisplayMode::Summary,
    ));
    assert!(stream_needs_terminating_newline(
        Some(ChatStreamKind::Reasoning),
        ReasoningDisplayMode::Full,
    ));
    assert!(stream_needs_terminating_newline(
        Some(ChatStreamKind::Content),
        ReasoningDisplayMode::Summary,
    ));
}

#[test]
fn finish_keeps_pending_reasoning_summary_state() {
    let mut renderer = StreamRenderer::new(
        ReasoningDisplayMode::Summary,
        ToolCallDisplayMode::Summary,
        false,
        true,
        10,
    );
    renderer.reasoning_title = Some("检查摘要状态".to_string());
    renderer.reasoning_text = "some reasoning".to_string();
    renderer.reasoning_started_at = Some(std::time::Instant::now());
    renderer.finish().unwrap();
    assert!(renderer.reasoning_text.is_empty());
    assert!(renderer.reasoning_title.is_none());
    assert!(renderer.reasoning_started_at.is_none());
    assert!(!renderer.summary_line_active);
}

#[test]
fn reasoning_summary_counts_tokens_and_uses_title() {
    let mut renderer = StreamRenderer::new(
        ReasoningDisplayMode::Summary,
        ToolCallDisplayMode::Summary,
        false,
        true,
        10,
    );
    renderer.record_reasoning_text("one\nt");
    renderer.record_reasoning_text("wo\nthree");
    renderer.reasoning_title = Some("分析摘要协议".to_string());
    // 词元数按 chunk 增量累加(避免每 chunk 对全文 O(n²) 重算),
    // 期望值即各 chunk 估算之和;跨 chunk 切词处与全文重算略有出入。
    let expected = crate::token_estimate::estimate_tokens("one\nt")
        + crate::token_estimate::estimate_tokens("wo\nthree");
    let summary = renderer.reasoning_summary_text();
    let title_separator = t(": ", "：");
    assert!(summary.starts_with(&format!(
        "{}{title_separator}分析摘要协议 · ",
        t("thinking", "思考")
    )));
    assert!(summary.contains(&format!("{expected} {}", t("tokens", "词元"))));
    assert!(!summary.contains("字符"));
    assert!(!summary.contains(" 行"));
}

#[test]
fn reasoning_without_title_still_estimates_summary_tokens() {
    let mut renderer = StreamRenderer::new(
        ReasoningDisplayMode::Summary,
        ToolCallDisplayMode::Summary,
        true,
        true,
        10,
    );
    renderer
        .start_reasoning_phase(std::time::Instant::now())
        .unwrap();
    renderer.record_reasoning_text("Plain summary content without a title.");

    let expected = crate::token_estimate::estimate_tokens(&renderer.reasoning_text);
    let live = renderer.waiting_phase_text();
    assert!(live.starts_with(&format!("{} · ", t("thinking", "思考"))));
    assert!(live.contains(&format!("{expected} {}", t("tokens", "词元"))));
}

#[test]
fn reasoning_part_end_commits_state_and_starts_next_timer_at_boundary() {
    let mut renderer = StreamRenderer::new(
        ReasoningDisplayMode::Summary,
        ToolCallDisplayMode::Summary,
        false,
        true,
        10,
    );
    let started_at = std::time::Instant::now();
    let ended_at = started_at + std::time::Duration::from_millis(750);
    renderer.start_reasoning_phase(started_at).unwrap();
    renderer.reasoning_title = Some("检查当前阶段".to_string());
    renderer.record_reasoning_text("summary body");

    renderer.finish_reasoning_part(ended_at).unwrap();

    assert!(renderer.reasoning_title.is_none());
    assert!(renderer.reasoning_text.is_empty());
    assert_eq!(renderer.reasoning_tokens, 0);
    assert_eq!(renderer.reasoning_started_at, Some(ended_at));
    assert!(renderer.reasoning_elapsed.is_none());
}

#[test]
fn new_reasoning_part_starts_a_fresh_timer_and_estimate() {
    let mut renderer = StreamRenderer::new(
        ReasoningDisplayMode::Summary,
        ToolCallDisplayMode::Summary,
        false,
        true,
        10,
    );
    let started_at = std::time::Instant::now();
    let next_part_at = started_at + std::time::Duration::from_millis(900);
    renderer.start_reasoning_phase(started_at).unwrap();
    renderer.reasoning_title = Some("上一阶段".to_string());
    renderer.record_reasoning_text("old body");

    renderer.start_reasoning_part(next_part_at).unwrap();

    assert!(renderer.reasoning_title.is_none());
    assert!(renderer.reasoning_text.is_empty());
    assert_eq!(renderer.reasoning_tokens, 0);
    assert_eq!(renderer.reasoning_started_at, Some(next_part_at));
    assert!(renderer.reasoning_elapsed.is_none());
}

#[test]
fn frozen_reasoning_elapsed_ignores_renderer_processing_delay() {
    let mut renderer = StreamRenderer::new(
        ReasoningDisplayMode::Summary,
        ToolCallDisplayMode::Summary,
        true,
        true,
        10,
    );
    let started_at = std::time::Instant::now() - std::time::Duration::from_secs(30);
    renderer.reasoning_started_at = Some(started_at);
    renderer.freeze_reasoning_elapsed_at(started_at + std::time::Duration::from_millis(1_500));
    renderer.reasoning_title = Some("检查事件排队".to_string());

    assert_eq!(
        renderer.reasoning_elapsed,
        Some(std::time::Duration::from_millis(1_500))
    );
    assert!(renderer.reasoning_summary_text().ends_with(" · 1.5s"));
}

#[test]
fn reasoning_live_text_updates_title_tokens_and_precise_elapsed_time() {
    let mut renderer = StreamRenderer::new(
        ReasoningDisplayMode::Summary,
        ToolCallDisplayMode::Summary,
        true,
        true,
        10,
    );
    renderer.reasoning_title = Some("The user is asking \"你确定\"".to_string());
    renderer.record_reasoning_text("Inspecting the current implementation.");
    renderer.reasoning_started_at =
        Some(std::time::Instant::now() - std::time::Duration::from_millis(11_700));

    let expected = crate::token_estimate::estimate_tokens(&renderer.reasoning_text);
    let title_separator = t(": ", "：");
    assert_eq!(
        renderer.reasoning_live_text(),
        format!(
            "{}{title_separator}The user is asking \"你确定\" · {expected} {} · 11.7s",
            t("thinking", "思考"),
            t("tokens", "词元")
        )
    );
}

#[test]
fn reasoning_title_is_not_truncated_at_forty_characters() {
    let mut renderer = StreamRenderer::new(
        ReasoningDisplayMode::Summary,
        ToolCallDisplayMode::Summary,
        false,
        true,
        10,
    );
    renderer.live_summary = false;
    let title = "a".repeat(60);

    renderer.write_reasoning_title(&title).unwrap();

    assert_eq!(renderer.reasoning_title.as_deref(), Some(title.as_str()));
}

#[test]
fn reasoning_elapsed_uses_milliseconds_then_decimal_seconds() {
    assert_eq!(format_reasoning_elapsed(std::time::Duration::ZERO), "<1ms");
    assert_eq!(
        format_reasoning_elapsed(std::time::Duration::from_nanos(1)),
        "<1ms"
    );
    assert_eq!(
        format_reasoning_elapsed(std::time::Duration::from_millis(38)),
        "38ms"
    );
    assert_eq!(
        format_reasoning_elapsed(std::time::Duration::from_millis(976)),
        "976ms"
    );
    assert_eq!(
        format_reasoning_elapsed(std::time::Duration::from_millis(11_700)),
        "11.7s"
    );
}

#[test]
fn reasoning_phase_starts_as_neutral_waiting_without_content() {
    let mut renderer = StreamRenderer::new(
        ReasoningDisplayMode::Summary,
        ToolCallDisplayMode::Summary,
        true,
        true,
        10,
    );

    renderer
        .start_reasoning_phase(std::time::Instant::now() - std::time::Duration::from_millis(1_200))
        .unwrap();

    assert!(renderer.reasoning_title.is_none());
    assert_eq!(renderer.waiting_phase_text(), "1.2s");
    assert!(!renderer.waiting_phase_text().contains("思考"));
    assert!(!renderer.waiting_phase_text().contains("词元"));
}

#[test]
fn preparing_question_phase_overrides_reasoning_timer_until_handoff() {
    let mut renderer = StreamRenderer::new(
        ReasoningDisplayMode::Summary,
        ToolCallDisplayMode::Summary,
        true,
        true,
        10,
    );
    renderer.reasoning_started_at =
        Some(std::time::Instant::now() - std::time::Duration::from_secs(30));
    renderer.preparing_question_started_at =
        Some(std::time::Instant::now() - std::time::Duration::from_millis(1_200));

    let phase = renderer.waiting_phase_text();

    assert!(phase.starts_with(t("~ Preparing question · ", "~ 准备问题 · ")));
    assert!(phase.ends_with("1.2s"));
    renderer.prepare_for_external_output().unwrap();
    assert!(renderer.preparing_question_started_at.is_none());
}

#[test]
fn tool_preparing_announces_every_slow_argument_tool() {
    let phase_for = |name: &str| {
        let mut renderer = StreamRenderer::new(
            ReasoningDisplayMode::Summary,
            ToolCallDisplayMode::Summary,
            false,
            true,
            10,
        );
        renderer.use_external_cursor_control();
        renderer.use_buffered_output();
        // No TTY under test, so the spinner degrades to a summary line —
        // which is gated on the same flag a real terminal would set.
        renderer.live_summary = true;
        renderer.write_tool_preparing(name).unwrap();
        String::from_utf8_lossy(&renderer.take_output_frame()).into_owned()
    };

    // apply_artifact_patch used to fall through the label match and render
    // nothing even though the backend announced it.
    for name in ["apply_patch", "apply_artifact_patch", "write_file"] {
        let phase = phase_for(name);
        assert!(
            phase.contains(t("~ Preparing edit", "~ 准备编辑")),
            "{name}"
        );
        // Dim tool palette, not the green the model's thinking uses: a
        // tool is starting up here.
        assert!(phase.contains("\x1b[2m"), "{name}");
        assert!(!phase.contains("\x1b[38;5;10m"), "{name}");
    }
    assert!(phase_for("run_command").contains(t("~ Preparing command", "~ 准备执行")));
    assert!(phase_for("trash_path").contains(t("~ Preparing delete", "~ 准备删除")));
    assert!(phase_for("read_file").is_empty());
}

/// Regression: the hint above is announced mid-turn, when a reasoning
/// spinner is already up and earlier tools have filled `tool_stats`. Every
/// tick re-derives the phase from renderer state, so pushing the text into
/// the spinner was not enough — the tool summary overwrote it inside the
/// very `tick_spinner` that `ensure_waiting_phase` performs, and the hint
/// never reached the screen for anything except `ask_question` (which has
/// its own sticky flag).
#[test]
fn tool_preparing_survives_the_tick_that_re_derives_the_phase() {
    let mut renderer = StreamRenderer::new(
        ReasoningDisplayMode::Summary,
        ToolCallDisplayMode::Summary,
        false,
        true,
        10,
    );
    renderer.use_external_cursor_control();
    renderer.use_buffered_output();
    renderer.live_summary = true;
    // Mid-turn state: the model has been thinking and already ran a tool.
    renderer.reasoning_started_at =
        Some(std::time::Instant::now() - std::time::Duration::from_secs(30));
    renderer.tool_stats_entry("read_file").calls += 1;

    renderer.write_tool_preparing("run_command").unwrap();
    assert!(renderer
        .waiting_phase_text()
        .starts_with(t("~ Preparing command · ", "~ 准备执行 · ")));
    renderer.last_tick = None;
    renderer.tick_spinner().unwrap();
    assert!(
        renderer
            .waiting_phase_text()
            .contains(t("Preparing command", "准备执行")),
        "a tick must not hand the spinner back to the tool summary"
    );

    // The arguments arrived: the hint steps aside for the tool summary.
    renderer.write_tool_call("run_command", "{}").unwrap();
    assert!(renderer.tool_preparing.is_none());
    assert!(!renderer
        .waiting_phase_text()
        .contains(t("Preparing command", "准备执行")));
}

#[test]
fn buffered_output_returns_complete_frames_without_terminal_queries() {
    let mut renderer = StreamRenderer::new(
        ReasoningDisplayMode::Hidden,
        ToolCallDisplayMode::Hidden,
        true,
        true,
        10,
    );
    renderer.use_external_cursor_control();
    renderer.use_buffered_output();
    renderer
        .write_chunk(ChatStreamChunk {
            kind: ChatStreamKind::Content,
            text: "hello".to_string(),
        })
        .unwrap();

    assert_eq!(renderer.take_output_frame(), b"hello");
    assert!(renderer.take_output_frame().is_empty());

    renderer.finish().unwrap();
    let frame = renderer.take_output_frame();
    assert_eq!(frame, b"\n");
    assert!(!frame.windows(5).any(|bytes| bytes == b"?2026"));
    assert!(!frame.windows(3).any(|bytes| bytes == b"[6n"));
}

#[test]
fn full_reasoning_waiting_phase_is_empty() {
    let renderer = StreamRenderer::new(
        ReasoningDisplayMode::Full,
        ToolCallDisplayMode::Summary,
        true,
        true,
        10,
    );

    assert!(renderer.waiting_phase_text().is_empty());
}

#[test]
fn keeps_identifier_underscores_literal() {
    let output = render_inline("GTK_IM_MODULE and _italic_");
    assert!(output.contains("GTK_IM_MODULE"));
    assert!(output.contains(&format!("{ITALIC_STYLE}italic{RESET}")));
    assert!(!output.contains("GTK\x1b[3mIM\x1b[0mMODULE"));
    assert_eq!(render_inline("abc_def_ghi"), "abc_def_ghi");
}

#[test]
fn renders_math_formulas_visibly() {
    let output = render_inline("inline $E=mc^2$ and display $$a^2+b^2=c^2$$");
    assert!(output.contains("E=mc²"), "{output}");
    assert!(output.contains("a²+b²=c²"), "{output}");
    assert!(!output.contains("$E"), "raw tex must be replaced: {output}");
}

#[test]
fn renders_multiline_math_blocks_visibly() {
    let mut renderer = MarkdownStreamRenderer::new();
    let output = renderer.push("$$\na^2 + b^2 = c^2\n$$\n");
    assert!(
        output.contains('▀') || output.contains('▄'),
        "block math should render halfblocks: {output}"
    );
    assert!(!output.contains("a^2"), "{output}");
}

#[test]
fn renders_selected_inline_html_tags() {
    let output = render_inline("<u>under</u> H<sub>2</sub> x<sup>2</sup><br>next");
    assert!(output.contains("\x1b[4munder\x1b[0m"));
    assert!(output.contains("H\x1b[2m2\x1b[0m"));
    assert!(output.contains("x\x1b[1m2\x1b[0m"));
    assert!(output.contains("\nnext"));
}

#[test]
fn horizontal_rule_uses_terminal_width_fallback() {
    let output = render_markdown_line("---");
    assert!(output.starts_with("\x1b[2m"));
    assert!(output.ends_with("\x1b[0m"));
    assert!(visible_width(&output) >= 16);
}

#[test]
fn supports_table_alignment_markers() {
    let mut renderer = MarkdownStreamRenderer::new();
    let output = renderer.push("| left | mid | right |\n| :--- | :---: | ---: |\n| a | b | c |\n");
    let output = format!("{output}{}", renderer.flush());
    assert!(output.contains('┌'));
    assert!(output.contains('│'));
    assert!(!output.contains('+'));
    assert!(!output.contains(":---"));
    assert!(output.contains(&format!("{BOLD_STYLE}left{RESET}")));
}

#[test]
fn does_not_buffer_plain_lines_with_pipes_as_tables() {
    let mut renderer = MarkdownStreamRenderer::new();
    let output = renderer.push("echo hi | wc -l\nnext\n");
    assert!(output.contains("echo hi | wc -l\nnext\n"));
}

#[test]
fn parses_command_result_json() {
    let result = parse_command_result(
        r#"{"success":false,"exit_code":1,"stdout":"unused","stderr":"not found"}"#,
    )
    .unwrap();
    assert!(!result.success);
    assert_eq!(result.exit_code, Some(1));
    assert_eq!(result.stdout, "unused");
    assert_eq!(result.stderr, "not found");
}

#[cfg(test)]
mod math_stream_tests {
    use super::*;

    fn render_document(document: &str) -> String {
        let mut renderer = MarkdownLineRenderer::new();
        let mut output = String::new();
        for line in document.lines() {
            output.push_str(&renderer.render_line(line));
        }
        output.push_str(&renderer.flush());
        output
    }

    #[test]
    fn block_math_renders_to_halfblocks_and_inline_transliterates() {
        let document =
            "推导如下:\n$$\nE = mc^2\n$$\n其中 $\\alpha\\in(0,1)$,价格 $5 和 $10 不动。\n";
        let output = render_document(document);
        assert!(
            output.contains('▀') || output.contains('▄'),
            "block math should render halfblocks"
        );
        assert!(
            output.contains("α∈(0,1)"),
            "inline math should transliterate: {output}"
        );
        assert!(output.contains("$5"), "prices must stay literal");
        assert!(!output.contains("mc^2"), "raw tex should be replaced");
    }

    #[test]
    fn table_cells_render_stacked_fractions() {
        let document = "| 方法 | 收敛阶 |\n| --- | --- |\n| 牛顿法 | $q=2$ |\n| 割线法 | $q=\\frac{1+\\sqrt5}{2}$ |\n\n";
        let output = render_document(document);
        assert!(output.contains("q=2"), "{output}");
        assert!(output.contains("1+√5"), "分子应独立成行: {output}");
        assert!(output.contains("───"), "分数线应存在: {output}");
        assert!(!output.contains("\\frac"), "{output}");
    }

    #[test]
    fn unclosed_math_block_replays_verbatim_on_flush() {
        let output = render_document("$$\nE=mc^2\n");
        assert!(output.contains("$$"));
        assert!(output.contains("E=mc^2"));
    }

    #[test]
    fn single_line_display_math_renders() {
        let output = render_document("$$E=mc^2$$\n");
        assert!(output.contains('▀') || output.contains('▄'), "{output}");
    }

    /// 检视产物:整段 markdown 渲染输出落盘,供 ANSI→PNG 回显人工核看。
    #[test]
    #[ignore]
    fn dump_stream_preview() {
        let document = "偏导与分式:\n\n| 名称 | 表达式 |\n| --- | --- |\n| 偏导数 | $\\frac{\\partial f}{\\partial x}=\\lim_{h\\to 0}\\frac{f(x+h,y)-f(x,y)}{h}$ |\n| 二次方程 | $x=\\frac{-b\\pm\\sqrt{b^2-4ac}}{2a}$ |\n| 组合数 | $\\binom{n}{k}=\\frac{n!}{k!(n-k)!}$ |\n| 波函数 | $i\\hbar\\frac{\\partial}{\\partial t}\\Psi=\\hat{H}\\Psi$ |\n| 极限 | $\\lim_{x\\to\\infty}(1+1/x)^x=e$ |\n\n完事～\n";
        let output = render_document(document);
        std::fs::write("/tmp/claude-1000/math-stream.ansi", output).unwrap();
    }
}
