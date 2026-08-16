//! defs — 自 src/web.rs 拆分。

#![allow(clippy::too_many_arguments, clippy::large_enum_variant)]
pub(crate) use super::*;

use crate::agent::{AgentEvent, AgentMode};

use crate::cli::build_tool_registry;
use crate::config::{ActiveProviderModelConfig, AppConfig, PromptAudience};
use crate::i18n::text as t;
use crate::ipc::ImageAttachment;

use crate::llm::{ChatStreamKind, OpenAiCompatibleClient};

use crate::paths::GQYPaths;
use crate::question::{QuestionAnswers, QuestionRequest, QuestionResponse};
use crate::state::{QueuedPrompt, StateStore};

use crate::tools::{self, CommandOutputStream};
use anyhow::{Context, Result};

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet, VecDeque};

use std::net::IpAddr;

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

use std::time::{Duration, Instant};

use tokio::sync::{broadcast, mpsc, oneshot};

use crate::platforms::{self, PlatformRuntime};

pub(crate) const JSON_BODY_LIMIT: usize = 4 * 1024 * 1024;
pub(crate) const PERSONA_ASSET_LIMIT: usize = 8 * 1024 * 1024;
pub(crate) const ATTACHMENT_BODY_LIMIT: usize = 10 * 1024 * 1024;
pub(crate) const MAX_ATTACHMENT_TOTAL_BYTES: u64 = 32 * 1024 * 1024;
pub(crate) const MAX_TEXT_ATTACHMENT_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_ATTACHMENTS_PER_MESSAGE: usize = 12;
pub(crate) const DEFAULT_BOARD_TITLE: &str = "今天想聊些什么？";
pub(crate) const DEFAULT_BOARD_SUBTITLE: &str = "从一个问题、计划或此刻的想法开始。";
pub(crate) const DEFAULT_STARTER_PROMPTS: [&str; 4] = [
    "查询今天的天气",
    "分析一个问题",
    "发表情包打个招呼吧",
    "搜索一张图片",
];
pub(crate) const MAX_CONTENT_CHARS: usize = 20_000;
pub(crate) const MAX_PROMPT_DOCUMENT_CHARS: usize = 200_000;
pub(crate) const MAX_PROMPT_DOCUMENTS: usize = 128;
pub(crate) const MAX_SECRET_CHARS: usize = 100_000;
pub(crate) const MAX_THINKING_VARIANT_UPDATES: usize = 64;
pub(crate) const EVENT_CAPACITY: usize = 4096;
pub(crate) const AUTH_COOKIE: &str = "gqy_session";
pub(crate) const LOGIN_WINDOW: Duration = Duration::from_secs(60);
pub(crate) const LOGIN_ATTEMPT_LIMIT: u8 = 5;

pub(crate) const INDEX_HTML: &str = include_str!("../../web/index.html");
pub(crate) const STYLES_CSS: &str = include_str!("../../web/styles.css");
pub(crate) const APP_JS: &str = include_str!("../../web/app.js");
// KaTeX 0.18.4(vendored):公式渲染;字体只带 woff2(css 里 woff2 列首,
// 现代浏览器不会去请求 woff/ttf 回退项)。
pub(crate) const KATEX_JS: &str = include_str!("../../web/vendor/katex/katex.min.js");
pub(crate) const KATEX_CSS: &str = include_str!("../../web/vendor/katex/katex.min.css");
pub(crate) static KATEX_FONTS: &[(&str, &[u8])] = &[
    (
        "KaTeX_AMS-Regular.woff2",
        include_bytes!("../../web/vendor/katex/fonts/KaTeX_AMS-Regular.woff2"),
    ),
    (
        "KaTeX_Caligraphic-Bold.woff2",
        include_bytes!("../../web/vendor/katex/fonts/KaTeX_Caligraphic-Bold.woff2"),
    ),
    (
        "KaTeX_Caligraphic-Regular.woff2",
        include_bytes!("../../web/vendor/katex/fonts/KaTeX_Caligraphic-Regular.woff2"),
    ),
    (
        "KaTeX_Fraktur-Bold.woff2",
        include_bytes!("../../web/vendor/katex/fonts/KaTeX_Fraktur-Bold.woff2"),
    ),
    (
        "KaTeX_Fraktur-Regular.woff2",
        include_bytes!("../../web/vendor/katex/fonts/KaTeX_Fraktur-Regular.woff2"),
    ),
    (
        "KaTeX_Main-Bold.woff2",
        include_bytes!("../../web/vendor/katex/fonts/KaTeX_Main-Bold.woff2"),
    ),
    (
        "KaTeX_Main-BoldItalic.woff2",
        include_bytes!("../../web/vendor/katex/fonts/KaTeX_Main-BoldItalic.woff2"),
    ),
    (
        "KaTeX_Main-Italic.woff2",
        include_bytes!("../../web/vendor/katex/fonts/KaTeX_Main-Italic.woff2"),
    ),
    (
        "KaTeX_Main-Regular.woff2",
        include_bytes!("../../web/vendor/katex/fonts/KaTeX_Main-Regular.woff2"),
    ),
    (
        "KaTeX_Math-BoldItalic.woff2",
        include_bytes!("../../web/vendor/katex/fonts/KaTeX_Math-BoldItalic.woff2"),
    ),
    (
        "KaTeX_Math-Italic.woff2",
        include_bytes!("../../web/vendor/katex/fonts/KaTeX_Math-Italic.woff2"),
    ),
    (
        "KaTeX_SansSerif-Bold.woff2",
        include_bytes!("../../web/vendor/katex/fonts/KaTeX_SansSerif-Bold.woff2"),
    ),
    (
        "KaTeX_SansSerif-Italic.woff2",
        include_bytes!("../../web/vendor/katex/fonts/KaTeX_SansSerif-Italic.woff2"),
    ),
    (
        "KaTeX_SansSerif-Regular.woff2",
        include_bytes!("../../web/vendor/katex/fonts/KaTeX_SansSerif-Regular.woff2"),
    ),
    (
        "KaTeX_Script-Regular.woff2",
        include_bytes!("../../web/vendor/katex/fonts/KaTeX_Script-Regular.woff2"),
    ),
    (
        "KaTeX_Size1-Regular.woff2",
        include_bytes!("../../web/vendor/katex/fonts/KaTeX_Size1-Regular.woff2"),
    ),
    (
        "KaTeX_Size2-Regular.woff2",
        include_bytes!("../../web/vendor/katex/fonts/KaTeX_Size2-Regular.woff2"),
    ),
    (
        "KaTeX_Size3-Regular.woff2",
        include_bytes!("../../web/vendor/katex/fonts/KaTeX_Size3-Regular.woff2"),
    ),
    (
        "KaTeX_Size4-Regular.woff2",
        include_bytes!("../../web/vendor/katex/fonts/KaTeX_Size4-Regular.woff2"),
    ),
    (
        "KaTeX_Typewriter-Regular.woff2",
        include_bytes!("../../web/vendor/katex/fonts/KaTeX_Typewriter-Regular.woff2"),
    ),
];
pub(crate) const GQY_LOGO: &[u8] = include_bytes!("../../pics/GQY-icon.png");
pub(crate) const GQY_WALLPAPER: &[u8] = include_bytes!("../../pics/GQY-image.png");

