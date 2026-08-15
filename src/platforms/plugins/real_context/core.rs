//! core — 自 src/platforms/plugins/real_context/mod.rs 拆分。

pub(crate) use super::*;

pub(crate) const TRIGGER_KEY: &str = "real_context.trigger";
pub(crate) const MODERATION_NOTICE_KEY: &str = "real_context.moderation_notice";
pub(crate) const REPLY_MARKED_KEY: &str = "real_context.reply_marked";
pub(crate) const ACTIVE_TARGETS_KEY: &str = "real_context.active_targets";
pub(crate) const SESSION_STATE_SOFT_LIMIT: usize = 512;
pub(crate) const SESSION_STATE_IDLE_TTL: Duration = Duration::from_secs(24 * 60 * 60);
pub(crate) const PENDING_REPLY_TTL: Duration = Duration::from_secs(31 * 60);
pub(crate) const MAX_ACTIVE_TARGET_MESSAGES: usize = 8;
pub(crate) const MAX_ACTIVE_SUPPLEMENT_MESSAGES: usize = 5;
pub(crate) const MAX_ACTIVE_CURRENT_CONTENT_BYTES: usize = 16 * 1024;
pub(crate) const MAX_ACTIVE_TARGET_PROMPT_BYTES: usize = 128 * 1024;
pub(crate) const MAX_CONTEXT_IMAGE_REFS: usize = 8;
pub(crate) const REPLY_WATERMARK_KEY: &str = "reply_ingress_watermark";
/// How far back `vision_analyze` can still reach. Deliberately independent of
/// what this turn rendered: the log is incremental now, so a block usually
/// holds a handful of messages, and tying the resolvable set to it would leave
/// the model unable to open a picture it can plainly see in the replayed
/// history. Ids derive from the message they came from, so the wider sweep
/// mints exactly the ids already written down in earlier turns.
pub(crate) const CONTEXT_IMAGE_LOOKBACK_MESSAGES: usize = 200;
pub(crate) const MAX_CONTEXT_IMAGES_PER_MESSAGE: usize = 4;

pub(crate) struct RealContextPlugin {
    pub(crate) settings_cache: Mutex<
        Option<(
            Option<PlatformPluginInstanceConfig>,
            Arc<RealContextPluginSettings>,
        )>,
    >,
    pub(crate) runtime: Mutex<RuntimeState>,
    pub(crate) global_judge_gate: DynamicGate,
    pub(crate) reaction_expirations:
        Mutex<HashMap<(String, String, String), tokio::task::AbortHandle>>,
    pub(crate) affection_updates: affection::AffectionUpdateQueue,
}
