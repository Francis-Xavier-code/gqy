//! store — 自 src/state/conversation_db.rs 拆分。

pub(crate) use super::*;

impl ConversationDb {
    pub fn put_platform_meme_ref(&self, record: &PlatformMemeRefRecord) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT INTO platform_meme_refs (
                platform, account_id, conversation_kind, conversation_id,
                message_id, library, meme_id, direction, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT (
                platform, account_id, conversation_kind, conversation_id,
                message_id, library, meme_id
             ) DO UPDATE SET
                direction = excluded.direction,
                created_at = excluded.created_at",
            params![
                record.platform,
                record.account_id,
                record.conversation_kind,
                record.conversation_id,
                record.message_id,
                record.library,
                record.meme_id,
                record.direction,
                record.created_at,
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn platform_meme_refs_for_message(
        &self,
        platform: &str,
        account_id: &str,
        conversation_kind: &str,
        conversation_id: &str,
        message_id: &str,
    ) -> Result<Vec<PlatformMemeRefRecord>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT platform, account_id, conversation_kind, conversation_id,
                    message_id, library, meme_id, direction, created_at
             FROM platform_meme_refs
             WHERE platform = ?1 AND account_id = ?2
               AND conversation_kind = ?3 AND conversation_id = ?4
               AND message_id = ?5
             ORDER BY created_at ASC, library ASC, meme_id ASC",
        )?;
        let records = stmt
            .query_map(
                params![
                    platform,
                    account_id,
                    conversation_kind,
                    conversation_id,
                    message_id
                ],
                |row| {
                    Ok(PlatformMemeRefRecord {
                        platform: row.get(0)?,
                        account_id: row.get(1)?,
                        conversation_kind: row.get(2)?,
                        conversation_id: row.get(3)?,
                        message_id: row.get(4)?,
                        library: row.get(5)?,
                        meme_id: row.get(6)?,
                        direction: row.get(7)?,
                        created_at: row.get(8)?,
                    })
                },
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(records)
    }

    pub fn delete_platform_meme_ref(&self, library: &str, meme_id: &str) -> Result<usize> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let deleted = tx.execute(
            "DELETE FROM platform_meme_refs WHERE library = ?1 AND meme_id = ?2",
            params![library, meme_id],
        )?;
        tx.commit()?;
        Ok(deleted)
    }

