//! tests — 自 src/platforms/plugins/renderer.rs 外移。
#![cfg(test)]

pub(crate) use super::*;

use std::path::PathBuf;
use std::time::Duration;
#[allow(clippy::empty_line_after_outer_attr)]
#[cfg(test)]

fn render(markdown: &str, raw_config: &RenderConfig) -> Result<Vec<RenderedImage>> {
    render_in_process_for_test(markdown, raw_config)
}

#[test]
fn renderer_client_and_payloads_satisfy_async_bounds() {
    fn assert_send_static<T: Send + 'static>() {}
    fn assert_renderer<T: Clone + Send + Sync + 'static>() {}
    assert_send_static::<RenderConfig>();
    assert_send_static::<RenderedImage>();
    assert_renderer::<MarkdownImageRenderer>();
}

#[test]
fn renderer_client_is_lazy_and_limits_are_bounded() {
    let renderer = MarkdownImageRenderer::new().unwrap();
    assert!(renderer.worker.try_lock().unwrap().process.is_none());
    assert_eq!(MAX_CACHED_GLYPHS, 2048);
    assert_eq!(WORKER_IDLE_TIMEOUT, Duration::from_secs(60 * 60));
    assert_eq!(RENDER_TIMEOUT, Duration::from_secs(60));
    // debug 二进制 550MB+,光映射自身就撞 512MB;开发构建放宽。
    #[cfg(not(debug_assertions))]
    assert_eq!(WORKER_ADDRESS_SPACE_LIMIT, 512 * 1024 * 1024);
    #[cfg(debug_assertions)]
    assert_eq!(WORKER_ADDRESS_SPACE_LIMIT, 2 * 1024 * 1024 * 1024);
}

#[test]
fn renderer_loads_only_the_fonts_needed_by_the_request() {
    let font_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/fonts");
    let mut renderer = RendererState::from_font_dir(&font_dir).unwrap();
    let config = NormalizedConfig::new(&RenderConfig::default());

    let fonts = renderer.resolve_config_fonts(&config, false).unwrap();
    assert!(fonts.emoji.is_none());
    assert!(!renderer.emoji_loaded);
    let cjk_face_count = {
        let faces = renderer.font_system.db().faces().collect::<Vec<_>>();
        assert!(!faces.is_empty());
        assert!(faces.iter().all(|face| matches!(
            &face.source,
            fontdb::Source::File(path) if *path == font_dir.join(CJK_FONT_FILE)
        )));
        let families = faces
            .iter()
            .flat_map(|face| face.families.iter().map(|(name, _)| name.as_str()))
            .collect::<Vec<_>>();
        assert!(families.contains(&DEFAULT_BODY_FONT));
        assert!(families.contains(&DEFAULT_CODE_FONT));
        assert!(!families.contains(&DEFAULT_EMOJI_FONT));
        faces.len()
    };

    let fonts = renderer.resolve_config_fonts(&config, true).unwrap();
    assert_eq!(fonts.emoji.as_deref(), Some(DEFAULT_EMOJI_FONT));
    assert!(renderer.emoji_loaded);
    let with_emoji = renderer.font_system.db().faces().count();
    assert!(with_emoji > cjk_face_count);
    renderer.ensure_bundled_emoji_font().unwrap();
    assert_eq!(renderer.font_system.db().faces().count(), with_emoji);
}

#[test]
fn missing_emoji_font_does_not_block_text_only_requests() {
    let font_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/fonts");
    let mut renderer = RendererState::from_font_dir(&font_dir).unwrap();
    renderer.emoji_font_path = font_dir.join("missing-emoji.ttf");
    let config = NormalizedConfig::new(&RenderConfig::default());

    assert!(renderer.resolve_config_fonts(&config, false).is_ok());
    let error = renderer
        .resolve_config_fonts(&config, true)
        .err()
        .expect("missing Emoji font should fail only Emoji requests");
    assert!(error.to_string().contains("missing-emoji.ttf"));
}

#[test]
fn emoji_detection_only_marks_emoji_graphemes() {
    assert!(!markdown_contains_emoji("纯中文 and `code`"));
    assert!(markdown_contains_emoji("完成 ✅"));
    assert!(markdown_contains_emoji("family 👨‍👩‍👧‍👦"));
}

