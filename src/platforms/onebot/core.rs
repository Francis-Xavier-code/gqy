// core — 自 src/platforms/onebot.rs 拆分。

pub(crate) use super::*;

// OneBot v11 bridge (NapCat / QQ).
//
// NapCat connects to GQY as a reverse-WebSocket client
// (`GET /ws` on the existing web server; `/onebot/v11/ws` remains an
// alias). Inbound `message`
// events run agent turns via the platform-neutral core in the parent
// module; replies go back as `send_private_msg` / `send_group_msg`
// frames on the same socket. Query-style API calls (file URL lookup)
// use an echo-to-oneshot table. Sends are acknowledged before plugin
// success hooks run, so transformations can safely persist delivery state.

use super::{
    run_platform_turn, BotGroupRole, BotSendAvailability, ConversationKind, PlatformInboundEvent,
    PlatformMessagePosition,
};

use crate::config::{OneBotConfig, PlatformConversationKind, PlatformRateLimit};

use crate::i18n::text as t;

use crate::platforms::access_control::{has_dynamic_access, AccessPermission};
use crate::state::StateStore;
use crate::web::{random_id, DaemonState};

use anyhow::{bail, Context, Result};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, State};
use axum::http::{
    header::{AUTHORIZATION, HOST},
    HeaderMap, StatusCode,
};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::net::SocketAddr;

use std::sync::atomic::{AtomicI64, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::{mpsc, oneshot, watch, Semaphore};
use tokio::task::JoinHandle;

pub(crate) const MAX_INBOUND_IMAGE_BYTES: usize = 10 * 1024 * 1024;
pub(crate) const MAX_INBOUND_IMAGE_TOTAL_BYTES: usize = 20 * 1024 * 1024;
pub(crate) const MAX_INBOUND_FILE_BYTES: usize = 50 * 1024 * 1024;
pub(crate) const MAX_INBOUND_IMAGES: usize = 4;
pub(crate) const MAX_INBOUND_FILES: usize = 4;
pub(crate) const MAX_INBOUND_MEDIA_RECORDS: usize = 32;
pub(crate) const MAX_INBOUND_MESSAGE_BYTES: usize = 16 * 1024 * 1024;
pub(crate) const MAX_INBOUND_TEXT_CHARS: usize = 20_000;
pub(crate) const MAX_INBOUND_SEGMENTS: usize = 256;
pub(crate) const MAX_INBOUND_MENTIONS: usize = 32;
pub(crate) const MAX_CQ_FIELDS: usize = 32;
pub(crate) const MAX_ONEBOT_ID_BYTES: usize = 128;
pub(crate) const MAX_INBOUND_FILE_NAME_CHARS: usize = 512;
pub(crate) const MAX_OUTBOUND_IMAGE_BYTES: usize = 20 * 1024 * 1024;
pub(crate) const MAX_OUTBOUND_FILE_BYTES: usize = 50 * 1024 * 1024;
pub(crate) const MAX_BASE64_FILE_BYTES: usize = 16 * 1024 * 1024;
pub(crate) const IMAGE_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(15);
pub(crate) const FILE_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(60);
pub(crate) const API_CALL_TIMEOUT: Duration = Duration::from_secs(10);
/// Backstop for attachment sends (see `send_timeout_for`). Not a budget: it
/// only exists so a connected-but-silent NapCat cannot wedge a conversation.
pub(crate) const MAX_SEND_TIMEOUT: Duration = Duration::from_secs(180);
pub(crate) const QUOTED_MESSAGE_LOOKUP_TIMEOUT: Duration = Duration::from_secs(3);
/// Bounds parsed/in-flight events per NapCat connection. Same-conversation
/// LLM turns are serialized later; this cap only prevents an unbounded task
/// buildup under hostile traffic.
pub(crate) const MAX_IN_FLIGHT_MESSAGES: usize = 32;
pub(crate) static LAST_INGRESS_ORDER: AtomicI64 = AtomicI64::new(0);
pub(crate) const PLATFORM_FILE_STORAGE_BYTES: u64 = 1024 * 1024 * 1024;
pub(crate) const PLATFORM_FILE_STORAGE_ENTRIES: usize = 4096;
pub(crate) const PLATFORM_FILE_TTL: Duration = Duration::from_secs(7 * 24 * 60 * 60);
pub(crate) const GROUP_NAME_CACHE_TTL: Duration = Duration::from_secs(5 * 60);
pub(crate) const GROUP_NAME_CACHE_CAPACITY: usize = 1024;
pub(crate) const MENTION_NAME_CACHE_TTL: Duration = Duration::from_secs(10 * 60);
pub(crate) const MENTION_NAME_CACHE_CAPACITY: usize = 4096;
pub(crate) const MAX_MENTION_NAME_LOOKUPS: usize = 8;
pub(crate) const MENTION_NAME_LOOKUP_TIMEOUT: Duration = Duration::from_secs(3);
pub(crate) const GROUP_MUTE_AVAILABLE_TTL: Duration = Duration::from_secs(30);
pub(crate) const GROUP_MUTE_UNKNOWN_TTL: Duration = Duration::from_secs(10);
pub(crate) const GROUP_MUTE_WHOLE_NOTICE_TTL: Duration = Duration::from_secs(60);
pub(crate) const GROUP_MUTE_MAX_TTL: Duration = Duration::from_secs(31 * 24 * 60 * 60);
pub(crate) const GROUP_MUTE_CACHE_CAPACITY: usize = 1024;
pub(crate) const GROUP_MUTE_LOOKUP_TIMEOUT: Duration = Duration::from_secs(3);
pub(crate) const GROUP_ROLE_CACHE_TTL: Duration = Duration::from_secs(60);
pub(crate) const GROUP_ROLE_CACHE_CAPACITY: usize = 1024;

#[derive(Debug, Clone)]
pub(crate) struct GroupNameCacheEntry {
    name: String,
    expires_at: Instant,
    last_used: Instant,
}

#[derive(Default)]
pub(crate) struct GroupNameCache {
    pub(crate) entries: HashMap<(i64, i64), GroupNameCacheEntry>,
}

impl GroupNameCache {
    pub(crate) fn get(&mut self, key: (i64, i64), now: Instant) -> Option<String> {
        self.prune(now);
        let entry = self.entries.get_mut(&key)?;
        entry.last_used = now;
        Some(entry.name.clone())
    }

    pub(crate) fn insert(&mut self, key: (i64, i64), name: String, now: Instant) {
        self.prune(now);
        if self.entries.len() >= GROUP_NAME_CACHE_CAPACITY && !self.entries.contains_key(&key) {
            if let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(key, _)| *key)
            {
                self.entries.remove(&oldest);
            }
        }
        self.entries.insert(
            key,
            GroupNameCacheEntry {
                name,
                expires_at: now + GROUP_NAME_CACHE_TTL,
                last_used: now,
            },
        );
    }

    pub(crate) fn prune(&mut self, now: Instant) {
        self.entries.retain(|_, entry| entry.expires_at > now);
    }
}

