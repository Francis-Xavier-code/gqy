//! queries — 自 src/platforms/plugins/message_history/store.rs 拆分。

use super::*;

pub(crate) fn query_activity_ranking(
    conn: &Connection,
    query: ActivityRankingQuery,
) -> Result<ActivityRanking> {
    let mut stmt = conn.prepare(
        "WITH scoped AS (
             SELECT id,
                     CASE WHEN is_bot = 1 THEN ?2 ELSE sender_id END AS effective_sender_id,
                    sender_name,
                    sent_at
              FROM messages
              WHERE platform = ?1 AND account_id = ?2
                AND conversation_kind = ?3 AND conversation_id = ?4
                AND sent_at >= ?5 AND sent_at <= ?6
                AND (?7 OR is_bot = 0)
         ),
         named AS (
             SELECT effective_sender_id,
                    sender_name,
                    ROW_NUMBER() OVER (
                        PARTITION BY effective_sender_id
                        ORDER BY sent_at DESC, id DESC
                    ) AS name_rank
             FROM scoped
         ),
         aggregated AS (
             SELECT effective_sender_id,
                    COUNT(*) AS message_count,
                    COUNT(DISTINCT date(sent_at, 'unixepoch', 'localtime')) AS active_days,
                    MIN(sent_at) AS first_sent_at,
                    MAX(sent_at) AS last_sent_at
             FROM scoped
             GROUP BY effective_sender_id
         ),
         ranked AS (
             SELECT ROW_NUMBER() OVER (
                        ORDER BY aggregated.message_count DESC,
                                 aggregated.last_sent_at DESC,
                                 aggregated.effective_sender_id ASC
                    ) AS rank,
                    aggregated.effective_sender_id,
                    COALESCE(named.sender_name, aggregated.effective_sender_id) AS sender_name,
                    aggregated.message_count,
                    aggregated.active_days,
                    aggregated.first_sent_at,
                    aggregated.last_sent_at,
                    SUM(aggregated.message_count) OVER () AS total_messages,
                    COUNT(*) OVER () AS participant_count
             FROM aggregated
             LEFT JOIN named
               ON named.effective_sender_id = aggregated.effective_sender_id
              AND named.name_rank = 1
         )
         SELECT rank, effective_sender_id, sender_name, message_count, active_days,
                first_sent_at, last_sent_at, total_messages, participant_count
         FROM ranked
         ORDER BY rank
         LIMIT ?8",
    )?;
    let rows = stmt.query_map(
        params![
            query.group.platform,
            query.group.account_id,
            query.group.conversation_kind,
            query.group.conversation_id,
            query.since,
            query.until,
            query.include_bot,
            query.limit as i64,
        ],
        |row| {
            Ok((
                ActivityRankingItem {
                    rank: row.get(0)?,
                    sender_id: row.get(1)?,
                    sender_name: row.get(2)?,
                    message_count: row.get(3)?,
                    active_days: row.get(4)?,
                    first_sent_at: row.get(5)?,
                    last_sent_at: row.get(6)?,
                },
                row.get::<_, u64>(7)?,
                row.get::<_, u64>(8)?,
            ))
        },
    )?;
    let mut items = Vec::with_capacity(query.limit.min(32));
    let mut total_messages = 0;
    let mut participant_count = 0;
    for row in rows {
        let (item, total, participants) = row?;
        total_messages = total;
        participant_count = participants;
        items.push(item);
    }
    Ok(ActivityRanking {
        total_messages,
        participant_count,
        items,
    })
}

pub(crate) fn delete_history(
    conn: &mut Connection,
    request: DeleteRequest,
) -> Result<DeleteReport> {
    let cutoff = match request.mode {
        DeleteMode::All => None,
        DeleteMode::KeepDays(days) => Some(
            request
                .now
                .saturating_sub(i64::from(days).saturating_mul(SECONDS_PER_DAY)),
        ),
    };
    let batch_size = request.batch_size.clamp(1, MAX_DELETE_BATCH_SIZE);
    let mut report = DeleteReport::default();

    loop {
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let deleted = delete_message_batch(
            &tx,
            &request.scope,
            cutoff,
            request.sender_id.as_deref(),
            request.since,
            request.until,
            batch_size,
        )?;
        tx.commit()?;
        if deleted == 0 {
            break;
        }
        report.messages_deleted = report.messages_deleted.saturating_add(deleted as u64);
        report.batches = report.batches.saturating_add(1);
    }

    let delete_auxiliary =
        request.sender_id.is_none() && request.since.is_none() && request.until.is_none();
    if delete_auxiliary {
        loop {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let deleted = delete_recall_batch(&tx, &request.scope, cutoff, batch_size)?;
            tx.commit()?;
            if deleted == 0 {
                break;
            }
            report.recalls_deleted = report.recalls_deleted.saturating_add(deleted as u64);
            report.batches = report.batches.saturating_add(1);
        }

        let boundary_tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        report.boundaries_deleted = delete_boundaries(&boundary_tx, &request.scope, cutoff)? as u64;
        clamp_boundaries_to_current_rowid(&boundary_tx)?;
        boundary_tx.commit()?;
    }

    // Never run a full VACUUM in the daemon. Reclaim a bounded number of pages
    // after an explicit admin purge and let later purges continue the work.
    conn.execute_batch("PRAGMA wal_checkpoint(PASSIVE); PRAGMA incremental_vacuum(256);")?;
    Ok(report)
}