#[tokio::test]
async fn worker_binary_response_round_trips_without_base64() {
    let expected_png = b"\x89PNG\r\n\x1a\nworker".to_vec();
    let (mut worker_side, mut client_side) = tokio::io::duplex(1024);
    let write = write_worker_response(
        &mut worker_side,
        Ok(vec![RenderedImage {
            mime: "image/png".to_string(),
            png: expected_png.clone(),
            width: 960,
            height: MIN_RENDERED_HEIGHT,
        }]),
    );
    let read = read_worker_response(&mut client_side);
    let (write_result, read_result) = tokio::join!(write, read);
    write_result.unwrap();
    let images = read_result.unwrap();
    assert_eq!(images.len(), 1);
    assert_eq!(images[0].mime, "image/png");
    assert_eq!(images[0].png, expected_png);
    assert_eq!(images[0].width, 960);
    assert_eq!(images[0].height, MIN_RENDERED_HEIGHT);
}

#[tokio::test]
async fn request_frames_enforce_the_input_budget() {
    let (mut writer, mut reader) = tokio::io::duplex(64);
    let payload = b"request";
    let (write_result, read_result) = tokio::join!(
        write_frame(&mut writer, payload),
        read_frame(&mut reader, MAX_REQUEST_FRAME_BYTES)
    );
    write_result.unwrap();
    assert_eq!(read_result.unwrap().unwrap(), payload);
    assert!(write_frame(
        &mut tokio::io::sink(),
        &vec![0; MAX_REQUEST_FRAME_BYTES + 1]
    )
    .await
    .is_err());
}

