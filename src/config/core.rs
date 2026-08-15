//! core — 自 src/config.rs 拆分。

use super::*;

use crate::default_models::{
    OPENCODE_DEFAULT_CHAT_MODEL, OPENCODE_DEFAULT_VISION_MODEL, OPENCODE_PROVIDER_ID,
    OPENCODE_ZEN_BASE_URL,
};
use crate::paths::GQYPaths;
use crate::prompts::default_system_prompt;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

pub const MAX_COMMAND_OUTPUT_LINES: usize = 1_000;

/// Dev 模式提示词文件名(config 目录下,可编辑;清空=回退内置默认)。
pub const DEV_PROMPT_FILE: &str = "dev-prompt.md";
/// Dev 模式内置默认提示词。dsh 极简变体同款措辞——贴近编码 RL 训练分布
/// 是它强的主因(08-15 与用户讨论定稿,修正了社区传言的拼写错误)。
pub const DEFAULT_DEV_SYSTEM_PROMPT: &str = "You are a helpful software engineer assistant.";
/// Replay redraws whole turns, so a large value floods the screen on startup.
pub const MAX_REPL_REPLAY_TURNS: usize = 20;
pub const CURRENT_CONFIG_VERSION: u32 = 2;
pub(crate) const LEGACY_DEFAULT_TEMPERATURE: f32 = 0.7;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default)]
    pub config_version: u32,
    pub active_provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_provider_models: Option<Vec<ActiveProviderModelConfig>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_multimodal_provider_models: Option<Vec<ActiveProviderModelConfig>>,
    pub providers: Vec<ProviderConfig>,
    #[serde(default, skip_serializing_if = "EmbeddingConfig::is_default")]
    pub embedding: EmbeddingConfig,
    #[serde(default)]
    pub context: ContextConfig,
    #[serde(default)]
    pub tools: ToolsConfig,
    #[serde(default, skip_serializing_if = "CacheConfig::is_default")]
    pub cache: CacheConfig,
    #[serde(default)]
    pub mcp: McpConfig,
    #[serde(default)]
    pub skills: SkillsConfig,
    #[serde(default)]
    pub display: DisplayConfig,
    #[serde(default)]
    pub notifications: NotificationsConfig,
    #[serde(default)]
    pub prompt: PromptConfig,
    #[serde(default)]
    pub plugins: PluginsConfig,
    #[serde(default, skip_serializing)]
    pub memory: MemoryConfig,
    #[serde(default)]
    pub system_prompt_file: Option<String>,
    /// 裸 `gqy` 的默认模式:"normal" | "dev";空(默认)=打印带模式说明的
    /// 帮助,逼一次显式选择。`gqy normal` / `gqy dev` 子命令始终可用。
    #[serde(default)]
    pub default_mode: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    #[serde(default, skip_serializing_if = "SubagentTiersConfig::is_empty")]
    pub subagent_tiers: SubagentTiersConfig,
    #[serde(default, skip_serializing_if = "PlatformsConfig::is_empty")]
    pub platforms: PlatformsConfig,
}

/// Provider prompt-cache tuning (v7, DeepSeek 高命中策略实测产物). The
/// tuning knobs default to off — they trade a little latency or a few cheap
/// requests for prefix-cache hits on best-effort provider caches. The
/// accounting log defaults to on (numbers only, ~0.2 KB per request).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CacheConfig {
    /// Idle keepalive: while the agent waits for the next user turn, re-send
    /// the exact prompt prefix of the last request every N seconds as a
    /// non-streaming max_tokens=1 completion so hot-tier prefix caches
    /// (DeepSeek-style) keep the deep prefix alive across turn gaps. The ping
    /// is billed at the provider's cache-hit input price. 0 disables (the
    /// default — enable only after measuring your provider: on per-REQUEST
    /// billed endpoints every ping burns quota for nothing).
    /// Only effective in long-lived processes (daemon/REPL); one-shot `ask`
    /// exits before any ping fires.
    pub keepalive_seconds: u64,
    /// Stop pinging after this many keepalives per turn (bounds idle cost).
    pub keepalive_max_pings: u32,
    /// Provider cache writes are asynchronous (measured: a follow-up within
    /// ~2s can miss the prefix the previous request just computed). When >0,
    /// consecutive tool-loop requests wait until at least this many
    /// milliseconds have passed since the previous round completed.
    pub write_grace_ms: u64,
    /// Per-request cache accounting log: one JSONL line of absolute token
    /// numbers (prompt/cache_read/completion/…) per LLM request under
    /// cache/logs/cache-usage.<date>.jsonl. Numbers only — never prompt text.
    /// Roughly 0.2 KB per request; daily files, pruned by retention below.
    pub request_log: bool,
    /// Days of cache-usage JSONL files to keep (older files are deleted when
    /// a new line is written).
    pub request_log_retention_days: u64,
}

impl Default for CacheConfig {
    pub(crate) fn default() -> Self {
        Self {
            keepalive_seconds: 0,
            keepalive_max_pings: 20,
            write_grace_ms: 0,
            request_log: true,
            request_log_retention_days: 14,
        }
    }
}

impl CacheConfig {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// Messaging-platform settings. Public configuration is named after the
/// product users connect to; transport protocols remain implementation
/// details of each platform adapter.
pub const DEFAULT_PLATFORM_COMMAND_PREFIX: &str = "/";
pub const MAX_PLATFORM_COMMAND_PREFIX_CHARS: usize = 32;
pub const MAX_PLATFORM_SESSION_RUNNING: usize = 16;
pub const MAX_PLATFORM_SESSION_QUEUED: usize = 64;

/// Group overflow handling. Groups and terminal sessions want opposite things
/// here: a coding session benefits from `compact` folding old turns into a
/// summary it can keep reasoning from, while summarising a group log destroys
/// the structured record every `回复引用: msg=…` points at. Groups drop whole
/// turns instead, and drop a lot at once so the surviving prefix stays stable
/// for a long stretch rather than being clipped every few turns.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PlatformGroupContextConfig {
    /// `compact` / `pop`; empty inherits `context.on_overflow`.
    pub on_overflow: String,
    /// Fraction of the window released in one trim; 0 inherits
    /// `context.trim_batch_ratio`.
    pub trim_batch_ratio: f32,
}

