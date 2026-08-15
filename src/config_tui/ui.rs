//! ui — 自 src/config_tui.rs 拆分。

use super::*;

use crate::config::{
    merge_real_context_settings, ActiveProviderModelConfig, ApiQuotaAccountConfig,
    ApiQuotaProviderConfig, AppConfig, PlatformCommandPermission, PlatformConversationConfig,
    PlatformConversationKind, PlatformModelPoolInheritance, PlatformModelRoute,
    PlatformPersonaOverride, PlatformRateLimit, PlatformSessionLimits, ProviderConfig,
    ProviderModelChoice, QqMemeCollectorPluginSettings, QqMessageHistoryPluginSettings,
    RealContextIdentityMapping, RealContextPluginSettings, MAX_COMMAND_OUTPUT_LINES,
    MAX_PLATFORM_COMMAND_PREFIX_CHARS, MAX_PLATFORM_SESSION_QUEUED, MAX_PLATFORM_SESSION_RUNNING,
    MAX_REPL_REPLAY_TURNS, QQ_MEME_COLLECTOR_PLUGIN_ID, QQ_MESSAGE_HISTORY_PLUGIN_ID,
    REAL_CONTEXT_PLUGIN_ID,
};
use crate::default_models::{OPENCODE_DEFAULT_VISION_MODEL, OPENCODE_PROVIDER_ID};
use crate::i18n::{is_zh, text as t};
use crate::llm::{
    thinking_variant_options_for_model, ThinkingVariantOptions, ThinkingVariantPreferences,
};
use crate::paths::GQYPaths;
use crate::platforms::commands::{self, PlatformCommandDescriptor};
use crate::platforms::plugins::{
    active_judgement_skip_ids, apply_active_judgement_skip_editor_changes,
};
use crate::state::StateStore;
use anyhow::{bail, Result};
use crossterm::cursor::{Hide, MoveTo, Show};
use crossterm::event::{self, Event, KeyCode, KeyEvent};
use crossterm::style::{Attribute, Print, SetAttribute};
use crossterm::terminal::{self, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{execute, queue};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::Command;
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

pub fn run(paths: &GQYPaths) -> Result<bool> {
    AppConfig::init_files(paths)?;
    crate::models_cache::try_load(paths);
    crate::models_cache::spawn_background_refresh(paths.clone());
    let config = AppConfig::load_or_default(paths)?;
    let thinking_variants = ThinkingVariantPreferences::load(paths);
    TerminalSession::start()?.run(paths, config, thinking_variants)
}

pub(crate) struct TerminalSession {
    stdout: io::Stdout,
}

impl TerminalSession {
    pub(crate) fn start() -> Result<Self> {
        terminal::enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen, Hide)?;
        Ok(Self { stdout })
    }

    pub(crate) fn run(
        mut self,
        paths: &GQYPaths,
        mut config: AppConfig,
        mut thinking_variants: ThinkingVariantPreferences,
    ) -> Result<bool> {
        let result = run_main_menu(&mut self.stdout, paths, &mut config, &mut thinking_variants);
        execute!(self.stdout, Show, LeaveAlternateScreen)?;
        terminal::disable_raw_mode()?;
        result
    }
}

impl Drop for TerminalSession {
    pub(crate) fn drop(&mut self) {
        let _ = execute!(self.stdout, Show, LeaveAlternateScreen);
        let _ = terminal::disable_raw_mode();
    }
}

pub(crate) fn run_main_menu(
    stdout: &mut io::Stdout,
    paths: &GQYPaths,
    config: &mut AppConfig,
    thinking_variants: &mut ThinkingVariantPreferences,
) -> Result<bool> {
    // Detects edits on quit; sub-menus mutate `config` in place without any
    // dirty flag of their own.
    let pristine_config = serde_json::to_string(config).ok();
    let mut selected = 0usize;
    loop {
        let active = active_label(config);
        let multimodal = active_multimodal_label(config);
        let options = [
            t("Providers and models", "供应商和模型").to_string(),
            format!(
                "{} ({}: {active})",
                t("Configure text model", "配置文本模型"),
                t("Current", "当前")
            ),
            format!(
                "{} ({}: {multimodal})",
                t("Configure multimodal model", "配置多模态模型"),
                t("Current", "当前")
            ),
            format!(
                "{} ({}: {})",
                t("Configure embedding model", "配置 Embedding 模型"),
                t("Current", "当前"),
                embedding_model_label(config)
            ),
            format!(
                "{} ({})",
                t("Configure subagent tier pools", "配置子代理档位池"),
                subagent_tiers_label(config)
            ),
            t("Plugins", "插件配置").to_string(),
            t("Custom prompts", "自定义提示词").to_string(),
            format!(
                "{} ({})",
                t("IM platforms", "接入通讯平台"),
                platforms_label(config)
            ),
            t("Global settings", "全局参数设置").to_string(),
            t("Save and exit", "保存并退出").to_string(),
        ];
        draw_menu(
            stdout,
            t(" GQY CONFIG ", " GQY 配置 "),
            &options,
            selected,
            "",
        )?;

        match read_key()? {
            KeyCode::Char('q') | KeyCode::Esc => {
                let dirty = thinking_variants.is_dirty()
                    || serde_json::to_string(config).ok() != pristine_config;
                if !dirty {
                    return Ok(false);
                }
                if confirm_save_on_exit(stdout)? {
                    config.save(paths)?;
                    thinking_variants.save(paths)?;
                    return Ok(true);
                }
                return Ok(false);
            }
            KeyCode::Up | KeyCode::Char('k') => selected = selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => selected = (selected + 1).min(options.len() - 1),
            KeyCode::Enter => match selected {
                0 => ProviderBrowser::new(paths, config, thinking_variants).run(stdout)?,
                1 => select_active_provider(stdout, config)?,
                2 => select_active_multimodal_provider(stdout, config)?,
                3 => edit_embedding_model(stdout, config)?,
                4 => select_subagent_tiers(stdout, config)?,
                5 => edit_plugins(stdout, config)?,
                6 => edit_custom_prompts(stdout, paths, config)?,
                7 => select_platforms(stdout, paths, config)?,
                8 => edit_settings(stdout, config)?,
                9 => {
                    config.save(paths)?;
                    thinking_variants.save(paths)?;
                    return Ok(true);
                }
                _ => {}
            },
            _ => {}
        }
    }
}

