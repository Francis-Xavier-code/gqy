//! defs — 自 src/cli.rs 拆分。

use super::*;

use keyboard_enhancement::KeyboardEnhancementState;

pub(crate) const REPL_MAX_VISIBLE_INPUT_ROWS: u16 = 12;
pub(crate) const REPL_PASTE_PLACEHOLDER_MIN_LINES: usize = 3;
pub(crate) const REPL_PASTE_PLACEHOLDER_MIN_CHARS: usize = 150;
pub(crate) const RELOAD_MAX_ATTEMPTS: usize = 12;
pub(crate) const RELOAD_RETRY_INTERVAL: Duration = Duration::from_secs(5);
pub(crate) const RELOAD_RESPONSE_TIMEOUT: Duration = Duration::from_secs(60);
#[derive(Clone, Debug)]
pub(crate) struct PastedText {
    text: String,
}

#[derive(Clone, Debug)]
pub(crate) struct ReplFooterStatus {
    provider: String,
    model: String,
    mixed_models: bool,
    thinking: Option<String>,
    token_usage: render::TokenMeter,
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

#[derive(Debug, Parser)]
#[command(name = "gqy", version, about = "GQY CLI AI Agent")]
pub struct Cli {
    #[arg(long, global = true)]
    pub debug: bool,

    #[arg(long)]
    pub stdout: bool,

    /// 仅为本次命令指定目标会话（名称或编号），不改变全局当前会话
    #[arg(long)]
    pub session: Option<String>,

    #[arg(short = 'c', long = "continue", conflicts_with = "session")]
    pub continue_session: bool,

    #[arg(long, hide = true)]
    pub shell_intercept: bool,

    #[arg(long, hide = true)]
    pub shell_classify: bool,

    #[arg(long, hide = true)]
    pub shell: Option<String>,

    #[arg(long, hide = true)]
    pub stdin: bool,

    #[arg(long, hide = true)]
    pub clipboard_paste: bool,

