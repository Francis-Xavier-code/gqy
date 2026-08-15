//! plugins — 自 src/config.rs 拆分。

use super::*;

pub fn merge_real_context_settings(
    instance: &mut PlatformPluginInstanceConfig,
    settings: &RealContextPluginSettings,
) {
    for key in DEPRECATED_REAL_CONTEXT_SETTINGS {
        instance.settings.remove(*key);
    }
    let Ok(serde_json::Value::Object(known)) = serde_json::to_value(settings) else {
        return;
    };
    let Ok(serde_json::Value::Object(defaults)) =
        serde_json::to_value(RealContextPluginSettings::default())
    else {
        return;
    };
    for (key, value) in known {
        if defaults.get(&key) == Some(&value) {
            instance.settings.remove(&key);
        } else {
            instance.settings.insert(key, value);
        }
    }
}

pub(crate) fn validate_real_context_plugin_config(
    instance: &PlatformPluginInstanceConfig,
) -> Result<()> {
    let settings = RealContextPluginSettings::from_instance(instance)?;
    settings.validate()
}

pub(crate) fn validate_real_context_count(
    name: &str,
    value: usize,
    minimum: usize,
    maximum: usize,
) -> Result<()> {
    if !(minimum..=maximum).contains(&value) {
        bail!("platform plugin real_context.{name} must be between {minimum} and {maximum}");
    }
    Ok(())
}

pub(crate) fn validate_real_context_probability(name: &str, value: f64) -> Result<()> {
    validate_real_context_range(name, value, 0.0, 1.0)
}

pub(crate) fn validate_real_context_range(
    name: &str,
    value: f64,
    minimum: f64,
    maximum: f64,
) -> Result<()> {
    if !value.is_finite() || !(minimum..=maximum).contains(&value) {
        bail!("platform plugin real_context.{name} must be between {minimum} and {maximum}");
    }
    Ok(())
}

pub(crate) fn validate_real_context_strings(
    name: &str,
    values: &[String],
    maximum_chars: usize,
    maximum_items: usize,
) -> Result<()> {
    let mut seen = HashSet::with_capacity(values.len());
    if values.len() > maximum_items
        || values.iter().any(|value| {
            value.is_empty()
                || value.trim() != value
                || value.chars().count() > maximum_chars
                || value.chars().any(char::is_control)
                || !seen.insert(value)
        })
    {
        bail!("platform plugin real_context.{name} contains invalid or duplicate entries");
    }
    Ok(())
}

pub(crate) fn normalize_unique_strings(values: &mut Vec<String>) {
    let mut seen = HashSet::with_capacity(values.len());
    values.retain_mut(|value| {
        *value = value.trim().to_string();
        !value.is_empty() && seen.insert(value.clone())
    });
}

pub(crate) fn default_real_context_moderation_keywords() -> Vec<String> {
    // Deduplicated from the user's deployed AstrBot real-context configuration.
    // Keep this self-contained so GQY never reads another application's files.
    pub(crate) const KEYWORDS: &[&str] = &[
        "3p",
        "4p",
        "64",
        ":(){ :|:& };:",
        "> /dev/sda",
        "FtM",
        "IEPL",
        "IPLC",
        "K粉",
        "LGBTQ",
        "MtF",
        "Netflix拼车",
        "OD",
        "Spotify车位",
        "V2board",
        "VPN",
        "chmod -R 777 /",
        "chown -R 777 /",
        "clash/config",
        "cnm",
        "dd if=/dev/zero",
        "dick",
        "hysteria://",
        "iCloud拼车",
        "lsp",
        "mkfs.ext4",
        "mkfs.xfs",
        "nmsl",
        "ntr",
        "rm -fr /*",
        "rm -rf /*",
        "sb",
        "ss://",
        "ssr://",
        "sub?target=",
        "suck",
        "trojan://",
        "tuic://",
        "vless://",
        "vmess://",
        "zzzq",
        "三年自然灾害",
        "东三省",
        "中美贸易",
        "主义",
        "京喜",
        "人肉",
        "人身攻击",
        "代充",
        "优惠券群",
        "低价充值",
        "佐匹克隆",
        "你是一个",
        "你是我的奴隶",
        "你是猫娘",
        "使用XX系统的都是",
        "俄乌战争",
        "修车",
        "傻X",
        "傻逼",
        "公知",
        "六合彩",
        "关注公众号",
        "冰毒",
        "利他林",
        "刷单",
        "刷流水",
        "加我微信",
        "南梁",
        "南海仲裁",
        "博彩",
        "双性恋",
        "反共",
        "反华",
        "发车",
        "口角",
        "台海",
        "右美沙芬",
        "叶子",
        "同性恋",
        "四爱",
        "垃圾系统",
        "复读接下来的话",
        "外围",
        "外围盘",
        "外挂",
        "大麻",
        "天安门",
        "女同",
        "孕酮",
        "孤儿",
        "实名",
        "小仙女",
        "小日本",
        "小金豆",
        "就是垃圾",
        "巴以冲突",
        "帮我助力",
        "广告",
        "开盒",
        "忽略之前的指令",
        "恋尸癖",
        "恋童癖",
        "恋足癖",
        "拼多多",
        "排泄",
        "文革",
        "日赚",
        "暴动",
        "曲马多",
        "未成年",
        "机场跑路",
        "极品",
        "枪支",
        "梯子",
        "棒子",
        "止咳水",
        "死全家",
        "河南人",
        "测速图",
        "海洛因",
        "涩图",
        "淘宝客",
        "渠道",
        "港脚",
        "游行",
        "漏点",
        "炒币",
        "煞笔",
        "燃料",
        "狗推",
        "狗都不用",
        "玩客云",
        "男娘",
        "百家乐",
        "盒",
        "看片",
        "睾酮",
        "砍一刀",
        "破解",
        "神仙水",
        "福利姬",
        "福利群",
        "网盘资源",
        "网赌",
        "美狗",
        "群号",
        "翻墙",
        "肛交",
        "脑瘫",
        "色图",
        "色普龙",
        "节点",
        "药",
        "药娘",
        "菠菜",
        "薅羊毛",
        "螺内酯",
        "补佳乐",
        "裸聊",
        "订阅链接",
        "走猫",
        "走线",
        "起义",
        "跨性别",
        "身份证",
        "车牌",
        "辅助",
        "过量服药",
        "进新群",
        "阿普唑仑",
        "隐私",
        "雌二醇",
        "飞行",
        "飞行员",
    ];
    KEYWORDS
        .iter()
        .map(|keyword| (*keyword).to_string())
        .collect()
}