pub(crate) fn edit_plugins(stdout: &mut io::Stdout, config: &mut AppConfig) -> Result<()> {
    let mut selected = 0usize;
    loop {
        let count = plugin_names().len();
        draw_plugin_menu(stdout, config, selected)?;
        match read_key()? {
            KeyCode::Esc | KeyCode::Char('q') => return Ok(()),
            KeyCode::Up | KeyCode::Char('k') => selected = selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => selected = (selected + 1).min(count - 1),
            KeyCode::Char(' ') => toggle_plugin(config, selected),
            KeyCode::Enter | KeyCode::Char('i') => edit_plugin_detail(stdout, config, selected)?,
            _ => {}
        }
    }
}

pub(crate) fn draw_plugin_menu(
    stdout: &mut io::Stdout,
    config: &AppConfig,
    selected: usize,
) -> Result<()> {
    let (cols, rows) = terminal::size()?;
    let width = cols.saturating_sub(4).max(60);
    let height = rows.saturating_sub(2).max(10);
    let x = 2;
    let y = 1;
    queue!(stdout, Clear(ClearType::All))?;
    draw_box(stdout, x, y, width, height, t(" PLUGINS ", " 插件 "))?;
    queue!(
        stdout,
        MoveTo(x + 2, y + 1),
        Print(t(
            "[Space]enable/disable [Enter]configure [j/k]move [q]back",
            "[Space]启用/禁用 [Enter]配置 [j/k]移动 [q]返回",
        ))
    )?;
    queue!(
        stdout,
        MoveTo(x + 2, y + 3),
        SetAttribute(Attribute::Bold),
        Print(pad(
            &plugin_row(
                t("Status", "状态"),
                t("Plugin", "插件"),
                t("Description", "说明"),
                width.saturating_sub(4) as usize,
            ),
            width.saturating_sub(4) as usize,
        )),
        SetAttribute(Attribute::Reset)
    )?;
    let plugins = plugin_names();
    let visible_rows = height.saturating_sub(6) as usize;
    let start = selected.saturating_sub(visible_rows.saturating_sub(1));
    for row in 0..visible_rows {
        let index = start + row;
        if index >= plugins.len() {
            break;
        }
        let (_, name, description) = plugins[index];
        let state = if plugin_enabled(config, index) {
            t("[ON]", "[开]")
        } else {
            t("[OFF]", "[关]")
        };
        let line = plugin_row(state, name, description, width.saturating_sub(4) as usize);
        queue!(stdout, MoveTo(x + 2, y + row as u16 + 4))?;
        if index == selected {
            queue!(
                stdout,
                SetAttribute(Attribute::Reverse),
                Print(pad(&line, width.saturating_sub(4) as usize)),
                SetAttribute(Attribute::Reset)
            )?;
        } else {
            queue!(stdout, Print(pad(&line, width.saturating_sub(4) as usize)))?;
        }
    }
    stdout.flush()?;
    Ok(())
}

pub(crate) fn plugin_row(state: &str, name: &str, description: &str, width: usize) -> String {
    let fixed = pad(state, 8) + &pad(name, 24);
    let remaining = width.saturating_sub(display_width(&fixed)).max(10);
    fixed + &truncate(description, remaining)
}