pub(crate) static GROUP_NAME_CACHE: OnceLock<Mutex<GroupNameCache>> = OnceLock::new();

pub(crate) fn group_name_cache() -> &'static Mutex<GroupNameCache> {
    GROUP_NAME_CACHE.get_or_init(|| Mutex::new(GroupNameCache::default()))
}

#[derive(Debug, Clone)]
pub(crate) struct MentionNameCacheEntry {
    name: String,
    expires_at: Instant,
    last_used: Instant,
}

#[derive(Default)]
pub(crate) struct MentionNameCache {
    entries: HashMap<(i64, i64, String), MentionNameCacheEntry>,
}

impl MentionNameCache {
    pub(crate) fn get(&mut self, key: &(i64, i64, String), now: Instant) -> Option<String> {
        self.entries.retain(|_, entry| entry.expires_at > now);
        let entry = self.entries.get_mut(key)?;
        entry.last_used = now;
        Some(entry.name.clone())
    }

    pub(crate) fn insert(&mut self, key: (i64, i64, String), name: String, now: Instant) {
        self.entries.retain(|_, entry| entry.expires_at > now);
        if self.entries.len() >= MENTION_NAME_CACHE_CAPACITY && !self.entries.contains_key(&key) {
            if let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(key, _)| key.clone())
            {
                self.entries.remove(&oldest);
            }
        }
        self.entries.insert(
            key,
            MentionNameCacheEntry {
                name,
                expires_at: now + MENTION_NAME_CACHE_TTL,
                last_used: now,
            },
        );
    }
}

pub(crate) static MENTION_NAME_CACHE: OnceLock<Mutex<MentionNameCache>> = OnceLock::new();

pub(crate) fn mention_name_cache() -> &'static Mutex<MentionNameCache> {
    MENTION_NAME_CACHE.get_or_init(|| Mutex::new(MentionNameCache::default()))
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct GroupRoleCacheEntry {
    role: BotGroupRole,
    expires_at: Instant,
    last_used: Instant,
}

#[derive(Default)]
pub(crate) struct GroupRoleCache {
    entries: HashMap<(i64, i64), GroupRoleCacheEntry>,
}

impl GroupRoleCache {
    pub(crate) fn get(&mut self, key: (i64, i64), now: Instant) -> Option<BotGroupRole> {
        self.entries.retain(|_, entry| entry.expires_at > now);
        let entry = self.entries.get_mut(&key)?;
        entry.last_used = now;
        Some(entry.role)
    }

    pub(crate) fn insert(&mut self, key: (i64, i64), role: BotGroupRole, now: Instant) {
        self.entries.retain(|_, entry| entry.expires_at > now);
        if self.entries.len() >= GROUP_ROLE_CACHE_CAPACITY && !self.entries.contains_key(&key) {
            if let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(key, _)| *key)
            {
                self.entries.remove(&oldest);
            }
        }
        self.entries.insert(
            key,
            GroupRoleCacheEntry {
                role,
                expires_at: now + GROUP_ROLE_CACHE_TTL,
                last_used: now,
            },
        );
    }

    pub(crate) fn remove_account(&mut self, account_id: i64) {
        self.entries.retain(|(id, _), _| *id != account_id);
    }
}

pub(crate) static GROUP_ROLE_CACHE: OnceLock<Mutex<GroupRoleCache>> = OnceLock::new();

pub(crate) fn group_role_cache() -> &'static Mutex<GroupRoleCache> {
    GROUP_ROLE_CACHE.get_or_init(|| Mutex::new(GroupRoleCache::default()))
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct GroupMuteCacheEntry {
    availability: BotSendAvailability,
    expires_at: Instant,
    last_used: Instant,
}

#[derive(Default)]
pub(crate) struct GroupMuteCache {
    entries: HashMap<(i64, i64), GroupMuteCacheEntry>,
}

impl GroupMuteCache {
    pub(crate) fn get(&mut self, key: (i64, i64), now: Instant) -> Option<BotSendAvailability> {
        self.prune(now);
        let entry = self.entries.get_mut(&key)?;
        entry.last_used = now;
        Some(entry.availability)
    }