    #[command(subcommand)]
    pub command: Option<Command>,

    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub message: Vec<String>,
}

pub fn parse() -> Cli {
    parse_args(std::env::args_os().collect()).unwrap_or_else(|err| err.exit())
}

pub(crate) fn parse_args(mut args: Vec<OsString>) -> std::result::Result<Cli, clap::Error> {
    let debug = extract_debug_flag(&mut args);
    let matches = localized_command().try_get_matches_from(args)?;
    let web_port_explicit = matches
        .subcommand_matches("web")
        .and_then(|web| web.value_source("port"))
        == Some(clap::parser::ValueSource::CommandLine);
    let mut cli = Cli::from_arg_matches(&matches)?;
    if let Some(Command::Web(args)) = &mut cli.command {
        args.port_explicit = web_port_explicit;
    }
    cli.debug |= debug;
    Ok(cli)
}

pub(crate) fn extract_debug_flag(args: &mut Vec<OsString>) -> bool {
    let mut debug = false;
    let mut index = 1;
    while index < args.len() {
        if args[index] == "--" {
            break;
        }
        if args[index] == "--debug" {
            args.remove(index);
            debug = true;
        } else {
            index += 1;
        }
    }
    debug
}

pub(crate) fn localized_command() -> clap::Command {
    let mut command = Cli::command();
    command = command
        .about(t("GQY AI assistant", "GQY AI 助手"))
        .override_usage(t(
            "gqy [OPTIONS] [MESSAGE]... [COMMAND]",
            "gqy [选项] [消息]... [命令]",
        ));
    if is_zh() {
        command = command
            .subcommand_help_heading("命令")
            .arg_required_else_help(false)
            .next_help_heading("选项")
            .help_template("{about}\n\n用法: {usage}\n\n命令:\n{subcommands}\n参数:\n{positionals}\n选项:\n{options}\n{after-help}")
            .after_help("提示：不带参数进入 REPL；直接输入消息会发送一次对话。可在配置界面设置语言，GQY_LANG 可临时覆盖。")
            .disable_help_subcommand(true);
    } else {
        command = command
            .after_help(
                "Tip: run without arguments to enter the REPL; pass MESSAGE to send one chat turn. Set the language in the configuration UI; GQY_LANG is a temporary override.",
            )
            .disable_help_subcommand(true);
    }
    command = localize_top_args(command);
    command = localize_subcommands(command);
    command = apply_localized_help_flags(command, true);
    if is_zh() {
        command = apply_chinese_help_template(command);
    }
    // 终端无缝集成组在根帮助里以静态段单独成节(这些子命令已 hide,
    // 不进 {subcommands});最后设置以免被上面的通用中文模板覆盖。
    command = command.help_template(root_help_template());
    command
}

pub(crate) fn root_help_template() -> String {
    let shell_block = t(
        "  fish-init          Integrate with fish; then chat in natural language directly in the terminal
  bash-init          Integrate with bash
  zsh-init           Integrate with zsh
  remove-shell-hook  Safely remove installed GQY shell hooks
  models             Switch the terminal-integration session's model
  variant            Switch the terminal session model's thinking level
  history            Show conversation history
  reset              Clear the terminal-integration session context
  reset-memory       Erase this persona's long-term memory
  pop                Move conversation turns out of active context",
        "  fish-init          集成到 fish，集成后可在终端直接使用自然语言交流
  bash-init          集成到 bash
  zsh-init           集成到 zsh
  remove-shell-hook  安全删除已安装的 GQY shell hook
  models             修改终端集成会话的模型
  variant            切换终端集成会话模型的思考档位
  history            显示会话历史
  reset              清除终端集成会话上下文
  reset-memory       清空长期记忆
  pop                将对话轮次移出当前上下文",
    );
    if is_zh() {
        format!(
            "{{about}}

用法: {{usage}}

命令:
{{subcommands}}

终端无缝集成相关：
{shell_block}

参数:
{{positionals}}
选项:
{{options}}
{{after-help}}"
        )
    } else {
        format!(
            "{{about}}

Usage: {{usage}}

Commands:
{{subcommands}}

Terminal integration:
{shell_block}

Arguments:
{{positionals}}
Options:
{{options}}
{{after-help}}"
        )
    }
}

pub(crate) fn apply_localized_help_flags(mut command: clap::Command, root: bool) -> clap::Command {
    command = command.disable_help_flag(true).arg(
        Arg::new("help")
            .short('h')
            .long("help")
            .help(t("Print help", "显示帮助"))
            .action(ArgAction::Help),
    );
    if root {
        command = command.disable_version_flag(true).arg(
            Arg::new("version")
                .short('V')
                .long("version")
                .help(t("Print version", "显示版本"))
                .action(ArgAction::Version),
        );
    }
    let subcommands = command
        .get_subcommands()
        .map(|subcommand| subcommand.get_name().to_string())
        .collect::<Vec<_>>();
    for name in subcommands {
        command = command.mut_subcommand(&name, |subcommand| {
            apply_localized_help_flags(subcommand, false)
        });
    }
    command
}

pub(crate) fn apply_chinese_help_template(mut command: clap::Command) -> clap::Command {
    let has_subcommands = command.get_subcommands().next().is_some();
    command = if has_subcommands {
        command.help_template(
            "{about}\n\n用法: {usage}\n\n命令:\n{subcommands}\n参数:\n{positionals}\n选项:\n{options}\n{after-help}",
        )
    } else {
        command.help_template(
            "{about}\n\n用法: {usage}\n\n参数:\n{positionals}\n选项:\n{options}\n{after-help}",
        )
    };
    let subcommands = command
        .get_subcommands()
        .map(|subcommand| subcommand.get_name().to_string())
        .collect::<Vec<_>>();
    for name in subcommands {
        command = command.mut_subcommand(&name, apply_chinese_help_template);
    }
    command
}

pub(crate) fn localize_top_args(command: clap::Command) -> clap::Command {
    command
        .mut_arg("debug", |arg| {
            arg.help(t(
                "Write detailed diagnostics to the GQY log directory",
                "将详细诊断信息写入 GQY 日志目录",
            ))
        })
        .mut_arg("stdout", |arg| {
            arg.help(t(
                "Plain output mode (no colors, no TUI); pipe-friendly for stdout redirection",
                "纯文本输出模式（无颜色、无 TUI）；适合管道重定向",
            ))
        })
        .mut_arg("continue_session", |arg| {
            arg.help(t(
                "Send the message into the terminal-integration session instead of a throwaway one-shot chat",
                "把消息发进终端集成会话，而不是用完即弃的一次性对话",
            ))
        })
        .mut_arg("message", |arg| {
            arg.help(t(
                "Message to send; omitted to enter REPL",
                "要发送的消息；省略则进入 REPL",
            ))
        })
}

pub(crate) fn localize_subcommands(mut command: clap::Command) -> clap::Command {
    let descriptions = [
        (
            "ask",
            "Send one message to the assistant as a one-shot chat",
            "向助手发送一条消息，一次性对话",
        ),
        (
            "normal",
            "Enter the normal-mode REPL (full persona abilities)",
            "进入普通模式 REPL（人格全能力）",
        ),
        (
            "dev",
            "Enter the dev-mode REPL (minimal coding form, no persona)",
            "进入开发模式 REPL（极简编码形态，无人格）",
        ),
        (
            "tool-call",
            "Tool bridge: call AI tools from the command line",
            "工具桥：通过命令行调用 AI 工具",
        ),
        (
            "init",
            "Create default config and state files",
            "创建默认配置和状态文件",
        ),
        (
            "paths",
            "Show app config, data, and cache paths",
            "显示应用配置、数据和缓存路径",
        ),
        ("config", "Configure via the TUI", "使用 TUI 进行配置"),
        ("reload", "Reload configuration", "重新加载配置"),
        (
            "models",
            "Switch the terminal-integration session's model",
            "修改终端集成会话的模型",
        ),
        ("list-models", "List available models", "列出可用模型"),
        (
            "variant",
            "Switch the terminal session model's thinking level",
            "切换终端集成会话模型的思考档位",
        ),
        (
            "fish-init",
            "Integrate with fish so you can chat in natural language directly in the terminal",
            "集成到 fish，集成后可在终端直接使用自然语言交流。",
        ),
        ("bash-init", "Integrate with bash", "集成到 bash"),
        ("zsh-init", "Integrate with zsh", "集成到 zsh"),
        (
            "remove-shell-hook",
            "Safely remove installed GQY shell hooks",
            "安全删除已安装的 GQY shell hook",
        ),
        ("history", "Show conversation history", "显示会话历史"),
        (
            "pop",
            "Move conversation turns out of active context",
            "将对话轮次移出当前上下文",
        ),
        ("kb", "Manage the knowledge base", "管理知识库"),
        (
            "update-default-kb",
            "Update GQY default knowledge base",
            "更新 GQY 默认知识库",
        ),
        ("memory", "Manage assistant memory", "管理记忆"),
        ("skills", "Manage assistant skills", "管理助手 skills"),
        (
            "reset",
            "Clear the terminal-integration session context",
            "清除终端集成会话上下文",
        ),
        (
            "reset-memory",
            "Erase this persona's long-term memory",
            "清空长期记忆",
        ),
        (
            "wipe",
            "Erase all conversation history, memory, group contexts and their artifacts",
            "抹掉所有会话历史、记忆、群聊上下文和其产物",
        ),
        ("web", "Open the local GQY WebUI", "访问本地 GQY WebUI"),
        (
            "daemon",
            "Manage the unified GQY background service",
            "管理 GQY 统一后台服务",
        ),
        (
            "export",
            "Export configuration into a portable archive",
            "导出配置，把当前配置打包成可移植归档",
        ),
        ("import", "Import configuration", "导入配置"),
    ];
    for (name, en, zh) in descriptions {
        command = command.mut_subcommand(name, |subcommand| subcommand.about(t(en, zh)));
    }
    // 终端无缝集成组:从 {subcommands} 里藏掉,根帮助模板里以静态段
    // 单独成节(clap 不支持子命令分组);`gqy <cmd> -h` 不受影响。
    for name in [
        "fish-init",
        "bash-init",
        "zsh-init",
        "remove-shell-hook",
        "models",
        "variant",
        "history",
        "reset",
        "reset-memory",
        "pop",
    ] {
        command = command.mut_subcommand(name, |subcommand| subcommand.hide(true));
    }
    for (index, name) in [
        "init",
        "config",
        "normal",
        "dev",
        "daemon",
        "web",
        "tool-call",
        "ask",
        "list-models",
        "export",
        "import",
        "kb",
        "memory",
        "skills",
        "update-default-kb",
        "wipe",
        "paths",
        "reload",
    ]
    .into_iter()
    .enumerate()
    {
        command = command.mut_subcommand(name, move |subcommand| subcommand.display_order(index));
    }
    command = command
        .mut_subcommand("ask", localize_ask_command)
        .mut_subcommand("models", localize_models_command)
        .mut_subcommand("variant", localize_variant_command)
        .mut_subcommand("history", localize_history_command)
        .mut_subcommand("pop", localize_pop_command)
        .mut_subcommand("kb", localize_kb_command)
        .mut_subcommand("memory", localize_memory_command)
        .mut_subcommand("skills", localize_skills_command)
        .mut_subcommand("config", localize_config_command)
        .mut_subcommand("web", localize_web_command)
        .mut_subcommand("daemon", localize_daemon_command)
        .mut_subcommand("export", localize_export_command)
        .mut_subcommand("import", localize_import_command);
    command
}

pub(crate) fn localize_export_command(command: clap::Command) -> clap::Command {
    command
        .mut_arg("output", |arg| {
            arg.help(t(
                "Archive path to write; omit to name it after this host and time",
                "要写入的归档路径；省略则按主机名与时间自动命名",
            ))
        })
        .mut_arg("all", |arg| {
            arg.help(t(
                "Include everything portable, index and platform history included",
                "包含全部可移植数据，含向量索引与平台历史",
            ))
        })
        .mut_arg("index", |arg| {
            arg.help(t(
                "Include the knowledge-base vector index (large; rebuildable with `gqy kb embed`)",
                "包含知识库向量索引（很大；可用 gqy kb embed 重建）",
            ))
        })
        .mut_arg("platforms", |arg| {
            arg.help(t("Include chat-platform history", "包含通讯平台的聊天历史"))
        })
        .mut_arg("no_secrets", |arg| {
            arg.help(t(
                "Blank out API keys and tokens (you must refill them after importing)",
                "清空 API key 与访问令牌（导入后需要自行补填）",
            ))
        })
        .mut_arg("dry_run", |arg| {
            arg.help(t(
                "Print what would be packed without writing an archive",
                "只打印将要打包的内容，不实际写归档",
            ))
        })
        .mut_arg("force", |arg| {
            arg.help(t("Overwrite an existing archive", "覆盖已存在的归档文件"))
        })
}

pub(crate) fn localize_import_command(command: clap::Command) -> clap::Command {
    command
        .mut_arg("archive", |arg| {
            arg.help(t(
                "Archive produced by `gqy export`",
                "gqy export 生成的归档",
            ))
        })
        .mut_arg("force", |arg| {
            arg.help(t(
                "Overwrite existing data (the current installation is backed up first)",
                "覆盖已有数据（覆盖前会先备份当前安装）",
            ))
        })
}

pub(crate) fn localize_ask_command(command: clap::Command) -> clap::Command {
    command.mut_arg("message", |arg| {
        arg.help(t("Message to send", "要发送的消息"))
    })
}

pub(crate) fn localize_models_command(command: clap::Command) -> clap::Command {
    command.mut_arg("target", |arg| {
        arg.help(t(
            "List index, provider/model, or 'default' to follow the global pool",
            "模型列表序号、供应商/模型名，或 default 恢复跟随全局模型池",
        ))
    })
}

pub(crate) fn localize_variant_command(command: clap::Command) -> clap::Command {
    command.mut_arg("name", |arg| {
        arg.help(t(
            "Thinking level to select; omit to choose interactively",
            "要选择的思考档位；省略则进入交互选择",
        ))
    })
}

pub(crate) fn localize_history_command(command: clap::Command) -> clap::Command {
    command
        .mut_arg("limit", |arg| {
            arg.help(t("Number of history entries to show", "显示的历史条数"))
        })
        .mut_arg("raw", |arg| {
            arg.help(t("Print raw JSONL entries", "输出原始 JSONL 条目"))
        })
        .mut_arg("no_thinking", |arg| {
            arg.help(t("Hide stored reasoning", "隐藏已保存的思考内容"))
        })
}

pub(crate) fn localize_pop_command(command: clap::Command) -> clap::Command {
    command.mut_arg("count", |arg| {
        arg.help(t(
            "Number of oldest turns to pop; omit to select interactively",
            "要弹出的最旧轮次数；省略则进入交互多选",
        ))
    })
}

pub(crate) fn localize_config_command(command: clap::Command) -> clap::Command {
    command
        .mut_subcommand("validate", |subcommand| {
            subcommand.about(t("Validate configuration", "校验配置"))
        })
        .mut_subcommand("paths", |subcommand| {
            subcommand.about(t("Show configuration paths", "显示配置路径"))
        })
}

pub(crate) fn localize_web_command(command: clap::Command) -> clap::Command {
    command
        .mut_arg("port", |arg| arg.help(t("Local TCP port", "本地 TCP 端口")))
        .mut_arg("password", |arg| {
            arg.help(t(
                "Prompt securely for a required password",
                "安全输入所需的访问密码",
            ))
        })
        .mut_arg("password_file", |arg| {
            arg.help(t(
                "Read the WebUI password from a file",
                "从文件读取 WebUI 访问密码",
            ))
        })
}

pub(crate) fn localize_daemon_command(mut command: clap::Command) -> clap::Command {
    let descriptions = [
        (
            "start",
            "Start all configured GQY interfaces",
            "启动所有已配置的 GQY 接口",
        ),
        (
            "stop",
            "Stop the GQY background service",
            "停止 GQY 后台服务",
        ),
        (
            "restart",
            "Restart the GQY background service",
            "重启 GQY 后台服务",
        ),
        (
            "status",
            "Show daemon and interface status",
            "显示 daemon 与接口状态",
        ),
        ("logs", "Follow daemon logs", "持续查看 daemon 日志"),
    ];
    for (name, en, zh) in descriptions {
        command = command.mut_subcommand(name, |subcommand| subcommand.about(t(en, zh)));
    }
    command
        .mut_arg("port", |arg| {
            arg.help(t("WebUI TCP port", "WebUI TCP 端口"))
        })
        .mut_subcommand("logs", |subcommand| {
            subcommand.mut_arg("lines", |arg| {
                arg.help(t(
                    "Print only the most recent N lines and exit",
                    "仅输出最近 N 行后退出",
                ))
            })
        })
}

pub(crate) fn localize_kb_command(mut command: clap::Command) -> clap::Command {
    let descriptions = [
        ("add", "Add a file or directory", "添加文件或目录"),
        ("list", "List indexed files", "列出已索引文件"),
        ("search", "Search knowledge base content", "搜索知识库内容"),
        ("find", "Find files by name", "按文件名查找文件"),
        ("read", "Read a knowledge base file", "读取知识库文件"),
        ("remove", "Remove a knowledge base file", "移除知识库文件"),
        (
            "reindex",
            "Rebuild keyword index on demand",
            "按需重建关键词索引",
        ),
        ("stats", "Show knowledge base statistics", "显示知识库统计"),
        ("embed", "Manage semantic embeddings", "管理语义嵌入"),
    ];
    for (name, en, zh) in descriptions {
        command = command.mut_subcommand(name, |subcommand| subcommand.about(t(en, zh)));
    }
    command
        .mut_subcommand("add", |subcommand| {
            subcommand
                .mut_arg("path", |arg| arg.help(t("Path to add", "要添加的路径")))
                .mut_arg("recursive", |arg| {
                    arg.help(t(
                        "Compatibility flag; directories are recursive by default",
                        "兼容参数；目录默认递归导入",
                    ))
                })
        })
        .mut_subcommand("search", |subcommand| {
            subcommand
                .mut_arg("query", |arg| arg.help(t("Search query", "搜索查询")))
                .mut_arg("limit", |arg| arg.help(t("Maximum results", "最大结果数")))
        })
        .mut_subcommand("find", |subcommand| {
            subcommand
                .mut_arg("query", |arg| arg.help(t("Filename query", "文件名查询")))
                .mut_arg("limit", |arg| arg.help(t("Maximum results", "最大结果数")))
        })
        .mut_subcommand("read", |subcommand| {
            subcommand
                .mut_arg("file", |arg| {
                    arg.help(t("Knowledge base file name", "知识库文件名"))
                })
                .mut_arg("start", |arg| arg.help(t("Starting line", "起始行")))
                .mut_arg("lines", |arg| arg.help(t("Number of lines", "读取行数")))
        })
        .mut_subcommand("remove", |subcommand| {
            subcommand.mut_arg("file", |arg| arg.help(t("File to remove", "要移除的文件")))
        })
}

pub(crate) fn localize_memory_command(mut command: clap::Command) -> clap::Command {
    let descriptions = [
        ("stats", "Show memory statistics", "显示记忆统计"),
        ("reset", "Clear assistant memory", "清空助手记忆"),
        ("search", "Search memories", "搜索记忆"),
        ("remember", "Save a manual fact", "手动保存事实"),
    ];
    for (name, en, zh) in descriptions {
        command = command.mut_subcommand(name, |subcommand| subcommand.about(t(en, zh)));
    }
    command
        .mut_subcommand("reset", |subcommand| {
            subcommand.mut_arg("include_skills", |arg| {
                arg.help(t(
                    "Also remove generated skills",
                    "同时移除自动生成的 skills",
                ))
            })
        })
        .mut_subcommand("search", |subcommand| {
            subcommand
                .mut_arg("query", |arg| arg.help(t("Search query", "搜索查询")))
                .mut_arg("limit", |arg| arg.help(t("Maximum results", "最大结果数")))
                .mut_arg("forgotten", |arg| {
                    arg.help(t("Include forgotten memories", "包含已遗忘记忆"))
                })
        })
        .mut_subcommand("remember", |subcommand| {
            subcommand
                .mut_arg("content", |arg| arg.help(t("Fact content", "事实内容")))
                .mut_arg("source", |arg| arg.help(t("Source label", "来源标签")))
        })
}

pub(crate) fn localize_skills_command(mut command: clap::Command) -> clap::Command {
    let descriptions = [
        ("list", "List skills", "列出 skills"),
        ("show", "Show a skill", "显示 skill"),
        ("enable", "Enable a skill", "启用 skill"),
        ("disable", "Disable a skill", "禁用 skill"),
        ("remove", "Remove a skill", "移除 skill"),
        ("stats", "Show skill statistics", "显示 skill 统计"),
        (
            "prune",
            "Remove disabled generated skills",
            "清理已禁用的自动 skills",
        ),
    ];
    for (name, en, zh) in descriptions {
        command = command.mut_subcommand(name, |subcommand| subcommand.about(t(en, zh)));
    }
    for name in ["show", "enable", "disable", "remove"] {
        command = command.mut_subcommand(name, |subcommand| {
            subcommand.mut_arg("name", |arg| arg.help(t("Skill name", "skill 名称")))
        });
    }
    command
}

#[derive(Debug, Subcommand)]
pub enum Command {
    #[command(name = "__alarm-worker", hide = true)]
    AlarmWorker(AlarmWorkerArgs),
    #[command(name = "__tool", hide = true)]
    Tool(ToolArgs),
    /// Internal: run as the GQY daemon (spawned by the CLI via
    /// `current_exe`, replacing the former separate `gqyd` binary).
    #[command(name = "__daemon", hide = true)]
    DaemonWorker(WebArgs),
    Ask(MessageArgs),
    Init,
    Paths,
    Config(ConfigArgs),
    Reload,
    Models(ModelsArgs),
    ListModels,
    Variant(VariantArgs),
    FishInit,
    BashInit,
    ZshInit,
    RemoveShellHook,
    History(HistoryArgs),
    Pop(PopArgs),
    Kb(KbArgs),
    Export(ExportArgs),
    Import(ImportArgs),
    UpdateDefaultKb,
    Memory(MemoryArgs),
    Skills(SkillsArgs),
    Reset,
    #[command(name = "reset-memory")]
    ResetMemoryCli,
    Wipe(WipeArgs),
    Web(WebArgs),
    Daemon(DaemonArgs),
    /// 进入普通模式 REPL(人格全能力)
    Normal,
    /// 进入开发模式 REPL(极简编码形态,无人格)
    Dev,
    /// 工具桥:以当前会话身份调用一个结构化工具(供 run_command 脚本编排)
    #[command(name = "tool-call")]
    ToolCallCmd(ToolCallArgs),
}

#[derive(Debug, Args)]
pub struct MessageArgs {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub message: Vec<String>,
}

#[derive(Debug, Args)]
pub struct WipeArgs {
    /// 跳过确认（供 shell hook 等非交互场景使用）。
    #[arg(long)]
    pub yes: bool,
}

#[derive(Args)]
pub struct WebArgs {
    #[arg(long, default_value_t = ipc::DEFAULT_WEB_PORT)]
    pub port: u16,

