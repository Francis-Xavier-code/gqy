use crate::i18n::text as t;
use crate::llm::{ChatMessage, TurnTokens};
use crate::memory::EvictedTurn;
use crate::question::QuestionExchange;
use anyhow::{bail, Context, Result};
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Mutex;

mod store;

mod search;

const PENDING_PLACEHOLDER: &str = "<system-reminder>上一轮prompt正在由另一轮回复处理中，你只需要回应用户当前的prompt，不要处理上一轮的prompt</system-reminder>";
const INTERRUPTED_TEXT: &str =
    "<system-reminder>上一轮prompt已被中断，除非用户重新要求否则不要处理上一轮的prompt</system-reminder>";

/// Budget for a finished turn's display transcript. Generous enough for a
/// normal turn's prose plus a handful of tool blocks, small enough that a
/// session's worth of them stays cheap to load.
const REPLAY_JOURNAL_MAX_CHARS: usize = 8 * 1024;
/// Per-entry clamp so one runaway tool result cannot eat the whole budget.
const REPLAY_ENTRY_MAX_CHARS: usize = 2 * 1024;

/// One entry of a finished turn's display transcript, in stream order.
///
/// Reconstructed from the live journal just before it is dropped, so the
/// interleaving of prose and tool blocks survives — which is the whole point,
/// since `assistant_content` alone would flatten a turn into one paragraph.
/// Command output tails are deliberately absent: they are the bulky part and
/// the settled block reads fine without them.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReplayEntry {
    Text {
        text: String,
    },
    ToolCall {
        name: String,
        #[serde(default)]
        arguments: String,
    },
    ToolResult {
        name: String,
        ok: bool,
        #[serde(default)]
        output: String,
    },
}

/// `app_state` key prefixes for the two persona-scoped session pointers. The
/// terminal lane (shell-hook, `gqy new`/`session`) and the REPL lane move
/// independently; one-shot `ask` turns use neither.
const CURRENT_SESSION_POINTER: &str = "current_session_persona";
const REPL_SESSION_POINTER: &str = "repl_session_persona";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnStatus {
    Running,
    Completed,
    Interrupted,
}

