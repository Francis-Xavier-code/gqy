//! runtime — 自 src/platforms/plugins/real_context/mod.rs 拆分。

pub(crate) use super::*;

impl RuntimeState {
    pub(crate) fn session_mut(&mut self, key: &str, now: Instant) -> &mut SessionRuntime {
        let session = self
            .sessions
            .entry(key.to_string())
            .or_insert_with(|| SessionRuntime::new(now));
        session.last_touched = now;
        session
    }

    pub(crate) fn prune(&mut self, now: Instant) {
        for session in self.sessions.values_mut() {
            session
                .pending
                .retain(|_, pending| now.duration_since(pending.started) <= PENDING_REPLY_TTL);
        }
        if self.sessions.len() > SESSION_STATE_SOFT_LIMIT {
            self.sessions.retain(|_, session| {
                !session.pending.is_empty()
                    || now.duration_since(session.last_touched) <= SESSION_STATE_IDLE_TTL
            });
        }
        let removable = self.sessions.len().saturating_sub(SESSION_STATE_SOFT_LIMIT);
        if removable > 0 {
            let mut inactive = self
                .sessions
                .iter()
                .filter(|(_, session)| session.pending.is_empty())
                .map(|(key, session)| (key.clone(), session.last_touched))
                .collect::<Vec<_>>();
            inactive.sort_unstable_by_key(|(_, touched)| *touched);
            for (key, _) in inactive.into_iter().take(removable) {
                self.sessions.remove(&key);
            }
        }
    }
}

pub(crate) struct SessionRuntime {
    last_touched: Instant,
    pub(crate) last_reply: Option<Instant>,
    pub(crate) heat: f64,
    heat_updated: Instant,
    continuation: Option<Continuation>,
    pub(crate) pending: HashMap<String, PendingReply>,
}

impl SessionRuntime {
    pub(crate) fn new(now: Instant) -> Self {
        Self {
            last_touched: now,
            last_reply: None,
            heat: 0.0,
            heat_updated: now,
            continuation: None,
            pending: HashMap::new(),
        }
    }

    pub(crate) fn decay_heat(&mut self, now: Instant, recover_minutes: u64) {
        let recover = Duration::from_secs(recover_minutes.max(1) * 60).as_secs_f64();
        let elapsed = now.duration_since(self.heat_updated).as_secs_f64();
        self.heat = (self.heat - elapsed / recover).max(0.0);
        self.heat_updated = now;
    }

    pub(crate) fn increase_heat(&mut self, now: Instant, settings: &RealContextPluginSettings) {
        if !settings.reply_restraint_enable {
            return;
        }
        self.decay_heat(now, settings.reply_restraint_recover_minutes);
        self.heat += settings.reply_restraint_multiplier;
        self.heat_updated = now;
    }

    pub(crate) fn continuation_match(
        &mut self,
        sender_id: &str,
        now: Instant,
        enabled: bool,
    ) -> bool {
        if !enabled {
            self.continuation = None;
            return false;
        }
        let Some(continuation) = self.continuation.as_ref() else {
            return false;
        };
        // Only the clock and the speaker bound a continuation. There used to be
        // a turn cap as well, which cut a conversation off mid-flow purely
        // because it had gone on for a few exchanges — the window itself is
        // what expresses "we are still talking".
        if now > continuation.expires_at || continuation.user_id != sender_id {
            self.continuation = None;
            return false;
        }
        true
    }

    pub(crate) fn mark_continuation(
        &mut self,
        sender_id: &str,
        now: Instant,
        settings: &RealContextPluginSettings,
    ) {
        if !settings.continuation_enable {
            self.continuation = None;
            return;
        }
        // Every reply we actually send restarts the clock, including one the
        // continuation window itself prompted: answering inside the window is
        // exactly the evidence that the exchange is still live, so it should
        // extend the window rather than count down against it.
        self.continuation = Some(Continuation {
            user_id: sender_id.to_string(),
            expires_at: now + Duration::from_secs(settings.continuation_window_seconds),
        });
    }
}

pub(crate) struct Continuation {
    user_id: String,
    expires_at: Instant,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct ActiveReplyTarget {
    pub(crate) message_id: String,
    pub(crate) sender_id: String,
    pub(crate) sender_name: String,
    pub(crate) timestamp: i64,
    pub(crate) content: String,
    pub(crate) reply_message_id: Option<String>,
    pub(crate) reply_sender_id: Option<String>,
    pub(crate) reply_sender_name: Option<String>,
    pub(crate) reply_content: Option<String>,
    #[serde(default)]
    pub(crate) mentioned_user_ids: Vec<String>,
    #[serde(default)]
    pub(crate) mentioned_users: Vec<PlatformMention>,
    pub(crate) supplemental: bool,
}

pub(crate) struct PendingReply {
    pub(crate) generation: u64,
    pub(crate) started: Instant,
    pub(crate) trigger: TriggerKind,
    /// 回复承诺已成立(直触发,或主动判断已通过)。补救窗口内的新消息
    /// 直接顶替目标而不再重新判断;未承诺(仍在判断中)则取消旧判断、
    /// 对新消息重新判断。
    pub(crate) committed: bool,
    pub(crate) reactions: Vec<(String, String)>,
    pub(crate) targets: Vec<ActiveReplyTarget>,
    pub(crate) cancel: tokio::sync::watch::Sender<bool>,
}

pub(crate) async fn wait_for_supersede(receiver: &mut tokio::sync::watch::Receiver<bool>) {
    if *receiver.borrow() {
        return;
    }
    while receiver.changed().await.is_ok() {
        if *receiver.borrow() {
            return;
        }
    }
}

#[derive(Default)]
pub(crate) struct DynamicGate {
    pub(crate) active: AtomicUsize,
    pub(crate) notify: Notify,
}