    /// WebUI 监听地址；默认 0.0.0.0（所有网卡），127.0.0.1 仅限本机访问。
    #[arg(long, value_name = "ADDR")]
    pub bind: Option<std::net::IpAddr>,

    #[arg(short = 'p', long, num_args = 0, default_missing_value = "")]
    pub password: Option<String>,

    #[arg(long, value_name = "PATH", conflicts_with = "password")]
    pub password_file: Option<PathBuf>,

    #[arg(skip)]
    pub port_explicit: bool,
}

#[derive(Debug, Args)]
pub struct DaemonArgs {
    #[arg(long, value_name = "PORT", global = true)]
    pub port: Option<u16>,

    #[command(subcommand)]
    pub command: Option<DaemonCommand>,
}

#[derive(Debug, Subcommand)]
pub enum DaemonCommand {
    Start,
    Stop,
    Restart,
    Status,
    Logs(DaemonLogsArgs),
}

#[derive(Debug, Args)]
pub struct DaemonLogsArgs {
    #[arg(short = 'n', long, value_name = "N")]
    pub lines: Option<usize>,

    /// `request`:开启出网请求录制并实时监控;Ctrl+C 停止并关闭录制
    #[arg(value_name = "TOPIC")]
    pub topic: Option<String>,
}

impl std::fmt::Debug for WebArgs {
    pub(crate) fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WebArgs")
            .field("port", &self.port)
            .field("password", &self.password.as_ref().map(|_| "<redacted>"))
            .field("password_file", &self.password_file)
            .field("port_explicit", &self.port_explicit)
            .finish()
    }
}