impl Default for PlatformGroupContextConfig {
    pub(crate) fn default() -> Self {
        Self {
            on_overflow: "pop".to_string(),
            trim_batch_ratio: 0.5,
        }
    }
}

impl PlatformGroupContextConfig {
    pub(crate) fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PlatformSessionLimits {
    pub running: usize,
    pub queued: usize,
}

impl Default for PlatformSessionLimits {
    pub(crate) fn default() -> Self {
        Self {
            running: 8,
            queued: 16,
        }
    }
}

impl PlatformSessionLimits {
    pub(crate) fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlatformsConfig {
    #[serde(
        default = "default_platform_command_prefix",
        skip_serializing_if = "is_default_platform_command_prefix"
    )]
    pub command_prefix: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub commands: BTreeMap<String, PlatformCommandConfig>,
    #[serde(default, skip_serializing_if = "OneBotConfig::is_default")]
    pub qq: OneBotConfig,
}

impl Default for PlatformsConfig {
    pub(crate) fn default() -> Self {
        Self {
            command_prefix: default_platform_command_prefix(),
            commands: BTreeMap::new(),
            qq: OneBotConfig::default(),
        }
    }
}

impl PlatformsConfig {
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }

    pub fn command_permission(
        &self,
        command: &str,
        default: PlatformCommandPermission,
    ) -> PlatformCommandPermission {
        self.commands
            .get(command)
            .map(|config| config.permission)
            .unwrap_or(default)
    }

    pub fn set_command_permission(
        &mut self,
        command: &str,
        permission: PlatformCommandPermission,
        default: PlatformCommandPermission,
    ) {
        if permission == default {
            self.commands.remove(command);
        } else {
            self.commands
                .insert(command.to_string(), PlatformCommandConfig { permission });
        }
    }

    pub fn model_route(
        &self,
        kind: PlatformConversationKind,
        conversation_id: &str,
    ) -> Option<&PlatformModelRoute> {
        self.qq
            .conversations
            .iter()
            .find(|route| route.matches(kind, conversation_id))
    }

    pub fn model_route_mut(
        &mut self,
        kind: PlatformConversationKind,
        conversation_id: &str,
    ) -> Option<&mut PlatformModelRoute> {
        self.qq
            .conversations
            .iter_mut()
            .find(|route| route.matches(kind, conversation_id))
    }

    /// Inserts a route or replaces the route with the same stable identity.
    /// Inherited pools are meaningful conversation configuration and are kept
    /// until the user explicitly removes the entry.
    pub fn upsert_model_route(&mut self, mut route: PlatformModelRoute) {
        route.normalize();
        if let Some(index) = self
            .qq
            .conversations
            .iter()
            .position(|existing| existing.identity() == route.identity())
        {
            self.qq.conversations[index] = route;
        } else {
            self.qq.conversations.push(route);
        }
    }

    pub fn remove_model_route(
        &mut self,
        kind: PlatformConversationKind,
        conversation_id: &str,
    ) -> bool {
        let old_len = self.qq.conversations.len();
        self.qq
            .conversations
            .retain(|route| !route.matches(kind, conversation_id));
        self.qq.conversations.len() != old_len
    }

    pub fn rename_persona_references(&mut self, old_name: &str, new_name: &str) {
        for route in &mut self.qq.conversations {
            if route.persona.custom_name() == Some(old_name) {
                route.persona = PlatformPersonaOverride::Custom {
                    name: new_name.to_string(),
                };
            }
        }
    }

    pub fn persona_reference_count(&self, name: &str) -> usize {
        self.qq
            .conversations
            .iter()
            .filter(|route| route.persona.custom_name() == Some(name))
            .count()
    }

    pub fn normalize_model_routes(&mut self) {
        self.command_prefix = self.command_prefix.trim().to_string();
        self.qq.private_chats.migrate_legacy_rate_limit();
        self.qq.group_chats.migrate_legacy_rate_limits();
        self.qq.admin_users.sort_unstable();
        self.qq.admin_users.dedup();
        self.qq.private_chats.whitelist.sort_unstable();
        self.qq.private_chats.whitelist.dedup();
        self.qq.group_chats.whitelist.sort_unstable();
        self.qq.group_chats.whitelist.dedup();
        let mut keywords = HashSet::with_capacity(self.qq.group_chats.trigger_keywords.len());
        self.qq.group_chats.trigger_keywords = self
            .qq
            .group_chats
            .trigger_keywords
            .drain(..)
            .map(|keyword| keyword.trim().to_string())
            .filter(|keyword| !keyword.is_empty() && keywords.insert(keyword.clone()))
            .collect();
        self.qq.asset_base_url = self
            .qq
            .asset_base_url
            .trim()
            .trim_end_matches('/')
            .to_string();
        normalize_route_pool(&mut self.qq.text_models);
        normalize_route_pool(&mut self.qq.multimodal_models);
        normalize_route_pool(&mut self.qq.non_whitelist_text_models);
        for route in &mut self.qq.conversations {
            route.normalize();
        }
        migrate_message_history_instance(&mut self.qq.plugins);
        if let Some(instance) = self.qq.plugins.get_mut(REAL_CONTEXT_PLUGIN_ID) {
            normalize_real_context_instance(instance);
        }
        self.qq
            .plugins
            .retain(|name, instance| !name.trim().is_empty() && !instance.is_empty());
    }

    pub fn prune_model_references(&mut self, providers: &[ProviderConfig]) {
        prune_pool(&mut self.qq.text_models, providers, false);
        prune_pool(&mut self.qq.multimodal_models, providers, true);
        prune_pool(&mut self.qq.non_whitelist_text_models, providers, false);
        for route in &mut self.qq.conversations {
            route.prune_model_references(providers);
        }
        mutate_real_context_settings(&mut self.qq.plugins, |settings| {
            for pool in [&mut settings.text_models] {
                if let Some(models) = pool {
                    models.retain(|model| active_model_exists(providers, model));
                }
                normalize_route_pool(pool);
            }
        });
        self.normalize_model_routes();
    }

    pub fn remove_model_references(&mut self, provider_id: &str, model: &str) {
        for pool in [
            &mut self.qq.text_models,
            &mut self.qq.multimodal_models,
            &mut self.qq.non_whitelist_text_models,
        ] {
            if let Some(entries) = pool {
                entries.retain(|entry| !(entry.provider_id == provider_id && entry.model == model));
            }
            normalize_route_pool(pool);
        }
        for route in &mut self.qq.conversations {
            route.remove_model_references(provider_id, model);
        }
        mutate_real_context_settings(&mut self.qq.plugins, |settings| {
            for pool in [&mut settings.text_models] {
                if let Some(models) = pool {
                    models.retain(|entry| {
                        !(entry.provider_id == provider_id && entry.model == model)
                    });
                }
                normalize_route_pool(pool);
            }
        });
        self.normalize_model_routes();
    }

    pub fn remove_provider_references(&mut self, provider_id: &str) {
        for pool in [
            &mut self.qq.text_models,
            &mut self.qq.multimodal_models,
            &mut self.qq.non_whitelist_text_models,
        ] {
            if let Some(entries) = pool {
                entries.retain(|entry| entry.provider_id != provider_id);
            }
            normalize_route_pool(pool);
        }
        for route in &mut self.qq.conversations {
            for pool in [&mut route.text_models, &mut route.multimodal_models] {
                if let Some(entries) = pool {
                    entries.retain(|entry| entry.provider_id != provider_id);
                }
                normalize_route_pool(pool);
            }
        }
        mutate_real_context_settings(&mut self.qq.plugins, |settings| {
            for pool in [&mut settings.text_models] {
                if let Some(models) = pool {
                    models.retain(|entry| entry.provider_id != provider_id);
                }
                normalize_route_pool(pool);
            }
        });
        self.normalize_model_routes();
    }

    pub fn rename_provider_references(&mut self, old_id: &str, new_id: &str) {
        for pool in [
            &mut self.qq.text_models,
            &mut self.qq.multimodal_models,
            &mut self.qq.non_whitelist_text_models,
        ] {
            if let Some(entries) = pool {
                rename_provider_in_pool(entries, old_id, new_id);
            }
            normalize_route_pool(pool);
        }
        for route in &mut self.qq.conversations {
            route.rename_provider_references(old_id, new_id);
        }
        mutate_real_context_settings(&mut self.qq.plugins, |settings| {
            for pool in [&mut settings.text_models] {
                if let Some(models) = pool {
                    rename_provider_in_pool(models, old_id, new_id);
                }
                normalize_route_pool(pool);
            }
        });
    }

    pub fn rename_model_references(&mut self, provider_id: &str, old: &str, new: &str) {
        for pool in [
            &mut self.qq.text_models,
            &mut self.qq.multimodal_models,
            &mut self.qq.non_whitelist_text_models,
        ] {
            if let Some(entries) = pool {
                for entry in entries {
                    if entry.provider_id == provider_id && entry.model == old {
                        entry.model = new.to_string();
                    }
                }
            }
            normalize_route_pool(pool);
        }
        for route in &mut self.qq.conversations {
            route.rename_model_references(provider_id, old, new);
        }
        mutate_real_context_settings(&mut self.qq.plugins, |settings| {
            for pool in [&mut settings.text_models] {
                if let Some(models) = pool {
                    for entry in models {
                        if entry.provider_id == provider_id && entry.model == old {
                            entry.model = new.to_string();
                        }
                    }
                }
                normalize_route_pool(pool);
            }
        });
    }
}

