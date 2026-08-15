// registry — 自 src/platforms/mod.rs 拆分。

use super::*;

// IM platform bridges.
//
// This module is the platform-neutral core: turn driving against the
// agent actor, session resolution, rate limiting and reply shaping.
// Each protocol lives in its own submodule (`onebot` = NapCat / QQ);
// later platforms (Telegram, QQ official, WeChat) add submodules and
// reuse everything here without touching the web core.

pub(crate) use types::{
    BotGroupRole, BotSendAvailability, ConversationKind, ForwardNode, OutboundBody,
    OutboundMessage, OutboundOrigin, OutboundSegment, PartialSendError, PlatformAdapter,
    PlatformContextImageRef, PlatformConversation, PlatformGroupMember, PlatformImageData,
    PlatformInboundEvent, PlatformInboundEventKind, PlatformInboundMedia, PlatformMediaKind,
    PlatformMention, PlatformMessageInfo, PlatformMessagePosition, PlatformPrincipal,
    ResponseTarget, SendReceipt, TriggerDecision,
};

use crate::agent::{AgentMode, QueueIngressBarrier, QueueIngressReservation};
use crate::config::{
    ActiveProviderModelConfig, AppConfig, PlatformRateLimit, PlatformSessionLimits, PromptAudience,
};
use crate::i18n::{text_for, Locale};
use crate::ipc::ImageAttachment;
use crate::paths::GQYPaths;
use crate::state::{PlatformSessionBindingKey, StateStore};
use crate::web::{random_id, validate_content, ActorCommand, DaemonState, IpcRunGuard, RunInfo};
use anyhow::{anyhow, bail, Context, Result};
use futures_util::StreamExt;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};
use tokio::sync::broadcast;

/// How long a delivered image stays deduplicated for its conversation.
/// Auto-attached reply images (generate_image / search_web_images) must not
/// be sent twice when a turn is retried or recovered after an interrupted
/// send; an explicit "send it again" goes through send_message_to_user,
/// which is not filtered by this.
/// Kept short: it only needs to span a recovery turn, and a genuine
/// "send that one again" outside the window must still work.
pub(crate) const RECENT_IMAGE_TTL: Duration = Duration::from_secs(5 * 60);
pub(crate) const RECENT_IMAGE_CONVERSATIONS: usize = 64;
pub(crate) const RECENT_IMAGES_PER_CONVERSATION: usize = 32;

type RecentImageLedger = HashMap<String, Vec<(blake3::Hash, Instant)>>;

pub(crate) fn recent_images() -> &'static Mutex<RecentImageLedger> {
    pub(crate) static LEDGER: OnceLock<Mutex<RecentImageLedger>> = OnceLock::new();
    LEDGER.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(crate) fn record_recent_conversation_images(scope_key: &str, digests: &[blake3::Hash]) {
    let now = Instant::now();
    let mut ledger = recent_images().lock().unwrap();
    ledger.retain(|_, entries| {
        entries.retain(|(_, at)| now.duration_since(*at) < RECENT_IMAGE_TTL);
        !entries.is_empty()
    });
    let entries = ledger.entry(scope_key.to_string()).or_default();
    for digest in digests {
        entries.retain(|(known, _)| known != digest);
        entries.push((*digest, now));
    }
    if entries.len() > RECENT_IMAGES_PER_CONVERSATION {
        let excess = entries.len() - RECENT_IMAGES_PER_CONVERSATION;
        entries.drain(..excess);
    }
    if ledger.len() > RECENT_IMAGE_CONVERSATIONS {
        // Bound the ledger even when every conversation stays inside the TTL.
        let oldest = ledger
            .iter()
            .filter_map(|(key, entries)| entries.last().map(|(_, at)| (*at, key.clone())))
            .min()
            .map(|(_, key)| key);
        if let Some(key) = oldest {
            ledger.remove(&key);
        }
    }
}

pub(crate) fn recent_conversation_images(scope_key: &str) -> Vec<blake3::Hash> {
    let now = Instant::now();
    recent_images()
        .lock()
        .unwrap()
        .get(scope_key)
        .map(|entries| {
            entries
                .iter()
                .filter(|(_, at)| now.duration_since(*at) < RECENT_IMAGE_TTL)
                .map(|(digest, _)| *digest)
                .collect()
        })
        .unwrap_or_default()
}

/// Hard ceiling for one platform-driven turn; beyond this the run is
/// cancelled so a wedged turn cannot pin the bridge task forever.
pub(crate) const PLATFORM_TURN_TIMEOUT: Duration = Duration::from_secs(30 * 60);
pub(crate) const RATE_PRUNE_INTERVAL: Duration = Duration::from_secs(60);
pub(crate) const MAX_CONCURRENT_PLATFORM_TURNS: usize = 16;
pub(crate) const PLATFORM_TOOL_LOG_MAX_CHARS: usize = 2_400;
pub(crate) const PLATFORM_REPLY_LOG_MAX_CHARS: usize = 1_200;
pub(crate) const MESSAGE_ACTIVITY_SCOPE_SOFT_LIMIT: usize = 512;
pub(crate) const MESSAGE_ACTIVITY_SEEN_LIMIT: usize = 4_096;
pub(crate) const MESSAGE_ACTIVITY_MAX_ID_BYTES: usize = 256;

#[derive(Clone, Default)]
pub(crate) struct MessageActivityRegistry {
    entries: Arc<Mutex<HashMap<String, Weak<MessageActivity>>>>,
}

#[derive(Clone)]
pub(crate) struct MessageActivityHandle(Arc<MessageActivity>);

pub(crate) struct MessageActivity {
    state: Mutex<MessageActivityState>,
}

#[derive(Default)]
pub(crate) struct MessageActivityState {
    total_messages: u64,
    sender_messages: HashMap<String, u64>,
    seen_messages: HashMap<String, SeenMessage>,
}

