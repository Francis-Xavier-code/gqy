//! core — 自 src/state/mod.rs 拆分。

use super::*;

mod conversation_db;
mod migrations;
pub use migrations::DEFAULT_SESSION_ID;
pub(crate) mod usage;

/// Newest `conversation.db` schema this build can open — the gate an import
/// checks before restoring a database written by a newer GQY.
pub fn latest_schema_version() -> i64 {
    migrations::LATEST_VERSION
}

use crate::llm::{TurnTokens, Usage};
use crate::memory::EvictedTurn;
use crate::paths::GQYPaths;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::{Cursor, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, RwLock, Weak};

#[allow(unused_imports)]
pub use conversation_db::{
    interrupted_text, pending_placeholder, ArtifactAsset, ArtifactAssetData, ConversationDb,
    ImageAsset, ImageAssetData, PlatformAccessActor, PlatformAccessGrant, PlatformAccessGrantKey,
    PlatformMemeRefRecord, PlatformPluginScopeKey, PlatformSessionBinding,
    PlatformSessionBindingKey, PruneStats, QueuedPrompt, QueuedPromptAttachment, RedoCandidate,
    RedoInputKind, RedoStart, ReplayEntry, SessionOverview, SessionRecord, ToolFootprint, Turn,
    TurnFollowup, TurnReplay,
    TurnJournalEvent,
    TurnRedoCheckpointPayload, TurnStatus, UserAttachment, UserAttachmentData,
    GLOBAL_PLATFORM_ACCOUNT_SCOPE,
    ToolFlowCall, ToolFlowRound,
};
pub use usage::{UsageMeta, UsageRange, UsageSnapshot, UsageStats};

/// The only session kind users can list, name, switch to, or bind a platform
/// to. Everything else is infrastructure and stays out of the session list.
pub const USER_SESSION_KIND: &str = "user";
/// Build/Dev 模式的保留人格 scope:dev 会话全部挂在它名下,借现有
/// 按人格隔离机制白拿会话/记忆/REPL 指针的分家;模式由会话的
/// persona==DEV_PERSONA 推导,无需迁移。
pub const DEV_PERSONA: &str = "dev";
/// Backs a one-shot `gqy ask` / `gqy '<message>'` turn: created just before
/// the turn, deleted right after, and invisible to every listing in between.
pub const ASK_SESSION_KIND: &str = "ask";

type PlatformAccessSubjects = HashSet<String>;
type PlatformAccessKinds = HashMap<String, PlatformAccessSubjects>;
type PlatformAccessPermissions = HashMap<String, PlatformAccessKinds>;
type PlatformAccessScopes = HashMap<String, PlatformAccessPermissions>;

#[derive(Debug)]
pub(crate) struct SharedPlatformAccess {
    index: RwLock<PlatformAccessIndex>,
    mutations: Mutex<()>,
}

pub(crate) static PLATFORM_ACCESS_INDEXES: OnceLock<Mutex<HashMap<PathBuf, Weak<SharedPlatformAccess>>>> =
    OnceLock::new();

#[derive(Debug, Default)]
pub(crate) struct PlatformAccessIndex {
    platforms: HashMap<String, PlatformAccessScopes>,
}

impl PlatformAccessIndex {
    pub(crate) fn from_grants(grants: impl IntoIterator<Item = PlatformAccessGrant>) -> Self {
        let mut index = Self::default();
        for grant in grants {
            index.insert(&grant.key);
        }
        index
    }

    pub(crate) fn contains(
        &self,
        platform: &str,
        account_scope: &str,
        permission: &str,
        subject_kind: &str,
        subject_id: &str,
    ) -> bool {
        self.platforms
            .get(platform)
            .and_then(|scopes| scopes.get(account_scope))
            .and_then(|permissions| permissions.get(permission))
            .and_then(|kinds| kinds.get(subject_kind))
            .is_some_and(|subjects| subjects.contains(subject_id))
    }

    pub(crate) fn insert(&mut self, key: &PlatformAccessGrantKey) {
        self.platforms
            .entry(key.platform.clone())
            .or_default()
            .entry(key.account_scope.clone())
            .or_default()
            .entry(key.permission.clone())
            .or_default()
            .entry(key.subject_kind.clone())
            .or_default()
            .insert(key.subject_id.clone());
    }

    pub(crate) fn remove(&mut self, key: &PlatformAccessGrantKey) -> bool {
        if let Some(subjects) = self
            .platforms
            .get_mut(&key.platform)
            .and_then(|scopes| scopes.get_mut(&key.account_scope))
            .and_then(|permissions| permissions.get_mut(&key.permission))
            .and_then(|kinds| kinds.get_mut(&key.subject_kind))
        {
            return subjects.remove(&key.subject_id);
        }
        false
    }
}

pub(crate) fn shared_platform_access_index(
    state_dir: &Path,
    conv_db: &ConversationDb,
) -> Result<Arc<SharedPlatformAccess>> {
    let key = state_dir
        .canonicalize()
        .unwrap_or_else(|_| state_dir.to_path_buf());
    let indexes = PLATFORM_ACCESS_INDEXES.get_or_init(|| Mutex::new(HashMap::new()));
    let mut indexes = indexes.lock().unwrap();
    if let Some(index) = indexes.get(&key).and_then(Weak::upgrade) {
        return Ok(index);
    }
    indexes.retain(|_, index| index.strong_count() > 0);
    let index = Arc::new(SharedPlatformAccess {
        index: RwLock::new(PlatformAccessIndex::from_grants(
            conv_db.platform_access_grants(None)?,
        )),
        mutations: Mutex::new(()),
    });
    indexes.insert(key, Arc::downgrade(&index));
    Ok(index)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunningTurnQueueTarget {
    pub turn_id: String,
    pub queue_session_id: Option<String>,
    pub owner_pid: Option<u32>,
}

#[derive(Clone, Debug)]
pub(crate) struct PlatformAccessAuthorization {
    pub(crate) statically_authorized: bool,
    pub(crate) dynamic_key: PlatformAccessGrantKey,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PlatformAccessMutation {
    Grant,
    Revoke,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PlatformAccessMutationResult {
    Unauthorized,
    Unchanged,
    Changed,
}

#[derive(Debug, Clone)]
pub struct StateStore {
    state_dir: PathBuf,
    artifacts_dir: PathBuf,
    conv_db: Arc<ConversationDb>,
    platform_access: Arc<SharedPlatformAccess>,
    /// Active session. Shared across clones and swappable at runtime so a
    /// long-lived daemon switches every holder atomically.
    session_id: Arc<std::sync::RwLock<Arc<str>>>,
    queue_session_id: Arc<str>,
    queue_owner_pid: u32,
}