pub(crate) fn plugin_names() -> [(&'static str, &'static str, &'static str); 13] {
    [
        (
            "web",
            t("Web search", "网络搜索"),
            t(
                "Search APIs with script fallback",
                "搜索 API 与脚本 fallback",
            ),
        ),
        (
            "deep_research",
            t("Deep research", "深度研究"),
            t(
                "Run long research tasks and output Markdown",
                "长任务研究并输出 Markdown",
            ),
        ),
        (
            "vision",
            t("Vision", "识图"),
            t(
                "Image understanding and terminal preview",
                "图片理解和终端预览",
            ),
        ),
        (
            "image_generation",
            t("Image generation", "生图"),
            t("Generate images from text", "文本生成图片"),
        ),
        (
            "web_images",
            t("Image search", "搜图"),
            t(
                "Search, download, and review web images",
                "网络图片搜索、下载与审核",
            ),
        ),
        (
            "print_image",
            t("Print image", "打印图片"),
            t("Terminal image print size", "终端图片打印尺寸"),
        ),
        (
            "memes",
            t("Memes", "表情包"),
            t("Persona meme library and send size", "人格表情库与发送尺寸"),
        ),
        (
            "knowledge_base",
            t("Knowledge base", "知识库"),
            t(
                "Local file search and semantic index",
                "本地文件检索与语义索引",
            ),
        ),
        (
            "brew",
            "Homebrew",
            t(
                "Homebrew status and package lookup",
                "Homebrew 状态与包查询",
            ),
        ),
        (
            "man",
            t("Online manuals", "在线手册"),
            t(
                "Search and read online man pages",
                "在线 man 手册搜索与读取",
            ),
        ),
        (
            "memory",
            t("Memory", "记忆"),
            t("Long-term memory and association", "长期记忆与联想"),
        ),
        (
            "package_advisor",
            t("Homebrew review", "Homebrew 审查"),
            t("Formula/cask security review", "Formula/cask 安全审查"),
        ),
        (
            "api_quota",
            t("LLM API quota", "大模型额度查询"),
            t(
                "Query DeepSeek and OpenRouter API quota",
                "查询 DeepSeek 与 OpenRouter API 额度",
            ),
        ),
    ]
}

pub(crate) fn plugin_enabled(config: &AppConfig, index: usize) -> bool {
    match index {
        0 => config.plugins.web.enabled,
        1 => config.plugins.deep_research.enabled,
        2 => config.plugins.vision.enabled,
        3 => config.plugins.image_generation.enabled,
        4 => config.plugins.web_images.enabled,
        5 => config.plugins.print_image.enabled,
        6 => config.plugins.memes.enabled,
        7 => config.plugins.knowledge_base.enabled,
        8 => config.plugins.brew.enabled,
        9 => config.plugins.man.enabled,
        10 => config.plugins.memory.enabled,
        11 => config.plugins.package_advisor.enabled,
        12 => config.plugins.api_quota.enabled,
        _ => false,
    }
}

pub(crate) fn toggle_plugin(config: &mut AppConfig, index: usize) {
    let value = !plugin_enabled(config, index);
    match index {
        0 => config.plugins.web.enabled = value,
        1 => config.plugins.deep_research.enabled = value,
        2 => config.plugins.vision.enabled = value,
        3 => config.plugins.image_generation.enabled = value,
        4 => config.plugins.web_images.enabled = value,
        5 => config.plugins.print_image.enabled = value,
        6 => config.plugins.memes.enabled = value,
        7 => config.plugins.knowledge_base.enabled = value,
        8 => config.plugins.brew.enabled = value,
        9 => config.plugins.man.enabled = value,
        10 => config.plugins.memory.enabled = value,
        11 => config.plugins.package_advisor.enabled = value,
        12 => config.plugins.api_quota.enabled = value,
        _ => {}
    }
}

pub(crate) fn edit_plugin_detail(
    stdout: &mut io::Stdout,
    config: &mut AppConfig,
    index: usize,
) -> Result<()> {
    if index == 13 {
        return edit_api_quota(stdout, config);
    }
    let title = format!(" {}: {} ", t("PLUGIN", "插件"), plugin_names()[index].1);
    let mut fields = plugin_fields(config, index);
    if !run_form(stdout, &title, &mut fields)? {
        return Ok(());
    }
    apply_plugin_fields(config, index, &fields)
}

pub(crate) fn edit_api_quota(stdout: &mut io::Stdout, config: &mut AppConfig) -> Result<()> {
    let mut selected = 0usize;
    loop {
        let options = [
            format!(
                "DeepSeek ({})",
                configured_count(&config.plugins.api_quota.deepseek)
            ),
            format!(
                "OpenRouter ({})",
                configured_count(&config.plugins.api_quota.openrouter)
            ),
        ];
        draw_menu(
            stdout,
            t(" LLM API QUOTA ", " 大模型额度查询 "),
            &options,
            selected,
            t("[Enter]configure [q]back", "[Enter]配置 [q]返回"),
        )?;
        match read_key()? {
            KeyCode::Esc | KeyCode::Char('q') => return Ok(()),
            KeyCode::Up | KeyCode::Char('k') => selected = selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => selected = (selected + 1).min(1),
            KeyCode::Enter | KeyCode::Char('i') => {
                if selected == 0 {
                    edit_api_quota_accounts(
                        stdout,
                        "DeepSeek",
                        &mut config.plugins.api_quota.deepseek,
                    )?;
                } else {
                    edit_api_quota_accounts(
                        stdout,
                        "OpenRouter",
                        &mut config.plugins.api_quota.openrouter,
                    )?;
                }
            }
            _ => {}
        }
    }
}

pub(crate) fn edit_api_quota_accounts(
    stdout: &mut io::Stdout,
    name: &str,
    config: &mut ApiQuotaProviderConfig,
) -> Result<()> {
    if config.accounts.is_empty() {
        config.accounts.push(ApiQuotaAccountConfig {
            id: "account-1".to_string(),
            name: "默认账号".to_string(),
            api_key: std::mem::take(&mut config.api_key),
        });
    }
    let mut selected = 0usize;
    loop {
        let mut options = config
            .accounts
            .iter()
            .map(|account| {
                format!(
                    "{} ({})",
                    account.name,
                    if account.api_key.trim().is_empty() {
                        t("not configured", "未配置")
                    } else {
                        t("configured", "已配置")
                    }
                )
            })
            .collect::<Vec<_>>();
        options.push(t("New account", "新建账号").to_string());
        selected = selected.min(options.len().saturating_sub(1));
        draw_menu(
            stdout,
            &format!(" {name} "),
            &options,
            selected,
            if name == "DeepSeek" {
                t(
                    "[Enter]edit [n]new [d]delete",
                    "[Enter]编辑 [n]新建 [d]删除",
                )
            } else {
                t(
                    "[Enter]edit [n]new [d]delete [q]back",
                    "[Enter]编辑 [n]新建 [d]删除 [q]返回",
                )
            },
        )?;
        match read_key()? {
            KeyCode::Esc | KeyCode::Char('q') => return Ok(()),
            KeyCode::Up | KeyCode::Char('k') => selected = selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                selected = (selected + 1).min(options.len().saturating_sub(1))
            }
            KeyCode::Char('n') => {
                if config.accounts.len() < 32 && add_api_quota_account(stdout, config, name)? {
                    selected = config.accounts.len().saturating_sub(1);
                }
            }
            KeyCode::Char('d') if selected < config.accounts.len() => {
                if confirm_api_quota_delete(stdout, &config.accounts[selected].name)? {
                    if config.accounts.len() == 1 {
                        config.accounts[0].name = "默认账号".to_string();
                        config.accounts[0].api_key.clear();
                    } else {
                        config.accounts.remove(selected);
                        selected = selected.min(config.accounts.len() - 1);
                    }
                }
            }
            KeyCode::Enter | KeyCode::Char('i') if selected < config.accounts.len() => {
                let _ = edit_api_quota_account(stdout, name, &mut config.accounts[selected])?;
            }
            KeyCode::Enter | KeyCode::Char('i') => {
                if config.accounts.len() < 32 && add_api_quota_account(stdout, config, name)? {
                    selected = config.accounts.len().saturating_sub(1);
                }
            }
            _ => {}
        }
    }
}

/// true = save and exit, false = discard and exit. A choice is mandatory:
/// `q`/`Esc` are ignored so an accidental key press cannot lose edits.
pub(crate) fn confirm_save_on_exit(stdout: &mut io::Stdout) -> Result<bool> {
    let options = [
        t("Save", "保存").to_string(),
        t("Discard", "不保存").to_string(),
    ];
    let mut selected = 0usize;
    loop {
        draw_menu(
            stdout,
            t(" SAVE EDITED CHANGES? ", " 是否保存已编辑内容 "),
            &options,
            selected,
            t("[j/k]move [Enter]confirm", "[j/k]移动 [Enter]确认"),
        )?;
        match read_key()? {
            KeyCode::Up | KeyCode::Char('k') => selected = selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => selected = (selected + 1).min(1),
            KeyCode::Enter => return Ok(selected == 0),
            _ => {}
        }
    }
}

