//! replay — 会话回放与 journal/附件装配（自 src/state/conversation_db.rs 拆分）。

use super::*;

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

pub(crate) fn store_replay_journal(tx: &Transaction, turn_id: &str) -> Result<()> {
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

pub(crate) fn truncate_chars_owned(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        return value.to_string();
    }
    let kept: String = value.chars().take(max).collect();
    format!("{kept}…")
}

pub(crate) fn attach_turn_journal_events_locked(
    conn: &Connection,
    turns: &mut [Turn],
) -> Result<()> {
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

pub(crate) fn attach_turn_attachments_locked(conn: &Connection, turns: &mut [Turn]) -> Result<()> {
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

pub(crate) fn attach_question_exchanges_locked(
    conn: &Connection,
    turns: &mut [Turn],
) -> Result<()> {
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

pub(crate) fn attach_followups_locked(conn: &Connection, turns: &mut [Turn]) -> Result<()> {
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

pub(crate) fn attach_prompt_attachments_locked(
    conn: &Connection,
    prompts: &mut [QueuedPrompt],
) -> Result<()> {
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

pub(crate) fn attach_followup_attachments_locked(
    conn: &Connection,
    turns: &mut [Turn],
) -> Result<()> {
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
