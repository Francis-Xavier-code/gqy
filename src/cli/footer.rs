//! footer — REPL 底部状态栏与常量（自 src/cli/defs.rs 拆分）。

pub(crate) use super::*;

pub(crate) const REPL_MAX_VISIBLE_INPUT_ROWS: u16 = 12;
pub(crate) const REPL_PASTE_PLACEHOLDER_MIN_LINES: usize = 3;
pub(crate) const REPL_PASTE_PLACEHOLDER_MIN_CHARS: usize = 150;
pub(crate) const RELOAD_MAX_ATTEMPTS: usize = 12;
pub(crate) const RELOAD_RETRY_INTERVAL: Duration = Duration::from_secs(5);
pub(crate) const RELOAD_RESPONSE_TIMEOUT: Duration = Duration::from_secs(60);
#[derive(Clone, Debug)]
pub(crate) struct PastedText {
    pub(crate) text: String,
}

#[derive(Clone, Debug)]
pub(crate) struct ReplFooterStatus {
    pub(crate) provider: String,
    pub(crate) model: String,
    mixed_models: bool,
    pub(crate) thinking: Option<String>,
    pub(crate) token_usage: render::TokenMeter,
}

/// The daemon reports Σ as three flat numbers; regroup them for the meters.
pub(crate) fn state_cumulative(state: &ipc::SessionState) -> TurnTokens {
    TurnTokens {
        total: state.cumulative_tokens,
        prompt: state.cumulative_prompt_tokens,
        cache_read: state.cumulative_cache_read_tokens,
    }
}

/// Σ is hidden entirely when nothing has been spent yet, so an empty session
/// does not carry a "Σ0" that means nothing.
pub(crate) fn meter_cumulative(cumulative: TurnTokens) -> render::TokenMeter {
    render::TokenMeter {
        cumulative_tokens: (cumulative.total > 0).then_some(cumulative.total),
        cumulative_prompt_tokens: cumulative.prompt,
        cumulative_cached_tokens: cumulative.cache_read,
        ..Default::default()
    }
}

impl ReplFooterStatus {
    pub(crate) fn from_config(
        config: &AppConfig,
        session_tokens: u64,
        cumulative: TurnTokens,
    ) -> Self {
        let active = config.active_provider_model_choices();
        let mixed_models = active.len() > 1;
        let (provider_id, model) = match active.as_slice() {
            [] => ("-".to_string(), t("None", "无").to_string()),
            [choice] => (
                choice.provider_id.clone(),
                short_model_name(&choice.model, &choice.provider_id),
            ),
            _ => ("mixed".to_string(), t("Mixed", "混合").to_string()),
        };

        Self {
            model,
            provider: provider_id,
            mixed_models,
            thinking: None,
            token_usage: render::TokenMeter {
                session_tokens,
                context_window: config.active_context_window().ok().flatten(),
                ..meter_cumulative(cumulative)
            },
        }
    }

    pub(crate) fn update_token_usage(
        &mut self,
        result: &crate::llm::ChatResult,
        session_tokens: u64,
        context_window: Option<usize>,
        cumulative: TurnTokens,
    ) {
        if result.usage.is_some() {
            let turn = TurnTokens::from_usage(result.usage.as_ref());
            self.set_token_usage_with_cache(turn, session_tokens, context_window, cumulative);
        }
    }

    pub(crate) fn set_token_usage(
        &mut self,
        turn_tokens: u64,
        session_tokens: u64,
        context_window: Option<usize>,
        cumulative: TurnTokens,
    ) {
        self.set_token_usage_with_cache(
            TurnTokens {
                total: turn_tokens,
                ..TurnTokens::default()
            },
            session_tokens,
            context_window,
            cumulative,
        );
    }

    pub(crate) fn set_token_usage_with_cache(
        &mut self,
        turn: TurnTokens,
        session_tokens: u64,
        context_window: Option<usize>,
        cumulative: TurnTokens,
    ) {
        self.token_usage = render::TokenMeter {
            turn_tokens: turn.total,
            turn_prompt_tokens: turn.prompt,
            turn_cached_tokens: turn.cache_read,
            session_tokens,
            context_window,
            ..meter_cumulative(cumulative)
        };
    }

    pub(crate) fn update_session_tokens(&mut self, session_tokens: u64) {
        self.token_usage.session_tokens = session_tokens;
    }