pub(crate) fn confirm_api_quota_delete(stdout: &mut io::Stdout, account: &str) -> Result<bool> {
    let options = [
        t("Cancel", "取消").to_string(),
        format!("{}: {account}", t("Delete", "删除")),
    ];
    let mut selected = 0usize;
    loop {
        draw_menu(
            stdout,
            t(" DELETE ACCOUNT ", " 删除账号 "),
            &options,
            selected,
            t("[Enter]confirm [q]cancel", "[Enter]确认 [q]取消"),
        )?;
        match read_key()? {
            KeyCode::Esc | KeyCode::Char('q') => return Ok(false),
            KeyCode::Up | KeyCode::Char('k') => selected = selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => selected = (selected + 1).min(1),
            KeyCode::Enter => return Ok(selected == 1),
            _ => {}
        }
    }
}

pub(crate) fn edit_api_quota_account(
    stdout: &mut io::Stdout,
    provider: &str,
    account: &mut ApiQuotaAccountConfig,
) -> Result<bool> {
    let mut fields = vec![
        Field::new(t("Account name", "账号名称"), account.name.clone()),
        Field::new("API Key", account.api_key.clone()).sensitive(),
    ];
    if run_form(stdout, &format!(" {provider} "), &mut fields)? {
        account.name = fields[0].value.trim().to_string();
        if account.name.is_empty() {
            account.name = "默认账号".to_string();
        }
        account.api_key = fields[1].value.trim().to_string();
        return Ok(true);
    }
    Ok(false)
}

pub(crate) fn add_api_quota_account(
    stdout: &mut io::Stdout,
    config: &mut ApiQuotaProviderConfig,
    provider: &str,
) -> Result<bool> {
    let name = next_api_quota_account_name(config);
    let id = next_api_quota_account_id(config);
    config.accounts.push(ApiQuotaAccountConfig {
        id,
        name,
        api_key: String::new(),
    });
    let index = config.accounts.len() - 1;
    if edit_api_quota_account(stdout, provider, &mut config.accounts[index])? {
        Ok(true)
    } else {
        config.accounts.pop();
        Ok(false)
    }
}

pub(crate) fn next_api_quota_account_id(_config: &ApiQuotaProviderConfig) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    pub(crate) static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    let sequence = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("account-{nanos}-{sequence}")
}

pub(crate) fn next_api_quota_account_name(config: &ApiQuotaProviderConfig) -> String {
    let mut number = 2usize;
    loop {
        let candidate = format!("账号 {number}");
        if config
            .accounts
            .iter()
            .all(|account| account.name != candidate)
        {
            return candidate;
        }
        number += 1;
    }
}

pub(crate) fn configured_count(config: &ApiQuotaProviderConfig) -> String {
    let count = if config.accounts.is_empty() {
        usize::from(!config.api_key.trim().is_empty())
    } else {
        config
            .accounts
            .iter()
            .filter(|account| !account.api_key.trim().is_empty())
            .count()
    };
    if is_zh() {
        format!("{count} 个已配置")
    } else {
        format!("{count} configured")
    }
}

