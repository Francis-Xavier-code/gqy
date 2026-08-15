//! handler — 自 src/platforms/plugins/renderer.rs 拆分。

pub(crate) use super::*;

#[derive(Clone, Copy)]
pub(crate) struct TaskBox {
    checked: bool,
    x: u32,
    y: u32,
    size: u32,
}

pub(crate) fn layout_blocks(
    font_system: &mut FontSystem,
    blocks: Vec<Block>,
    config: &NormalizedConfig,
    palette: Palette,
    fonts: &ResolvedFonts,
) -> Result<Vec<LayoutBlock>> {
    blocks
        .into_iter()
        .map(|block| layout_block(font_system, block, config, palette, fonts))
        .collect()
}

pub(crate) fn layout_block(
    font_system: &mut FontSystem,
    block: Block,
    config: &NormalizedConfig,
    palette: Palette,
    fonts: &ResolvedFonts,
) -> Result<LayoutBlock> {
    if block.kind == BlockKind::Rule {
        return Ok(LayoutBlock {
            kind: block.kind,
            buffer: None,
            table: None,
            task: None,
            total_height: 28,
            vertical_padding: 0,
            inset_left: 0,
            boundaries: vec![28],
            margin_before: 20,
            margin_after: 20,
            default_color: color(palette.text),
            inline_code_background: palette.code_background,
        });
    }

    if block.kind == BlockKind::Table {
        return layout_table(
            font_system,
            block
                .table
                .ok_or_else(|| anyhow!("Markdown table is missing its structured rows"))?,
            config,
            palette,
            fonts,
        );
    }

    let (mut inset_left, inset_right, vertical_padding) = block_insets(block.kind);
    let task = block.task.map(|checked| {
        let size = (config.font_size * 3 / 5).clamp(18, 30);
        let marker_x = inset_left.saturating_add(4);
        let marker_y = vertical_padding.saturating_add(
            ((metrics_for(block.kind, InlineStyle::default(), config).line_height as u32)
                .saturating_sub(size))
                / 2,
        );
        inset_left = inset_left.saturating_add(size).saturating_add(16);
        TaskBox {
            checked,
            x: marker_x,
            y: marker_y,
            size,
        }
    });
    let content_width = COLUMN_WIDTH
        .saturating_sub(inset_left)
        .saturating_sub(inset_right)
        .max(64);
    let metrics = metrics_for(block.kind, InlineStyle::default(), config);
    let default_attrs = attrs_for(
        block.kind,
        InlineStyle::default(),
        false,
        metrics,
        palette,
        fonts,
    );
    let expanded = expand_spans(&block.spans, fonts.emoji.is_some());
    let rich_spans = expanded
        .iter()
        .map(|span| {
            let metrics = metrics_for(block.kind, span.style, config);
            let attrs = attrs_for(block.kind, span.style, span.emoji, metrics, palette, fonts);
            (span.text.clone(), attrs)
        })
        .collect::<Vec<_>>();

    let mut buffer = Buffer::new(font_system, metrics);
    buffer.set_size(Some(content_width as f32), None);
    buffer.set_wrap(Wrap::WordOrGlyph);
    buffer.set_rich_text(
        rich_spans
            .iter()
            .map(|(text, attrs)| (text.as_str(), attrs.clone())),
        &default_attrs,
        Shaping::Advanced,
        None,
    );
    buffer.shape_until_scroll(font_system, true);

    let mut boundaries = Vec::new();
    let mut text_height = 1_u32;
    for run in buffer.layout_runs() {
        let bottom = (run.line_top + run.line_height).ceil().max(1.0) as u32;
        text_height = text_height.max(bottom);
        let boundary = vertical_padding.saturating_add(bottom);
        if boundaries.last().copied() != Some(boundary) {
            boundaries.push(boundary);
        }
    }
    let total_height = text_height.saturating_add(vertical_padding.saturating_mul(2));
    if let Some(last) = boundaries.last_mut() {
        *last = total_height;
    } else {
        boundaries.push(total_height);
    }
    let (margin_before, margin_after) = block_margins(block.kind, config.font_size);
    let default_color = if block.kind == BlockKind::Code {
        palette.code_text
    } else {
        palette.text
    };
    Ok(LayoutBlock {
        kind: block.kind,
        buffer: Some(buffer),
        table: None,
        task,
        total_height,
        vertical_padding,
        inset_left,
        boundaries,
        margin_before,
        margin_after,
        default_color: color(default_color),
        inline_code_background: palette.code_background,
    })
}