    /// Records the model identity and token usage a subagent session actually
    /// used (audit columns on `sessions`).
    /// Writes a subagent row the way builds before v19 did: usage present,
    /// `cache_read_tokens` left NULL.
    #[cfg(test)]
    pub fn record_legacy_subagent_usage_for_test(
        &self,
        session_id: &str,
        prompt_tokens: i64,
        total_tokens: i64,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE sessions SET prompt_tokens = ?2, total_tokens = ?3,
                    cache_read_tokens = NULL
             WHERE session_id = ?1",
            params![session_id, prompt_tokens, total_tokens],
        )?;
        Ok(())
    }

    pub fn record_subagent_usage(
        &self,
        session_id: &str,
        provider_id: Option<&str>,
        model: Option<&str>,
        context_window: Option<i64>,
        prompt_tokens: i64,
        completion_tokens: i64,
        total_tokens: i64,
        cache_read_tokens: i64,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE sessions SET provider_id = ?2, model = ?3, context_window = ?4,
                    prompt_tokens = ?5, completion_tokens = ?6, total_tokens = ?7,
                    updated_at = ?8, cache_read_tokens = ?9
             WHERE session_id = ?1",
            params![
                session_id,
                provider_id,
                model,
                context_window,
                prompt_tokens,
                completion_tokens,
                total_tokens,
                Utc::now().to_rfc3339(),
                cache_read_tokens,
            ],
        )?;
        Ok(())
    }

    /// Deletes subagent audit sessions older than the retention window;
    /// their turns/images/queues cascade away.
    pub fn delete_subagent_sessions_older_than(&self, days: i64) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        let deleted = conn.execute(
            "DELETE FROM sessions
             WHERE kind = 'subagent'
               AND datetime(updated_at) < datetime('now', '-' || ?1 || ' days')",
            params![days],
        )?;
        Ok(deleted)
    }

    /// Deletes abandoned one-shot sessions older than the retention window. A
    /// `gqy ask` turn deletes its own session; anything still here was
    /// orphaned by a client that died mid-turn (Ctrl+C, SIGKILL).
    pub fn delete_ask_sessions_older_than(&self, hours: i64) -> Result<usize> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        // queued_prompts.session_id arrived via ALTER and has no cascading FK,
        // so its rows have to go first (same reason as `delete_session`).
        tx.execute(
            "DELETE FROM queued_prompts WHERE session_id IN (
                 SELECT session_id FROM sessions
                 WHERE kind = ?1
                   AND datetime(updated_at) < datetime('now', '-' || ?2 || ' hours'))",
            params![crate::state::ASK_SESSION_KIND, hours],
        )?;
        let deleted = tx.execute(
            "DELETE FROM sessions
             WHERE kind = ?1
               AND datetime(updated_at) < datetime('now', '-' || ?2 || ' hours')",
            params![crate::state::ASK_SESSION_KIND, hours],
        )?;
        tx.commit()?;
        Ok(deleted)
    }

    pub fn update_session_field(
        &self,
        session_id: &str,
        field: &'static str,
        value: Option<&str>,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let updated = conn.execute(
            &format!("UPDATE sessions SET {field} = ?2, updated_at = ?3 WHERE session_id = ?1"),
            params![session_id, value, Utc::now().to_rfc3339()],
        )?;
        if updated == 0 {
            bail!("session not found: {session_id}");
        }
        Ok(())
    }

    pub fn start_turn(
        &self,
        session_id: &str,
        turn_id: &str,
        user_content: &str,
        display_content: &str,
        owner_pid: u32,
        queue_session_id: &str,
        workspace: Option<&str>,
        attachment_run_id: Option<&str>,
    ) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let seq = self.next_seq_locked(&tx, session_id)?;
        let now = Utc::now().to_rfc3339();
        tx.execute(
            "INSERT INTO turns (turn_id, session_id, seq, user_content, display_content, user_timestamp, assistant_content, status, owner_pid, queue_session_id, workspace)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'running', ?8, ?9, ?10)",
            params![
                turn_id,
                session_id,
                seq,
                user_content,
                display_content,
                now,
                PENDING_PLACEHOLDER,
                owner_pid as i64,
                queue_session_id,
                workspace
            ],
        )?;
        tx.execute(
            "INSERT INTO turn_journal_segments
                (turn_id, revision, segment_index, status, started_at)
             VALUES (?1, 0, 0, 'running', ?2)",
            params![turn_id, now],
        )?;
        if let Some(run_id) = attachment_run_id {
            tx.execute(
                "UPDATE user_attachments SET run_id = NULL, turn_id = ?1
                 WHERE session_id = ?2 AND run_id = ?3",
                params![turn_id, session_id, run_id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn insert_user_attachment(
        &self,
        session_id: &str,
        attachment: &UserAttachment,
        data: &[u8],
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO user_attachments
                (attachment_id, session_id, file_name, mime, kind, size_bytes,
                 width, height, data, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                attachment.attachment_id,
                session_id,
                attachment.file_name,
                attachment.mime,
                attachment.kind,
                attachment.size_bytes as i64,
                i64::from(attachment.width),
                i64::from(attachment.height),
                data,
                attachment.created_at,
            ],
        )?;
        Ok(())
    }

    pub fn load_user_attachment(
        &self,
        session_id: &str,
        attachment_id: &str,
    ) -> Result<Option<UserAttachmentData>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT attachment_id, file_name, mime, kind, size_bytes, width,
                    height, created_at, data
             FROM user_attachments WHERE session_id = ?1 AND attachment_id = ?2",
            params![session_id, attachment_id],
            |row| {
                Ok(UserAttachmentData {
                    attachment: map_user_attachment_row(row)?,
                    bytes: row.get(8)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn load_user_attachment_by_id(
        &self,
        attachment_id: &str,
    ) -> Result<Option<UserAttachmentData>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT attachment_id, file_name, mime, kind, size_bytes, width,
                    height, created_at, data
             FROM user_attachments WHERE attachment_id = ?1",
            params![attachment_id],
            |row| {
                Ok(UserAttachmentData {
                    attachment: map_user_attachment_row(row)?,
                    bytes: row.get(8)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn load_user_attachment_data_for_turn(
        &self,
        session_id: &str,
        turn_id: &str,
    ) -> Result<Vec<UserAttachmentData>> {
        self.load_bound_user_attachment_data(session_id, "turn_id", turn_id)
    }

    pub fn load_user_attachment_data_for_prompt(
        &self,
        session_id: &str,
        prompt_id: &str,
    ) -> Result<Vec<UserAttachmentData>> {
        self.load_bound_user_attachment_data(session_id, "prompt_id", prompt_id)
    }

    pub fn load_bound_user_attachment_data(
        &self,
        session_id: &str,
        field: &'static str,
        value: &str,
    ) -> Result<Vec<UserAttachmentData>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(&format!(
            "SELECT attachment_id, file_name, mime, kind, size_bytes, width,
                    height, created_at, data
             FROM user_attachments
             WHERE session_id = ?1 AND {field} = ?2
             ORDER BY created_at, attachment_id"
        ))?;
        let attachments = stmt
            .query_map(params![session_id, value], |row| {
                Ok(UserAttachmentData {
                    attachment: map_user_attachment_row(row)?,
                    bytes: row.get(8)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(attachments)
    }

    pub fn load_user_attachments(
        &self,
        session_id: &str,
        attachment_ids: &[String],
    ) -> Result<Vec<UserAttachmentData>> {
        let conn = self.conn.lock().unwrap();
        let mut attachments = Vec::with_capacity(attachment_ids.len());
        for attachment_id in attachment_ids {
            let attachment = conn
                .query_row(
                    "SELECT attachment_id, file_name, mime, kind, size_bytes, width,
                            height, created_at, data
                     FROM user_attachments
                     WHERE session_id = ?1 AND attachment_id = ?2
                       AND turn_id IS NULL AND prompt_id IS NULL AND run_id IS NULL",
                    params![session_id, attachment_id],
                    |row| {
                        Ok(UserAttachmentData {
                            attachment: map_user_attachment_row(row)?,
                            bytes: row.get(8)?,
                        })
                    },
                )
                .optional()?;
            let Some(attachment) = attachment else {
                bail!("attachment is unavailable: {attachment_id}");
            };
            attachments.push(attachment);
        }
        Ok(attachments)
    }

    pub fn reserve_user_attachments(
        &self,
        session_id: &str,
        attachment_ids: &[String],
        run_id: &str,
    ) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        for attachment_id in attachment_ids {
            let affected = tx.execute(
                "UPDATE user_attachments SET run_id = ?1
                 WHERE session_id = ?2 AND attachment_id = ?3
                   AND turn_id IS NULL AND prompt_id IS NULL AND run_id IS NULL",
                params![run_id, session_id, attachment_id],
            )?;
            if affected != 1 {
                bail!("attachment changed before it could be submitted: {attachment_id}");
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn release_user_attachments_for_run(&self, run_id: &str) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        Ok(conn.execute(
            "UPDATE user_attachments SET run_id = NULL WHERE run_id = ?1",
            params![run_id],
        )?)
    }

    pub fn delete_staged_user_attachment(
        &self,
        session_id: &str,
        attachment_id: &str,
    ) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        Ok(conn.execute(
            "DELETE FROM user_attachments
             WHERE session_id = ?1 AND attachment_id = ?2
               AND turn_id IS NULL AND prompt_id IS NULL AND run_id IS NULL",
            params![session_id, attachment_id],
        )? == 1)
    }

    pub fn purge_stale_user_attachments(&self) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        Ok(conn.execute(
            "DELETE FROM user_attachments
             WHERE turn_id IS NULL AND prompt_id IS NULL AND run_id IS NULL
               AND datetime(created_at) < datetime('now', '-1 day')",
            [],
        )?)
    }

    pub fn append_turn_journal_event(
        &self,
        turn_id: &str,
        revision: i64,
        segment_index: i64,
        kind: &str,
        call_id: Option<&str>,
        name: Option<&str>,
        text_payload: Option<&str>,
        blob_payload: Option<&[u8]>,
        ok: Option<bool>,
    ) -> Result<()> {
        if text_payload.is_some_and(|payload| payload.len() > MAX_JOURNAL_TEXT_EVENT_BYTES) {
            bail!("turn journal text event exceeds the 64 MiB limit");
        }
        if blob_payload.is_some_and(|payload| payload.len() > MAX_JOURNAL_BLOB_EVENT_BYTES) {
            bail!("turn journal binary event exceeds the 8 MiB limit");
        }
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let valid: bool = tx.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM turns t
                 INNER JOIN turn_journal_segments s
                   ON s.turn_id = t.turn_id AND s.revision = t.revision
                  AND s.segment_index = ?3
                 WHERE t.turn_id = ?1 AND t.revision = ?2
                   AND t.status = 'running' AND s.status != 'superseded'
             )",
            params![turn_id, revision, segment_index],
            |row| row.get(0),
        )?;
        if !valid {
            bail!("turn journal generation is no longer active");
        }
        tx.execute(
            "INSERT INTO turn_journal_events
                (turn_id, revision, segment_index, kind, call_id, name,
                 text_payload, blob_payload, ok, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                turn_id,
                revision,
                segment_index,
                kind,
                call_id,
                name,
                text_payload,
                blob_payload,
                ok.map(i64::from),
                Utc::now().to_rfc3339(),
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn supersede_turn_journal_segment(
        &self,
        turn_id: &str,
        revision: i64,
        segment_index: i64,
    ) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let affected = tx.execute(
            "UPDATE turn_journal_segments
             SET status = 'superseded', finished_at = ?1
             WHERE turn_id = ?2 AND revision = ?3 AND segment_index = ?4
               AND status = 'running'",
            params![Utc::now().to_rfc3339(), turn_id, revision, segment_index],
        )?;
        if affected != 1 {
            bail!("turn journal segment changed before supersession");
        }
        tx.execute(
            "INSERT INTO turn_journal_events
                (turn_id, revision, segment_index, kind, created_at)
             VALUES (?1, ?2, ?3, 'generation_superseded', ?4)",
            params![turn_id, revision, segment_index, Utc::now().to_rfc3339()],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn append_tool_reports(&self, turn_id: &str, reports: &[String]) -> Result<()> {
        if reports.is_empty() {
            return Ok(());
        }
        let conn = self.conn.lock().unwrap();
        let existing: String = conn.query_row(
            "SELECT tool_reports FROM turns WHERE turn_id = ?1",
            params![turn_id],
            |row| row.get(0),
        )?;
        let mut all: Vec<String> = serde_json::from_str(&existing).unwrap_or_default();
        all.extend(reports.iter().cloned());
        conn.execute(
            "UPDATE turns SET tool_reports = ?1 WHERE turn_id = ?2",
            params![serde_json::to_string(&all)?, turn_id],
        )?;
        Ok(())
    }

    /// Stores the fossilized transient tail for a turn (v7 append-only).
    pub fn set_turn_context_messages(&self, turn_id: &str, messages: &[ChatMessage]) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE turns SET context_messages = ?1 WHERE turn_id = ?2",
            params![serde_json::to_string(messages)?, turn_id],
        )?;
        Ok(())
    }

    /// 完成后落一次结构化工具流。独立 UPDATE 而非扩 complete 签名:调用点多,
    /// 且流为空(无工具回合)时根本不写。
    pub fn set_turn_tool_flow(&self, turn_id: &str, flow: &[ToolFlowRound]) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE turns SET tool_flow = ?1 WHERE turn_id = ?2",
            params![serde_json::to_string(flow)?, turn_id],
        )?;
        Ok(())
    }

    #[allow(dead_code)]
    pub fn complete_turn(
        &self,
        turn_id: &str,
        content: &str,
        reasoning: Option<&str>,
    ) -> Result<()> {
        self.complete_turn_with_usage(
            turn_id,
            content,
            reasoning,
            None,
            None,
            TurnTokens::default(),
            false,
        )
    }

    pub fn complete_turn_with_usage(
        &self,
        turn_id: &str,
        content: &str,
        reasoning: Option<&str>,
        provider_id: Option<&str>,
        model: Option<&str>,
        tokens: TurnTokens,
        token_usage_estimated: bool,
    ) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = Utc::now().to_rfc3339();
        let token_usage_estimated = i64::from(token_usage_estimated);
        let affected = tx.execute(
            "UPDATE turns SET assistant_content = ?1, assistant_reasoning = ?2,
                    assistant_provider_id = ?3, assistant_model = ?4, assistant_timestamp = ?5,
                    status = 'completed', token_total = ?6, token_usage_estimated = ?7,
                    token_prompt = ?9, token_cache_read = ?10
              WHERE turn_id = ?8 AND status = 'running'",
            params![
                content,
                reasoning,
                provider_id,
                model,
                now,
                tokens.total as i64,
                token_usage_estimated,
                turn_id,
                tokens.prompt as i64,
                tokens.cache_read as i64
            ],
        )?;
        if affected != 1 {
            bail!("turn changed before it could be completed");
        }
        bump_completion_seq_locked(&tx, turn_id)?;
        // Snapshot the display transcript before the journal goes: the tables
        // below are load-bearing for in-flight turn recovery, so they keep
        // being wiped on completion exactly as before.
        store_replay_journal(&tx, turn_id)?;
        tx.execute(
            "DELETE FROM turn_journal_segments WHERE turn_id = ?1",
            params![turn_id],
        )?;
        touch_session_last_request(&tx, turn_id)?;
        tx.commit()?;
        Ok(())
    }

    pub fn complete_turn_revision_with_usage(
        &self,
        turn_id: &str,
        revision: i64,
        content: &str,
        reasoning: Option<&str>,
        provider_id: Option<&str>,
        model: Option<&str>,
        tokens: TurnTokens,
        token_usage_estimated: bool,
    ) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = Utc::now().to_rfc3339();
        let affected = tx.execute(
            "UPDATE turns SET assistant_content = ?1, assistant_reasoning = ?2,
                    assistant_provider_id = ?3, assistant_model = ?4, assistant_timestamp = ?5,
                    status = 'completed', token_total = ?6, token_usage_estimated = ?7,
                    token_prompt = ?10, token_cache_read = ?11
             WHERE turn_id = ?8 AND revision = ?9 AND status = 'running'",
            params![
                content,
                reasoning,
                provider_id,
                model,
                now,
                tokens.total as i64,
                i64::from(token_usage_estimated),
                turn_id,
                revision,
                tokens.prompt as i64,
                tokens.cache_read as i64
            ],
        )?;
        if affected != 1 {
            bail!("redo generation changed before it could be completed");
        }
        tx.execute(
            "DELETE FROM turn_redo_backups WHERE turn_id = ?1 AND revision = ?2",
            params![turn_id, revision],
        )?;
        tx.execute(
            "DELETE FROM turn_journal_segments WHERE turn_id = ?1",
            params![turn_id],
        )?;
        touch_session_last_request(&tx, turn_id)?;
        tx.commit()?;
        Ok(())
    }

    pub fn interrupt_turn(&self, turn_id: &str) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let revision: Option<i64> = tx
            .query_row(
                "SELECT revision FROM turns WHERE turn_id = ?1 AND status = 'running'",
                params![turn_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(revision) = revision else {
            tx.commit()?;
            return Ok(());
        };
        let now = Utc::now().to_rfc3339();
        let (content, reasoning) = interrupted_projection_locked(&tx, turn_id, revision)?;
        tx.execute(
            "UPDATE turns SET assistant_content = ?1, assistant_reasoning = ?2,
                    assistant_timestamp = ?3, status = 'interrupted'
             WHERE turn_id = ?4 AND revision = ?5 AND status = 'running'",
            params![content, reasoning, now, turn_id, revision],
        )?;
        bump_completion_seq_locked(&tx, turn_id)?;
        tx.execute(
            "UPDATE turn_journal_segments
             SET status = 'interrupted', finished_at = ?1
             WHERE turn_id = ?2 AND revision = ?3 AND status = 'running'",
            params![now, turn_id, revision],
        )?;
        touch_session_last_request(&tx, turn_id)?;
        tx.commit()?;
        Ok(())
    }

    pub fn interrupt_turn_revision(&self, turn_id: &str, revision: i64) -> Result<bool> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let restored = restore_redo_backup_locked(&tx, turn_id, revision)?;
        if !restored {
            let (content, reasoning) = interrupted_projection_locked(&tx, turn_id, revision)?;
            let now = Utc::now().to_rfc3339();
            tx.execute(
                "UPDATE turns SET assistant_content = ?1, assistant_reasoning = ?2,
                        assistant_timestamp = ?3, status = 'interrupted'
                 WHERE turn_id = ?4 AND revision = ?5 AND status = 'running'",
                params![content, reasoning, now, turn_id, revision],
            )?;
            tx.execute(
                "UPDATE turn_journal_segments
                 SET status = 'interrupted', finished_at = ?1
                 WHERE turn_id = ?2 AND revision = ?3 AND status = 'running'",
                params![now, turn_id, revision],
            )?;
        }
        tx.commit()?;
        Ok(restored)
    }

    /// Unions `delta` into the turn's stored footprint. Read-modify-write is
    /// safe here: the turn is running and owned by exactly one process.
    pub fn merge_turn_footprint(&self, turn_id: &str, delta: &ToolFootprint) -> Result<()> {
        if delta.is_empty() {
            return Ok(());
        }
        let conn = self.conn.lock().unwrap();
        let existing: Option<Option<String>> = conn
            .query_row(
                "SELECT tool_footprint FROM turns WHERE turn_id = ?1",
                params![turn_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(existing) = existing else {
            return Ok(());
        };
        let mut footprint = existing
            .as_deref()
            .and_then(|json| serde_json::from_str::<ToolFootprint>(json).ok())
            .unwrap_or_default();
        footprint.merge(delta.clone());
        conn.execute(
            "UPDATE turns SET tool_footprint = ?1 WHERE turn_id = ?2",
            params![serde_json::to_string(&footprint)?, turn_id],
        )?;
        Ok(())
    }

    /// Merged footprint across the given turns (summary rows included — they
    /// carry the accumulated footprint of everything they folded).
    pub fn load_merged_footprint(
        &self,
        session_id: &str,
        turn_ids: &[String],
    ) -> Result<ToolFootprint> {
        let conn = self.conn.lock().unwrap();
        let mut merged = ToolFootprint::default();
        let mut stmt = conn
            .prepare("SELECT tool_footprint FROM turns WHERE session_id = ?1 AND turn_id = ?2")?;
        for turn_id in turn_ids {
            let value: Option<Option<String>> = stmt
                .query_row(params![session_id, turn_id], |row| row.get(0))
                .optional()?;
            if let Some(Some(json)) = value {
                if let Ok(footprint) = serde_json::from_str::<ToolFootprint>(&json) {
                    merged.merge(footprint);
                }
            }
        }
        Ok(merged)
    }

    /// Unix seconds of this session's most recent completed/interrupted
    /// request write-point. None on legacy sessions (cold-resume prune skips).
    pub fn session_last_request_at(&self, session_id: &str) -> Result<Option<i64>> {
        let conn = self.conn.lock().unwrap();
        let value: Option<Option<i64>> = conn
            .query_row(
                "SELECT last_request_at FROM sessions WHERE session_id = ?1",
                params![session_id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(value.flatten())
    }

    pub fn append_tool_report(&self, turn_id: &str, report: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let existing: Option<String> = conn
            .query_row(
                "SELECT tool_reports FROM turns WHERE turn_id = ?1",
                params![turn_id],
                |row| row.get(0),
            )
            .optional()?;
        let mut reports: Vec<String> = existing
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default();
        reports.push(report.to_string());
        let encoded = serde_json::to_string(&reports)?;
        conn.execute(
            "UPDATE turns SET tool_reports = ?1 WHERE turn_id = ?2",
            params![encoded, turn_id],
        )?;
        Ok(())
    }

    pub fn insert_image_asset(&self, asset: &ImageAsset, data: &[u8]) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO image_assets
                (asset_id, turn_id, tool_id, mime, width, height, alt, data, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                asset.asset_id,
                asset.turn_id,
                asset.tool_id,
                asset.mime,
                i64::from(asset.width),
                i64::from(asset.height),
                asset.alt,
                data,
                asset.created_at,
            ],
        )?;
        Ok(())
    }

    pub fn load_image_assets(&self, session_id: &str) -> Result<Vec<ImageAsset>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT a.asset_id, a.turn_id, a.tool_id, a.mime, a.width, a.height, a.alt, a.created_at
             FROM image_assets a
             INNER JOIN turns t ON t.turn_id = a.turn_id
             WHERE t.session_id = ?1
             ORDER BY a.turn_id ASC, a.created_at ASC, a.asset_id ASC",
        )?;
        let assets = stmt
            .query_map(params![session_id], map_image_asset_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(assets)
    }

    pub fn load_image_asset(&self, asset_id: &str) -> Result<Option<ImageAssetData>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT asset_id, turn_id, tool_id, mime, width, height, alt, created_at, data
             FROM image_assets WHERE asset_id = ?1",
            params![asset_id],
            |row| {
                Ok(ImageAssetData {
                    asset: map_image_asset_row(row)?,
                    bytes: row.get(8)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn upsert_artifact_asset(&self, asset: &ArtifactAsset, data: &[u8]) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO artifact_assets
                (asset_id, turn_id, tool_id, source_key, file_name, mime, kind,
                 size_bytes, data, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
              ON CONFLICT(turn_id, source_key) DO UPDATE SET
                tool_id = excluded.tool_id,
                file_name = excluded.file_name,
                mime = excluded.mime,
                kind = excluded.kind,
                size_bytes = excluded.size_bytes,
                data = excluded.data,
                created_at = excluded.created_at,
                updated_at = excluded.updated_at
              ON CONFLICT(asset_id) DO UPDATE SET
                turn_id = excluded.turn_id,
                tool_id = excluded.tool_id,
                source_key = excluded.source_key,
                file_name = excluded.file_name,
                mime = excluded.mime,
                kind = excluded.kind,
                size_bytes = excluded.size_bytes,
                data = excluded.data,
                created_at = excluded.created_at,
                updated_at = excluded.updated_at",
            params![
                asset.asset_id,
                asset.turn_id,
                asset.tool_id,
                asset.source_key,
                asset.file_name,
                asset.mime,
                asset.kind,
                asset.size_bytes as i64,
                data,
                asset.created_at,
                asset.updated_at,
            ],
        )?;
        Ok(())
    }

    pub fn load_artifact_assets(&self, session_id: &str) -> Result<Vec<ArtifactAsset>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT a.asset_id, a.turn_id, a.tool_id, a.source_key, a.file_name,
                    a.mime, a.kind, a.size_bytes, a.created_at, a.updated_at
             FROM artifact_assets a
             INNER JOIN turns t ON t.turn_id = a.turn_id
             WHERE t.session_id = ?1
             ORDER BY a.turn_id, a.updated_at, a.asset_id",
        )?;
        let assets = stmt
            .query_map(params![session_id], map_artifact_asset_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(assets)
    }

    pub fn load_artifact_asset(&self, asset_id: &str) -> Result<Option<ArtifactAssetData>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT asset_id, turn_id, tool_id, source_key, file_name, mime, kind,
                    size_bytes, created_at, updated_at, data
             FROM artifact_assets WHERE asset_id = ?1",
            params![asset_id],
            |row| {
                Ok(ArtifactAssetData {
                    asset: map_artifact_asset_row(row)?,
                    bytes: row.get(10)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn load_artifact_asset_data_for_turn(
        &self,
        turn_id: &str,
    ) -> Result<Vec<ArtifactAssetData>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT asset_id, turn_id, tool_id, source_key, file_name, mime, kind,
                    size_bytes, created_at, updated_at, data
             FROM artifact_assets WHERE turn_id = ?1 ORDER BY updated_at, asset_id",
        )?;
        let assets = stmt
            .query_map(params![turn_id], |row| {
                Ok(ArtifactAssetData {
                    asset: map_artifact_asset_row(row)?,
                    bytes: row.get(10)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(assets)
    }

    pub fn turn_session_id(&self, turn_id: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT session_id FROM turns WHERE turn_id = ?1",
            params![turn_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn append_question_exchange(
        &self,
        turn_id: &str,
        exchange: &QuestionExchange,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let next_index: i64 = conn.query_row(
            "SELECT COALESCE(MAX(exchange_index), -1) + 1
             FROM question_exchanges WHERE turn_id = ?1",
            params![turn_id],
            |row| row.get(0),
        )?;
        conn.execute(
            "INSERT INTO question_exchanges (turn_id, exchange_index, payload)
             VALUES (?1, ?2, ?3)",
            params![turn_id, next_index, serde_json::to_string(exchange)?],
        )?;
        Ok(())
    }

    pub fn enqueue_prompt(
        &self,
        session_id: &str,
        target_turn_id: Option<&str>,
        prompt_id: &str,
        content: &str,
        display_content: &str,
        attachments: &[QueuedPromptAttachment],
        uploaded_attachment_ids: &[String],
        queue_session_id: &str,
        owner_pid: u32,
    ) -> Result<QueuedPrompt> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let target_running: bool = match target_turn_id {
            Some(turn_id) => tx.query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM turns
                     WHERE session_id = ?1 AND turn_id = ?2 AND status = 'running'
                       AND queue_session_id = ?3 AND owner_pid = ?4
                 )",
                params![session_id, turn_id, queue_session_id, owner_pid as i64],
                |row| row.get(0),
            )?,
            None => true,
        };
        if !target_running {
            bail!("the target turn is no longer accepting follow-up messages");
        }
        let submitted_at = Utc::now().to_rfc3339();
        let attachments_json = serde_json::to_string(attachments)?;
        tx.execute(
            "INSERT INTO queued_prompts
                (session_id, prompt_id, content, display_content, attachments, status, submitted_at,
                 queue_session_id, owner_pid)
             VALUES (?1, ?2, ?3, ?4, ?5, 'queued', ?6, ?7, ?8)",
            params![
                session_id,
                prompt_id,
                content,
                display_content,
                attachments_json,
                submitted_at,
                queue_session_id,
                owner_pid as i64
            ],
        )?;
        let seq = tx.last_insert_rowid();
        for attachment_id in uploaded_attachment_ids {
            let affected = tx.execute(
                "UPDATE user_attachments SET prompt_id = ?1
                 WHERE session_id = ?2 AND attachment_id = ?3
                   AND turn_id IS NULL AND prompt_id IS NULL AND run_id IS NULL",
                params![prompt_id, session_id, attachment_id],
            )?;
            if affected != 1 {
                bail!("attachment changed before it could be queued: {attachment_id}");
            }
        }
        tx.commit()?;
        drop(conn);
        let uploaded_attachments = self.user_attachments_for_prompt(prompt_id)?;
        Ok(QueuedPrompt {
            prompt_id: prompt_id.to_string(),
            seq,
            content: content.to_string(),
            display_content: display_content.to_string(),
            attachments: attachments.to_vec(),
            uploaded_attachments,
            submitted_at,
        })
    }

    pub fn load_queued_prompts(
        &self,
        session_id: &str,
        queue_session_id: &str,
    ) -> Result<Vec<QueuedPrompt>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT prompt_id, seq, content, display_content, attachments, submitted_at
             FROM queued_prompts
             WHERE status = 'queued' AND session_id = ?1 AND queue_session_id = ?2
             ORDER BY seq ASC",
        )?;
        let mut prompts = stmt
            .query_map(params![session_id, queue_session_id], |row| {
                let attachments_json: String = row.get(4)?;
                let attachments = serde_json::from_str(&attachments_json).unwrap_or_default();
                Ok(QueuedPrompt {
                    prompt_id: row.get(0)?,
                    seq: row.get(1)?,
                    content: row.get(2)?,
                    display_content: row.get(3)?,
                    attachments,
                    uploaded_attachments: Vec::new(),
                    submitted_at: row.get(5)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        attach_prompt_attachments_locked(&conn, &mut prompts)?;
        Ok(prompts)
    }
}