pub(crate) fn plugin_fields(config: &AppConfig, index: usize) -> Vec<Field> {
    match index {
        0 => vec![
            Field::boolean(t("Enabled", "启用"), config.plugins.web.enabled),
            Field::new(
                t("Results per request", "每次返回数量"),
                config.plugins.web.max_results.to_string(),
            ),
            Field::textarea(
                "Tavily API Keys",
                config.plugins.web.tavily_api_keys.join("\n"),
            )
            .sensitive(),
            Field::textarea(
                "Firecrawl API Keys",
                config.plugins.web.firecrawl_api_keys.join("\n"),
            )
            .sensitive(),
            Field::textarea(
                "AnySearch API Keys",
                config.plugins.web.anysearch_api_keys.join("\n"),
            )
            .sensitive(),
            Field::textarea(
                t(
                    "Exa API Keys (optional; keyless free quota)",
                    "Exa API Keys（可留空用免费额度）",
                ),
                config.plugins.web.exa_api_keys.join("\n"),
            )
            .sensitive(),
            Field::new("SearXNG URL", config.plugins.web.searxng_base_url.clone()),
        ],
        1 => vec![
            Field::boolean(t("Enabled", "启用"), config.plugins.deep_research.enabled),
            Field::new(
                t("Output directory", "输出目录"),
                config.plugins.deep_research.output_dir.clone(),
            ),
            Field::new(
                t("Thinking depth", "思考深度"),
                config.plugins.deep_research.thinking_depth.clone(),
            )
            .choices(&["minimal", "low", "medium", "high", "xhigh"]),
            Field::new(
                t("Maximum review revisions", "最大审视修正次数"),
                config
                    .plugins
                    .deep_research
                    .max_review_revisions
                    .to_string(),
            ),
            Field::new(
                t("Tool steps per round", "每轮工具步数"),
                config
                    .plugins
                    .deep_research
                    .max_tool_steps_per_round
                    .to_string(),
            ),
            Field::new(
                t("Final answer character limit", "最终字数上限"),
                config
                    .plugins
                    .deep_research
                    .max_final_answer_chars
                    .to_string(),
            ),
            Field::new(
                t("Tool timeout (seconds)", "工具超时秒数"),
                config
                    .plugins
                    .deep_research
                    .tool_call_timeout_seconds
                    .to_string(),
            ),
            Field::boolean(
                t("Show progress", "显示过程进度"),
                config.plugins.deep_research.show_progress,
            ),
        ],
        2 => vec![
            Field::boolean(t("Enabled", "启用"), config.plugins.vision.enabled),
            Field::boolean(
                t(
                    "Prefer current chat model for images",
                    "优先使用当前对话模型识图",
                ),
                config.plugins.vision.prefer_current_multimodal_model,
            ),
            Field::new(
                t("Vision provider/model", "识图 Provider/模型"),
                vision_provider_value(config),
            )
            .choices_owned(vision_provider_model_choice_values(config)),
            Field::new(
                t("Response header timeout (seconds)", "响应头超时秒数"),
                config
                    .plugins
                    .vision
                    .response_header_timeout_seconds
                    .to_string(),
            ),
            Field::new(
                t("Stream idle timeout (seconds)", "流空闲超时秒数"),
                config
                    .plugins
                    .vision
                    .stream_idle_timeout_seconds
                    .to_string(),
            ),
            Field::new(
                t("Per-image timeout (seconds)", "单图总超时秒数"),
                config.plugins.vision.image_timeout_seconds.to_string(),
            ),
        ],
        3 => vec![
            Field::boolean(
                t("Enabled", "启用"),
                config.plugins.image_generation.enabled,
            ),
            Field::new(
                t("Image API type", "生图 API 类型"),
                config.plugins.image_generation.provider_type.clone(),
            )
            .choices(&["openai", "rightcode"]),
            Field::new("Base URL", config.plugins.image_generation.base_url.clone()),
            Field::textarea(
                "API Keys",
                config.plugins.image_generation.api_keys.join("\n"),
            )
            .sensitive(),
            Field::new(
                t("Model", "模型"),
                config.plugins.image_generation.model.clone(),
            ),
            Field::new(
                t("Default aspect ratio", "默认宽高比"),
                config.plugins.image_generation.default_aspect_ratio.clone(),
            )
            .choices(&[
                "自动", "1:1", "2:3", "3:2", "3:4", "4:3", "4:5", "5:4", "9:16", "16:9", "21:9",
            ]),
            Field::new(
                t("Default resolution", "默认分辨率"),
                config.plugins.image_generation.default_resolution.clone(),
            )
            .choices(&["1K", "2K", "4K"]),
            Field::new(
                t("Output directory", "输出目录"),
                config.plugins.image_generation.output_dir.clone(),
            ),
            Field::boolean(
                t("Print when complete", "完成后打印"),
                config.plugins.image_generation.auto_print,
            ),
            Field::new(
                t("Timeout (seconds)", "超时秒数"),
                config.plugins.image_generation.timeout_seconds.to_string(),
            ),
        ],
        4 => vec![
            Field::boolean(t("Enabled", "启用"), config.plugins.web_images.enabled),
            Field::new(
                t("Search source mode", "搜索来源模式"),
                config.plugins.web_images.source_mode.clone(),
            )
            .choices(&["auto", "global", "mainland"]),
            Field::boolean(
                t("Vision model review", "视觉模型审核"),
                config.plugins.web_images.vision_screening_enabled,
            ),
            Field::new(
                t("Maximum results", "数量上限"),
                config.plugins.web_images.max_results.to_string(),
            ),
            Field::boolean(
                t("Safe search", "安全搜索"),
                config.plugins.web_images.safe_search,
            ),
            Field::boolean(
                t("Automatic preview", "自动预览"),
                config.plugins.web_images.auto_preview,
            ),
            Field::new(
                t("Default preview count", "默认预览数量"),
                config.plugins.web_images.preview_count.to_string(),
            ),
            Field::new(
                t("Maximum download (MB)", "最大下载 MB"),
                config.plugins.web_images.max_download_mb.to_string(),
            ),
            Field::new(
                t("Timeout (seconds)", "超时秒数"),
                config.plugins.web_images.timeout_seconds.to_string(),
            ),
        ],
        5 => vec![
            Field::boolean(t("Enabled", "启用"), config.plugins.print_image.enabled),
            Field::new(
                t("Print width percent", "打印宽度百分比"),
                config.plugins.print_image.width_percent.to_string(),
            ),
            Field::new(
                t("Print height percent", "打印高度百分比"),
                config.plugins.print_image.height_percent.to_string(),
            ),
        ],
        6 => vec![
            Field::boolean(t("Enabled", "启用"), config.plugins.memes.enabled),
            Field::new(
                t("Send width percent", "发送宽度百分比"),
                config.plugins.memes.width_percent.to_string(),
            ),
            Field::new(
                t("Send height percent", "发送高度百分比"),
                config.plugins.memes.height_percent.to_string(),
            ),
            Field::new(
                t("Maximum image size (MB)", "最大图片 MB"),
                config.plugins.memes.max_image_mb.to_string(),
            ),
            Field::new(
                t("Maximum search results", "搜索最大结果数"),
                config.plugins.memes.search_max_results.to_string(),
            ),
            Field::boolean(
                t("Allow animated GIFs", "允许 GIF 动画"),
                config.plugins.memes.allow_gif_animation,
            ),
            Field::boolean(
                t("Suggest memes automatically", "自动提示发送表情"),
                config.plugins.memes.auto_send_enabled,
            ),
            Field::boolean(
                t(
                    "Suggest memes automatically on platforms",
                    "通讯平台自动提示发送表情",
                ),
                config.plugins.memes.auto_send_platform_enabled,
            ),
            Field::new(
                t(
                    "Automatic meme suggestion probability",
                    "自动提示发送表情概率",
                ),
                config.plugins.memes.auto_send_probability.to_string(),
            ),
        ],
        7 => vec![
            Field::boolean(t("Enabled", "启用"), config.plugins.knowledge_base.enabled),
            Field::new(
                t("Knowledge base directory", "知识库目录"),
                config.plugins.knowledge_base.data_dir.clone(),
            ),
            Field::new(
                t("Maximum search results", "搜索最大结果数"),
                config.plugins.knowledge_base.max_search_results.to_string(),
            ),
            Field::new(
                t("Snippet context characters", "片段上下文字数"),
                config
                    .plugins
                    .knowledge_base
                    .snippet_context_chars
                    .to_string(),
            ),
            Field::new(
                t("Proximity window characters", "同窗匹配范围"),
                config
                    .plugins
                    .knowledge_base
                    .proximity_window_chars
                    .to_string(),
            ),
            Field::new(
                t("Maximum lines to read", "读取最大行数"),
                config.plugins.knowledge_base.max_read_lines.to_string(),
            ),
            Field::new(
                t("Maximum file size (KB)", "最大文件 KB"),
                config.plugins.knowledge_base.max_file_size_kb.to_string(),
            ),
            Field::boolean(
                t("Allow AI uploads", "允许 AI 上传"),
                config.plugins.knowledge_base.upload_tool_enabled,
            ),
            Field::boolean(
                t("Enable embedding", "启用 Embedding"),
                config.plugins.knowledge_base.embedding_enabled,
            ),
            Field::new(
                t("Embedding provider/model", "Embedding Provider/模型"),
                kb_embedding_provider_value(config),
            )
            .choices_owned(provider_model_choice_values(config, false))
            .empty_choice_label(t("Embedding not configured", "未配置 Embedding")),
            Field::new(
                t("Semantic chunk size", "语义块大小"),
                config
                    .plugins
                    .knowledge_base
                    .semantic_chunk_chars
                    .to_string(),
            ),
            Field::new(
                t("Semantic chunk overlap", "语义块重叠"),
                config
                    .plugins
                    .knowledge_base
                    .semantic_chunk_overlap
                    .to_string(),
            ),
            Field::new(
                t("Semantic candidates", "语义候选数"),
                config.plugins.knowledge_base.semantic_top_k.to_string(),
            ),
            Field::new(
                t("Minimum semantic score", "语义最低分"),
                config.plugins.knowledge_base.semantic_min_score.to_string(),
            ),
            Field::new(
                t("Strong keyword match threshold", "关键词强命中阈值"),
                config
                    .plugins
                    .knowledge_base
                    .keyword_strong_score_threshold
                    .to_string(),
            ),
            Field::new(
                t("Embedding timeout (seconds)", "Embedding 超时秒数"),
                config
                    .plugins
                    .knowledge_base
                    .embedding_timeout_seconds
                    .to_string(),
            ),
        ],
        8 => vec![Field::boolean(
            t("Enabled", "启用"),
            config.plugins.brew.enabled,
        )],
        9 => vec![Field::boolean(
            t("Enabled", "启用"),
            config.plugins.man.enabled,
        )],
        10 => {
            let memory = config.memory_config();
            vec![
                Field::boolean(t("Enabled", "启用"), memory.enabled),
                Field::boolean(
                    t("Evicted context cache", "上下文弹出缓存"),
                    memory.evicted_context_enabled,
                ),
                Field::boolean(
                    t("Enable association", "联想启用"),
                    memory.association_enabled,
                ),
                Field::boolean(t("Automatic diary", "自动日记"), memory.auto_diary_enabled),
                Field::boolean(
                    t("Automatic fact memory", "自动知识记忆"),
                    memory.auto_fact_enabled,
                ),
                Field::new(
                    t("Diary batch size", "日记整理轮数"),
                    memory.diary_batch_size.to_string(),
                ),
                Field::new(
                    t("Short diary retention days", "短期日记保留天数"),
                    memory.short_diary_retention_days.to_string(),
                ),
                Field::new(
                    t("Diary promotion recalls", "日记长期化召回次数"),
                    memory.diary_promotion_recalls.to_string(),
                ),
                Field::new(
                    t("Organizer timeout seconds", "记忆整理超时秒数"),
                    memory.organizer_timeout_seconds.to_string(),
                ),
                Field::new(
                    t("Associated facts", "联想知识条数"),
                    memory.association_facts.to_string(),
                ),
                Field::new(
                    t("Associated events", "联想事件条数"),
                    memory.association_episodes.to_string(),
                ),
                Field::new(
                    t("Association character limit", "联想字符上限"),
                    memory.association_max_chars.to_string(),
                ),
                Field::boolean(
                    t("Enable forgetting", "遗忘启用"),
                    memory.forgetting_enabled,
                ),
                Field::new(
                    t("Forgetting half-life (days)", "遗忘半衰期天"),
                    memory.forgetting_half_life_days.to_string(),
                ),
                Field::new(
                    t("Minimum forgetting strength", "遗忘最低强度"),
                    memory.forgetting_min_strength.to_string(),
                ),
                Field::new(
                    t("Recall boost strength", "回忆增强强度"),
                    memory.forgetting_review_boost.to_string(),
                ),
                Field::boolean(
                    t("Association dedup", "联想跨回合去重"),
                    memory.association_dedup,
                ),
            ]
        }
        11 => vec![Field::boolean(
            t("Enabled", "启用"),
            config.plugins.package_advisor.enabled,
        )],
        12 => vec![Field::boolean(
            t("Enabled", "启用"),
            config.plugins.api_quota.enabled,
        )],
        _ => vec![Field::boolean(
            t("Enabled", "启用"),
            plugin_enabled(config, index),
        )],
    }
}

