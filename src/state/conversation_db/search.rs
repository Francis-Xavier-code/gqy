//! search — 自 src/state/conversation_db.rs 拆分。

#![allow(clippy::redundant_closure)]
pub(crate) use super::*;

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

    pub fn load_session_loaded_items(
        &self,
        session_id: &str,
        kind: &str,
    ) -> Result<std::collections::BTreeSet<String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT name FROM session_loaded_items
             WHERE session_id = ?1 AND kind = ?2 ORDER BY name ASC",
        )?;
        let items = stmt
            .query_map(params![session_id, kind], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<std::collections::BTreeSet<_>, _>>()?;
        Ok(items)
    }

    pub fn load_session_loaded_items_with_sources(
        &self,
        session_id: &str,
        kind: &str,
    ) -> Result<Vec<(String, Option<String>)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT name, source_turn_id FROM session_loaded_items
             WHERE session_id = ?1 AND kind = ?2 ORDER BY name ASC",
        )?;
        let items = stmt
            .query_map(params![session_id, kind], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(items)
    }

    pub fn add_session_loaded_items(
        &self,
        session_id: &str,
        kind: &str,
        names: &[String],
        source_turn_id: Option<&str>,
    ) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        let now = Utc::now().to_rfc3339();
        let mut affected = 0usize;
        for name in names
            .iter()
            .map(|name| name.trim())
            .filter(|name| !name.is_empty())
        {
            affected += conn.execute(
                "INSERT INTO session_loaded_items (session_id, kind, name, source_turn_id, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?5)
                 ON CONFLICT(session_id, kind, name) DO UPDATE SET
                    source_turn_id = COALESCE(excluded.source_turn_id, session_loaded_items.source_turn_id),
                    updated_at = excluded.updated_at",
                params![session_id, kind, name, source_turn_id, now],
            )?;
        }
        Ok(affected)
    }

    pub fn load_turns(&self, session_id: &str) -> Result<Vec<Turn>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT turn_id, seq, user_content, display_content, user_timestamp, assistant_content,
                    assistant_reasoning, assistant_provider_id, assistant_model, assistant_timestamp, status, tool_reports, hidden, is_summary, owner_pid,
                    token_total, token_usage_estimated, revision, context_messages, token_prompt, token_cache_read, tool_flow
             FROM turns WHERE session_id = ?1 ORDER BY seq ASC",
        )?;
        let mut turns = stmt
            .query_map(params![session_id], map_turn_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        attach_turn_children_locked(&conn, &mut turns)?;
        Ok(turns)
    }

    #[allow(dead_code)]
    pub fn load_turns_excluding(
        &self,
        session_id: &str,
        exclude_turn_id: &str,
    ) -> Result<Vec<Turn>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT turn_id, seq, user_content, display_content, user_timestamp, assistant_content,
                    assistant_reasoning, assistant_provider_id, assistant_model, assistant_timestamp, status, tool_reports, hidden, is_summary, owner_pid,
                    token_total, token_usage_estimated, revision, context_messages, token_prompt, token_cache_read, tool_flow
             FROM turns WHERE session_id = ?1 AND turn_id != ?2 ORDER BY seq ASC",
        )?;
        let mut turns = stmt
            .query_map(params![session_id, exclude_turn_id], map_turn_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        attach_turn_children_locked(&conn, &mut turns)?;
        Ok(turns)
    }

    #[allow(dead_code)]
    pub fn load_turns_for_context(&self, session_id: &str) -> Result<Vec<Turn>> {
        self.load_turns(session_id)
    }

    pub fn load_visible_turns(&self, session_id: &str) -> Result<Vec<Turn>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT turn_id, seq, user_content, display_content, user_timestamp, assistant_content,
                    assistant_reasoning, assistant_provider_id, assistant_model, assistant_timestamp, status, tool_reports, hidden, is_summary, owner_pid,
                    token_total, token_usage_estimated, revision, context_messages, token_prompt, token_cache_read, tool_flow
             FROM turns WHERE session_id = ?1 AND hidden = 0 ORDER BY seq ASC",
        )?;
        let mut turns = stmt
            .query_map(params![session_id], map_turn_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        attach_turn_children_locked(&conn, &mut turns)?;
        Ok(turns)
    }

    pub fn load_visible_turns_excluding(
        &self,
        session_id: &str,
        exclude_turn_id: &str,
    ) -> Result<Vec<Turn>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT turn_id, seq, user_content, display_content, user_timestamp, assistant_content,
                    assistant_reasoning, assistant_provider_id, assistant_model, assistant_timestamp, status, tool_reports, hidden, is_summary, owner_pid,
                    token_total, token_usage_estimated, revision, context_messages, token_prompt, token_cache_read, tool_flow
             FROM turns WHERE session_id = ?1 AND hidden = 0 AND turn_id != ?2 ORDER BY seq ASC",
        )?;
        let mut turns = stmt
            .query_map(params![session_id, exclude_turn_id], map_turn_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        attach_turn_children_locked(&conn, &mut turns)?;
        Ok(turns)
    }

    #[allow(dead_code)]
    pub fn hide_turns_before_seq(&self, session_id: &str, seq: i64) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        let affected = conn.execute(
            "UPDATE turns SET hidden = 1 WHERE session_id = ?1 AND seq <= ?2",
            params![session_id, seq],
        )?;
        Ok(affected)
    }

    #[allow(dead_code)]
    pub fn insert_summary_turn(
        &self,
        session_id: &str,
        summary: &str,
        tokens: TurnTokens,
        token_usage_estimated: bool,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let turn_id = format!(
            "summary_{}_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0),
            rand::random::<u16>()
        );
        let seq = self.next_seq_locked(&conn, session_id)?;
        let now = Utc::now().to_rfc3339();
        let token_usage_estimated = i64::from(token_usage_estimated);
        conn.execute(
            "INSERT INTO turns (turn_id, session_id, seq, user_content, user_timestamp, assistant_content, assistant_timestamp, status, tool_reports, hidden, is_summary, token_total, token_usage_estimated, token_prompt, token_cache_read)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'completed', '[]', 0, 1, ?8, ?9, ?10, ?11)",
            params![turn_id, session_id, seq, "[conversation summary]", now, summary, now, tokens.total as i64, token_usage_estimated, tokens.prompt as i64, tokens.cache_read as i64],
        )?;
        Ok(())
    }

    pub fn load_last_summary(&self, session_id: &str) -> Result<Option<Turn>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT turn_id, seq, user_content, display_content, user_timestamp, assistant_content,
                    assistant_reasoning, assistant_provider_id, assistant_model, assistant_timestamp, status, tool_reports, hidden, is_summary, owner_pid,
                    token_total, token_usage_estimated, revision, context_messages, token_prompt, token_cache_read, tool_flow
             FROM turns WHERE session_id = ?1 AND is_summary = 1 AND hidden = 0 ORDER BY seq DESC LIMIT 1",
        )?;
        let turn = stmt
            .query_map(params![session_id], map_turn_row)?
            .next()
            .transpose()?;
        Ok(turn)
    }

    #[allow(dead_code)]
    pub fn count_turns(&self, session_id: &str) -> Result<i64> {
        let conn = self.conn.lock().unwrap();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM turns WHERE session_id = ?1",
            params![session_id],
            |row| row.get(0),
        )?;
        Ok(count)
    }

    #[allow(dead_code)]
    pub fn total_chars(&self, session_id: &str) -> Result<usize> {
        let turns = self.load_turns(session_id)?;
        Ok(turns.iter().map(|t| turn_chars(t)).sum())
    }

    #[allow(dead_code)]
    pub fn trim_oldest_turns(&self, session_id: &str, count: usize) -> Result<Vec<Turn>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT turn_id, seq, user_content, display_content, user_timestamp, assistant_content,
                    assistant_reasoning, assistant_provider_id, assistant_model, assistant_timestamp, status, tool_reports, hidden, is_summary, owner_pid,
                    token_total, token_usage_estimated, revision, context_messages, token_prompt, token_cache_read, tool_flow
             FROM turns WHERE session_id = ?1 AND is_summary = 0 ORDER BY seq ASC LIMIT ?2",
        )?;
        let mut to_remove: Vec<Turn> = stmt
            .query_map(params![session_id, count as i64], map_turn_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        drop(stmt);
        attach_turn_children_locked(&conn, &mut to_remove)?;
        for turn in &to_remove {
            conn.execute(
                "DELETE FROM turns WHERE turn_id = ?1",
                params![turn.turn_id],
            )?;
        }
        Ok(to_remove)
    }

    pub fn oldest_evictable_visible_turns(
        &self,
        session_id: &str,
        count: usize,
    ) -> Result<Vec<Turn>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT turn_id, seq, user_content, display_content, user_timestamp, assistant_content,
                    assistant_reasoning, assistant_provider_id, assistant_model, assistant_timestamp, status, tool_reports, hidden, is_summary, owner_pid,
                    token_total, token_usage_estimated, revision, context_messages, token_prompt, token_cache_read, tool_flow
             FROM turns
             WHERE session_id = ?1 AND hidden = 0 AND is_summary = 0 AND status != 'running'
             ORDER BY seq ASC LIMIT ?2",
        )?;
        let count = i64::try_from(count).unwrap_or(i64::MAX);
        let mut turns = stmt
            .query_map(params![session_id, count], map_turn_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        attach_turn_children_locked(&conn, &mut turns)?;
        Ok(turns)
    }

    pub fn delete_visible_turns(&self, session_id: &str, turn_ids: &[String]) -> Result<usize> {
        self.delete_visible_turns_checked(session_id, turn_ids, None)
    }

    pub fn delete_visible_turns_checked(
        &self,
        session_id: &str,
        turn_ids: &[String],
        expected_loaded_tools: Option<&[(String, Option<String>)]>,
    ) -> Result<usize> {
        if turn_ids.is_empty() {
            return Ok(0);
        }
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        verify_loaded_tool_sources(&tx, session_id, expected_loaded_tools)?;
        let affected = delete_visible_turns_in_transaction(&tx, session_id, turn_ids)?;
        tx.commit()?;
        Ok(affected)
    }

    pub fn archive_and_delete_visible_turns(
        &self,
        session_id: &str,
        archive_db: &Path,
        turns: &[EvictedTurn],
        turn_ids: &[String],
        expected_loaded_tools: Option<&[(String, Option<String>)]>,
    ) -> Result<usize> {
        if turn_ids.is_empty() {
            return Ok(0);
        }
        let mut conn = self.conn.lock().unwrap();
        let archive_db = archive_db.to_string_lossy().into_owned();
        let archive_alias = format!("evicted_context_{}", rand::random::<u32>());
        conn.execute(
            &format!("ATTACH DATABASE ?1 AS {archive_alias}"),
            params![archive_db],
        )?;
        let insert_sql = format!(
            "INSERT OR IGNORE INTO {archive_alias}.evicted_turns
             (source_id, timestamp, role, content, created_at,
              visibility, owner_principal, owner_display_name)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)"
        );
        let operation = (|| -> Result<usize> {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            verify_loaded_tool_sources(&tx, session_id, expected_loaded_tools)?;
            let created_at = Utc::now().to_rfc3339();
            for turn in turns {
                tx.execute(
                    &insert_sql,
                    params![
                        turn.source_id,
                        turn.timestamp,
                        turn.role,
                        turn.content,
                        created_at,
                        turn.visibility,
                        turn.owner_principal,
                        turn.owner_display_name,
                    ],
                )?;
            }
            let affected = delete_visible_turns_in_transaction(&tx, session_id, turn_ids)?;
            tx.commit()?;
            Ok(affected)
        })();
        let detach = conn.execute_batch(&format!("DETACH DATABASE {archive_alias}"));
        if let Err(detach_err) = detach {
            tracing::warn!(
                error = %detach_err,
                archive_alias,
                "{}",
                crate::i18n::text(
                    "failed to detach evicted-context database",
                    "分离已移出上下文的数据库失败",
                )
            );
        }
        operation
    }

    /// Mechanical prune: replaces old visible turns' tool_reports with a
    /// one-line placeholder (tool output is re-derivable — files can be
    /// re-read, commands re-run). All-or-nothing behind a harvest gate:
    /// rewriting history is a prefix-cache reset, so it only happens when the
    /// batch saves enough to pay for that reset. Write-once archive keeps the
    /// original JSON; a turn with an archive is never rewritten again, which
    /// makes the prune monotonic (repeat calls never re-crater the cache).
    pub fn prune_stale_tool_reports(
        &self,
        session_id: &str,
        protect_recent: usize,
        min_saved_chars: usize,
    ) -> Result<PruneStats> {
        const MIN_PRUNE_BYTES: usize = 1024;
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let rows: Vec<(String, String, Option<String>)> = {
            let mut stmt = tx.prepare(
                "SELECT turn_id, tool_reports, tool_reports_archive FROM turns
                 WHERE session_id = ?1 AND hidden = 0 AND is_summary = 0
                   AND status = 'completed'
                 ORDER BY seq ASC",
            )?;
            let rows = stmt
                .query_map(params![session_id], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            rows
        };
        let eligible = rows.len().saturating_sub(protect_recent);
        let mut updates = Vec::new();
        let mut saved_chars = 0usize;
        for (turn_id, reports_json, archive) in rows.into_iter().take(eligible) {
            if archive.is_some() {
                continue;
            }
            let reports: Vec<String> = serde_json::from_str(&reports_json).unwrap_or_default();
            if reports.is_empty() {
                continue;
            }
            let total: usize = reports.iter().map(|report| report.len()).sum();
            if total < MIN_PRUNE_BYTES {
                continue;
            }
            let placeholder = format!(
                "[{} 条旧工具记录已折叠以释放上下文 — 原文已归档；需要该数据时请重新调用工具 / {} old tool report(s) elided to free context — re-run the tool if the data is needed again]",
                reports.len(),
                reports.len(),
            );
            saved_chars += total.saturating_sub(placeholder.len());
            let new_json = serde_json::to_string(&vec![placeholder])?;
            updates.push((turn_id, reports_json, new_json));
        }
        if updates.is_empty() || saved_chars < min_saved_chars {
            tx.rollback()?;
            return Ok(PruneStats::default());
        }
        let turns = updates.len();
        {
            let mut stmt = tx.prepare(
                "UPDATE turns SET tool_reports_archive = ?2, tool_reports = ?3
                 WHERE turn_id = ?1 AND session_id = ?4",
            )?;
            for (turn_id, original, replacement) in &updates {
                stmt.execute(params![turn_id, original, replacement, session_id])?;
            }
        }
        tx.commit()?;
        Ok(PruneStats { turns, saved_chars })
    }

    pub fn replace_visible_with_summary(
        &self,
        session_id: &str,
        fold_turn_ids: &[String],
        visible_turn_ids: &[String],
        summary: &str,
        tokens: TurnTokens,
        token_usage_estimated: bool,
        footprint_json: Option<&str>,
    ) -> Result<()> {
        if summary.trim().is_empty() {
            bail!("compact returned an empty summary");
        }
        if fold_turn_ids.is_empty() {
            bail!("compact selected no turns to fold");
        }

        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let current_turn_ids = {
            let mut stmt = tx.prepare(
                "SELECT turn_id FROM turns
                 WHERE session_id = ?1 AND hidden = 0 ORDER BY seq ASC",
            )?;
            let turn_ids = stmt
                .query_map(params![session_id], |row| row.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            turn_ids
        };
        if current_turn_ids != visible_turn_ids {
            bail!("conversation changed while compact was running");
        }
        // The previous summary (if any) is superseded by the merged one and
        // folds together with the selected turns. Tail turns keep lower seqs
        // than the old summary row, so membership is by explicit id, not by a
        // seq watermark.
        let prior_summary_ids = {
            let mut stmt = tx.prepare(
                "SELECT turn_id FROM turns
                 WHERE session_id = ?1 AND hidden = 0 AND is_summary = 1",
            )?;
            let ids = stmt
                .query_map(params![session_id], |row| row.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            ids
        };
        let parent_summary_seq: Option<i64> = tx.query_row(
            "SELECT MAX(seq) FROM turns
                 WHERE session_id = ?1 AND hidden = 0 AND is_summary = 1",
            params![session_id],
            |row| row.get(0),
        )?;
        let mut hidden_ids: Vec<String> = fold_turn_ids.to_vec();
        for id in prior_summary_ids {
            if !hidden_ids.contains(&id) {
                hidden_ids.push(id);
            }
        }
        let mut hidden = 0usize;
        {
            let mut stmt = tx.prepare(
                "UPDATE turns SET hidden = 1
                 WHERE session_id = ?1 AND hidden = 0 AND turn_id = ?2",
            )?;
            for id in &hidden_ids {
                hidden += stmt.execute(params![session_id, id])?;
            }
        }
        if hidden == 0 {
            bail!("conversation changed before compact could be saved");
        }
        let hidden_json = serde_json::to_string(&hidden_ids)?;

        let turn_id = format!(
            "summary_{}_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0),
            rand::random::<u16>()
        );
        let seq: i64 = tx.query_row(
            "SELECT COALESCE(MAX(seq), 0) + 1 FROM turns WHERE session_id = ?1",
            params![session_id],
            |row| row.get(0),
        )?;
        let now = Utc::now().to_rfc3339();
        let token_total = tokens.total as i64;
        let token_usage_estimated = i64::from(token_usage_estimated);
        tx.execute(
            "INSERT INTO turns (turn_id, session_id, seq, user_content, user_timestamp, assistant_content, assistant_timestamp, status, tool_reports, hidden, is_summary, token_total, token_usage_estimated, token_prompt, token_cache_read, compact_reversible, compact_parent_summary_seq, compact_hidden_json, tool_footprint)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'completed', '[]', 0, 1, ?8, ?9, ?13, ?14, 1, ?10, ?11, ?12)",
            params![turn_id, session_id, seq, "[conversation summary]", now, summary, now, token_total, token_usage_estimated, parent_summary_seq, hidden_json, footprint_json, tokens.prompt as i64, tokens.cache_read as i64],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn reset(&self, session_id: &str) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM queued_prompts WHERE session_id = ?1",
            params![session_id],
        )?;
        tx.execute(
            "DELETE FROM turns WHERE session_id = ?1",
            params![session_id],
        )?;
        tx.execute(
            "DELETE FROM session_loaded_items WHERE session_id = ?1",
            params![session_id],
        )?;
        // Subagent audit sessions now count toward this session's Σ, so a
        // reset that left them behind would zero the history and still report
        // a running total. They are records of a conversation that no longer
        // exists; they go with it.
        tx.execute(
            "DELETE FROM sessions WHERE parent_session_id = ?1 AND kind = 'subagent'",
            params![session_id],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn reset_persona_contexts(&self, persona: &str, platform: &str) -> Result<Vec<String>> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let target_sql = "WITH RECURSIVE targets(session_id) AS (
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
             )";
        let session_ids = {
            let mut stmt = tx.prepare(&format!(
                "{target_sql} SELECT session_id FROM targets ORDER BY session_id"
            ))?;
            let rows = stmt.query_map(params![persona, platform], |row| row.get(0))?;
            rows.collect::<std::result::Result<Vec<_>, _>>()?
        };
        for table in ["queued_prompts", "turns", "session_loaded_items"] {
            tx.execute(
                &format!(
                    "{target_sql} DELETE FROM {table} WHERE session_id IN (SELECT session_id FROM targets)"
                ),
                params![persona, platform],
            )?;
        }
        // Subagent runs bill to the session that launched them, and their usage
        // lives on the session row rather than in `turns` — deleting the turns
        // alone would leave every Σ still carrying the subagent totals of a
        // conversation that no longer exists.
        tx.execute(
            &format!(
                "{target_sql} DELETE FROM sessions
                  WHERE kind = 'subagent' AND session_id IN (SELECT session_id FROM targets)"
            ),
            params![persona, platform],
        )?;
        tx.commit()?;
        Ok(session_ids)
    }

    /// Lifetime token total of one session, summed over every turn row —
    /// including hidden (compacted) turns and summary rows, so the counter
    /// keeps growing across compactions and only /reset (which deletes the
    /// rows) brings it back to zero.
    pub fn session_token_total(&self, session_id: &str) -> Result<u64> {
        Ok(self.session_token_totals(session_id)?.total)
    }

    /// Session-lifetime sums behind the Σ meter. Returned together because the
    /// cumulative cache rate is `cache_read / prompt` and reading the two
    /// halves through separate locks could straddle a turn commit.
    pub fn session_token_totals(&self, session_id: &str) -> Result<TurnTokens> {
        let conn = self.conn.lock().unwrap();
        let (total, prompt, cache_read): (i64, i64, i64) = conn.query_row(
            "SELECT COALESCE(SUM(token_total), 0), COALESCE(SUM(token_prompt), 0),
                    COALESCE(SUM(token_cache_read), 0)
             FROM turns WHERE session_id = ?1",
            rusqlite::params![session_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        // Subagents bill to the session that launched them: their audit
        // sessions hang off this one, and a Σ that ignored them would hide the
        // single biggest thing a turn can spend. Estimated runs land in
        // `total_tokens` only — `prompt_tokens` stays 0 when the provider
        // reported nothing — so a guessed number can inflate Σ but never
        // reaches the cache rate's denominator.
        let (sub_total, sub_prompt, sub_cache): (i64, i64, i64) = conn.query_row(
            "SELECT COALESCE(SUM(total_tokens), 0),
                    COALESCE(SUM(CASE WHEN cache_read_tokens IS NULL THEN 0
                                      ELSE prompt_tokens END), 0),
                    COALESCE(SUM(cache_read_tokens), 0)
             FROM sessions WHERE parent_session_id = ?1 AND kind = 'subagent'",
            rusqlite::params![session_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        Ok(TurnTokens {
            total: total.saturating_add(sub_total).max(0) as u64,
            prompt: prompt.saturating_add(sub_prompt).max(0) as u64,
            cache_read: cache_read.saturating_add(sub_cache).max(0) as u64,
        })
    }

    pub fn reset_history(&self, session_id: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM turns WHERE session_id = ?1",
            params![session_id],
        )?;
        conn.execute(
            "DELETE FROM session_loaded_items WHERE session_id = ?1",
            params![session_id],
        )?;
        Ok(())
    }

    pub fn undo_last_turn(&self, session_id: &str) -> Result<(usize, Option<String>)> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let running: i64 = tx.query_row(
            "SELECT COUNT(*) FROM turns WHERE session_id = ?1 AND hidden = 0 AND status = 'running'",
            params![session_id],
            |row| row.get(0),
        )?;
        if running > 0 {
            tx.rollback()?;
            return Ok((0, None));
        }
        let last: Option<(String, i64, String, bool, bool, Option<i64>, Option<String>)> = tx
            .query_row(
                "SELECT turn_id, seq, user_content, is_summary,
                        compact_reversible, compact_parent_summary_seq, compact_hidden_json
                 FROM turns WHERE session_id = ?1 AND hidden = 0 ORDER BY seq DESC LIMIT 1",
                params![session_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get::<_, i64>(3)? != 0,
                        row.get::<_, i64>(4)? != 0,
                        row.get(5)?,
                        row.get(6)?,
                    ))
                },
            )
            .optional()?;
        match last {
            Some((turn_id, _, user_content, false, _, _, _)) => {
                tx.execute("DELETE FROM turns WHERE turn_id = ?1", params![turn_id])?;
                tx.commit()?;
                Ok((1, Some(user_content)))
            }
            Some((_, _, _, true, false, _, _)) => {
                tx.rollback()?;
                Ok((0, None))
            }
            Some((turn_id, _summary_seq, _, true, true, _, Some(hidden_json))) => {
                // Tail-retention era summary: restore exactly the set this
                // compaction hid (folded turns + the superseded summary row).
                let hidden_ids: Vec<String> =
                    serde_json::from_str(&hidden_json).unwrap_or_default();
                if hidden_ids.is_empty() {
                    tx.rollback()?;
                    return Ok((0, None));
                }
                let mut restored = 0usize;
                {
                    let mut stmt = tx.prepare(
                        "UPDATE turns SET hidden = 0
                         WHERE session_id = ?1 AND hidden = 1 AND turn_id = ?2",
                    )?;
                    for id in &hidden_ids {
                        restored += stmt.execute(params![session_id, id])?;
                    }
                }
                if restored == 0 {
                    tx.rollback()?;
                    return Ok((0, None));
                }
                tx.execute("DELETE FROM turns WHERE turn_id = ?1", params![turn_id])?;
                tx.commit()?;
                Ok((1, None))
            }
            Some((turn_id, summary_seq, _, true, true, parent_summary_seq, None)) => {
                let restorable: i64 = match parent_summary_seq {
                    Some(previous_seq) => tx.query_row(
                        "SELECT COUNT(*) FROM turns
                         WHERE session_id = ?1 AND hidden = 1 AND seq < ?2
                           AND (seq = ?3 OR (is_summary = 0 AND seq > ?3))",
                        params![session_id, summary_seq, previous_seq],
                        |row| row.get(0),
                    )?,
                    None => tx.query_row(
                        "SELECT COUNT(*) FROM turns
                         WHERE session_id = ?1 AND hidden = 1 AND is_summary = 0 AND seq < ?2",
                        params![session_id, summary_seq],
                        |row| row.get(0),
                    )?,
                };
                if restorable == 0 {
                    tx.rollback()?;
                    return Ok((0, None));
                }

                tx.execute("DELETE FROM turns WHERE turn_id = ?1", params![turn_id])?;
                match parent_summary_seq {
                    Some(previous_seq) => {
                        tx.execute(
                            "UPDATE turns SET hidden = 0
                             WHERE session_id = ?1 AND hidden = 1 AND seq < ?2
                               AND (seq = ?3 OR (is_summary = 0 AND seq > ?3))",
                            params![session_id, summary_seq, previous_seq],
                        )?;
                    }
                    None => {
                        tx.execute(
                            "UPDATE turns SET hidden = 0
                             WHERE session_id = ?1 AND hidden = 1 AND is_summary = 0 AND seq < ?2",
                            params![session_id, summary_seq],
                        )?;
                    }
                }
                tx.commit()?;
                Ok((1, None))
            }
            None => Ok((0, None)),
        }
    }

    #[allow(dead_code)]
    /// Completed background-command wake turns after `after_seq`, oldest
    /// first: (seq, user display content, assistant reply).
    pub fn background_report_replies_after(
        &self,
        session_id: &str,
        after_seq: i64,
    ) -> Result<Vec<(i64, String, String, String)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT seq, turn_id, display_content,
                    CASE WHEN status = 'completed' THEN assistant_content
                         WHEN length(trim(assistant_content)) > 0 THEN assistant_content
                         ELSE '（自动跟进未能完成：模型请求失败或被中断，可用 job_status 查看任务输出）'
                    END
             FROM turns
             WHERE session_id = ?1 AND seq > ?2 AND status IN ('completed', 'failed', 'interrupted')
               AND user_content LIKE '<background-job-report>%'
             ORDER BY seq ASC LIMIT 8",
        )?;
        let rows = stmt
            .query_map(params![session_id, after_seq], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Largest turn seq in a session (0 when empty).
    pub fn latest_turn_seq(&self, session_id: &str) -> Result<i64> {
        let conn = self.conn.lock().unwrap();
        Ok(conn.query_row(
            "SELECT COALESCE(MAX(seq), 0) FROM turns WHERE session_id = ?1",
            params![session_id],
            |row| row.get(0),
        )?)
    }

    pub fn has_running_turns(&self, session_id: &str) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM turns WHERE session_id = ?1 AND status = 'running'",
            params![session_id],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    pub fn has_any_running_turns(&self) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM turns WHERE status = 'running'",
            [],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    pub fn running_turn_queue_target(
        &self,
        session_id: &str,
    ) -> Result<Option<(String, Option<String>, Option<u32>)>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT turns.turn_id,
                    COALESCE(
                        turns.queue_session_id,
                        (SELECT queued_prompts.queue_session_id
                           FROM queued_prompts
                          WHERE queued_prompts.owner_pid = turns.owner_pid
                            AND queued_prompts.queue_session_id IS NOT NULL
                          ORDER BY queued_prompts.seq DESC
                          LIMIT 1)
                    ),
                    turns.owner_pid
               FROM turns
              WHERE turns.session_id = ?1 AND turns.status = 'running'
              ORDER BY turns.seq DESC
              LIMIT 1",
            params![session_id],
            |row| {
                let owner_pid = row
                    .get::<_, Option<i64>>(2)?
                    .and_then(|pid| u32::try_from(pid).ok());
                Ok((row.get(0)?, row.get(1)?, owner_pid))
            },
        )
        .optional()
        .map_err(Into::into)
    }

    #[allow(dead_code)]
    pub fn running_turn_summaries(&self, session_id: &str) -> Result<Vec<String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT user_content FROM turns
             WHERE session_id = ?1 AND status = 'running' ORDER BY seq ASC",
        )?;
        let summaries = stmt
            .query_map(params![session_id], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(summaries)
    }

    pub fn running_turn_summaries_excluding(
        &self,
        session_id: &str,
        exclude_turn_id: &str,
    ) -> Result<Vec<String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT user_content FROM turns
             WHERE session_id = ?1 AND status = 'running' AND turn_id != ?2 ORDER BY seq ASC",
        )?;
        let summaries = stmt
            .query_map(params![session_id, exclude_turn_id], |row| {
                row.get::<_, String>(0)
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(summaries)
    }

    pub fn recover_stale_running_turns(&self) -> Result<Vec<StaleTurnRecovery>> {
        let mut conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT turn_id, session_id, owner_pid, revision, queue_session_id
             FROM turns WHERE status = 'running'",
        )?;
        let stale_turn_ids: Vec<(String, String, i64, Option<String>)> = stmt
            .query_map([], |row| {
                let turn_id: String = row.get(0)?;
                let session_id: String = row.get(1)?;
                let owner_pid: Option<i64> = row.get(2)?;
                let revision: i64 = row.get(3)?;
                let queue_session_id: Option<String> = row.get(4)?;
                Ok((turn_id, session_id, owner_pid, revision, queue_session_id))
            })?
            .filter_map(|row| {
                let (turn_id, session_id, owner_pid, revision, queue_session_id) = row.ok()?;
                let alive = owner_pid
                    .map(|pid| crate::alarm::process_exists(pid as u32))
                    .unwrap_or(false);
                if alive {
                    None
                } else {
                    Some((turn_id, session_id, revision, queue_session_id))
                }
            })
            .collect();
        drop(stmt);
        if stale_turn_ids.is_empty() {
            return Ok(Vec::new());
        }
        let tx = conn.transaction()?;
        let now = Utc::now().to_rfc3339();
        let mut recoveries = Vec::with_capacity(stale_turn_ids.len());
        for (turn_id, session_id, revision, queue_session_id) in &stale_turn_ids {
            if restore_redo_backup_locked(&tx, turn_id, *revision)? {
                recoveries.push(StaleTurnRecovery {
                    turn_id: turn_id.clone(),
                    session_id: session_id.clone(),
                    restored_redo: true,
                });
                continue;
            }
            consume_stale_queued_prompts_locked(
                &tx,
                turn_id,
                *revision,
                queue_session_id.as_deref(),
                &now,
            )?;
            let (content, reasoning) = interrupted_projection_locked(&tx, turn_id, *revision)?;
            let turn_affected = tx.execute(
                "UPDATE turns SET assistant_content = ?1, assistant_reasoning = ?2,
                        assistant_timestamp = ?3, status = 'interrupted'
                 WHERE turn_id = ?4 AND revision = ?5 AND status = 'running'",
                params![content, reasoning, now, turn_id, revision],
            )?;
            if turn_affected == 1 {
                bump_completion_seq_locked(&tx, turn_id)?;
                tx.execute(
                    "UPDATE turn_journal_segments
                     SET status = 'interrupted', finished_at = ?1
                     WHERE turn_id = ?2 AND revision = ?3 AND status = 'running'",
                    params![now, turn_id, revision],
                )?;
                recoveries.push(StaleTurnRecovery {
                    turn_id: turn_id.clone(),
                    session_id: session_id.clone(),
                    restored_redo: false,
                });
            }
        }
        tx.commit()?;
        Ok(recoveries)
    }

    pub fn next_seq_locked(&self, conn: &Connection, session_id: &str) -> Result<i64> {
        let next_seq: i64 = conn.query_row(
            "SELECT COALESCE(MAX(seq), 0) + 1 FROM turns WHERE session_id = ?1",
            params![session_id],
            |row| row.get(0),
        )?;
        Ok(next_seq)
    }

    #[allow(dead_code)]
    pub fn migrate_from_jsonl(&self, session_id: &str, jsonl_path: &Path) -> Result<usize> {
        if !jsonl_path.exists() {
            return Ok(0);
        }
        let turns = self.load_turns(session_id)?;
        if !turns.is_empty() {
            return Ok(0);
        }
        let file = std::fs::File::open(jsonl_path)?;
        use std::io::{BufRead, BufReader};
        let mut migrated = 0usize;
        let mut pending_user: Option<(String, String)> = None;
        for line in BufReader::new(file).lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let entry: serde_json::Value = match serde_json::from_str(&line) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let role = entry.get("role").and_then(|v| v.as_str()).unwrap_or("");
            let content = entry.get("content").and_then(|v| v.as_str()).unwrap_or("");
            let timestamp = entry
                .get("timestamp")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let reasoning = entry
                .get("reasoning")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            if role == "user" {
                if let Some((prev_ts, prev_content)) = pending_user.take() {
                    let turn_id = format!("migrated_{}", migrated);
                    let conn = self.conn.lock().unwrap();
                    let seq = self.next_seq_locked(&conn, session_id)?;
                    conn.execute(
                        "INSERT INTO turns (turn_id, session_id, seq, user_content, user_timestamp, assistant_content, status)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'completed')",
                        params![turn_id, session_id, seq, prev_content, prev_ts, "(migrated without reply)"],
                    )?;
                    drop(conn);
                    migrated += 1;
                }
                pending_user = Some((timestamp, content.to_string()));
            } else if role == "assistant" {
                if let Some((user_ts, user_content)) = pending_user.take() {
                    let turn_id = format!("migrated_{}", migrated);
                    let conn = self.conn.lock().unwrap();
                    let seq = self.next_seq_locked(&conn, session_id)?;
                    let now = Utc::now().to_rfc3339();
                    conn.execute(
                        "INSERT INTO turns (turn_id, session_id, seq, user_content, user_timestamp,
                         assistant_content, assistant_reasoning, assistant_timestamp, status)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'completed')",
                        params![
                            turn_id,
                            session_id,
                            seq,
                            user_content,
                            user_ts,
                            content,
                            reasoning,
                            now
                        ],
                    )?;
                    drop(conn);
                    migrated += 1;
                }
            }
        }
        if let Some((user_ts, user_content)) = pending_user {
            let turn_id = format!("migrated_{}", migrated);
            let conn = self.conn.lock().unwrap();
            let seq = self.next_seq_locked(&conn, session_id)?;
            conn.execute(
                "INSERT INTO turns (turn_id, session_id, seq, user_content, user_timestamp, assistant_content, status)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'interrupted')",
                params![
                    turn_id,
                    session_id,
                    seq,
                    user_content,
                    user_ts,
                    "上一轮响应已中断，未完成。不要继续执行上一轮任务，除非用户重新要求。"
                ],
            )?;
            drop(conn);
            migrated += 1;
        }
        Ok(migrated)
    }
}