    pub(crate) fn insert(
        &mut self,
        key: (i64, i64),
        availability: BotSendAvailability,
        ttl: Duration,
        now: Instant,
    ) {
        self.prune(now);
        if self.entries.len() >= GROUP_MUTE_CACHE_CAPACITY && !self.entries.contains_key(&key) {
            if let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(key, _)| *key)
            {
                self.entries.remove(&oldest);
            }
        }
        self.entries.insert(
            key,
            GroupMuteCacheEntry {
                availability,
                expires_at: now + ttl.min(GROUP_MUTE_MAX_TTL),
                last_used: now,
            },
        );
    }

    pub(crate) fn remove_account(&mut self, self_id: i64) {
        self.entries
            .retain(|(account_id, _), _| *account_id != self_id);
    }

    pub(crate) fn prune(&mut self, now: Instant) {
        self.entries.retain(|_, entry| entry.expires_at > now);
    }
}

pub(crate) static GROUP_MUTE_CACHE: OnceLock<Mutex<GroupMuteCache>> = OnceLock::new();

pub(crate) fn group_mute_cache() -> &'static Mutex<GroupMuteCache> {
    GROUP_MUTE_CACHE.get_or_init(|| Mutex::new(GroupMuteCache::default()))
}

pub(crate) fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or_default()
}

pub(crate) fn next_ingress_order() -> i64 {
    let wall_clock = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos().min(i64::MAX as u128) as i64)
        .unwrap_or_default();
    let mut previous = LAST_INGRESS_ORDER.load(AtomicOrdering::Relaxed);
    loop {
        let next = wall_clock.max(previous.saturating_add(1));
        match LAST_INGRESS_ORDER.compare_exchange_weak(
            previous,
            next,
            AtomicOrdering::AcqRel,
            AtomicOrdering::Relaxed,
        ) {
            Ok(_) => return next,
            Err(current) => previous = current,
        }
    }
}

// ---------------------------------------------------------------------------
// Connection registry
// ---------------------------------------------------------------------------

/// Live NapCat connections keyed by bot QQ id. NapCat reconnects on its
/// own schedule, which can leave a half-open predecessor; each new
/// connection bumps the generation and the old read loop notices it has
/// been replaced and exits, so replies are never duplicated.
#[derive(Default)]
pub(crate) struct ConnectionRegistry {
    next_generation: u64,
    connections: HashMap<i64, RegisteredConnection>,
}

pub(crate) struct RegisteredConnection {
    generation: u64,
    handle: ConnectionHandle,
}

impl ConnectionRegistry {
    pub(crate) fn register(&mut self, self_id: i64, handle: ConnectionHandle) -> u64 {
        self.next_generation += 1;
        let generation = self.next_generation;
        if self_id != 0 {
            self.connections
                .insert(self_id, RegisteredConnection { generation, handle });
        }
        generation
    }

    pub(crate) fn bind(&mut self, self_id: i64, generation: u64, handle: ConnectionHandle) -> bool {
        if self_id == 0
            || self
                .connections
                .get(&self_id)
                .is_some_and(|connection| connection.generation > generation)
        {
            return false;
        }
        self.connections
            .insert(self_id, RegisteredConnection { generation, handle });
        true
    }

    pub(crate) fn is_current(&self, self_id: i64, generation: u64) -> bool {
        self.connections
            .get(&self_id)
            .is_some_and(|connection| connection.generation == generation)
    }

    pub(crate) fn remove(&mut self, self_id: i64, generation: u64) -> bool {
        if self.is_current(self_id, generation) {
            self.connections.remove(&self_id);
            true
        } else {
            false
        }
    }

    pub(crate) fn handle(&self, self_id: i64) -> Option<ConnectionHandle> {
        self.connections
            .get(&self_id)
            .map(|connection| connection.handle.clone())
    }

    pub(crate) fn connected_accounts(&self) -> Vec<i64> {
        let mut accounts = self.connections.keys().copied().collect::<Vec<_>>();
        accounts.sort_unstable();
        accounts
    }

    pub(crate) fn disconnect_all(&mut self) {
        for connection in self.connections.values() {
            let _ = connection.handle.shutdown.send(true);
        }
        self.connections.clear();
    }
}

/// Cheap-to-clone sender half of one connection: outbound frames plus
/// the echo table for request/response API calls.
#[derive(Clone)]
pub(crate) struct ConnectionHandle {
    pub(crate) out_tx: mpsc::UnboundedSender<String>,
    pub(crate) pending: Arc<Mutex<HashMap<String, oneshot::Sender<Value>>>>,
    pub(crate) bot_name: Arc<Mutex<Option<String>>>,
    pub(crate) asset_base_url: Option<String>,
    pub(crate) assets: crate::platforms::assets::AssetLeaseStore,
    pub(crate) shutdown: watch::Sender<bool>,
}

impl ConnectionHandle {
    pub(crate) fn send_frame(&self, frame: String) -> Result<()> {
        self.out_tx
            .send(frame)
            .map_err(|_| anyhow::anyhow!("OneBot connection writer is closed"))
    }

    /// Sends an `{action, params, echo}` frame and waits for the frame
    /// that echoes it back.
    pub(crate) async fn call_api(&self, action: &str, params: Value) -> Result<Value> {
        self.call_api_with_timeout(action, params, API_CALL_TIMEOUT)
            .await
    }