pub(crate) fn layout_table(
    font_system: &mut FontSystem,
    table: TableBlock,
    config: &NormalizedConfig,
    palette: Palette,
    fonts: &ResolvedFonts,
) -> Result<LayoutBlock> {
    let column_count = table
        .alignments
        .len()
        .max(table.header.len())
        .max(table.rows.iter().map(Vec::len).max().unwrap_or(0));
    if column_count == 0 {
        bail!("Markdown table has no columns");
    }
    let column_count_u32 =
        u32::try_from(column_count).context("too many Markdown table columns")?;
    let base_width = COLUMN_WIDTH / column_count_u32;
    let remainder = COLUMN_WIDTH % column_count_u32;
    if base_width <= TABLE_CELL_PADDING.saturating_mul(2) {
        bail!("Markdown table has too many columns to render safely");
    }

    let mut widths = Vec::with_capacity(column_count);
    for index in 0..column_count_u32 {
        widths.push(base_width + u32::from(index < remainder));
    }

    let mut rows = Vec::with_capacity(table.rows.len().saturating_add(1));
    let mut source_y = 0_u32;
    if !table.header.is_empty() {
        let row = layout_table_row(
            font_system,
            &table.header,
            &table.alignments,
            &widths,
            true,
            false,
            source_y,
            config,
            palette,
            fonts,
        )?;
        source_y = row.source_end;
        rows.push(row);
    }
    let header_height = source_y;
    for (index, cells) in table.rows.iter().enumerate() {
        let row = layout_table_row(
            font_system,
            cells,
            &table.alignments,
            &widths,
            false,
            index % 2 == 1,
            source_y,
            config,
            palette,
            fonts,
        )?;
        source_y = row.source_end;
        rows.push(row);
    }
    let boundaries = rows.iter().map(|row| row.source_end).collect::<Vec<_>>();
    let (margin_before, margin_after) = block_margins(BlockKind::Table, config.font_size);
    Ok(LayoutBlock {
        kind: BlockKind::Table,
        buffer: None,
        table: Some(LayoutTable {
            rows,
            header_height,
        }),
        task: None,
        total_height: source_y,
        vertical_padding: 0,
        inset_left: 0,
        boundaries,
        margin_before,
        margin_after,
        default_color: color(palette.text),
        inline_code_background: palette.code_background,
    })
}

pub(crate) fn layout_table_row(
    font_system: &mut FontSystem,
    cells: &[Vec<RichSpan>],
    alignments: &[Alignment],
    widths: &[u32],
    header: bool,
    stripe: bool,
    source_start: u32,
    config: &NormalizedConfig,
    palette: Palette,
    fonts: &ResolvedFonts,
) -> Result<LayoutTableRow> {
    let metrics = metrics_for(BlockKind::Table, InlineStyle::default(), config);
    let mut x = 0_u32;
    let mut row_height = metrics.line_height.ceil().max(1.0) as u32;
    let mut laid_out = Vec::with_capacity(widths.len());
    for (index, width) in widths.iter().copied().enumerate() {
        let content_width = width.saturating_sub(TABLE_CELL_PADDING.saturating_mul(2));
        let spans = cells.get(index).map(Vec::as_slice).unwrap_or(&[]);
        let alignment = alignments.get(index).copied().unwrap_or(Alignment::None);
        let (buffer, text_height, default_color) = layout_rich_buffer(
            font_system,
            spans,
            BlockKind::Table,
            content_width,
            header,
            alignment,
            config,
            palette,
            fonts,
        );
        row_height = row_height.max(text_height);
        laid_out.push(LayoutTableCell {
            buffer,
            x,
            width,
            default_color,
            inline_code_background: palette.code_background,
        });
        x = x
            .checked_add(width)
            .context("Markdown table width overflowed")?;
    }
    row_height = row_height.saturating_add(TABLE_CELL_PADDING.saturating_mul(2));
    let source_end = source_start
        .checked_add(row_height)
        .context("Markdown table height overflowed")?;
    Ok(LayoutTableRow {
        cells: laid_out,
        source_start,
        source_end,
        header,
        stripe,
    })
}

pub(crate) fn layout_rich_buffer(
    font_system: &mut FontSystem,
    spans: &[RichSpan],
    kind: BlockKind,
    width: u32,
    force_bold: bool,
    alignment: Alignment,
    config: &NormalizedConfig,
    palette: Palette,
    fonts: &ResolvedFonts,
) -> (Buffer, u32, Color) {
    let metrics = metrics_for(kind, InlineStyle::default(), config);
    let default_attrs = attrs_for(
        kind,
        InlineStyle {
            bold: force_bold,
            ..InlineStyle::default()
        },
        false,
        metrics,
        palette,
        fonts,
    );
    let mut expanded = expand_spans(spans, fonts.emoji.is_some());
    if expanded.is_empty() {
        expanded.push(ExpandedSpan {
            text: " ".to_string(),
            style: InlineStyle::default(),
            emoji: false,
        });
    }
    let rich_spans = expanded
        .iter()
        .map(|span| {
            let mut style = span.style;
            style.bold |= force_bold;
            let metrics = metrics_for(kind, style, config);
            let attrs = attrs_for(kind, style, span.emoji, metrics, palette, fonts);
            (span.text.clone(), attrs)
        })
        .collect::<Vec<_>>();
    let alignment = match alignment {
        Alignment::Right => Some(TextAlign::Right),
        Alignment::Center => Some(TextAlign::Center),
        Alignment::Left => Some(TextAlign::Left),
        Alignment::None => None,
    };
    let mut buffer = Buffer::new(font_system, metrics);
    buffer.set_size(Some(width.max(1) as f32), None);
    buffer.set_wrap(Wrap::WordOrGlyph);
    buffer.set_rich_text(
        rich_spans
            .iter()
            .map(|(text, attrs)| (text.as_str(), attrs.clone())),
        &default_attrs,
        Shaping::Advanced,
        alignment,
    );
    buffer.shape_until_scroll(font_system, true);
    let text_height = buffer
        .layout_runs()
        .map(|run| (run.line_top + run.line_height).ceil().max(1.0) as u32)
        .max()
        .unwrap_or_else(|| metrics.line_height.ceil().max(1.0) as u32);
    (buffer, text_height, color(palette.text))
}

