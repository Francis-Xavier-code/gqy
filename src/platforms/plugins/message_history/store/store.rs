//! store — 自 src/platforms/plugins/message_history/store.rs 拆分。

use super::*;

use crate::platforms::{ConversationKind, PlatformMention};
use anyhow::{anyhow, bail, Context, Result};
use rusqlite::types::Value as SqlValue;
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, oneshot};

pub(crate) const SCHEMA_VERSION: i64 = 4;
pub(crate) const DEFAULT_QUEUE_CAPACITY: usize = 128;
pub(crate) const MAX_QUEUE_CAPACITY: usize = 4_096;
pub(crate) const MAX_BATCH_MESSAGES: usize = 256;
pub(crate) const DEFAULT_DELETE_BATCH_SIZE: usize = 1_000;
pub(crate) const MAX_DELETE_BATCH_SIZE: usize = 5_000;
pub(crate) const MAX_PAGE_SIZE: usize = 1_000;
pub(crate) const MAX_IDENTIFIER_BYTES: usize = 256;
pub(crate) const MAX_NAME_BYTES: usize = 512;
pub(crate) const MAX_TEXT_BYTES: usize = 64 * 1024;
pub(crate) const MAX_MEDIA_ITEMS: usize = 16;
pub(crate) const MAX_MENTIONED_USERS: usize = 32;
pub(crate) const MAX_MEDIA_LABEL_BYTES: usize = 512;
pub(crate) const MAX_MIME_BYTES: usize = 128;
pub(crate) const MAX_SEARCH_BYTES: usize = 1_024;
pub(crate) const MAX_SEARCH_TERMS: usize = 32;
pub(crate) const MAX_ACTIVITY_RANKING_LIMIT: usize = 200;
pub(crate) const SECONDS_PER_DAY: i64 = 86_400;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) struct ConversationKey {
    platform: String,
    account_id: String,
    conversation_kind: String,
    conversation_id: String,
}

/// Compatibility name for real-context code that only reads group history.
pub(crate) type GroupKey = ConversationKey;

impl ConversationKey {
    /// Constructs a group conversation key for existing real-context callers.
    pub(crate) fn new(
        platform: impl Into<String>,
        account_id: impl Into<String>,
        group_id: impl Into<String>,
    ) -> Result<Self> {
        Self::for_kind(platform, account_id, ConversationKind::Group, group_id)
    }

    pub(crate) fn for_kind(
        platform: impl Into<String>,
        account_id: impl Into<String>,
        kind: ConversationKind,
        conversation_id: impl Into<String>,
    ) -> Result<Self> {
        Ok(Self {
            platform: validate_identifier("platform", platform.into())?,
            account_id: validate_identifier("account id", account_id.into())?,
            conversation_kind: kind.as_str().to_string(),
            conversation_id: validate_identifier("conversation id", conversation_id.into())?,
        })
    }

    pub(crate) fn platform(&self) -> &str {
        &self.platform
    }

    pub(crate) fn account_id(&self) -> &str {
        &self.account_id
    }

    pub(crate) fn group_id(&self) -> &str {
        &self.conversation_id
    }

    pub(crate) fn conversation_kind(&self) -> &str {
        &self.conversation_kind
    }

    pub(crate) fn conversation_id(&self) -> &str {
        &self.conversation_id
    }

    pub(crate) fn is_group(&self) -> bool {
        self.conversation_kind == ConversationKind::Group.as_str()
    }

    pub(crate) fn account_scope(&self) -> AccountKey {
        AccountKey {
            platform: self.platform.clone(),
            account_id: self.account_id.clone(),
        }
    }
}

/// Account-wide history access is reserved for already-authorized tools. It
/// never crosses the platform or bot-account boundary.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) struct AccountKey {
    platform: String,
    account_id: String,
}

impl AccountKey {
    pub(crate) fn new(platform: impl Into<String>, account_id: impl Into<String>) -> Result<Self> {
        Ok(Self {
            platform: validate_identifier("platform", platform.into())?,
            account_id: validate_identifier("account id", account_id.into())?,
        })
    }

    pub(crate) fn platform(&self) -> &str {
        &self.platform
    }