    pub(crate) async fn call_api_with_timeout(
        &self,
        action: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value> {
        let echo = random_id("act", 12);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(echo.clone(), tx);
        if let Err(error) = self.send_frame(api_frame(action, params, &echo)) {
            self.pending.lock().unwrap().remove(&echo);
            return Err(error);
        }
        let result = tokio::time::timeout(timeout, rx).await;
        self.pending.lock().unwrap().remove(&echo);
        let Ok(Ok(response)) = result else {
            bail!("OneBot API {action} timed out");
        };
        let retcode = response.get("retcode").and_then(value_i64).unwrap_or(-1);
        if retcode != 0 {
            let status = response
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let detail = ["wording", "message", "msg"]
                .into_iter()
                .filter_map(|key| response.get(key).and_then(Value::as_str))
                .map(str::trim)
                .find(|value| !value.is_empty())
                .unwrap_or("no error detail returned");
            let detail = sanitize_api_detail(detail);
            bail!(
                "OneBot API {action} failed: status={status}, retcode={retcode}, detail={detail}"
            );
        }
        Ok(response.get("data").cloned().unwrap_or(Value::Null))
    }
}

/// Bridges sometimes splice raw protocol bytes into their error strings — a
/// failed kick comes back with the target's protobuf-encoded UID embedded.
/// Those bytes are unreadable, unhelpful, and go straight into the model's
/// context, so strip the unprintables and cap the length.
pub(crate) fn sanitize_api_detail(detail: &str) -> String {
    pub(crate) const MAX_DETAIL_CHARS: usize = 200;
    let mut cleaned = String::with_capacity(detail.len());
    let mut last_was_space = false;
    for ch in detail.chars() {
        let printable = !ch.is_control() && ch != '\u{fffd}';
        if printable {
            cleaned.push(ch);
            last_was_space = ch == ' ';
        } else if !last_was_space && !cleaned.is_empty() {
            cleaned.push(' ');
            last_was_space = true;
        }
    }
    let cleaned = cleaned.trim();
    if cleaned.chars().count() > MAX_DETAIL_CHARS {
        let kept: String = cleaned.chars().take(MAX_DETAIL_CHARS).collect();
        return format!("{kept}…");
    }
    cleaned.to_string()
}

#[derive(Clone, Default)]
pub(crate) struct QqListenerManager {
    pub(crate) inner: Arc<Mutex<QqListenerState>>,
}

#[derive(Default)]
pub(crate) struct QqListenerState {
    pub(crate) active_port: Option<u16>,
    pub(crate) task: Option<JoinHandle<()>>,
}

pub(crate) struct PreparedQqListener {
    manager: QqListenerManager,
    state: DaemonState,
    desired_port: Option<u16>,
    listener: Option<tokio::net::TcpListener>,
    disconnect_connections: bool,
}

impl QqListenerManager {
    pub(crate) fn active_port(&self) -> Option<u16> {
        self.inner.lock().unwrap().active_port
    }

    pub(crate) async fn prepare(
        &self,
        state: &DaemonState,
        current: Option<&OneBotConfig>,
        next: &OneBotConfig,
    ) -> Result<PreparedQqListener> {
        // The default QQ port is the daemon's WebUI port. If WebUI had to
        // fall back from 8300 because it was occupied, keep the short `/ws`
        // endpoint and the QQ listener on that same effective port. A
        // non-default configured port remains a dedicated listener.
        let desired_port = effective_reverse_ws_port(state, next);
        let active_port = self.inner.lock().unwrap().active_port;
        let needs_dedicated_bind =
            desired_port.is_some_and(|port| port != state.web_port && Some(port) != active_port);
        let listener = if needs_dedicated_bind {
            let port = desired_port.expect("dedicated bind requires a port");
            Some(
                tokio::net::TcpListener::bind(SocketAddr::from(([0, 0, 0, 0], port)))
                    .await
                    .with_context(|| {
                        format!("binding Tencent QQ reverse WebSocket to 0.0.0.0:{port}")
                    })?,
            )
        } else {
            None
        };
        let disconnect_connections = current.is_some_and(|current| {
            effective_reverse_ws_port(state, current) != desired_port
                || current.access_token != next.access_token
        });
        Ok(PreparedQqListener {
            manager: self.clone(),
            state: state.clone(),
            desired_port,
            listener,
            disconnect_connections,
        })
    }