#[derive(Clone)]
pub(crate) struct ExpandedSpan {
    text: String,
    style: InlineStyle,
    emoji: bool,
}

pub(crate) fn expand_spans(spans: &[RichSpan], split_emoji: bool) -> Vec<ExpandedSpan> {
    let mut expanded: Vec<ExpandedSpan> = Vec::new();
    for span in spans {
        if !split_emoji {
            expanded.push(ExpandedSpan {
                text: span.text.clone(),
                style: span.style,
                emoji: false,
            });
            continue;
        }
        for grapheme in span.text.graphemes(true) {
            let emoji = grapheme_is_emoji(grapheme);
            if let Some(last) = expanded
                .last_mut()
                .filter(|last| last.style == span.style && last.emoji == emoji)
            {
                last.text.push_str(grapheme);
            } else {
                expanded.push(ExpandedSpan {
                    text: grapheme.to_string(),
                    style: span.style,
                    emoji,
                });
            }
        }
    }
    expanded
}

pub(crate) fn markdown_contains_emoji(markdown: &str) -> bool {
    markdown.graphemes(true).any(grapheme_is_emoji)
}

pub(crate) fn grapheme_is_emoji(grapheme: &str) -> bool {
    grapheme.chars().any(|ch| {
        matches!(
            ch as u32,
            0x1F000..=0x1FAFF
                | 0x2300..=0x23FF
                | 0x2600..=0x27BF
                | 0x2B00..=0x2BFF
                | 0xFE0F
                | 0x200D
        )
    })
}

pub(crate) fn attrs_for<'a>(
    kind: BlockKind,
    style: InlineStyle,
    emoji: bool,
    metrics: Metrics,
    palette: Palette,
    fonts: &'a ResolvedFonts,
) -> Attrs<'a> {
    let named = if emoji {
        fonts.emoji.as_deref()
    } else if style.code || matches!(kind, BlockKind::Code) {
        fonts.code.as_deref()
    } else if matches!(kind, BlockKind::Heading(_)) {
        fonts.title.as_deref().or(fonts.body.as_deref())
    } else {
        fonts.body.as_deref()
    };
    let fallback = if style.code || matches!(kind, BlockKind::Code) {
        Family::Monospace
    } else {
        Family::SansSerif
    };
    let family = named.map(Family::Name).unwrap_or(fallback);
    let foreground = if matches!(kind, BlockKind::Code) {
        palette.code_text
    } else if style.code {
        palette.code_text
    } else if style.link {
        palette.link
    } else if style.muted {
        palette.muted
    } else if matches!(kind, BlockKind::Heading(_)) {
        palette.heading
    } else {
        palette.text
    };
    let mut attrs = Attrs::new()
        .family(family)
        .color(color(foreground))
        .metrics(metrics);
    if style.bold || matches!(kind, BlockKind::Heading(_)) {
        attrs = attrs.weight(Weight::BOLD);
    }
    if style.italic {
        attrs = attrs.style(FontStyle::Italic);
    }
    if style.code && !matches!(kind, BlockKind::Code) {
        // 行内代码经 metadata 传到 LayoutGlyph,绘制时据此画底色小块;
        // 代码块整块已有背景,不标。
        attrs = attrs.metadata(INLINE_CODE_METADATA);
    }
    attrs
}

pub(crate) const INLINE_CODE_METADATA: usize = 1;

/// 一条 layout 行内行内代码字形的连续 x 区间(相邻区间合并)。
pub(crate) fn inline_code_chip_ranges(glyphs: &[LayoutGlyph]) -> Vec<(f32, f32)> {
    let mut ranges: Vec<(f32, f32)> = Vec::new();
    for glyph in glyphs {
        if glyph.metadata != INLINE_CODE_METADATA {
            continue;
        }
        let start = glyph.x;
        let end = glyph.x + glyph.w;
        match ranges.last_mut() {
            Some((_, last_end)) if start - *last_end <= 0.5 => *last_end = end.max(*last_end),
            _ => ranges.push((start, end)),
        }
    }
    ranges
}

/// 行内代码底色块的水平/垂直留白。
pub(crate) const INLINE_CODE_CHIP_PAD_X: f32 = 5.0;
pub(crate) const INLINE_CODE_CHIP_INSET_RATIO: f32 = 0.10;

pub(crate) fn metrics_for(
    kind: BlockKind,
    style: InlineStyle,
    config: &NormalizedConfig,
) -> Metrics {
    let body = config.font_size as f32;
    let code = config.code_font_size as f32;
    let size = match kind {
        BlockKind::Heading(level) => {
            let scale = match level {
                1 => 1.55,
                2 => 1.35,
                3 => 1.20,
                4 => 1.10,
                _ => 1.0,
            };
            (body * scale).min(76.0)
        }
        BlockKind::Code => code,
        BlockKind::Table => (body * 0.92).max(14.0),
        _ if style.code => code,
        _ => body,
    };
    Metrics::new(size, (size * 1.42).ceil())
}