#[derive(Debug, Args)]
pub struct AlarmWorkerArgs {
    #[arg(long)]
    pub id: String,
    #[arg(long)]
    pub time: String,
    #[arg(long, default_value = "GQY alarm")]
    pub label: String,
    #[arg(long)]
    pub state_dir: PathBuf,
    #[arg(long)]
    pub cache_dir: PathBuf,
    #[arg(long)]
    pub audio_file: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct ToolArgs {
    pub name: String,
    pub arguments: Option<String>,
}

#[derive(Debug, Args)]
pub struct ToolCallArgs {
    /// 工具名(--list 时可省略)
    pub name: Option<String>,
    /// 参数 JSON(便捷位置参数;脚本里推荐 --stdin 免引号地狱)
    pub arguments: Option<String>,
    /// 从标准输入读参数 JSON(跨 shell 安全,PowerShell 也能用)
    #[arg(long = "stdin")]
    pub args_stdin: bool,
    /// 从文件读参数 JSON
    #[arg(long)]
    pub args_file: Option<std::path::PathBuf>,
    /// 列出当前可用工具(名称+显示名)
    #[arg(long)]
    pub list: bool,
    /// 打印指定工具的完整合同(描述+参数 schema)
    #[arg(long)]
    pub describe: bool,
}

#[derive(Debug, Args)]
pub struct ConfigArgs {
    #[command(subcommand)]
    pub command: Option<ConfigCommand>,
}

#[derive(Debug, Args)]
pub struct HistoryArgs {
    #[arg(short, long, default_value_t = 20)]
    pub limit: usize,