    pub(crate) async fn shutdown(&self, state: &DaemonState) {
        let task = {
            let mut inner = self.inner.lock().unwrap();
            inner.active_port = None;
            inner.task.take()
        };
        if let Some(task) = task {
            task.abort();
            let _ = task.await;
        }
        state.platforms.onebot.lock().unwrap().disconnect_all();
    }
}

pub(crate) fn effective_reverse_ws_port(state: &DaemonState, config: &OneBotConfig) -> Option<u16> {
    if !config.enabled {
        return None;
    }
    if config.reverse_ws_port == crate::ipc::DEFAULT_WEB_PORT
        && state.web_port != crate::ipc::DEFAULT_WEB_PORT
    {
        Some(state.web_port)
    } else {
        Some(config.reverse_ws_port)
    }
}

impl PreparedQqListener {
    pub(crate) fn commit(mut self) {
        let previous_port = self.manager.active_port();
        let previous_task = {
            let mut inner = self.manager.inner.lock().unwrap();
            if inner.active_port == self.desired_port {
                None
            } else {
                let previous = inner.task.take();
                inner.active_port = self.desired_port;
                inner.task = self.listener.take().map(|listener| {
                    let app = qq_listener_router(self.state.clone());
                    tokio::spawn(async move {
                        if let Err(error) = axum::serve(
                            listener,
                            app.into_make_service_with_connect_info::<SocketAddr>(),
                        )
                        .await
                        {
                            tracing::error!(target: "gqy::qq", error = %error, "{}", t("Tencent QQ listener stopped", "腾讯 QQ 监听器已停止"));
                        }
                    })
                });
                previous
            }
        };
        if let Some(task) = previous_task {
            task.abort();
        }
        if self.disconnect_connections {
            self.state.platforms.onebot.lock().unwrap().disconnect_all();
        }
        if previous_port != self.desired_port {
            match self.desired_port {
                Some(port) => {
                    tracing::info!(target: "gqy::qq", port, path = "/ws", "{}", t("Tencent QQ listener ready", "腾讯 QQ 监听器已就绪"))
                }
                None => {
                    tracing::info!(target: "gqy::qq", "{}", t("Tencent QQ listener disabled", "腾讯 QQ 监听器已禁用"))
                }
            }
        }
    }
}

pub(crate) fn qq_listener_router(state: DaemonState) -> Router {
    Router::new()
        .route("/ws", get(onebot_ws))
        .route("/onebot/v11/ws", get(onebot_ws))
        .route(
            "/api/platform-assets/{token}",
            get(crate::platforms::assets::platform_asset),
        )
        .with_state(state)
}

pub(crate) fn api_frame(action: &str, params: Value, echo: &str) -> String {
    json!({ "action": action, "params": params, "echo": echo }).to_string()
}

// ---------------------------------------------------------------------------
// WebSocket endpoint
// ---------------------------------------------------------------------------

/// 合成唤醒事件的 user_id:优先 spawn 回合记录的真实发起者;私聊退回会话
/// 对端(该私聊唯一的人类);群聊无记录时保持机器人自身——不凭空授予权限,
/// 只是回到修复前的降级行为。
pub(crate) fn wake_sender_user_id(initiator: Option<&str>, target: Target, self_id: i64) -> i64 {
    initiator
        .and_then(|id| id.trim().parse().ok())
        .or(match target {
            Target::Private { user_id } => Some(user_id),
            Target::Group { .. } => None,
        })
        .unwrap_or(self_id)
}

/// Background-job completion wake: a self-initiated model turn in a bound
/// QQ conversation. There is no inbound event — reply targeting, affection
/// and trigger judging all no-op — the sender display name stays "系统",
/// so the model reads the job result and reports it into the conversation
/// in its own voice.
pub(crate) async fn wake_conversation_for_job(
    state: &DaemonState,
    account_id: &str,
    conversation_kind: &str,
    conversation_id: &str,
    initiator: Option<&str>,
    content: String,
) -> Result<()> {
    let self_id: i64 = account_id
        .parse()
        .context("invalid QQ account id for a job wake")?;
    let conn = state
        .platforms
        .onebot
        .lock()
        .unwrap()
        .handle(self_id)
        .context("the QQ account is not connected")?;
    let target_id: i64 = conversation_id
        .parse()
        .context("invalid QQ conversation id for a job wake")?;
    let target = match conversation_kind {
        "group" => Target::Group {
            group_id: target_id,
        },
        "private" => Target::Private { user_id: target_id },
        other => bail!("unsupported QQ conversation kind: {other}"),
    };
    let config = state.manager.lock().unwrap().config.clone();
    // issue #29:合成事件的 user_id 决定 is_admin → host_tools_allowed →
    // 工具表选择。必须继承真实发起者的身份,伪装成机器人自己会把跟进 turn
    // 降级成受限工具集,job_status 都不存在。
    let sender_user_id = wake_sender_user_id(initiator, target, self_id);
    let event = json!({
        "self_id": self_id,
        "user_id": sender_user_id,
        "sender": { "nickname": "系统" },
    });
    let context = Arc::new(platform_turn_context(
        state, conn, target, &event, config, None,
    )?);
    let session_id = resolve_onebot_session(state, &context, target, &event)?;
    let conversation_kind_enum = match target {
        Target::Private { .. } => PlatformConversationKind::Private,
        Target::Group { .. } => PlatformConversationKind::Group,
    };
    // Run the normal turn preparation so plugins inject group history and
    // context blocks — the wake turn should see the conversation exactly
    // like an inbound turn would.
    let prepared = context.prepare_turn(content).await;
    let mut turn_system_context = vec![
        "本轮由系统自动触发：一个后台任务刚刚结束，报告与结果就在本轮消息里。\
         这不是任何群成员或用户发来的消息；以你自己的身份把结果自然地发到会话里。"
            .to_string(),
    ];
    turn_system_context.extend(prepared.turn_system_context);
    let profile = super::TurnProfile {
        active_persona: Some(context.config.prompt.active_persona.clone()),
        text_models: context.config.active_provider_models.clone(),
        multimodal_models: context
            .config
            .qq_multimodal_model_pool(
                conversation_kind_enum,
                &context.conversation.conversation_id,
            )
            .map(<[_]>::to_vec),
        system_context: prepared.system_context,
        turn_system_context,
        memory_content: Some(prepared.memory_content),
        context_images: prepared.context_images,
        image_cache_namespace: Some("qq".to_string()),
        image_source_label: Some("QQ".to_string()),
        memory_write_enabled: context.config.platforms.qq.memory.write_enabled,
        // Groups keep their own turn history now. The structured log still
        // carries who said what — the protocol offers no third role and drops
        // `name`, so identity can only live in the text — but the log is
        // additive: each turn appends what arrived since the last one, and
        // earlier turns replay verbatim. GQY's own turns become real
        // assistant messages instead of one `[你]` line in a rolling window.
        suppress_session_history: false,
        group_context: (context.conversation.kind == ConversationKind::Group)
            .then(|| context.config.platforms.qq.group_context.clone()),
        platform: Some(context.clone()),
        followup: None,
    };
    let dispatch =
        run_platform_turn(state, session_id, prepared.content, Vec::new(), profile).await?;
    deliver_dispatch(state, &context, dispatch).await?;
    Ok(())
}

pub(crate) async fn onebot_ws(
    State(state): State<DaemonState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    let config = onebot_config(&state);
    if !config.enabled {
        return StatusCode::NOT_FOUND.into_response();
    }
    if !connection_authorized(&headers, &config.access_token, peer) {
        if config.access_token.trim().is_empty() {
            tracing::warn!(target: "gqy::qq", %peer, reason = "non_loopback_without_token", "{}", t("OneBot client rejected", "OneBot 客户端已拒绝"));
        } else {
            tracing::warn!(target: "gqy::qq", %peer, reason = "bad_token", "{}", t("OneBot client rejected", "OneBot 客户端已拒绝"));
        }
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let self_id = headers
        .get("x-self-id")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<i64>().ok())
        .unwrap_or(0);
    let asset_base_url = resolve_asset_base_url(&headers, &config);
    ws.max_message_size(MAX_INBOUND_MESSAGE_BYTES)
        .max_frame_size(MAX_INBOUND_MESSAGE_BYTES)
        .on_upgrade(move |socket| connection_loop(state, socket, self_id, asset_base_url))
}

pub(crate) async fn onebot_ws_on_web_port(
    State(state): State<DaemonState>,
    peer: ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    if state.platforms.qq_listener.active_port() != Some(state.web_port) {
        return StatusCode::NOT_FOUND.into_response();
    }
    onebot_ws(State(state), peer, headers, ws).await
}

pub(crate) fn connection_authorized(headers: &HeaderMap, expected: &str, peer: SocketAddr) -> bool {
    let expected = expected.trim();
    if expected.is_empty() {
        peer.ip().is_loopback()
    } else {
        token_matches(headers, expected)
    }
}

pub(crate) fn resolve_asset_base_url(headers: &HeaderMap, config: &OneBotConfig) -> Option<String> {
    let configured = config.asset_base_url.trim().trim_end_matches('/');
    if configured.starts_with("http://") || configured.starts_with("https://") {
        return Some(configured.to_string());
    }
    headers
        .get(HOST)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|host| {
            !host.is_empty()
                && host
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b".-:[]".contains(&byte))
        })
        .map(|host| format!("http://{host}"))
}