pub(crate) fn apply_plugin_fields(
    config: &mut AppConfig,
    index: usize,
    fields: &[Field],
) -> Result<()> {
    match index {
        0 => {
            config.plugins.web.enabled = parse_bool_field(&fields[0].value)?;
            config.plugins.web.max_results = fields[1].value.trim().parse::<usize>()?.clamp(1, 10);
            config.plugins.web.tavily_api_keys = parse_key_list(&fields[2].value);
            config.plugins.web.firecrawl_api_keys = parse_key_list(&fields[3].value);
            config.plugins.web.anysearch_api_keys = parse_key_list(&fields[4].value);
            config.plugins.web.exa_api_keys = parse_key_list(&fields[5].value);
            config.plugins.web.searxng_base_url =
                fields[6].value.trim().trim_end_matches('/').to_string();
        }
        1 => {
            config.plugins.deep_research.enabled = parse_bool_field(&fields[0].value)?;
            config.plugins.deep_research.output_dir = fields[1].value.trim().to_string();
            config.plugins.deep_research.thinking_depth = fields[2].value.trim().to_string();
            config.plugins.deep_research.max_review_revisions = fields[3].value.trim().parse()?;
            config.plugins.deep_research.max_tool_steps_per_round =
                fields[4].value.trim().parse()?;
            config.plugins.deep_research.max_final_answer_chars = fields[5].value.trim().parse()?;
            config.plugins.deep_research.tool_call_timeout_seconds =
                fields[6].value.trim().parse()?;
            config.plugins.deep_research.show_progress = parse_bool_field(&fields[7].value)?;
        }
        2 => {
            config.plugins.vision.enabled = parse_bool_field(&fields[0].value)?;
            config.plugins.vision.prefer_current_multimodal_model =
                parse_bool_field(&fields[1].value)?;
            let (provider_id, model) = parse_provider_model_choice(&fields[2].value);
            config.plugins.vision.vision_provider_id = provider_id;
            config.plugins.vision.vision_model = model;
            config.plugins.vision.response_header_timeout_seconds =
                fields[3].value.trim().parse::<u64>()?.max(1);
            config.plugins.vision.stream_idle_timeout_seconds =
                fields[4].value.trim().parse::<u64>()?.max(1);
            config.plugins.vision.image_timeout_seconds =
                fields[5].value.trim().parse::<u64>()?.max(1);
        }
        3 => {
            config.plugins.image_generation.enabled = parse_bool_field(&fields[0].value)?;
            config.plugins.image_generation.provider_type = fields[1].value.trim().to_string();
            config.plugins.image_generation.base_url =
                fields[2].value.trim().trim_end_matches('/').to_string();
            config.plugins.image_generation.api_keys = parse_key_list(&fields[3].value);
            config.plugins.image_generation.model = fields[4].value.trim().to_string();
            config.plugins.image_generation.default_aspect_ratio =
                fields[5].value.trim().to_string();
            config.plugins.image_generation.default_resolution = fields[6].value.trim().to_string();
            config.plugins.image_generation.output_dir = fields[7].value.trim().to_string();
            config.plugins.image_generation.auto_print = parse_bool_field(&fields[8].value)?;
            config.plugins.image_generation.timeout_seconds = fields[9].value.trim().parse()?;
        }
        4 => {
            config.plugins.web_images.enabled = parse_bool_field(&fields[0].value)?;
            config.plugins.web_images.source_mode = match fields[1].value.trim() {
                "auto" | "global" | "mainland" => fields[1].value.trim().to_string(),
                other => {
                    if is_zh() {
                        anyhow::bail!("未知搜图来源模式: {other}")
                    } else {
                        anyhow::bail!("Unknown image search source mode: {other}")
                    }
                }
            };
            config.plugins.web_images.vision_screening_enabled =
                parse_bool_field(&fields[2].value)?;
            config.plugins.web_images.max_results =
                fields[3].value.trim().parse::<usize>()?.clamp(1, 10);
            config.plugins.web_images.safe_search = parse_bool_field(&fields[4].value)?;
            config.plugins.web_images.auto_preview = parse_bool_field(&fields[5].value)?;
            config.plugins.web_images.preview_count =
                fields[6].value.trim().parse::<usize>()?.min(5);
            config.plugins.web_images.max_download_mb =
                fields[7].value.trim().parse::<f64>()?.clamp(0.1, 50.0);
            config.plugins.web_images.timeout_seconds =
                fields[8].value.trim().parse::<u64>()?.clamp(5, 120);
        }
        5 => {
            config.plugins.print_image.enabled = parse_bool_field(&fields[0].value)?;
            config.plugins.print_image.width_percent = fields[1].value.trim().parse::<u8>()?;
            config.plugins.print_image.height_percent = fields[2].value.trim().parse::<u8>()?;
        }
        6 => {
            config.plugins.memes.enabled = parse_bool_field(&fields[0].value)?;
            config.plugins.memes.width_percent =
                fields[1].value.trim().parse::<u8>()?.clamp(1, 100);
            config.plugins.memes.height_percent =
                fields[2].value.trim().parse::<u8>()?.clamp(1, 100);
            config.plugins.memes.max_image_mb =
                fields[3].value.trim().parse::<u64>()?.clamp(1, 100);
            config.plugins.memes.search_max_results =
                fields[4].value.trim().parse::<usize>()?.clamp(1, 3);
            config.plugins.memes.allow_gif_animation = parse_bool_field(&fields[5].value)?;
            config.plugins.memes.auto_send_enabled = parse_bool_field(&fields[6].value)?;
            config.plugins.memes.auto_send_platform_enabled = parse_bool_field(&fields[7].value)?;
            config.plugins.memes.auto_send_probability =
                fields[8].value.trim().parse::<f32>()?.clamp(0.0, 1.0);
        }
        7 => {
            config.plugins.knowledge_base.enabled = parse_bool_field(&fields[0].value)?;
            config.plugins.knowledge_base.data_dir = fields[1].value.trim().to_string();
            config.plugins.knowledge_base.max_search_results = fields[2].value.trim().parse()?;
            config.plugins.knowledge_base.snippet_context_chars = fields[3].value.trim().parse()?;
            config.plugins.knowledge_base.proximity_window_chars =
                fields[4].value.trim().parse()?;
            config.plugins.knowledge_base.max_read_lines = fields[5].value.trim().parse()?;
            config.plugins.knowledge_base.max_file_size_kb = fields[6].value.trim().parse()?;
            config.plugins.knowledge_base.upload_tool_enabled = parse_bool_field(&fields[7].value)?;
            config.plugins.knowledge_base.embedding_enabled = parse_bool_field(&fields[8].value)?;
            let (provider_id, model) = parse_provider_model_choice(&fields[9].value);
            config.plugins.knowledge_base.embedding_provider_id = provider_id;
            config.plugins.knowledge_base.embedding_model = model;
            config.plugins.knowledge_base.semantic_chunk_chars = fields[10].value.trim().parse()?;
            config.plugins.knowledge_base.semantic_chunk_overlap =
                fields[11].value.trim().parse()?;
            config.plugins.knowledge_base.semantic_top_k = fields[12].value.trim().parse()?;
            config.plugins.knowledge_base.semantic_min_score = fields[13].value.trim().parse()?;
            config.plugins.knowledge_base.keyword_strong_score_threshold =
                fields[14].value.trim().parse()?;
            config.plugins.knowledge_base.embedding_timeout_seconds =
                fields[15].value.trim().parse()?;
        }
        8 => {
            config.plugins.brew.enabled = parse_bool_field(&fields[0].value)?;
        }
        9 => {
            config.plugins.man.enabled = parse_bool_field(&fields[0].value)?;
        }
        10 => {
            config.memory = crate::config::MemoryConfig::default();
            config.plugins.memory.enabled = parse_bool_field(&fields[0].value)?;
            config.plugins.memory.evicted_context_enabled = parse_bool_field(&fields[1].value)?;
            config.plugins.memory.association_enabled = parse_bool_field(&fields[2].value)?;
            config.plugins.memory.auto_diary_enabled = parse_bool_field(&fields[3].value)?;
            config.plugins.memory.auto_fact_enabled = parse_bool_field(&fields[4].value)?;
            config.plugins.memory.auto_skill_enabled = false;
            config.plugins.memory.diary_batch_size =
                fields[5].value.trim().parse::<usize>()?.clamp(2, 100);
            config.plugins.memory.short_diary_retention_days =
                fields[6].value.trim().parse::<u64>()?.clamp(1, 3650);
            config.plugins.memory.diary_promotion_recalls =
                fields[7].value.trim().parse::<u64>()?.clamp(1, 100);
            config.plugins.memory.organizer_timeout_seconds =
                fields[8].value.trim().parse::<u64>()?.clamp(5, 600);
            config.plugins.memory.association_facts = fields[9].value.trim().parse::<usize>()?;
            config.plugins.memory.association_episodes =
                fields[10].value.trim().parse::<usize>()?;
            config.plugins.memory.association_max_chars =
                fields[11].value.trim().parse::<usize>()?;
            config.plugins.memory.forgetting_enabled = parse_bool_field(&fields[12].value)?;
            config.plugins.memory.forgetting_half_life_days =
                fields[13].value.trim().parse::<f64>()?;
            config.plugins.memory.forgetting_min_strength =
                fields[14].value.trim().parse::<f64>()?;
            config.plugins.memory.forgetting_review_boost =
                fields[15].value.trim().parse::<f64>()?;
            config.plugins.memory.association_dedup = parse_bool_field(&fields[16].value)?;
        }
        11 => {
            config.plugins.package_advisor.enabled = parse_bool_field(&fields[0].value)?;
        }
        12 => {
            config.plugins.api_quota.enabled = parse_bool_field(&fields[0].value)?;
        }
        _ => {
            let value = parse_bool_field(&fields[0].value)?;
            if plugin_enabled(config, index) != value {
                toggle_plugin(config, index);
            }
        }
    }
    Ok(())
}

