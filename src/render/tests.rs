//! tests — 自 src/render/mod.rs 外移。
#![cfg(test)]

pub(crate) use super::*;

use unicode_width::UnicodeWidthStr;
#[cfg(test)]

fn visible_command_lines(lines: Vec<String>) -> Vec<String> {
    lines
        .into_iter()
        .map(|line| strip_ansi_for_test(&line))
        .collect()
}

#[test]
fn full_reasoning_reapplies_color_for_every_chunk() {
    let mut green = Vec::new();
    execute!(green, SetForegroundColor(Color::Green)).unwrap();
    let green = String::from_utf8(green).unwrap();
    let mut output = Vec::new();

    write_full_reasoning_chunk(&mut output, "用户").unwrap();
    execute!(output, ResetColor).unwrap();
    write_full_reasoning_chunk(&mut output, "询问明天几号").unwrap();

    let output = String::from_utf8(output).unwrap();
    assert_eq!(output.matches(&green).count(), 2);
    assert!(output.ends_with("询问明天几号"));
}

#[test]
fn command_stream_handles_split_utf8_and_crlf() {
    let mut state = CommandStreamState::default();
    let text = "开始\r\n完成\n".as_bytes();
    let split = "开始".len() - 1;

    assert!(state.push(&text[..split], 1).is_empty());
    let completed = state.push(&text[split..], 2);

    assert_eq!(completed.len(), 2);
    assert_eq!(completed[0].text, "开始");
    assert_eq!(completed[1].text, "完成");
    assert!(state.current.is_empty());
}

#[test]
fn command_stream_carriage_return_replaces_current_line() {
    let mut state = CommandStreamState::default();

    assert!(state.push(b"progress 10%\r", 1).is_empty());
    let completed = state.push(b"progress 20%\n", 2);

    assert_eq!(completed.len(), 1);
    assert_eq!(completed[0].text, "progress 20%");
}

#[test]
fn command_stream_strips_split_terminal_sequences() {
    let mut state = CommandStreamState::default();

    assert!(state.push(b"safe\x1b[31", 1).is_empty());
    let completed = state.push(b"m red\x1b[0m\n", 2);

    assert_eq!(completed.len(), 1);
    assert_eq!(completed[0].text, "safe red");
}

#[test]
fn command_stream_finalizes_incomplete_utf8() {
    let mut state = CommandStreamState::default();

    assert!(state.push(&[0xe4, 0xb8], 1).is_empty());
    state.finalize_pending(1);

    assert_eq!(state.current, "�");
}

#[test]
fn command_text_strips_cursor_and_osc_sequences() {
    assert_eq!(
        sanitize_terminal_text("safe\x1b[2J text\x1b]52;c;secret\x07 end"),
        "safe text end"
    );
    assert_eq!(sanitize_terminal_text("a\x1b(Bb"), "ab");
}

#[test]
fn command_wrap_uses_terminal_width_for_wide_graphemes() {
    assert_eq!(wrap_plain_text("中文测试", 4), vec!["中文", "测试"]);
    assert_eq!(wrap_plain_text("a👨‍👩‍👧‍👦b", 3), vec!["a👨‍👩‍👧‍👦", "b"]);
    assert_eq!(wrap_plain_text("e\u{301}x", 1), vec!["e\u{301}", "x"]);
}

#[test]
fn display_width_clip_preserves_graphemes_and_reserves_last_column() {
    assert_eq!(clip_to_display_width("中文测试", 5), "中文…");
    assert_eq!(clip_to_display_width("a👨‍👩‍👧‍👦bc", 4), "a👨‍👩‍👧‍👦…");
    assert_eq!(clip_to_display_width("e\u{301}x", 2), "e\u{301}x");

    for columns in [20, 40, 80] {
        let lines = transient_summary_lines(&format!("思考：{}", "中文".repeat(80)), columns);
        assert_eq!(lines.len(), 1);
        assert!(UnicodeWidthStr::width(lines[0].as_str()) < columns);
    }
}