pub(crate) fn onebot_config(state: &DaemonState) -> OneBotConfig {
    state.manager.lock().unwrap().config.platforms.qq.clone()
}

/// Compares digests rather than raw strings so length/prefix timing
/// leaks nothing. An empty configured token disables the check.
pub(crate) fn token_matches(headers: &HeaderMap, expected: &str) -> bool {
    let expected = expected.trim();
    if expected.is_empty() {
        return true;
    }
    let supplied = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .and_then(|value| {
            value
                .strip_prefix("Bearer ")
                .or_else(|| value.strip_prefix("Token "))
                .or(Some(value))
        })
        .map(str::trim);
    let Some(supplied) = supplied else {
        return false;
    };
    Sha256::digest(supplied.as_bytes()) == Sha256::digest(expected.as_bytes())
}

pub(crate) async fn connection_loop(
    state: DaemonState,
    socket: WebSocket,
    self_id: i64,
    asset_base_url: Option<String>,
) {
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();
    let (shutdown, mut shutdown_rx) = watch::channel(false);
    let handle = ConnectionHandle {
        out_tx,
        pending: Arc::new(Mutex::new(HashMap::new())),
        bot_name: Arc::new(Mutex::new(None)),
        asset_base_url,
        assets: state.platforms.assets.clone(),
        shutdown,
    };
    let generation = state
        .platforms
        .onebot
        .lock()
        .unwrap()
        .register(self_id, handle.clone());
    tracing::info!(target: "gqy::qq", self_id, generation, "{}", t("OneBot client connected", "OneBot 客户端已连接"));

    let (mut sink, mut stream) = socket.split();
    let writer = tokio::spawn(async move {
        while let Some(frame) = out_rx.recv().await {
            if sink.send(Message::Text(frame.into())).await.is_err() {
                break;
            }
        }
    });
    let permits = Arc::new(Semaphore::new(MAX_IN_FLIGHT_MESSAGES));
    let mut bound_self_id = self_id;

    loop {
        let message = tokio::select! {
            changed = shutdown_rx.changed() => {
                if changed.is_err() || *shutdown_rx.borrow() {
                    break;
                }
                continue;
            }
            message = stream.next() => {
                let Some(message) = message else { break; };
                message
            }
        };
        let message = match message {
            Ok(message) => message,
            Err(_) => break,
        };
        if bound_self_id != 0
            && !state
                .platforms
                .onebot
                .lock()
                .unwrap()
                .is_current(bound_self_id, generation)
        {
            tracing::info!(target: "gqy::qq",
                self_id,
                generation,
                "{}",
                t("OneBot connection replaced by a newer one", "OneBot 连接已被新连接替换")
            );
            break;
        }
        let text = match message {
            Message::Text(text) => text,
            Message::Close(_) => break,
            _ => continue,
        };
        let Ok(frame) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        if let Some(event_self_id) = frame
            .get("self_id")
            .and_then(Value::as_i64)
            .filter(|id| *id != 0)
        {
            if bound_self_id == 0 {
                bound_self_id = event_self_id;
                let bound = state.platforms.onebot.lock().unwrap().bind(
                    bound_self_id,
                    generation,
                    handle.clone(),
                );
                if !bound {
                    tracing::info!(target: "gqy::qq",
                    self_id = bound_self_id,
                    generation,
                    "{}",
                    t("OneBot connection identity is already owned by a newer connection", "OneBot 连接身份已被新连接占用")
                    );
                    break;
                }
                group_mute_cache()
                    .lock()
                    .unwrap()
                    .remove_account(bound_self_id);
                group_role_cache()
                    .lock()
                    .unwrap()
                    .remove_account(bound_self_id);
                tracing::info!(target: "gqy::qq",
                    self_id = bound_self_id,
                    generation,
                    "{}",
                    t("OneBot connection identity bound from event", "已从事件绑定 OneBot 连接身份")
                );
            } else if bound_self_id != event_self_id {
                tracing::warn!(target: "gqy::qq",
                    expected = bound_self_id,
                    received = event_self_id,
                    "{}",
                    t("OneBot connection changed self_id", "OneBot 连接更改了 self_id")
                );
                break;
            }
        }
        if frame.get("post_type").is_none() {
            route_api_response(&handle, frame);
            continue;
        }
        if frame.get("post_type").and_then(Value::as_str) == Some("message") {
            let ingress_order = next_ingress_order();
            let activity = observe_message_activity(&state, &frame, bound_self_id, Instant::now());
            let config = state.manager.lock().unwrap().config.clone();
            if config.platforms.qq.enabled {
                if let Some(inbound) =
                    ingress_message_event(&frame, bound_self_id, ingress_order, activity.as_ref())
                {
                    match state.platforms.plugins() {
                        Ok(plugins) => {
                            plugins
                                .observe_ingress(&state.paths, &config, &inbound)
                                .await;
                        }
                        Err(error) => tracing::warn!(
                            target: "gqy::qq",
                            error = %error,
                            "{}",
                            t(
                                "OneBot message history initialization failed",
                                "OneBot 消息历史初始化失败"
                            )
                        ),
                    }
                }
            }
            let connection_permit = match permits.clone().try_acquire_owned() {
                Ok(permit) => permit,
                Err(_) => {
                    tracing::warn!(target: "gqy::qq",
                        self_id = bound_self_id,
                        "{}",
                        t("OneBot connection event queue is full; dropping a message", "OneBot 连接事件队列已满，丢弃消息")
                    );
                    continue;
                }
            };
            let state = state.clone();
            let handle = handle.clone();
            tokio::spawn(async move {
                let _connection_permit = connection_permit;
                handle_message_with_activity(state, handle, frame, ingress_order, activity).await;
            });
        } else if is_message_recall(&frame) {
            let connection_permit = match permits.clone().try_acquire_owned() {
                Ok(permit) => permit,
                Err(_) => {
                    tracing::warn!(target: "gqy::qq",
                        self_id = bound_self_id,
                        "{}",
                        t("OneBot connection concurrency is full; dropping a recall notice", "OneBot 连接并发已满，丢弃撤回通知")
                    );
                    continue;
                }
            };
            let state = state.clone();
            let handle = handle.clone();
            tokio::spawn(async move {
                let _connection_permit = connection_permit;
                handle_message_recall(state, handle, frame).await;
            });
        } else if is_friend_add_request(&frame) {
            let state = state.clone();
            let handle = handle.clone();
            tokio::spawn(async move {
                handle_friend_add_request(state, handle, frame).await;
            });
        } else if is_group_ban_notice(&frame) {
            update_group_ban_notice(&frame);
            let state = state.clone();
            let handle = handle.clone();
            tokio::spawn(async move {
                handle_group_management_notice(state, handle, frame).await;
            });
        } else if is_group_decrease_notice(&frame) {
            let state = state.clone();
            let handle = handle.clone();
            tokio::spawn(async move {
                handle_group_management_notice(state, handle, frame).await;
            });
        }
    }

    let removed = state
        .platforms
        .onebot
        .lock()
        .unwrap()
        .remove(bound_self_id, generation);
    if removed {
        group_mute_cache()
            .lock()
            .unwrap()
            .remove_account(bound_self_id);
        group_role_cache()
            .lock()
            .unwrap()
            .remove_account(bound_self_id);
    }
    writer.abort();
    tracing::info!(target: "gqy::qq",
        self_id = bound_self_id,
        generation,
        "{}",
        t("OneBot client disconnected", "OneBot 客户端已断开")
    );
}