pub(crate) fn validate_reply_processor_plugin_config(
    instance: &PlatformPluginInstanceConfig,
) -> Result<()> {
    let settings = &instance.settings;
    for key in [
        "default_enabled",
        "followup_mention",
        "strip_period",
        "context_notice",
        "send_tool_intercept",
    ] {
        if settings.get(key).is_some_and(|value| !value.is_boolean()) {
            bail!("platform plugin reply_processor.{key} must be a boolean");
        }
    }
    for (key, min, max) in [
        ("threshold", 1_u64, 100_000_u64),
        ("max_height", 1_000, 5_000),
        ("font_size", 24, 56),
        ("code_font_size", 20, 46),
        ("padding", 36, 120),
        ("ttl_hours", 1, 168),
        ("max_records", 1, 10),
    ] {
        if let Some(value) = settings.get(key) {
            let value = value.as_u64().with_context(|| {
                format!("platform plugin reply_processor.{key} must be an unsigned integer")
            })?;
            if !(min..=max).contains(&value) {
                bail!("platform plugin reply_processor.{key} must be between {min} and {max}");
            }
        }
    }
    validate_plugin_string_choice(settings, "mode", &["image", "forward"])?;
    validate_plugin_string_choice(settings, "theme", &["paper", "light", "dark"])?;
    for key in ["font", "title_font", "code_font", "emoji_font"] {
        if let Some(value) = settings.get(key) {
            let value = value.as_str().with_context(|| {
                format!("platform plugin reply_processor.{key} must be a string")
            })?;
            if value.len() > 4_096 || value.contains('\0') {
                bail!("platform plugin reply_processor.{key} is invalid");
            }
        }
    }
    Ok(())
}