pub(crate) fn block_insets(kind: BlockKind) -> (u32, u32, u32) {
    match kind {
        BlockKind::Code => (32, 32, 24),
        BlockKind::Table => (20, 20, 16),
        BlockKind::Quote => (32, 14, 12),
        BlockKind::ListItem { depth } => {
            (u32::from(depth.saturating_sub(1)).saturating_mul(18), 0, 0)
        }
        _ => (0, 0, 0),
    }
}

pub(crate) fn block_margins(kind: BlockKind, font_size: u32) -> (u32, u32) {
    let small = (font_size / 4).max(6);
    match kind {
        BlockKind::Heading(1) => (font_size, font_size / 2),
        BlockKind::Heading(_) => (font_size / 2, small),
        BlockKind::Code | BlockKind::Table => (font_size / 2, font_size / 2),
        BlockKind::Rule => (font_size / 2, font_size / 2),
        BlockKind::Quote => (small, small),
        BlockKind::ListItem { .. } => (small / 2, small / 2),
        BlockKind::Paragraph => (small, small),
    }
}

#[derive(Default)]
pub(crate) struct ColumnPlan {
    pub(crate) placements: Vec<Placement>,
    pub(crate) used_height: u32,
}

pub(crate) struct Placement {
    pub(crate) block_index: usize,
    pub(crate) source_start: u32,
    pub(crate) source_end: u32,
    pub(crate) y: u32,
}

pub(crate) fn plan_columns(
    layouts: &[LayoutBlock],
    config: &NormalizedConfig,
) -> Result<Vec<ColumnPlan>> {
    let usable_height = config
        .max_height
        .saturating_sub(config.padding.saturating_mul(2));
    plan_columns_with_height(layouts, usable_height)
}

pub(crate) fn plan_columns_with_height(
    layouts: &[LayoutBlock],
    usable_height: u32,
) -> Result<Vec<ColumnPlan>> {
    if usable_height < 128 {
        bail!("page height leaves too little room for rendered content");
    }
    let mut columns = vec![ColumnPlan::default()];

    for (block_index, block) in layouts.iter().enumerate() {
        if let Some(table) = block.table.as_ref() {
            if table.header_height > usable_height {
                bail!("a Markdown table header exceeds the usable image height");
            }
            for row in table.rows.iter().filter(|row| !row.header) {
                let row_height = row.source_end.saturating_sub(row.source_start);
                if table.header_height.saturating_add(row_height) > usable_height {
                    bail!("a Markdown table row exceeds the usable image height");
                }
            }
        }
        let mut source_start = 0;
        let mut first_fragment = true;
        while source_start < block.total_height {
            if source_start > 0 {
                if let Some(table) = block
                    .table
                    .as_ref()
                    .filter(|table| table.header_height > 0 && source_start >= table.header_height)
                {
                    let column = columns
                        .last_mut()
                        .ok_or_else(|| anyhow!("renderer column planner lost its active column"))?;
                    if column.used_height == 0 {
                        column.placements.push(Placement {
                            block_index,
                            source_start: 0,
                            source_end: table.header_height,
                            y: 0,
                        });
                        column.used_height = table.header_height;
                    }
                }
            }
            let column = columns
                .last_mut()
                .ok_or_else(|| anyhow!("renderer column planner lost its active column"))?;
            let margin: u32 = if first_fragment && column.used_height > 0 {
                block.margin_before
            } else {
                0
            };
            let remaining = block.total_height.saturating_sub(source_start);
            let available: u32 = usable_height
                .saturating_sub(column.used_height)
                .saturating_sub(margin);

            if first_fragment && column.used_height > 0 {
                if let Some(table) = block.table.as_ref() {
                    let first_body_height = table
                        .rows
                        .iter()
                        .find(|row| !row.header)
                        .map(|row| row.source_end.saturating_sub(row.source_start))
                        .unwrap_or(0);
                    let first_table_chunk = table.header_height.saturating_add(first_body_height);
                    if first_table_chunk > available && first_table_chunk <= usable_height {
                        push_column(&mut columns)?;
                        continue;
                    }
                }
            }

            if first_fragment
                && block.kind != BlockKind::Code
                && block.total_height <= usable_height
                && remaining > available
                && column.used_height > 0
            {
                push_column(&mut columns)?;
                continue;
            }
            if available == 0 {
                push_column(&mut columns)?;
                continue;
            }

            let limit = source_start.saturating_add(available);
            let source_end = if remaining <= available {
                block.total_height
            } else {
                block
                    .boundaries
                    .iter()
                    .copied()
                    .take_while(|boundary| *boundary <= limit)
                    .last()
                    .unwrap_or(source_start)
            };
            if source_end <= source_start {
                if column.used_height == 0 {
                    bail!("a rendered text line exceeds the usable page height");
                }
                push_column(&mut columns)?;
                continue;
            }

            let y = column.used_height.saturating_add(margin);
            column.placements.push(Placement {
                block_index,
                source_start,
                source_end,
                y,
            });
            column.used_height = y.saturating_add(source_end.saturating_sub(source_start));
            source_start = source_end;
            first_fragment = false;
            if source_start < block.total_height {
                push_column(&mut columns)?;
            } else {
                column.used_height = column
                    .used_height
                    .saturating_add(block.margin_after)
                    .min(usable_height);
            }
        }
    }
    Ok(columns)
}

