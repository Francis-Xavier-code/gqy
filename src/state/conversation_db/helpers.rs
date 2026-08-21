//! helpers — 会话数据库内部辅助函数（自 src/state/conversation_db.rs 拆分）。

use super::*;

pub(crate) fn delete_visible_turns_in_transaction(
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

pub(crate) fn verify_loaded_tool_sources(
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
pub(crate) fn turn_chars(turn: &Turn) -> usize {
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

pub(crate) fn load_redo_checkpoint_locked(
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

pub(crate) fn consume_stale_queued_prompts_locked(
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
pub(crate) fn touch_session_last_request(tx: &Transaction<'_>, turn_id: &str) -> Result<()> {
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
pub(crate) fn bump_completion_seq_locked(tx: &Transaction<'_>, turn_id: &str) -> Result<()> {
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

pub(crate) fn interrupted_projection_locked(
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

pub(crate) fn journal_segment_projection_locked(
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

pub(crate) fn interrupted_prefix(content: &str) -> String {
    let suffix = format!("\n\n{INTERRUPTED_TEXT}");
    content
        .strip_suffix(&suffix)
        .unwrap_or_else(|| content.strip_suffix(INTERRUPTED_TEXT).unwrap_or(content))
        .to_string()
}

pub(crate) fn restore_redo_backup_locked(
    tx: &Transaction<'_>,
    turn_id: &str,
    revision: i64,
) -> Result<bool> {
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
pub(crate) fn map_turn_row(row: &rusqlite::Row) -> rusqlite::Result<Turn> {
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

pub(crate) fn map_user_attachment_row(row: &rusqlite::Row) -> rusqlite::Result<UserAttachment> {
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

pub(crate) fn map_image_asset_row(row: &rusqlite::Row) -> rusqlite::Result<ImageAsset> {
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

pub(crate) fn map_artifact_asset_row(row: &rusqlite::Row) -> rusqlite::Result<ArtifactAsset> {
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

pub(crate) fn attach_turn_children_locked(conn: &Connection, turns: &mut [Turn]) -> Result<()> {
    attach_question_exchanges_locked(conn, turns)?;
    attach_followups_locked(conn, turns)?;
    attach_turn_attachments_locked(conn, turns)?;
    attach_turn_journal_events_locked(conn, turns)
}