    pub(crate) fn account_id(&self) -> &str {
        &self.account_id
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum HistoryScope {
    Group(GroupKey),
    Private(ConversationKey),
    Account(AccountKey),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MediaKind {
    Image,
    Sticker,
    File,
    Audio,
    Video,
    Other,
}

/// Deliberately contains no URL, filesystem path, byte buffer, or Base64
/// field. History only needs enough structure to tell the model what appeared.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct MediaPlaceholder {
    pub(crate) kind: MediaKind,
    pub(crate) label: Option<String>,
    pub(crate) mime: Option<String>,
}

impl MediaPlaceholder {
    pub(crate) fn new(
        kind: MediaKind,
        label: Option<impl Into<String>>,
        mime: Option<impl Into<String>>,
    ) -> Self {
        Self {
            kind,
            label: label.map(Into::into),
            mime: mime.map(Into::into),
        }
    }

    pub(crate) fn sanitized(mut self) -> Self {
        self.label = self
            .label
            .map(|value| sanitize_single_line(&value, MAX_MEDIA_LABEL_BYTES))
            .filter(|value| !value.is_empty());
        self.mime = self
            .mime
            .map(|value| sanitize_single_line(&value, MAX_MIME_BYTES))
            .filter(|value| !value.is_empty());
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub(crate) struct SanitizedContent {
    pub(crate) text: String,
    pub(crate) media: Vec<MediaPlaceholder>,
    pub(crate) mentioned_user_ids: Vec<String>,
    #[serde(default)]
    pub(crate) mentioned_users: Vec<PlatformMention>,
}

impl SanitizedContent {
    pub(crate) fn new(text: impl Into<String>, media: Vec<MediaPlaceholder>) -> Self {
        Self {
            text: text.into(),
            media,
            mentioned_user_ids: Vec::new(),
            mentioned_users: Vec::new(),
        }
    }

    pub(crate) fn sanitized(mut self) -> Result<Self> {
        self.text = sanitize_multiline(&self.text, MAX_TEXT_BYTES);
        self.media = self
            .media
            .into_iter()
            .take(MAX_MEDIA_ITEMS)
            .map(MediaPlaceholder::sanitized)
            .collect();
        let mut seen = HashSet::with_capacity(self.mentioned_user_ids.len());
        self.mentioned_user_ids = self
            .mentioned_user_ids
            .into_iter()
            .map(|value| validate_identifier("mentioned user id", value))
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .filter(|value| seen.insert(value.clone()))
            .take(MAX_MENTIONED_USERS)
            .collect();
        let mut seen = HashSet::with_capacity(self.mentioned_users.len());
        self.mentioned_users = self
            .mentioned_users
            .into_iter()
            .map(|mention| {
                Ok(PlatformMention {
                    user_id: validate_identifier("mentioned user id", mention.user_id)?,
                    display_name: mention
                        .display_name
                        .map(|name| sanitize_single_line(&name, MAX_NAME_BYTES))
                        .filter(|name| !name.is_empty()),
                })
            })
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .filter(|mention| seen.insert(mention.user_id.clone()))
            .take(MAX_MENTIONED_USERS)
            .collect();
        Ok(self)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct NewHistoryMessage {
    pub(crate) group: GroupKey,
    pub(crate) message_id: String,
    pub(crate) sender_id: String,
    pub(crate) sender_name: String,
    pub(crate) content: SanitizedContent,
    pub(crate) reply_to_message_id: Option<String>,
    pub(crate) is_bot: bool,
    /// Unix timestamp supplied by the platform event.
    pub(crate) sent_at: i64,
    /// Monotonic receive order shared by all messages produced for one
    /// inbound turn. Legacy and externally recorded rows may omit it.
    pub(crate) ingress_order: Option<i64>,
}

impl NewHistoryMessage {
    pub(crate) fn sanitized(mut self) -> Result<Self> {
        self.message_id = validate_identifier("message id", self.message_id)?;
        self.sender_id = validate_identifier("sender id", self.sender_id)?;
        self.sender_name = sanitize_single_line(&self.sender_name, MAX_NAME_BYTES);
        if self.sender_name.is_empty() {
            self.sender_name.clone_from(&self.sender_id);
        }
        self.reply_to_message_id = self
            .reply_to_message_id
            .map(|value| validate_identifier("reply message id", value))
            .transpose()?;
        self.content = self.content.sanitized()?;
        Ok(self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct HistoryMessage {
    pub(crate) row_id: i64,
    #[serde(rename = "conversation")]
    pub(crate) group: GroupKey,
    pub(crate) message_id: String,
    pub(crate) sender_id: String,
    pub(crate) sender_name: String,
    pub(crate) content: SanitizedContent,
    pub(crate) reply_to_message_id: Option<String>,
    pub(crate) is_bot: bool,
    pub(crate) sent_at: i64,
    pub(crate) ingress_order: Option<i64>,
    pub(crate) recalled_at: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) struct RecordOutcome {
    pub(crate) row_id: i64,
    pub(crate) inserted: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct NewRecall {
    pub(crate) group: GroupKey,
    pub(crate) message_id: String,
    pub(crate) operator_id: Option<String>,
    pub(crate) recalled_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) struct RecallOutcome {
    pub(crate) newly_recorded: bool,
    pub(crate) matched_message: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct HistoryCursor {
    pub(crate) sent_at: i64,
    pub(crate) row_id: i64,
}

#[derive(Debug, Clone)]
pub(crate) struct RecentQuery {
    pub(crate) group: GroupKey,
    pub(crate) persona_scope: String,
    pub(crate) before: Option<HistoryCursor>,
    pub(crate) limit: usize,
    pub(crate) respect_context_boundary: bool,
    pub(crate) include_recalled: bool,
    pub(crate) before_ingress_order: Option<i64>,
    /// Lower bound used by the reply turn: everything the previous turn already
    /// rendered stays in the conversation history, so a turn only has to carry
    /// what arrived since.
    pub(crate) after_ingress_order: Option<i64>,
}

impl RecentQuery {
    pub(crate) fn for_context(
        group: GroupKey,
        persona_scope: impl Into<String>,
        limit: usize,
    ) -> Self {
        Self {
            group,
            persona_scope: persona_scope.into(),
            before: None,
            limit,
            respect_context_boundary: true,
            include_recalled: false,
            before_ingress_order: None,
            after_ingress_order: None,
        }
    }

    pub(crate) fn for_history(group: GroupKey, limit: usize) -> Self {
        Self {
            group,
            persona_scope: String::new(),
            before: None,
            limit,
            respect_context_boundary: false,
            include_recalled: false,
            before_ingress_order: None,
            after_ingress_order: None,
        }
    }

    pub(crate) fn after_ingress_order(mut self, order: Option<i64>) -> Self {
        self.after_ingress_order = order;
        self
    }

    pub(crate) fn before_ingress_order(mut self, order: Option<i64>) -> Self {
        self.before_ingress_order = order;
        self
    }
}

#[derive(Debug, Clone)]
pub(crate) struct SearchQuery {
    pub(crate) scope: HistoryScope,
    pub(crate) text: String,
    pub(crate) sender_id: Option<String>,
    pub(crate) before: Option<HistoryCursor>,
    pub(crate) since: Option<i64>,
    pub(crate) until: Option<i64>,
    pub(crate) limit: usize,
    pub(crate) include_recalled: bool,
    pub(crate) include_bot: bool,
}

impl SearchQuery {
    pub(crate) fn new(scope: HistoryScope, text: impl Into<String>, limit: usize) -> Self {
        Self {
            scope,
            text: text.into(),
            sender_id: None,
            before: None,
            since: None,
            until: None,
            limit,
            include_recalled: false,
            include_bot: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct HistoryPage {
    /// Search results are newest-first. Recent-history results are chronological
    /// within the selected newest page so they can be injected directly.
    pub(crate) messages: Vec<HistoryMessage>,
    pub(crate) next_cursor: Option<HistoryCursor>,
}

#[derive(Debug, Clone)]
pub(crate) struct ActivityRankingQuery {
    pub(crate) group: GroupKey,
    pub(crate) since: i64,
    pub(crate) until: i64,
    pub(crate) limit: usize,
    pub(crate) include_bot: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ActivityRankingItem {
    pub(crate) rank: u64,
    pub(crate) sender_id: String,
    pub(crate) sender_name: String,
    pub(crate) message_count: u64,
    pub(crate) active_days: u64,
    pub(crate) first_sent_at: i64,
    pub(crate) last_sent_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ActivityRanking {
    pub(crate) total_messages: u64,
    pub(crate) participant_count: u64,
    pub(crate) items: Vec<ActivityRankingItem>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum DeleteMode {
    All,
    KeepDays(u32),
}

#[derive(Debug, Clone)]
pub(crate) struct DeleteRequest {
    pub(crate) scope: HistoryScope,
    pub(crate) mode: DeleteMode,
    pub(crate) sender_id: Option<String>,
    pub(crate) since: Option<i64>,
    pub(crate) until: Option<i64>,
    /// Unix timestamp used as a stable reference for `KeepDays`.
    pub(crate) now: i64,
    pub(crate) batch_size: usize,
}

impl DeleteRequest {
    pub(crate) fn all(scope: HistoryScope, now: i64) -> Self {
        Self {
            scope,
            mode: DeleteMode::All,
            sender_id: None,
            since: None,
            until: None,
            now,
            batch_size: DEFAULT_DELETE_BATCH_SIZE,
        }
    }

    pub(crate) fn keep_days(scope: HistoryScope, days: u32, now: i64) -> Result<Self> {
        if days == 0 {
            bail!("keep_days must be a positive integer");
        }
        Ok(Self {
            scope,
            mode: DeleteMode::KeepDays(days),
            sender_id: None,
            since: None,
            until: None,
            now,
            batch_size: DEFAULT_DELETE_BATCH_SIZE,
        })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub(crate) struct DeleteReport {
    pub(crate) messages_deleted: u64,
    pub(crate) recalls_deleted: u64,
    pub(crate) boundaries_deleted: u64,
    pub(crate) batches: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) struct ContextBoundary {
    pub(crate) after_row_id: i64,
    pub(crate) reset_at: i64,
}

/// Cheap-to-clone, backpressured handle to a single SQLite owner thread.
/// Construction does not create a directory, DB, thread, or SQLite connection.
#[derive(Clone)]
pub(crate) struct HistoryStore {
    inner: Arc<HistoryStoreInner>,
}

impl std::fmt::Debug for HistoryStore {
    pub(crate) fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HistoryStore")
            .field("db_path", &self.inner.db_path)
            .field("queue_capacity", &self.inner.queue_capacity)
            .finish_non_exhaustive()
    }
}

pub(crate) struct HistoryStoreInner {
    db_path: PathBuf,
    queue_capacity: usize,
    actor: Mutex<Option<mpsc::Sender<Command>>>,
}

impl HistoryStore {
    pub(crate) fn new(db_path: impl Into<PathBuf>) -> Self {
        Self::with_queue_capacity(db_path, DEFAULT_QUEUE_CAPACITY)
    }

    pub(crate) fn with_queue_capacity(db_path: impl Into<PathBuf>, queue_capacity: usize) -> Self {
        Self {
            inner: Arc::new(HistoryStoreInner {
                db_path: db_path.into(),
                queue_capacity: queue_capacity.clamp(1, MAX_QUEUE_CAPACITY),
                actor: Mutex::new(None),
            }),
        }
    }

    pub(crate) fn db_path(&self) -> &Path {
        &self.inner.db_path
    }

    pub(crate) async fn record_message(&self, message: NewHistoryMessage) -> Result<RecordOutcome> {
        let mut outcomes = self.record_messages(vec![message]).await?;
        outcomes
            .pop()
            .ok_or_else(|| anyhow!("history actor returned no record outcome"))
    }

    pub(crate) async fn record_messages(
        &self,
        messages: Vec<NewHistoryMessage>,
    ) -> Result<Vec<RecordOutcome>> {
        if messages.is_empty() {
            return Ok(Vec::new());
        }
        if messages.len() > MAX_BATCH_MESSAGES {
            bail!("history record batch exceeds the limit of {MAX_BATCH_MESSAGES} messages");
        }
        let messages = messages
            .into_iter()
            .map(NewHistoryMessage::sanitized)
            .collect::<Result<Vec<_>>>()?;
        self.request(|reply| Command::Record { messages, reply })
            .await
    }

    pub(crate) async fn record_recall(&self, mut recall: NewRecall) -> Result<RecallOutcome> {
        recall.message_id = validate_identifier("message id", recall.message_id)?;
        recall.operator_id = recall
            .operator_id
            .map(|value| validate_identifier("recall operator id", value))
            .transpose()?;
        self.request(|reply| Command::Recall { recall, reply })
            .await
    }

    pub(crate) async fn reset_context(
        &self,
        group: GroupKey,
        persona_scope: String,
        reset_at: i64,
    ) -> Result<ContextBoundary> {
        self.request(|reply| Command::ResetContext {
            group,
            persona_scope,
            reset_at,
            reply,
        })
        .await
    }

    pub(crate) async fn context_boundary(
        &self,
        group: GroupKey,
        persona_scope: String,
    ) -> Result<Option<ContextBoundary>> {
        self.request(|reply| Command::GetBoundary {
            group,
            persona_scope,
            reply,
        })
        .await
    }

    pub(crate) async fn recent(&self, query: RecentQuery) -> Result<HistoryPage> {
        self.request(|reply| Command::Recent { query, reply }).await
    }

    pub(crate) async fn search(&self, mut query: SearchQuery) -> Result<HistoryPage> {
        query.sender_id = query
            .sender_id
            .map(|value| validate_identifier("sender id", value))
            .transpose()?;
        if query
            .since
            .zip(query.until)
            .is_some_and(|(from, to)| from > to)
        {
            bail!("history search time range must have since <= until");
        }
        self.request(|reply| Command::Search { query, reply }).await
    }

    pub(crate) async fn activity_ranking(
        &self,
        mut query: ActivityRankingQuery,
    ) -> Result<ActivityRanking> {
        if query.since > query.until {
            bail!("activity ranking time range must have since <= until");
        }
        query.limit = query.limit.clamp(1, MAX_ACTIVITY_RANKING_LIMIT);
        self.request(|reply| Command::ActivityRanking { query, reply })
            .await
    }

    /// The caller must complete GQY-admin authorization before invoking this.
    /// The store intentionally has no concept of QQ group-owner/admin roles.
    pub(crate) async fn delete_history(&self, mut request: DeleteRequest) -> Result<DeleteReport> {
        if matches!(request.mode, DeleteMode::KeepDays(0)) {
            bail!("keep_days must be a positive integer");
        }
        request.sender_id = request
            .sender_id
            .map(|value| validate_identifier("sender id", value))
            .transpose()?;
        if request
            .since
            .zip(request.until)
            .is_some_and(|(from, to)| from > to)
        {
            bail!("history deletion time range must have since <= until");
        }
        request.batch_size = request.batch_size.clamp(1, MAX_DELETE_BATCH_SIZE);
        self.request(|reply| Command::Delete { request, reply })
            .await
    }

    pub(crate) async fn request<T>(
        &self,
        build: impl FnOnce(oneshot::Sender<Result<T>>) -> Command,
    ) -> Result<T>
    where
        T: Send + 'static,
    {
        let actor = self.actor_sender()?;
        let (reply, receiver) = oneshot::channel();
        actor
            .send(build(reply))
            .await
            .map_err(|_| anyhow!("message history actor is unavailable"))?;
        receiver
            .await
            .map_err(|_| anyhow!("message history actor stopped before replying"))?
    }

    pub(crate) fn actor_sender(&self) -> Result<mpsc::Sender<Command>> {
        let mut guard = self
            .inner
            .actor
            .lock()
            .map_err(|_| anyhow!("message history actor lock was poisoned"))?;
        if let Some(sender) = guard.as_ref().filter(|sender| !sender.is_closed()) {
            return Ok(sender.clone());
        }

        let (sender, receiver) = mpsc::channel(self.inner.queue_capacity);
        let path = self.inner.db_path.clone();
        std::thread::Builder::new()
            .name("gqy-message-history".to_string())
            .spawn(move || actor_loop(path, receiver))
            .context("starting message history actor")?;
        *guard = Some(sender.clone());
        Ok(sender)
    }
}

pub(crate) enum Command {
    Record {
        messages: Vec<NewHistoryMessage>,
        reply: oneshot::Sender<Result<Vec<RecordOutcome>>>,
    },
    Recall {
        recall: NewRecall,
        reply: oneshot::Sender<Result<RecallOutcome>>,
    },
    ResetContext {
        group: GroupKey,
        persona_scope: String,
        reset_at: i64,
        reply: oneshot::Sender<Result<ContextBoundary>>,
    },
    GetBoundary {
        group: GroupKey,
        persona_scope: String,
        reply: oneshot::Sender<Result<Option<ContextBoundary>>>,
    },
    Recent {
        query: RecentQuery,
        reply: oneshot::Sender<Result<HistoryPage>>,
    },
    Search {
        query: SearchQuery,
        reply: oneshot::Sender<Result<HistoryPage>>,
    },
    ActivityRanking {
        query: ActivityRankingQuery,
        reply: oneshot::Sender<Result<ActivityRanking>>,
    },
    Delete {
        request: DeleteRequest,
        reply: oneshot::Sender<Result<DeleteReport>>,
    },
}

pub(crate) fn actor_loop(db_path: PathBuf, mut receiver: mpsc::Receiver<Command>) {
    let mut connection = None;
    while let Some(command) = receiver.blocking_recv() {
        match command {
            Command::Record { messages, reply } => {
                let result = actor_connection(&mut connection, &db_path)
                    .and_then(|conn| insert_messages(conn, messages));
                let _ = reply.send(result);
            }
            Command::Recall { recall, reply } => {
                let result = actor_connection(&mut connection, &db_path)
                    .and_then(|conn| insert_recall(conn, recall));
                let _ = reply.send(result);
            }
            Command::ResetContext {
                group,
                persona_scope,
                reset_at,
                reply,
            } => {
                let result = actor_connection(&mut connection, &db_path)
                    .and_then(|conn| upsert_boundary(conn, &group, &persona_scope, reset_at));
                let _ = reply.send(result);
            }
            Command::GetBoundary {
                group,
                persona_scope,
                reply,
            } => {
                let result = actor_connection(&mut connection, &db_path)
                    .and_then(|conn| read_boundary(conn, &group, &persona_scope));
                let _ = reply.send(result);
            }
            Command::Recent { query, reply } => {
                let result = actor_connection(&mut connection, &db_path)
                    .and_then(|conn| query_recent(conn, query));
                let _ = reply.send(result);
            }
            Command::Search { query, reply } => {
                let result = actor_connection(&mut connection, &db_path)
                    .and_then(|conn| query_search(conn, query));
                let _ = reply.send(result);
            }
            Command::ActivityRanking { query, reply } => {
                let result = actor_connection(&mut connection, &db_path)
                    .and_then(|conn| query_activity_ranking(conn, query));
                let _ = reply.send(result);
            }
            Command::Delete { request, reply } => {
                let result = actor_connection(&mut connection, &db_path)
                    .and_then(|conn| delete_history(conn, request));
                let _ = reply.send(result);
            }
        }
    }
}

pub(crate) fn actor_connection<'a>(
    connection: &'a mut Option<Connection>,
    db_path: &Path,
) -> Result<&'a mut Connection> {
    if connection.is_none() {
        *connection = Some(open_database(db_path)?);
    }
    connection
        .as_mut()
        .ok_or_else(|| anyhow!("message history connection was not initialized"))
}

pub(crate) fn open_database(db_path: &Path) -> Result<Connection> {
    if let Some(parent) = db_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating message history directory: {}", parent.display()))?;
    }
    let conn = Connection::open(db_path)
        .with_context(|| format!("opening message history database: {}", db_path.display()))?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    conn.execute_batch(
        "PRAGMA auto_vacuum = INCREMENTAL;
         PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         PRAGMA foreign_keys = ON;
         PRAGMA wal_autocheckpoint = 1000;
         PRAGMA cache_size = -4096;
         PRAGMA mmap_size = 0;",
    )?;
    migrate(&conn)?;
    // Version-1 databases may already contain a boundary left above the
    // largest surviving rowid by an older keep-days cleanup. Repair it every
    // time the lazy connection opens so existing installations recover
    // without requiring another destructive operation.
    clamp_boundaries_to_current_rowid(&conn)?;
    Ok(conn)
}

pub(crate) fn migrate(conn: &Connection) -> Result<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version > SCHEMA_VERSION {
        bail!("message history database schema {version} is newer than supported {SCHEMA_VERSION}");
    }
    if version == SCHEMA_VERSION {
        return Ok(());
    }

    conn.execute_batch(
        "BEGIN IMMEDIATE;
         CREATE TABLE IF NOT EXISTS messages (
             id INTEGER PRIMARY KEY,
             platform TEXT NOT NULL,
             account_id TEXT NOT NULL,
             group_id TEXT NOT NULL,
             message_id TEXT NOT NULL,
             sender_id TEXT NOT NULL,
             sender_name TEXT NOT NULL,
             text TEXT NOT NULL,
             media_json TEXT NOT NULL,
             mentions_json TEXT NOT NULL,
             reply_to_message_id TEXT,
             is_bot INTEGER NOT NULL CHECK (is_bot IN (0, 1)),
             sent_at INTEGER NOT NULL,
             recalled_at INTEGER,
             recorded_at INTEGER NOT NULL DEFAULT (unixepoch()),
             UNIQUE (platform, account_id, group_id, message_id)
         );
         CREATE INDEX IF NOT EXISTS idx_messages_scope_time
             ON messages(platform, account_id, group_id, sent_at DESC, id DESC);
         CREATE INDEX IF NOT EXISTS idx_messages_account_time
             ON messages(platform, account_id, sent_at DESC, id DESC);
         CREATE INDEX IF NOT EXISTS idx_messages_scope_sender_time
             ON messages(platform, account_id, group_id, sender_id, sent_at DESC, id DESC);
         CREATE INDEX IF NOT EXISTS idx_messages_account_sender_time
             ON messages(platform, account_id, sender_id, sent_at DESC, id DESC);
         CREATE INDEX IF NOT EXISTS idx_messages_scope_reply
             ON messages(platform, account_id, group_id, reply_to_message_id)
             WHERE reply_to_message_id IS NOT NULL;

         CREATE TABLE IF NOT EXISTS recalls (
             id INTEGER PRIMARY KEY,
             platform TEXT NOT NULL,
             account_id TEXT NOT NULL,
             group_id TEXT NOT NULL,
             message_id TEXT NOT NULL,
             operator_id TEXT,
             recalled_at INTEGER NOT NULL,
             UNIQUE (platform, account_id, group_id, message_id)
         );
         CREATE INDEX IF NOT EXISTS idx_recalls_scope_time
             ON recalls(platform, account_id, group_id, recalled_at DESC, id DESC);
         CREATE INDEX IF NOT EXISTS idx_recalls_account_time
             ON recalls(platform, account_id, recalled_at DESC, id DESC);
         CREATE INDEX IF NOT EXISTS idx_recalls_scope_operator_time
             ON recalls(platform, account_id, group_id, operator_id, recalled_at DESC)
             WHERE operator_id IS NOT NULL;

         CREATE TABLE IF NOT EXISTS context_boundaries (
             platform TEXT NOT NULL,
             account_id TEXT NOT NULL,
             group_id TEXT NOT NULL,
             persona_scope TEXT NOT NULL DEFAULT 'default',
             after_row_id INTEGER NOT NULL,
             reset_at INTEGER NOT NULL,
             PRIMARY KEY (platform, account_id, group_id, persona_scope)
         ) WITHOUT ROWID;

         CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts USING fts5(
             text,
             sender_name,
             content='messages',
             content_rowid='id',
             tokenize='trigram'
         );
         CREATE TRIGGER IF NOT EXISTS messages_fts_insert AFTER INSERT ON messages BEGIN
             INSERT INTO messages_fts(rowid, text, sender_name)
             VALUES (new.id, new.text, new.sender_name);
         END;
         CREATE TRIGGER IF NOT EXISTS messages_fts_delete AFTER DELETE ON messages BEGIN
             INSERT INTO messages_fts(messages_fts, rowid, text, sender_name)
             VALUES ('delete', old.id, old.text, old.sender_name);
         END;
         CREATE TRIGGER IF NOT EXISTS messages_fts_update
         AFTER UPDATE OF text, sender_name ON messages BEGIN
             INSERT INTO messages_fts(messages_fts, rowid, text, sender_name)
             VALUES ('delete', old.id, old.text, old.sender_name);
             INSERT INTO messages_fts(rowid, text, sender_name)
             VALUES (new.id, new.text, new.sender_name);
         END;
         PRAGMA user_version = 1;
         COMMIT;",
    )
    .context("creating message history schema")?;
    if version < 2 {
        conn.execute_batch(
            "BEGIN IMMEDIATE;
             ALTER TABLE messages ADD COLUMN ingress_order INTEGER;
             CREATE INDEX IF NOT EXISTS idx_messages_scope_ingress
                 ON messages(platform, account_id, group_id, ingress_order)
                 WHERE ingress_order IS NOT NULL;
             PRAGMA user_version = 2;
             COMMIT;",
        )
        .context("migrating message history schema to version 2")?;
    }
    if version < 3 {
        conn.execute_batch(
            "BEGIN IMMEDIATE;
             ALTER TABLE context_boundaries RENAME TO context_boundaries_v2;
             CREATE TABLE context_boundaries (
                 platform TEXT NOT NULL,
                 account_id TEXT NOT NULL,
                 group_id TEXT NOT NULL,
                 persona_scope TEXT NOT NULL,
                 after_row_id INTEGER NOT NULL,
                 reset_at INTEGER NOT NULL,
                 PRIMARY KEY (platform, account_id, group_id, persona_scope)
             ) WITHOUT ROWID;
             INSERT INTO context_boundaries (
                 platform, account_id, group_id, persona_scope, after_row_id, reset_at
             )
             SELECT platform, account_id, group_id, 'default', after_row_id, reset_at
             FROM context_boundaries_v2;
             DROP TABLE context_boundaries_v2;
             PRAGMA user_version = 3;
             COMMIT;",
        )
        .context("migrating message history schema to version 3")?;
    }
    if version < 4 {
        conn.execute_batch(
            "BEGIN IMMEDIATE;
             DROP TRIGGER IF EXISTS messages_fts_insert;
             DROP TRIGGER IF EXISTS messages_fts_delete;
             DROP TRIGGER IF EXISTS messages_fts_update;
             DROP TABLE IF EXISTS messages_fts;

             ALTER TABLE messages RENAME TO messages_v3;
             ALTER TABLE recalls RENAME TO recalls_v3;
             ALTER TABLE context_boundaries RENAME TO context_boundaries_v3;

             CREATE TABLE messages (
                 id INTEGER PRIMARY KEY,
                 platform TEXT NOT NULL,
                 account_id TEXT NOT NULL,
                 conversation_kind TEXT NOT NULL
                     CHECK (conversation_kind IN ('group', 'private')),
                 conversation_id TEXT NOT NULL,
                 message_id TEXT NOT NULL,
                 sender_id TEXT NOT NULL,
                 sender_name TEXT NOT NULL,
                 text TEXT NOT NULL,
                 media_json TEXT NOT NULL,
                 mentions_json TEXT NOT NULL,
                 reply_to_message_id TEXT,
                 is_bot INTEGER NOT NULL CHECK (is_bot IN (0, 1)),
                 sent_at INTEGER NOT NULL,
                 ingress_order INTEGER,
                 recalled_at INTEGER,
                 recorded_at INTEGER NOT NULL DEFAULT (unixepoch()),
                 UNIQUE (
                     platform, account_id, conversation_kind, conversation_id, message_id
                 )
             );
             INSERT INTO messages (
                 id, platform, account_id, conversation_kind, conversation_id,
                 message_id, sender_id, sender_name, text, media_json, mentions_json,
                 reply_to_message_id, is_bot, sent_at, ingress_order, recalled_at,
                 recorded_at
             )
             SELECT id, platform, account_id, 'group', group_id, message_id, sender_id,
                    sender_name, text, media_json, mentions_json, reply_to_message_id,
                    is_bot, sent_at, ingress_order, recalled_at, recorded_at
             FROM messages_v3;

             CREATE TABLE recalls (
                 id INTEGER PRIMARY KEY,
                 platform TEXT NOT NULL,
                 account_id TEXT NOT NULL,
                 conversation_kind TEXT NOT NULL
                     CHECK (conversation_kind IN ('group', 'private')),
                 conversation_id TEXT NOT NULL,
                 message_id TEXT NOT NULL,
                 operator_id TEXT,
                 recalled_at INTEGER NOT NULL,
                 UNIQUE (
                     platform, account_id, conversation_kind, conversation_id, message_id
                 )
             );
             INSERT INTO recalls (
                 id, platform, account_id, conversation_kind, conversation_id,
                 message_id, operator_id, recalled_at
             )
             SELECT id, platform, account_id, 'group', group_id, message_id,
                    operator_id, recalled_at
             FROM recalls_v3;

             CREATE TABLE context_boundaries (
                 platform TEXT NOT NULL,
                 account_id TEXT NOT NULL,
                 conversation_kind TEXT NOT NULL
                     CHECK (conversation_kind IN ('group', 'private')),
                 conversation_id TEXT NOT NULL,
                 persona_scope TEXT NOT NULL,
                 after_row_id INTEGER NOT NULL,
                 reset_at INTEGER NOT NULL,
                 PRIMARY KEY (
                     platform, account_id, conversation_kind, conversation_id, persona_scope
                 )
             ) WITHOUT ROWID;
             INSERT INTO context_boundaries (
                 platform, account_id, conversation_kind, conversation_id,
                 persona_scope, after_row_id, reset_at
             )
             SELECT platform, account_id, 'group', group_id, persona_scope,
                    after_row_id, reset_at
             FROM context_boundaries_v3;

             DROP TABLE messages_v3;
             DROP TABLE recalls_v3;
             DROP TABLE context_boundaries_v3;

             CREATE INDEX idx_messages_scope_time
                 ON messages(
                     platform, account_id, conversation_kind, conversation_id,
                     sent_at DESC, id DESC
                 );
             CREATE INDEX idx_messages_account_time
                 ON messages(platform, account_id, sent_at DESC, id DESC);
             CREATE INDEX idx_messages_scope_sender_time
                 ON messages(
                     platform, account_id, conversation_kind, conversation_id,
                     sender_id, sent_at DESC, id DESC
                 );
             CREATE INDEX idx_messages_account_sender_time
                 ON messages(platform, account_id, sender_id, sent_at DESC, id DESC);
             CREATE INDEX idx_messages_scope_reply
                 ON messages(
                     platform, account_id, conversation_kind, conversation_id,
                     reply_to_message_id
                 )
                 WHERE reply_to_message_id IS NOT NULL;
             CREATE INDEX idx_messages_scope_ingress
                 ON messages(
                     platform, account_id, conversation_kind, conversation_id, ingress_order
                 )
                 WHERE ingress_order IS NOT NULL;

             CREATE INDEX idx_recalls_scope_time
                 ON recalls(
                     platform, account_id, conversation_kind, conversation_id,
                     recalled_at DESC, id DESC
                 );
             CREATE INDEX idx_recalls_account_time
                 ON recalls(platform, account_id, recalled_at DESC, id DESC);
             CREATE INDEX idx_recalls_scope_operator_time
                 ON recalls(
                     platform, account_id, conversation_kind, conversation_id,
                     operator_id, recalled_at DESC
                 )
                 WHERE operator_id IS NOT NULL;

             CREATE VIRTUAL TABLE messages_fts USING fts5(
                 text,
                 sender_name,
                 content='messages',
                 content_rowid='id',
                 tokenize='trigram'
             );
             INSERT INTO messages_fts(rowid, text, sender_name)
                 SELECT id, text, sender_name FROM messages;
             CREATE TRIGGER messages_fts_insert AFTER INSERT ON messages BEGIN
                 INSERT INTO messages_fts(rowid, text, sender_name)
                 VALUES (new.id, new.text, new.sender_name);
             END;
             CREATE TRIGGER messages_fts_delete AFTER DELETE ON messages BEGIN
                 INSERT INTO messages_fts(messages_fts, rowid, text, sender_name)
                 VALUES ('delete', old.id, old.text, old.sender_name);
             END;
             CREATE TRIGGER messages_fts_update
             AFTER UPDATE OF text, sender_name ON messages BEGIN
                 INSERT INTO messages_fts(messages_fts, rowid, text, sender_name)
                 VALUES ('delete', old.id, old.text, old.sender_name);
                 INSERT INTO messages_fts(rowid, text, sender_name)
                 VALUES (new.id, new.text, new.sender_name);
             END;
             PRAGMA user_version = 4;
             COMMIT;",
        )
        .context("migrating message history schema to version 4")?;
    }
    Ok(())
}

pub(crate) fn insert_messages(
    conn: &mut Connection,
    messages: Vec<NewHistoryMessage>,
) -> Result<Vec<RecordOutcome>> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut outcomes = Vec::with_capacity(messages.len());
    for message in messages {
        let media_json = serde_json::to_string(&message.content.media)?;
        let mentions_json = if message.content.mentioned_users.is_empty() {
            serde_json::to_string(&message.content.mentioned_user_ids)?
        } else {
            serde_json::to_string(&message.content.mentioned_users)?
        };
        let inserted = tx.execute(
            "INSERT OR IGNORE INTO messages (
                 platform, account_id, conversation_kind, conversation_id, message_id,
                 sender_id, sender_name, text, media_json, mentions_json,
                 reply_to_message_id, is_bot, sent_at, ingress_order, recalled_at
             ) VALUES (
                 ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                 (SELECT recalled_at FROM recalls
                  WHERE platform = ?1 AND account_id = ?2
                    AND conversation_kind = ?3 AND conversation_id = ?4
                    AND message_id = ?5)
             )",
            params![
                message.group.platform,
                message.group.account_id,
                message.group.conversation_kind,
                message.group.conversation_id,
                message.message_id,
                message.sender_id,
                message.sender_name,
                message.content.text,
                media_json,
                mentions_json,
                message.reply_to_message_id,
                message.is_bot,
                message.sent_at,
                message.ingress_order,
            ],
        )? != 0;
        let row_id = tx.query_row(
            "SELECT id FROM messages
             WHERE platform = ?1 AND account_id = ?2
               AND conversation_kind = ?3 AND conversation_id = ?4 AND message_id = ?5",
            params![
                message.group.platform,
                message.group.account_id,
                message.group.conversation_kind,
                message.group.conversation_id,
                message.message_id,
            ],
            |row| row.get(0),
        )?;
        outcomes.push(RecordOutcome { row_id, inserted });
    }
    tx.commit()?;
    Ok(outcomes)
}

pub(crate) fn insert_recall(conn: &mut Connection, recall: NewRecall) -> Result<RecallOutcome> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let existed: bool = tx.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM recalls
             WHERE platform = ?1 AND account_id = ?2
               AND conversation_kind = ?3 AND conversation_id = ?4 AND message_id = ?5
         )",
        params![
            recall.group.platform,
            recall.group.account_id,
            recall.group.conversation_kind,
            recall.group.conversation_id,
            recall.message_id,
        ],
        |row| row.get(0),
    )?;
    tx.execute(
        "INSERT INTO recalls (
             platform, account_id, conversation_kind, conversation_id,
             message_id, operator_id, recalled_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(
             platform, account_id, conversation_kind, conversation_id, message_id
         ) DO UPDATE SET
             operator_id = COALESCE(recalls.operator_id, excluded.operator_id),
             recalled_at = MIN(recalls.recalled_at, excluded.recalled_at)",
        params![
            recall.group.platform,
            recall.group.account_id,
            recall.group.conversation_kind,
            recall.group.conversation_id,
            recall.message_id,
            recall.operator_id,
            recall.recalled_at,
        ],
    )?;
    let matched_message = tx.execute(
        "UPDATE messages
         SET recalled_at = CASE
             WHEN recalled_at IS NULL THEN ?6
             ELSE MIN(recalled_at, ?6)
         END
         WHERE platform = ?1 AND account_id = ?2
           AND conversation_kind = ?3 AND conversation_id = ?4 AND message_id = ?5",
        params![
            recall.group.platform,
            recall.group.account_id,
            recall.group.conversation_kind,
            recall.group.conversation_id,
            recall.message_id,
            recall.recalled_at,
        ],
    )? != 0;
    tx.commit()?;
    Ok(RecallOutcome {
        newly_recorded: !existed,
        matched_message,
    })
}

pub(crate) fn upsert_boundary(
    conn: &Connection,
    group: &GroupKey,
    persona_scope: &str,
    reset_at: i64,
) -> Result<ContextBoundary> {
    let after_row_id = conn.query_row(
        "SELECT COALESCE(MAX(id), 0) FROM messages
         WHERE platform = ?1 AND account_id = ?2
           AND conversation_kind = ?3 AND conversation_id = ?4",
        params![
            group.platform,
            group.account_id,
            group.conversation_kind,
            group.conversation_id
        ],
        |row| row.get(0),
    )?;
    conn.execute(
        "INSERT INTO context_boundaries (
             platform, account_id, conversation_kind, conversation_id,
             persona_scope, after_row_id, reset_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(
             platform, account_id, conversation_kind, conversation_id, persona_scope
         ) DO UPDATE SET
             after_row_id = excluded.after_row_id,
             reset_at = excluded.reset_at",
        params![
            group.platform,
            group.account_id,
            group.conversation_kind,
            group.conversation_id,
            persona_scope,
            after_row_id,
            reset_at,
        ],
    )?;
    Ok(ContextBoundary {
        after_row_id,
        reset_at,
    })
}

pub(crate) fn read_boundary(
    conn: &Connection,
    group: &GroupKey,
    persona_scope: &str,
) -> Result<Option<ContextBoundary>> {
    conn.query_row(
        "SELECT after_row_id, reset_at FROM context_boundaries
         WHERE platform = ?1 AND account_id = ?2
           AND conversation_kind = ?3 AND conversation_id = ?4 AND persona_scope = ?5",
        params![
            group.platform,
            group.account_id,
            group.conversation_kind,
            group.conversation_id,
            persona_scope
        ],
        |row| {
            Ok(ContextBoundary {
                after_row_id: row.get(0)?,
                reset_at: row.get(1)?,
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

pub(crate) const MESSAGE_COLUMNS: &str = "m.id, m.platform, m.account_id, m.conversation_kind, \
    m.conversation_id, m.message_id, m.sender_id, m.sender_name, m.text, m.media_json, \
    m.mentions_json, m.reply_to_message_id, m.is_bot, m.sent_at, m.ingress_order, m.recalled_at";

pub(crate) fn query_recent(conn: &Connection, query: RecentQuery) -> Result<HistoryPage> {
    let page_size = page_size(query.limit);
    let fetch_size = page_size + 1;
    let before = query.before.unwrap_or(HistoryCursor {
        sent_at: i64::MAX,
        row_id: i64::MAX,
    });
    let boundary = if query.respect_context_boundary {
        read_boundary(conn, &query.group, &query.persona_scope)?
            .map(|boundary| boundary.after_row_id)
            .unwrap_or(0)
    } else {
        0
    };
    let sql = format!(
        "SELECT {MESSAGE_COLUMNS} FROM messages AS m
         WHERE m.platform = ?1 AND m.account_id = ?2
           AND m.conversation_kind = ?3 AND m.conversation_id = ?4
           AND m.id > ?5
           AND (?6 OR m.recalled_at IS NULL)
           AND (m.sent_at < ?7 OR (m.sent_at = ?7 AND m.id < ?8))
           AND (?9 IS NULL OR m.ingress_order IS NULL OR m.ingress_order < ?9)
           AND (?11 IS NULL OR (m.ingress_order IS NOT NULL AND m.ingress_order > ?11))
          ORDER BY
            CASE WHEN ?9 IS NOT NULL AND m.ingress_order IS NOT NULL THEN 0 ELSE 1 END ASC,
            CASE WHEN ?9 IS NOT NULL THEN m.ingress_order END DESC,
            m.sent_at DESC,
            m.id DESC
         LIMIT ?10"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(
        params![
            query.group.platform,
            query.group.account_id,
            query.group.conversation_kind,
            query.group.conversation_id,
            boundary,
            query.include_recalled,
            before.sent_at,
            before.row_id,
            query.before_ingress_order,
            fetch_size as i64,
            query.after_ingress_order,
        ],
        map_message,
    )?;
    let mut messages = Vec::with_capacity(page_size.min(64));
    let mut has_more = false;
    for row in rows {
        let message = row?;
        if messages.len() == page_size {
            has_more = true;
            break;
        }
        messages.push(message);
    }
    let next_cursor = has_more.then(|| cursor_for(messages.last().expect("non-empty page")));
    messages.reverse();
    Ok(HistoryPage {
        messages,
        next_cursor,
    })
}

pub(crate) fn query_search(conn: &Connection, query: SearchQuery) -> Result<HistoryPage> {
    let terms = search_terms(&query.text)?;
    let page_size = page_size(query.limit);
    let fetch_size = page_size + 1;
    let before = query.before.unwrap_or(HistoryCursor {
        sent_at: i64::MAX,
        row_id: i64::MAX,
    });
    let use_fts = !terms.is_empty() && terms.iter().all(|term| term.chars().count() >= 3);
    let mut arguments = Vec::<SqlValue>::new();
    let mut conditions = Vec::<String>::new();
    let from = if use_fts {
        arguments.push(SqlValue::Text(build_fts_query(&terms)));
        conditions.push("messages_fts MATCH ?1".to_string());
        "messages_fts JOIN messages AS m ON m.id = messages_fts.rowid"
    } else {
        for term in &terms {
            arguments.push(SqlValue::Text(term.clone()));
            let parameter = arguments.len();
            conditions.push(format!(
                "(instr(lower(m.text), lower(?{parameter})) > 0 OR \
                 instr(lower(m.sender_name), lower(?{parameter})) > 0)"
            ));
        }
        "messages AS m"
    };

    match &query.scope {
        HistoryScope::Group(conversation) | HistoryScope::Private(conversation) => {
            arguments.push(SqlValue::Text(conversation.platform.clone()));
            let platform = arguments.len();
            arguments.push(SqlValue::Text(conversation.account_id.clone()));
            let account = arguments.len();
            arguments.push(SqlValue::Text(conversation.conversation_kind.clone()));
            let kind = arguments.len();
            arguments.push(SqlValue::Text(conversation.conversation_id.clone()));
            let conversation_id = arguments.len();
            conditions.push(format!(
                "m.platform = ?{platform} AND m.account_id = ?{account} \
                 AND m.conversation_kind = ?{kind} AND m.conversation_id = ?{conversation_id}"
            ));
        }
        HistoryScope::Account(account) => {
            arguments.push(SqlValue::Text(account.platform.clone()));
            let platform = arguments.len();
            arguments.push(SqlValue::Text(account.account_id.clone()));
            let account = arguments.len();
            conditions.push(format!(
                "m.platform = ?{platform} AND m.account_id = ?{account}"
            ));
        }
    }

    if let Some(sender_id) = query.sender_id {
        arguments.push(SqlValue::Text(sender_id));
        let sender = arguments.len();
        conditions.push(format!("m.sender_id = ?{sender}"));
    }
    arguments.push(SqlValue::Integer(i64::from(query.include_recalled)));
    let recalled = arguments.len();
    conditions.push(format!("(?{recalled} OR m.recalled_at IS NULL)"));
    arguments.push(SqlValue::Integer(i64::from(query.include_bot)));
    let bot = arguments.len();
    conditions.push(format!("(?{bot} OR NOT m.is_bot)"));
    arguments.push(query.since.map(SqlValue::Integer).unwrap_or(SqlValue::Null));
    let since = arguments.len();
    conditions.push(format!("(?{since} IS NULL OR m.sent_at >= ?{since})"));
    arguments.push(query.until.map(SqlValue::Integer).unwrap_or(SqlValue::Null));
    let until = arguments.len();
    conditions.push(format!("(?{until} IS NULL OR m.sent_at <= ?{until})"));
    arguments.push(SqlValue::Integer(before.sent_at));
    let before_at = arguments.len();
    arguments.push(SqlValue::Integer(before.row_id));
    let before_id = arguments.len();
    conditions.push(format!(
        "(m.sent_at < ?{before_at} OR (m.sent_at = ?{before_at} AND m.id < ?{before_id}))"
    ));
    arguments.push(SqlValue::Integer(fetch_size as i64));
    let limit = arguments.len();

    let sql = format!(
        "SELECT {MESSAGE_COLUMNS} FROM {from}
         WHERE {}
         ORDER BY m.sent_at DESC, m.id DESC
         LIMIT ?{limit}",
        conditions.join(" AND ")
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(arguments.iter()), map_message)?;
    let mut messages = Vec::with_capacity(page_size.min(64));
    let mut has_more = false;
    for row in rows {
        let message = row?;
        if messages.len() == page_size {
            has_more = true;
            break;
        }
        messages.push(message);
    }
    let next_cursor = has_more.then(|| cursor_for(messages.last().expect("non-empty page")));
    Ok(HistoryPage {
        messages,
        next_cursor,
    })
}
