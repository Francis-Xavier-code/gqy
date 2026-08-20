//! queued_prompts — 排队提示词与 redo 相关操作（自 src/state/conversation_db/search.rs 拆分）。

use super::super::*;

impl ConversationDb {
    pub fn user_attachments_for_prompt(&self, prompt_id: &str) -> Result<Vec<UserAttachment>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT attachment_id, file_name, mime, kind, size_bytes, width,
                    height, created_at FROM user_attachments
             WHERE prompt_id = ?1 ORDER BY created_at, attachment_id",
        )?;
        let attachments = stmt
            .query_map(params![prompt_id], map_user_attachment_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(attachments)
    }

    pub fn consume_queued_prompts(
        &self,
        session_id: &str,
        turn_id: &str,
        prompts: &[(String, String)],
        preceding_assistant_content: Option<&str>,
        preceding_assistant_reasoning: Option<&str>,
        preceding_assistant_provider_id: Option<&str>,
        preceding_assistant_model: Option<&str>,
        queue_session_id: &str,
    ) -> Result<()> {
        self.consume_queued_prompts_with_checkpoint(
            session_id,
            turn_id,
            prompts,
            preceding_assistant_content,
            preceding_assistant_reasoning,
            preceding_assistant_provider_id,
            preceding_assistant_model,
            queue_session_id,
            None,
        )
    }

    pub fn consume_queued_prompts_with_checkpoint(
        &self,
        session_id: &str,
        turn_id: &str,
        prompts: &[(String, String)],
        preceding_assistant_content: Option<&str>,
        preceding_assistant_reasoning: Option<&str>,
        preceding_assistant_provider_id: Option<&str>,
        preceding_assistant_model: Option<&str>,
        queue_session_id: &str,
        mut checkpoint: Option<TurnRedoCheckpointPayload>,
    ) -> Result<()> {
        if prompts.is_empty() {
            return Ok(());
        }
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let running: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM turns WHERE turn_id = ?1 AND status = 'running')",
            params![turn_id],
            |row| row.get(0),
        )?;
        if !running {
            bail!("cannot consume queued prompts into a non-running turn");
        }
        if let Some(checkpoint) = checkpoint.as_mut() {
            checkpoint.prefix_question_count = tx.query_row(
                "SELECT COUNT(*) FROM question_exchanges WHERE turn_id = ?1",
                params![turn_id],
                |row| row.get::<_, i64>(0),
            )? as usize;
            checkpoint.prefix_image_asset_ids = {
                let mut stmt = tx.prepare(
                    "SELECT asset_id FROM image_assets WHERE turn_id = ?1 ORDER BY created_at, asset_id",
                )?;
                let rows = stmt
                    .query_map(params![turn_id], |row| row.get::<_, String>(0))?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                rows
            };
            checkpoint.prefix_artifact_asset_ids = {
                let mut stmt = tx.prepare(
                    "SELECT asset_id FROM artifact_assets
                     WHERE turn_id = ?1 ORDER BY updated_at, asset_id",
                )?;
                let rows = stmt
                    .query_map(params![turn_id], |row| row.get::<_, String>(0))?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                rows
            };
            checkpoint.loaded_items = {
                let mut stmt = tx.prepare(
                    "SELECT kind, name, source_turn_id FROM session_loaded_items
                     WHERE session_id = ?1 ORDER BY kind, name",
                )?;
                let rows = stmt
                    .query_map(params![session_id], |row| {
                        Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                    })?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                rows
            };
        }
        let consumed_at = Utc::now().to_rfc3339();
        for (index, (prompt_id, context_content)) in prompts.iter().enumerate() {
            let preceding_content = (index == 0)
                .then_some(preceding_assistant_content)
                .flatten();
            let preceding_reasoning = (index == 0)
                .then_some(preceding_assistant_reasoning)
                .flatten();
            let affected = tx.execute(
                "UPDATE queued_prompts
                  SET status = 'consumed', consumed_at = ?1, turn_id = ?2,
                      context_content = ?3, preceding_assistant_content = ?4,
                      preceding_assistant_reasoning = ?5,
                      preceding_assistant_provider_id = ?6,
                      preceding_assistant_model = ?7
                   WHERE prompt_id = ?8 AND status = 'queued' AND session_id = ?9
                     AND queue_session_id = ?10",
                params![
                    consumed_at,
                    turn_id,
                    context_content,
                    preceding_content,
                    preceding_reasoning,
                    preceding_assistant_provider_id,
                    preceding_assistant_model,
                    prompt_id,
                    session_id,
                    queue_session_id
                ],
            )?;
            if affected != 1 {
                bail!("queued prompt changed before it could be consumed: {prompt_id}");
            }
        }
        let batch_prompt_ids = prompts
            .iter()
            .map(|(prompt_id, _)| prompt_id.as_str())
            .collect::<Vec<_>>();
        let batch_prompt_ids = serde_json::to_string(&batch_prompt_ids)?;
        let (payload, unavailable_reason) = match checkpoint {
            Some(checkpoint) => {
                let payload = serde_json::to_vec(&checkpoint)?;
                if payload.len() <= MAX_REDO_CHECKPOINT_BYTES {
                    (Some(payload), None)
                } else {
                    (
                        None,
                        Some(format!(
                            "replay checkpoint exceeds the {} byte limit",
                            MAX_REDO_CHECKPOINT_BYTES
                        )),
                    )
                }
            }
            None => (None, Some("replay checkpoint was not captured".to_string())),
        };
        tx.execute(
            "INSERT INTO turn_redo_checkpoints
                (turn_id, version, batch_prompt_ids, payload, unavailable_reason, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(turn_id) DO UPDATE SET
                version = excluded.version,
                batch_prompt_ids = excluded.batch_prompt_ids,
                payload = excluded.payload,
                unavailable_reason = excluded.unavailable_reason,
                created_at = excluded.created_at",
            params![
                turn_id,
                REDO_CHECKPOINT_VERSION,
                batch_prompt_ids,
                payload,
                unavailable_reason,
                consumed_at
            ],
        )?;
        let revision: i64 = tx.query_row(
            "SELECT revision FROM turns WHERE turn_id = ?1 AND status = 'running'",
            params![turn_id],
            |row| row.get(0),
        )?;
        tx.execute(
            "INSERT OR IGNORE INTO turn_journal_segments
                (turn_id, revision, segment_index, status, started_at)
             VALUES (?1, ?2, 0, 'running', ?3)",
            params![turn_id, revision, consumed_at],
        )?;
        let (segment_index, segment_status): (i64, String) = tx.query_row(
            "SELECT segment_index, status FROM turn_journal_segments
             WHERE turn_id = ?1 AND revision = ?2
             ORDER BY segment_index DESC LIMIT 1",
            params![turn_id, revision],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let next_segment = segment_index.saturating_add(1);
        let prompt_payload =
            serde_json::to_string(&prompts.iter().map(|(id, _)| id).collect::<Vec<_>>())?;
        if segment_status == "superseded" {
            tx.execute(
                "INSERT INTO turn_journal_segments
                    (turn_id, revision, segment_index, status, started_at)
                 VALUES (?1, ?2, ?3, 'running', ?4)",
                params![turn_id, revision, next_segment, consumed_at],
            )?;
            tx.execute(
                "INSERT INTO turn_journal_events
                    (turn_id, revision, segment_index, kind, text_payload, created_at)
                 VALUES (?1, ?2, ?3, 'queued_prompts_consumed', ?4, ?5)",
                params![turn_id, revision, next_segment, prompt_payload, consumed_at],
            )?;
        } else {
            tx.execute(
                "INSERT INTO turn_journal_events
                    (turn_id, revision, segment_index, kind, text_payload, created_at)
                 VALUES (?1, ?2, ?3, 'queued_prompts_consumed', ?4, ?5)",
                params![
                    turn_id,
                    revision,
                    segment_index,
                    prompt_payload,
                    consumed_at
                ],
            )?;
        }
        if segment_status == "running" {
            tx.execute(
                "UPDATE turn_journal_segments
                 SET status = 'completed', finished_at = ?1
                 WHERE turn_id = ?2 AND revision = ?3 AND segment_index = ?4",
                params![consumed_at, turn_id, revision, segment_index],
            )?;
        }
        if segment_status != "superseded" {
            tx.execute(
                "INSERT INTO turn_journal_segments
                    (turn_id, revision, segment_index, status, started_at)
                 VALUES (?1, ?2, ?3, 'running', ?4)",
                params![turn_id, revision, next_segment, consumed_at],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn redo_candidate(&self, session_id: &str) -> Result<Option<RedoCandidate>> {
        let conn = self.conn.lock().unwrap();
        let last = conn
            .query_row(
                "SELECT turn_id, revision, display_content, status
                 FROM turns
                 WHERE session_id = ?1 AND hidden = 0 AND is_summary = 0
                 ORDER BY seq DESC LIMIT 1",
                params![session_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                },
            )
            .optional()?;
        let Some((turn_id, revision, display_content, status)) = last else {
            return Ok(None);
        };
        if status == "running" {
            return Ok(None);
        }

        let consumed = {
            let mut stmt = conn.prepare(
                "SELECT prompt_id, display_content
                 FROM queued_prompts
                 WHERE turn_id = ?1 AND status = 'consumed'
                 ORDER BY seq ASC",
            )?;
            let rows = stmt
                .query_map(params![turn_id], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            rows
        };
        if consumed.is_empty() {
            return Ok(Some(RedoCandidate {
                input_id: turn_id.clone(),
                turn_id,
                revision,
                input_kind: RedoInputKind::Initial,
                display_content,
                batch_prompt_ids: Vec::new(),
            }));
        }

        let checkpoint = load_redo_checkpoint_locked(&conn, &turn_id)?;
        let Some(checkpoint) = checkpoint.filter(|checkpoint| checkpoint.payload.is_some()) else {
            return Ok(None);
        };
        if checkpoint.batch_prompt_ids.is_empty()
            || checkpoint.batch_prompt_ids.len() > consumed.len()
        {
            return Ok(None);
        }
        let suffix = &consumed[consumed.len() - checkpoint.batch_prompt_ids.len()..];
        if !suffix
            .iter()
            .map(|(prompt_id, _)| prompt_id)
            .eq(checkpoint.batch_prompt_ids.iter())
        {
            return Ok(None);
        }
        let (input_id, display_content) = suffix.last().cloned().expect("non-empty suffix");
        Ok(Some(RedoCandidate {
            turn_id,
            revision,
            input_id,
            input_kind: RedoInputKind::Followup,
            display_content,
            batch_prompt_ids: checkpoint.batch_prompt_ids,
        }))
    }

    pub fn load_redo_batch_prompts(
        &self,
        session_id: &str,
        turn_id: &str,
        prompt_ids: &[String],
    ) -> Result<Vec<QueuedPrompt>> {
        if prompt_ids.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT prompt_id, seq, COALESCE(context_content, content), display_content,
                    attachments, submitted_at
             FROM queued_prompts
             WHERE session_id = ?1 AND turn_id = ?2 AND status = 'consumed'
             ORDER BY seq ASC",
        )?;
        let mut prompts = stmt
            .query_map(params![session_id, turn_id], |row| {
                Ok(QueuedPrompt {
                    prompt_id: row.get(0)?,
                    seq: row.get(1)?,
                    content: row.get(2)?,
                    display_content: row.get(3)?,
                    attachments: serde_json::from_str(&row.get::<_, String>(4)?)
                        .unwrap_or_default(),
                    uploaded_attachments: Vec::new(),
                    submitted_at: row.get(5)?,
                })
            })?
            .filter_map(|row| match row {
                Ok(prompt) if prompt_ids.contains(&prompt.prompt_id) => Some(Ok(prompt)),
                Ok(_) => None,
                Err(error) => Some(Err(error)),
            })
            .collect::<std::result::Result<Vec<_>, _>>()?;
        drop(stmt);
        if prompts.len() != prompt_ids.len()
            || !prompts
                .iter()
                .map(|prompt| &prompt.prompt_id)
                .eq(prompt_ids.iter())
        {
            bail!("redo follow-up batch changed before it could be loaded");
        }
        attach_prompt_attachments_locked(&conn, &mut prompts)?;
        Ok(prompts)
    }

    pub fn begin_redo(
        &self,
        session_id: &str,
        turn_id: &str,
        input_id: &str,
        input_kind: RedoInputKind,
        expected_revision: i64,
        content: &str,
        display_content: &str,
        owner_pid: u32,
        queue_session_id: &str,
    ) -> Result<RedoStart> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let latest = tx
            .query_row(
                "SELECT turn_id, revision, status
                 FROM turns
                 WHERE session_id = ?1 AND hidden = 0 AND is_summary = 0
                 ORDER BY seq DESC LIMIT 1",
                params![session_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?;
        let Some((latest_turn_id, revision, status)) = latest else {
            bail!("redo target no longer exists");
        };
        if latest_turn_id != turn_id || revision != expected_revision || status == "running" {
            bail!("conversation changed before redo could start");
        }
        let other_running: i64 = tx.query_row(
            "SELECT COUNT(*) FROM turns
             WHERE session_id = ?1 AND status = 'running' AND turn_id != ?2",
            params![session_id, turn_id],
            |row| row.get(0),
        )?;
        if other_running != 0 {
            bail!("another turn is already running in this conversation");
        }

        let (
            user_content,
            old_display_content,
            assistant_content,
            assistant_reasoning,
            assistant_provider_id,
            assistant_model,
            assistant_timestamp,
            tool_reports,
            old_owner_pid,
            old_queue_session_id,
            token_total,
            token_usage_estimated,
            token_prompt,
            token_cache_read,
        ) = tx.query_row(
            "SELECT user_content, display_content, assistant_content, assistant_reasoning,
                    assistant_provider_id, assistant_model, assistant_timestamp, tool_reports,
                    owner_pid, queue_session_id, token_total, token_usage_estimated,
                    token_prompt, token_cache_read
             FROM turns WHERE turn_id = ?1",
            params![turn_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                    row.get(9)?,
                    row.get(10)?,
                    row.get(11)?,
                    row.get(12)?,
                    row.get(13)?,
                ))
            },
        )?;
        let followup = if input_kind == RedoInputKind::Followup {
            tx.query_row(
                "SELECT content, display_content, context_content
                 FROM queued_prompts
                 WHERE prompt_id = ?1 AND turn_id = ?2 AND status = 'consumed'",
                params![input_id, turn_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                },
            )
            .optional()?
        } else {
            None
        };
        let loaded_items = {
            let mut stmt = tx.prepare(
                "SELECT kind, name, source_turn_id, created_at, updated_at
                 FROM session_loaded_items WHERE session_id = ?1 ORDER BY kind, name",
            )?;
            let rows = stmt
                .query_map(params![session_id], |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            rows
        };
        let consumed_prompt_ids = {
            let mut stmt = tx.prepare(
                "SELECT prompt_id FROM queued_prompts
                 WHERE turn_id = ?1 AND status = 'consumed' ORDER BY seq",
            )?;
            let rows = stmt
                .query_map(params![turn_id], |row| row.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            rows
        };
        let checkpoint_backup = tx
            .query_row(
                "SELECT version, batch_prompt_ids, payload, unavailable_reason, created_at
                 FROM turn_redo_checkpoints WHERE turn_id = ?1",
                params![turn_id],
                |row| {
                    Ok(RedoCheckpointBackup {
                        version: row.get(0)?,
                        batch_prompt_ids: row.get(1)?,
                        payload: row.get(2)?,
                        unavailable_reason: row.get(3)?,
                        created_at: row.get(4)?,
                    })
                },
            )
            .optional()?;
        let backup = TurnRedoBackup {
            status,
            user_content,
            display_content: old_display_content,
            followup_content: followup.as_ref().map(|value| value.0.clone()),
            followup_display_content: followup.as_ref().map(|value| value.1.clone()),
            followup_context_content: followup.and_then(|value| value.2),
            assistant_content,
            assistant_reasoning,
            assistant_provider_id,
            assistant_model,
            assistant_timestamp,
            tool_reports,
            owner_pid: old_owner_pid,
            queue_session_id: old_queue_session_id,
            token_total,
            token_prompt,
            token_cache_read,
            token_usage_estimated,
            loaded_items,
            consumed_prompt_ids,
            checkpoint: checkpoint_backup,
        };
        let backup_payload = serde_json::to_vec(&backup)?;
        let redo_revision = expected_revision.saturating_add(1);
        let backup_created_at = Utc::now().to_rfc3339();
        tx.execute(
            "INSERT INTO turn_redo_backups (turn_id, revision, payload, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![turn_id, redo_revision, backup_payload, backup_created_at],
        )?;
        tx.execute(
            "INSERT INTO turn_redo_question_backups (turn_id, exchange_index, payload)
             SELECT turn_id, exchange_index, payload FROM question_exchanges WHERE turn_id = ?1",
            params![turn_id],
        )?;
        tx.execute(
            "INSERT INTO turn_redo_image_backups
                (turn_id, asset_id, tool_id, mime, width, height, alt, data, created_at)
             SELECT turn_id, asset_id, tool_id, mime, width, height, alt, data, created_at
             FROM image_assets WHERE turn_id = ?1",
            params![turn_id],
        )?;
        tx.execute(
            "INSERT INTO turn_redo_artifact_backups
                (turn_id, asset_id, tool_id, source_key, file_name, mime, kind,
                 size_bytes, data, created_at, updated_at)
             SELECT ?1, asset_id, tool_id, source_key, file_name, mime, kind,
                    size_bytes, data, created_at, updated_at
             FROM artifact_assets WHERE turn_id = ?1",
            params![turn_id],
        )?;

        let checkpoint = match input_kind {
            RedoInputKind::Initial => {
                if input_id != turn_id {
                    bail!("redo input no longer matches the initial prompt");
                }
                let followups: i64 = tx.query_row(
                    "SELECT COUNT(*) FROM queued_prompts
                     WHERE turn_id = ?1 AND status = 'consumed'",
                    params![turn_id],
                    |row| row.get(0),
                )?;
                if followups != 0 {
                    bail!("the last input changed before redo could start");
                }
                tx.execute(
                    "DELETE FROM question_exchanges WHERE turn_id = ?1",
                    params![turn_id],
                )?;
                tx.execute(
                    "DELETE FROM image_assets WHERE turn_id = ?1",
                    params![turn_id],
                )?;
                tx.execute(
                    "DELETE FROM artifact_assets WHERE turn_id = ?1",
                    params![turn_id],
                )?;
                tx.execute(
                    "DELETE FROM session_loaded_items
                     WHERE session_id = ?1 AND source_turn_id = ?2",
                    params![session_id, turn_id],
                )?;
                tx.execute(
                    "UPDATE turns SET user_content = ?1, display_content = ?2
                     WHERE turn_id = ?3",
                    params![content, display_content, turn_id],
                )?;
                None
            }
            RedoInputKind::Followup => {
                let checkpoint = load_redo_checkpoint_locked(&tx, turn_id)?
                    .and_then(|checkpoint| checkpoint.payload)
                    .context("redo checkpoint is unavailable")?;
                let row = tx
                    .query_row(
                        "SELECT prompt_id FROM queued_prompts
                         WHERE turn_id = ?1 AND status = 'consumed'
                         ORDER BY seq DESC LIMIT 1",
                        params![turn_id],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()?;
                if row.as_deref() != Some(input_id) {
                    bail!("the last follow-up changed before redo could start");
                }
                tx.execute(
                    "DELETE FROM question_exchanges
                     WHERE turn_id = ?1 AND exchange_index >= ?2",
                    params![turn_id, checkpoint.prefix_question_count as i64],
                )?;
                let prefix_assets = checkpoint
                    .prefix_image_asset_ids
                    .iter()
                    .collect::<std::collections::HashSet<_>>();
                let current_assets = {
                    let mut stmt =
                        tx.prepare("SELECT asset_id FROM image_assets WHERE turn_id = ?1")?;
                    let rows = stmt
                        .query_map(params![turn_id], |row| row.get::<_, String>(0))?
                        .collect::<std::result::Result<Vec<_>, _>>()?;
                    rows
                };
                for asset_id in current_assets {
                    if !prefix_assets.contains(&asset_id) {
                        tx.execute(
                            "DELETE FROM image_assets WHERE asset_id = ?1",
                            params![asset_id],
                        )?;
                    }
                }
                let prefix_artifacts = checkpoint
                    .prefix_artifact_asset_ids
                    .iter()
                    .collect::<std::collections::HashSet<_>>();
                let current_artifacts = {
                    let mut stmt =
                        tx.prepare("SELECT asset_id FROM artifact_assets WHERE turn_id = ?1")?;
                    let rows = stmt
                        .query_map(params![turn_id], |row| row.get::<_, String>(0))?
                        .collect::<std::result::Result<Vec<_>, _>>()?;
                    rows
                };
                for asset_id in current_artifacts {
                    if !prefix_artifacts.contains(&asset_id) {
                        tx.execute(
                            "DELETE FROM artifact_assets WHERE asset_id = ?1",
                            params![asset_id],
                        )?;
                    }
                }
                tx.execute(
                    "DELETE FROM session_loaded_items WHERE session_id = ?1",
                    params![session_id],
                )?;
                let now = Utc::now().to_rfc3339();
                for (kind, name, source_turn_id) in &checkpoint.loaded_items {
                    tx.execute(
                        "INSERT INTO session_loaded_items
                            (session_id, kind, name, source_turn_id, created_at, updated_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
                        params![session_id, kind, name, source_turn_id, now],
                    )?;
                }
                tx.execute(
                    "UPDATE queued_prompts
                     SET content = ?1, display_content = ?2, context_content = ?1
                     WHERE prompt_id = ?3 AND turn_id = ?4 AND status = 'consumed'",
                    params![content, display_content, input_id, turn_id],
                )?;
                Some(checkpoint)
            }
        };

        let prefix_reports = checkpoint
            .as_ref()
            .map(|checkpoint| checkpoint.prefix_tool_reports.as_slice())
            .unwrap_or_default();
        let prefix_reports = serde_json::to_string(prefix_reports)?;
        let prefix_question_count = checkpoint
            .as_ref()
            .map(|checkpoint| checkpoint.prefix_question_count)
            .unwrap_or(0);
        let now = Utc::now().to_rfc3339();
        let affected = tx.execute(
            "UPDATE turns SET
                assistant_content = ?1,
                assistant_reasoning = NULL,
                assistant_provider_id = NULL,
                assistant_model = NULL,
                assistant_timestamp = NULL,
                status = 'running',
                tool_reports = ?2,
                owner_pid = ?3,
                queue_session_id = ?4,
                token_total = 0,
                token_usage_estimated = 0,
                revision = revision + 1
             WHERE turn_id = ?5 AND session_id = ?6 AND revision = ?7 AND status != 'running'",
            params![
                PENDING_PLACEHOLDER,
                prefix_reports,
                owner_pid as i64,
                queue_session_id,
                turn_id,
                session_id,
                expected_revision
            ],
        )?;
        if affected != 1 {
            bail!("conversation changed before redo could be claimed");
        }
        tx.execute(
            "UPDATE sessions SET updated_at = ?2 WHERE session_id = ?1",
            params![session_id, now],
        )?;
        tx.execute(
            "INSERT INTO turn_journal_segments
                (turn_id, revision, segment_index, status, started_at)
             VALUES (?1, ?2, 0, 'running', ?3)",
            params![turn_id, redo_revision, now],
        )?;
        tx.execute(
            "INSERT INTO turn_journal_events
                (turn_id, revision, segment_index, kind, text_payload, created_at)
             VALUES (?1, ?2, 0, 'redo_prefix_question_count', ?3, ?4)",
            params![
                turn_id,
                redo_revision,
                prefix_question_count.to_string(),
                now
            ],
        )?;
        tx.commit()?;
        Ok(RedoStart {
            revision: expected_revision.saturating_add(1),
            checkpoint,
        })
    }

    pub fn discard_queued_prompts(
        &self,
        session_id: &str,
        queue_session_id: &str,
    ) -> Result<usize> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let turn = tx
            .query_row(
                "SELECT turn_id, status, revision, assistant_content, assistant_reasoning
                 FROM turns
                 WHERE session_id = ?1 AND queue_session_id = ?2
                 ORDER BY seq DESC LIMIT 1",
                params![session_id, queue_session_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, Option<String>>(4)?,
                    ))
                },
            )
            .optional()?;
        let Some((turn_id, status, revision, assistant_content, assistant_reasoning)) = turn else {
            let deleted = tx.execute(
                "DELETE FROM queued_prompts
                 WHERE status = 'queued' AND session_id = ?1 AND queue_session_id = ?2",
                params![session_id, queue_session_id],
            )?;
            tx.commit()?;
            return Ok(deleted);
        };
        if status == "running" {
            let deleted = tx.execute(
                "DELETE FROM queued_prompts
                 WHERE status = 'queued' AND session_id = ?1 AND queue_session_id = ?2",
                params![session_id, queue_session_id],
            )?;
            tx.commit()?;
            return Ok(deleted);
        }

        let now = Utc::now().to_rfc3339();
        let preceding_content = if status == "interrupted" {
            interrupted_prefix(&assistant_content)
        } else {
            assistant_content
        };
        let preceding_content = (!preceding_content.trim().is_empty()).then_some(preceding_content);
        let mut stmt = tx.prepare(
            "SELECT prompt_id FROM queued_prompts
             WHERE status = 'queued' AND session_id = ?1 AND queue_session_id = ?2
             ORDER BY seq",
        )?;
        let prompt_ids = stmt
            .query_map(params![session_id, queue_session_id], |row| {
                row.get::<_, String>(0)
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        drop(stmt);
        for (index, prompt_id) in prompt_ids.iter().enumerate() {
            tx.execute(
                "UPDATE queued_prompts
                 SET status = 'consumed', consumed_at = ?1, turn_id = ?2,
                     context_content = content,
                     preceding_assistant_content = ?3,
                     preceding_assistant_reasoning = ?4
                 WHERE prompt_id = ?5 AND status = 'queued'",
                params![
                    now,
                    turn_id,
                    (index == 0)
                        .then_some(preceding_content.as_deref())
                        .flatten(),
                    (index == 0)
                        .then_some(assistant_reasoning.as_deref())
                        .flatten(),
                    prompt_id,
                ],
            )?;
        }
        if status == "interrupted" && !prompt_ids.is_empty() {
            let next_segment: i64 = tx.query_row(
                "SELECT COALESCE(MAX(segment_index), -1) + 1
                 FROM turn_journal_segments WHERE turn_id = ?1 AND revision = ?2",
                params![turn_id, revision],
                |row| row.get(0),
            )?;
            tx.execute(
                "INSERT INTO turn_journal_segments
                    (turn_id, revision, segment_index, status, started_at, finished_at)
                 VALUES (?1, ?2, ?3, 'interrupted', ?4, ?4)",
                params![turn_id, revision, next_segment, now],
            )?;
            tx.execute(
                "INSERT INTO turn_journal_events
                    (turn_id, revision, segment_index, kind, text_payload, created_at)
                 VALUES (?1, ?2, ?3, 'queued_prompts_consumed', ?4, ?5)",
                params![
                    turn_id,
                    revision,
                    next_segment,
                    serde_json::to_string(&prompt_ids)?,
                    now
                ],
            )?;
        }
        tx.commit()?;
        Ok(prompt_ids.len())
    }

    pub fn remove_queued_prompt(
        &self,
        session_id: &str,
        prompt_id: &str,
        queue_session_id: &str,
    ) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        Ok(conn.execute(
            "DELETE FROM queued_prompts
             WHERE prompt_id = ?1 AND status = 'queued' AND session_id = ?2
               AND queue_session_id = ?3",
            params![prompt_id, session_id, queue_session_id],
        )? == 1)
    }

    /// Hard-drop every still-queued prompt of a queue session and return
    /// their ids. Unlike `discard_queued_prompts` this never folds prompts
    /// into the conversation: it backs an explicit user cancel, where the
    /// queued follow-ups are withdrawn rather than preserved as context.
    pub fn delete_queued_prompts(
        &self,
        session_id: &str,
        queue_session_id: &str,
    ) -> Result<Vec<String>> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let prompt_ids = {
            let mut stmt = tx.prepare(
                "SELECT prompt_id FROM queued_prompts
                 WHERE status = 'queued' AND session_id = ?1 AND queue_session_id = ?2
                 ORDER BY seq",
            )?;
            let prompt_ids = stmt
                .query_map(params![session_id, queue_session_id], |row| {
                    row.get::<_, String>(0)
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            prompt_ids
        };
        if !prompt_ids.is_empty() {
            tx.execute(
                "DELETE FROM queued_prompts
                 WHERE status = 'queued' AND session_id = ?1 AND queue_session_id = ?2",
                params![session_id, queue_session_id],
            )?;
        }
        tx.commit()?;
        Ok(prompt_ids)
    }

    pub fn discard_stale_queued_prompts(
        &self,
        current_session_id: &str,
        _current_pid: u32,
    ) -> Result<usize> {
        let mut conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT q.prompt_id, q.queue_session_id, q.owner_pid,
                    EXISTS(
                        SELECT 1 FROM turns t
                        WHERE t.status = 'running'
                          AND t.queue_session_id = q.queue_session_id
                    )
             FROM queued_prompts q WHERE q.status = 'queued'",
        )?;
        let queued_prompts = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                    row.get::<_, bool>(3)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let stale_prompt_ids = queued_prompts
            .into_iter()
            .filter_map(|row| {
                let (prompt_id, session_id, owner_pid, belongs_to_running_turn) = row;
                if session_id.as_deref() == Some(current_session_id) {
                    return None;
                }
                if belongs_to_running_turn {
                    return None;
                }
                let owner_pid = owner_pid.and_then(|pid| u32::try_from(pid).ok());
                // Multiple stores in the daemon share a PID. A different
                // queue identity owned by this live process may belong to an
                // active parent turn, so only dead owners are stale here.
                let stale =
                    session_id.is_none() || !owner_pid.is_some_and(crate::alarm::process_exists);
                stale.then_some(prompt_id)
            })
            .collect::<Vec<_>>();
        drop(stmt);
        if stale_prompt_ids.is_empty() {
            return Ok(0);
        }
        let tx = conn.transaction()?;
        let mut discarded = 0usize;
        for prompt_id in stale_prompt_ids {
            discarded += tx.execute(
                "DELETE FROM queued_prompts WHERE prompt_id = ?1 AND status = 'queued'",
                params![prompt_id],
            )?;
        }
        tx.commit()?;
        Ok(discarded)
    }
}