#[derive(Clone)]
pub(crate) struct DaemonState {
    pub(crate) auth: WebAuth,
    pub(crate) boot_id: Arc<str>,
    pub(crate) web_port: u16,
    pub(crate) web_public: bool,
    pub(crate) web_bind: IpAddr,
    pub(crate) paths: GQYPaths,
    pub(crate) manager: Arc<Mutex<ManagerState>>,
    pub(crate) state_store: StateStore,
    pub(crate) events: EventHub,
    pub(crate) questions: QuestionBroker,
    pub(crate) actor_tx: mpsc::UnboundedSender<ActorCommand>,
    pub(crate) shutdown_tx: broadcast::Sender<()>,
    pub(crate) turn_engine: TurnEngineState,
    pub(crate) platforms: PlatformRuntime,
}

#[cfg(test)]
impl DaemonState {
    pub(crate) fn for_test(paths: GQYPaths, web_port: u16) -> Result<Self> {
        let state_store = StateStore::new(&paths)?;
        let config = AppConfig::default();
        let context = cold_context(&config, &state_store)?;
        let manager = Arc::new(Mutex::new(ManagerState {
            config,
            active_runs: HashMap::new(),
            admin_busy: false,
            context,
            persona_session_ids: HashMap::new(),
        }));
        let (actor_tx, _actor_rx) = mpsc::unbounded_channel();
        let (shutdown_tx, _shutdown_rx) = broadcast::channel(1);
        Ok(Self {
            auth: WebAuth::new(None),
            boot_id: Arc::from("boot-test"),
            web_port,
            web_public: false,
            web_bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
            paths,
            manager,
            state_store,
            events: EventHub::new(),
            questions: QuestionBroker::new(),
            actor_tx,
            shutdown_tx,
            turn_engine: TurnEngineState::default(),
            platforms: PlatformRuntime::new()?,
        })
    }

    pub(crate) fn for_test_with_actor(
        paths: GQYPaths,
        web_port: u16,
    ) -> Result<(Self, std::thread::JoinHandle<Result<()>>)> {
        let state_store = StateStore::new(&paths)?;
        let config = AppConfig::default();
        let context = cold_context(&config, &state_store)?;
        let manager = Arc::new(Mutex::new(ManagerState {
            config: config.clone(),
            active_runs: HashMap::new(),
            admin_busy: false,
            context,
            persona_session_ids: HashMap::new(),
        }));
        let events = EventHub::new();
        let questions = QuestionBroker::new();
        let turn_engine = TurnEngineState::default();
        let (actor_tx, actor_join) = spawn_actor(
            config,
            paths.clone(),
            state_store.clone(),
            manager.clone(),
            events.clone(),
            questions.clone(),
            turn_engine.clone(),
            None,
        )?;
        let (shutdown_tx, _shutdown_rx) = broadcast::channel(1);
        Ok((
            Self {
                auth: WebAuth::new(None),
                boot_id: Arc::from("boot-test"),
                web_port,
                web_public: false,
                web_bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
                paths,
                manager,
                state_store,
                events,
                questions,
                actor_tx,
                shutdown_tx,
                turn_engine,
                platforms: PlatformRuntime::new()?,
            },
            actor_join,
        ))
    }
}

#[derive(Clone, Default)]
pub(crate) struct TurnEngineState(Arc<AtomicU8>);

impl TurnEngineState {
    pub(crate) const COLD: u8 = 0;
    pub(crate) const INITIALIZING: u8 = 1;
    pub(crate) const READY: u8 = 2;
    pub(crate) const FAILED: u8 = 3;

    pub(crate) fn set(&self, state: u8) {
        self.0.store(state, Ordering::Release);
    }

    pub(crate) fn is_ready(&self) -> bool {
        self.0.load(Ordering::Acquire) == Self::READY
    }

    pub(crate) fn label(&self) -> &'static str {
        match self.0.load(Ordering::Acquire) {
            Self::INITIALIZING => "initializing",
            Self::READY => "ready",
            Self::FAILED => "failed",
            _ => "cold",
        }
    }
}