#[allow(dead_code)]
impl TurnStatus {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Interrupted => "interrupted",
        }
    }

    fn from_str(s: &str) -> Self {
        match s {
            "completed" => Self::Completed,
            "interrupted" => Self::Interrupted,
            _ => Self::Running,
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct PruneStats {
    pub turns: usize,
    pub saved_chars: usize,
}

/// Deterministic per-turn tool footprint. BTreeSet: sorted, deduplicated,
/// byte-deterministic serialization (cache-purity requirement for anything
/// that ends up in a rendered summary).
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ToolFootprint {
    #[serde(default, skip_serializing_if = "std::collections::BTreeSet::is_empty")]
    pub read: std::collections::BTreeSet<String>,
    #[serde(default, skip_serializing_if = "std::collections::BTreeSet::is_empty")]
    pub modified: std::collections::BTreeSet<String>,
    #[serde(default, skip_serializing_if = "std::collections::BTreeSet::is_empty")]
    pub memories: std::collections::BTreeSet<String>,
}

impl ToolFootprint {
    pub fn is_empty(&self) -> bool {
        self.read.is_empty() && self.modified.is_empty() && self.memories.is_empty()
    }

    pub fn merge(&mut self, other: ToolFootprint) {
        self.read.extend(other.read);
        self.modified.extend(other.modified);
        self.memories.extend(other.memories);
    }
}

/// 一轮工具调用:assistant(可带思考)发起若干 call,随后各自的结果。
/// `output` 与该轮模型实际看到的字节一致(超限时是 spill 预览),回放即重现。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ToolFlowRound {
    #[serde(default)]
    pub assistant_content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assistant_reasoning: Option<String>,
    pub calls: Vec<ToolFlowCall>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolFlowCall {
    pub id: String,
    pub name: String,
    /// 模型原样产出的 JSON 字符串,不解析不重排(dsh:字节保真)。
    pub arguments: String,
    pub output: String,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Turn {
    pub turn_id: String,
    pub seq: i64,
    pub user_content: String,
    pub display_content: String,
    pub user_timestamp: String,
    pub assistant_content: String,
    pub assistant_reasoning: Option<String>,
    pub assistant_provider_id: Option<String>,
    pub assistant_model: Option<String>,
    pub assistant_timestamp: Option<String>,
    pub status: TurnStatus,
    pub tool_reports: Vec<String>,
    /// 结构化工具流(v20+):非空时历史回放走原生 tool_calls/role:"tool" 形态,
    /// tool_reports 只服务 UI 与旧回合兜底。
    pub tool_flow: Vec<ToolFlowRound>,
    pub question_exchanges: Vec<QuestionExchange>,
    pub followups: Vec<TurnFollowup>,
    pub attachments: Vec<UserAttachment>,
    pub hidden: bool,
    pub is_summary: bool,
    pub owner_pid: Option<i64>,
    pub token_total: u64,
    /// Prompt half of the turn's usage and how much of it the provider served
    /// from cache. A hit rate needs the prompt as its denominator, not the
    /// total: output tokens only enter the prompt on the *next* turn.
    pub token_prompt: u64,
    pub token_cache_read: u64,
    pub token_usage_estimated: bool,
    pub revision: i64,
    /// Semantic events for a non-completed generation. Completed turns keep
    /// this empty so normal history loading does not materialize large logs.
    pub journal_events: Vec<TurnJournalEvent>,
    /// Fossilized transient tail (v7 append-only): the system messages that
    /// followed the user message in the live request, replayed verbatim so the
    /// provider prefix cache sees a pure extension instead of a divergence.
    pub context_messages: Vec<ChatMessage>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnJournalEvent {
    pub event_id: i64,
    pub revision: i64,
    pub segment_index: i64,
    pub kind: String,
    pub call_id: Option<String>,
    pub name: Option<String>,
    pub text_payload: Option<String>,
    pub blob_payload: Option<Vec<u8>>,
    pub ok: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnRedoCheckpointPayload {
    pub replay_messages: Vec<ChatMessage>,
    pub prefix_tool_reports: Vec<String>,
    pub tool_rounds: usize,
    pub question_rounds: usize,
    pub loaded_items: Vec<(String, String, Option<String>)>,
    pub prefix_question_count: usize,
    pub prefix_image_asset_ids: Vec<String>,
    #[serde(default)]
    pub prefix_artifact_asset_ids: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct TurnRedoCheckpoint {
    pub batch_prompt_ids: Vec<String>,
    pub payload: Option<TurnRedoCheckpointPayload>,
    pub unavailable_reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedoInputKind {
    Initial,
    Followup,
}

#[derive(Debug, Clone)]
pub struct RedoCandidate {
    pub turn_id: String,
    pub revision: i64,
    pub input_id: String,
    pub input_kind: RedoInputKind,
    pub display_content: String,
    pub batch_prompt_ids: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct RedoStart {
    pub revision: i64,
    pub checkpoint: Option<TurnRedoCheckpointPayload>,
}

#[derive(Debug, Clone)]
pub struct StaleTurnRecovery {
    pub turn_id: String,
    pub session_id: String,
    pub restored_redo: bool,
}

#[derive(Debug, Serialize, Deserialize)]
struct TurnRedoBackup {
    status: String,
    user_content: String,
    display_content: String,
    followup_content: Option<String>,
    followup_display_content: Option<String>,
    followup_context_content: Option<String>,
    assistant_content: String,
    assistant_reasoning: Option<String>,
    assistant_provider_id: Option<String>,
    assistant_model: Option<String>,
    assistant_timestamp: Option<String>,
    tool_reports: String,
    owner_pid: Option<i64>,
    queue_session_id: Option<String>,
    token_total: i64,
    #[serde(default)]
    token_prompt: i64,
    #[serde(default)]
    token_cache_read: i64,
    token_usage_estimated: i64,
    loaded_items: Vec<(String, String, Option<String>, String, String)>,
    consumed_prompt_ids: Vec<String>,
    checkpoint: Option<RedoCheckpointBackup>,
}

#[derive(Debug, Serialize, Deserialize)]
struct RedoCheckpointBackup {
    version: i64,
    batch_prompt_ids: String,
    payload: Option<Vec<u8>>,
    unavailable_reason: Option<String>,
    created_at: String,
}

const REDO_CHECKPOINT_VERSION: i64 = 1;
const MAX_REDO_CHECKPOINT_BYTES: usize = 2 * 1024 * 1024;
const MAX_JOURNAL_TEXT_EVENT_BYTES: usize = 64 * 1024 * 1024;
const MAX_JOURNAL_BLOB_EVENT_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum QueuedPromptAttachment {
    Binary { mime: String, data_base64: String },
    Path { path: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedPrompt {
    pub prompt_id: String,
    pub seq: i64,
    pub content: String,
    pub display_content: String,
    pub attachments: Vec<QueuedPromptAttachment>,
    pub uploaded_attachments: Vec<UserAttachment>,
    pub submitted_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnFollowup {
    pub prompt_id: String,
    pub content: String,
    pub display_content: String,
    pub attachments: Vec<QueuedPromptAttachment>,
    pub uploaded_attachments: Vec<UserAttachment>,
    pub submitted_at: String,
    pub preceding_assistant_content: Option<String>,
    pub preceding_assistant_reasoning: Option<String>,
    pub preceding_assistant_provider_id: Option<String>,
    pub preceding_assistant_model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserAttachment {
    pub attachment_id: String,
    pub file_name: String,
    pub mime: String,
    pub kind: String,
    pub size_bytes: u64,
    pub width: u32,
    pub height: u32,
    pub created_at: String,
}

#[derive(Debug, Clone)]
pub struct UserAttachmentData {
    pub attachment: UserAttachment,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageAsset {
    pub asset_id: String,
    pub turn_id: String,
    pub tool_id: Option<String>,
    pub mime: String,
    pub width: u32,
    pub height: u32,
    pub alt: String,
    pub created_at: String,
}

#[derive(Debug, Clone)]
pub struct ImageAssetData {
    pub asset: ImageAsset,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactAsset {
    pub asset_id: String,
    pub turn_id: String,
    pub tool_id: Option<String>,
    pub source_key: String,
    pub file_name: String,
    pub mime: String,
    pub kind: String,
    pub size_bytes: u64,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone)]
pub struct ArtifactAssetData {
    pub asset: ArtifactAsset,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRecord {
    pub session_id: String,
    pub persona: String,
    pub name: String,
    pub kind: String,
    pub parent_session_id: Option<String>,
    pub workspace: Option<String>,
    pub archived: bool,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone)]
pub struct SessionOverview {
    pub record: SessionRecord,
    pub turn_count: i64,
    pub last_user_content: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PlatformSessionBindingKey {
    pub platform: String,
    pub account_id: String,
    pub conversation_kind: String,
    pub conversation_id: String,
    pub participant_id: Option<String>,
    pub persona: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformSessionBinding {
    pub key: PlatformSessionBindingKey,
    pub session_id: String,
}

impl PlatformSessionBindingKey {
    fn normalized_participant_id(&self) -> &str {
        self.participant_id.as_deref().unwrap_or("")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PlatformPluginScopeKey {
    pub plugin_id: String,
    pub platform: String,
    pub account_id: String,
    pub conversation_kind: String,
    pub conversation_id: String,
}

/// Account scope shared by every account on one platform.
pub const GLOBAL_PLATFORM_ACCOUNT_SCOPE: &str = "*";

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PlatformAccessGrantKey {
    pub platform: String,
    pub account_scope: String,
    pub permission: String,
    pub subject_kind: String,
    pub subject_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformAccessActor {
    pub platform: String,
    pub account_id: String,
    pub user_id: String,
    pub conversation_kind: String,
    pub conversation_id: String,
    pub message_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformAccessGrant {
    pub key: PlatformAccessGrantKey,
    pub granted_by: PlatformAccessActor,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformMemeRefRecord {
    pub platform: String,
    pub account_id: String,
    pub conversation_kind: String,
    pub conversation_id: String,
    pub message_id: String,
    pub library: String,
    pub meme_id: String,
    pub direction: String,
    pub created_at: String,
}

fn insert_platform_access_audit(
    tx: &Transaction<'_>,
    operation: &str,
    key: &PlatformAccessGrantKey,
    actor: &PlatformAccessActor,
    created_at: &str,
) -> Result<()> {
    tx.execute(
        "INSERT INTO platform_access_audit (
             audit_id, operation, platform, account_scope, permission,
             subject_kind, subject_id, actor_platform, actor_account_id,
             actor_user_id, actor_conversation_kind, actor_conversation_id,
             actor_message_id, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        params![
            format!("access-audit-{:032x}", rand::random::<u128>()),
            operation,
            key.platform,
            key.account_scope,
            key.permission,
            key.subject_kind,
            key.subject_id,
            actor.platform,
            actor.account_id,
            actor.user_id,
            actor.conversation_kind,
            actor.conversation_id,
            actor.message_id,
            created_at,
        ],
    )?;
    Ok(())
}

const SESSION_COLUMNS: &str = "session_id, persona, name, kind, parent_session_id, workspace, archived, created_at, updated_at";

fn session_record_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SessionRecord> {
    Ok(SessionRecord {
        session_id: row.get("session_id")?,
        persona: row.get("persona")?,
        name: row.get("name")?,
        kind: row.get("kind")?,
        parent_session_id: row.get("parent_session_id")?,
        workspace: row.get("workspace")?,
        archived: row.get("archived")?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
    })
}

pub struct ConversationDb {
    conn: Mutex<Connection>,
}

impl std::fmt::Debug for ConversationDb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConversationDb").finish_non_exhaustive()
    }
}

impl ConversationDb {
    pub fn open(state_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(state_dir)?;
        let db_path = state_dir.join("conversation.db");
        let mut conn = Connection::open(&db_path)
            .with_context(|| format!("failed to open conversation db: {}", db_path.display()))?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA busy_timeout = 5000;
             PRAGMA foreign_keys = ON;",
        )?;
        // Back up the database file before applying schema migrations to a
        // database that already holds data.
        if super::migrations::current_version(&conn)? < super::migrations::LATEST_VERSION {
            let has_turns: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='turns')",
                [],
                |row| row.get(0),
            )?;
            if has_turns {
                let _ = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()));
                let _ = std::fs::copy(&db_path, state_dir.join("conversation.db.bak"));
            }
        }
        super::migrations::run_migrations(&mut conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Resolves the current session pointer from `app_state`, self-healing a
    /// missing pointer or dangling session row back to the default session.
    pub fn resolve_current_session(&self) -> Result<String> {
        let conn = self.conn.lock().unwrap();
        let pointer: Option<String> = conn
            .query_row(
                "SELECT value FROM app_state WHERE key = 'current_session'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(session_id) = pointer {
            let exists: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM sessions WHERE session_id = ?1)",
                params![session_id],
                |row| row.get(0),
            )?;
            if exists {
                return Ok(session_id);
            }
        }
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT OR IGNORE INTO sessions (session_id, persona, name, kind, created_at, updated_at)
             VALUES (?1, '', ?2, 'user', ?3, ?3)",
            params![
                super::migrations::DEFAULT_SESSION_ID,
                t("Terminal session", "终端集成会话"),
                now
            ],
        )?;
        conn.execute(
            "INSERT INTO app_state (key, value) VALUES ('current_session', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![super::migrations::DEFAULT_SESSION_ID],
        )?;
        Ok(super::migrations::DEFAULT_SESSION_ID.to_string())
    }

    /// Persists the current-session pointer. The target session must exist.
    pub fn set_current_session(&self, session_id: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let exists: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sessions WHERE session_id = ?1)",
            params![session_id],
            |row| row.get(0),
        )?;
        if !exists {
            bail!("session not found: {session_id}");
        }
        conn.execute(
            "INSERT INTO app_state (key, value) VALUES ('current_session', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![session_id],
        )?;
        Ok(())
    }

    /// Reads a persona-scoped session pointer, returning `None` when it points
    /// at something the caller must not land on (wrong persona, non-user kind,
    /// archived, or already deleted). Callers fall back and heal the pointer.
    fn persona_session_pointer(&self, prefix: &str, persona: &str) -> Result<Option<String>> {
        let key = format!("{prefix}:{persona}");
        let conn = self.conn.lock().unwrap();
        let session_id = conn
            .query_row(
                "SELECT value FROM app_state WHERE key = ?1",
                params![key],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        let Some(session_id) = session_id else {
            return Ok(None);
        };
        let valid = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sessions WHERE session_id = ?1 AND persona = ?2 AND kind = ?3)",
                params![session_id, persona, super::USER_SESSION_KIND],
                |row| row.get::<_, bool>(0),
            )?;
        Ok(valid.then_some(session_id))
    }

    fn set_persona_session_pointer(
        &self,
        prefix: &str,
        persona: &str,
        session_id: &str,
    ) -> Result<()> {
        let key = format!("{prefix}:{persona}");
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO app_state (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, session_id],
        )?;
        Ok(())
    }

    pub fn persona_current_session(&self, persona: &str) -> Result<Option<String>> {
        self.persona_session_pointer(CURRENT_SESSION_POINTER, persona)
    }

    pub fn set_persona_current_session(&self, persona: &str, session_id: &str) -> Result<()> {
        self.set_persona_session_pointer(CURRENT_SESSION_POINTER, persona, session_id)
    }

    /// The REPL's own lane. Kept apart from the current-session pointer so a
    /// REPL reopens where it left off while shell-hook keeps using the
    /// terminal session it was on.
    pub fn repl_session(&self, persona: &str) -> Result<Option<String>> {
        self.persona_session_pointer(REPL_SESSION_POINTER, persona)
    }

    pub fn set_repl_session(&self, persona: &str, session_id: &str) -> Result<()> {
        self.set_persona_session_pointer(REPL_SESSION_POINTER, persona, session_id)
    }

    /// Claims persona-less sessions (schema-v2 migrated rows) for the given
    /// persona scope. Called once at daemon startup with the active persona.
    pub fn adopt_sessions_for_persona(&self, persona: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE sessions SET persona = ?1 WHERE persona = ''",
            params![persona],
        )?;
        Ok(())
    }

    pub fn rename_persona_scope(&self, old_scope: &str, new_scope: &str) -> Result<()> {
        if old_scope == new_scope {
            return Ok(());
        }
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let target_exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM sessions WHERE persona = ?1)",
            params![new_scope],
            |row| row.get(0),
        )?;
        if target_exists {
            bail!("persona scope already has sessions: {new_scope}");
        }
        let old_key = format!("current_session_persona:{old_scope}");
        let new_key = format!("current_session_persona:{new_scope}");
        let target_pointer_exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM app_state WHERE key = ?1)",
            params![new_key],
            |row| row.get(0),
        )?;
        if target_pointer_exists {
            bail!("persona scope already has a current-session pointer: {new_scope}");
        }
        let old_affection_key = format!("affection_profile:{old_scope}");
        let new_affection_key = format!("affection_profile:{new_scope}");
        let target_affection_exists: bool = tx.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM platform_plugin_kv
                 WHERE plugin_id = 'real_context' AND key = ?1
             )",
            params![new_affection_key],
            |row| row.get(0),
        )?;
        if target_affection_exists {
            bail!("persona scope already has affection state: {new_scope}");
        }

        tx.execute(
            "UPDATE platform_session_bindings SET persona = ?2 WHERE persona = ?1",
            params![old_scope, new_scope],
        )?;
        tx.execute(
            "UPDATE sessions SET persona = ?2 WHERE persona = ?1",
            params![old_scope, new_scope],
        )?;
        tx.execute(
            "UPDATE app_state SET key = ?2 WHERE key = ?1",
            params![old_key, new_key],
        )?;
        tx.execute(
            "UPDATE platform_plugin_kv SET key = ?2
              WHERE plugin_id = 'real_context' AND key = ?1",
            params![old_affection_key, new_affection_key],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn delete_persona_scope(&self, scope: &str) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute("DELETE FROM sessions WHERE persona = ?1", params![scope])?;
        tx.execute(
            "DELETE FROM app_state WHERE key = ?1",
            params![format!("current_session_persona:{scope}")],
        )?;
        tx.execute(
            "DELETE FROM platform_plugin_kv
              WHERE plugin_id = 'real_context' AND key = ?1",
            params![format!("affection_profile:{scope}")],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn session_record(&self, session_id: &str) -> Result<Option<SessionRecord>> {
        let conn = self.conn.lock().unwrap();
        Ok(conn
            .query_row(
                &format!("SELECT {SESSION_COLUMNS} FROM sessions WHERE session_id = ?1"),
                params![session_id],
                session_record_from_row,
            )
            .optional()?)
    }

    /// User-facing sessions of a persona, most recently updated first.
    /// Subagent sessions (`kind != 'user'`) are excluded.
    pub fn list_sessions(&self, persona: &str) -> Result<Vec<SessionOverview>> {
        self.list_sessions_filtered(persona, false)
    }

    /// Local user sessions suitable for CLI/WebUI navigation. Sessions
    /// owned by a messaging-platform binding keep their history but are not
    /// exposed as local conversations.
    pub fn list_local_sessions(&self, persona: &str) -> Result<Vec<SessionOverview>> {
        self.list_sessions_filtered(persona, true)
    }

    fn list_sessions_filtered(
        &self,
        persona: &str,
        local_only: bool,
    ) -> Result<Vec<SessionOverview>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(&format!(
            "SELECT {SESSION_COLUMNS},
                    (SELECT count(*) FROM turns
                      WHERE turns.session_id = sessions.session_id
                        AND hidden = 0 AND is_summary = 0) AS turn_count,
                    (SELECT display_content FROM turns
                      WHERE turns.session_id = sessions.session_id
                        AND hidden = 0 AND is_summary = 0
                      ORDER BY seq DESC LIMIT 1) AS last_user_content
             FROM sessions
             WHERE persona = ?1 AND kind = 'user'
               AND (?2 = 0 OR NOT EXISTS (
                    SELECT 1 FROM platform_session_bindings
                    WHERE platform_session_bindings.session_id = sessions.session_id
               ))
             ORDER BY updated_at DESC"
        ))?;
        let rows = stmt.query_map(params![persona, local_only], |row| {
            Ok(SessionOverview {
                record: session_record_from_row(row)?,
                turn_count: row.get("turn_count")?,
                last_user_content: row.get("last_user_content")?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// 最老可见轮的用户时间戳(排除指定回合;Utc RFC3339)。联想自回声
    /// 过滤用它当"仍在眼前"的下界:被 compact 藏起的轮不算。
    pub fn oldest_visible_turn_timestamp(
        &self,
        session_id: &str,
        excluding_turn_id: &str,
    ) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        Ok(conn.query_row(
            "SELECT MIN(user_timestamp) FROM turns
              WHERE session_id = ?1 AND hidden = 0 AND is_summary = 0 AND turn_id != ?2",
            params![session_id, excluding_turn_id],
            |row| row.get::<_, Option<String>>(0),
        )?)
    }

    pub fn is_platform_session(&self, session_id: &str) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        Ok(conn.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM platform_session_bindings WHERE session_id = ?1
            )",
            params![session_id],
            |row| row.get(0),
        )?)
    }

    pub fn persona_reset_session_ids(&self, persona: &str, platform: &str) -> Result<Vec<String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "WITH RECURSIVE targets(session_id) AS (
                 SELECT sessions.session_id
                   FROM sessions
                  WHERE sessions.persona = ?1
                    AND sessions.kind = 'user'
                    AND (
                        (NOT EXISTS (
                            SELECT 1 FROM platform_session_bindings
                             WHERE platform_session_bindings.session_id = sessions.session_id
                        ))
                        OR EXISTS (
                            SELECT 1 FROM platform_session_bindings
                             WHERE platform_session_bindings.session_id = sessions.session_id
                               AND platform_session_bindings.platform = ?2
                        )
                    )
                 UNION
                 SELECT child.session_id
                   FROM sessions child
                   JOIN targets parent ON child.parent_session_id = parent.session_id
                  WHERE child.persona = ?1
             )
             SELECT session_id FROM targets ORDER BY session_id",
        )?;
        let rows = stmt.query_map(params![persona, platform], |row| row.get(0))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn platform_session_bindings(
        &self,
        persona: &str,
        platform: &str,
    ) -> Result<Vec<PlatformSessionBinding>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT platform, account_id, conversation_kind, conversation_id,
                    participant_id, persona, session_id
               FROM platform_session_bindings
              WHERE persona = ?1 AND platform = ?2
              ORDER BY account_id, conversation_kind, conversation_id, participant_id",
        )?;
        let rows = stmt.query_map(params![persona, platform], |row| {
            let participant_id: String = row.get(4)?;
            Ok(PlatformSessionBinding {
                key: PlatformSessionBindingKey {
                    platform: row.get(0)?,
                    account_id: row.get(1)?,
                    conversation_kind: row.get(2)?,
                    conversation_id: row.get(3)?,
                    participant_id: (!participant_id.is_empty()).then_some(participant_id),
                    persona: row.get(5)?,
                },
                session_id: row.get(6)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn create_session(
        &self,
        persona: &str,
        name: &str,
        kind: &str,
        parent_session_id: Option<&str>,
    ) -> Result<SessionRecord> {
        let conn = self.conn.lock().unwrap();
        let now = Utc::now().to_rfc3339();
        let session_id = format!(
            "sess_{}_{:08x}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_millis())
                .unwrap_or(0),
            rand::random::<u32>()
        );
        conn.execute(
            "INSERT INTO sessions (session_id, persona, name, kind, parent_session_id, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
            params![session_id, persona, name, kind, parent_session_id, now],
        )?;
        drop(conn);
        Ok(self
            .session_record(&session_id)?
            .expect("session row just inserted"))
    }

    pub fn create_or_get_platform_session(
        &self,
        key: &PlatformSessionBindingKey,
        name: &str,
    ) -> Result<(SessionRecord, bool)> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(session_id) = tx
            .query_row(
                "SELECT session_id FROM platform_session_bindings
                 WHERE platform = ?1 AND account_id = ?2
                   AND conversation_kind = ?3 AND conversation_id = ?4
                   AND participant_id = ?5 AND persona = ?6",
                params![
                    key.platform,
                    key.account_id,
                    key.conversation_kind,
                    key.conversation_id,
                    key.normalized_participant_id(),
                    key.persona,
                ],
                |row| row.get::<_, String>(0),
            )
            .optional()?
        {
            let record = tx.query_row(
                &format!("SELECT {SESSION_COLUMNS} FROM sessions WHERE session_id = ?1"),
                params![session_id],
                session_record_from_row,
            )?;
            tx.commit()?;
            return Ok((record, false));
        }

        let now = Utc::now().to_rfc3339();
        let session_id = format!(
            "sess_{}_{:08x}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_millis())
                .unwrap_or(0),
            rand::random::<u32>()
        );
        tx.execute(
            "INSERT INTO sessions (session_id, persona, name, kind, created_at, updated_at)
             VALUES (?1, ?2, ?3, 'user', ?4, ?4)",
            params![session_id, key.persona, name, now],
        )?;
        tx.execute(
            "INSERT INTO platform_session_bindings (
                platform, account_id, conversation_kind, conversation_id,
                participant_id, persona, session_id, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)",
            params![
                key.platform,
                key.account_id,
                key.conversation_kind,
                key.conversation_id,
                key.normalized_participant_id(),
                key.persona,
                session_id,
                now,
            ],
        )?;
        let record = SessionRecord {
            session_id,
            persona: key.persona.clone(),
            name: name.to_string(),
            kind: "user".to_string(),
            parent_session_id: None,
            workspace: None,
            archived: false,
            created_at: now.clone(),
            updated_at: now,
        };
        tx.commit()?;
        Ok((record, true))
    }

    pub fn rename_session(&self, session_id: &str, name: &str) -> Result<()> {
        self.update_session_field(session_id, "name", Some(name))
    }

    pub fn set_session_workspace(&self, session_id: &str, workspace: Option<&str>) -> Result<()> {
        self.update_session_field(session_id, "workspace", workspace)
    }

    /// JSON-encoded per-session model pool override
    /// (`[{"provider_id": ..., "model": ...}, ...]`); None follows the global
    /// active pool.
    pub fn session_model_override(&self, session_id: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        let value = conn
            .query_row(
                "SELECT model_override FROM sessions WHERE session_id = ?1",
                params![session_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?;
        Ok(value.flatten())
    }

    pub fn set_session_model_override(&self, session_id: &str, value: Option<&str>) -> Result<()> {
        self.update_session_field(session_id, "model_override", value)
    }

    pub fn delete_session(&self, session_id: &str) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        // queued_prompts gained session_id through an ALTER TABLE migration,
        // so existing databases cannot rely on an ON DELETE foreign key.
        tx.execute(
            "DELETE FROM queued_prompts WHERE session_id = ?1",
            params![session_id],
        )?;
        let deleted = tx.execute(
            "DELETE FROM sessions WHERE session_id = ?1",
            params![session_id],
        )?;
        if deleted == 0 {
            bail!("session not found: {session_id}");
        }
        tx.commit()?;
        Ok(())
    }

    pub fn touch_session(&self, session_id: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE sessions SET updated_at = ?2 WHERE session_id = ?1",
            params![session_id, Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }

    pub fn find_session_by_name(&self, persona: &str, name: &str) -> Result<Option<SessionRecord>> {
        self.find_session_by_name_filtered(persona, name, false)
    }

    pub fn find_local_session_by_name(
        &self,
        persona: &str,
        name: &str,
    ) -> Result<Option<SessionRecord>> {
        self.find_session_by_name_filtered(persona, name, true)
    }

    fn find_session_by_name_filtered(
        &self,
        persona: &str,
        name: &str,
        local_only: bool,
    ) -> Result<Option<SessionRecord>> {
        let conn = self.conn.lock().unwrap();
        Ok(conn
            .query_row(
                &format!(
                    "SELECT {SESSION_COLUMNS} FROM sessions
                      WHERE persona = ?1 AND kind = 'user' AND name = ?2 COLLATE NOCASE
                        AND (?3 = 0 OR NOT EXISTS (
                            SELECT 1 FROM platform_session_bindings
                             WHERE platform_session_bindings.session_id = sessions.session_id
                        ))
                      ORDER BY archived ASC, updated_at DESC LIMIT 1"
                ),
                params![persona, name, local_only],
                session_record_from_row,
            )
            .optional()?)
    }

    pub fn find_platform_session_binding(
        &self,
        key: &PlatformSessionBindingKey,
    ) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        Ok(conn
            .query_row(
                "SELECT session_id FROM platform_session_bindings
                 WHERE platform = ?1 AND account_id = ?2
                   AND conversation_kind = ?3 AND conversation_id = ?4
                   AND participant_id = ?5 AND persona = ?6",
                params![
                    key.platform,
                    key.account_id,
                    key.conversation_kind,
                    key.conversation_id,
                    key.normalized_participant_id(),
                    key.persona,
                ],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// Binds an external conversation identity to a session in one immediate
    /// transaction. A key may be reassigned, but a session already owned by a
    /// different key is never stolen.
    pub fn bind_platform_session(
        &self,
        key: &PlatformSessionBindingKey,
        session_id: &str,
    ) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let session_exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM sessions WHERE session_id = ?1)",
            params![session_id],
            |row| row.get(0),
        )?;
        if !session_exists {
            bail!("session not found: {session_id}");
        }

        let owned_by_another_key: bool = tx.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM platform_session_bindings
                 WHERE session_id = ?7
                   AND NOT (
                       platform = ?1 AND account_id = ?2
                       AND conversation_kind = ?3 AND conversation_id = ?4
                       AND participant_id = ?5 AND persona = ?6
                   )
             )",
            params![
                key.platform,
                key.account_id,
                key.conversation_kind,
                key.conversation_id,
                key.normalized_participant_id(),
                key.persona,
                session_id,
            ],
            |row| row.get(0),
        )?;
        if owned_by_another_key {
            bail!("session is already bound to another platform conversation: {session_id}");
        }

        let now = Utc::now().to_rfc3339();
        tx.execute(
            "INSERT INTO platform_session_bindings (
                platform, account_id, conversation_kind, conversation_id,
                participant_id, persona, session_id, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)
             ON CONFLICT (
                platform, account_id, conversation_kind, conversation_id,
                participant_id, persona
             ) DO UPDATE SET
                session_id = excluded.session_id,
                updated_at = excluded.updated_at",
            params![
                key.platform,
                key.account_id,
                key.conversation_kind,
                key.conversation_id,
                key.normalized_participant_id(),
                key.persona,
                session_id,
                now,
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Claims an unbound external key without replacing an existing binding.
    /// Returns the winning session id so concurrent first messages converge
    /// on one history instead of creating two active sessions.
    pub fn claim_platform_session(
        &self,
        key: &PlatformSessionBindingKey,
        candidate_session_id: &str,
    ) -> Result<String> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(existing) = tx
            .query_row(
                "SELECT session_id FROM platform_session_bindings
                 WHERE platform = ?1 AND account_id = ?2
                   AND conversation_kind = ?3 AND conversation_id = ?4
                   AND participant_id = ?5 AND persona = ?6",
                params![
                    key.platform,
                    key.account_id,
                    key.conversation_kind,
                    key.conversation_id,
                    key.normalized_participant_id(),
                    key.persona,
                ],
                |row| row.get::<_, String>(0),
            )
            .optional()?
        {
            tx.commit()?;
            return Ok(existing);
        }
        let session_exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM sessions WHERE session_id = ?1)",
            params![candidate_session_id],
            |row| row.get(0),
        )?;
        if !session_exists {
            bail!("session not found: {candidate_session_id}");
        }
        let already_owned: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM platform_session_bindings WHERE session_id = ?1)",
            params![candidate_session_id],
            |row| row.get(0),
        )?;
        if already_owned {
            bail!(
                "session is already bound to another platform conversation: {candidate_session_id}"
            );
        }
        let now = Utc::now().to_rfc3339();
        tx.execute(
            "INSERT INTO platform_session_bindings (
                platform, account_id, conversation_kind, conversation_id,
                participant_id, persona, session_id, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)",
            params![
                key.platform,
                key.account_id,
                key.conversation_kind,
                key.conversation_id,
                key.normalized_participant_id(),
                key.persona,
                candidate_session_id,
                now,
            ],
        )?;
        tx.commit()?;
        Ok(candidate_session_id.to_string())
    }

    pub fn unbind_platform_session(&self, key: &PlatformSessionBindingKey) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        let deleted = conn.execute(
            "DELETE FROM platform_session_bindings
             WHERE platform = ?1 AND account_id = ?2
               AND conversation_kind = ?3 AND conversation_id = ?4
               AND participant_id = ?5 AND persona = ?6",
            params![
                key.platform,
                key.account_id,
                key.conversation_kind,
                key.conversation_id,
                key.normalized_participant_id(),
                key.persona,
            ],
        )?;
        Ok(deleted != 0)
    }

    pub fn platform_access_grants(
        &self,
        platform: Option<&str>,
    ) -> Result<Vec<PlatformAccessGrant>> {
        let conn = self.conn.lock().unwrap();
        let mut statement = conn.prepare(
            "SELECT
                 platform, account_scope, permission, subject_kind, subject_id,
                 granted_by_platform, granted_by_account_id, granted_by_user_id,
                 granted_conversation_kind, granted_conversation_id,
                 granted_message_id, created_at
             FROM platform_access_grants
             WHERE (?1 IS NULL OR platform = ?1)
             ORDER BY platform, account_scope, permission, subject_kind, subject_id",
        )?;
        let rows = statement.query_map(params![platform], |row| {
            Ok(PlatformAccessGrant {
                key: PlatformAccessGrantKey {
                    platform: row.get("platform")?,
                    account_scope: row.get("account_scope")?,
                    permission: row.get("permission")?,
                    subject_kind: row.get("subject_kind")?,
                    subject_id: row.get("subject_id")?,
                },
                granted_by: PlatformAccessActor {
                    platform: row.get("granted_by_platform")?,
                    account_id: row.get("granted_by_account_id")?,
                    user_id: row.get("granted_by_user_id")?,
                    conversation_kind: row.get("granted_conversation_kind")?,
                    conversation_id: row.get("granted_conversation_id")?,
                    message_id: row.get("granted_message_id")?,
                },
                created_at: row.get("created_at")?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn add_platform_access_grant(
        &self,
        key: &PlatformAccessGrantKey,
        actor: &PlatformAccessActor,
    ) -> Result<bool> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let created_at = Utc::now().to_rfc3339();
        let inserted = tx.execute(
            "INSERT INTO platform_access_grants (
                 platform, account_scope, permission, subject_kind, subject_id,
                 granted_by_platform, granted_by_account_id, granted_by_user_id,
                 granted_conversation_kind, granted_conversation_id,
                 granted_message_id, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             ON CONFLICT (
                 platform, account_scope, permission, subject_kind, subject_id
             ) DO NOTHING",
            params![
                key.platform,
                key.account_scope,
                key.permission,
                key.subject_kind,
                key.subject_id,
                actor.platform,
                actor.account_id,
                actor.user_id,
                actor.conversation_kind,
                actor.conversation_id,
                actor.message_id,
                created_at,
            ],
        )?;
        if inserted != 0 {
            insert_platform_access_audit(&tx, "grant", key, actor, &created_at)?;
        }
        tx.commit()?;
        Ok(inserted != 0)
    }

    pub fn remove_platform_access_grant(
        &self,
        key: &PlatformAccessGrantKey,
        actor: &PlatformAccessActor,
    ) -> Result<bool> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let deleted = tx.execute(
            "DELETE FROM platform_access_grants
             WHERE platform = ?1 AND account_scope = ?2 AND permission = ?3
               AND subject_kind = ?4 AND subject_id = ?5",
            params![
                key.platform,
                key.account_scope,
                key.permission,
                key.subject_kind,
                key.subject_id,
            ],
        )?;
        if deleted != 0 {
            let created_at = Utc::now().to_rfc3339();
            insert_platform_access_audit(&tx, "revoke", key, actor, &created_at)?;
        }
        tx.commit()?;
        Ok(deleted != 0)
    }

    pub fn plugin_get_json<T: DeserializeOwned>(
        &self,
        scope: &PlatformPluginScopeKey,
        key: &str,
    ) -> Result<Option<T>> {
        let conn = self.conn.lock().unwrap();
        let value_json = conn
            .query_row(
                "SELECT value_json FROM platform_plugin_kv
                 WHERE plugin_id = ?1 AND platform = ?2 AND account_id = ?3
                   AND conversation_kind = ?4 AND conversation_id = ?5 AND key = ?6",
                params![
                    scope.plugin_id,
                    scope.platform,
                    scope.account_id,
                    scope.conversation_kind,
                    scope.conversation_id,
                    key,
                ],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        drop(conn);
        value_json
            .map(|value| serde_json::from_str(&value).context("invalid platform plugin JSON state"))
            .transpose()
    }

    pub fn plugin_json_revision(
        &self,
        scope: &PlatformPluginScopeKey,
        key: &str,
    ) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT updated_at FROM platform_plugin_kv
             WHERE plugin_id = ?1 AND platform = ?2 AND account_id = ?3
               AND conversation_kind = ?4 AND conversation_id = ?5 AND key = ?6",
            params![
                scope.plugin_id,
                scope.platform,
                scope.account_id,
                scope.conversation_kind,
                scope.conversation_id,
                key,
            ],
            |row| row.get(0),
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn plugin_get_json_with_revision<T: DeserializeOwned>(
        &self,
        scope: &PlatformPluginScopeKey,
        key: &str,
    ) -> Result<Option<(T, String)>> {
        let conn = self.conn.lock().unwrap();
        let row = conn
            .query_row(
                "SELECT value_json, updated_at FROM platform_plugin_kv
                 WHERE plugin_id = ?1 AND platform = ?2 AND account_id = ?3
                   AND conversation_kind = ?4 AND conversation_id = ?5 AND key = ?6",
                params![
                    scope.plugin_id,
                    scope.platform,
                    scope.account_id,
                    scope.conversation_kind,
                    scope.conversation_id,
                    key,
                ],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        drop(conn);
        row.map(|(value, revision)| {
            serde_json::from_str(&value)
                .context("invalid platform plugin JSON state")
                .map(|value| (value, revision))
        })
        .transpose()
    }

    pub fn plugin_put_json<T: Serialize + ?Sized>(
        &self,
        scope: &PlatformPluginScopeKey,
        key: &str,
        value: &T,
    ) -> Result<()> {
        let value_json =
            serde_json::to_string(value).context("failed to serialize platform plugin state")?;
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO platform_plugin_kv (
                plugin_id, platform, account_id, conversation_kind,
                conversation_id, key, value_json, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT (
                plugin_id, platform, account_id, conversation_kind,
                conversation_id, key
             ) DO UPDATE SET
                value_json = excluded.value_json,
                updated_at = excluded.updated_at",
            params![
                scope.plugin_id,
                scope.platform,
                scope.account_id,
                scope.conversation_kind,
                scope.conversation_id,
                key,
                value_json,
                Utc::now().to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    /// Atomically replaces one plugin value. Returning `None` deletes it.
    /// The callback runs inside an immediate transaction and must not re-enter
    /// this database connection.
    pub fn plugin_update_json<T, F>(
        &self,
        scope: &PlatformPluginScopeKey,
        key: &str,
        update: F,
    ) -> Result<Option<T>>
    where
        T: DeserializeOwned + Serialize,
        F: FnOnce(Option<T>) -> Result<Option<T>>,
    {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let value_json = tx
            .query_row(
                "SELECT value_json FROM platform_plugin_kv
                 WHERE plugin_id = ?1 AND platform = ?2 AND account_id = ?3
                   AND conversation_kind = ?4 AND conversation_id = ?5 AND key = ?6",
                params![
                    scope.plugin_id,
                    scope.platform,
                    scope.account_id,
                    scope.conversation_kind,
                    scope.conversation_id,
                    key,
                ],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        let current = value_json
            .map(|value| serde_json::from_str(&value).context("invalid platform plugin JSON state"))
            .transpose()?;
        let current_json = current
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .context("failed to serialize platform plugin state")?;
        let next = update(current)?;
        let next_json = next
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .context("failed to serialize platform plugin state")?;
        if next_json == current_json {
            tx.commit()?;
            return Ok(next);
        }
        if let Some(value_json) = next_json {
            tx.execute(
                "INSERT INTO platform_plugin_kv (
                    plugin_id, platform, account_id, conversation_kind,
                    conversation_id, key, value_json, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT (
                    plugin_id, platform, account_id, conversation_kind,
                    conversation_id, key
                 ) DO UPDATE SET
                    value_json = excluded.value_json,
                    updated_at = excluded.updated_at",
                params![
                    scope.plugin_id,
                    scope.platform,
                    scope.account_id,
                    scope.conversation_kind,
                    scope.conversation_id,
                    key,
                    value_json,
                    Utc::now().to_rfc3339(),
                ],
            )?;
        } else {
            tx.execute(
                "DELETE FROM platform_plugin_kv
                 WHERE plugin_id = ?1 AND platform = ?2 AND account_id = ?3
                   AND conversation_kind = ?4 AND conversation_id = ?5 AND key = ?6",
                params![
                    scope.plugin_id,
                    scope.platform,
                    scope.account_id,
                    scope.conversation_kind,
                    scope.conversation_id,
                    key,
                ],
            )?;
        }
        tx.commit()?;
        Ok(next)
    }

    pub fn plugin_delete_key(&self, scope: &PlatformPluginScopeKey, key: &str) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        let deleted = conn.execute(
            "DELETE FROM platform_plugin_kv
             WHERE plugin_id = ?1 AND platform = ?2 AND account_id = ?3
               AND conversation_kind = ?4 AND conversation_id = ?5 AND key = ?6",
            params![
                scope.plugin_id,
                scope.platform,
                scope.account_id,
                scope.conversation_kind,
                scope.conversation_id,
                key,
            ],
        )?;
        Ok(deleted != 0)
    }

    pub fn plugin_delete_scope(&self, scope: &PlatformPluginScopeKey) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        Ok(conn.execute(
            "DELETE FROM platform_plugin_kv
             WHERE plugin_id = ?1 AND platform = ?2 AND account_id = ?3
               AND conversation_kind = ?4 AND conversation_id = ?5",
            params![
                scope.plugin_id,
                scope.platform,
                scope.account_id,
                scope.conversation_kind,
                scope.conversation_id,
            ],
        )?)
    }
}

fn delete_visible_turns_in_transaction(
    tx: &Transaction<'_>,
    session_id: &str,
    turn_ids: &[String],
) -> Result<usize> {
    let mut affected = 0usize;
    for turn_id in turn_ids {
        let deleted = tx.execute(
            "DELETE FROM turns
             WHERE turn_id = ?1 AND session_id = ?2 AND hidden = 0 AND is_summary = 0
               AND status != 'running'",
            params![turn_id, session_id],
        )?;
        if deleted != 1 {
            bail!(
                "{}",
                t(
                    "conversation changed before popped turns could be deleted",
                    "删除弹出轮次前会话已发生变化"
                )
            );
        }
        tx.execute(
            "DELETE FROM session_loaded_items
             WHERE session_id = ?1 AND source_turn_id = ?2",
            params![session_id, turn_id],
        )?;
        affected += deleted;
    }
    Ok(affected)
}

fn verify_loaded_tool_sources(
    tx: &Transaction<'_>,
    session_id: &str,
    expected: Option<&[(String, Option<String>)]>,
) -> Result<()> {
    let Some(expected) = expected else {
        return Ok(());
    };
    let current = {
        let mut stmt = tx.prepare(
            "SELECT name, source_turn_id FROM session_loaded_items
             WHERE session_id = ?1 AND kind = 'tool' ORDER BY name ASC",
        )?;
        let rows = stmt
            .query_map(params![session_id], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<std::result::Result<Vec<(String, Option<String>)>, _>>()?;
        rows
    };
    if current != expected {
        bail!(
            "{}",
            t(
                "dynamic tool state changed while popping context",
                "弹出上下文时动态工具状态已发生变化"
            )
        );
    }
    Ok(())
}

#[allow(dead_code)]
fn turn_chars(turn: &Turn) -> usize {
    turn.user_content.chars().count()
        + turn.assistant_content.chars().count()
        + turn
            .assistant_reasoning
            .as_deref()
            .map(str::chars)
            .map(Iterator::count)
            .unwrap_or(0)
        + turn
            .tool_reports
            .iter()
            .map(|r| r.chars().count())
            .sum::<usize>()
        + turn
            .question_exchanges
            .iter()
            .filter_map(|exchange| serde_json::to_string(exchange).ok())
            .map(|exchange| exchange.chars().count())
            .sum::<usize>()
        + turn
            .followups
            .iter()
            .map(|followup| {
                followup.content.chars().count()
                    + followup
                        .preceding_assistant_content
                        .as_deref()
                        .map(str::chars)
                        .map(Iterator::count)
                        .unwrap_or(0)
                    + followup
                        .preceding_assistant_reasoning
                        .as_deref()
                        .map(str::chars)
                        .map(Iterator::count)
                        .unwrap_or(0)
            })
            .sum::<usize>()
}

fn load_redo_checkpoint_locked(
    conn: &Connection,
    turn_id: &str,
) -> Result<Option<TurnRedoCheckpoint>> {
    conn.query_row(
        "SELECT version, batch_prompt_ids, payload, unavailable_reason
         FROM turn_redo_checkpoints WHERE turn_id = ?1",
        params![turn_id],
        |row| {
            let version = row.get::<_, i64>(0)?;
            let batch_prompt_ids =
                serde_json::from_str::<Vec<String>>(&row.get::<_, String>(1)?).unwrap_or_default();
            let payload = row
                .get::<_, Option<Vec<u8>>>(2)?
                .and_then(|payload| serde_json::from_slice(&payload).ok());
            let unavailable_reason = if version == REDO_CHECKPOINT_VERSION {
                row.get(3)?
            } else {
                Some(format!("unsupported redo checkpoint version: {version}"))
            };
            Ok(TurnRedoCheckpoint {
                batch_prompt_ids,
                payload: (version == REDO_CHECKPOINT_VERSION)
                    .then_some(payload)
                    .flatten(),
                unavailable_reason,
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

fn consume_stale_queued_prompts_locked(
    tx: &Transaction<'_>,
    turn_id: &str,
    revision: i64,
    queue_session_id: Option<&str>,
    now: &str,
) -> Result<usize> {
    let Some(queue_session_id) = queue_session_id else {
        return Ok(0);
    };
    let prompts = {
        let mut stmt = tx.prepare(
            "SELECT prompt_id, content FROM queued_prompts
             WHERE status = 'queued' AND queue_session_id = ?1
             ORDER BY seq",
        )?;
        let rows = stmt
            .query_map(params![queue_session_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        rows
    };
    if prompts.is_empty() {
        return Ok(0);
    }

    tx.execute(
        "INSERT OR IGNORE INTO turn_journal_segments
            (turn_id, revision, segment_index, status, started_at)
         VALUES (?1, ?2, 0, 'running', ?3)",
        params![turn_id, revision, now],
    )?;
    let (segment_index, segment_status): (i64, String) = tx.query_row(
        "SELECT segment_index, status FROM turn_journal_segments
         WHERE turn_id = ?1 AND revision = ?2
         ORDER BY segment_index DESC LIMIT 1",
        params![turn_id, revision],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let (preceding_content, preceding_reasoning) = if segment_status == "running" {
        journal_segment_projection_locked(tx, turn_id, revision, segment_index)?
    } else {
        (String::new(), None)
    };

    for (index, (prompt_id, content)) in prompts.iter().enumerate() {
        let affected = tx.execute(
            "UPDATE queued_prompts
             SET status = 'consumed', consumed_at = ?1, turn_id = ?2,
                 context_content = ?3, preceding_assistant_content = ?4,
                 preceding_assistant_reasoning = ?5
             WHERE prompt_id = ?6 AND status = 'queued' AND queue_session_id = ?7",
            params![
                now,
                turn_id,
                content,
                (index == 0 && !preceding_content.trim().is_empty())
                    .then_some(preceding_content.as_str()),
                (index == 0)
                    .then_some(preceding_reasoning.as_deref())
                    .flatten(),
                prompt_id,
                queue_session_id,
            ],
        )?;
        if affected != 1 {
            bail!("queued prompt changed during stale-turn recovery: {prompt_id}");
        }
    }

    let prompt_ids = prompts
        .iter()
        .map(|(prompt_id, _)| prompt_id)
        .collect::<Vec<_>>();
    let prompt_payload = serde_json::to_string(&prompt_ids)?;
    let next_segment = segment_index.saturating_add(1);
    if segment_status == "superseded" {
        tx.execute(
            "INSERT INTO turn_journal_segments
                (turn_id, revision, segment_index, status, started_at)
             VALUES (?1, ?2, ?3, 'running', ?4)",
            params![turn_id, revision, next_segment, now],
        )?;
        tx.execute(
            "INSERT INTO turn_journal_events
                (turn_id, revision, segment_index, kind, text_payload, created_at)
             VALUES (?1, ?2, ?3, 'queued_prompts_consumed', ?4, ?5)",
            params![turn_id, revision, next_segment, prompt_payload, now],
        )?;
    } else {
        tx.execute(
            "INSERT INTO turn_journal_events
                (turn_id, revision, segment_index, kind, text_payload, created_at)
             VALUES (?1, ?2, ?3, 'queued_prompts_consumed', ?4, ?5)",
            params![turn_id, revision, segment_index, prompt_payload, now],
        )?;
        tx.execute(
            "UPDATE turn_journal_segments
             SET status = 'completed', finished_at = ?1
             WHERE turn_id = ?2 AND revision = ?3 AND segment_index = ?4",
            params![now, turn_id, revision, segment_index],
        )?;
        tx.execute(
            "INSERT INTO turn_journal_segments
                (turn_id, revision, segment_index, status, started_at)
             VALUES (?1, ?2, ?3, 'running', ?4)",
            params![turn_id, revision, next_segment, now],
        )?;
    }
    Ok(prompts.len())
}

/// MAX() keeps the stamp monotonic even if a stale writer commits late; a
/// wall-clock step backwards must never make an idle session look fresh.
fn touch_session_last_request(tx: &Transaction<'_>, turn_id: &str) -> Result<()> {
    tx.execute(
        "UPDATE sessions SET last_request_at = MAX(COALESCE(last_request_at, 0), ?1)
         WHERE session_id = (SELECT session_id FROM turns WHERE turn_id = ?2)",
        params![Utc::now().timestamp(), turn_id],
    )?;
    Ok(())
}

/// 并发回合完成序追加(消除插入型缓存断点):回合从 running 首次转为
/// 可回放(completed/interrupted)时,若同会话已有 seq 更靠后的可回放
/// 回合,按原 seq 插回会落在后续请求已缓存前缀的中间,之后每个请求都
/// 从那里断链(群聊约 1/5 回合重叠)。把 seq 提升到会话全局 max+1,让
/// "已完成历史"跨请求保持 append-only——这也更忠实:并发回合的实况
/// 请求本来就没见过彼此,群聊时间线由各回合的群聊转储自己承载。
/// 只动首次完成的回合(revision=0):redo 修订的位置已被历史请求看过,
/// 原位改写才是正确语义。
fn bump_completion_seq_locked(tx: &Transaction<'_>, turn_id: &str) -> Result<()> {
    tx.execute(
        "UPDATE turns AS t
            SET seq = (SELECT MAX(o.seq) + 1 FROM turns AS o
                        WHERE o.session_id = t.session_id)
          WHERE t.turn_id = ?1
            AND t.revision = 0
            AND EXISTS (SELECT 1 FROM turns AS later
                         WHERE later.session_id = t.session_id
                           AND later.seq > t.seq
                           AND later.status != 'running')",
        params![turn_id],
    )?;
    Ok(())
}

fn interrupted_projection_locked(
    tx: &Transaction<'_>,
    turn_id: &str,
    revision: i64,
) -> Result<(String, Option<String>)> {
    let segment_index: Option<i64> = tx
        .query_row(
            "SELECT segment_index
             FROM turn_journal_segments
             WHERE turn_id = ?1 AND revision = ?2 AND status != 'superseded'
             ORDER BY segment_index DESC LIMIT 1",
            params![turn_id, revision],
            |row| row.get(0),
        )
        .optional()?;
    let Some(segment_index) = segment_index else {
        return Ok((INTERRUPTED_TEXT.to_string(), None));
    };
    let (content, reasoning) =
        journal_segment_projection_locked(tx, turn_id, revision, segment_index)?;
    let content = if content.trim().is_empty() {
        INTERRUPTED_TEXT.to_string()
    } else {
        format!("{content}\n\n{INTERRUPTED_TEXT}")
    };
    Ok((content, reasoning))
}

fn journal_segment_projection_locked(
    tx: &Transaction<'_>,
    turn_id: &str,
    revision: i64,
    segment_index: i64,
) -> Result<(String, Option<String>)> {
    let mut content = String::new();
    let mut reasoning = String::new();
    let mut stmt = tx.prepare(
        "SELECT kind, text_payload
         FROM turn_journal_events
         WHERE turn_id = ?1 AND revision = ?2 AND segment_index = ?3
         ORDER BY event_id",
    )?;
    let rows = stmt.query_map(params![turn_id, revision, segment_index], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
    })?;
    for row in rows {
        let (kind, text) = row?;
        match kind.as_str() {
            "assistant_content" => {
                if let Some(text) = text {
                    content.push_str(&text);
                }
            }
            "assistant_reasoning" => {
                if let Some(text) = text {
                    reasoning.push_str(&text);
                }
            }
            "reasoning_reset" => reasoning.clear(),
            _ => {}
        }
    }
    let reasoning = (!reasoning.trim().is_empty()).then_some(reasoning);
    Ok((content, reasoning))
}

fn interrupted_prefix(content: &str) -> String {
    let suffix = format!("\n\n{INTERRUPTED_TEXT}");
    content
        .strip_suffix(&suffix)
        .unwrap_or_else(|| content.strip_suffix(INTERRUPTED_TEXT).unwrap_or(content))
        .to_string()
}

fn restore_redo_backup_locked(tx: &Transaction<'_>, turn_id: &str, revision: i64) -> Result<bool> {
    let payload = tx
        .query_row(
            "SELECT payload FROM turn_redo_backups
             WHERE turn_id = ?1 AND revision = ?2",
            params![turn_id, revision],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .optional()?;
    let Some(payload) = payload else {
        return Ok(false);
    };
    let backup: TurnRedoBackup = serde_json::from_slice(&payload)?;
    let session_id: String = tx.query_row(
        "SELECT session_id FROM turns
         WHERE turn_id = ?1 AND revision = ?2 AND status = 'running'",
        params![turn_id, revision],
        |row| row.get(0),
    )?;

    // The failed redo generation is disposable. Its journal must disappear
    // before the previous revision becomes active again, otherwise a later
    // interruption could replay output from the cancelled branch.
    tx.execute(
        "DELETE FROM turn_journal_segments WHERE turn_id = ?1 AND revision = ?2",
        params![turn_id, revision],
    )?;

    tx.execute(
        "DELETE FROM question_exchanges WHERE turn_id = ?1",
        params![turn_id],
    )?;
    tx.execute(
        "INSERT INTO question_exchanges (turn_id, exchange_index, payload)
         SELECT turn_id, exchange_index, payload
         FROM turn_redo_question_backups WHERE turn_id = ?1",
        params![turn_id],
    )?;
    tx.execute(
        "DELETE FROM image_assets WHERE turn_id = ?1",
        params![turn_id],
    )?;
    tx.execute(
        "INSERT INTO image_assets
            (asset_id, turn_id, tool_id, mime, width, height, alt, data, created_at)
         SELECT asset_id, turn_id, tool_id, mime, width, height, alt, data, created_at
         FROM turn_redo_image_backups WHERE turn_id = ?1",
        params![turn_id],
    )?;
    tx.execute(
        "DELETE FROM artifact_assets WHERE turn_id = ?1",
        params![turn_id],
    )?;
    tx.execute(
        "INSERT INTO artifact_assets
            (asset_id, turn_id, tool_id, source_key, file_name, mime, kind,
             size_bytes, data, created_at, updated_at)
         SELECT asset_id, turn_id, tool_id, source_key, file_name, mime, kind,
                size_bytes, data, created_at, updated_at
         FROM turn_redo_artifact_backups WHERE turn_id = ?1",
        params![turn_id],
    )?;
    tx.execute(
        "DELETE FROM session_loaded_items WHERE session_id = ?1",
        params![session_id],
    )?;
    for (kind, name, source_turn_id, created_at, updated_at) in &backup.loaded_items {
        tx.execute(
            "INSERT INTO session_loaded_items
                (session_id, kind, name, source_turn_id, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                session_id,
                kind,
                name,
                source_turn_id,
                created_at,
                updated_at
            ],
        )?;
    }
    let original_prompts = backup
        .consumed_prompt_ids
        .iter()
        .collect::<std::collections::HashSet<_>>();
    let current_prompts = {
        let mut stmt = tx.prepare(
            "SELECT prompt_id FROM queued_prompts
             WHERE turn_id = ?1 AND status = 'consumed'",
        )?;
        let rows = stmt
            .query_map(params![turn_id], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        rows
    };
    for prompt_id in current_prompts {
        if !original_prompts.contains(&prompt_id) {
            tx.execute(
                "DELETE FROM queued_prompts WHERE prompt_id = ?1",
                params![prompt_id],
            )?;
        }
    }
    tx.execute(
        "DELETE FROM turn_redo_checkpoints WHERE turn_id = ?1",
        params![turn_id],
    )?;
    if let Some(checkpoint) = &backup.checkpoint {
        tx.execute(
            "INSERT INTO turn_redo_checkpoints
                (turn_id, version, batch_prompt_ids, payload, unavailable_reason, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                turn_id,
                checkpoint.version,
                checkpoint.batch_prompt_ids,
                checkpoint.payload,
                checkpoint.unavailable_reason,
                checkpoint.created_at
            ],
        )?;
    }
    tx.execute(
        "UPDATE turns SET
            user_content = ?1,
            display_content = ?2,
            assistant_content = ?3,
            assistant_reasoning = ?4,
            assistant_provider_id = ?5,
            assistant_model = ?6,
            assistant_timestamp = ?7,
            status = ?8,
            tool_reports = ?9,
            owner_pid = ?10,
            queue_session_id = ?11,
            token_total = ?12,
            token_usage_estimated = ?13,
            revision = ?14,
            token_prompt = ?17,
            token_cache_read = ?18
         WHERE turn_id = ?15 AND revision = ?16 AND status = 'running'",
        params![
            backup.user_content,
            backup.display_content,
            backup.assistant_content,
            backup.assistant_reasoning,
            backup.assistant_provider_id,
            backup.assistant_model,
            backup.assistant_timestamp,
            backup.status,
            backup.tool_reports,
            backup.owner_pid,
            backup.queue_session_id,
            backup.token_total,
            backup.token_usage_estimated,
            revision.saturating_sub(1),
            turn_id,
            revision,
            backup.token_prompt,
            backup.token_cache_read
        ],
    )?;
    if let (Some(content), Some(display_content)) = (
        backup.followup_content.as_deref(),
        backup.followup_display_content.as_deref(),
    ) {
        tx.execute(
            "UPDATE queued_prompts
             SET content = ?1, display_content = ?2, context_content = ?3
             WHERE prompt_id = (
                SELECT prompt_id FROM queued_prompts
                WHERE turn_id = ?4 AND status = 'consumed'
                ORDER BY seq DESC LIMIT 1
             )",
            params![
                content,
                display_content,
                backup.followup_context_content,
                turn_id
            ],
        )?;
    }
    tx.execute(
        "DELETE FROM turn_redo_backups WHERE turn_id = ?1 AND revision = ?2",
        params![turn_id, revision],
    )?;
    Ok(true)
}

#[allow(dead_code)]
pub fn pending_placeholder() -> &'static str {
    PENDING_PLACEHOLDER
}

#[allow(dead_code)]
pub fn interrupted_text() -> &'static str {
    INTERRUPTED_TEXT
}

fn map_turn_row(row: &rusqlite::Row) -> rusqlite::Result<Turn> {
    let tool_reports_json: String = row.get(11)?;
    let tool_reports: Vec<String> = serde_json::from_str(&tool_reports_json).unwrap_or_default();
    let context_messages_json: String = row.get::<_, Option<String>>(18)?.unwrap_or_default();
    let context_messages: Vec<ChatMessage> =
        serde_json::from_str(&context_messages_json).unwrap_or_default();
    let tool_flow_json: String = row.get::<_, Option<String>>(21)?.unwrap_or_default();
    let tool_flow: Vec<ToolFlowRound> = serde_json::from_str(&tool_flow_json).unwrap_or_default();
    Ok(Turn {
        turn_id: row.get(0)?,
        seq: row.get(1)?,
        user_content: row.get(2)?,
        display_content: row.get(3)?,
        user_timestamp: row.get(4)?,
        assistant_content: row.get(5)?,
        assistant_reasoning: row.get(6)?,
        assistant_provider_id: row.get(7)?,
        assistant_model: row.get(8)?,
        assistant_timestamp: row.get(9)?,
        status: TurnStatus::from_str(row.get::<_, String>(10)?.as_str()),
        tool_reports,
        tool_flow,
        question_exchanges: Vec::new(),
        followups: Vec::new(),
        attachments: Vec::new(),
        hidden: row.get::<_, i64>(12)? != 0,
        is_summary: row.get::<_, i64>(13)? != 0,
        owner_pid: row.get(14)?,
        token_total: row.get::<_, i64>(15)?.max(0) as u64,
        token_prompt: row.get::<_, i64>(19)?.max(0) as u64,
        token_cache_read: row.get::<_, i64>(20)?.max(0) as u64,
        token_usage_estimated: row.get::<_, i64>(16)? != 0,
        revision: row.get(17)?,
        journal_events: Vec::new(),
        context_messages,
    })
}

fn map_user_attachment_row(row: &rusqlite::Row) -> rusqlite::Result<UserAttachment> {
    Ok(UserAttachment {
        attachment_id: row.get(0)?,
        file_name: row.get(1)?,
        mime: row.get(2)?,
        kind: row.get(3)?,
        size_bytes: row.get::<_, i64>(4)?.max(0) as u64,
        width: row.get::<_, i64>(5)?.max(0) as u32,
        height: row.get::<_, i64>(6)?.max(0) as u32,
        created_at: row.get(7)?,
    })
}

fn map_image_asset_row(row: &rusqlite::Row) -> rusqlite::Result<ImageAsset> {
    Ok(ImageAsset {
        asset_id: row.get(0)?,
        turn_id: row.get(1)?,
        tool_id: row.get(2)?,
        mime: row.get(3)?,
        width: row.get::<_, i64>(4)?.max(0) as u32,
        height: row.get::<_, i64>(5)?.max(0) as u32,
        alt: row.get(6)?,
        created_at: row.get(7)?,
    })
}

fn map_artifact_asset_row(row: &rusqlite::Row) -> rusqlite::Result<ArtifactAsset> {
    Ok(ArtifactAsset {
        asset_id: row.get(0)?,
        turn_id: row.get(1)?,
        tool_id: row.get(2)?,
        source_key: row.get(3)?,
        file_name: row.get(4)?,
        mime: row.get(5)?,
        kind: row.get(6)?,
        size_bytes: row.get::<_, i64>(7)?.max(0) as u64,
        created_at: row.get(8)?,
        updated_at: row.get(9)?,
    })
}

fn attach_turn_children_locked(conn: &Connection, turns: &mut [Turn]) -> Result<()> {
    attach_question_exchanges_locked(conn, turns)?;
    attach_followups_locked(conn, turns)?;
    attach_turn_attachments_locked(conn, turns)?;
    attach_turn_journal_events_locked(conn, turns)
}

impl ConversationDb {
    /// Display transcripts of the last `limit` visible turns of a session,
    /// oldest first. Turns finished before this column existed simply come
    /// back with an empty transcript, and the caller falls back to the plain
    /// prompt/reply pair.
    pub fn session_replay(&self, session_id: &str, limit: usize) -> Result<Vec<TurnReplay>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            // The `LIKE` marks daemon-synthesized background-job wake turns.
            // They are not user prompts and must not be replayed as one — same
            // test the wake-report poller uses.
            "SELECT display_content, assistant_content, replay_journal,
                    user_content LIKE '<background-job-report>%'
               FROM turns
              WHERE session_id = ?1 AND hidden = 0 AND is_summary = 0
                AND status = 'completed'
              ORDER BY seq DESC
              LIMIT ?2",
        )?;
        let mut rows = stmt
            .query_map(params![session_id, limit as i64], |row| {
                Ok(TurnReplay {
                    display_content: row.get::<_, Option<String>>(0)?.unwrap_or_default(),
                    assistant_content: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                    entries: row
                        .get::<_, Option<String>>(2)?
                        .and_then(|json| serde_json::from_str(&json).ok())
                        .unwrap_or_default(),
                    is_job_wake: row.get::<_, i64>(3)? != 0,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows.reverse();
        Ok(rows)
    }
}

/// One replayable turn: the prompt echo plus either its ordered transcript or,
/// for turns predating the transcript column, just the final reply.
#[derive(Clone, Debug, Default)]
pub struct TurnReplay {
    /// What the user saw as the prompt — or, for a wake turn, the
    /// `[后台任务完成] …` headline.
    pub display_content: String,
    pub assistant_content: String,
    pub entries: Vec<ReplayEntry>,
    /// Daemon-synthesized follow-up to a finished background job, not a
    /// prompt anybody typed.
    pub is_job_wake: bool,
}

/// Folds the live journal of a just-finished turn into `turns.replay_journal`.
/// Everything only the live view needed — reasoning, progress ticks, command
/// output blobs — is dropped; what is left is the ordered prose/tool sequence
/// the REPL redraws when the session is reopened.
fn store_replay_journal(tx: &Transaction, turn_id: &str) -> Result<()> {
    let mut stmt = tx.prepare(
        "SELECT kind, call_id, name, text_payload, ok
           FROM turn_journal_events
          WHERE turn_id = ?1
            AND kind IN ('assistant_content', 'tool_call', 'tool_result')
          ORDER BY event_id",
    )?;
    let rows = stmt
        .query_map(params![turn_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<i64>>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);

    let mut entries: Vec<ReplayEntry> = Vec::new();
    let mut call_names: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    let mut text = String::new();
    let flush_text = |entries: &mut Vec<ReplayEntry>, text: &mut String| {
        if !text.trim().is_empty() {
            entries.push(ReplayEntry::Text {
                text: truncate_chars_owned(text, REPLAY_ENTRY_MAX_CHARS),
            });
        }
        text.clear();
    };
    for (kind, call_id, name, payload, ok) in rows {
        match kind.as_str() {
            "assistant_content" => text.push_str(payload.as_deref().unwrap_or_default()),
            "tool_call" => {
                flush_text(&mut entries, &mut text);
                let Some(name) = name else { continue };
                if let Some(call_id) = call_id {
                    call_names.insert(call_id, name.clone());
                }
                entries.push(ReplayEntry::ToolCall {
                    name,
                    arguments: truncate_chars_owned(
                        payload.as_deref().unwrap_or_default(),
                        REPLAY_ENTRY_MAX_CHARS,
                    ),
                });
            }
            "tool_result" => {
                flush_text(&mut entries, &mut text);
                let name = call_id
                    .as_deref()
                    .and_then(|id| call_names.get(id).cloned())
                    .or(name)
                    .unwrap_or_default();
                if name.is_empty() {
                    continue;
                }
                entries.push(ReplayEntry::ToolResult {
                    name,
                    ok: ok.unwrap_or(1) != 0,
                    output: truncate_chars_owned(
                        payload.as_deref().unwrap_or_default(),
                        REPLAY_ENTRY_MAX_CHARS,
                    ),
                });
            }
            _ => {}
        }
    }
    flush_text(&mut entries, &mut text);
    if entries.is_empty() {
        return Ok(());
    }
    // Whole-turn budget: drop the oldest entries, so what survives is the tail
    // the user was actually looking at when the turn ended.
    let mut encoded = serde_json::to_string(&entries)?;
    while encoded.len() > REPLAY_JOURNAL_MAX_CHARS && entries.len() > 1 {
        entries.remove(0);
        encoded = serde_json::to_string(&entries)?;
    }
    tx.execute(
        "UPDATE turns SET replay_journal = ?1 WHERE turn_id = ?2",
        params![encoded, turn_id],
    )?;
    Ok(())
}

fn truncate_chars_owned(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        return value.to_string();
    }
    let kept: String = value.chars().take(max).collect();
    format!("{kept}…")
}

fn attach_turn_journal_events_locked(conn: &Connection, turns: &mut [Turn]) -> Result<()> {
    // BTreeMap keeps the chunking below deterministic; HashMap iteration order
    // would shuffle turn ids across the 900-id chunks between calls.
    let indexes = turns
        .iter()
        .enumerate()
        .filter(|(_, turn)| turn.status != TurnStatus::Completed)
        .map(|(index, turn)| (turn.turn_id.clone(), index))
        .collect::<std::collections::BTreeMap<_, _>>();
    if indexes.is_empty() {
        return Ok(());
    }
    let turn_ids = indexes.keys().collect::<Vec<_>>();
    for chunk in turn_ids.chunks(900) {
        let placeholders = std::iter::repeat_n("?", chunk.len())
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT e.turn_id, e.event_id, e.revision, e.segment_index, e.kind,
                    e.call_id, e.name, e.text_payload, e.blob_payload, e.ok
             FROM turn_journal_events e
             INNER JOIN turn_journal_segments s
               ON s.turn_id = e.turn_id AND s.revision = e.revision
              AND s.segment_index = e.segment_index
             INNER JOIN turns t ON t.turn_id = e.turn_id AND t.revision = e.revision
             WHERE e.turn_id IN ({placeholders})
                AND (
                    s.status != 'superseded'
                    OR (
                        e.kind IN (
                            'tool_call', 'tool_result', 'tool_progress',
                            'command_stdout', 'command_stderr', 'image', 'artifact'
                        )
                        AND EXISTS(
                            SELECT 1 FROM turn_journal_events result_event
                            WHERE result_event.turn_id = e.turn_id
                              AND result_event.revision = e.revision
                              AND result_event.segment_index = e.segment_index
                              AND result_event.kind = 'tool_result'
                              AND result_event.call_id = e.call_id
                        )
                    )
                )
             ORDER BY e.event_id"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(chunk.iter()), |row| {
            Ok((
                row.get::<_, String>(0)?,
                TurnJournalEvent {
                    event_id: row.get(1)?,
                    revision: row.get(2)?,
                    segment_index: row.get(3)?,
                    kind: row.get(4)?,
                    call_id: row.get(5)?,
                    name: row.get(6)?,
                    text_payload: row.get(7)?,
                    blob_payload: row.get(8)?,
                    ok: row.get::<_, Option<i64>>(9)?.map(|value| value != 0),
                },
            ))
        })?;
        for row in rows {
            let (turn_id, event) = row?;
            if let Some(index) = indexes.get(&turn_id).copied() {
                turns[index].journal_events.push(event);
            }
        }
    }
    Ok(())
}

fn attach_turn_attachments_locked(conn: &Connection, turns: &mut [Turn]) -> Result<()> {
    if turns.is_empty() {
        return Ok(());
    }
    let indexes = turns
        .iter()
        .enumerate()
        .map(|(index, turn)| (turn.turn_id.clone(), index))
        .collect::<std::collections::HashMap<_, _>>();
    let turn_ids = indexes.keys().collect::<Vec<_>>();
    for chunk in turn_ids.chunks(900) {
        let placeholders = std::iter::repeat_n("?", chunk.len())
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT turn_id, attachment_id, file_name, mime, kind, size_bytes,
                    width, height, created_at FROM user_attachments
             WHERE turn_id IN ({placeholders}) ORDER BY created_at, attachment_id"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(chunk.iter()), |row| {
            Ok((
                row.get::<_, String>(0)?,
                UserAttachment {
                    attachment_id: row.get(1)?,
                    file_name: row.get(2)?,
                    mime: row.get(3)?,
                    kind: row.get(4)?,
                    size_bytes: row.get::<_, i64>(5)?.max(0) as u64,
                    width: row.get::<_, i64>(6)?.max(0) as u32,
                    height: row.get::<_, i64>(7)?.max(0) as u32,
                    created_at: row.get(8)?,
                },
            ))
        })?;
        for row in rows {
            let (turn_id, attachment) = row?;
            if let Some(index) = indexes.get(&turn_id).copied() {
                turns[index].attachments.push(attachment);
            }
        }
    }
    Ok(())
}

fn attach_question_exchanges_locked(conn: &Connection, turns: &mut [Turn]) -> Result<()> {
    if turns.is_empty() {
        return Ok(());
    }
    let indexes = turns
        .iter()
        .enumerate()
        .map(|(index, turn)| (turn.turn_id.clone(), index))
        .collect::<std::collections::HashMap<_, _>>();
    let turn_ids = indexes.keys().collect::<Vec<_>>();
    for chunk in turn_ids.chunks(900) {
        let placeholders = std::iter::repeat_n("?", chunk.len())
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT turn_id, payload FROM question_exchanges
             WHERE turn_id IN ({placeholders}) ORDER BY turn_id, exchange_index"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(chunk.iter()), |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (turn_id, payload) = row?;
            let Some(index) = indexes.get(&turn_id).copied() else {
                continue;
            };
            let exchange = serde_json::from_str::<QuestionExchange>(&payload)
                .with_context(|| format!("invalid question exchange for turn {turn_id}"))?;
            turns[index].question_exchanges.push(exchange);
        }
    }
    Ok(())
}

fn attach_followups_locked(conn: &Connection, turns: &mut [Turn]) -> Result<()> {
    if turns.is_empty() {
        return Ok(());
    }
    let indexes = turns
        .iter()
        .enumerate()
        .map(|(index, turn)| (turn.turn_id.clone(), index))
        .collect::<std::collections::HashMap<_, _>>();
    let turn_ids = indexes.keys().collect::<Vec<_>>();
    for chunk in turn_ids.chunks(900) {
        let placeholders = std::iter::repeat_n("?", chunk.len())
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT prompt_id, turn_id, COALESCE(context_content, content), display_content,
                    attachments, submitted_at, preceding_assistant_content,
                    preceding_assistant_reasoning, preceding_assistant_provider_id,
                    preceding_assistant_model
             FROM queued_prompts
             WHERE status = 'consumed' AND turn_id IN ({placeholders})
             ORDER BY seq ASC"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(chunk.iter()), |row| {
            Ok((
                row.get::<_, String>(1)?,
                TurnFollowup {
                    prompt_id: row.get(0)?,
                    content: row.get(2)?,
                    display_content: row.get(3)?,
                    attachments: serde_json::from_str(&row.get::<_, String>(4)?)
                        .unwrap_or_default(),
                    uploaded_attachments: Vec::new(),
                    submitted_at: row.get(5)?,
                    preceding_assistant_content: row.get(6)?,
                    preceding_assistant_reasoning: row.get(7)?,
                    preceding_assistant_provider_id: row.get(8)?,
                    preceding_assistant_model: row.get(9)?,
                },
            ))
        })?;
        for row in rows {
            let (turn_id, followup) = row?;
            let Some(index) = indexes.get(&turn_id).copied() else {
                continue;
            };
            turns[index].followups.push(followup);
        }
    }
    attach_followup_attachments_locked(conn, turns)?;
    Ok(())
}

fn attach_prompt_attachments_locked(conn: &Connection, prompts: &mut [QueuedPrompt]) -> Result<()> {
    let indexes = prompts
        .iter()
        .enumerate()
        .map(|(index, prompt)| (prompt.prompt_id.clone(), index))
        .collect::<std::collections::HashMap<_, _>>();
    if indexes.is_empty() {
        return Ok(());
    }
    let prompt_ids = indexes.keys().collect::<Vec<_>>();
    for chunk in prompt_ids.chunks(900) {
        let placeholders = std::iter::repeat_n("?", chunk.len())
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT prompt_id, attachment_id, file_name, mime, kind, size_bytes,
                    width, height, created_at FROM user_attachments
             WHERE prompt_id IN ({placeholders}) ORDER BY created_at, attachment_id"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(chunk.iter()), |row| {
            Ok((
                row.get::<_, String>(0)?,
                UserAttachment {
                    attachment_id: row.get(1)?,
                    file_name: row.get(2)?,
                    mime: row.get(3)?,
                    kind: row.get(4)?,
                    size_bytes: row.get::<_, i64>(5)?.max(0) as u64,
                    width: row.get::<_, i64>(6)?.max(0) as u32,
                    height: row.get::<_, i64>(7)?.max(0) as u32,
                    created_at: row.get(8)?,
                },
            ))
        })?;
        for row in rows {
            let (prompt_id, attachment) = row?;
            if let Some(index) = indexes.get(&prompt_id).copied() {
                prompts[index].uploaded_attachments.push(attachment);
            }
        }
    }
    Ok(())
}

fn attach_followup_attachments_locked(conn: &Connection, turns: &mut [Turn]) -> Result<()> {
    let mut locations = std::collections::HashMap::new();
    for (turn_index, turn) in turns.iter().enumerate() {
        for (followup_index, followup) in turn.followups.iter().enumerate() {
            locations.insert(followup.prompt_id.clone(), (turn_index, followup_index));
        }
    }
    if locations.is_empty() {
        return Ok(());
    }
    let prompt_ids = locations.keys().collect::<Vec<_>>();
    for chunk in prompt_ids.chunks(900) {
        let placeholders = std::iter::repeat_n("?", chunk.len())
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT prompt_id, attachment_id, file_name, mime, kind, size_bytes,
                    width, height, created_at FROM user_attachments
             WHERE prompt_id IN ({placeholders}) ORDER BY created_at, attachment_id"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(chunk.iter()), |row| {
            Ok((
                row.get::<_, String>(0)?,
                UserAttachment {
                    attachment_id: row.get(1)?,
                    file_name: row.get(2)?,
                    mime: row.get(3)?,
                    kind: row.get(4)?,
                    size_bytes: row.get::<_, i64>(5)?.max(0) as u64,
                    width: row.get::<_, i64>(6)?.max(0) as u32,
                    height: row.get::<_, i64>(7)?.max(0) as u32,
                    created_at: row.get(8)?,
                },
            ))
        })?;
        for row in rows {
            let (prompt_id, attachment) = row?;
            if let Some((turn_index, followup_index)) = locations.get(&prompt_id).copied() {
                turns[turn_index].followups[followup_index]
                    .uploaded_attachments
                    .push(attachment);
            }
        }
    }
    Ok(())
}