pub(crate) fn push_column(columns: &mut Vec<ColumnPlan>) -> Result<()> {
    columns
        .len()
        .checked_add(1)
        .context("rendered Markdown column count overflowed")?;
    columns.push(ColumnPlan::default());
    Ok(())
}

/// Plans columns and then rebalances them so multi-column images approach the
/// target aspect ratio instead of leaving a nearly empty trailing column.
///
/// The full-height greedy plan fixes the column-count ceiling `n_max` (and
/// propagates any planning error unchanged). For every candidate column count
/// a binary search finds the smallest usable column height that still fits in
/// that many columns; planner errors or overflowing column counts during the
/// search are treated as "too short" rather than fatal. The candidate whose
/// overall image is closest to `TARGET_ASPECT_RATIO` (log-distance, ties going
/// to fewer columns) wins.
pub(crate) fn plan_balanced_columns(
    layouts: &[LayoutBlock],
    config: &NormalizedConfig,
) -> Result<Vec<ColumnPlan>> {
    let max_usable = config
        .max_height
        .saturating_sub(config.padding.saturating_mul(2));
    let full_plan = plan_columns_with_height(layouts, max_usable)?;
    let column_ceiling = full_plan.len();
    if column_ceiling <= 1 {
        return Ok(full_plan);
    }

    let total_content: u64 = layouts
        .iter()
        .map(|block| u64::from(block.total_height))
        .sum();
    let height_floor = u64::from(
        MIN_RENDERED_HEIGHT
            .saturating_sub(config.padding.saturating_mul(2))
            .max(128),
    );
    let mut best: Option<(Vec<ColumnPlan>, f32)> = None;
    for candidate in 1..=column_ceiling {
        let low = total_content
            .div_ceil(candidate as u64)
            .max(height_floor)
            .min(u64::from(max_usable)) as u32;
        let Some(plan) = balanced_plan_for_count(layouts, candidate, low, max_usable) else {
            continue;
        };
        let distance = aspect_distance(&plan, config);
        let improves = best
            .as_ref()
            .map(|(_, best_distance)| distance + ASPECT_TIE_EPSILON < *best_distance)
            .unwrap_or(true);
        if improves {
            best = Some((plan, distance));
        }
    }
    Ok(best.map(|(plan, _)| plan).unwrap_or(full_plan))
}

/// Binary-searches the smallest usable height in `[low, high]` whose plan fits
/// in at most `target_columns` columns. Returns `None` when even the full
/// height `high` cannot satisfy the target.
pub(crate) fn balanced_plan_for_count(
    layouts: &[LayoutBlock],
    target_columns: usize,
    low: u32,
    high: u32,
) -> Option<Vec<ColumnPlan>> {
    let mut best = match plan_columns_with_height(layouts, high) {
        Ok(plan) if plan.len() <= target_columns => plan,
        _ => return None,
    };
    let mut low = low.min(high);
    let mut high = high;
    while low < high {
        let mid = low + (high - low) / 2;
        match plan_columns_with_height(layouts, mid) {
            Ok(plan) if plan.len() <= target_columns => {
                best = plan;
                high = mid;
            }
            _ => low = mid.saturating_add(1),
        }
    }
    Some(best)
}

/// Log-space distance between the finished image's aspect ratio (using the
/// same width/height rules as `render_pages`) and `TARGET_ASPECT_RATIO`.
pub(crate) fn aspect_distance(columns: &[ColumnPlan], config: &NormalizedConfig) -> f32 {
    let count = columns.len() as u64;
    let width = u64::from(config.padding) * 2
        + u64::from(COLUMN_WIDTH) * count
        + u64::from(COLUMN_GAP) * count.saturating_sub(1);
    let content_height = columns
        .iter()
        .map(|column| column.used_height)
        .max()
        .unwrap_or(0);
    let height = content_height
        .saturating_add(config.padding.saturating_mul(2))
        .clamp(MIN_RENDERED_HEIGHT, config.max_height);
    ((width as f32 / height as f32).ln() - TARGET_ASPECT_RATIO.ln()).abs()
}