pub(crate) fn edit_custom_prompts(
    stdout: &mut io::Stdout,
    paths: &GQYPaths,
    config: &mut AppConfig,
) -> Result<()> {
    let mut selected = 0usize;
    loop {
        let options = [
            t("Normal mode", "普通模式").to_string(),
            t("Dev mode", "开发模式").to_string(),
            // 08-15 A/B 二轮:干净体制下预设对话单独已满分,提醒降为可关
            // 开关;重噪声 QQ 长群聊体制未复测,默认保持启用。
            format!(
                "{}: {}",
                t("Anti-amnesia reminder", "防失忆提醒"),
                if config.prompt.persona_reminder {
                    t("Enabled", "启用")
                } else {
                    t("Disabled", "禁用")
                }
            ),
            format!(
                "{}: {}",
                t("Reminder interval (turns)", "防失忆间隔轮数"),
                config.prompt.persona_reminder_interval.max(1)
            ),
        ];
        draw_menu(
            stdout,
            t(" CUSTOM PROMPTS ", " 自定义提示词 "),
            &options,
            selected,
            t("[Enter]select/toggle [q]back", "[Enter]选择/切换 [q]返回"),
        )?;
        match read_key()? {
            KeyCode::Esc | KeyCode::Char('q') => return Ok(()),
            KeyCode::Up | KeyCode::Char('k') => selected = selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => selected = (selected + 1).min(options.len() - 1),
            KeyCode::Enter if selected == 0 => edit_normal_mode_prompts(stdout, paths, config)?,
            KeyCode::Enter if selected == 1 => edit_dev_prompt(stdout, paths)?,
            KeyCode::Enter if selected == 2 => {
                config.prompt.persona_reminder = !config.prompt.persona_reminder;
            }
            KeyCode::Enter if selected == 3 => {
                if let Some(value) = edit_inline_value(
                    stdout,
                    t("Reminder interval (turns)", "防失忆间隔轮数"),
                    &config.prompt.persona_reminder_interval.to_string(),
                    false,
                )? {
                    if let Ok(interval) = value.trim().parse::<u32>() {
                        config.prompt.persona_reminder_interval = interval.max(1);
                    }
                }
            }
            _ => {}
        }
    }
}