/// Routes an API response frame to its waiting `call_api`; unmatched
/// response failures still get a diagnostic.
pub(crate) fn route_api_response(handle: &ConnectionHandle, frame: Value) {
    let echo = frame
        .get("echo")
        .and_then(Value::as_str)
        .map(str::to_string);
    if let Some(echo) = echo {
        if let Some(waiter) = handle.pending.lock().unwrap().remove(&echo) {
            let _ = waiter.send(frame);
            return;
        }
    }
    let retcode = frame.get("retcode").and_then(Value::as_i64).unwrap_or(0);
    if retcode != 0 {
        tracing::warn!(retcode, "{}", t("OneBot send failed", "OneBot 发送失败"));
    }
}

// ---------------------------------------------------------------------------
// Inbound message pipeline
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
pub(crate) enum Target {
    Private { user_id: i64 },
    Group { group_id: i64 },
}

impl Target {
    pub(crate) fn kind(self) -> &'static str {
        match self {
            Self::Private { .. } => "private",
            Self::Group { .. } => "group",
        }
    }

    pub(crate) fn conversation_id(self) -> i64 {
        match self {
            Self::Private { user_id } => user_id,
            Self::Group { group_id } => group_id,
        }
    }
}

#[derive(Clone)]
pub(crate) struct InboundMessageActivity {
    pub(crate) handle: crate::platforms::MessageActivityHandle,
    pub(crate) position: PlatformMessagePosition,
    pub(crate) received_at: Instant,
}

pub(crate) fn observe_message_activity(
    state: &DaemonState,
    event: &Value,
    fallback_self_id: i64,
    received_at: Instant,
) -> Option<InboundMessageActivity> {
    let self_id = event
        .get("self_id")
        .and_then(Value::as_i64)
        .filter(|id| *id != 0)
        .unwrap_or(fallback_self_id);
    let user_id = event.get("user_id").and_then(Value::as_i64)?;
    if self_id == 0 || user_id == 0 || user_id == self_id {
        return None;
    }
    let target = match event.get("message_type").and_then(Value::as_str) {
        Some("private") => Target::Private { user_id },
        Some("group") => Target::Group {
            group_id: event
                .get("group_id")
                .and_then(Value::as_i64)
                .filter(|group_id| *group_id != 0)?,
        },
        _ => return None,
    };
    let conversation = platform_conversation(target, self_id);
    let message_id = event
        .get("message_id")
        .and_then(value_id_string)
        .unwrap_or_default();
    let sender_id = user_id.to_string();
    let (handle, position, received_at) = state.platforms.message_activity.observe(
        &conversation.scope_key(),
        &message_id,
        &sender_id,
        received_at,
    );
    Some(InboundMessageActivity {
        handle,
        position,
        received_at,
    })
}