pub(crate) fn render_pages(
    font_system: &mut FontSystem,
    swash_cache: &mut SwashCache,
    layouts: &[LayoutBlock],
    columns: &[ColumnPlan],
    config: &NormalizedConfig,
    palette: Palette,
) -> Result<Vec<RenderedImage>> {
    let column_count = u32::try_from(columns.len()).context("too many image columns")?;
    let columns_width = COLUMN_WIDTH
        .checked_mul(column_count)
        .context("rendered image width overflowed")?;
    let gaps_width = COLUMN_GAP
        .checked_mul(column_count.saturating_sub(1))
        .context("rendered image gap width overflowed")?;
    let width = config
        .padding
        .checked_mul(2)
        .and_then(|padding| padding.checked_add(columns_width))
        .and_then(|width| width.checked_add(gaps_width))
        .context("rendered image width overflowed")?;
    let content_height = columns
        .iter()
        .map(|column| column.used_height)
        .max()
        .unwrap_or(0);
    let height = content_height
        .saturating_add(config.padding.saturating_mul(2))
        .clamp(MIN_RENDERED_HEIGHT, config.max_height);
    validate_page_dimensions(width, height)?;
    let pixels = u64::from(width) * u64::from(height);
    checked_total_page_pixels(0, pixels)?;

    let mut image = RgbaImage::from_pixel(width, height, Rgba(palette.background));
    for (column_index, column) in columns.iter().enumerate() {
        let column_index =
            u32::try_from(column_index).context("image column index does not fit in u32")?;
        let column_x = config
            .padding
            .saturating_add(column_index.saturating_mul(COLUMN_WIDTH.saturating_add(COLUMN_GAP)));
        for placement in &column.placements {
            let block = layouts
                .get(placement.block_index)
                .ok_or_else(|| anyhow!("renderer placement references a missing block"))?;
            let destination_y = config.padding.saturating_add(placement.y);
            if block.table.is_some() {
                draw_table_fragment(
                    &mut image,
                    font_system,
                    swash_cache,
                    block,
                    placement,
                    column_x,
                    destination_y,
                    palette,
                );
                continue;
            }
            draw_decoration(
                &mut image,
                block,
                placement,
                column_x,
                destination_y,
                palette,
            );
            draw_text_fragment(
                &mut image,
                font_system,
                swash_cache,
                block,
                placement,
                column_x,
                destination_y,
            );
        }
    }

    let png_limit = MAX_PAGE_PNG_BYTES.min(MAX_TOTAL_PNG_BYTES);
    let mut writer = CappedVecWriter::new(png_limit);
    let encoded = PngEncoder::new(&mut writer).write_image(
        image.as_raw(),
        width,
        height,
        ColorType::Rgba8.into(),
    );
    if let Err(error) = encoded {
        if writer.exceeded() {
            bail!("rendered image exceeds the {png_limit}-byte PNG limit");
        }
        return Err(error).context("failed to encode rendered Markdown as PNG");
    }
    let png = writer.into_inner();
    Ok(vec![RenderedImage {
        mime: "image/png".to_string(),
        png,
        width,
        height,
    }])
}

pub(crate) struct CappedVecWriter {
    bytes: Vec<u8>,
    limit: usize,
    exceeded: bool,
}

impl CappedVecWriter {
    pub(crate) fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::new(),
            limit,
            exceeded: false,
        }
    }

    pub(crate) fn exceeded(&self) -> bool {
        self.exceeded
    }

    pub(crate) fn into_inner(self) -> Vec<u8> {
        self.bytes
    }
}

impl Write for CappedVecWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let Some(next_len) = self.bytes.len().checked_add(buffer.len()) else {
            self.exceeded = true;
            return Err(io::Error::other("rendered PNG byte budget exceeded"));
        };
        if next_len > self.limit {
            self.exceeded = true;
            return Err(io::Error::other("rendered PNG byte budget exceeded"));
        }
        self.bytes.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(crate) fn validate_page_dimensions(width: u32, height: u32) -> Result<()> {
    if width == 0 {
        bail!("rendered image width must be non-zero");
    }
    if !(MIN_RENDERED_HEIGHT..=MAX_PAGE_HEIGHT).contains(&height) {
        bail!("rendered image height {height} is outside the supported range");
    }
    let pixels = u64::from(width).saturating_mul(u64::from(height));
    if pixels > MAX_PAGE_PIXELS {
        bail!("rendered image would exceed the {MAX_PAGE_PIXELS}-pixel limit");
    }
    Ok(())
}

pub(crate) fn checked_total_page_pixels(current: u64, page: u64) -> Result<u64> {
    let total = current
        .checked_add(page)
        .context("rendered page pixel count overflowed")?;
    if total > MAX_TOTAL_PAGE_PIXELS {
        bail!("rendered Markdown exceeds the {MAX_TOTAL_PAGE_PIXELS}-pixel total limit");
    }
    Ok(total)
}

pub(crate) fn draw_decoration(
    image: &mut RgbaImage,
    block: &LayoutBlock,
    placement: &Placement,
    x: u32,
    y: u32,
    palette: Palette,
) {
    let height = placement.source_end.saturating_sub(placement.source_start);
    match block.kind {
        BlockKind::Code => {
            fill_rect(image, x, y, COLUMN_WIDTH, height, palette.code_background);
        }
        BlockKind::Quote => {
            fill_rect(image, x, y, COLUMN_WIDTH, height, palette.quote_background);
            fill_rect(image, x, y, 6, height, palette.quote_bar);
        }
        BlockKind::Rule => {
            let line_y = y.saturating_add(height / 2);
            fill_rect(image, x, line_y, COLUMN_WIDTH, 2, palette.rule);
        }
        BlockKind::Heading(1) if placement.source_end == block.total_height => {
            let line_y = y.saturating_add(height).saturating_sub(2);
            fill_rect(image, x, line_y, COLUMN_WIDTH, 2, palette.rule);
        }
        _ => {}
    }
    if placement.source_start == 0 {
        if let Some(task) = block.task {
            draw_checkbox(
                image,
                x.saturating_add(task.x),
                y.saturating_add(task.y),
                task.size,
                task.checked,
                palette.text,
            );
        }
    }
}