/// 普通模式的提示词面:AI 人格与用户身份(原顶层两项下沉至此)。
pub(crate) fn edit_normal_mode_prompts(
    stdout: &mut io::Stdout,
    paths: &GQYPaths,
    config: &mut AppConfig,
) -> Result<()> {
    let mut selected = 0usize;
    loop {
        let persona = if config.prompt.active_persona.trim().is_empty() {
            "GQY".to_string()
        } else {
            persona_display_name(&config.prompt.active_persona).to_string()
        };
        let options = [
            format!(
                "{} ({}: {persona})",
                t("AI persona", "AI 人格"),
                t("Current", "当前")
            ),
            t("User identity", "用户身份").to_string(),
        ];
        draw_menu(
            stdout,
            t(" NORMAL MODE ", " 普通模式 "),
            &options,
            selected,
            t("[Enter]select [q]back", "[Enter]选择 [q]返回"),
        )?;
        match read_key()? {
            KeyCode::Esc | KeyCode::Char('q') => return Ok(()),
            KeyCode::Up | KeyCode::Char('k') => selected = selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => selected = (selected + 1).min(options.len() - 1),
            KeyCode::Enter if selected == 0 => edit_personas(stdout, paths, config)?,
            KeyCode::Enter if selected == 1 => edit_identities(stdout, paths, config)?,
            _ => {}
        }
    }
}

/// 开发模式的「AI 提示词」:编辑 config/dev-prompt.md 一个文件。清空
/// 保存=删文件,运行时回退内置默认一行;记忆按保留人格 "dev" 落库,
/// 与这份提示词的内容完全解耦——怎么改都不会切库。
pub(crate) fn edit_dev_prompt(stdout: &mut io::Stdout, paths: &GQYPaths) -> Result<()> {
    let path = paths.config_dir.join(crate::config::DEV_PROMPT_FILE);
    let current = std::fs::read_to_string(&path).unwrap_or_default();
    let prefill = if current.trim().is_empty() {
        crate::config::DEFAULT_DEV_SYSTEM_PROMPT.to_string()
    } else {
        current.trim_end().to_string()
    };
    let mut fields = vec![Field::textarea(
        t(
            "AI prompt (empty = built-in default)",
            "AI 提示词(清空=恢复内置默认)",
        ),
        prefill,
    )];
    if !run_form(stdout, t(" DEV MODE ", " 开发模式 "), &mut fields)? {
        return Ok(());
    }
    let value = fields[0].value.trim();
    if value.is_empty() {
        if path.exists() {
            std::fs::remove_file(&path)?;
        }
    } else {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, format!("{value}\n"))?;
    }
    Ok(())
}

pub(crate) fn edit_personas(
    stdout: &mut io::Stdout,
    paths: &GQYPaths,
    config: &mut AppConfig,
) -> Result<()> {
    manage_personas(stdout, paths, config, PersonaMenuTarget::Global)?;
    Ok(())
}

pub(crate) enum PersonaMenuTarget {
    Global,
    Platform(PlatformPersonaOverride),
}