pub(crate) fn ingress_message_event(
    event: &Value,
    fallback_self_id: i64,
    ingress_order: i64,
    activity: Option<&InboundMessageActivity>,
) -> Option<PlatformInboundEvent> {
    let self_id = event
        .get("self_id")
        .and_then(Value::as_i64)
        .filter(|id| *id != 0)
        .unwrap_or(fallback_self_id);
    let user_id = event.get("user_id").and_then(Value::as_i64)?;
    if self_id == 0 || user_id == 0 || user_id == self_id {
        return None;
    }
    let target = match event.get("message_type").and_then(Value::as_str) {
        Some("private") => Target::Private { user_id },
        Some("group") => Target::Group {
            group_id: event
                .get("group_id")
                .and_then(Value::as_i64)
                .filter(|group_id| *group_id != 0)?,
        },
        _ => return None,
    };
    let mut normalized_event = event.clone();
    normalized_event["self_id"] = Value::from(self_id);
    let parsed = parse_message(
        normalized_event.get("message"),
        normalized_event.get("raw_message"),
        self_id,
    );
    let mut inbound = message_event_at(
        target,
        &normalized_event,
        &parsed,
        activity
            .map(|activity| activity.received_at)
            .unwrap_or_else(Instant::now),
        activity.map(|activity| activity.position),
    );
    inbound.ingress_order = Some(ingress_order);
    Some(inbound)
}

pub(crate) fn sends_rate_limit_notice(target: Target) -> bool {
    matches!(target, Target::Group { .. })
}

pub(crate) struct Admission {
    pub(crate) allowed: bool,
    pub(crate) rate_key: Option<String>,
    pub(crate) rate_limit: PlatformRateLimit,
    pub(crate) use_non_whitelist_text_models: bool,
}

pub(crate) fn admission_for(
    config: &OneBotConfig,
    target: Target,
    self_id: i64,
    user_id: i64,
) -> Admission {
    admission_for_access(config, None, target, self_id, user_id)
}

pub(crate) fn admission_for_with_state(
    config: &OneBotConfig,
    state: &StateStore,
    target: Target,
    self_id: i64,
    user_id: i64,
) -> Admission {
    admission_for_access(config, Some(state), target, self_id, user_id)
}

pub(crate) fn admission_for_access(
    config: &OneBotConfig,
    state: Option<&StateStore>,
    target: Target,
    self_id: i64,
    user_id: i64,
) -> Admission {
    let account_id = self_id.to_string();
    let user_id_text = user_id.to_string();
    let is_admin = state.map_or_else(
        || config.admin_users.contains(&user_id),
        |state| {
            config.admin_users.contains(&user_id)
                || has_dynamic_access(
                    state,
                    &account_id,
                    AccessPermission::Administrator,
                    &user_id_text,
                )
        },
    );
    match target {
        Target::Private { user_id } => {
            if is_admin {
                return Admission {
                    allowed: true,
                    rate_key: None,
                    rate_limit: PlatformRateLimit::default(),
                    use_non_whitelist_text_models: false,
                };
            }
            let whitelisted = state.map_or_else(
                || config.private_chats.whitelist.contains(&user_id),
                |state| {
                    config.private_chats.whitelist.contains(&user_id)
                        || has_dynamic_access(
                            state,
                            &account_id,
                            AccessPermission::PrivateWhitelist,
                            &user_id_text,
                        )
                },
            );
            if whitelisted {
                Admission {
                    allowed: true,
                    rate_key: None,
                    rate_limit: PlatformRateLimit::default(),
                    use_non_whitelist_text_models: false,
                }
            } else {
                Admission {
                    allowed: config.private_chats.allow_non_whitelist,
                    rate_key: Some(format!("qq:{self_id}:private:{user_id}")),
                    rate_limit: config.private_chats.non_whitelist_rate_limit,
                    use_non_whitelist_text_models: true,
                }
            }
        }
        Target::Group { group_id } => {
            let group_id_text = group_id.to_string();
            let whitelisted = state.map_or_else(
                || config.group_chats.whitelist.contains(&group_id),
                |state| {
                    config.group_chats.whitelist.contains(&group_id)
                        || has_dynamic_access(
                            state,
                            &account_id,
                            AccessPermission::GroupWhitelist,
                            &group_id_text,
                        )
                },
            );
            if is_admin {
                return Admission {
                    allowed: true,
                    rate_key: None,
                    rate_limit: PlatformRateLimit::default(),
                    use_non_whitelist_text_models: !whitelisted,
                };
            }
            let privileged = state.map_or_else(
                || config.private_chats.whitelist.contains(&user_id),
                |state| {
                    config.private_chats.whitelist.contains(&user_id)
                        || has_dynamic_access(
                            state,
                            &account_id,
                            AccessPermission::PrivateWhitelist,
                            &user_id_text,
                        )
                },
            );
            Admission {
                allowed: whitelisted || config.group_chats.allow_non_whitelist,
                rate_key: (!privileged).then(|| format!("qq:{self_id}:group:{group_id}")),
                rate_limit: if whitelisted {
                    config.group_chats.whitelist_rate_limit
                } else {
                    config.group_chats.non_whitelist_rate_limit
                },
                use_non_whitelist_text_models: !whitelisted,
            }
        }
    }
}

pub(crate) fn apply_admission_text_model_pool(
    config: &mut crate::config::AppConfig,
    target: Target,
    admission: &Admission,
) {
    let kind = match target {
        Target::Private { .. } => PlatformConversationKind::Private,
        Target::Group { .. } => PlatformConversationKind::Group,
    };
    let conversation_id = target.conversation_id().to_string();
    let models = config
        .qq_text_model_pool(
            kind,
            &conversation_id,
            admission.use_non_whitelist_text_models,
        )
        .map(<[_]>::to_vec);
    config.active_provider_models = models;
}