pub(crate) fn prune_pool(
    pool: &mut Option<Vec<ActiveProviderModelConfig>>,
    providers: &[ProviderConfig],
    require_multimodal: bool,
) {
    if let Some(models) = pool {
        models.retain(|model| {
            active_model_exists(providers, model)
                && (!require_multimodal || active_model_supports_image(providers, model))
        });
    }
    normalize_route_pool(pool);
}

pub(crate) fn default_platform_command_prefix() -> String {
    DEFAULT_PLATFORM_COMMAND_PREFIX.to_string()
}

pub(crate) fn is_default_platform_command_prefix(value: &String) -> bool {
    value == DEFAULT_PLATFORM_COMMAND_PREFIX
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlatformCommandPermission {
    Everyone,
    #[default]
    AdminOnly,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlatformCommandConfig {
    #[serde(default)]
    pub permission: PlatformCommandPermission,
}

pub type PlatformPluginsConfig = BTreeMap<String, PlatformPluginInstanceConfig>;

type PlatformPluginConfigValidator = fn(&PlatformPluginInstanceConfig) -> Result<()>;

pub const REAL_CONTEXT_PLUGIN_ID: &str = "real_context";
pub const QQ_MESSAGE_HISTORY_PLUGIN_ID: &str = "qq_message_history";
pub const QQ_GROUP_MANAGEMENT_PLUGIN_ID: &str = "qq_group_management";
pub const QQ_MESSAGE_RECALL_PLUGIN_ID: &str = "qq_message_recall";
pub const QQ_MEME_COLLECTOR_PLUGIN_ID: &str = "qq_meme_collector";

pub(crate) const PLATFORM_PLUGIN_VALIDATORS: &[(&str, PlatformPluginConfigValidator)] = &[
    ("reply_processor", validate_reply_processor_plugin_config),
    (REAL_CONTEXT_PLUGIN_ID, validate_real_context_plugin_config),
    (
        QQ_MESSAGE_HISTORY_PLUGIN_ID,
        validate_qq_message_history_plugin_config,
    ),
    (
        QQ_GROUP_MANAGEMENT_PLUGIN_ID,
        validate_qq_group_management_plugin_config,
    ),
    (
        QQ_MESSAGE_RECALL_PLUGIN_ID,
        validate_qq_message_recall_plugin_config,
    ),
    (
        QQ_MEME_COLLECTOR_PLUGIN_ID,
        validate_qq_meme_collector_plugin_config,
    ),
];

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PlatformPluginInstanceConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub settings: serde_json::Map<String, serde_json::Value>,
}

impl PlatformPluginInstanceConfig {
    pub fn is_empty(&self) -> bool {
        self.enabled.is_none() && self.settings.is_empty()
    }

    pub fn enabled_or(&self, default: bool) -> bool {
        self.enabled.unwrap_or(default)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct QqGroupManagementPluginSettings {
    pub enable_tool: bool,
    pub enable_kick_tool: bool,
    pub enable_special_title_tool: bool,
    pub enable_record: bool,
    pub enable_offender_history: bool,
    pub sync_external_unmute_notice: bool,
    pub default_duration_seconds: u64,
    pub max_reason_length: usize,
    pub max_special_title_length: usize,
    pub max_special_title_duration_seconds: i64,
    pub max_groups: usize,
    pub max_records_per_group: usize,
    pub expired_record_retention_seconds: u64,
    pub cleanup_interval_seconds: u64,
    pub max_offender_history_per_group: usize,
    pub max_kick_history_per_group: usize,
}

impl Default for QqGroupManagementPluginSettings {
    pub(crate) fn default() -> Self {
        Self {
            enable_tool: true,
            enable_kick_tool: true,
            enable_special_title_tool: true,
            enable_record: true,
            enable_offender_history: true,
            sync_external_unmute_notice: true,
            default_duration_seconds: 600,
            max_reason_length: 500,
            max_special_title_length: 18,
            max_special_title_duration_seconds: -1,
            max_groups: 200,
            max_records_per_group: 500,
            expired_record_retention_seconds: 604_800,
            cleanup_interval_seconds: 300,
            max_offender_history_per_group: 500,
            max_kick_history_per_group: 500,
        }
    }
}

impl QqGroupManagementPluginSettings {
    pub fn from_instance(instance: &PlatformPluginInstanceConfig) -> Result<Self> {
        serde_json::from_value(serde_json::Value::Object(instance.settings.clone()))
            .context("invalid qq_group_management plugin settings")
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct QqMessageRecallPluginSettings {
    pub enable_tool: bool,
    pub capture_outgoing_messages: bool,
    pub max_reason_length: usize,
    pub max_messages_per_conversation: usize,
    pub cancel_record_ttl_seconds: u64,
    pub cancel_cleanup_interval_seconds: u64,
}

impl Default for QqMessageRecallPluginSettings {
    pub(crate) fn default() -> Self {
        Self {
            enable_tool: true,
            capture_outgoing_messages: true,
            max_reason_length: 500,
            max_messages_per_conversation: 20,
            cancel_record_ttl_seconds: 300,
            cancel_cleanup_interval_seconds: 60,
        }
    }
}

impl QqMessageRecallPluginSettings {
    pub fn from_instance(instance: &PlatformPluginInstanceConfig) -> Result<Self> {
        serde_json::from_value(serde_json::Value::Object(instance.settings.clone()))
            .context("invalid qq_message_recall plugin settings")
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct QqMemeCollectorPluginSettings {
    pub collect_probability: f64,
    pub max_images_per_message: usize,
    pub allow_non_admin_save_tool: bool,
}

impl Default for QqMemeCollectorPluginSettings {
    pub(crate) fn default() -> Self {
        Self {
            collect_probability: 0.02,
            max_images_per_message: 2,
            allow_non_admin_save_tool: false,
        }
    }
}

impl QqMemeCollectorPluginSettings {
    pub fn from_instance(instance: &PlatformPluginInstanceConfig) -> Result<Self> {
        serde_json::from_value(serde_json::Value::Object(instance.settings.clone()))
            .context("invalid qq_meme_collector plugin settings")
    }
}

pub(crate) fn validate_qq_group_management_plugin_config(
    instance: &PlatformPluginInstanceConfig,
) -> Result<()> {
    let settings = QqGroupManagementPluginSettings::from_instance(instance)?;
    if settings.max_reason_length > 10_000
        || settings.max_special_title_length > 100
        || settings.max_groups == 0
        || settings.max_records_per_group == 0
        || settings.max_offender_history_per_group == 0
        || settings.max_kick_history_per_group == 0
        || settings.cleanup_interval_seconds == 0
    {
        bail!("invalid qq_group_management plugin limits");
    }
    Ok(())
}

pub(crate) fn validate_qq_message_recall_plugin_config(instance: &PlatformPluginInstanceConfig) -> Result<()> {
    let settings = QqMessageRecallPluginSettings::from_instance(instance)?;
    if settings.max_reason_length > 10_000
        || settings.max_messages_per_conversation == 0
        || settings.max_messages_per_conversation > 1_000
        || settings.cancel_record_ttl_seconds < 10
        || settings.cancel_cleanup_interval_seconds < 5
    {
        bail!("invalid qq_message_recall plugin limits");
    }
    Ok(())
}

pub(crate) fn validate_qq_meme_collector_plugin_config(instance: &PlatformPluginInstanceConfig) -> Result<()> {
    let settings = QqMemeCollectorPluginSettings::from_instance(instance)?;
    if !settings.collect_probability.is_finite()
        || !(0.0..=1.0).contains(&settings.collect_probability)
        || !(1..=4).contains(&settings.max_images_per_message)
    {
        bail!("invalid qq_meme_collector plugin limits");
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct QqMessageHistoryPluginSettings {
    pub history_search_max_results: usize,
    pub history_safe_page_limit: usize,
    pub allow_cross_conversation_search: bool,
}

impl Default for QqMessageHistoryPluginSettings {
    pub(crate) fn default() -> Self {
        Self {
            history_search_max_results: 0,
            history_safe_page_limit: 500,
            allow_cross_conversation_search: true,
        }
    }
}

impl QqMessageHistoryPluginSettings {
    pub fn from_instance(instance: &PlatformPluginInstanceConfig) -> Result<Self> {
        serde_json::from_value(serde_json::Value::Object(instance.settings.clone()))
            .context("invalid qq_message_history plugin settings")
    }

    pub fn validate(&self) -> Result<()> {
        if self.history_safe_page_limit == 0 || self.history_safe_page_limit > 1_000 {
            bail!("platform plugin qq_message_history.history_safe_page_limit must be between 1 and 1000");
        }
        if self.history_search_max_results > self.history_safe_page_limit {
            bail!("platform plugin qq_message_history.history_search_max_results must be 0 or no greater than history_safe_page_limit");
        }
        Ok(())
    }
}

pub(crate) fn validate_qq_message_history_plugin_config(
    instance: &PlatformPluginInstanceConfig,
) -> Result<()> {
    QqMessageHistoryPluginSettings::from_instance(instance)?.validate()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RealContextIdentityMapping {
    pub nickname: String,
    pub user_id: i64,
}

/// Configuration contract for the built-in QQ group real-context plugin.
///
/// The values intentionally stay flat in the generic platform-plugin map. This
/// keeps the persisted format forward compatible while giving the runtime and
/// TUI one strongly typed source of defaults and validation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RealContextPluginSettings {
    /// How much group log the reply turn starts from. Once the history is
    /// append-only this is a one-off opening snapshot rather than a per-turn
    /// window, so it can afford to be generous.
    pub reply_context_window: usize,
    /// How much group log the active-reply judge sees. It rates the mood of the
    /// moment, so a longer window dilutes the recent signal and stretches the
    /// timeframe — and the judge runs on every message, not once per turn.
    pub judge_context_window: usize,
    #[serde(alias = "group_member_page_size")]
    pub group_member_search_max_results: usize,

    pub active_reply_enable: bool,
    pub judge_include_persona: bool,
    pub judge_persona_prompt: String,
    pub text_models: Option<Vec<ActiveProviderModelConfig>>,
    pub active_judge_probability: f64,
    pub reply_threshold: f64,
    pub judge_timeout_seconds: u64,
    pub judge_endpoint_timeout_seconds: u64,
    pub judge_queue_wait_timeout_seconds: u64,
    pub judge_max_concurrency: usize,
    pub judge_max_retries: usize,
    pub skip_pure_image_active_judge: bool,
    pub active_reply_supersede_enable: bool,
    pub active_reply_supersede_window_seconds: u64,
    pub reply_restraint_enable: bool,
    pub reply_restraint_recover_minutes: u64,
    pub reply_restraint_strength: String,
    pub reply_restraint_multiplier: f64,
    pub judge_relevance_weight: f64,
    pub judge_willingness_weight: f64,
    pub judge_social_weight: f64,
    pub judge_timing_weight: f64,
    pub judge_continuity_weight: f64,
    pub judge_should_reply_adjust_enable: bool,
    pub judge_should_reply_boost_score: f64,
    pub judge_should_reply_penalty_score: f64,

    pub continuation_enable: bool,
    pub continuation_window_seconds: u64,
    pub continuation_boost_score: f64,
    pub takeover_direct_trigger_enable: bool,
    pub takeover_direct_trigger_boost_score: f64,
    pub privileged_direct_trigger_skip_active_judgement: bool,

    pub active_reply_reaction_enable: bool,
    pub active_reply_reaction_emoji_ids: Vec<u32>,
    pub active_reply_reaction_timeout_seconds: u64,
    pub reply_target_enable: bool,
    pub reply_target_quote_enable: bool,
    pub reply_target_quote_after_other_messages: u64,
    pub reply_target_mention_enable: bool,
    pub reply_target_mention_after_seconds: u64,

    pub moderation_enable: bool,
    pub moderation_keyword_trigger_enable: bool,
    pub moderation_keywords: Vec<String>,
    pub moderation_min_severity: f64,
    pub moderation_timeout_seconds: u64,
    pub moderation_custom_rules: String,
    pub base64_moderation_enable: bool,
    pub base64_moderation_min_chars: usize,
    pub base64_moderation_max_decoded_chars: usize,
    pub base64_moderation_min_printable_ratio: f64,

    pub affection_enable: bool,
    pub affection_update_enable: bool,
    pub affection_update_timeout_seconds: u64,
    pub affection_initial_score: f64,
    pub affection_min_score: f64,
    pub affection_max_score: f64,
    pub affection_regular_max_score: f64,
    pub affection_unlimited_user_ids: Vec<i64>,
    pub affection_bias_min: f64,
    pub affection_bias_max: f64,
    pub affection_gain_pivot: f64,
    pub affection_delta_scale: f64,
    pub affection_delta_min: f64,
    pub affection_delta_max: f64,
    pub affection_update_confidence_threshold: f64,
    pub affection_daily_gain_limit: f64,
    pub affection_daily_loss_limit: f64,
    pub affection_auto_tag_enable: bool,
    pub affection_max_tags: usize,
    pub affection_recent_events_for_prompt: usize,
    pub affection_prompt_estranged: String,
    pub affection_prompt_cold: String,
    pub affection_prompt_neutral: String,
    pub affection_prompt_known: String,
    pub affection_prompt_friend: String,
    pub affection_prompt_trusted: String,
    pub affection_prompt_close: String,

    pub identity_mappings: Vec<RealContextIdentityMapping>,
}

impl Default for RealContextPluginSettings {
    pub(crate) fn default() -> Self {
        Self {
            reply_context_window: 25,
            judge_context_window: 20,
            group_member_search_max_results: 200,
            active_reply_enable: true,
            judge_include_persona: true,
            judge_persona_prompt: String::new(),
            text_models: None,
            active_judge_probability: 0.05,
            reply_threshold: 0.8,
            judge_timeout_seconds: 60,
            judge_endpoint_timeout_seconds: 15,
            judge_queue_wait_timeout_seconds: 15,
            judge_max_concurrency: 4,
            judge_max_retries: 1,
            skip_pure_image_active_judge: true,
            active_reply_supersede_enable: true,
            active_reply_supersede_window_seconds: 5,
            reply_restraint_enable: true,
            reply_restraint_recover_minutes: 3,
            reply_restraint_strength: "medium".to_string(),
            reply_restraint_multiplier: 1.0,
            judge_relevance_weight: 0.25,
            judge_willingness_weight: 0.25,
            judge_social_weight: 0.15,
            judge_timing_weight: 0.15,
            judge_continuity_weight: 0.20,
            judge_should_reply_adjust_enable: true,
            judge_should_reply_boost_score: 0.2,
            judge_should_reply_penalty_score: 0.2,
            continuation_enable: true,
            continuation_window_seconds: 15,
            continuation_boost_score: 0.1,
            takeover_direct_trigger_enable: true,
            takeover_direct_trigger_boost_score: 0.3,
            privileged_direct_trigger_skip_active_judgement: true,
            active_reply_reaction_enable: true,
            active_reply_reaction_emoji_ids: vec![289],
            active_reply_reaction_timeout_seconds: 600,
            reply_target_enable: true,
            reply_target_quote_enable: true,
            reply_target_quote_after_other_messages: 4,
            reply_target_mention_enable: true,
            reply_target_mention_after_seconds: 15,
            moderation_enable: true,
            moderation_keyword_trigger_enable: true,
            moderation_keywords: default_real_context_moderation_keywords(),
            moderation_min_severity: 7.0,
            moderation_timeout_seconds: 120,
            moderation_custom_rules: String::new(),
            base64_moderation_enable: true,
            base64_moderation_min_chars: 24,
            base64_moderation_max_decoded_chars: 5_000,
            base64_moderation_min_printable_ratio: 0.85,
            affection_enable: false,
            affection_update_enable: true,
            affection_update_timeout_seconds: 120,
            affection_initial_score: 10.0,
            affection_min_score: -50.0,
            affection_max_score: 100.0,
            affection_regular_max_score: 94.0,
            affection_unlimited_user_ids: Vec::new(),
            affection_bias_min: -0.2,
            affection_bias_max: 0.1,
            affection_gain_pivot: 60.0,
            affection_delta_scale: 1.0,
            affection_delta_min: -10.0,
            affection_delta_max: 2.0,
            affection_update_confidence_threshold: 0.8,
            affection_daily_gain_limit: 6.0,
            affection_daily_loss_limit: 15.0,
            affection_auto_tag_enable: true,
            affection_max_tags: 10,
            affection_recent_events_for_prompt: 3,
            affection_prompt_estranged: "你和该用户关系疏远。回复时保持克制、礼貌和简短，不主动延展话题，不使用熟人玩笑。拒绝为对方进行生图、天气搜索、复杂知识问答、塔罗牌、算卦等高级内容。".to_string(),
            affection_prompt_cold: "你对该用户态度冷淡。回复时以完成必要交流为主，避免热情、调侃和主动关心。拒绝为对方进行生图、复杂知识问答。".to_string(),
            affection_prompt_neutral: "你和该用户关系普通。按正常群聊或助手语气回复，保持自然、简洁和客观。".to_string(),
            affection_prompt_known: "你认识该用户。可以适度承接过往互动，语气比陌生人更自然，但不要表现得过分亲密。".to_string(),
            affection_prompt_friend: "你和该用户关系较熟。可以自然接话，允许轻微吐槽、接梗和熟人语气，但不要过度亲密。".to_string(),
            affection_prompt_trusted: "你信任该用户。回复时可以更主动承接上下文，表达更直接明确的判断，但仍要保持事实准确和边界。".to_string(),
            affection_prompt_close: "你和该用户是挚友。可以使用更熟悉、轻松的语气和轻微玩笑。".to_string(),
            identity_mappings: Vec::new(),
        }
    }
}

impl RealContextPluginSettings {
    pub fn from_instance(instance: &PlatformPluginInstanceConfig) -> Result<Self> {
        let mut settings = instance.settings.clone();
        migrate_real_context_settings_map(&mut settings);
        serde_json::from_value(serde_json::Value::Object(settings))
            .context("invalid real_context plugin settings")
    }

    pub fn normalize(&mut self) {
        self.judge_persona_prompt = self.judge_persona_prompt.trim().to_string();
        normalize_route_pool(&mut self.text_models);
        normalize_unique_strings(&mut self.moderation_keywords);
        self.active_reply_reaction_emoji_ids.retain(|id| *id > 0);
        self.active_reply_reaction_emoji_ids.sort_unstable();
        self.active_reply_reaction_emoji_ids.dedup();
        self.affection_unlimited_user_ids.retain(|id| *id > 0);
        self.affection_unlimited_user_ids.sort_unstable();
        self.affection_unlimited_user_ids.dedup();
        for mapping in &mut self.identity_mappings {
            mapping.nickname = mapping.nickname.trim().to_string();
        }
        let mut nicknames = HashSet::with_capacity(self.identity_mappings.len());
        self.identity_mappings.retain(|mapping| {
            !mapping.nickname.is_empty() && nicknames.insert(mapping.nickname.clone())
        });
    }

    pub fn validate(&self) -> Result<()> {
        validate_real_context_count("reply_context_window", self.reply_context_window, 1, 200)?;
        validate_real_context_count("judge_context_window", self.judge_context_window, 1, 200)?;
        validate_real_context_count(
            "group_member_search_max_results",
            self.group_member_search_max_results,
            1,
            200,
        )?;
        validate_real_context_probability(
            "active_judge_probability",
            self.active_judge_probability,
        )?;
        validate_real_context_probability("reply_threshold", self.reply_threshold)?;
        validate_real_context_count(
            "judge_timeout_seconds",
            self.judge_timeout_seconds as usize,
            0,
            600,
        )?;
        validate_real_context_count(
            "judge_endpoint_timeout_seconds",
            self.judge_endpoint_timeout_seconds as usize,
            1,
            600,
        )?;
        validate_real_context_count(
            "judge_queue_wait_timeout_seconds",
            self.judge_queue_wait_timeout_seconds as usize,
            1,
            600,
        )?;
        validate_real_context_count("judge_max_concurrency", self.judge_max_concurrency, 1, 64)?;
        validate_real_context_count("judge_max_retries", self.judge_max_retries, 0, 10)?;
        if self.judge_persona_prompt.len() > 32_768 || self.judge_persona_prompt.contains('\0') {
            bail!("platform plugin real_context.judge_persona_prompt is invalid");
        }
        validate_real_context_count(
            "active_reply_supersede_window_seconds",
            self.active_reply_supersede_window_seconds as usize,
            1,
            300,
        )?;
        validate_real_context_count(
            "reply_restraint_recover_minutes",
            self.reply_restraint_recover_minutes as usize,
            1,
            1_440,
        )?;
        if !matches!(
            self.reply_restraint_strength.as_str(),
            "light" | "medium" | "strong"
        ) {
            bail!("platform plugin real_context.reply_restraint_strength must be light, medium, or strong");
        }
        validate_real_context_range(
            "reply_restraint_multiplier",
            self.reply_restraint_multiplier,
            0.0,
            3.0,
        )?;
        for (name, value) in [
            ("judge_relevance_weight", self.judge_relevance_weight),
            ("judge_willingness_weight", self.judge_willingness_weight),
            ("judge_social_weight", self.judge_social_weight),
            ("judge_timing_weight", self.judge_timing_weight),
            ("judge_continuity_weight", self.judge_continuity_weight),
            (
                "judge_should_reply_boost_score",
                self.judge_should_reply_boost_score,
            ),
            (
                "judge_should_reply_penalty_score",
                self.judge_should_reply_penalty_score,
            ),
            ("continuation_boost_score", self.continuation_boost_score),
            (
                "takeover_direct_trigger_boost_score",
                self.takeover_direct_trigger_boost_score,
            ),
        ] {
            validate_real_context_range(name, value, 0.0, 1.0)?;
        }
        let weight_sum = self.judge_relevance_weight
            + self.judge_willingness_weight
            + self.judge_social_weight
            + self.judge_timing_weight
            + self.judge_continuity_weight;
        if !weight_sum.is_finite() || weight_sum <= f64::EPSILON {
            bail!("platform plugin real_context judge weights must have a positive sum");
        }
        validate_real_context_count(
            "continuation_window_seconds",
            self.continuation_window_seconds as usize,
            1,
            86_400,
        )?;
        validate_real_context_count(
            "active_reply_reaction_timeout_seconds",
            self.active_reply_reaction_timeout_seconds as usize,
            1,
            86_400,
        )?;
        validate_real_context_count(
            "reply_target_quote_after_other_messages",
            self.reply_target_quote_after_other_messages as usize,
            0,
            100_000,
        )?;
        validate_real_context_count(
            "reply_target_mention_after_seconds",
            self.reply_target_mention_after_seconds as usize,
            0,
            86_400,
        )?;
        if self.active_reply_reaction_emoji_ids.len() > 100
            || self.active_reply_reaction_enable && self.active_reply_reaction_emoji_ids.is_empty()
            || self.active_reply_reaction_emoji_ids.contains(&0)
        {
            bail!("platform plugin real_context.active_reply_reaction_emoji_ids must contain 1-100 positive ids");
        }
        validate_real_context_strings(
            "moderation_keywords",
            &self.moderation_keywords,
            256,
            4_096,
        )?;
        validate_real_context_range(
            "moderation_min_severity",
            self.moderation_min_severity,
            0.0,
            10.0,
        )?;
        validate_real_context_count(
            "moderation_timeout_seconds",
            self.moderation_timeout_seconds as usize,
            0,
            600,
        )?;
        if self.moderation_custom_rules.len() > 32_768
            || self.moderation_custom_rules.contains('\0')
        {
            bail!("platform plugin real_context.moderation_custom_rules is invalid");
        }
        validate_real_context_count(
            "base64_moderation_min_chars",
            self.base64_moderation_min_chars,
            4,
            4_096,
        )?;
        validate_real_context_count(
            "base64_moderation_max_decoded_chars",
            self.base64_moderation_max_decoded_chars,
            1,
            1_000_000,
        )?;
        validate_real_context_probability(
            "base64_moderation_min_printable_ratio",
            self.base64_moderation_min_printable_ratio,
        )?;
        if self.base64_moderation_max_decoded_chars < self.base64_moderation_min_chars {
            bail!("platform plugin real_context Base64 decoded limit cannot be smaller than its minimum input length");
        }
        validate_real_context_count(
            "affection_update_timeout_seconds",
            self.affection_update_timeout_seconds as usize,
            0,
            3_600,
        )?;
        validate_real_context_range(
            "affection_min_score",
            self.affection_min_score,
            -1_000.0,
            999.0,
        )?;
        validate_real_context_range(
            "affection_max_score",
            self.affection_max_score,
            self.affection_min_score + 1.0,
            1_000.0,
        )?;
        validate_real_context_range(
            "affection_regular_max_score",
            self.affection_regular_max_score,
            self.affection_min_score + 1.0,
            self.affection_max_score,
        )?;
        validate_real_context_range(
            "affection_initial_score",
            self.affection_initial_score,
            self.affection_min_score,
            self.affection_max_score,
        )?;
        validate_real_context_range("affection_bias_min", self.affection_bias_min, -1.0, 1.0)?;
        validate_real_context_range("affection_bias_max", self.affection_bias_max, -1.0, 1.0)?;
        validate_real_context_range(
            "affection_gain_pivot",
            self.affection_gain_pivot,
            self.affection_min_score,
            self.affection_max_score,
        )?;
        validate_real_context_range(
            "affection_delta_scale",
            self.affection_delta_scale,
            0.1,
            5.0,
        )?;
        validate_real_context_range("affection_delta_min", self.affection_delta_min, -100.0, 0.0)?;
        validate_real_context_range("affection_delta_max", self.affection_delta_max, 0.0, 100.0)?;
        validate_real_context_probability(
            "affection_update_confidence_threshold",
            self.affection_update_confidence_threshold,
        )?;
        validate_real_context_range(
            "affection_daily_gain_limit",
            self.affection_daily_gain_limit,
            0.0,
            1_000.0,
        )?;
        validate_real_context_range(
            "affection_daily_loss_limit",
            self.affection_daily_loss_limit,
            0.0,
            1_000.0,
        )?;
        validate_real_context_count("affection_max_tags", self.affection_max_tags, 0, 200)?;
        validate_real_context_count(
            "affection_recent_events_for_prompt",
            self.affection_recent_events_for_prompt,
            0,
            20,
        )?;
        let mut unlimited = HashSet::with_capacity(self.affection_unlimited_user_ids.len());
        if self.affection_unlimited_user_ids.len() > 10_000
            || self
                .affection_unlimited_user_ids
                .iter()
                .any(|id| *id <= 0 || !unlimited.insert(*id))
        {
            bail!("platform plugin real_context.affection_unlimited_user_ids contains invalid or duplicate ids");
        }
        for (name, prompt) in [
            (
                "affection_prompt_estranged",
                &self.affection_prompt_estranged,
            ),
            ("affection_prompt_cold", &self.affection_prompt_cold),
            ("affection_prompt_neutral", &self.affection_prompt_neutral),
            ("affection_prompt_known", &self.affection_prompt_known),
            ("affection_prompt_friend", &self.affection_prompt_friend),
            ("affection_prompt_trusted", &self.affection_prompt_trusted),
            ("affection_prompt_close", &self.affection_prompt_close),
        ] {
            if prompt.chars().count() > 32_768 || prompt.contains('\0') {
                bail!("platform plugin real_context.{name} is invalid");
            }
        }
        for (name, models) in [("text_models", &self.text_models)] {
            let Some(models) = models else { continue };
            if models.is_empty() {
                bail!("platform plugin real_context.{name} must be omitted instead of empty");
            }
            let mut seen = HashSet::with_capacity(models.len());
            if models.iter().any(|model| {
                model.provider_id.trim().is_empty()
                    || model.model.trim().is_empty()
                    || !seen.insert((&model.provider_id, &model.model))
            }) {
                bail!("platform plugin real_context.{name} must contain unique, non-empty model references");
            }
        }
        let mut nicknames = HashSet::with_capacity(self.identity_mappings.len());
        if self.identity_mappings.len() > 10_000
            || self.identity_mappings.iter().any(|mapping| {
                mapping.user_id <= 0
                    || mapping.nickname.is_empty()
                    || mapping.nickname.trim() != mapping.nickname
                    || mapping.nickname.chars().count() > 128
                    || mapping.nickname.chars().any(char::is_control)
                    || !nicknames.insert(&mapping.nickname)
            })
        {
            bail!("platform plugin real_context.identity_mappings contains invalid or duplicate entries");
        }
        Ok(())
    }
}

pub(crate) fn normalize_real_context_instance(instance: &mut PlatformPluginInstanceConfig) {
    let Ok(mut settings) = RealContextPluginSettings::from_instance(instance) else {
        return;
    };
    settings.normalize();
    merge_real_context_settings(instance, &settings);
}

pub(crate) fn migrate_message_history_instance(plugins: &mut PlatformPluginsConfig) {
    if plugins
        .get(QQ_MESSAGE_HISTORY_PLUGIN_ID)
        .is_some_and(|instance| !instance.is_empty())
    {
        return;
    }
    let Some(real_context) = plugins.get(REAL_CONTEXT_PLUGIN_ID) else {
        return;
    };
    let enabled = (real_context.enabled == Some(false)
        || real_context.settings.get("record_enable") == Some(&serde_json::Value::Bool(false)))
    .then_some(false);
    let mut settings = serde_json::Map::new();
    for key in [
        "history_search_max_results",
        "history_safe_page_limit",
        "allow_cross_group_search",
    ] {
        if let Some(value) = real_context.settings.get(key).cloned() {
            let target_key = if key == "allow_cross_group_search" {
                "allow_cross_conversation_search"
            } else {
                key
            };
            settings.insert(target_key.to_string(), value);
        }
    }
    if enabled.is_some() || !settings.is_empty() {
        plugins.insert(
            QQ_MESSAGE_HISTORY_PLUGIN_ID.to_string(),
            PlatformPluginInstanceConfig { enabled, settings },
        );
    }
}

pub(crate) const DEPRECATED_REAL_CONTEXT_SETTINGS: &[&str] = &[
    "record_enable",
    "record_media_mode",
    "history_search_max_results",
    "history_safe_page_limit",
    "allow_cross_group_search",
    "group_member_page_size",
    "reply_context_messages",
    "active_context_messages",
    "context_messages",
    "activity_statistics_enable",
    "daily_reply_limit_per_session",
    "log_judge_decision",
    "keyword_trigger_enable",
    "keyword_trigger_keywords",
    "keyword_boost_score",
    "takeover_system_trigger_enable",
    "takeover_system_trigger_boost_score",
    "moderation_in_active_judge_enable",
    "moderation_custom_rules_enable",
    "check_contain",
    "judge_models",
    "affection_judge_models",
    "continuation_window_minutes",
];

pub(crate) fn migrate_real_context_settings_map(settings: &mut serde_json::Map<String, serde_json::Value>) {
    if !settings.contains_key("group_member_search_max_results") {
        if let Some(value) = settings.get("group_member_page_size").cloned() {
            settings.insert("group_member_search_max_results".to_string(), value);
        }
    }
    if !settings.contains_key("text_models") {
        let models = settings
            .get("judge_models")
            .cloned()
            .or_else(|| settings.get("affection_judge_models").cloned());
        if let Some(value) = models {
            settings.insert("text_models".to_string(), value);
        }
    }
    // One knob used to feed both the reply turn and the judge. Their optimal
    // sizes point in opposite directions — the reply wants a generous opening
    // snapshot, the judge wants a tight recent window — and so do their cost
    // models, since the judge runs on every message rather than once per turn.
    let legacy_window = settings
        .get("context_messages")
        .cloned()
        .or_else(|| settings.get("reply_context_messages").cloned())
        .or_else(|| settings.get("active_context_messages").cloned());
    if let Some(value) = legacy_window {
        for key in ["reply_context_window", "judge_context_window"] {
            if !settings.contains_key(key) {
                settings.insert(key.to_string(), value.clone());
            }
        }
    }
    if !settings.contains_key("takeover_direct_trigger_enable") {
        if let Some(value) = settings.get("takeover_system_trigger_enable").cloned() {
            settings.insert("takeover_direct_trigger_enable".to_string(), value);
        }
    }
    if !settings.contains_key("takeover_direct_trigger_boost_score") {
        if let Some(value) = settings.get("takeover_system_trigger_boost_score").cloned() {
            settings.insert("takeover_direct_trigger_boost_score".to_string(), value);
        }
    }
    if !settings.contains_key("continuation_window_seconds") {
        if let Some(minutes) = settings
            .get("continuation_window_minutes")
            .and_then(serde_json::Value::as_u64)
        {
            // 3 minutes was the old default, not a considered choice — carry
            // those users onto the current default instead of pinning them to
            // whatever it happened to be when the unit changed.
            let seconds = if minutes == 3 {
                RealContextPluginSettings::default().continuation_window_seconds
            } else {
                minutes.saturating_mul(60)
            };
            settings.insert(
                "continuation_window_seconds".to_string(),
                serde_json::json!(seconds),
            );
        }
    }
    for key in DEPRECATED_REAL_CONTEXT_SETTINGS {
        settings.remove(*key);
    }
}

pub(crate) fn mutate_real_context_settings(
    plugins: &mut PlatformPluginsConfig,
    mutate: impl FnOnce(&mut RealContextPluginSettings),
) {
    let Some(instance) = plugins.get_mut(REAL_CONTEXT_PLUGIN_ID) else {
        return;
    };
    let Ok(mut settings) = RealContextPluginSettings::from_instance(instance) else {
        return;
    };
    mutate(&mut settings);
    merge_real_context_settings(instance, &settings);
}