pub(crate) fn delete_message_batch(
    tx: &Transaction<'_>,
    scope: &HistoryScope,
    cutoff: Option<i64>,
    sender_id: Option<&str>,
    since: Option<i64>,
    until: Option<i64>,
    batch_size: usize,
) -> Result<usize> {
    match scope {
        HistoryScope::Group(conversation) | HistoryScope::Private(conversation) => Ok(tx.execute(
            "DELETE FROM messages WHERE id IN (
                 SELECT id FROM messages
                 WHERE platform = ?1 AND account_id = ?2
                   AND conversation_kind = ?3 AND conversation_id = ?4
                   AND (?5 IS NULL OR sent_at < ?5)
                   AND (?6 IS NULL OR sender_id = ?6)
                   AND (?7 IS NULL OR sent_at >= ?7)
                   AND (?8 IS NULL OR sent_at <= ?8)
                 ORDER BY id LIMIT ?9
             )",
            params![
                conversation.platform,
                conversation.account_id,
                conversation.conversation_kind,
                conversation.conversation_id,
                cutoff,
                sender_id,
                since,
                until,
                batch_size as i64,
            ],
        )?),
        HistoryScope::Account(account) => Ok(tx.execute(
            "DELETE FROM messages WHERE id IN (
                 SELECT id FROM messages
                 WHERE platform = ?1 AND account_id = ?2
                   AND (?3 IS NULL OR sent_at < ?3)
                   AND (?4 IS NULL OR sender_id = ?4)
                   AND (?5 IS NULL OR sent_at >= ?5)
                   AND (?6 IS NULL OR sent_at <= ?6)
                 ORDER BY id LIMIT ?7
             )",
            params![
                account.platform,
                account.account_id,
                cutoff,
                sender_id,
                since,
                until,
                batch_size as i64,
            ],
        )?),
    }
}

pub(crate) fn delete_recall_batch(
    tx: &Transaction<'_>,
    scope: &HistoryScope,
    cutoff: Option<i64>,
    batch_size: usize,
) -> Result<usize> {
    match scope {
        HistoryScope::Group(conversation) | HistoryScope::Private(conversation) => Ok(tx.execute(
            "DELETE FROM recalls WHERE id IN (
                 SELECT r.id FROM recalls AS r
                 WHERE r.platform = ?1 AND r.account_id = ?2
                   AND r.conversation_kind = ?3 AND r.conversation_id = ?4
                   AND (?5 IS NULL OR (
                       r.recalled_at < ?5 AND NOT EXISTS (
                           SELECT 1 FROM messages AS m
                           WHERE m.platform = r.platform AND m.account_id = r.account_id
                             AND m.conversation_kind = r.conversation_kind
                             AND m.conversation_id = r.conversation_id
                             AND m.message_id = r.message_id
                       )
                   ))
                 ORDER BY r.id LIMIT ?6
             )",
            params![
                conversation.platform,
                conversation.account_id,
                conversation.conversation_kind,
                conversation.conversation_id,
                cutoff,
                batch_size as i64,
            ],
        )?),
        HistoryScope::Account(account) => Ok(tx.execute(
            "DELETE FROM recalls WHERE id IN (
                 SELECT r.id FROM recalls AS r
                 WHERE r.platform = ?1 AND r.account_id = ?2
                   AND (?3 IS NULL OR (
                       r.recalled_at < ?3 AND NOT EXISTS (
                           SELECT 1 FROM messages AS m
                           WHERE m.platform = r.platform AND m.account_id = r.account_id
                             AND m.conversation_kind = r.conversation_kind
                             AND m.conversation_id = r.conversation_id
                             AND m.message_id = r.message_id
                       )
                   ))
                 ORDER BY r.id LIMIT ?4
             )",
            params![
                account.platform,
                account.account_id,
                cutoff,
                batch_size as i64,
            ],
        )?),
    }
}