pub(crate) fn draw_table_fragment(
    image: &mut RgbaImage,
    font_system: &mut FontSystem,
    swash_cache: &mut SwashCache,
    block: &LayoutBlock,
    placement: &Placement,
    column_x: u32,
    destination_y: u32,
    palette: Palette,
) {
    let Some(table) = block.table.as_ref() else {
        return;
    };
    for row in table.rows.iter().filter(|row| {
        row.source_start >= placement.source_start && row.source_end <= placement.source_end
    }) {
        let row_y =
            destination_y.saturating_add(row.source_start.saturating_sub(placement.source_start));
        let row_height = row.source_end.saturating_sub(row.source_start);
        let background = if row.header {
            palette.table_header_background
        } else if row.stripe {
            palette.quote_background
        } else {
            palette.table_background
        };
        fill_rect(image, column_x, row_y, COLUMN_WIDTH, row_height, background);
        fill_rect(image, column_x, row_y, COLUMN_WIDTH, 1, palette.border);
        fill_rect(
            image,
            column_x,
            row_y.saturating_add(row_height.saturating_sub(1)),
            COLUMN_WIDTH,
            1,
            palette.border,
        );
        for cell in &row.cells {
            let cell_x = column_x.saturating_add(cell.x);
            fill_rect(image, cell_x, row_y, 1, row_height, palette.border);
            if cell.x.saturating_add(cell.width) == COLUMN_WIDTH {
                fill_rect(
                    image,
                    cell_x.saturating_add(cell.width.saturating_sub(1)),
                    row_y,
                    1,
                    row_height,
                    palette.border,
                );
            }
            draw_table_cell_text(
                image,
                font_system,
                swash_cache,
                cell,
                cell_x.saturating_add(TABLE_CELL_PADDING),
                row_y.saturating_add(TABLE_CELL_PADDING),
                cell_x.saturating_add(cell.width.saturating_sub(TABLE_CELL_PADDING)),
                row_y.saturating_add(row_height.saturating_sub(TABLE_CELL_PADDING)),
            );
        }
    }
}

pub(crate) fn draw_table_cell_text(
    image: &mut RgbaImage,
    font_system: &mut FontSystem,
    swash_cache: &mut SwashCache,
    cell: &LayoutTableCell,
    origin_x: u32,
    origin_y: u32,
    clip_x_end: u32,
    clip_y_end: u32,
) {
    for run in cell.buffer.layout_runs() {
        for (start_x, end_x) in inline_code_chip_ranges(run.glyphs) {
            let inset = (run.line_height * INLINE_CODE_CHIP_INSET_RATIO).max(2.0);
            let top = i64::from(origin_y) + (run.line_top + inset) as i64;
            let bottom = i64::from(origin_y) + (run.line_top + run.line_height - inset) as i64;
            let x0 = (i64::from(origin_x) + (start_x - INLINE_CODE_CHIP_PAD_X).floor() as i64)
                .max(i64::from(origin_x));
            let x1 = (i64::from(origin_x) + (end_x + INLINE_CODE_CHIP_PAD_X).ceil() as i64)
                .min(i64::from(clip_x_end));
            let bottom = bottom.min(i64::from(clip_y_end));
            let (Ok(x0), Ok(top)) = (u32::try_from(x0), u32::try_from(top)) else {
                continue;
            };
            if x1 <= i64::from(x0) || bottom <= i64::from(top) {
                continue;
            }
            fill_rect(
                image,
                x0,
                top,
                (x1 - i64::from(x0)) as u32,
                (bottom - i64::from(top)) as u32,
                cell.inline_code_background,
            );
        }
        for glyph in run.glyphs {
            if swash_cache.image_cache.len() >= MAX_CACHED_GLYPHS {
                swash_cache.image_cache.clear();
            }
            let physical = glyph.physical((0.0, 0.0), 1.0);
            let glyph_color = glyph.color_opt.unwrap_or(cell.default_color);
            swash_cache.with_pixels(
                font_system,
                physical.cache_key,
                glyph_color,
                |pixel_x, pixel_y, pixel_color| {
                    let global_x = i64::from(origin_x) + i64::from(physical.x) + i64::from(pixel_x);
                    let global_y = i64::from(origin_y)
                        + run.line_y as i64
                        + i64::from(physical.y)
                        + i64::from(pixel_y);
                    let (Ok(global_x), Ok(global_y)) =
                        (u32::try_from(global_x), u32::try_from(global_y))
                    else {
                        return;
                    };
                    if global_x < origin_x
                        || global_x >= clip_x_end
                        || global_y < origin_y
                        || global_y >= clip_y_end
                    {
                        return;
                    }
                    if let Some(destination) = image.get_pixel_mut_checked(global_x, global_y) {
                        destination.blend(&Rgba(pixel_color.as_rgba()));
                    }
                },
            );
        }
    }
}

pub(crate) fn draw_checkbox(
    image: &mut RgbaImage,
    x: u32,
    y: u32,
    size: u32,
    checked: bool,
    color: [u8; 4],
) {
    if size < 4 {
        return;
    }
    fill_rect(image, x, y, size, 2, color);
    fill_rect(
        image,
        x,
        y.saturating_add(size.saturating_sub(2)),
        size,
        2,
        color,
    );
    fill_rect(image, x, y, 2, size, color);
    fill_rect(
        image,
        x.saturating_add(size.saturating_sub(2)),
        y,
        2,
        size,
        color,
    );
    if checked {
        draw_line(
            image,
            x.saturating_add(size / 5),
            y.saturating_add(size / 2),
            x.saturating_add(size * 2 / 5),
            y.saturating_add(size * 3 / 4),
            3,
            color,
        );
        draw_line(
            image,
            x.saturating_add(size * 2 / 5),
            y.saturating_add(size * 3 / 4),
            x.saturating_add(size * 4 / 5),
            y.saturating_add(size / 4),
            3,
            color,
        );
    }
}