#[derive(Clone, Copy)]
pub(crate) struct SeenMessage {
    position: PlatformMessagePosition,
    received_at: Instant,
}

impl MessageActivityRegistry {
    pub(crate) fn observe(
        &self,
        scope: &str,
        message_id: &str,
        sender_id: &str,
        received_at: Instant,
    ) -> (MessageActivityHandle, PlatformMessagePosition, Instant) {
        let activity = {
            let mut entries = self.entries.lock().unwrap();
            if entries.len() >= MESSAGE_ACTIVITY_SCOPE_SOFT_LIMIT && !entries.contains_key(scope) {
                entries.retain(|_, activity| activity.strong_count() > 0);
            }
            match entries.get(scope).and_then(Weak::upgrade) {
                Some(activity) => activity,
                None => {
                    let activity = Arc::new(MessageActivity {
                        state: Mutex::new(MessageActivityState::default()),
                    });
                    entries.insert(scope.to_string(), Arc::downgrade(&activity));
                    activity
                }
            }
        };
        let handle = MessageActivityHandle(activity);
        let (position, received_at) = handle.observe(message_id, sender_id, received_at);
        (handle, position, received_at)
    }
}

impl MessageActivityHandle {
    pub(crate) fn observe(
        &self,
        message_id: &str,
        sender_id: &str,
        received_at: Instant,
    ) -> (PlatformMessagePosition, Instant) {
        let mut state = self.0.state.lock().unwrap();
        let track_id = !message_id.is_empty() && message_id.len() <= MESSAGE_ACTIVITY_MAX_ID_BYTES;
        if track_id {
            if let Some(seen) = state.seen_messages.get(message_id) {
                return (seen.position, seen.received_at);
            }
        }
        state.total_messages = state.total_messages.saturating_add(1);
        let total_messages = state.total_messages;
        let sender_messages = {
            let count = state
                .sender_messages
                .entry(sender_id.to_string())
                .or_default();
            *count = count.saturating_add(1);
            *count
        };
        let position = PlatformMessagePosition {
            total_messages,
            sender_messages,
        };
        if track_id {
            if state.seen_messages.len() >= MESSAGE_ACTIVITY_SEEN_LIMIT {
                state.seen_messages.clear();
            }
            state.seen_messages.insert(
                message_id.to_string(),
                SeenMessage {
                    position,
                    received_at,
                },
            );
        }
        (position, received_at)
    }