pub(crate) fn delete_boundaries(
    tx: &Transaction<'_>,
    scope: &HistoryScope,
    cutoff: Option<i64>,
) -> Result<usize> {
    match scope {
        HistoryScope::Group(conversation) | HistoryScope::Private(conversation) => Ok(tx.execute(
            "DELETE FROM context_boundaries
             WHERE platform = ?1 AND account_id = ?2
               AND conversation_kind = ?3 AND conversation_id = ?4
               AND (?5 IS NULL OR reset_at < ?5)",
            params![
                conversation.platform,
                conversation.account_id,
                conversation.conversation_kind,
                conversation.conversation_id,
                cutoff
            ],
        )?),
        HistoryScope::Account(account) => Ok(tx.execute(
            "DELETE FROM context_boundaries
              WHERE platform = ?1 AND account_id = ?2
               AND (?3 IS NULL OR reset_at < ?3)",
            params![account.platform, account.account_id, cutoff],
        )?),
    }
}

pub(crate) fn clamp_boundaries_to_current_rowid(conn: &Connection) -> Result<()> {
    // `INTEGER PRIMARY KEY` may reuse lower rowids after the highest messages
    // are deleted. A retained reset boundary must therefore never remain above
    // the current global maximum, or later messages could stay hidden until
    // their reused rowids eventually pass that stale boundary.
    let maximum_row_id: i64 =
        conn.query_row("SELECT COALESCE(MAX(id), 0) FROM messages", [], |row| {
            row.get(0)
        })?;
    conn.execute(
        "UPDATE context_boundaries
         SET after_row_id = ?1
         WHERE after_row_id > ?1",
        params![maximum_row_id],
    )?;
    Ok(())
}

pub(crate) fn map_message(row: &rusqlite::Row<'_>) -> rusqlite::Result<HistoryMessage> {
    let media_json: String = row.get(9)?;
    let media = serde_json::from_str(&media_json).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(9, rusqlite::types::Type::Text, Box::new(error))
    })?;
    let mentions_json: String = row.get(10)?;
    #[derive(Deserialize)]
    #[serde(untagged)]
    pub(crate) enum StoredMentions {
        Users(Vec<PlatformMention>),
        Ids(Vec<String>),
    }
    let (mentioned_user_ids, mentioned_users) =
        match serde_json::from_str(&mentions_json).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                10,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })? {
            StoredMentions::Users(users) => (
                users
                    .iter()
                    .map(|mention| mention.user_id.clone())
                    .collect(),
                users,
            ),
            StoredMentions::Ids(ids) => (ids, Vec::new()),
        };
    Ok(HistoryMessage {
        row_id: row.get(0)?,
        group: GroupKey {
            platform: row.get(1)?,
            account_id: row.get(2)?,
            conversation_kind: row.get(3)?,
            conversation_id: row.get(4)?,
        },
        message_id: row.get(5)?,
        sender_id: row.get(6)?,
        sender_name: row.get(7)?,
        content: SanitizedContent {
            text: row.get(8)?,
            media,
            mentioned_user_ids,
            mentioned_users,
        },
        reply_to_message_id: row.get(11)?,
        is_bot: row.get(12)?,
        sent_at: row.get(13)?,
        ingress_order: row.get(14)?,
        recalled_at: row.get(15)?,
    })
}

pub(crate) fn cursor_for(message: &HistoryMessage) -> HistoryCursor {
    HistoryCursor {
        sent_at: message.sent_at,
        row_id: message.row_id,
    }
}

pub(crate) fn search_terms(text: &str) -> Result<Vec<String>> {
    let text = sanitize_multiline(text, MAX_SEARCH_BYTES);
    let terms = text
        .split_whitespace()
        .filter(|term| !term.is_empty())
        .take(MAX_SEARCH_TERMS)
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    Ok(terms)
}

pub(crate) fn build_fts_query(terms: &[String]) -> String {
    terms
        .iter()
        .map(|term| format!("\"{}\"", term.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" AND ")
}

pub(crate) fn page_size(requested: usize) -> usize {
    requested.clamp(1, MAX_PAGE_SIZE)
}

pub(crate) fn validate_identifier(label: &str, value: String) -> Result<String> {
    let value = value.trim();
    if value.is_empty() {
        bail!("{label} cannot be empty");
    }
    if value.len() > MAX_IDENTIFIER_BYTES {
        bail!("{label} exceeds {MAX_IDENTIFIER_BYTES} bytes");
    }
    if value.chars().any(char::is_control) {
        bail!("{label} contains control characters");
    }
    Ok(value.to_string())
}

pub(crate) fn sanitize_multiline(value: &str, max_bytes: usize) -> String {
    let filtered = value
        .chars()
        .filter(|character| !character.is_control() || matches!(character, '\n' | '\r' | '\t'))
        .collect::<String>();
    truncate_utf8(filtered.trim(), max_bytes)
}

pub(crate) fn sanitize_single_line(value: &str, max_bytes: usize) -> String {
    let filtered = value
        .chars()
        .map(|character| {
            if character.is_control() || matches!(character, '\n' | '\r' | '\t') {
                ' '
            } else {
                character
            }
        })
        .collect::<String>();
    truncate_utf8(filtered.trim(), max_bytes)
}

pub(crate) fn truncate_utf8(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}