    /// 回合中途的逐请求刷新:在(回合前的)基线上叠加回合累计。必须作用
    /// 在基线快照的克隆上,同一回合内可重复调用而不重复相加。
    pub(crate) fn apply_round_usage(&mut self, context_tokens: u64, turn: TurnTokens) {
        let meter = &mut self.token_usage;
        meter.turn_tokens = turn.total;
        meter.turn_prompt_tokens = turn.prompt;
        meter.turn_cached_tokens = turn.cache_read;
        if context_tokens > 0 {
            meter.session_tokens = context_tokens;
        }
        let cumulative = meter.cumulative_tokens.unwrap_or(0) + turn.total;
        meter.cumulative_tokens = (cumulative > 0).then_some(cumulative);
        meter.cumulative_prompt_tokens += turn.prompt;
        meter.cumulative_cached_tokens += turn.cache_read;
    }

    pub(crate) fn update_context_window(&mut self, context_window: Option<usize>) {
        self.token_usage.context_window = context_window;
    }

    /// Returns whether anything actually moved, so an idle tick only forces a
    /// redraw when the numbers changed.
    pub(crate) fn update_cumulative_tokens(&mut self, cumulative: TurnTokens) -> bool {
        let meter = meter_cumulative(cumulative);
        let changed = self.token_usage.cumulative_tokens != meter.cumulative_tokens
            || self.token_usage.cumulative_prompt_tokens != meter.cumulative_prompt_tokens
            || self.token_usage.cumulative_cached_tokens != meter.cumulative_cached_tokens;
        self.token_usage.cumulative_tokens = meter.cumulative_tokens;
        self.token_usage.cumulative_prompt_tokens = meter.cumulative_prompt_tokens;
        self.token_usage.cumulative_cached_tokens = meter.cumulative_cached_tokens;
        changed
    }

    pub(crate) fn reset_token_usage(&mut self, session_tokens: u64, context_window: Option<usize>) {
        self.token_usage = render::TokenMeter {
            session_tokens,
            context_window,
            ..Default::default()
        };
    }

    pub(crate) fn update_thinking_variant(&mut self, variant: Option<&str>) {
        self.thinking = if self.mixed_models {
            None
        } else {
            variant.map(str::to_string)
        };
    }
}

pub(crate) fn short_model_name(model: &str, provider: &str) -> String {
    model
        .strip_prefix(&format!("{provider}/"))
        .unwrap_or(model)
        .rsplit('/')
        .next()
        .unwrap_or(model)
        .to_string()
}

/// crossterm asks the terminal where the cursor is (`ESC[6n`) and gives up if
/// the reply does not arrive within a fixed wait. Over a laggy SSH link that
/// wait expires routinely, and every `?` on it used to take the whole REPL
/// down with "The cursor position could not be read within a normal duration".
/// The answer is only ever used to re-anchor a redraw, so a stale one costs a
/// single imperfect frame — losing the session costs the session.
pub(crate) fn cursor_position_or(fallback: (u16, u16)) -> (u16, u16) {
    // 终端已挂断时 ESC[6n 永远等不到应答,而 crossterm 的应答等待对
    // HUP fd 会无限自旋(超时失效)——直接用回退值,让退出路径走完。
    if terminal_hangup() {
        return fallback;
    }
    cursor::position().unwrap_or(fallback)
}

pub(crate) fn cursor_row_or(fallback: u16) -> u16 {
    cursor_position_or((0, fallback)).1
}

pub(crate) fn cursor_col_or(fallback: u16) -> u16 {
    cursor_position_or((fallback, 0)).0
}