/// Expensive per-turn dependencies are initialized on first use and shared
/// by subsequent turns. The cache is keyed by the effective configuration so
/// a QQ conversation-specific model pool gets its own client/tool snapshot.
/// Configuration reloads clear the cache before the next request.
pub(crate) struct TurnResources {
    pub(crate) client: OpenAiCompatibleClient,
    pub(crate) normal_tools: tools::ToolRegistry,
    pub(crate) dev_tools: tools::ToolRegistry,
    pub(crate) restricted_tools: tools::ToolRegistry,
}

pub(crate) const MAX_CACHED_TURN_RESOURCE_CONFIGS: usize = 16;

#[derive(Default)]
pub(crate) struct TurnResourceCache {
    entries: HashMap<[u8; 32], Arc<TurnResources>>,
    order: VecDeque<[u8; 32]>,
}

impl TurnResourceCache {
    pub(crate) fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
    }

    pub(crate) fn key(config: &AppConfig) -> Result<[u8; 32]> {
        let encoded =
            serde_json::to_vec(config).context("serializing effective turn configuration")?;
        Ok(*blake3::hash(&encoded).as_bytes())
    }

    pub(crate) fn get_or_build(
        &mut self,
        config: &AppConfig,
        paths: &GQYPaths,
    ) -> Result<Arc<TurnResources>> {
        let key = Self::key(config)?;
        if let Some(resources) = self.entries.get(&key).cloned() {
            self.order.retain(|entry| *entry != key);
            self.order.push_back(key);
            return Ok(resources);
        }

        crate::models_cache::ensure_active_metadata(paths, config);
        let restricted_tools = if config.tools.enabled {
            tools::restricted_platform_registry(config, paths)
        } else {
            tools::ToolRegistry::new()
        };
        tools::register_script_display_names(&restricted_tools);
        let resources = Arc::new(TurnResources {
            client: OpenAiCompatibleClient::from_config(config, paths)?,
            normal_tools: build_tool_registry(config, paths, AgentMode::Normal, false)?,
            dev_tools: build_tool_registry(config, paths, AgentMode::Dev, false)?,
            restricted_tools,
        });

        if self.entries.len() >= MAX_CACHED_TURN_RESOURCE_CONFIGS {
            if let Some(oldest) = self.order.pop_front() {
                self.entries.remove(&oldest);
            }
        }
        self.order.push_back(key);
        self.entries.insert(key, resources.clone());
        Ok(resources)
    }
}

#[derive(Clone)]
pub(crate) struct WebAuth {
    password_digest: Option<[u8; 32]>,
    sessions: Arc<Mutex<HashSet<String>>>,
    attempts: Arc<Mutex<HashMap<IpAddr, LoginAttempt>>>,
}

#[derive(Clone, Copy)]
pub(crate) struct LoginAttempt {
    window_started: Instant,
    failures: u8,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum LoginFailure {
    Invalid,
    RateLimited,
}

impl WebAuth {
    pub(crate) fn new(password: Option<&str>) -> Self {
        let password_digest = password.map(|password| {
            let mut digest = Sha256::new();
            digest.update(password.as_bytes());
            digest.finalize().into()
        });
        Self {
            password_digest,
            sessions: Arc::new(Mutex::new(HashSet::new())),
            attempts: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub(crate) fn required(&self) -> bool {
        self.password_digest.is_some()
    }

    pub(crate) fn is_authenticated(&self, supplied: Option<&str>) -> bool {
        if !self.required() {
            return true;
        }
        supplied.is_some_and(|token| self.sessions.lock().unwrap().contains(token))
    }

    pub(crate) fn login(
        &self,
        peer: IpAddr,
        password: &str,
    ) -> std::result::Result<String, LoginFailure> {
        let Some(expected) = self.password_digest else {
            return Ok(String::new());
        };
        let now = Instant::now();
        {
            let mut attempts = self.attempts.lock().unwrap();
            let entry = attempts.entry(peer).or_insert(LoginAttempt {
                window_started: now,
                failures: 0,
            });
            if now.duration_since(entry.window_started) >= LOGIN_WINDOW {
                entry.window_started = now;
                entry.failures = 0;
            }
            if entry.failures >= LOGIN_ATTEMPT_LIMIT {
                return Err(LoginFailure::RateLimited);
            }
        }

        let mut digest = Sha256::new();
        digest.update(password.as_bytes());
        let supplied: [u8; 32] = digest.finalize().into();
        if !constant_time_eq(&supplied, &expected) {
            let mut attempts = self.attempts.lock().unwrap();
            if let Some(entry) = attempts.get_mut(&peer) {
                entry.failures = entry.failures.saturating_add(1);
            }
            return Err(LoginFailure::Invalid);
        }

        let token = random_token(32);
        let mut sessions = self.sessions.lock().unwrap();
        sessions.insert(token.clone());
        if sessions.len() > 64 {
            sessions.clear();
            sessions.insert(token.clone());
        }
        Ok(token)
    }
}

/// A turn currently executing in the daemon.
pub(crate) struct RunInfo {
    pub(crate) session_id: Arc<str>,
    pub(crate) mode: AgentMode,
    pub(crate) audience: PromptAudience,
    /// Signals cancellation to the turn task; the task selects on the
    /// paired receiver.
    pub(crate) cancel: tokio::sync::watch::Sender<bool>,
    pub(crate) turn_id: Option<String>,
    pub(crate) queue_target: Option<crate::state::RunningTurnQueueTarget>,
    pub(crate) supersede: Arc<crate::agent::TurnSupersedeSignal>,
    pub(crate) platform_followup: Option<Arc<platforms::PlatformFollowupRun>>,
    pub(crate) operation: RunOperation,
    /// True for daemon-initiated background-command wake turns; lets REPL
    /// clients discover and attach to them for live rendering.
    pub(crate) job_wake: bool,
    /// 本回合的发起来源(goal 权限与取消语义用,见 workspace::TurnOrigin)。
    pub(crate) turn_origin: crate::tools::workspace::TurnOrigin,
    /// Display label for wake turns: "<job_id> · <title>".
    pub(crate) job_wake_label: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) enum RunOperation {
    Create,
    Redo { turn_id: String, input_id: String },
}

impl RunOperation {
    pub(crate) fn name(&self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Redo { .. } => "redo",
        }
    }

    pub(crate) fn turn_id(&self) -> Option<&str> {
        match self {
            Self::Create => None,
            Self::Redo { turn_id, .. } => Some(turn_id),
        }
    }

    pub(crate) fn input_id(&self) -> Option<&str> {
        match self {
            Self::Create => None,
            Self::Redo { input_id, .. } => Some(input_id),
        }
    }
}

impl RunInfo {
    pub(crate) fn request_cancel(&self) {
        if let Some(followup) = self.platform_followup.as_ref() {
            followup.close();
        }
        let _ = self.cancel.send(true);
    }
}

pub(crate) struct ManagerState {
    pub(crate) config: AppConfig,
    /// Concurrently running turns, keyed by run id. Turns run in parallel —
    /// including several in the same session (placeholder semantics) — so
    /// this replaces the old single `active_run_id`.
    pub(crate) active_runs: HashMap<String, RunInfo>,
    pub(crate) admin_busy: bool,
    pub(crate) context: ContextSnapshot,
    pub(crate) persona_session_ids: HashMap<String, String>,
}

impl ManagerState {
    /// A run currently executing in the given session, if any (most callers
    /// only need one representative — e.g. the WebUI compat field).
    pub(crate) fn run_in_session(&self, session_id: &str) -> Option<&String> {
        self.active_runs
            .iter()
            .find(|(_, info)| &*info.session_id == session_id)
            .map(|(run_id, _)| run_id)
    }