#[test]
fn renders_supported_markdown_and_unicode_to_nonempty_png() {
    let markdown = r#"# GQY 长回复 🚀

普通中文段落，包含 **粗体**、*斜体*、`inline code` 和 [链接文字](https://example.com)。

> 引用内容支持中文和 Emoji 😀。

- 第一项
- 第二项

1. ordered one
2. ordered two

```rust
fn main() {
println!("hello");
}
```

| 名称 | 状态 |
| --- | --- |
| renderer | ready |

---

结束。"#;
    let pages = render(markdown, &RenderConfig::default()).unwrap();
    assert_eq!(pages.len(), 1);
    for page in pages {
        assert_eq!(page.mime, "image/png");
        assert!(page.png.starts_with(b"\x89PNG\r\n\x1a\n"));
        assert!((MIN_RENDERED_HEIGHT..=MAX_PAGE_HEIGHT).contains(&page.height));
        assert!(u64::from(page.width) * u64::from(page.height) <= MAX_PAGE_PIXELS);
        let decoded = image::load_from_memory(&page.png).unwrap().to_rgba8();
        assert_eq!(decoded.dimensions(), (page.width, page.height));
        let background = decoded.get_pixel(0, 0);
        assert!(decoded.pixels().any(|pixel| pixel != background));
    }
}

#[test]
fn freshly_shaped_cjk_word_keeps_positive_advances() {
    // cosmic-text 0.15 在冷字体系统上首次整词塑形时,"背景"的首字形
    // advance 为 0,后续字形全部叠画在同一位置(0.19 修复)。锁死该回归:
    // 任何字形 advance 归零都会让文字叠加。
    let mut font_system = FontSystem::new();
    for text in ["背景", "背 景", "背包"] {
        let metrics = Metrics::new(36.0, 52.0);
        let mut buffer = Buffer::new(&mut font_system, metrics);
        buffer.set_size(Some(960.0), None);
        let attrs = Attrs::new().family(Family::SansSerif).metrics(metrics);
        buffer.set_rich_text([(text, attrs.clone())], &attrs, Shaping::Advanced, None);
        buffer.shape_until_scroll(&mut font_system, true);
        for run in buffer.layout_runs() {
            for glyph in run.glyphs {
                assert!(
                    glyph.w > 0.0,
                    "{text:?} 中 start={} 的字形 advance 为 0",
                    glyph.start
                );
            }
        }
    }
}

#[test]
fn inline_code_gets_a_background_chip() {
    let count_chip_pixels = |markdown: &str| {
        let pages = render(markdown, &RenderConfig::default()).unwrap();
        let decoded = image::load_from_memory(&pages[0].png).unwrap().to_rgba8();
        let palette = Palette::for_theme(&RenderConfig::default().theme);
        decoded
            .pixels()
            .filter(|pixel| **pixel == Rgba(palette.code_background))
            .count()
    };
    let with_code = count_chip_pixels("行内 `code` 提示,以及 `第二段代码` 也要有底色。");
    let without_code = count_chip_pixels("行内 code 提示,没有任何反引号的对照段落。");
    assert!(
        with_code > 200,
        "行内代码应有底色块,实际命中 {with_code} 像素"
    );
    assert_eq!(without_code, 0, "无行内代码时不应出现底色像素");
}

#[test]
fn input_limit_is_measured_in_unicode_characters() {
    let accepted = "界".repeat(MAX_INPUT_CHARS);
    let rejected = "界".repeat(MAX_INPUT_CHARS + 1);
    assert!(validate_markdown(&accepted).is_ok());
    assert!(validate_markdown(&rejected).is_err());
}

#[test]
fn total_pixel_and_png_writers_enforce_hard_budgets() {
    assert_eq!(
        checked_total_page_pixels(MAX_TOTAL_PAGE_PIXELS - 1, 1).unwrap(),
        MAX_TOTAL_PAGE_PIXELS
    );
    assert!(checked_total_page_pixels(MAX_TOTAL_PAGE_PIXELS, 1).is_err());

    let mut writer = CappedVecWriter::new(3);
    writer.write_all(b"abc").unwrap();
    assert!(writer.write_all(b"d").is_err());
    assert!(writer.exceeded());
    assert_eq!(writer.into_inner(), b"abc");
}

#[test]
fn html_only_output_is_not_rendered_as_a_blank_page() {
    let blocks = collect_blocks("<div>visible</div>");
    assert!(blocks
        .iter()
        .any(|block| { block.spans.iter().any(|span| span.text.contains("visible")) }));
}

#[test]
fn fenced_configuration_keeps_heading_markers_inside_code() {
    let markdown = r#"下面是 Niri 配置：

```kdl
input {
focus-follows-mouse
keyboard { mod-key "Mod1" }
}
```

Kitty 透明度：

```conf
# ~/.config/kitty/kitty.conf
background_opacity 0.92
dynamic_background_opacity yes
```
"#;
    let blocks = collect_blocks(markdown);
    let code_blocks = blocks
        .iter()
        .filter(|block| block.kind == BlockKind::Code)
        .collect::<Vec<_>>();

    assert_eq!(code_blocks.len(), 2);
    let code_text = code_blocks
        .iter()
        .flat_map(|block| &block.spans)
        .map(|span| span.text.as_str())
        .collect::<String>();
    assert!(code_text.contains("focus-follows-mouse"));
    assert!(code_text.contains("# ~/.config/kitty/kitty.conf"));
    assert!(!blocks.iter().any(|block| {
        matches!(block.kind, BlockKind::Heading(_))
            && block
                .spans
                .iter()
                .any(|span| span.text.contains("kitty.conf"))
    }));
}

#[test]
fn code_surface_remains_distinct_after_qq_sized_downscale() {
    let markdown = r#"正文内容用于对比代码块。

```kdl
# ~/.config/kitty/kitty.conf
background_opacity 0.92
```

代码块之后的正文。"#;
    let raw_config = RenderConfig::default();
    let config = NormalizedConfig::new(&raw_config);
    let palette = Palette::for_theme("paper");
    let mut renderer = RendererState::new().unwrap();
    let fonts = renderer.resolve_config_fonts(&config, false).unwrap();
    let layouts = layout_blocks(
        &mut renderer.font_system,
        collect_blocks(markdown),
        &config,
        palette,
        &fonts,
    )
    .unwrap();
    let code_index = layouts
        .iter()
        .position(|block| block.kind == BlockKind::Code)
        .expect("fenced block should use the code layout");
    let columns = plan_columns(&layouts, &config).unwrap();
    let placement = columns
        .iter()
        .flat_map(|column| &column.placements)
        .find(|placement| placement.block_index == code_index)
        .expect("code block placement");
    let code_y = config.padding + placement.y;
    let sample_y = code_y + (placement.source_end - placement.source_start) / 2;

    let page = render(markdown, &raw_config).unwrap().remove(0);
    let image = image::load_from_memory(&page.png).unwrap().to_rgba8();
    let outside_x = config.padding / 2;
    let inside_x = config.padding + COLUMN_WIDTH - 12;
    assert_eq!(
        *image.get_pixel(outside_x, sample_y),
        Rgba(palette.background)
    );
    assert_eq!(
        *image.get_pixel(inside_x, sample_y),
        Rgba(palette.code_background)
    );
    assert_eq!(
        *image.get_pixel(config.padding, sample_y),
        Rgba(palette.code_background)
    );

    let scaled_width = 568_u32;
    let scaled_height =
        (u64::from(page.height) * u64::from(scaled_width) / u64::from(page.width)).max(1) as u32;
    let scaled = image::imageops::resize(
        &image,
        scaled_width,
        scaled_height,
        image::imageops::FilterType::Triangle,
    );
    let scale_x = |x: u32| (u64::from(x) * u64::from(scaled_width) / u64::from(page.width)) as u32;
    let scale_y =
        |y: u32| (u64::from(y) * u64::from(scaled_height) / u64::from(page.height)) as u32;
    let outside = scaled.get_pixel(scale_x(outside_x), scale_y(sample_y));
    let inside = scaled.get_pixel(scale_x(inside_x), scale_y(sample_y));
    let rgb_distance = (0..3)
        .map(|channel| u32::from(outside[channel].abs_diff(inside[channel])))
        .sum::<u32>();
    assert!(
        rgb_distance > 50,
        "downscaled code surface contrast was only {rgb_distance}"
    );
}

#[test]
fn extreme_config_values_are_clamped_and_missing_fonts_fall_back() {
    let config = RenderConfig {
        theme: "unknown".to_string(),
        max_height: 1,
        font_size: 0,
        code_font_size: u32::MAX,
        padding: u32::MAX,
        font: "/definitely/missing/body.ttf".to_string(),
        title_font: "/definitely/missing/title.ttf".to_string(),
        code_font: "/definitely/missing/code.ttf".to_string(),
        emoji_font: "/definitely/missing/emoji.ttf".to_string(),
    };
    let pages = render("fallback 中文 😀", &config).unwrap();
    assert_eq!(pages.len(), 1);
    assert_eq!(
        NormalizedConfig::new(&config).max_height,
        MIN_CONFIGURED_HEIGHT
    );
    assert_eq!(pages[0].height, MIN_RENDERED_HEIGHT);
    assert!(!pages[0].png.is_empty());
}

#[test]
fn empty_markdown_produces_a_valid_blank_page() {
    let pages = render("", &RenderConfig::default()).unwrap();
    assert_eq!(pages.len(), 1);
    assert_eq!(pages[0].mime, "image/png");
    assert_eq!(pages[0].height, MIN_RENDERED_HEIGHT);
    assert!(image::load_from_memory(&pages[0].png).is_ok());
}

#[test]
fn task_list_markers_are_structured_instead_of_literal_text() {
    let blocks = collect_blocks("- [ ] pending\n- [x] complete\n");
    let tasks = blocks
        .iter()
        .filter_map(|block| block.task.map(|checked| (checked, block)))
        .collect::<Vec<_>>();
    assert_eq!(tasks.len(), 2);
    assert!(!tasks[0].0);
    assert!(tasks[1].0);
    for (_, block) in tasks {
        let text = block
            .spans
            .iter()
            .map(|span| span.text.as_str())
            .collect::<String>();
        assert!(!text.contains('•'));
        assert!(!text.contains("[ ]"));
        assert!(!text.contains("[x]"));
    }
}

#[test]
fn checked_task_box_has_drawn_check_while_empty_box_does_not() {
    let background = [255, 255, 255, 255];
    let foreground = [1, 2, 3, 255];
    let mut unchecked = RgbaImage::from_pixel(40, 40, Rgba(background));
    let mut checked = unchecked.clone();
    draw_checkbox(&mut unchecked, 5, 5, 24, false, foreground);
    draw_checkbox(&mut checked, 5, 5, 24, true, foreground);
    assert_eq!(*unchecked.get_pixel(17, 17), Rgba(background));
    assert!(checked
        .enumerate_pixels()
        .any(|(x, y, pixel)| (9..27).contains(&x)
            && (9..27).contains(&y)
            && *pixel == Rgba(foreground)));
}

#[test]
fn table_parser_preserves_cells_rows_and_alignment() {
    let blocks =
        collect_blocks("| Left | Center | Right |\n| :--- | :---: | ---: |\n| a | b | c |\n");
    let table = blocks
        .iter()
        .find_map(|block| block.table.as_ref())
        .expect("structured table");
    assert_eq!(
        table.alignments,
        vec![Alignment::Left, Alignment::Center, Alignment::Right]
    );
    assert_eq!(table.header.len(), 3);
    assert_eq!(table.rows.len(), 1);
    assert_eq!(table.rows[0].len(), 3);
    assert!(table.header[0].iter().all(|span| span.style.bold));
}

#[test]
fn code_block_uses_remaining_column_space_before_continuing() {
    let mut markdown = String::from("```text\n");
    for line in 0..8 {
        markdown.push_str(&format!("first {line}\n"));
    }
    markdown.push_str("```\n\n```text\n");
    for line in 0..12 {
        markdown.push_str(&format!("second {line}\n"));
    }
    markdown.push_str("```\n");

    let config = NormalizedConfig::new(&RenderConfig {
        max_height: MIN_CONFIGURED_HEIGHT,
        ..RenderConfig::default()
    });
    let mut renderer = RendererState::new().unwrap();
    let fonts = renderer.resolve_config_fonts(&config, false).unwrap();
    let layouts = layout_blocks(
        &mut renderer.font_system,
        collect_blocks(&markdown),
        &config,
        Palette::for_theme("paper"),
        &fonts,
    )
    .unwrap();
    assert_eq!(layouts.len(), 2);
    let columns = plan_balanced_columns(&layouts, &config).unwrap();
    let placement = columns[0]
        .placements
        .iter()
        .find(|placement| placement.block_index == 1)
        .expect("second code block should begin in the first column");
    assert_eq!(placement.source_start, 0);
    assert!(placement.y > 0);
    assert!(placement.source_end < layouts[1].total_height);
}

#[test]
fn table_continuation_repeats_header_and_never_splits_rows() {
    let mut markdown = String::from("| Name | Value |\n| --- | ---: |\n");
    for row in 0..24 {
        markdown.push_str(&format!("| row {row} | {row} |\n"));
    }
    let config = NormalizedConfig::new(&RenderConfig {
        max_height: MIN_CONFIGURED_HEIGHT,
        ..RenderConfig::default()
    });
    let mut renderer = RendererState::new().unwrap();
    let fonts = renderer.resolve_config_fonts(&config, false).unwrap();
    let layouts = layout_blocks(
        &mut renderer.font_system,
        collect_blocks(&markdown),
        &config,
        Palette::for_theme("paper"),
        &fonts,
    )
    .unwrap();
    let table = layouts[0].table.as_ref().unwrap();
    let columns = plan_balanced_columns(&layouts, &config).unwrap();
    assert!(columns.len() > 1);
    for column in columns.iter().skip(1) {
        let header = column.placements.first().expect("repeated table header");
        assert_eq!(header.source_start, 0);
        assert_eq!(header.source_end, table.header_height);
    }
    for placement in columns.iter().flat_map(|column| &column.placements) {
        assert!(
            placement.source_start == 0 || layouts[0].boundaries.contains(&placement.source_start)
        );
        assert!(layouts[0].boundaries.contains(&placement.source_end));
    }
}

#[test]
fn rendered_table_has_grid_header_and_zebra_backgrounds() {
    let markdown = "| A | B |\n| --- | --- |\n| one | two |\n| three | four |\n";
    let raw_config = RenderConfig::default();
    let config = NormalizedConfig::new(&raw_config);
    let palette = Palette::for_theme("paper");
    let mut renderer = RendererState::new().unwrap();
    let fonts = renderer.resolve_config_fonts(&config, false).unwrap();
    let layouts = layout_blocks(
        &mut renderer.font_system,
        collect_blocks(markdown),
        &config,
        palette,
        &fonts,
    )
    .unwrap();
    let table = layouts[0].table.as_ref().unwrap();
    let header = &table.rows[0];
    let first = &table.rows[1];
    let second = &table.rows[2];
    let page = render(markdown, &raw_config).unwrap().remove(0);
    let image = image::load_from_memory(&page.png).unwrap().to_rgba8();
    let x = config.padding + COLUMN_WIDTH - 5;
    assert_eq!(
        *image.get_pixel(x, config.padding + 5),
        Rgba(palette.table_header_background)
    );
    assert_eq!(
        *image.get_pixel(x, config.padding + first.source_start + 5),
        Rgba(palette.table_background)
    );
    assert_eq!(
        *image.get_pixel(x, config.padding + second.source_start + 5),
        Rgba(palette.quote_background)
    );
    let grid_x = config.padding + header.cells[0].width;
    assert_eq!(
        *image.get_pixel(grid_x, config.padding + header.source_end / 2),
        Rgba(palette.border)
    );
}

#[test]
fn long_content_grows_one_image_past_three_columns() {
    let mut markdown = String::from("```text\n");
    for line in 0..70 {
        markdown.push_str(&format!("line {line:02}: rendered column content\n"));
    }
    markdown.push_str("```\n");
    let config = RenderConfig {
        max_height: MIN_CONFIGURED_HEIGHT,
        ..RenderConfig::default()
    };
    let pages = render(&markdown, &config).unwrap();
    assert_eq!(pages.len(), 1);
    let page = &pages[0];
    let old_three_column_width = config.padding * 2 + COLUMN_WIDTH * 3 + COLUMN_GAP * 2;
    assert!(page.width > old_three_column_width);
    assert!((MIN_RENDERED_HEIGHT..=MIN_CONFIGURED_HEIGHT).contains(&page.height));
    // Balancing shares the trailing partial column across all columns, so
    // the finished image no longer stays pinned at the full page height.
    assert!(page.height < NormalizedConfig::new(&config).max_height);
    assert!(u64::from(page.width) * u64::from(page.height) <= MAX_PAGE_PIXELS);
}

fn code_layouts_for_balancing(lines: u32) -> (NormalizedConfig, Vec<LayoutBlock>) {
    let mut markdown = String::from("```text\n");
    for line in 0..lines {
        markdown.push_str(&format!("line {line:02}: rendered column content\n"));
    }
    markdown.push_str("```\n");
    let config = NormalizedConfig::new(&RenderConfig {
        max_height: MIN_CONFIGURED_HEIGHT,
        ..RenderConfig::default()
    });
    let mut renderer = RendererState::new().unwrap();
    let fonts = renderer.resolve_config_fonts(&config, false).unwrap();
    let layouts = layout_blocks(
        &mut renderer.font_system,
        collect_blocks(&markdown),
        &config,
        Palette::for_theme("paper"),
        &fonts,
    )
    .unwrap();
    (config, layouts)
}

#[test]
fn balanced_columns_have_similar_used_heights() {
    let (config, layouts) = code_layouts_for_balancing(70);
    let usable_height = config.max_height - config.padding * 2;
    let greedy = plan_columns(&layouts, &config).unwrap();
    let balanced = plan_balanced_columns(&layouts, &config).unwrap();
    assert!(balanced.len() > 1);
    let heights = |columns: &[ColumnPlan]| {
        let min = columns.iter().map(|c| c.used_height).min().unwrap();
        let max = columns.iter().map(|c| c.used_height).max().unwrap();
        (min, max)
    };
    let (greedy_min, greedy_max) = heights(&greedy);
    let (balanced_min, balanced_max) = heights(&balanced);
    assert!(balanced_max - balanced_min < usable_height * 30 / 100);
    assert!(balanced_max - balanced_min < greedy_max - greedy_min);
}

#[test]
fn balancing_removes_trailing_sliver_column_and_shrinks_height() {
    let (config, layouts) = code_layouts_for_balancing(60);
    let usable_height = config.max_height - config.padding * 2;
    let greedy = plan_columns(&layouts, &config).unwrap();
    let sliver = greedy.last().unwrap().used_height;
    assert!(
        sliver < usable_height / 4,
        "test premise: greedy leaves a nearly empty last column, got {sliver}"
    );
    let balanced = plan_balanced_columns(&layouts, &config).unwrap();
    assert!(balanced.len() > 1);
    let min = balanced.iter().map(|c| c.used_height).min().unwrap();
    let max = balanced.iter().map(|c| c.used_height).max().unwrap();
    assert!(min * 2 >= max, "no column holds under half of the tallest");
    assert!(
        max + config.padding * 2 < config.max_height,
        "balanced image should shrink below the full page height"
    );
}

#[test]
fn documents_over_the_pixel_budget_fail_instead_of_truncating() {
    let mut markdown = String::from("```text\n");
    for _ in 0..500 {
        markdown.push_str("x\n");
    }
    markdown.push_str("```\n");
    let config = RenderConfig {
        max_height: MIN_CONFIGURED_HEIGHT,
        font_size: 56,
        code_font_size: 52,
        padding: 160,
        ..RenderConfig::default()
    };
    let error = render(&markdown, &config).unwrap_err();
    assert!(error.to_string().contains("pixel limit"));
}