pub(crate) fn repl_footer_line(mode: AgentMode, footer: &ReplFooterStatus, cols: usize) -> String {
    let cols = cols.max(1);
    let bar = input_prompt_bar(mode);
    let bar_width = visible_width(&bar);
    // The footer carries only the two standing gauges — how much context is
    // left, and what the session has cost. The per-turn figure is transient and
    // already has its own home in the `Token:` line printed after each reply;
    // keeping it here cost 14 columns and pushed the whole footer past 80.
    let usage = render::TokenMeter {
        turn_tokens: 0,
        ..footer.token_usage
    };
    // Narrow terminals: drop the cumulative total first, then the percent,
    // so the core context meter survives as long as possible.
    let mut right_plain = String::new();
    for (with_cumulative, with_percent) in [(true, true), (false, true), (false, false)] {
        let meter = render::TokenMeter {
            cumulative_tokens: usage.cumulative_tokens.filter(|_| with_cumulative),
            ..usage
        };
        right_plain = render::format_token_usage_inline_opts(&meter, with_percent);
        let left_room = cols
            .saturating_sub(bar_width)
            .saturating_sub(visible_width(&right_plain));
        if left_room >= 24 {
            break;
        }
    }
    let right = format!("\x1b[2m{right_plain}\x1b[0m");
    let right_width = visible_width(&right);
    let left_budget = cols.saturating_sub(bar_width.saturating_add(right_width).saturating_add(1));
    let left = repl_footer_left(mode, footer, left_budget);
    let gap = cols
        .saturating_sub(
            bar_width
                .saturating_add(visible_width(&left))
                .saturating_add(right_width),
        )
        .max(1);
    format!("{bar}{left}{}{right}", " ".repeat(gap))
}

pub(crate) fn repl_footer_left(mode: AgentMode, footer: &ReplFooterStatus, width: usize) -> String {
    let thinking = footer.thinking.as_deref().unwrap_or_default();
    let colored_thinking = (!thinking.is_empty()).then(|| primary_footer_text(thinking));
    let colored_thinking = colored_thinking.as_deref().unwrap_or_default();
    let provider = format!("\x1b[2m{}\x1b[0m", footer.provider);
    let mode = colored_footer_mode_label(mode);
    let full = repl_footer_left_parts(&mode, &footer.model, Some(&provider), colored_thinking);
    if visible_width(&full) <= width {
        return full;
    }

    let compact = repl_footer_left_parts(&mode, &footer.model, None, colored_thinking);
    if visible_width(&compact) <= width {
        return compact;
    }

    let fixed_width =
        visible_width(&mode)
            .saturating_add(3)
            .saturating_add(if thinking.is_empty() {
                0
            } else {
                3 + visible_width(colored_thinking)
            });
    let model_budget = width.saturating_sub(fixed_width).max(1);
    let model = truncate_display(&footer.model, model_budget);
    repl_footer_left_parts(&mode, &model, None, colored_thinking)
}

pub(crate) fn repl_footer_left_parts(
    mode: &str,
    model: &str,
    provider: Option<&str>,
    thinking: &str,
) -> String {
    let mut endpoint = model.to_string();
    if let Some(provider) = provider.filter(|provider| !provider.is_empty()) {
        if !endpoint.is_empty() {
            endpoint.push(' ');
        }
        endpoint.push_str(provider);
    }
    let mut parts = vec![mode.to_string(), endpoint];
    if !thinking.is_empty() {
        parts.push(thinking.to_string());
    }
    parts.join(" · ")
}

pub(crate) fn print_mixed_model_endpoint(
    show: bool,
    result: &crate::llm::ChatResult,
    variant: Option<&str>,
) {
    if !show {
        return;
    }
    let provider = result.provider_id.as_deref().unwrap_or("-");
    let model = result.model.as_deref().unwrap_or("-");
    println!(
        "\x1b[2m{}\x1b[0m\n",
        mixed_model_endpoint_label(provider, model, variant)
    );
}

pub(crate) fn mixed_model_endpoint_label(
    provider: &str,
    model: &str,
    variant: Option<&str>,
) -> String {
    let variant = variant
        .filter(|variant| !variant.is_empty())
        .map(|variant| format!(" · {variant}"))
        .unwrap_or_default();
    format!("{provider} / {model}{variant}")
}

pub(crate) fn show_mixed_model_endpoint(config: &AppConfig, interactive: bool) -> bool {
    config.active_provider_model_choices().len() > 1
        && match config.display.mixed_model_endpoint_display.as_str() {
            "off" => false,
            "all" => true,
            _ => interactive,
        }
}

pub(crate) fn colored_footer_mode_label(mode: AgentMode) -> String {
    let label = mode.label();
    match mode {
        AgentMode::Normal => primary_footer_text(label),
        // tertiary(35 酒红,与 render/webui 的 tertiary 一致),区别于普通
        // 模式的 primary 蓝。
        AgentMode::Dev => format!("\x1b[1m\x1b[35m{label}\x1b[0m"),
    }
}

pub(crate) fn primary_footer_text(text: &str) -> String {
    format!("\x1b[1m\x1b[34m{text}\x1b[0m")
}