    pub(crate) fn session_has_runs(&self, session_id: &str) -> bool {
        self.active_runs
            .values()
            .any(|info| &*info.session_id == session_id)
    }

    pub(crate) fn session_has_redo(&self, session_id: &str) -> bool {
        self.active_runs.values().any(|info| {
            &*info.session_id == session_id && matches!(info.operation, RunOperation::Redo { .. })
        })
    }

    pub(crate) fn session_runs_match_audience(
        &self,
        session_id: &str,
        audience: PromptAudience,
    ) -> bool {
        let mut runs = self
            .active_runs
            .values()
            .filter(|info| &*info.session_id == session_id);
        runs.next().is_some_and(|first| {
            first.audience == audience && runs.all(|info| info.audience == audience)
        })
    }
}

#[derive(Clone, Copy, Debug, Serialize)]
pub(crate) struct ContextSnapshot {
    pub(crate) tokens: u64,
    pub(crate) window: Option<usize>,
    pub(crate) cumulative_tokens: u64,
    pub(crate) cumulative_prompt_tokens: u64,
    pub(crate) cumulative_cache_read_tokens: u64,
}

pub(crate) enum ActorCommand {
    StartTurn {
        run_id: String,
        session_id: Arc<str>,
        content: String,
        display_content: String,
        attachment_run_id: Option<String>,
        mode: AgentMode,
        images: Vec<Option<ImageAttachment>>,
        cwd: Option<std::path::PathBuf>,
        /// 触发回合的终端(shellhook/单次 CLI);后台任务完成回写用。
        origin_tty: Option<crate::ipc::OriginTty>,
        audience: PromptAudience,
        /// Platform-only per-turn overrides. CLI/WebUI turns leave this empty.
        profile: Option<platforms::TurnProfile>,
        cancel: tokio::sync::watch::Receiver<bool>,
        /// 回合发起来源(缺省 Human;goal 驱动器与 job 唤醒如实声明)。
        /// 装箱:GoalRound 变体带 String,内联会顶爆 ActorCommand 的
        /// 512B 队列项护栏。
        turn_origin: Box<crate::tools::workspace::TurnOrigin>,
    },
    RedoTurn {
        run_id: String,
        session_id: Arc<str>,
        candidate: crate::state::RedoCandidate,
        prompts: Vec<RedoWebPrompt>,
        mode: AgentMode,
        cancel: tokio::sync::watch::Receiver<bool>,
    },
    SetModels {
        models: Vec<ActiveProviderModelConfig>,
        reply: oneshot::Sender<std::result::Result<(), AdminFailure>>,
    },
    SetThinkingVariants {
        updates: Vec<ThinkingVariantUpdate>,
        reply: oneshot::Sender<std::result::Result<(), AdminFailure>>,
    },
    ApplyConfig {
        config: Box<AppConfig>,
        prompts: PromptDocuments,
        reset_conversation: bool,
        reply: oneshot::Sender<std::result::Result<(), AdminFailure>>,
    },
    ResetConversation {
        session_id: Arc<str>,
        reply: oneshot::Sender<std::result::Result<(), AdminFailure>>,
    },
    ResetPersonaState {
        config: Box<AppConfig>,
        reply: oneshot::Sender<std::result::Result<(), AdminFailure>>,
    },
    ClearSessionContent {
        session_id: Arc<str>,
        reply: oneshot::Sender<std::result::Result<(), AdminFailure>>,
    },
    SwitchSession {
        session_id: String,
        release_reservation: bool,
        reply: oneshot::Sender<std::result::Result<(), AdminFailure>>,
    },
    Undo {
        session_id: Arc<str>,
        reply: oneshot::Sender<std::result::Result<Value, AdminFailure>>,
    },
    Pop {
        session_id: Arc<str>,
        turn_ids: Vec<String>,
        reply: oneshot::Sender<std::result::Result<Value, AdminFailure>>,
    },
    Compact {
        session_id: Arc<str>,
        reply: oneshot::Sender<std::result::Result<Value, AdminFailure>>,
    },
    Shutdown,
}

#[derive(Debug)]
pub(crate) enum AdminFailure {
    Invalid(String),
    Internal(String),
}

#[derive(Debug)]
pub(crate) enum PlatformSessionResetError {
    Busy,
    Unavailable,
    Internal(String),
}

#[derive(Debug)]
pub(crate) enum PlatformPersonaResetError {
    Busy,
    Unavailable,
    Internal(String),
}

#[derive(Clone, Debug)]
pub(crate) struct EventRecord {
    pub(crate) id: u64,
    pub(crate) kind: String,
    pub(crate) data: String,
}

#[derive(Clone)]
pub(crate) struct EventHub {
    inner: Arc<Mutex<EventHubInner>>,
    sender: broadcast::Sender<EventRecord>,
}

pub(crate) struct EventHubInner {
    next_id: u64,
    records: VecDeque<EventRecord>,
}

pub(crate) struct EventSubscription {
    pub(crate) pending: VecDeque<EventRecord>,
    pub(crate) receiver: broadcast::Receiver<EventRecord>,
}

impl EventHub {
    pub(crate) fn new() -> Self {
        let (sender, _) = broadcast::channel(EVENT_CAPACITY);
        Self {
            inner: Arc::new(Mutex::new(EventHubInner {
                next_id: 1,
                records: VecDeque::with_capacity(EVENT_CAPACITY),
            })),
            sender,
        }
    }