    pub(crate) fn position_for(&self, sender_id: &str) -> PlatformMessagePosition {
        let state = self.0.state.lock().unwrap();
        PlatformMessagePosition {
            total_messages: state.total_messages,
            sender_messages: state.sender_messages.get(sender_id).copied().unwrap_or(0),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct AdaptiveResponseTargetPolicy {
    position: Option<PlatformMessagePosition>,
    received_at: Instant,
    quote_after_other_messages: u64,
    mention_after: Duration,
}

impl AdaptiveResponseTargetPolicy {
    pub(crate) fn new(
        position: Option<PlatformMessagePosition>,
        received_at: Instant,
        quote_after_other_messages: u64,
        mention_after_seconds: u64,
    ) -> Self {
        Self {
            position,
            received_at,
            quote_after_other_messages,
            mention_after: Duration::from_secs(mention_after_seconds),
        }
    }

    pub(crate) fn resolve(
        self,
        mut target: ResponseTarget,
        current: Option<PlatformMessagePosition>,
        now: Instant,
    ) -> Option<ResponseTarget> {
        let other_messages = self.position.zip(current).map(|(start, current)| {
            let total = current.total_messages.saturating_sub(start.total_messages);
            let same_sender = current
                .sender_messages
                .saturating_sub(start.sender_messages);
            total.saturating_sub(same_sender)
        });
        if target.quote {
            target.quote = self.quote_after_other_messages == 0
                || other_messages.is_some_and(|count| count >= self.quote_after_other_messages);
        }
        if target.mention {
            // Unknown activity preserves the original time-only mention behavior.
            target.mention = now
                .checked_duration_since(self.received_at)
                .unwrap_or_default()
                >= self.mention_after
                && other_messages.is_none_or(|count| count > 0);
        }
        target.is_effective().then_some(target)
    }
}

#[derive(Clone)]
pub(crate) struct PendingResponseTarget {
    target: ResponseTarget,
    policy: Option<AdaptiveResponseTargetPolicy>,
}

/// Shared state for all IM bridges, hung off `DaemonState`. Cheap to clone;
/// everything inside is reference counted.
#[derive(Clone)]
pub(crate) struct PlatformRuntime {
    http: Arc<OnceLock<std::result::Result<reqwest::Client, String>>>,
    pub(crate) onebot: Arc<Mutex<onebot::ConnectionRegistry>>,
    pub(crate) qq_listener: onebot::QqListenerManager,
    pub(crate) rate: Arc<Mutex<RateWindow>>,
    plugins: Arc<OnceLock<std::result::Result<Arc<plugins::PlatformPluginRegistry>, String>>>,
    pub(crate) assets: assets::AssetLeaseStore,
    pub(crate) turn_permits: Arc<tokio::sync::Semaphore>,
    pub(crate) file_store_lock: Arc<tokio::sync::Mutex<()>>,
    pub(crate) message_activity: MessageActivityRegistry,
    session_turn_locks: Arc<Mutex<HashMap<String, Weak<SessionTurnState>>>>,
}

impl PlatformRuntime {
    pub(crate) fn new() -> Result<Self> {
        Ok(Self {
            http: Arc::new(OnceLock::new()),
            onebot: Arc::new(Mutex::new(onebot::ConnectionRegistry::default())),
            qq_listener: onebot::QqListenerManager::default(),
            rate: Arc::new(Mutex::new(RateWindow::new())),
            plugins: Arc::new(OnceLock::new()),
            assets: assets::AssetLeaseStore::new(),
            turn_permits: Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_PLATFORM_TURNS)),
            file_store_lock: Arc::new(tokio::sync::Mutex::new(())),
            message_activity: MessageActivityRegistry::default(),
            session_turn_locks: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    pub(crate) fn http_client(&self) -> Result<reqwest::Client> {
        self.http
            .get_or_init(|| {
                reqwest::Client::builder()
                    .connect_timeout(Duration::from_secs(10))
                    .build()
                    .map_err(|error| error.to_string())
            })
            .as_ref()
            .cloned()
            .map_err(|error| anyhow!("building the IM platform HTTP client: {error}"))
    }

    pub(crate) fn plugins(&self) -> Result<Arc<plugins::PlatformPluginRegistry>> {
        self.plugins
            .get_or_init(|| {
                plugins::PlatformPluginRegistry::built_in()
                    .map(Arc::new)
                    .map_err(|error| error.to_string())
            })
            .as_ref()
            .cloned()
            .map_err(|error| anyhow!("building the IM platform plugin registry: {error}"))
    }

    pub(crate) fn session_turn_ticket(
        &self,
        session_id: &str,
        limits: PlatformSessionLimits,
    ) -> SessionTurnTicket {
        let state = {
            let mut locks = self.session_turn_locks.lock().unwrap();
            match locks.get(session_id).and_then(Weak::upgrade) {
                Some(state) => state,
                None => {
                    let state = Arc::new(SessionTurnState::new(limits));
                    locks.insert(session_id.to_string(), Arc::downgrade(&state));
                    state
                }
            }
        };
        SessionTurnTicket {
            session_id: session_id.to_string(),
            generation: state.generation.load(Ordering::Acquire),
            state,
            states: self.session_turn_locks.clone(),
            exclusive: false,
        }
    }

    pub(crate) async fn acquire_session_turn(
        &self,
        session_id: &str,
        limits: PlatformSessionLimits,
    ) -> std::result::Result<SessionTurnLease, SessionTurnAcquireError> {
        self.session_turn_ticket(session_id, limits).acquire().await
    }

    pub(crate) fn preempt_session_turns(&self, session_id: &str) -> SessionTurnTicket {
        let mut ticket = self.session_turn_ticket(session_id, PlatformSessionLimits::default());
        ticket.generation = ticket
            .state
            .generation
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1);
        ticket.exclusive = true;
        ticket.state.preempting.store(true, Ordering::Release);
        ticket
    }

    pub(crate) fn queued_session_turns(&self, session_id: &str) -> usize {
        let locks = self.session_turn_locks.lock().unwrap();
        locks
            .get(session_id)
            .and_then(Weak::upgrade)
            .map(|state| state.waiting.load(Ordering::Acquire))
            .unwrap_or(0)
    }
}

pub(crate) struct SessionTurnState {
    slots: Arc<tokio::sync::Semaphore>,
    gate: Arc<tokio::sync::RwLock<()>>,
    waiting: AtomicUsize,
    max_queued: usize,
    preempting: AtomicBool,
    preemption_changed: tokio::sync::Notify,
    generation: AtomicU64,
}

impl SessionTurnState {
    pub(crate) fn new(limits: PlatformSessionLimits) -> Self {
        Self {
            slots: Arc::new(tokio::sync::Semaphore::new(limits.running)),
            gate: Arc::new(tokio::sync::RwLock::new(())),
            waiting: AtomicUsize::new(0),
            max_queued: limits.queued,
            preempting: AtomicBool::new(false),
            preemption_changed: tokio::sync::Notify::new(),
            generation: AtomicU64::new(0),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionTurnAcquireError {
    Full,
    Closed,
}

pub(crate) struct SessionTurnTicket {
    states: Arc<Mutex<HashMap<String, Weak<SessionTurnState>>>>,
    session_id: String,
    state: Arc<SessionTurnState>,
    generation: u64,
    exclusive: bool,
}

impl SessionTurnTicket {
    pub(crate) async fn acquire(
        self,
    ) -> std::result::Result<SessionTurnLease, SessionTurnAcquireError> {
        let (guard, permit) = if self.exclusive {
            (
                SessionTurnGuard::Write(self.state.gate.clone().write_owned().await),
                None,
            )
        } else {
            let permit = match self.state.slots.clone().try_acquire_owned() {
                Ok(permit) => permit,
                Err(tokio::sync::TryAcquireError::Closed) => {
                    return Err(SessionTurnAcquireError::Closed)
                }
                Err(tokio::sync::TryAcquireError::NoPermits) => {
                    if self
                        .state
                        .waiting
                        .try_update(Ordering::AcqRel, Ordering::Acquire, |waiting| {
                            (waiting < self.state.max_queued).then_some(waiting + 1)
                        })
                        .is_err()
                    {
                        return Err(SessionTurnAcquireError::Full);
                    }
                    let acquired = self.state.slots.clone().acquire_owned().await;
                    self.state.waiting.fetch_sub(1, Ordering::AcqRel);
                    acquired.map_err(|_| SessionTurnAcquireError::Closed)?
                }
            };
            while self.state.preempting.load(Ordering::Acquire) {
                let changed = self.state.preemption_changed.notified();
                if !self.state.preempting.load(Ordering::Acquire) {
                    break;
                }
                changed.await;
            }
            (
                SessionTurnGuard::Read(self.state.gate.clone().read_owned().await),
                Some(permit),
            )
        };
        Ok(SessionTurnLease {
            guard: Some(guard),
            permit,
            states: self.states,
            session_id: self.session_id,
            state: self.state,
            generation: self.generation,
            exclusive: self.exclusive,
        })
    }
}

pub(crate) enum SessionTurnGuard {
    Read(tokio::sync::OwnedRwLockReadGuard<()>),
    Write(tokio::sync::OwnedRwLockWriteGuard<()>),
}

pub(crate) struct SessionTurnLease {
    guard: Option<SessionTurnGuard>,
    permit: Option<tokio::sync::OwnedSemaphorePermit>,
    states: Arc<Mutex<HashMap<String, Weak<SessionTurnState>>>>,
    session_id: String,
    state: Arc<SessionTurnState>,
    generation: u64,
    exclusive: bool,
}

impl SessionTurnLease {
    pub(crate) fn is_valid(&self) -> bool {
        self.state.generation.load(Ordering::Acquire) == self.generation
    }
}

impl Drop for SessionTurnLease {
    fn drop(&mut self) {
        // Release the session before removing its registry entry. Otherwise a
        // new arrival could create a second lock during this guard's drop.
        self.guard.take();
        self.permit.take();
        if self.exclusive {
            self.state.preempting.store(false, Ordering::Release);
            self.state.preemption_changed.notify_waiters();
        }
        let mut states = self.states.lock().unwrap();
        if Arc::strong_count(&self.state) == 1
            && states
                .get(&self.session_id)
                .is_some_and(|registered| Weak::ptr_eq(registered, &Arc::downgrade(&self.state)))
        {
            states.remove(&self.session_id);
        }
    }
}

pub(crate) use assets::platform_asset;

#[derive(Clone)]
pub(crate) struct TurnProfile {
    pub(crate) active_persona: Option<String>,
    pub(crate) text_models: Option<Vec<ActiveProviderModelConfig>>,
    pub(crate) multimodal_models: Option<Vec<ActiveProviderModelConfig>>,
    pub(crate) system_context: Vec<String>,
    /// Per-message transport context (sender identity JSON, message ids, …).
    /// Rendered as a tail system message after the user turn instead of being
    /// folded into the system prompt, so the stable prefix stays byte-identical
    /// across turns (v7 Phase 2.1).
    pub(crate) turn_system_context: Vec<String>,
    /// Raw input snapshot for the memory diary (pre-plugin content); `None`
    /// keeps the agent's default of recording the turn content as-is.
    pub(crate) memory_content: Option<String>,
    pub(crate) context_images: Vec<PlatformContextImageRef>,
    pub(crate) platform: Option<Arc<PlatformTurnContext>>,
    pub(crate) image_cache_namespace: Option<String>,
    pub(crate) image_source_label: Option<String>,
    pub(crate) memory_write_enabled: bool,
    /// Structured platform history replaces ambiguous core user/assistant
    /// replay for shared conversations such as QQ groups.
    pub(crate) suppress_session_history: bool,
    /// Group overflow handling; `None` inherits the global `context` settings.
    pub(crate) group_context: Option<crate::config::PlatformGroupContextConfig>,
    pub(crate) followup: Option<Arc<PlatformFollowupRun>>,
}

impl Default for TurnProfile {
    fn default() -> Self {
        Self {
            active_persona: None,
            text_models: None,
            multimodal_models: None,
            system_context: Vec::new(),
            turn_system_context: Vec::new(),
            memory_content: None,
            context_images: Vec::new(),
            platform: None,
            image_cache_namespace: None,
            image_source_label: None,
            memory_write_enabled: true,
            suppress_session_history: false,
            group_context: None,
            followup: None,
        }
    }
}

pub(crate) struct PlatformFollowupRun {
    pub(crate) conversation: PlatformConversation,
    pub(crate) sender_id: String,
    pub(crate) context: Arc<PlatformTurnContext>,
    ingress: Arc<QueueIngressBarrier>,
    enqueue: tokio::sync::Mutex<()>,
    started: Instant,
}

impl PlatformFollowupRun {
    pub(crate) fn new(context: Arc<PlatformTurnContext>) -> Arc<Self> {
        Arc::new(Self {
            conversation: context.conversation.clone(),
            sender_id: context.sender_id.clone(),
            context,
            ingress: Arc::new(QueueIngressBarrier::default()),
            enqueue: tokio::sync::Mutex::new(()),
            started: Instant::now(),
        })
    }

    pub(crate) fn ingress(&self) -> Arc<QueueIngressBarrier> {
        self.ingress.clone()
    }

    pub(crate) fn try_reserve(&self) -> Option<QueueIngressReservation> {
        self.ingress.try_reserve()
    }

    pub(crate) async fn lock_enqueue(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.enqueue.lock().await
    }

    pub(crate) fn started(&self) -> Instant {
        self.started
    }

    pub(crate) fn close(&self) {
        self.ingress.close();
    }
}

pub(crate) struct PlatformTurnContext {
    pub(crate) conversation: PlatformConversation,
    pub(crate) sender_id: String,
    pub(crate) sender_display_name: String,
    pub(crate) is_admin: bool,
    pub(crate) config: AppConfig,
    pub(crate) paths: GQYPaths,
    pub(crate) state_store: StateStore,
    adapter: Arc<dyn PlatformAdapter>,
    plugins: Arc<plugins::PlatformPluginRegistry>,
    config_manager: Option<Weak<Mutex<crate::web::ManagerState>>>,
    inbound_event: Option<Arc<PlatformInboundEvent>>,
    message_activity: Option<MessageActivityHandle>,
    response_target: Mutex<Option<PendingResponseTarget>>,
    group_member_cache: Mutex<HashMap<String, PlatformGroupMember>>,
    plugin_values: Mutex<BTreeMap<String, Value>>,
    delivered_image_digests: Mutex<HashSet<blake3::Hash>>,
    reply_rate_available: AtomicBool,
    pending_final_reply_suppression: AtomicBool,
    pending_prior_reply_suppression: AtomicBool,
}

impl PlatformTurnContext {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        conversation: PlatformConversation,
        sender_id: String,
        sender_display_name: String,
        is_admin: bool,
        config: AppConfig,
        paths: GQYPaths,
        state_store: StateStore,
        adapter: Arc<dyn PlatformAdapter>,
        plugins: Arc<plugins::PlatformPluginRegistry>,
    ) -> Self {
        Self {
            conversation,
            sender_id,
            sender_display_name,
            is_admin,
            config,
            paths,
            state_store,
            adapter,
            plugins,
            config_manager: None,
            inbound_event: None,
            message_activity: None,
            response_target: Mutex::new(None),
            group_member_cache: Mutex::new(HashMap::new()),
            plugin_values: Mutex::new(BTreeMap::new()),
            delivered_image_digests: Mutex::new(HashSet::new()),
            reply_rate_available: AtomicBool::new(true),
            pending_final_reply_suppression: AtomicBool::new(false),
            pending_prior_reply_suppression: AtomicBool::new(false),
        }
    }

    pub(crate) fn with_inbound_event(mut self, event: PlatformInboundEvent) -> Self {
        self.inbound_event = Some(Arc::new(event));
        self
    }

    pub(crate) fn with_message_activity(mut self, activity: MessageActivityHandle) -> Self {
        self.message_activity = Some(activity);
        self
    }

    pub(crate) fn with_config_manager(
        mut self,
        manager: Arc<Mutex<crate::web::ManagerState>>,
    ) -> Self {
        self.config_manager = Some(Arc::downgrade(&manager));
        self
    }

    pub(crate) fn with_current_config<T>(&self, read: impl FnOnce(&AppConfig) -> T) -> T {
        match self.config_manager.as_ref().and_then(Weak::upgrade) {
            Some(manager) => read(&manager.lock().unwrap().config),
            None => read(&self.config),
        }
    }

    pub(crate) fn inbound_event(&self) -> Option<&PlatformInboundEvent> {
        self.inbound_event.as_deref()
    }

    pub(crate) fn principal(&self) -> PlatformPrincipal {
        PlatformPrincipal {
            platform: self.conversation.platform.clone(),
            account_id: self.conversation.account_id.clone(),
            user_id: self.sender_id.clone(),
        }
    }

    pub(crate) fn set_response_target(&self, target: Option<ResponseTarget>) {
        let target = target.filter(ResponseTarget::is_effective);
        let mut pending = self.response_target.lock().unwrap();
        match target {
            Some(target)
                if pending
                    .as_ref()
                    .is_some_and(|existing| existing.target == target) =>
            {
                pending.as_mut().expect("target exists").target = target;
            }
            Some(target) => {
                *pending = Some(PendingResponseTarget {
                    target,
                    policy: None,
                });
            }
            None => *pending = None,
        }
    }

    pub(crate) fn set_adaptive_response_target(
        &self,
        target: Option<ResponseTarget>,
        policy: AdaptiveResponseTargetPolicy,
    ) {
        let mut pending = self.response_target.lock().unwrap();
        let explicit_mentions = pending
            .as_ref()
            .map(|pending| pending.target.explicit_mention_user_ids.clone())
            .filter(|mentions| !mentions.is_empty());
        let target = target.filter(ResponseTarget::is_effective);
        *pending = match (target, explicit_mentions) {
            (Some(mut target), Some(mentions)) => {
                target.mention = false;
                target.explicit_mention_user_ids = mentions;
                Some(PendingResponseTarget {
                    target,
                    policy: Some(policy),
                })
            }
            (Some(target), None) => Some(PendingResponseTarget {
                target,
                policy: Some(policy),
            }),
            (None, Some(mentions)) => Some(PendingResponseTarget {
                target: ResponseTarget {
                    message_id: String::new(),
                    user_id: String::new(),
                    quote: false,
                    mention: false,
                    explicit_mention_user_ids: mentions,
                },
                policy: None,
            }),
            (None, None) => None,
        };
    }

    pub(crate) fn response_target(&self) -> Option<ResponseTarget> {
        self.response_target
            .lock()
            .unwrap()
            .as_ref()
            .map(|pending| pending.target.clone())
    }

    pub(crate) fn set_explicit_response_mentions(&self, user_ids: Vec<String>) {
        if user_ids.is_empty() {
            return;
        }
        let mut pending = self.response_target.lock().unwrap();
        if let Some(pending) = pending.as_mut() {
            pending.target.mention = false;
            pending.target.explicit_mention_user_ids = user_ids;
        } else {
            *pending = Some(PendingResponseTarget {
                target: ResponseTarget {
                    message_id: String::new(),
                    user_id: String::new(),
                    quote: false,
                    mention: false,
                    explicit_mention_user_ids: user_ids,
                },
                policy: None,
            });
        }
    }

    pub(crate) fn set_plugin_value(&self, key: impl Into<String>, value: Value) {
        self.plugin_values.lock().unwrap().insert(key.into(), value);
    }

    pub(crate) fn remove_plugin_value(&self, key: &str) {
        self.plugin_values.lock().unwrap().remove(key);
    }

    pub(crate) fn plugin_value(&self, key: &str) -> Option<Value> {
        self.plugin_values.lock().unwrap().get(key).cloned()
    }

    pub(crate) fn set_reply_rate_available(&self, available: bool) {
        self.reply_rate_available
            .store(available, Ordering::Release);
    }

    pub(crate) fn reply_rate_available(&self) -> bool {
        self.reply_rate_available.load(Ordering::Acquire)
    }

    pub(crate) fn plugin_enabled(&self, id: &str, default_enabled: bool) -> bool {
        self.config
            .platforms
            .qq
            .plugins
            .get(id)
            .and_then(|plugin| plugin.enabled)
            .unwrap_or(default_enabled)
    }

    pub(crate) fn host_tools_allowed(&self) -> bool {
        if self.is_admin {
            return true;
        }
        self.conversation.kind == ConversationKind::Private
            && self.config.platforms.qq.allow_non_admin_host_tools
            && self.sender_id.parse::<i64>().ok().is_some_and(|sender| {
                self.config
                    .platforms
                    .qq
                    .private_chats
                    .whitelist
                    .contains(&sender)
                    || access_control::has_dynamic_access(
                        &self.state_store,
                        &self.conversation.account_id,
                        access_control::AccessPermission::PrivateWhitelist,
                        &self.sender_id,
                    )
            })
    }

    pub(crate) async fn handle_command(&self, text: &str) -> Option<OutboundMessage> {
        self.plugins.handle_command(self, text).await
    }

    pub(crate) async fn prepare_turn(&self, content: String) -> plugins::PlatformTurnInput {
        let mut input = plugins::PlatformTurnInput {
            memory_content: content.clone(),
            content,
            system_context: Vec::new(),
            turn_system_context: Vec::new(),
            context_images: Vec::new(),
        };
        self.plugins.before_turn(self, &mut input).await;
        input
    }

    pub(crate) async fn observe_inbound(&self, event: &PlatformInboundEvent) {
        self.plugins.observe_inbound(self, event).await;
    }

    pub(crate) fn accept_followup(&self, event: &PlatformInboundEvent) {
        self.plugins.accept_followup(self, event);
    }

    pub(crate) fn preempt_inbound(&self, event: &PlatformInboundEvent) -> bool {
        self.plugins.preempt_inbound(self, event)
    }

    pub(crate) async fn confirm_supersede(&self, event: &PlatformInboundEvent) {
        self.plugins.confirm_supersede(self, event).await;
    }

    pub(crate) fn turn_is_superseded(&self) -> bool {
        self.plugins.turn_is_superseded(self)
    }

    pub(crate) fn turn_started(&self, cancel: tokio::sync::watch::Sender<bool>) {
        self.plugins.turn_started(self, cancel);
    }

    pub(crate) async fn after_turn_aborted(&self) {
        self.plugins.after_turn_aborted(self).await;
    }

    pub(crate) async fn decide_trigger(
        &self,
        event: &PlatformInboundEvent,
        decision: &mut TriggerDecision,
    ) {
        self.plugins.decide_trigger(self, event, decision).await;
    }

    pub(crate) async fn after_session_reset(&self) -> Result<()> {
        self.plugins.after_session_reset(self).await
    }

    pub(crate) async fn send(&self, mut message: OutboundMessage) -> Result<SendReceipt> {
        if matches!(
            message.origin,
            OutboundOrigin::FinalReply | OutboundOrigin::IntermediateReply | OutboundOrigin::Tool
        ) && message_is_parenthetical_only(&message)
        {
            tracing::info!(
                platform = %self.conversation.platform,
                conversation_kind = self.conversation.kind.as_str(),
                conversation_id = %self.conversation.conversation_id,
                "{}",
                crate::i18n::text(
                    "suppressed a parenthetical-only model reply",
                    "已抑制仅含括号内容的模型回复",
                )
            );
            return Ok(SendReceipt::default());
        }
        let reserved_target = if message.response_target.is_none()
            && matches!(
                message.origin,
                OutboundOrigin::FinalReply | OutboundOrigin::Tool
            ) {
            self.response_target.lock().unwrap().take()
        } else {
            None
        };
        if let Some(target) = reserved_target.as_ref() {
            message.response_target = Some(target.target.clone());
        }
        let mut prepared = self.plugins.before_send(self, message).await;
        if let Some(target) = reserved_target.as_ref() {
            let current = self
                .message_activity
                .as_ref()
                .map(|activity| activity.position_for(&target.target.user_id));
            let resolved = target
                .policy
                .and_then(|policy| policy.resolve(target.target.clone(), current, Instant::now()))
                .or_else(|| target.policy.is_none().then(|| target.target.clone()));
            apply_resolved_response_target(
                &mut prepared.primary,
                &target.target,
                resolved.as_ref(),
            );
            if let Some(fallback) = prepared.fallback.as_mut() {
                apply_resolved_response_target(fallback, &target.target, resolved.as_ref());
            }
        }
        let primary = prepared.primary;
        let delivered = match self.adapter.send(primary.clone()).await {
            Ok(receipt) => Ok((primary, receipt, true)),
            Err(error) => {
                let (partially_delivered, response_target_delivered) =
                    self.record_partial_delivery(&error);
                match (partially_delivered, prepared.fallback) {
                    (true, _) => {
                        // `gqy::qq` and not the module default: these are
                        // delivery outcomes an operator reads next to the
                        // "回复已投递" lines, and every other target is filtered
                        // to ERROR unless GQY_LOG says otherwise (see
                        // `logging::init`), which kept this whole branch
                        // invisible in the QQ log.
                        tracing::warn!(
                            target: "gqy::qq",
                            error = %error,
                            "{}",
                            crate::i18n::text(
                                "platform message partially succeeded; skipped the full fallback to avoid duplicate delivery",
                                "平台消息部分发送成功；为避免重复投递，已跳过完整回退消息",
                            )
                        );
                        Err((error, response_target_delivered))
                    }
                    (false, Some(fallback)) => {
                        tracing::warn!(target: "gqy::qq", error = %error, "{}", crate::i18n::text("transformed platform message failed; sending fallback", "转换后的平台消息发送失败；正在发送回退消息"));
                        match self.adapter.send(fallback.clone()).await {
                            Ok(receipt) => Ok((fallback, receipt, false)),
                            Err(error) => {
                                let (_, response_target_delivered) =
                                    self.record_partial_delivery(&error);
                                Err((error, response_target_delivered))
                            }
                        }
                    }
                    (false, None) => Err((error, false)),
                }
            }
        };
        let (delivered_message, receipt, transformed_primary_succeeded) = match delivered {
            Ok(delivered) => delivered,
            Err((error, response_target_delivered)) => {
                if !response_target_delivered {
                    if let Some(target) = reserved_target {
                        self.restore_response_target(target);
                    }
                }
                return Err(error);
            }
        };
        self.record_delivered_images(&receipt);
        self.plugins
            .after_send(self, &delivered_message, &receipt)
            .await;
        for message in prepared.after_success {
            let history_text = outbound_text_for_history(&message);
            match self.adapter.send(message).await {
                Ok(receipt) => {
                    self.record_delivered_images(&receipt);
                    let message_id = receipt
                        .message_ids
                        .first()
                        .map(String::as_str)
                        .unwrap_or("");
                    self.plugins
                        .record_external_bot_message(self, message_id, &history_text)
                        .await;
                }
                Err(error) => {
                    let _ = self.record_partial_delivery(&error);
                    tracing::warn!(target: "gqy::qq", error = %error, "{}", crate::i18n::text("platform plugin follow-up send failed", "平台插件后续消息发送失败"));
                }
            }
        }
        if prepared.suppress_final_reply
            && transformed_primary_succeeded
            && delivered_message.origin == OutboundOrigin::Tool
        {
            self.pending_final_reply_suppression
                .store(true, Ordering::Release);
            if prepared.suppress_prior_reply {
                self.pending_prior_reply_suppression
                    .store(true, Ordering::Release);
            }
        }
        Ok(receipt)
    }

    pub(crate) async fn send_bypass_plugins(
        &self,
        message: OutboundMessage,
    ) -> Result<SendReceipt> {
        let history_text = outbound_text_for_history(&message);
        match self.adapter.send(message).await {
            Ok(receipt) => {
                self.record_delivered_images(&receipt);
                let message_id = receipt
                    .message_ids
                    .first()
                    .map(String::as_str)
                    .unwrap_or("");
                self.plugins
                    .record_external_bot_message(self, message_id, &history_text)
                    .await;
                Ok(receipt)
            }
            Err(error) => {
                let _ = self.record_partial_delivery(&error);
                Err(error)
            }
        }
    }

    pub(crate) fn record_delivered_images(&self, receipt: &SendReceipt) {
        if receipt.image_digests.is_empty() {
            return;
        }
        self.delivered_image_digests
            .lock()
            .unwrap()
            .extend(receipt.image_digests.iter().copied());
        record_recent_conversation_images(&self.conversation.scope_key(), &receipt.image_digests);
    }

    pub(crate) fn record_partial_delivery(&self, error: &anyhow::Error) -> (bool, bool) {
        let Some(partial) = error.downcast_ref::<PartialSendError>() else {
            return (false, false);
        };
        self.record_delivered_images(partial.receipt());
        (
            partial.receipt().has_delivery(),
            partial.receipt().response_target_delivered,
        )
    }

    pub(crate) fn restore_response_target(&self, target: PendingResponseTarget) {
        let mut available = self.response_target.lock().unwrap();
        match available.as_mut() {
            Some(current)
                if current.target.explicit_mention_user_ids.is_empty()
                    && !target.target.explicit_mention_user_ids.is_empty() =>
            {
                current.target.mention = false;
                current.target.explicit_mention_user_ids = target.target.explicit_mention_user_ids;
            }
            Some(_) => {}
            None => *available = Some(target),
        }
    }

    pub(crate) fn delivered_image_digests(&self) -> HashSet<blake3::Hash> {
        let mut digests = self.delivered_image_digests.lock().unwrap().clone();
        digests.extend(recent_conversation_images(&self.conversation.scope_key()));
        digests
    }

    pub(crate) async fn bot_display_name(&self) -> Result<String> {
        self.adapter.bot_display_name().await
    }

    pub(crate) async fn bot_send_availability(&self) -> types::BotSendAvailability {
        match self.adapter.bot_send_availability().await {
            Ok(availability) => availability,
            Err(error) => {
                tracing::debug!(error = %error, "{}", crate::i18n::text("platform bot send availability lookup failed", "平台机器人发送可用性查询失败"));
                types::BotSendAvailability::Unknown
            }
        }
    }

    pub(crate) async fn set_message_reaction(
        &self,
        message_id: &str,
        reaction_id: &str,
        active: bool,
    ) -> Result<()> {
        self.adapter
            .set_message_reaction(message_id, reaction_id, active)
            .await
    }

    pub(crate) fn schedule_message_reaction_removal(
        &self,
        message_id: String,
        reaction_id: String,
        delay: Duration,
    ) -> tokio::task::AbortHandle {
        let adapter = self.adapter.clone();
        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            if let Err(error) = adapter
                .set_message_reaction(&message_id, &reaction_id, false)
                .await
            {
                tracing::debug!(
                    error = %error,
                    %message_id,
                    %reaction_id,
                    "{}",
                    crate::i18n::text(
                        "expired platform reaction could not be removed",
                        "无法移除已过期的平台表情回应",
                    )
                );
            }
        })
        .abort_handle()
    }

    pub(crate) async fn message_info(
        &self,
        message_id: &str,
    ) -> Result<Option<PlatformMessageInfo>> {
        self.adapter.message_info(message_id).await
    }

    pub(crate) fn message_images_task(
        &self,
        message_id: String,
    ) -> futures_util::future::BoxFuture<'static, Result<Vec<PlatformImageData>>> {
        let adapter = self.adapter.clone();
        Box::pin(async move { adapter.message_images(&message_id).await })
    }

    pub(crate) async fn group_members(&self) -> Result<Vec<PlatformGroupMember>> {
        let members = self.adapter.group_members().await?;
        self.group_member_cache.lock().unwrap().extend(
            members
                .iter()
                .cloned()
                .map(|member| (member.user_id.clone(), member)),
        );
        Ok(members)
    }

    pub(crate) async fn group_member(&self, user_id: &str) -> Result<Option<PlatformGroupMember>> {
        if let Some(member) = self
            .group_member_cache
            .lock()
            .unwrap()
            .get(user_id)
            .cloned()
        {
            return Ok(Some(member));
        }
        let member = self.adapter.group_member(user_id).await?;
        if let Some(member) = member.as_ref() {
            self.group_member_cache
                .lock()
                .unwrap()
                .insert(member.user_id.clone(), member.clone());
        }
        Ok(member)
    }

    /// Membership as the server sees it *now*, skipping both the per-turn cache
    /// and the platform's roster cache. Destructive actions validate through
    /// this so a member who already left is refused here instead of failing
    /// deep inside the bridge.
    pub(crate) async fn group_member_fresh(
        &self,
        user_id: &str,
    ) -> Result<Option<PlatformGroupMember>> {
        let member = self.adapter.group_member_fresh(user_id).await?;
        let mut cache = self.group_member_cache.lock().unwrap();
        match member.as_ref() {
            Some(member) => {
                cache.insert(member.user_id.clone(), member.clone());
            }
            None => {
                cache.remove(user_id);
            }
        }
        Ok(member)
    }

    /// Drops a member from the per-turn cache — used when a leave/kick notice
    /// arrives so later lookups in the same turn cannot resurrect them.
    pub(crate) fn forget_group_member(&self, user_id: &str) {
        self.group_member_cache.lock().unwrap().remove(user_id);
    }

    pub(crate) async fn bot_group_role(&self) -> types::BotGroupRole {
        self.adapter
            .bot_group_role()
            .await
            .unwrap_or(types::BotGroupRole::Unknown)
    }

    pub(crate) async fn delete_message(&self, message_id: &str) -> Result<()> {
        self.adapter.delete_message(message_id).await
    }

    pub(crate) async fn set_group_ban(&self, user_id: &str, duration_seconds: u64) -> Result<()> {
        self.adapter.set_group_ban(user_id, duration_seconds).await
    }

    pub(crate) async fn set_group_kick(
        &self,
        user_id: &str,
        reject_add_request: bool,
    ) -> Result<()> {
        self.adapter
            .set_group_kick(user_id, reject_add_request)
            .await
    }

    pub(crate) async fn set_group_special_title(
        &self,
        user_id: &str,
        special_title: &str,
        duration_seconds: i64,
    ) -> Result<()> {
        self.adapter
            .set_group_special_title(user_id, special_title, duration_seconds)
            .await
    }

    pub(crate) async fn record_external_bot_message(&self, message_id: &str, text: &str) {
        self.plugins
            .record_external_bot_message(self, message_id, text)
            .await;
    }

    pub(crate) fn take_final_reply_suppression(&self) -> bool {
        let suppress = self
            .pending_final_reply_suppression
            .swap(false, Ordering::AcqRel);
        self.pending_prior_reply_suppression
            .store(false, Ordering::Release);
        suppress
    }

    pub(crate) fn take_final_reply_suppression_start(&self, text_len: usize) -> Option<usize> {
        if !self
            .pending_final_reply_suppression
            .swap(false, Ordering::AcqRel)
        {
            return None;
        }
        let suppress_prior = self
            .pending_prior_reply_suppression
            .swap(false, Ordering::AcqRel);
        Some(if suppress_prior { 0 } else { text_len })
    }
}

pub(crate) fn apply_resolved_response_target(
    message: &mut OutboundMessage,
    original: &ResponseTarget,
    resolved: Option<&ResponseTarget>,
) {
    if message.response_target.as_ref() == Some(original) {
        message.response_target = resolved.cloned();
    }
}

pub(crate) fn message_is_parenthetical_only(message: &OutboundMessage) -> bool {
    let OutboundBody::Segments(segments) = &message.body else {
        return false;
    };
    let mut text = String::new();
    for segment in segments {
        match segment {
            OutboundSegment::Markdown(part) | OutboundSegment::Text(part) => text.push_str(part),
            OutboundSegment::Mention(_) => {}
            OutboundSegment::ImageBytes { .. }
            | OutboundSegment::ImagePath { .. }
            | OutboundSegment::FilePath { .. } => return false,
        }
    }
    let text = text.trim();
    if text.is_empty() || !text.starts_with('（') || !text.ends_with('）') {
        return false;
    }
    let mut depth = 0_u32;
    let mut chars = text.chars().peekable();
    while let Some(character) = chars.next() {
        match character {
            '（' => depth = depth.saturating_add(1),
            '）' => {
                let Some(next_depth) = depth.checked_sub(1) else {
                    return false;
                };
                depth = next_depth;
                if depth == 0 && chars.peek().is_some() {
                    return false;
                }
            }
            _ if depth == 0 => return false,
            _ => {}
        }
    }
    depth == 0
}

pub(crate) fn outbound_text_for_history(message: &OutboundMessage) -> String {
    pub(crate) fn append(parts: &mut Vec<String>, segments: &[OutboundSegment]) {
        for segment in segments {
            match segment {
                OutboundSegment::Markdown(text) | OutboundSegment::Text(text) => {
                    if !text.trim().is_empty() {
                        parts.push(text.clone());
                    }
                }
                OutboundSegment::Mention(user_id) => parts.push(format!("@{user_id}")),
                OutboundSegment::ImageBytes { .. }
                | OutboundSegment::ImagePath { .. }
                | OutboundSegment::FilePath { .. } => {}
            }
        }
    }

    let mut parts = Vec::new();
    match &message.body {
        OutboundBody::Segments(segments) => append(&mut parts, segments),
        OutboundBody::Forward(nodes) => {
            for node in nodes {
                append(&mut parts, &node.segments);
            }
        }
    }
    parts.join("\n").trim().to_string()
}

pub(crate) fn register_platform_tools(
    registry: &mut crate::tools::ToolRegistry,
    context: Arc<PlatformTurnContext>,
) {
    tool::register(registry, context.clone());
    context.plugins.register_tools(registry, context.clone());
}