    #[arg(long)]
    pub raw: bool,

    #[arg(long)]
    pub no_thinking: bool,
}

#[derive(Debug, Args)]
pub struct PopArgs {
    #[arg(value_parser = parse_positive_pop_count)]
    pub count: Option<usize>,
}

#[derive(Debug, Args)]
pub struct ExportArgs {
    /// Where to write the archive; defaults to a host- and time-stamped name
    /// in the current directory.
    pub output: Option<PathBuf>,
    #[arg(long)]
    pub all: bool,
    #[arg(long)]
    pub index: bool,
    #[arg(long)]
    pub platforms: bool,
    #[arg(long)]
    pub no_secrets: bool,
    #[arg(long)]
    pub dry_run: bool,
    #[arg(long)]
    pub force: bool,
}

#[derive(Debug, Args)]
pub struct ImportArgs {
    pub archive: PathBuf,
    #[arg(long)]
    pub force: bool,
}

#[derive(Debug, Args)]
pub struct ModelsArgs {
    /// 1-based list index, `provider/model`, a bare model name, or
    /// `default` to follow the global active pool again.
    pub target: Option<String>,
}

#[derive(Debug, Args)]
pub struct VariantArgs {
    pub name: Option<String>,
}

#[derive(Debug, Args)]
pub struct KbArgs {
    #[command(subcommand)]
    pub command: KbCommand,
}

#[derive(Debug, Args)]
pub struct MemoryArgs {
    #[command(subcommand)]
    pub command: MemoryCommand,
}

#[derive(Debug, Subcommand)]
pub enum MemoryCommand {
    Stats,
    Reset(MemoryResetArgs),
    Search(MemorySearchArgs),
    Remember(MemoryRememberArgs),
}

#[derive(Debug, Args)]
pub struct MemoryResetArgs {
    #[arg(long)]
    pub include_skills: bool,
}

#[derive(Debug, Args)]
pub struct MemorySearchArgs {
    pub query: Vec<String>,
    #[arg(short, long)]
    pub limit: Option<usize>,
    #[arg(long)]
    pub forgotten: bool,
}

#[derive(Debug, Args)]
pub struct MemoryRememberArgs {
    pub content: Vec<String>,
    #[arg(short, long, default_value = "manual")]
    pub source: String,
}

#[derive(Debug, Args)]
pub struct SkillsArgs {
    #[command(subcommand)]
    pub command: SkillsCommand,
}

#[derive(Debug, Subcommand)]
pub enum SkillsCommand {
    List,
    Show(SkillNameArgs),
    Enable(SkillNameArgs),
    Disable(SkillNameArgs),
    Remove(SkillNameArgs),
    Stats,
    Prune,
}

#[derive(Debug, Args)]
pub struct SkillNameArgs {
    pub name: String,
}

#[derive(Debug, Subcommand)]
pub enum KbCommand {
    Add(KbAddArgs),
    List,
    Search(KbSearchArgs),
    Find(KbFindArgs),
    Read(KbReadArgs),
    Remove(KbRemoveArgs),
    Reindex,
    Stats,
    Embed(KbEmbedArgs),
}

#[derive(Debug, Args)]
pub struct KbAddArgs {
    pub path: PathBuf,
    #[arg(
        short,
        long,
        help = "Compatibility flag; directories are recursive by default"
    )]
    pub recursive: bool,
}

#[derive(Debug, Args)]
pub struct KbSearchArgs {
    pub query: Vec<String>,
    #[arg(short, long)]
    pub limit: Option<usize>,
}

#[derive(Debug, Args)]
pub struct KbFindArgs {
    pub query: Vec<String>,
    #[arg(short, long)]
    pub limit: Option<usize>,
}

#[derive(Debug, Args)]
pub struct KbReadArgs {
    pub file: String,
    #[arg(long, default_value_t = 1)]
    pub start: usize,
    #[arg(long)]
    pub lines: Option<usize>,
}