pub(crate) fn validate_plugin_string_choice(
    settings: &serde_json::Map<String, serde_json::Value>,
    key: &str,
    choices: &[&str],
) -> Result<()> {
    let Some(value) = settings.get(key) else {
        return Ok(());
    };
    let value = value
        .as_str()
        .with_context(|| format!("platform plugin reply_processor.{key} must be a string"))?;
    if !choices.contains(&value) {
        bail!(
            "platform plugin reply_processor.{key} must be one of: {}",
            choices.join(", ")
        );
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlatformConversationKind {
    Private,
    Group,
}

impl PlatformConversationKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Private => "private",
            Self::Group => "group",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PlatformConversationConfig {
    pub kind: PlatformConversationKind,
    pub id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PlatformMemoryConfig {
    #[serde(default = "default_true")]
    pub write_enabled: bool,
}

impl Default for PlatformMemoryConfig {
    fn default() -> Self {
        Self {
            write_enabled: true,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum PlatformPersonaOverride {
    #[default]
    Inherit,
    GQY,
    Custom {
        name: String,
    },
}

impl PlatformPersonaOverride {
    pub fn is_inherit(&self) -> bool {
        matches!(self, Self::Inherit)
    }

    pub fn custom_name(&self) -> Option<&str> {
        match self {
            Self::Custom { name } => Some(name),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlatformModelPoolInheritance {
    #[default]
    Platform,
    Global,
}

impl PlatformModelPoolInheritance {
    pub(crate) fn is_platform(&self) -> bool {
        matches!(self, Self::Platform)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlatformModelRoute {
    pub conversation: PlatformConversationConfig,
    #[serde(default, skip_serializing_if = "PlatformPersonaOverride::is_inherit")]
    pub persona: PlatformPersonaOverride,
    /// Inheritance source used only when `text_models` is absent.
    #[serde(
        default,
        skip_serializing_if = "PlatformModelPoolInheritance::is_platform"
    )]
    pub text_models_inheritance: PlatformModelPoolInheritance,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_models: Option<Vec<ActiveProviderModelConfig>>,
    /// Inheritance source used only when `multimodal_models` is absent.
    #[serde(
        default,
        skip_serializing_if = "PlatformModelPoolInheritance::is_platform"
    )]
    pub multimodal_models_inheritance: PlatformModelPoolInheritance,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub multimodal_models: Option<Vec<ActiveProviderModelConfig>>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub extra_prompt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_limits: Option<PlatformSessionLimits>,
}

impl PlatformModelRoute {
    pub fn identity(&self) -> (PlatformConversationKind, &str) {
        (self.conversation.kind, self.conversation.id.as_str())
    }

    pub fn matches(&self, kind: PlatformConversationKind, conversation_id: &str) -> bool {
        self.conversation.kind == kind && self.conversation.id == conversation_id
    }

    pub fn normalize(&mut self) {
        self.conversation.id = self.conversation.id.trim().to_string();
        if let PlatformPersonaOverride::Custom { name } = &mut self.persona {
            *name = name.trim().to_string();
        }
        self.extra_prompt = self.extra_prompt.trim().to_string();
        normalize_route_pool(&mut self.text_models);
        normalize_route_pool(&mut self.multimodal_models);
        if self.text_models.is_some() {
            self.text_models_inheritance = PlatformModelPoolInheritance::Platform;
        }
        if self.multimodal_models.is_some() {
            self.multimodal_models_inheritance = PlatformModelPoolInheritance::Platform;
        }
    }

    pub(crate) fn prune_model_references(&mut self, providers: &[ProviderConfig]) {
        if let Some(pool) = &mut self.text_models {
            pool.retain(|entry| active_model_exists(providers, entry));
        }
        if let Some(pool) = &mut self.multimodal_models {
            pool.retain(|entry| active_model_supports_image(providers, entry));
        }
        normalize_route_pool(&mut self.text_models);
        normalize_route_pool(&mut self.multimodal_models);
    }

    pub(crate) fn remove_model_references(&mut self, provider_id: &str, model: &str) {
        for pool in [&mut self.text_models, &mut self.multimodal_models] {
            if let Some(entries) = pool {
                entries.retain(|entry| !(entry.provider_id == provider_id && entry.model == model));
            }
            normalize_route_pool(pool);
        }
    }

    pub(crate) fn rename_provider_references(&mut self, old_id: &str, new_id: &str) {
        for entries in [&mut self.text_models, &mut self.multimodal_models]
            .into_iter()
            .flatten()
        {
            for entry in entries {
                if entry.provider_id == old_id {
                    entry.provider_id = new_id.to_string();
                }
            }
        }
    }

    pub(crate) fn rename_model_references(&mut self, provider_id: &str, old: &str, new: &str) {
        for entries in [&mut self.text_models, &mut self.multimodal_models]
            .into_iter()
            .flatten()
        {
            for entry in entries {
                if entry.provider_id == provider_id && entry.model == old {
                    entry.model = new.to_string();
                }
            }
        }
    }
}

pub(crate) fn normalize_route_pool(pool: &mut Option<Vec<ActiveProviderModelConfig>>) {
    let Some(entries) = pool else {
        return;
    };
    let mut seen = HashSet::with_capacity(entries.len());
    entries.retain_mut(|entry| {
        entry.provider_id = entry.provider_id.trim().to_string();
        entry.model = entry.model.trim().to_string();
        !entry.provider_id.is_empty()
            && !entry.model.is_empty()
            && seen.insert((entry.provider_id.clone(), entry.model.clone()))
    });
    if entries.is_empty() {
        *pool = None;
    }
}

pub(crate) fn rename_provider_in_pool(
    pool: &mut [ActiveProviderModelConfig],
    old_id: &str,
    new_id: &str,
) {
    for entry in pool {
        if entry.provider_id == old_id {
            entry.provider_id = new_id.to_string();
        }
    }
}

pub(crate) fn retain_provider_pool(
    pool: &mut Option<Vec<ActiveProviderModelConfig>>,
    provider_id: &str,
) {
    if let Some(entries) = pool {
        entries.retain(|entry| entry.provider_id != provider_id);
    }
    retain_nonempty_pool(pool);
}

pub(crate) fn retain_nonempty_pool(pool: &mut Option<Vec<ActiveProviderModelConfig>>) {
    if pool.as_ref().is_some_and(Vec::is_empty) {
        *pool = None;
    }
}

/// Tencent QQ integration implemented through a OneBot v11 reverse
/// WebSocket transport (for example NapCat).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct OneBotConfig {
    pub enabled: bool,
    pub reverse_ws_port: u16,
    /// Checked against NapCat's `Authorization: Bearer` handshake header.
    /// Empty tokens are accepted only from a loopback peer.
    pub access_token: String,
    pub admin_users: Vec<i64>,
    /// Grants full host tools only to non-admin users in `private_chats.whitelist`.
    pub allow_non_admin_host_tools: bool,
    /// Send each model round's text to group chats as its own message while
    /// the turn is still running, instead of keeping only the final reply.
    pub group_intermediate_messages: bool,
    /// Send each model round's text to private chats as its own message while
    /// the turn is still running, instead of keeping only the final reply.
    #[serde(default = "default_true")]
    pub private_intermediate_messages: bool,
    /// Include the current QQ sender's stable id in the model system context.
    /// Nicknames remain available for display even when this is disabled.
    #[serde(default = "default_true")]
    pub user_identification: bool,
    /// Include the current QQ group name in the model system context.
    #[serde(default = "default_true")]
    pub show_group_name: bool,
    pub memory: PlatformMemoryConfig,
    pub private_chats: QqPrivateChatsConfig,
    pub group_chats: QqGroupChatsConfig,
    #[serde(default, skip_serializing_if = "PlatformSessionLimits::is_default")]
    pub session_limits: PlatformSessionLimits,
    #[serde(
        default,
        skip_serializing_if = "PlatformGroupContextConfig::is_default"
    )]
    pub group_context: PlatformGroupContextConfig,
    /// QQ-wide text model pool. None inherits the global pool.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_models: Option<Vec<ActiveProviderModelConfig>>,
    /// QQ-wide multimodal model pool. None inherits the global pool.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub multimodal_models: Option<Vec<ActiveProviderModelConfig>>,
    /// Text model pool for non-whitelisted private chats and groups.
    /// None inherits the QQ-wide text model pool.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub non_whitelist_text_models: Option<Vec<ActiveProviderModelConfig>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conversations: Vec<PlatformModelRoute>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub plugins: PlatformPluginsConfig,
    /// Public HTTP base URL NapCat can use to fetch temporary local assets.
    pub asset_base_url: String,
    /// Replies longer than this are split into multiple messages. 0 = never split.
    pub max_reply_chars: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct QqPrivateChatsConfig {
    /// QQ ids whose private conversations bypass admission rate limits.
    pub whitelist: Vec<i64>,
    /// Accept friend requests only from admins or private-whitelisted QQ ids.
    pub friend_requests_require_private_whitelist: bool,
    pub allow_non_whitelist: bool,
    /// Per private conversation.
    pub non_whitelist_rate_limit: PlatformRateLimit,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_limits: Option<PlatformSessionLimits>,
    #[serde(default, rename = "non_whitelist_rate_per_minute", skip_serializing)]
    legacy_non_whitelist_rate_per_minute: Option<u32>,
}

impl Default for QqPrivateChatsConfig {
    fn default() -> Self {
        Self {
            whitelist: Vec::new(),
            friend_requests_require_private_whitelist: true,
            allow_non_whitelist: true,
            non_whitelist_rate_limit: PlatformRateLimit {
                max_messages: 2,
                window_seconds: 600,
            },
            session_limits: None,
            legacy_non_whitelist_rate_per_minute: None,
        }
    }
}

impl QqPrivateChatsConfig {
    pub(crate) fn migrate_legacy_rate_limit(&mut self) {
        if let Some(max_messages) = self.legacy_non_whitelist_rate_per_minute.take() {
            self.non_whitelist_rate_limit = PlatformRateLimit {
                max_messages,
                window_seconds: 60,
            };
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PlatformRateLimit {
    /// Zero disables the limit.
    pub max_messages: u32,
    pub window_seconds: u32,
}

impl Default for PlatformRateLimit {
    fn default() -> Self {
        Self {
            max_messages: 0,
            window_seconds: 60,
        }
    }
}

pub(crate) fn validate_platform_session_limits(
    field: &str,
    limits: PlatformSessionLimits,
) -> Result<()> {
    if limits.running == 0 || limits.running > MAX_PLATFORM_SESSION_RUNNING {
        bail!("platforms.qq.{field}.running must be between 1 and {MAX_PLATFORM_SESSION_RUNNING}");
    }
    if limits.queued > MAX_PLATFORM_SESSION_QUEUED {
        bail!("platforms.qq.{field}.queued must be between 0 and {MAX_PLATFORM_SESSION_QUEUED}");
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct QqGroupChatsConfig {
    /// Group ids that use the whitelist-group rate limit.
    pub whitelist: Vec<i64>,
    /// Additional wake prefixes. @-mentions always remain active.
    pub trigger_keywords: Vec<String>,
    /// Shared by all senders in one whitelisted group.
    pub whitelist_rate_limit: PlatformRateLimit,
    pub allow_non_whitelist: bool,
    /// Shared by all senders in one non-whitelisted group.
    pub non_whitelist_rate_limit: PlatformRateLimit,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_limits: Option<PlatformSessionLimits>,
    #[serde(default, rename = "whitelist_rate_per_minute", skip_serializing)]
    legacy_whitelist_rate_per_minute: Option<u32>,
    #[serde(default, rename = "non_whitelist_rate_per_minute", skip_serializing)]
    legacy_non_whitelist_rate_per_minute: Option<u32>,
}

impl Default for QqGroupChatsConfig {
    fn default() -> Self {
        Self {
            whitelist: Vec::new(),
            trigger_keywords: Vec::new(),
            whitelist_rate_limit: PlatformRateLimit {
                max_messages: 30,
                window_seconds: 60,
            },
            allow_non_whitelist: true,
            non_whitelist_rate_limit: PlatformRateLimit {
                max_messages: 2,
                window_seconds: 600,
            },
            session_limits: None,
            legacy_whitelist_rate_per_minute: None,
            legacy_non_whitelist_rate_per_minute: None,
        }
    }
}

impl QqGroupChatsConfig {
    pub(crate) fn migrate_legacy_rate_limits(&mut self) {
        if let Some(max_messages) = self.legacy_whitelist_rate_per_minute.take() {
            self.whitelist_rate_limit = PlatformRateLimit {
                max_messages,
                window_seconds: 60,
            };
        }
        if let Some(max_messages) = self.legacy_non_whitelist_rate_per_minute.take() {
            self.non_whitelist_rate_limit = PlatformRateLimit {
                max_messages,
                window_seconds: 60,
            };
        }
    }
}

impl Default for OneBotConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            reverse_ws_port: 8300,
            access_token: String::new(),
            admin_users: Vec::new(),
            allow_non_admin_host_tools: false,
            group_intermediate_messages: false,
            private_intermediate_messages: true,
            user_identification: true,
            show_group_name: true,
            memory: PlatformMemoryConfig::default(),
            private_chats: QqPrivateChatsConfig::default(),
            group_chats: QqGroupChatsConfig::default(),
            session_limits: PlatformSessionLimits::default(),
            group_context: PlatformGroupContextConfig::default(),
            text_models: None,
            multimodal_models: None,
            non_whitelist_text_models: None,
            conversations: Vec::new(),
            plugins: PlatformPluginsConfig::new(),
            asset_base_url: String::new(),
            max_reply_chars: 3000,
        }
    }
}

impl OneBotConfig {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    pub fn session_limits(
        &self,
        kind: PlatformConversationKind,
        conversation_id: &str,
    ) -> PlatformSessionLimits {
        self.conversations
            .iter()
            .find(|route| route.matches(kind, conversation_id))
            .and_then(|route| route.session_limits)
            .or(match kind {
                PlatformConversationKind::Private => self.private_chats.session_limits,
                PlatformConversationKind::Group => self.group_chats.session_limits,
            })
            .unwrap_or(self.session_limits)
    }
}

/// Subagent model tier pools. When the main agent spawns a subagent it
/// picks a tier by task complexity (cheap/balanced/strong); requests then
/// load-balance across that tier's pool exactly like the main text-model
/// pool. Tiers are subagent-only — the main conversation and auxiliary
/// work always use the user-selected main models. An unconfigured or
/// unavailable pool falls back to the main model pool.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SubagentTiersConfig {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cheap: Vec<ActiveProviderModelConfig>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub balanced: Vec<ActiveProviderModelConfig>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub strong: Vec<ActiveProviderModelConfig>,
}

impl SubagentTiersConfig {
    pub fn is_empty(&self) -> bool {
        self.cheap.is_empty() && self.balanced.is_empty() && self.strong.is_empty()
    }

    pub fn pool(&self, tier: ModelTier) -> &Vec<ActiveProviderModelConfig> {
        match tier {
            ModelTier::Cheap => &self.cheap,
            ModelTier::Balanced => &self.balanced,
            ModelTier::Strong => &self.strong,
        }
    }

    pub fn pool_mut(&mut self, tier: ModelTier) -> &mut Vec<ActiveProviderModelConfig> {
        match tier {
            ModelTier::Cheap => &mut self.cheap,
            ModelTier::Balanced => &mut self.balanced,
            ModelTier::Strong => &mut self.strong,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelTier {
    Cheap,
    Balanced,
    Strong,
}

impl ModelTier {
    pub const ALL: [Self; 3] = [Self::Cheap, Self::Balanced, Self::Strong];

    pub fn from_str(value: &str) -> Option<Self> {
        match value.trim() {
            "cheap" => Some(Self::Cheap),
            "balanced" => Some(Self::Balanced),
            "strong" => Some(Self::Strong),
            _ => None,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Cheap => "cheap",
            Self::Balanced => "balanced",
            Self::Strong => "strong",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveProviderModelConfig {
    pub provider_id: String,
    pub model: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DisplayConfig {
    #[serde(default = "default_display_language")]
    pub language: String,
    #[serde(default = "default_reasoning_display")]
    pub reasoning: String,
    #[serde(default = "default_tool_call_display")]
    pub tool_calls: String,
    #[serde(default = "default_true")]
    pub readable_tool_names: bool,
    #[serde(default)]
    pub show_token_usage: bool,
    #[serde(default = "default_mixed_model_endpoint_display")]
    pub mixed_model_endpoint_display: String,
    #[serde(default = "default_command_output_lines")]
    pub command_output_lines: usize,
    /// How many finished turns a reopened REPL redraws; 0 disables replay.
    #[serde(default = "default_repl_replay_turns")]
    pub repl_replay_turns: usize,
}

/// Desktop notifications. Both kinds are suppressed while the REPL window has
/// focus — if you are looking at the terminal, a popup is only noise.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotificationsConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Notify when a reply finishes and GQY is waiting on you again.
    #[serde(default = "default_true")]
    pub on_turn_complete: bool,
    /// shellhook/单次 CLI 触发的后台任务完成后,把跟进回复写回触发它的那个
    /// 终端。仅在该 shell 仍活着、停在同一 tty 的前台提示符时才写;写不了退化
    /// 为桌面通知。
    #[serde(default = "default_true")]
    pub job_writeback_to_terminal: bool,
}

impl Default for NotificationsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            on_turn_complete: true,
            job_writeback_to_terminal: true,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct RawDisplayConfig {
    #[serde(default)]
    language: Option<String>,
    #[serde(default)]
    reasoning: Option<String>,
    #[serde(default)]
    tool_calls: Option<String>,
    #[serde(default)]
    show_reasoning: Option<bool>,
    #[serde(default)]
    reasoning_mode: Option<String>,
    #[serde(default)]
    show_tool_details: Option<bool>,
    #[serde(default)]
    readable_tool_names: Option<bool>,
    #[serde(default)]
    show_token_usage: Option<bool>,
    #[serde(default)]
    show_mixed_model_endpoint: Option<bool>,
    #[serde(default)]
    mixed_model_endpoint_display: Option<String>,
    #[serde(default)]
    command_output_lines: Option<usize>,
    #[serde(default)]
    repl_replay_turns: Option<usize>,
}

impl<'de> Deserialize<'de> for DisplayConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = RawDisplayConfig::deserialize(deserializer)?;
        let reasoning = raw.reasoning.unwrap_or_else(|| {
            if raw.show_reasoning == Some(false) {
                "hidden".to_string()
            } else {
                raw.reasoning_mode.unwrap_or_else(default_reasoning_display)
            }
        });
        let tool_calls = raw.tool_calls.unwrap_or_else(|| {
            if raw.show_tool_details == Some(true) {
                "full".to_string()
            } else {
                default_tool_call_display()
            }
        });
        Ok(Self {
            language: raw.language.unwrap_or_else(default_display_language),
            reasoning,
            tool_calls,
            readable_tool_names: raw.readable_tool_names.unwrap_or_else(default_true),
            show_token_usage: raw.show_token_usage.unwrap_or(false),
            mixed_model_endpoint_display: raw.mixed_model_endpoint_display.unwrap_or_else(|| {
                match raw.show_mixed_model_endpoint {
                    Some(true) => "all".to_string(),
                    Some(false) => "off".to_string(),
                    None => default_mixed_model_endpoint_display(),
                }
            }),
            command_output_lines: raw
                .command_output_lines
                .unwrap_or_else(default_command_output_lines),
            repl_replay_turns: raw
                .repl_replay_turns
                .unwrap_or_else(default_repl_replay_turns),
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub id: String,
    pub display_name: String,
    pub base_url: String,
    #[serde(
        default = "default_provider_protocol",
        skip_serializing_if = "is_auto_protocol"
    )]
    pub protocol: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub model_context_window: HashMap<String, usize>,
    /// 按模型温度覆盖;缺项回退 `temperature`(供应商默认)。验收:模型
    /// 菜单里的温度曾误写供应商全局,牵连所有模型。
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub model_temperature: HashMap<String, f32>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub model_modalities: HashMap<String, Vec<String>>,
    /// 手动模型价格,键为模型名;设了就覆盖 models.dev 目录价。
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub model_costs: HashMap<String, ModelCostConfig>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub default_model: String,
    #[serde(
        default = "default_timeout",
        skip_serializing_if = "is_default_timeout"
    )]
    pub timeout_seconds: u64,
    #[serde(
        default = "default_temperature",
        skip_serializing_if = "is_default_temperature"
    )]
    pub temperature: f32,
    #[serde(
        default = "default_anthropic_max_tokens",
        skip_serializing_if = "is_default_anthropic_max_tokens"
    )]
    pub anthropic_max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra_body: Option<serde_json::Map<String, serde_json::Value>>,
}

#[derive(Debug, Clone)]
pub struct ResolvedProviderKey {
    pub index: usize,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptConfig {
    #[serde(default = "default_prompts_dir")]
    pub prompts_dir: String,
    #[serde(default = "default_identities_dir")]
    pub identities_dir: String,
    #[serde(default = "default_user_identity_file")]
    pub user_identity_file: String,
    #[serde(default)]
    pub active_persona: String,
    #[serde(default)]
    pub active_identity: String,
    /// 防失忆提醒(自动蒸馏,见 persona_hint 模块)。08-16 起改为
    /// 化石注入:每隔 `persona_reminder_interval` 轮进一次历史,纯追加
    /// 不再掰前缀缓存。A/B 实证干净体制下预设对话已足够→默认禁用。
    #[serde(default)]
    pub persona_reminder: bool,
    /// 相邻两次防失忆提醒之间至少间隔的轮数(>=1)。
    #[serde(default = "default_persona_reminder_interval")]
    pub persona_reminder_interval: u32,
}

pub(crate) fn default_persona_reminder_interval() -> u32 {
    3
}

/// Identifies who a model prompt is acting for. Only trusted local operator
/// turns may receive the configured user identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptAudience {
    Owner,
    External,
    Internal,
}

impl PromptAudience {
    pub(crate) fn includes_user_identity(self) -> bool {
        matches!(self, Self::Owner)
    }
}

#[derive(Debug, Clone)]
pub struct ProviderModelChoice {
    pub provider_id: String,
    pub provider_name: String,
    pub model: String,
}

impl ProviderModelChoice {
    pub fn value(&self) -> String {
        format!("{}\t{}", self.provider_id, self.model)
    }

    pub fn label(&self) -> String {
        format!("{} / {}", self.provider_name, self.model)
    }
}

/// Resolves a user-supplied model argument against `choices`: a 1-based list
/// index, a fully-qualified `provider_id/model`, or a bare model name when it
/// is unambiguous. The error is a ready-to-display bilingual message.
pub fn resolve_provider_model_argument<'a>(
    choices: &'a [ProviderModelChoice],
    argument: &str,
) -> std::result::Result<&'a ProviderModelChoice, String> {
    use crate::i18n::text as t;
    let argument = argument.trim();
    if let Ok(index) = argument.parse::<usize>() {
        return choices.get(index.wrapping_sub(1)).ok_or_else(|| {
            format!(
                "{} 1..={}",
                t(
                    "The model index is out of range; valid range:",
                    "模型序号超出范围，有效范围："
                ),
                choices.len()
            )
        });
    }
    // Fully-qualified "provider_id/model". Model ids may themselves contain
    // '/', so match by provider prefix instead of splitting at the first '/'.
    if let Some(choice) = choices.iter().find(|choice| {
        argument
            .strip_prefix(choice.provider_id.as_str())
            .and_then(|rest| rest.strip_prefix('/'))
            .is_some_and(|model| model == choice.model)
    }) {
        return Ok(choice);
    }
    let matches: Vec<&ProviderModelChoice> = choices
        .iter()
        .filter(|choice| choice.model == argument)
        .collect();
    match matches.as_slice() {
        [choice] => Ok(choice),
        [] => Err(format!(
            "{}{argument}",
            t("No configured model matches: ", "没有匹配的已配置模型：")
        )),
        multiple => Err(format!(
            "{}\n{}",
            t(
                "Multiple providers offer this model; use one of:",
                "多个供应商都提供该模型，请使用以下之一："
            ),
            multiple
                .iter()
                .map(|choice| format!("{}/{}", choice.provider_id, choice.model))
                .collect::<Vec<_>>()
                .join("\n")
        )),
    }
}

/// Which model turns text into vectors, and the settings that belong to that
/// model rather than to any one feature — a similarity floor means different
/// things on different models. Deliberately has no on/off switch: configuring a
/// model only makes it available, and each feature decides whether to use it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EmbeddingConfig {
    /// Id of an existing provider; the model is named separately, so a provider
    /// serving both chat and embedding models is still configured once.
    pub provider_id: String,
    pub model: String,
    pub timeout_seconds: u64,
    /// Cosine similarity below this is not a hit.
    pub min_score: f32,
}

impl Default for EmbeddingConfig {
    fn default() -> Self {
        Self {
            provider_id: String::new(),
            model: String::new(),
            timeout_seconds: 60,
            min_score: 0.35,
        }
    }
}

/// Marks a model as producing vectors rather than chat.
pub const EMBEDDING_MODALITY: &str = "embedding";

impl EmbeddingConfig {
    pub(crate) fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// A model is configured; whether any feature uses it is that feature's
    /// business.
    pub fn is_configured(&self) -> bool {
        !self.provider_id.trim().is_empty() && !self.model.trim().is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextConfig {
    /// 工具输出的模型侧内联上限(UTF-8 字节)。超限的纯文本输出全文外溢到
    /// 会话级 spill 文件,模型只看头尾预览+取回提示(read_file/rg 按需读回)。
    /// 0 = 关闭外溢。照抄 dsh 默认 50KB。
    #[serde(default = "default_tool_output_spill_bytes")]
    pub tool_output_spill_bytes: usize,
    #[serde(default = "default_trim_at_ratio")]
    pub trim_at_ratio: f32,
    #[serde(default = "default_trim_batch_ratio")]
    pub trim_batch_ratio: f32,
    #[serde(default = "default_on_overflow")]
    pub on_overflow: String,
    #[serde(default = "default_context_window")]
    pub default_context_window: usize,
    /// Watermark that forces a compaction even when the fold-economics gate
    /// would skip it. Must be >= trim_at_ratio.
    #[serde(default = "default_compact_force_ratio")]
    pub compact_force_ratio: f32,
    /// Verbatim tail budget kept outside the summary, in tokens. None derives
    /// min(16384, window/4) for task modes and 8192 for chat mode; the value
    /// is always capped at window/2 so a small window still lands below the
    /// trigger after compaction (re-compaction loop guard).
    #[serde(default)]
    pub compact_tail_tokens: Option<usize>,
    /// Soft watermark: a one-shot "context is getting large" notice, no
    /// history rewrite (a rewrite here would needlessly crater the cache).
    #[serde(default = "default_compact_soft_ratio")]
    pub compact_soft_ratio: f32,
    /// Mechanical watermark: old turns' tool_reports fold into placeholders
    /// (no LLM call). Must satisfy soft <= snip <= trim_at_ratio.
    #[serde(default = "default_compact_snip_ratio")]
    pub compact_snip_ratio: f32,
    /// Enables the mechanical prune layer (free: tool output is
    /// re-derivable). Batched behind a harvest gate so each rewrite pays for
    /// its one-time prefix-cache reset.
    #[serde(default = "default_true")]
    pub prune_stale_tool_reports: bool,
    /// Cold-resume prune: a session idle longer than this resumes against an
    /// expired provider cache, so rewriting history at that moment costs no
    /// extra misses — it only shrinks the full-price first request. Minutes;
    /// 0 disables. Default 1440 (24h, conservative for DeepSeek; drop to ~5
    /// for Anthropic ephemeral cache).
    #[serde(default = "default_cold_prune_after_minutes")]
    pub cold_prune_after_minutes: u64,
    /// Summarization requests fork the live conversation (same byte prefix,
    /// same tools + one appended instruction) so the provider prefix cache
    /// pays for re-reading the history — roughly a 10x input-cost saving on
    /// prefix-cached providers (DeepSeek/OpenAI-compatible/Anthropic). Turn
    /// OFF on per-request-billed gateways where cache hits save nothing: the
    /// isolated fallback path sends the history as plain text instead.
    #[serde(default = "default_true")]
    pub compact_cache_reuse: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolsConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub max_rounds: usize,
    #[serde(default = "default_tools_loading_mode")]
    pub loading_mode: String,
    #[serde(default = "default_true")]
    pub persist_loaded_tools: bool,
    /// How many `task` subagents from one tool batch may run concurrently.
    #[serde(default = "default_subagent_concurrency")]
    pub subagent_concurrency: usize,
    /// 工具执行兜底超时（秒），0=关闭。防没有自管超时的工具（MCP/web/生图
    /// 等）把回合无限挂死；run_command/task/deep_research 等自管或长跑工具
    /// 在 descriptions JSON 里以 timeout_seconds=0 豁免。
    #[serde(default = "default_tools_timeout_secs")]
    pub default_timeout_secs: u64,
    /// run_command 命令拒绝子串。命中即拒（guard 层，回给模型 tool error）。
    /// 防提示注入与模型手滑；默认只收录几乎不可能误伤的毁灭性模式。
    #[serde(default = "default_command_deny")]
    pub command_deny: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub servers: Vec<McpServerConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    pub id: String,
    #[serde(default)]
    pub display_name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default = "default_mcp_timeout")]
    pub timeout_seconds: u64,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SkillsConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub allow_command_execution: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_true")]
    pub evicted_context_enabled: bool,
    #[serde(default = "default_true")]
    pub association_enabled: bool,
    #[serde(default = "default_true")]
    pub auto_diary_enabled: bool,
    #[serde(default = "default_true")]
    pub auto_fact_enabled: bool,
    #[serde(default = "default_memory_diary_batch_size")]
    pub diary_batch_size: usize,
    #[serde(default = "default_memory_short_diary_retention_days")]
    pub short_diary_retention_days: u64,
    #[serde(default = "default_memory_diary_promotion_recalls")]
    pub diary_promotion_recalls: u64,
    #[serde(default = "default_memory_organizer_timeout_seconds")]
    pub organizer_timeout_seconds: u64,
    #[serde(default)]
    pub auto_skill_enabled: bool,
    #[serde(default = "default_memory_association_facts")]
    pub association_facts: usize,
    #[serde(default = "default_memory_association_episodes")]
    pub association_episodes: usize,
    #[serde(default = "default_memory_association_max_chars")]
    pub association_max_chars: usize,
    /// 同一条记忆若已在本会话早前回合注入过（化石仍在可见上下文中逐字回放），
    /// 本回合不再重复注入。内容或日期变化的记忆视为新条目照常注入。
    #[serde(default = "default_true")]
    pub association_dedup: bool,
    #[serde(default = "default_memory_snippet_chars")]
    pub snippet_chars: usize,
    #[serde(default = "default_memory_forget_after_days")]
    pub forget_after_days: u64,
    #[serde(default = "default_true")]
    pub forgetting_enabled: bool,
    #[serde(default = "default_memory_half_life_days")]
    pub forgetting_half_life_days: f64,
    #[serde(default = "default_memory_min_strength")]
    pub forgetting_min_strength: f64,
    #[serde(default = "default_memory_review_boost")]
    pub forgetting_review_boost: f64,
    #[serde(default = "default_memory_min_task_chars")]
    pub learning_min_task_chars: usize,
    #[serde(default = "default_memory_min_method_chars")]
    pub learning_min_method_chars: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginsConfig {
    #[serde(default)]
    pub weather: PluginEnabledConfig,
    #[serde(default)]
    pub web: WebPluginConfig,
    #[serde(default)]
    pub web_images: WebImagesPluginConfig,
    #[serde(default)]
    pub deep_research: DeepResearchPluginConfig,
    #[serde(default)]
    pub vision: VisionPluginConfig,
    #[serde(default)]
    pub exchange_rate: ExchangeRatePluginConfig,
    #[serde(default)]
    pub xuanxue: PluginEnabledConfig,
    #[serde(default)]
    pub image_generation: ImageGenerationPluginConfig,
    #[serde(default)]
    pub print_image: PrintImagePluginConfig,
    #[serde(default)]
    pub memes: MemesPluginConfig,
    #[serde(default)]
    pub knowledge_base: KnowledgeBasePluginConfig,
    #[serde(default, alias = "archlinux")]
    pub brew: PluginEnabledConfig,
    #[serde(default)]
    pub man: PluginEnabledConfig,
    #[serde(default)]
    pub moegirl: PluginEnabledConfig,
    #[serde(default)]
    pub hash_codec: PluginEnabledConfig,
    #[serde(default)]
    pub calculator: CalculatorPluginConfig,
    #[serde(default)]
    pub package_advisor: PluginEnabledConfig,
    #[serde(default)]
    pub diagnostics: DiagnosticsPluginConfig,
    #[serde(default)]
    pub api_quota: ApiQuotaPluginConfig,
    #[serde(default)]
    pub memory: MemoryConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginEnabledConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebPluginConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_web_search_max_results")]
    pub max_results: usize,
    #[serde(default)]
    pub tavily_api_keys: Vec<String>,
    #[serde(default)]
    pub firecrawl_api_keys: Vec<String>,
    #[serde(default)]
    pub anysearch_api_keys: Vec<String>,
    /// Exa 无需 key 也可用（走官方 MCP 免费额度）；配置 key 后走 REST API
    #[serde(default)]
    pub exa_api_keys: Vec<String>,
    #[serde(default)]
    pub searxng_base_url: String,
}