    pub(crate) fn publish(&self, kind: impl Into<String>, data: Value) -> u64 {
        let mut inner = self.inner.lock().unwrap();
        let id = inner.next_id;
        inner.next_id = inner.next_id.saturating_add(1);
        let record = EventRecord {
            id,
            kind: kind.into(),
            data: serde_json::to_string(&data)
                .unwrap_or_else(|_| "{\"error\":\"event serialization failed\"}".to_string()),
        };
        if inner.records.len() == EVENT_CAPACITY {
            inner.records.pop_front();
        }
        inner.records.push_back(record.clone());
        let _ = self.sender.send(record);
        id
    }

    pub(crate) fn latest_id(&self) -> u64 {
        self.inner.lock().unwrap().next_id.saturating_sub(1)
    }

    pub(crate) fn subscribe_after(&self, after: u64) -> EventSubscription {
        let mut inner = self.inner.lock().unwrap();
        let receiver = self.sender.subscribe();
        let pending = replay_records(&mut inner, after);
        EventSubscription { pending, receiver }
    }

    pub(crate) fn replay_after(&self, after: u64) -> VecDeque<EventRecord> {
        replay_records(&mut self.inner.lock().unwrap(), after)
    }
}

pub(crate) fn replay_records(inner: &mut EventHubInner, after: u64) -> VecDeque<EventRecord> {
    if after > inner.next_id.saturating_sub(1) {
        return resync_record(inner);
    }
    let Some(oldest) = inner.records.front().map(|record| record.id) else {
        return VecDeque::new();
    };
    if after < oldest.saturating_sub(1) {
        return resync_record(inner);
    }
    inner
        .records
        .iter()
        .filter(|record| record.id > after)
        .cloned()
        .collect()
}

pub(crate) fn resync_record(inner: &mut EventHubInner) -> VecDeque<EventRecord> {
    let id = inner.next_id;
    inner.next_id = inner.next_id.saturating_add(1);
    VecDeque::from([EventRecord {
        id,
        kind: "resync_required".to_string(),
        data: json!({ "latest_event_id": id }).to_string(),
    }])
}

#[derive(Clone)]
pub(crate) struct QuestionBroker {
    pub(crate) pending: Arc<Mutex<HashMap<String, PendingQuestion>>>,
}

pub(crate) struct PendingQuestion {
    run_id: String,
    request: QuestionRequest,
    responder: oneshot::Sender<QuestionResponse>,
}

#[derive(Debug)]
pub(crate) enum AnswerFailure {
    NotFound,
    Invalid(String),
    Gone,
}

impl QuestionBroker {
    pub(crate) fn new() -> Self {
        Self {
            pending: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub(crate) fn insert(
        &self,
        run_id: &str,
        request: QuestionRequest,
        responder: oneshot::Sender<QuestionResponse>,
    ) -> String {
        let mut pending = self.pending.lock().unwrap();
        loop {
            let question_id = random_id("question", 18);
            if !pending.contains_key(&question_id) {
                pending.insert(
                    question_id.clone(),
                    PendingQuestion {
                        run_id: run_id.to_string(),
                        request,
                        responder,
                    },
                );
                return question_id;
            }
        }
    }

    pub(crate) fn answer<F>(
        &self,
        question_id: &str,
        answers: QuestionAnswers,
        before_resume: F,
    ) -> std::result::Result<(), AnswerFailure>
    where
        F: FnOnce(&str, &QuestionAnswers),
    {
        let mut all_pending = self.pending.lock().unwrap();
        let request = all_pending
            .get(question_id)
            .map(|pending| pending.request.clone())
            .ok_or(AnswerFailure::NotFound)?;
        let answers = normalize_answers(&request, answers).map_err(AnswerFailure::Invalid)?;
        let pending = all_pending
            .remove(question_id)
            .ok_or(AnswerFailure::NotFound)?;
        let run_id = pending.run_id;
        if pending.responder.is_closed() {
            return Err(AnswerFailure::Gone);
        }
        before_resume(&run_id, &answers);
        pending
            .responder
            .send(QuestionResponse::Answered(answers.clone()))
            .map_err(|_| AnswerFailure::Gone)?;
        Ok(())
    }

    pub(crate) fn close<F>(
        &self,
        question_id: &str,
        before_resume: F,
    ) -> std::result::Result<(), AnswerFailure>
    where
        F: FnOnce(&str),
    {
        let mut all_pending = self.pending.lock().unwrap();
        let pending = all_pending
            .remove(question_id)
            .ok_or(AnswerFailure::NotFound)?;
        let run_id = pending.run_id;
        if pending.responder.is_closed() {
            return Err(AnswerFailure::Gone);
        }
        before_resume(&run_id);
        pending
            .responder
            .send(QuestionResponse::Closed)
            .map_err(|_| AnswerFailure::Gone)?;
        Ok(())
    }

    pub(crate) fn cancel_run(&self, run_id: &str) {
        let cancelled = {
            let mut pending = self.pending.lock().unwrap();
            let ids = pending
                .iter()
                .filter(|(_, question)| question.run_id == run_id)
                .map(|(id, _)| id.clone())
                .collect::<Vec<_>>();
            ids.into_iter()
                .filter_map(|id| pending.remove(&id))
                .collect::<Vec<_>>()
        };
        for pending in cancelled {
            let _ = pending.responder.send(QuestionResponse::Cancelled);
        }
    }
}

pub(crate) struct RunEventMapper {
    run_id: String,
    events: EventHub,
    questions: QuestionBroker,
    state_store: StateStore,
    manager: Arc<Mutex<ManagerState>>,
    turn_id: Option<String>,
    active_tools: Vec<ActiveTool>,
    queue_ingress: Option<Arc<crate::agent::QueueIngressBarrier>>,
    operation: &'static str,
    redo_input_id: Option<String>,
    redo_display_content: Option<String>,
    command_output_lines: usize,
}

pub(crate) struct ActiveTool {
    id: String,
    name: String,
    display_name: String,
    command_output: Option<crate::render::CommandOutputTail>,
}

impl RunEventMapper {
    pub(crate) fn new(
        run_id: String,
        events: EventHub,
        questions: QuestionBroker,
        state_store: StateStore,
        manager: Arc<Mutex<ManagerState>>,
        queue_ingress: Option<Arc<crate::agent::QueueIngressBarrier>>,
        operation: &'static str,
        redo_input_id: Option<String>,
        redo_display_content: Option<String>,
        command_output_lines: usize,
    ) -> Self {
        Self {
            run_id,
            events,
            questions,
            state_store,
            manager,
            turn_id: None,
            active_tools: Vec::new(),
            queue_ingress,
            operation,
            redo_input_id,
            redo_display_content,
            command_output_lines,
        }
    }

    pub(crate) fn publish(&self, kind: &str, data: Value) {
        self.events.publish(kind, data);
    }

    pub(crate) fn next_tool(&self, call_id: String, event_name: String) -> ActiveTool {
        let name = real_tool_name(&event_name).to_string();
        let display_name = tools::readable_tool_name(&event_name);
        ActiveTool {
            id: call_id,
            command_output: (name == "run_command")
                .then(|| crate::render::CommandOutputTail::new(self.command_output_lines)),
            name,
            display_name,
        }
    }

    pub(crate) fn handle(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::TurnStarted { turn_id } => {
                self.turn_id = Some(turn_id.clone());
                if let Some(run) = self
                    .manager
                    .lock()
                    .unwrap()
                    .active_runs
                    .get_mut(&self.run_id)
                {
                    run.turn_id = Some(turn_id.clone());
                    run.queue_target = Some(self.state_store.queue_target(turn_id.clone()));
                }
                self.publish(
                    "turn.started",
                    json!({
                        "run_id": self.run_id,
                        "turn_id": turn_id,
                        "operation": self.operation,
                        "input_id": self.redo_input_id,
                        "display_content": self.redo_display_content,
                    }),
                );
            }
            AgentEvent::RawReasoning(_) => {}
            AgentEvent::FlushJournal => {}
            AgentEvent::Chunk(chunk) => match chunk.kind {
                ChatStreamKind::Content => self.publish(
                    "assistant.delta",
                    json!({ "run_id": self.run_id, "delta": chunk.text }),
                ),
                ChatStreamKind::Reasoning => self.publish(
                    "reasoning.delta",
                    json!({ "run_id": self.run_id, "delta": chunk.text }),
                ),
                _ => {}
            },
            AgentEvent::ReasoningStart { .. } => {
                self.publish("reasoning.start", json!({ "run_id": self.run_id }))
            }
            AgentEvent::ReasoningReset { .. } => {
                self.publish("reasoning.reset", json!({ "run_id": self.run_id }))
            }
            AgentEvent::ReasoningPartStart { .. } => {
                self.publish("reasoning.part_start", json!({ "run_id": self.run_id }))
            }
            AgentEvent::ReasoningPartEnd { .. } => {
                self.publish("reasoning.part_end", json!({ "run_id": self.run_id }))
            }
            AgentEvent::ReasoningTitle(title) => self.publish(
                "reasoning.title",
                json!({ "run_id": self.run_id, "title": title }),
            ),
            AgentEvent::ToolCall {
                call_id,
                name,
                arguments,
            } => {
                if let Some(queue_ingress) = self.queue_ingress.as_ref() {
                    queue_ingress.tool_started(&call_id);
                }
                let tool = self.next_tool(call_id, name);
                self.publish(
                    "tool.started",
                    json!({
                        "run_id": self.run_id,
                        "tool_id": tool.id,
                        "name": tool.name,
                        "display_name": tool.display_name,
                        "arguments": arguments,
                    }),
                );
                self.active_tools.push(tool);
            }
            // `name` is the raw tool name, matching `tool.started` — it used to
            // be the readable one here alone, which is an easy way to wire a
            // consumer to the wrong field. `tool_name` stays as an alias for
            // browsers still running a cached asset.
            AgentEvent::ToolPreparing { name } => self.publish(
                "tool.preparing",
                json!({
                    "run_id": self.run_id,
                    "name": &name,
                    "tool_name": &name,
                    "display_name": tools::readable_tool_name(&name),
                    // Sent so the WebUI label tracks the backend list instead
                    // of keeping its own copy in sync.
                    "phase": tools::preparing_phase(&name),
                }),
            ),
            AgentEvent::ToolProgress {
                call_id,
                name,
                message,
            } => {
                let (tool_id, tool_name) = self.tool_identity(&call_id, &name);
                self.publish(
                    "tool.progress",
                    json!({
                        "run_id": self.run_id,
                        "tool_id": tool_id,
                        "name": tool_name,
                        "message": message,
                    }),
                );
            }
            AgentEvent::CommandOutput {
                call_id,
                name,
                stream,
                chunk,
            } => {
                let stream_name = match stream {
                    CommandOutputStream::Stdout => "stdout",
                    CommandOutputStream::Stderr => "stderr",
                };
                let (tool_id, tool_name, preview) = if let Some(tool) =
                    self.active_tools.iter_mut().find(|tool| tool.id == call_id)
                {
                    let preview = tool.command_output.as_mut().map(|output| {
                        output.push(stream, &chunk);
                        output.preview()
                    });
                    (tool.id.clone(), tool.name.clone(), preview)
                } else {
                    (call_id.clone(), real_tool_name(&name).to_string(), None)
                };
                self.publish(
                    "tool.output",
                    json!({
                        "run_id": self.run_id,
                        "tool_id": tool_id,
                        "name": tool_name,
                        "stream": stream_name,
                        "output": String::from_utf8_lossy(&chunk),
                        "preview": preview,
                    }),
                );
            }
            AgentEvent::ToolResult {
                call_id,
                name,
                ok,
                output,
            } => {
                if let Some(queue_ingress) = self.queue_ingress.as_ref() {
                    queue_ingress.tool_finished(&call_id);
                }
                let mut tool = self
                    .active_tools
                    .iter()
                    .position(|tool| tool.id == call_id)
                    .map(|index| self.active_tools.remove(index))
                    .unwrap_or_else(|| self.next_tool(call_id, name));
                let preview = tool.command_output.as_mut().map(|output| {
                    output.finalize();
                    output.preview()
                });
                self.publish(
                    "tool.finished",
                    json!({
                        "run_id": self.run_id,
                        "tool_id": tool.id,
                        "name": tool.name,
                        "display_name": tool.display_name,
                        "ok": ok,
                        "output": output,
                        "preview": preview,
                    }),
                );
            }
            AgentEvent::PrepareForExternalOutput { ready } => {
                let _ = ready.send(false);
            }
            AgentEvent::Image {
                call_id,
                name,
                path,
                alt,
            } => {
                let (tool_id, tool_name) = self.tool_identity(&call_id, &name);
                let hide_caption = tool_name == "show_meme";
                let Some(turn_id) = self.turn_id.as_deref() else {
                    self.publish(
                        "tool.image",
                        json!({
                            "run_id": self.run_id,
                            "tool_id": tool_id,
                            "name": tool_name,
                            "error": "image could not be associated with the current turn",
                        }),
                    );
                    return;
                };
                match self
                    .state_store
                    .save_image_asset(turn_id, Some(&tool_id), &path, &alt)
                {
                    Ok(asset) => self.publish(
                        "tool.image",
                        json!({
                            "run_id": self.run_id,
                            "tool_id": tool_id,
                            "name": tool_name,
                            "asset": SafeImageAsset::from_asset(asset, hide_caption),
                        }),
                    ),
                    Err(error) => {
                        tracing::warn!(
                            run_id = %self.run_id,
                            tool = %tool_name,
                            error = %error,
                            "{}",
                            t("failed to persist a WebUI image", "WebUI 图像保存失败")
                        );
                        self.publish(
                            "tool.image",
                            json!({
                                "run_id": self.run_id,
                                "tool_id": tool_id,
                                "name": tool_name,
                                "error": "image could not be added to the WebUI",
                            }),
                        );
                    }
                }
            }
            AgentEvent::Artifact {
                call_id,
                name,
                path,
                title,
            } => {
                let (tool_id, tool_name) = self.tool_identity(&call_id, &name);
                let Some(turn_id) = self.turn_id.as_deref() else {
                    self.publish(
                        "tool.artifact",
                        json!({
                            "run_id": self.run_id,
                            "tool_id": tool_id,
                            "name": tool_name,
                            "error": "artifact could not be associated with the current turn",
                        }),
                    );
                    return;
                };
                match self
                    .state_store
                    .save_artifact_asset(turn_id, Some(&tool_id), &path, &title)
                {
                    Ok(asset) => self.publish(
                        "tool.artifact",
                        json!({
                            "run_id": self.run_id,
                            "tool_id": tool_id,
                            "name": tool_name,
                            "artifact": SafeArtifactAsset::from(asset),
                        }),
                    ),
                    Err(error) => {
                        tracing::warn!(run_id = %self.run_id, tool = %tool_name, error = %error, "failed to persist a WebUI artifact");
                        self.publish(
                            "tool.artifact",
                            json!({
                                "run_id": self.run_id,
                                "tool_id": tool_id,
                                "name": tool_name,
                                "error": "file could not be added to the WebUI preview",
                            }),
                        );
                    }
                }
            }
            AgentEvent::AskQuestion {
                call_id,
                request,
                responder,
            } => {
                let question_id = self
                    .questions
                    .insert(&self.run_id, request.clone(), responder);
                let (tool_id, tool_name) = self.tool_identity(&call_id, "ask_question");
                self.publish(
                    "question.requested",
                    json!({
                        "run_id": self.run_id,
                        "question_id": question_id,
                        "tool_id": tool_id,
                        "name": tool_name,
                        "questions": request.questions,
                    }),
                );
            }
            AgentEvent::QueuedPromptsConsumed {
                prompt_ids,
                mode,
                provider_id,
                model,
            } => self.publish(
                "queue.consumed",
                json!({
                    "run_id": self.run_id,
                    "prompt_ids": prompt_ids,
                    "mode": mode_name(mode),
                    "provider_id": provider_id,
                    "model": model,
                }),
            ),
            AgentEvent::GenerationSuperseded { prompt_ids } => self.publish(
                "generation.superseded",
                json!({
                    "run_id": self.run_id,
                    "turn_id": self.turn_id,
                    "prompt_ids": prompt_ids,
                }),
            ),
            AgentEvent::SpinnerTick => {}
            // 逐请求计量快照:round 为刚结束请求的用量(prompt+completion
            // ≈ 当前上下文占用),turn 为回合累计。前端据此在回合中途刷新
            // 计量条,不必等 chat.done。
            AgentEvent::RoundUsage {
                round,
                turn,
                estimated,
            } => self.publish(
                "chat.round_usage",
                json!({
                    "run_id": self.run_id,
                    "turn_id": self.turn_id,
                    "usage": *round,
                    "turn_total": turn.total,
                    "turn_prompt": turn.prompt,
                    "turn_cache_read": turn.cache_read,
                    "estimated": estimated,
                }),
            ),
            AgentEvent::CompactStart => {
                self.publish("context.compact_start", json!({ "run_id": self.run_id }))
            }
            AgentEvent::CompactChunk(chunk) => self.publish(
                "context.compact_delta",
                json!({ "run_id": self.run_id, "delta": chunk.text }),
            ),
            AgentEvent::CompactEnd => {
                self.publish("context.compact_end", json!({ "run_id": self.run_id }))
            }
            AgentEvent::PopStart => {
                self.publish("context.pop_start", json!({ "run_id": self.run_id }))
            }
            AgentEvent::PopEnd => self.publish("context.pop_end", json!({ "run_id": self.run_id })),
            AgentEvent::Notice { text } => self.publish(
                "context.notice",
                json!({ "run_id": self.run_id, "text": text }),
            ),
        }
    }

    pub(crate) fn tool_identity(&self, call_id: &str, fallback: &str) -> (String, String) {
        self.active_tools
            .iter()
            .find(|tool| tool.id == call_id)
            .map(|tool| (tool.id.clone(), tool.name.clone()))
            .unwrap_or_else(|| (call_id.to_string(), real_tool_name(fallback).to_string()))
    }
}

#[derive(Debug)]
pub(crate) struct ApiError {
    pub(crate) status: StatusCode,
    pub(crate) message: String,
}

impl ApiError {
    pub(crate) fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }

    pub(crate) fn internal(error: impl std::fmt::Display) -> Self {
        tracing::error!(error = %error, "{}", t("WebUI request failed", "WebUI 请求失败"));
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "internal server error")
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(json!({ "error": { "message": self.message } })),
        )
            .into_response()
    }
}

#[derive(Default, Deserialize)]
pub(crate) struct EventsQuery {
    #[serde(default)]
    pub(crate) after: u64,
}

#[derive(Deserialize)]
pub(crate) struct AttachmentQuery {
    pub(crate) session_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CreateTurnRequest {
    pub(crate) content: String,
    /// 兼容字段:旧前端仍会带 mode;会话模式创建时定死,daemon 按会话
    /// 记录强制,这个值只解析不采信。缺省即普通。
    #[serde(default)]
    pub(crate) mode: Option<String>,
    #[serde(default)]
    pub(crate) attachment_ids: Vec<String>,
    /// Target session; defaults to the global current session. The turn runs
    /// there without moving the current pointer (per-view WebUI sessions).
    #[serde(default)]
    pub(crate) session_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct QueuePromptRequest {
    pub(crate) content: String,
    pub(crate) run_id: String,
    pub(crate) turn_id: String,
    #[serde(default)]
    pub(crate) attachment_ids: Vec<String>,
    /// Target session; defaults to the global current session.
    #[serde(default)]
    pub(crate) session_id: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TurnUpdateMode {
    Followup,
    Supersede,
}

pub(crate) struct TurnUpdateRequest {
    pub(crate) run_id: String,
    pub(crate) turn_id: String,
    pub(crate) session_id: Option<Arc<str>>,
    pub(crate) audience: PromptAudience,
    pub(crate) content: String,
    pub(crate) display_content: String,
    pub(crate) attachments: Vec<crate::state::QueuedPromptAttachment>,
    pub(crate) uploaded_attachment_ids: Vec<String>,
    pub(crate) mode: TurnUpdateMode,
}

pub(crate) struct TurnUpdateReceipt {
    pub(crate) run_id: String,
    pub(crate) turn_id: String,
    pub(crate) session_id: Arc<str>,
    pub(crate) prompt: QueuedPrompt,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RedoTurnRequest {
    pub(crate) expected_revision: i64,
    pub(crate) input_id: String,
    #[serde(default)]
    pub(crate) content: Option<String>,
    /// 同 CreateTurnRequest.mode:兼容旧前端,只解析不采信。
    #[serde(default)]
    pub(crate) mode: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AnswerQuestionRequest {
    pub(crate) answers: QuestionAnswers,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SetModelsRequest {
    pub(crate) models: Vec<ActiveProviderModelConfig>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ThinkingVariantUpdate {
    pub(crate) provider_id: String,
    pub(crate) model: String,
    pub(crate) selected: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SetThinkingVariantsRequest {
    pub(crate) updates: Vec<ThinkingVariantUpdate>,
}