#[test]
fn command_preview_limits_physical_rows_and_keeps_tail() {
    let mut display = CommandLiveDisplay::new(r#"{"command":"demo"}"#, 3, true, false);
    display.push(CommandOutputStream::Stdout, b"one\ntwo\nthree\nfour\n");

    let lines = visible_command_lines(display.rendered_log_lines(80));

    assert_eq!(lines.len(), 3);
    assert!(lines[0].contains("omitted") || lines[0].contains("省略"));
    assert!(lines[1].ends_with("three"));
    assert!(lines[2].ends_with("four"));
}

#[test]
fn command_preview_counts_soft_wrapped_rows() {
    let mut display = CommandLiveDisplay::new(r#"{"command":"demo"}"#, 3, true, false);
    display.push(
        CommandOutputStream::Stdout,
        "第一行很长\n第二行\n".as_bytes(),
    );

    let lines = visible_command_lines(display.rendered_log_lines(4));

    assert_eq!(lines.len(), 3);
    assert!(lines[0].contains("omitted") || lines[0].contains("省略"));
    assert!(lines[1].ends_with("第二"));
    assert!(lines[2].ends_with("行"));
}

#[test]
fn command_preview_orders_interleaved_streams_and_colors_stderr() {
    let mut display = CommandLiveDisplay::new(r#"{"command":"demo"}"#, 4, true, false);
    display.push(CommandOutputStream::Stdout, b"out");
    display.push(CommandOutputStream::Stderr, b"err");

    let lines = display.rendered_log_lines(80);

    assert!(strip_ansi_for_test(&lines[0]).ends_with("out"));
    assert!(strip_ansi_for_test(&lines[1]).ends_with("err"));
    assert!(lines[0].contains("\x1b[2mout\x1b[0m"));
    assert!(!lines[0].contains("\x1b[33m"));
    assert!(lines[1].contains("\x1b[2m\x1b[31merr\x1b[0m"));
    assert!(lines[1].contains("\x1b[31m"));
}

#[test]
fn shared_command_output_preview_sanitizes_and_keeps_tail() {
    let mut output = CommandOutputTail::new(3);
    output.push(
        CommandOutputStream::Stdout,
        b"old\nprogress 10%\rprogress 20%\n",
    );
    output.push(CommandOutputStream::Stderr, b"\x1b[31mwarning\x1b[0m\n");
    let chinese = "完成".as_bytes();
    output.push(CommandOutputStream::Stdout, &chinese[..2]);
    output.push(CommandOutputStream::Stdout, &chinese[2..]);

    let preview = output.preview();

    assert!(preview.omitted);
    assert_eq!(preview.lines.len(), 3);
    assert_eq!(preview.lines[0].text, "progress 20%");
    assert_eq!(preview.lines[1].stream, "stderr");
    assert_eq!(preview.lines[1].text, "warning");
    assert_eq!(preview.lines[2].text, "完成");
}

#[test]
fn shared_command_output_preview_can_be_disabled() {
    let mut output = CommandOutputTail::new(0);
    output.push(CommandOutputStream::Stdout, b"hidden\n");

    let preview = output.preview();

    assert!(preview.lines.is_empty());
    assert!(!preview.omitted);
}

#[test]
fn command_heading_is_part_of_live_block_and_updates_status() {
    let mut display = CommandLiveDisplay::new(r#"{"command":"printf ok"}"#, 2, true, false);
    let running = visible_command_lines(display.rendered_lines(80, true));
    let command = t("run command", "运行命令");
    assert_eq!(
        running[0],
        format!("$ {command}×1 {}", t("running", "运行中"))
    );
    assert!(running[1].contains("printf ok"));

    display.set_result(true);
    let completed = visible_command_lines(display.rendered_lines(80, false));
    assert_eq!(completed[0], format!("$ {command}×1 ok"));
    assert_eq!(
        completed
            .iter()
            .filter(|line| line.starts_with(&format!("$ {command}")))
            .count(),
        1
    );
}

#[test]
fn compact_multiline_command_keeps_two_head_and_four_tail_lines() {
    let command = (1..=10)
        .map(|line| format!("command line {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    let arguments = serde_json::json!({ "command": command }).to_string();
    let display = CommandLiveDisplay::new(&arguments, 0, false, false);

    let lines = visible_command_lines(display.rendered_lines(120, false));

    assert_eq!(lines.len(), 8);
    assert!(lines[1].starts_with("  ↳ ") && lines[1].ends_with("command line 1"));
    assert!(lines[2].starts_with("  │ ") && lines[2].ends_with("command line 2"));
    assert!(lines[3].contains('4'));
    assert!(lines[3].contains("omitted") || lines[3].contains("省略"));
    assert!(lines[4].ends_with("command line 7"));
    assert!(lines[5].ends_with("command line 8"));
    assert!(lines[6].ends_with("command line 9"));
    assert!(lines[7].starts_with("  └ ") && lines[7].ends_with("command line 10"));
    assert!(!lines.iter().any(|line| line.ends_with("command line 3")));
    assert!(!lines.iter().any(|line| line.ends_with("command line 6")));
}

#[test]
fn full_multiline_command_keeps_every_logical_line() {
    let command = (1..=10)
        .map(|line| format!("command line {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    let arguments = serde_json::json!({ "command": command }).to_string();
    let display = CommandLiveDisplay::new(&arguments, 0, false, true);

    let lines = visible_command_lines(display.rendered_lines(120, false));

    assert_eq!(lines.len(), 11);
    assert!(lines.iter().any(|line| line.ends_with("command line 3")));
    assert!(lines.iter().any(|line| line.ends_with("command line 6")));
    assert!(!lines
        .iter()
        .any(|line| line.contains("omitted") || line.contains("省略")));
}

#[test]
fn multiline_command_soft_wraps_with_continuation_prefix() {
    let arguments = serde_json::json!({
        "command": "1234567890abcdef\nlast"
    })
    .to_string();
    let display = CommandLiveDisplay::new(&arguments, 0, false, false);

    let lines = visible_command_lines(display.rendered_lines(16, false));

    assert_eq!(lines[1], "  ↳ 123456789");
    assert_eq!(lines[2], "  │   0abcdef");
    assert_eq!(lines[3], "  └ last");
}

#[test]
fn final_multiline_command_wrap_closes_tree_on_last_physical_row() {
    let arguments = serde_json::json!({
        "command": "first\n1234567890abcdef"
    })
    .to_string();
    let display = CommandLiveDisplay::new(&arguments, 0, false, false);

    let lines = visible_command_lines(display.rendered_lines(16, false));

    assert_eq!(lines[2], "  │ 123456789");
    assert_eq!(lines[3], "  └   0abcdef");
}

#[test]
fn omitted_command_notice_wraps_within_narrow_width() {
    let command = (1..=10)
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");

    let lines = render_command_preview(&command, 12, false, false, 0);

    assert!(lines.iter().all(|line| command_ansi_width(line) <= 12));
    assert!(visible_command_lines(lines)
        .iter()
        .any(|line| line.contains('4')));
}

#[test]
fn static_full_command_block_shows_multiline_body() {
    let arguments = serde_json::json!({
        "command": "first\nsecond\nthird\nfourth\nfifth\nsixth\nseventh"
    })
    .to_string();
    let mut output = Vec::new();

    write_command_block_with_status(&mut output, &arguments, CommandStatus::Ok).unwrap();

    let output = strip_ansi_for_test(&String::from_utf8(output).unwrap());
    assert!(output.contains("  │ third\n"));
    assert!(output.contains("  └ seventh\n"));
    assert!(!output.contains("omitted") && !output.contains("省略"));
}

#[test]
fn command_display_detects_output_row_growth_before_redraw() {
    let mut display = CommandLiveDisplay::new(r#"{"command":"printf ok"}"#, 3, true, false);
    display.rendered_line_widths = display
        .rendered_lines(80, true)
        .iter()
        .map(|line| command_ansi_width(line))
        .collect();
    assert!(!display.tick_changes_layout_at_width(80));

    display.push(CommandOutputStream::Stdout, b"one\n");

    assert!(display.tick_changes_layout_at_width(80));
}

#[test]
fn committed_command_blocks_end_with_exactly_one_blank_line() {
    let mut live = Vec::new();
    write_command_block_gap(&mut live, false).unwrap();
    assert_eq!(live, b"\n\n");

    let mut already_terminated = Vec::new();
    write_command_block_gap(&mut already_terminated, true).unwrap();
    assert_eq!(already_terminated, b"\n");
}

#[test]
fn run_command_replaces_an_active_tool_summary() {
    let mut renderer = StreamRenderer::new(
        ReasoningDisplayMode::Summary,
        ToolCallDisplayMode::Summary,
        false,
        true,
        10,
    );
    renderer.live_summary = true;
    renderer.summary_line_active = true;
    renderer.summary_lines_active = 1;

    renderer
        .write_tool_call("run_command", r#"{"command":"printf ok"}"#)
        .unwrap();

    assert!(!renderer.summary_line_active);
    assert_eq!(renderer.summary_lines_active, 0);
    assert!(renderer.command_display.is_some());
    assert!(renderer.tool_stats.is_empty());
}

#[test]
fn completed_tools_are_committed_per_call_instead_of_aggregated() {
    let mut renderer = StreamRenderer::new(
        ReasoningDisplayMode::Summary,
        ToolCallDisplayMode::Summary,
        false,
        true,
        10,
    );
    renderer.live_summary = false;

    renderer
        .write_tool_call("web_search", r#"{"query":"first subject"}"#)
        .unwrap();
    assert_eq!(
        renderer.tool_summary_text(),
        format!(
            "~ {}×1 {}\n  ↳ first subject",
            t("Web search", "网络搜索"),
            t("running", "运行中")
        )
    );
    renderer
        .write_tool_result("web_search", true, "{}")
        .unwrap();
    assert!(renderer.tool_stats.is_empty());

    renderer
        .write_tool_call("web_search", r#"{"query":"second subject"}"#)
        .unwrap();
    assert_eq!(
        renderer.tool_summary_text(),
        format!(
            "~ {}×1 {}\n  ↳ second subject",
            t("Web search", "网络搜索"),
            t("running", "运行中")
        )
    );
}

#[test]
fn tool_summary_uses_spinner_and_updates_subagent_elapsed_time() {
    let mut renderer = StreamRenderer::new(
        ReasoningDisplayMode::Summary,
        ToolCallDisplayMode::Summary,
        false,
        true,
        10,
    );
    renderer.live_summary = true;

    renderer
        .write_tool_call(
            "task",
            r#"{"description":"确认工作区环境","prompt":"details"}"#,
        )
        .unwrap();

    assert!(renderer.wait_spinner.is_some());
    assert!(!renderer.summary_line_active);
    assert_eq!(
        renderer.tool_summary_text(),
        format!(
            "~ {}×1 {} · 0s\n  ↳ 确认工作区环境",
            t("Subagent", "子代理"),
            t("running", "运行中")
        )
    );
    renderer.tool_stats.get_mut("task").unwrap().started_at =
        Some(std::time::Instant::now() - std::time::Duration::from_secs(2));
    renderer.tick_spinner().unwrap();
    assert_eq!(
        renderer.tool_summary_text(),
        format!(
            "~ {}×1 {} · 2s\n  ↳ 确认工作区环境",
            t("Subagent", "子代理"),
            t("running", "运行中")
        )
    );
}

#[test]
fn subagent_summary_keeps_current_internal_tool_without_raw_reasoning() {
    let mut renderer = StreamRenderer::new(
        ReasoningDisplayMode::Full,
        ToolCallDisplayMode::Summary,
        false,
        true,
        10,
    );
    renderer.live_summary = false;
    renderer
        .write_tool_call(
            "task",
            r#"{"description":"查询磁盘占用","prompt":"details"}"#,
        )
        .unwrap();
    renderer
        .write_tool_progress("task", "工具 #2：运行命令 · du -sh /home/shorin/* 运行中")
        .unwrap();
    renderer
        .write_tool_progress("task", "__subagent_reasoning__private analysis")
        .unwrap();

    let summary = renderer.tool_summary_text();
    assert!(summary.contains("↳ 查询磁盘占用"));
    assert!(summary.contains("↳ 工具 #2：运行命令 · du -sh /home/shorin/* 运行中"));
    assert!(!summary.contains("private analysis"));
    assert_eq!(renderer.subagent_mode, None);
}

#[test]
fn external_output_clears_every_active_summary_row() {
    let mut renderer = StreamRenderer::new(
        ReasoningDisplayMode::Summary,
        ToolCallDisplayMode::Summary,
        false,
        true,
        10,
    );
    renderer.live_summary = true;
    renderer.summary_line_active = true;
    renderer.summary_lines_active = 2;

    renderer.prepare_for_external_output().unwrap();

    assert!(!renderer.summary_line_active);
    assert_eq!(renderer.summary_lines_active, 0);
}

#[test]
fn streams_only_complete_lines() {
    let mut renderer = MarkdownStreamRenderer::new();
    assert_eq!(renderer.push("**bo"), "");
    assert_eq!(
        renderer.push("ld**\n"),
        format!("{BOLD_STYLE}bold{RESET}\n")
    );
}

#[test]
fn flushes_partial_final_line() {
    let mut renderer = MarkdownStreamRenderer::new();
    assert_eq!(renderer.push("# Title"), "");
    assert_eq!(renderer.flush(), format!("{HEADER_STYLE}# Title{RESET}\n"));
}

#[test]
fn headings_use_one_color_and_distinct_prefix_lengths() {
    assert_eq!(
        render_markdown_line("# One"),
        format!("{HEADER_STYLE}# One{RESET}")
    );
    assert_eq!(
        render_markdown_line("## Two"),
        format!("{HEADER_STYLE}## Two{RESET}")
    );
    assert_eq!(
        render_markdown_line("### Three"),
        format!("{HEADER_STYLE}### Three{RESET}")
    );
    assert_eq!(
        render_markdown_line("###### Six"),
        format!("{HEADER_STYLE}###### Six{RESET}")
    );
}

#[test]
fn list_markers_use_tertiary_color() {
    assert!(render_markdown_line("- item").contains(&format!("{TERTIARY_STYLE}-{RESET}")));
    assert!(render_markdown_line("1. item").contains(&format!("{TERTIARY_STYLE}1.{RESET}")));
}

#[test]
fn token_usage_hides_zero_turn_tokens() {
    assert_eq!(
        format_token_usage_inline(&TokenMeter {
            session_tokens: 1_300,
            context_window: Some(272_000),
            ..Default::default()
        }),
        "1.3k/272k(0.5%)"
    );
    assert_eq!(
        format_token_usage_inline(&TokenMeter {
            turn_tokens: 1_300,
            session_tokens: 1_300,
            context_window: Some(272_000),
            ..Default::default()
        }),
        "1.3k · 1.3k/272k(0.5%)"
    );
    assert_eq!(
        format_token_usage_inline(&TokenMeter {
            turn_tokens: 5_300,
            session_tokens: 10_000,
            context_window: Some(200_000),
            cumulative_tokens: Some(86_200),
            ..Default::default()
        }),
        "5.3k · 10k/200k(5.0%) · Σ86.2k"
    );
}

#[test]
fn a_cache_rate_divides_by_the_prompt_not_the_whole_turn() {
    // 24.8k turn = 12.0k prompt + 12.8k output, 11.2k of the prompt cached.
    // Dividing by the turn total would report 45% and would sag further the
    // longer the model talked, which says nothing about the cache.
    let meter = TokenMeter {
        turn_tokens: 24_800,
        turn_prompt_tokens: 12_000,
        turn_cached_tokens: 11_200,
        session_tokens: 12_000,
        context_window: Some(200_000),
        cumulative_tokens: Some(380_000),
        cumulative_prompt_tokens: 248_000,
        cumulative_cached_tokens: 226_000,
    };
    assert_eq!(
        format_token_usage_inline(&meter),
        "24.8k(C93%) · 12k/200k(6.0%) · Σ380k(C91%)"
    );
}

#[test]
fn a_provider_that_reports_no_cache_shows_no_rate() {
    // Turns recorded before the cache columns existed read as zeros; a flat
    // "C0%" would be a claim the database cannot support.
    let meter = TokenMeter {
        turn_tokens: 5_300,
        turn_prompt_tokens: 4_000,
        session_tokens: 10_000,
        context_window: Some(200_000),
        cumulative_tokens: Some(86_200),
        cumulative_prompt_tokens: 70_000,
        ..Default::default()
    };
    assert_eq!(
        format_token_usage_inline(&meter),
        "5.3k · 10k/200k(5.0%) · Σ86.2k"
    );
}

#[test]
fn buffers_tables_until_non_table_line() {
    let mut renderer = MarkdownStreamRenderer::new();
    assert_eq!(renderer.push("| a | b |\n"), "");
    assert_eq!(renderer.push("| - | - |\n"), "");
    let output = renderer.push("| 1 | 2 |\n");
    assert!(output.contains(&format!("{BOLD_STYLE}a{RESET}")));
    assert!(output.contains("1"));
    assert!(output.contains('┌'));
    assert!(output.contains('┬'));
    assert!(output.contains('├'));
    assert!(output.contains('┼'));
    assert!(output.contains("\x1b[2m│\x1b[0m"));
    assert!(output.contains('─'));
    assert!(!output.contains('+'));
    let output = renderer.push("done\n");
    assert!(output.contains('└'));
    assert!(output.ends_with("done\n"));
}

#[test]
fn short_tables_use_content_width() {
    let output = render_table(&[
        "| 项目 | 内容 |".to_string(),
        "|---|---|".to_string(),
        "| 名字 | 未有 / GQY |".to_string(),
        "| 年龄 | 18 |".to_string(),
    ]);
    let terminal_width = terminal::size()
        .map(|(width, _)| usize::from(width))
        .unwrap_or(100);
    let widest = output.lines().map(visible_width).max().unwrap_or(0);
    assert!(widest < terminal_width / 2, "table too wide: {widest}");
}

#[test]
fn todo_output_uses_single_column_rendered_table() {
    let output = render_todo_table(&[
        "| #Todo |".to_string(),
        "|---|".to_string(),
        "| [·] 修复 todo 表格渲染 |".to_string(),
        "| [ ] 补充单元测试 |".to_string(),
        "| [✔] 跑 cargo test |".to_string(),
    ]);
    let visible = strip_ansi_for_test(&output);
    assert!(output.contains('┌'));
    assert!(output.contains('├'));
    assert!(output.contains('└'));
    assert!(!output.contains('┬'));
    assert!(!output.contains('┼'));
    assert!(!output.contains('┴'));
    assert!(visible.contains("#Todo"));
    assert!(!output.contains(&format!("{BOLD_STYLE}#Todo{RESET}")));
    assert_eq!(visible.matches('│').count(), 8);
    assert!(visible.contains("[·]"));
    assert!(visible.contains("todo"));
    assert!(visible.contains("[ ]"));
    assert!(visible.contains("[✔]"));
    assert!(!visible.contains("优先级"));
    assert!(!visible.contains("序号"));
    let terminal_width = terminal::size()
        .map(|(width, _)| usize::from(width))
        .unwrap_or(100);
    for line in output.lines() {
        assert!(
            visible_width(line) < terminal_width,
            "line too wide: {line}"
        );
    }
}

#[test]
fn todo_status_symbols_contribute_to_table_width() {
    assert_eq!(visible_width("把冰箱门打开"), 12);
    assert_eq!(visible_width("[✔] 把冰箱门打开"), 16);
    assert_eq!(visible_width("[·] 把冰箱门打开"), 16);

    let lines = [
        "| #Todo |".to_string(),
        "|---|".to_string(),
        "| [✔] 把冰箱门打开 |".to_string(),
        "| [·] 把冰箱门关上 |".to_string(),
    ];
    let normal = render_table(&lines);
    let output = render_todo_table(&lines);
    let visible = strip_ansi_for_test(&output);
    assert_eq!(
        visible_width(output.lines().next().unwrap()),
        visible_width(normal.lines().next().unwrap())
    );
    assert!(!output.contains(&format!("{BOLD_STYLE}#Todo{RESET}")));
    assert!(visible.contains("[✔]"));
    assert!(visible.contains("[·]"));
    assert_eq!(visible.lines().filter(|line| line.contains('│')).count(), 3);
}

#[test]
fn patch_diff_uses_muted_change_backgrounds() {
    let diff = "--- a/demo.txt\n+++ b/demo.txt\n@@ -1,1 +1,1 @@\n-old\n+new\n";
    let output = render_patch_diff("demo.txt", diff);

    assert!(output.contains("\x1b[48;2;60;41;53m"));
    assert!(output.contains("\x1b[48;2;32;52;67m"));
    assert!(!output.contains("\x1b[48;5;52m"));
    assert!(!output.contains("\x1b[48;5;22m"));
}

#[test]
fn patch_diff_wraps_long_lines_with_aligned_gutter() {
    let diff = format!(
        "--- a/run-vm.sh\n+++ b/run-vm.sh\n@@ -1,0 +1,1 @@\n+{}\n",
        "RESULT=$(system_profiler SPHardwareDataType --macbook ".repeat(8)
    );
    let output = render_patch_diff("run-vm.sh", &diff);
    let visible = strip_ansi_for_test(&output);
    let diff_lines = visible
        .lines()
        .filter(|line| line.contains('│'))
        .collect::<Vec<_>>();
    assert!(diff_lines.len() > 1, "diff line was not wrapped: {visible}");
    assert!(diff_lines[0].starts_with("    1 + │ "));
    assert!(diff_lines[1].starts_with("        │ "));

    let terminal_width = terminal::size()
        .map(|(width, _)| usize::from(width))
        .unwrap_or(100);
    for line in output.lines().filter(|line| line.contains('│')) {
        assert!(
            visible_width(line) < terminal_width,
            "diff line too wide: {line}"
        );
    }
}

#[test]
fn patch_diff_wraps_wide_character_lines() {
    let diff = format!(
        "--- a/demo.txt\n+++ b/demo.txt\n@@ -1,0 +1,1 @@\n+{}\n",
        "软换行问题".repeat(30)
    );
    let output = render_patch_diff("demo.txt", &diff);
    let visible = strip_ansi_for_test(&output);
    assert!(visible.lines().filter(|line| line.contains('│')).count() > 1);

    let terminal_width = terminal::size()
        .map(|(width, _)| usize::from(width))
        .unwrap_or(100);
    for line in output.lines().filter(|line| line.contains('│')) {
        assert!(
            visible_width(line) < terminal_width,
            "wide-char diff line too wide: {line}"
        );
    }
}

pub(crate) fn strip_ansi_for_test(input: &str) -> String {
    let mut output = String::new();
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' && chars.peek() == Some(&'[') {
            chars.next();
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
            continue;
        }
        output.push(ch);
    }
    output
}

#[test]
fn wraps_wide_table_cells_to_terminal_width() {
    let output = render_table(&[
        "| 项目 | 内容 |".to_string(),
        "|---|---|".to_string(),
        format!("| 很长 | {} |", "这是一段非常长的内容".repeat(20)),
    ]);
    let terminal_width = terminal::size()
        .map(|(width, _)| usize::from(width))
        .unwrap_or(100);
    for line in output.lines() {
        assert!(
            visible_width(line) < terminal_width,
            "line too wide: {line}"
        );
    }
    assert!(output.lines().count() > 5);
}

#[test]
fn many_column_tables_stay_within_terminal_width() {
    let output = render_table(&[
        "| 参数名 | 参数类型 | 默认值 | 是否必填 | 说明 | 取值范围 | 示例值 | 适用版本 | 更新日志 | 备注 |".to_string(),
        "|---|---|---|---|---|---|---|---|---|---|".to_string(),
        "| database_host | string | localhost | 否 | 数据库主机地址 | 合法IP或域名 | 192.168.1.100 | v1.0+ | 无 | 支持IPv6 |".to_string(),
    ]);
    let terminal_width = terminal::size()
        .map(|(width, _)| usize::from(width))
        .unwrap_or(100);
    for line in output.lines() {
        assert!(
            visible_width(line) < terminal_width,
            "line too wide: {line}"
        );
    }
}

#[test]
fn blockquote_is_visually_distinct() {
    let mut renderer = MarkdownStreamRenderer::new();
    let output = renderer.push(">> quoted\n");
    assert!(output.contains("\x1b[32m| \x1b[0m\x1b[32m| \x1b[0m"));
    assert!(output.contains("\x1b[32mquoted\x1b[0m"));
    assert!(!output.contains("48;5;236"));
}

#[test]
fn code_block_has_label_and_readable_content() {
    let mut renderer = MarkdownStreamRenderer::new();
    let output = renderer.push("```rust\nfn main() {}\n```\n");
    assert!(output.contains("╭─ code rust"));
    assert!(!output.contains(",-- code rust"));
    assert!(!output.contains("\x1b[2m|\x1b[0m"));
    assert!(output.contains(&format!(
        "{CODE_BLOCK_BG}{CODE_KEYWORD_STYLE}fn{CODE_TOKEN_RESET}"
    )));
    assert!(output.contains(&format!("{CODE_FUNCTION_STYLE}main{CODE_TOKEN_RESET}")));
    assert!(output.contains(&format!("{CODE_BLOCK_FRAME_STYLE}╭─ code rust ─")));
    assert!(output.contains(&format!(
        "{CODE_BLOCK_FRAME_STYLE}{}{RESET}",
        "─".repeat(24)
    )));
    assert!(!output.contains("`--"));
}

#[test]
fn code_block_content_has_default_color() {
    let mut renderer = MarkdownStreamRenderer::new();
    let output =
        renderer.push("```\ndefaults write NSGlobalDomain AppleKeyboardUIMode -int 3\n```\n");
    assert!(output.contains(&format!(
        "{CODE_BLOCK_BG}defaults write NSGlobalDomain AppleKeyboardUIMode -int 3{}{RESET}",
        " ".repeat(2)
    )));
    assert!(!output.contains("\x1b[33mdefaults"));
}

#[test]
fn code_block_variables_use_primary_color() {
    let mut renderer = MarkdownStreamRenderer::new();
    let output = renderer.push("```rust\nlet msg = String::from(\"hi\");\n```\n");
    assert!(output.contains(&format!("{PRIMARY_STYLE}msg{CODE_TOKEN_RESET}")));
}

#[test]
fn code_block_background_uses_longest_line_width() {
    let mut renderer = MarkdownStreamRenderer::new();
    let output = renderer.push("```\nshort\nlonger line\n```\n");
    assert!(output.contains(&format!("{CODE_BLOCK_BG}short{}{RESET}", " ".repeat(19))));
    assert!(output.contains(&format!(
        "{CODE_BLOCK_BG}longer line{}{RESET}",
        " ".repeat(13)
    )));
    assert!(output.contains(&format!(
        "{CODE_BLOCK_FRAME_STYLE}{}{RESET}",
        "─".repeat(24)
    )));
    assert!(!output.contains("48;5;236"));
}

#[test]
fn renders_more_inline_markdown() {
    let output = render_inline(
        "*i* ~~gone~~ [site](https://example.com) <https://example.org> ![pic](https://img)",
    );
    assert!(output.contains(&format!("{ITALIC_STYLE}i{RESET}")));
    assert!(output.contains(&format!("{STRIKE_STYLE}gone{RESET}")));
    assert!(output.contains(&format!("<{URL_STYLE}https://example.com{RESET}>")));
    assert!(output.contains(&format!(
        "\x1b[4m<{URL_STYLE}https://example.org{RESET}>{RESET}"
    )));
    assert!(output.contains(&format!(
        "{IMAGE_STYLE}[image: pic]{RESET}({URL_STYLE}https://img{RESET})"
    )));
    assert!(!output.contains("\x1b[35mimage\x1b[0m"));
}

#[test]
fn renders_inline_code_at_start_of_bullet() {
    let output = render_markdown_line("- `read_file` — 读文件内容");
    assert!(output.contains(&format!("{INLINE_CODE_STYLE}read_file\x1b[0m")));
    assert!(output.contains("— 读文件内容"));
}

#[test]
fn renders_multiple_inline_code_spans_in_bullet_with_chinese_text() {
    let output = render_markdown_line(
        "- `~/.config/Thunar/` - 里面有 `accels.scm`（快捷键绑定）和 `uca.xml`（自定义右键菜单）",
    );
    assert!(output.contains(&format!("{INLINE_CODE_STYLE}~/.config/Thunar/\x1b[0m")));
    assert!(output.contains(&format!("{INLINE_CODE_STYLE}accels.scm\x1b[0m")));
    assert!(output.contains(&format!("{INLINE_CODE_STYLE}uca.xml\x1b[0m")));
    assert!(!output.contains('`'));
}

#[test]
fn renders_inline_code_when_stream_chunks_split_backticks() {
    let mut renderer = MarkdownStreamRenderer::new();
    assert_eq!(renderer.push("- `~/.config/Thu"), "");
    let output = renderer.push("nar/` - 里面有 `accels.scm`\n");
    assert!(output.contains(&format!("{INLINE_CODE_STYLE}~/.config/Thunar/\x1b[0m")));
    assert!(output.contains(&format!("{INLINE_CODE_STYLE}accels.scm\x1b[0m")));
    assert!(!output.contains('`'));
}

#[test]
fn tool_status_prefers_running_for_single_active_call() {
    let stats = ToolStats {
        calls: 1,
        ok: 0,
        error: 0,
        subject: None,
        progress: None,
        final_progress: None,
        ..ToolStats::default()
    };
    assert_eq!(
        tool_status_text("grep", &stats, false),
        format!("grep×1 {}", t("running", "运行中"))
    );
}

#[test]
fn tool_status_uses_simple_single_success() {
    let stats = ToolStats {
        calls: 1,
        ok: 1,
        error: 0,
        subject: None,
        progress: None,
        final_progress: None,
        ..ToolStats::default()
    };
    assert_eq!(tool_status_text("grep", &stats, false), "grep×1 ok");
}

#[test]
fn detached_subagents_drop_the_meaningless_elapsed_timer() {
    let finished = ToolStats {
        calls: 1,
        ok: 1,
        elapsed: Some(std::time::Duration::from_secs(12)),
        ..ToolStats::default()
    };
    assert_eq!(
        tool_status_text("子代理", &finished, true),
        "子代理×1 ok · 12s"
    );

    // Handing off to the background returns immediately, so the timer only
    // ever read `0s` — which looked like the work had finished instantly.
    let detached = ToolStats {
        calls: 1,
        ok: 1,
        elapsed: Some(std::time::Duration::from_millis(3)),
        detached: true,
        ..ToolStats::default()
    };
    assert_eq!(tool_status_text("子代理", &detached, true), "子代理×1 ok");
}

#[test]
fn tool_status_subagent_tool_keeps_count_suffix() {
    let stats = ToolStats {
        calls: 1,
        ok: 0,
        error: 0,
        subject: None,
        progress: None,
        final_progress: None,
        ..ToolStats::default()
    };
    assert_eq!(
        tool_status_text("deep_research", &stats, true),
        format!("deep_research×1 {}", t("running", "运行中"))
    );
    let stats = ToolStats {
        calls: 1,
        ok: 1,
        error: 0,
        subject: None,
        progress: None,
        final_progress: None,
        ..ToolStats::default()
    };
    assert_eq!(
        tool_status_text("deep_research", &stats, true),
        "deep_research×1 ok"
    );
}

#[test]
fn subagent_status_shows_live_and_frozen_elapsed_time() {
    let running = ToolStats {
        calls: 1,
        started_at: Some(std::time::Instant::now() - std::time::Duration::from_secs(68)),
        ..ToolStats::default()
    };
    assert_eq!(
        tool_status_text("task", &running, true),
        format!("task×1 {} · 1m 08s", t("running", "运行中"))
    );
    assert_eq!(
        tool_status_text("task", &running, false),
        format!("task×1 {}", t("running", "运行中"))
    );

    let completed = ToolStats {
        calls: 1,
        ok: 1,
        elapsed: Some(std::time::Duration::from_secs(3_720)),
        ..ToolStats::default()
    };
    assert_eq!(
        tool_status_text("deep_research", &completed, true),
        "deep_research×1 ok · 1h 02m"
    );
}

#[test]
fn elapsed_time_formats_seconds_minutes_and_hours() {
    assert_eq!(format_elapsed(std::time::Duration::from_secs(5)), "5s");
    assert_eq!(format_elapsed(std::time::Duration::from_secs(65)), "1m 05s");
    assert_eq!(
        format_elapsed(std::time::Duration::from_secs(7_380)),
        "2h 03m"
    );
}

#[test]
fn full_mode_subagent_result_uses_elapsed_status_and_clears_timer() {
    let mut renderer = StreamRenderer::new(
        ReasoningDisplayMode::Summary,
        ToolCallDisplayMode::Full,
        false,
        true,
        10,
    );
    renderer.live_summary = false;
    renderer
        .write_tool_call("task", r#"{"description":"计时","prompt":"details"}"#)
        .unwrap();
    renderer.tool_stats.get_mut("task").unwrap().started_at =
        Some(std::time::Instant::now() - std::time::Duration::from_secs(5));

    renderer.write_tool_result("task", true, "{}").unwrap();

    assert!(!renderer.tool_stats.contains_key("task"));
    assert_eq!(
        tool_result_status("ok", Some(std::time::Duration::from_secs(5))),
        "ok · 5s"
    );
}