pub(crate) fn draw_line(
    image: &mut RgbaImage,
    x0: u32,
    y0: u32,
    x1: u32,
    y1: u32,
    width: u32,
    color: [u8; 4],
) {
    let dx = i64::from(x1).saturating_sub(i64::from(x0));
    let dy = i64::from(y1).saturating_sub(i64::from(y0));
    let steps = dx.unsigned_abs().max(dy.unsigned_abs()).max(1);
    for step in 0..=steps {
        let x = i64::from(x0).saturating_add(
            dx.saturating_mul(step as i64)
                .checked_div(steps as i64)
                .unwrap_or(0),
        );
        let y = i64::from(y0).saturating_add(
            dy.saturating_mul(step as i64)
                .checked_div(steps as i64)
                .unwrap_or(0),
        );
        let (Ok(x), Ok(y)) = (u32::try_from(x), u32::try_from(y)) else {
            continue;
        };
        fill_rect(image, x, y, width, width, color);
    }
}

pub(crate) fn draw_text_fragment(
    image: &mut RgbaImage,
    font_system: &mut FontSystem,
    swash_cache: &mut SwashCache,
    block: &LayoutBlock,
    placement: &Placement,
    column_x: u32,
    destination_y: u32,
) {
    let Some(buffer) = block.buffer.as_ref() else {
        return;
    };
    let clip_x_end = column_x.saturating_add(COLUMN_WIDTH);
    let clip_y_end =
        destination_y.saturating_add(placement.source_end.saturating_sub(placement.source_start));
    for run in buffer.layout_runs() {
        let run_top = block.vertical_padding as f32 + run.line_top;
        let run_bottom = run_top + run.line_height;
        if run_bottom <= placement.source_start as f32 || run_top >= placement.source_end as f32 {
            continue;
        }
        for (start_x, end_x) in inline_code_chip_ranges(run.glyphs) {
            let inset = (run.line_height * INLINE_CODE_CHIP_INSET_RATIO).max(2.0);
            let top = (run_top + inset).max(placement.source_start as f32);
            let bottom = (run_bottom - inset).min(placement.source_end as f32);
            if bottom <= top {
                continue;
            }
            let global_y =
                i64::from(destination_y) + top as i64 - i64::from(placement.source_start);
            let x_base = i64::from(column_x) + i64::from(block.inset_left);
            let x0 = (x_base + (start_x - INLINE_CODE_CHIP_PAD_X).floor() as i64)
                .max(i64::from(column_x));
            let x1 = (x_base + (end_x + INLINE_CODE_CHIP_PAD_X).ceil() as i64)
                .min(i64::from(clip_x_end));
            let (Ok(x0), Ok(global_y)) = (u32::try_from(x0), u32::try_from(global_y)) else {
                continue;
            };
            if x1 <= i64::from(x0) {
                continue;
            }
            fill_rect(
                image,
                x0,
                global_y,
                (x1 - i64::from(x0)) as u32,
                (bottom - top) as u32,
                block.inline_code_background,
            );
        }
        for glyph in run.glyphs {
            if swash_cache.image_cache.len() >= MAX_CACHED_GLYPHS {
                swash_cache.image_cache.clear();
            }
            let physical = glyph.physical((0.0, 0.0), 1.0);
            let glyph_color = glyph.color_opt.unwrap_or(block.default_color);
            swash_cache.with_pixels(
                font_system,
                physical.cache_key,
                glyph_color,
                |pixel_x, pixel_y, pixel_color| {
                    let global_x = i64::from(column_x)
                        + i64::from(block.inset_left)
                        + i64::from(physical.x)
                        + i64::from(pixel_x);
                    let global_block_y = i64::from(block.vertical_padding)
                        + run.line_y as i64
                        + i64::from(physical.y)
                        + i64::from(pixel_y);
                    if global_block_y < i64::from(placement.source_start)
                        || global_block_y >= i64::from(placement.source_end)
                    {
                        return;
                    }
                    let global_y = i64::from(destination_y) + global_block_y
                        - i64::from(placement.source_start);
                    let (Ok(global_x), Ok(global_y)) =
                        (u32::try_from(global_x), u32::try_from(global_y))
                    else {
                        return;
                    };
                    if global_x < column_x
                        || global_x >= clip_x_end
                        || global_y < destination_y
                        || global_y >= clip_y_end
                    {
                        return;
                    }
                    if let Some(destination) = image.get_pixel_mut_checked(global_x, global_y) {
                        destination.blend(&Rgba(pixel_color.as_rgba()));
                    }
                },
            );
        }
    }
}

pub(crate) fn fill_rect(
    image: &mut RgbaImage,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    color: [u8; 4],
) {
    let end_x = x.saturating_add(width).min(image.width());
    let end_y = y.saturating_add(height).min(image.height());
    for py in y.min(end_y)..end_y {
        for px in x.min(end_x)..end_x {
            if let Some(pixel) = image.get_pixel_mut_checked(px, py) {
                *pixel = Rgba(color);
            }
        }
    }
}

pub(crate) fn color(rgba: [u8; 4]) -> Color {
    Color::rgba(rgba[0], rgba[1], rgba[2], rgba[3])
}
