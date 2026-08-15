//! state_impl2 — 自 src/state/state_impl.rs 拆分。

pub(crate) use super::*;

impl StateStore {
    pub fn load_user_attachment(&self, attachment_id: &str) -> Result<Option<UserAttachmentData>> {
        self.conv_db
            .load_user_attachment(&self.session(), attachment_id)
    }

    pub fn load_user_attachment_by_id(
        &self,
        attachment_id: &str,
    ) -> Result<Option<UserAttachmentData>> {
        self.conv_db.load_user_attachment_by_id(attachment_id)
    }

    pub fn load_user_attachment_data_for_turn(
        &self,
        turn_id: &str,
    ) -> Result<Vec<UserAttachmentData>> {
        self.conv_db
            .load_user_attachment_data_for_turn(&self.session(), turn_id)
    }

    pub fn load_user_attachment_data_for_prompt(
        &self,
        prompt_id: &str,
    ) -> Result<Vec<UserAttachmentData>> {
        self.conv_db
            .load_user_attachment_data_for_prompt(&self.session(), prompt_id)
    }

    pub fn load_staged_user_attachments(
        &self,
        attachment_ids: &[String],
    ) -> Result<Vec<UserAttachmentData>> {
        self.conv_db
            .load_user_attachments(&self.session(), attachment_ids)
    }

    pub fn reserve_user_attachments(&self, attachment_ids: &[String], run_id: &str) -> Result<()> {
        self.conv_db
            .reserve_user_attachments(&self.session(), attachment_ids, run_id)
    }

    pub fn release_user_attachments_for_run(&self, run_id: &str) -> Result<usize> {
        self.conv_db.release_user_attachments_for_run(run_id)
    }

    pub fn delete_staged_user_attachment(&self, attachment_id: &str) -> Result<bool> {
        self.conv_db
            .delete_staged_user_attachment(&self.session(), attachment_id)
    }

    pub fn purge_stale_user_attachments(&self) -> Result<usize> {
        self.conv_db.purge_stale_user_attachments()
    }

    pub fn append_question_exchange(
        &self,
        turn_id: &str,
        exchange: &crate::question::QuestionExchange,
    ) -> Result<()> {
        self.conv_db.append_question_exchange(turn_id, exchange)
    }

    pub fn enqueue_prompt(
        &self,
        prompt_id: &str,
        content: &str,
        display_content: &str,
        attachments: &[QueuedPromptAttachment],
    ) -> Result<QueuedPrompt> {
        self.enqueue_prompt_with_uploads(prompt_id, content, display_content, attachments, &[])
    }

    pub fn enqueue_prompt_with_uploads(
        &self,
        prompt_id: &str,
        content: &str,
        display_content: &str,
        attachments: &[QueuedPromptAttachment],
        uploaded_attachment_ids: &[String],
    ) -> Result<QueuedPrompt> {
        self.conv_db.enqueue_prompt(
            &self.session(),
            None,
            prompt_id,
            content,
            display_content,
            attachments,
            uploaded_attachment_ids,
            &self.queue_session_id,
            self.queue_owner_pid,
        )
    }

    pub fn running_turn_queue_target(&self) -> Result<Option<RunningTurnQueueTarget>> {
        Ok(self
            .conv_db
            .running_turn_queue_target(&self.session())?
            .map(
                |(turn_id, queue_session_id, owner_pid)| RunningTurnQueueTarget {
                    turn_id,
                    queue_session_id,
                    owner_pid,
                },
            ))
    }

    pub fn enqueue_prompt_for_target(
        &self,
        target: &RunningTurnQueueTarget,
        prompt_id: &str,
        content: &str,
        display_content: &str,
        attachments: &[QueuedPromptAttachment],
    ) -> Result<QueuedPrompt> {
        self.enqueue_prompt_for_target_with_uploads(
            target,
            prompt_id,
            content,
            display_content,
            attachments,
            &[],
        )
    }

    pub fn enqueue_prompt_for_target_with_uploads(
        &self,
        target: &RunningTurnQueueTarget,
        prompt_id: &str,
        content: &str,
        display_content: &str,
        attachments: &[QueuedPromptAttachment],
        uploaded_attachment_ids: &[String],
    ) -> Result<QueuedPrompt> {
        let queue_session_id = target
            .queue_session_id
            .as_deref()
            .context("running turn does not expose a queue session")?;
        let owner_pid = target
            .owner_pid
            .context("running turn does not expose an owner process")?;
        self.conv_db.enqueue_prompt(
            &self.session(),
            Some(&target.turn_id),
            prompt_id,
            content,
            display_content,
            attachments,
            uploaded_attachment_ids,
            queue_session_id,
            owner_pid,
        )
    }

    pub fn load_queued_prompts_for_target(
        &self,
        target: &RunningTurnQueueTarget,
    ) -> Result<Vec<QueuedPrompt>> {
        let Some(queue_session_id) = target.queue_session_id.as_deref() else {
            return Ok(Vec::new());
        };
        self.conv_db
            .load_queued_prompts(&self.session(), queue_session_id)
    }

    pub fn remove_queued_prompt_for_target(
        &self,
        target: &RunningTurnQueueTarget,
        prompt_id: &str,
    ) -> Result<bool> {
        let Some(queue_session_id) = target.queue_session_id.as_deref() else {
            return Ok(false);
        };
        self.conv_db
            .remove_queued_prompt(&self.session(), prompt_id, queue_session_id)
    }

    pub fn load_queued_prompts(&self) -> Result<Vec<QueuedPrompt>> {
        self.conv_db
            .load_queued_prompts(&self.session(), &self.queue_session_id)
    }

    #[cfg(test)]
    pub fn consume_queued_prompts(
        &self,
        turn_id: &str,
        prompts: &[(String, String)],
        preceding_assistant_content: Option<&str>,
        preceding_assistant_reasoning: Option<&str>,
    ) -> Result<()> {
        self.conv_db.consume_queued_prompts(
            &self.session(),
            turn_id,
            prompts,
            preceding_assistant_content,
            preceding_assistant_reasoning,
            None,
            None,
            &self.queue_session_id,
        )
    }

    pub fn consume_queued_prompts_with_model(
        &self,
        turn_id: &str,
        prompts: &[(String, String)],
        preceding_assistant_content: Option<&str>,
        preceding_assistant_reasoning: Option<&str>,
        preceding_assistant_provider_id: Option<&str>,
        preceding_assistant_model: Option<&str>,
    ) -> Result<()> {
        self.conv_db.consume_queued_prompts(
            &self.session(),
            turn_id,
            prompts,
            preceding_assistant_content,
            preceding_assistant_reasoning,
            preceding_assistant_provider_id,
            preceding_assistant_model,
            &self.queue_session_id,
        )
    }

    pub fn consume_queued_prompts_with_checkpoint(
        &self,
        turn_id: &str,
        prompts: &[(String, String)],
        preceding_assistant_content: Option<&str>,
        preceding_assistant_reasoning: Option<&str>,
        preceding_assistant_provider_id: Option<&str>,
        preceding_assistant_model: Option<&str>,
        checkpoint: TurnRedoCheckpointPayload,
    ) -> Result<()> {
        self.conv_db.consume_queued_prompts_with_checkpoint(
            &self.session(),
            turn_id,
            prompts,
            preceding_assistant_content,
            preceding_assistant_reasoning,
            preceding_assistant_provider_id,
            preceding_assistant_model,
            &self.queue_session_id,
            Some(checkpoint),
        )
    }

    /// Explicit-cancel variant of queue cleanup: drop still-queued prompts
    /// outright (no fold into context) and return the dropped ids.
    pub fn delete_queued_prompts(&self) -> Result<Vec<String>> {
        self.conv_db
            .delete_queued_prompts(&self.session(), &self.queue_session_id)
    }

    pub fn discard_queued_prompts(&self) -> Result<usize> {
        self.conv_db
            .discard_queued_prompts(&self.session(), &self.queue_session_id)
    }

    pub fn remove_queued_prompt(&self, prompt_id: &str) -> Result<bool> {
        self.conv_db
            .remove_queued_prompt(&self.session(), prompt_id, &self.queue_session_id)
    }

    pub fn load_session_loaded_tools(&self) -> Result<BTreeSet<String>> {
        self.conv_db
            .load_session_loaded_items(&self.session(), "tool")
    }

    pub fn load_session_loaded_tools_with_sources(&self) -> Result<Vec<(String, Option<String>)>> {
        self.conv_db
            .load_session_loaded_items_with_sources(&self.session(), "tool")
    }

    pub fn add_session_loaded_tools(
        &self,
        names: &[String],
        source_turn_id: Option<&str>,
    ) -> Result<()> {
        self.conv_db
            .add_session_loaded_items(&self.session(), "tool", names, source_turn_id)?;
        Ok(())
    }

    pub fn add_session_loaded_targets(
        &self,
        names: &[String],
        source_turn_id: Option<&str>,
    ) -> Result<()> {
        self.conv_db
            .add_session_loaded_items(&self.session(), "target", names, source_turn_id)?;
        Ok(())
    }

    pub fn recover_stale_turns(&self) -> Result<usize> {
        let recoveries = self.conv_db.recover_stale_running_turns()?;
        for recovery in &recoveries {
            if recovery.restored_redo {
                self.reconcile_managed_artifacts_for_turn(&recovery.session_id, &recovery.turn_id)?;
            } else {
                self.recover_journal_assets(&recovery.session_id, &recovery.turn_id)?;
            }
        }
        Ok(recoveries.len())
    }

    pub fn history(&self, limit: usize) -> Result<Vec<StoredConversationEntry>> {
        let turns = self
            .conv_db
            .load_turns(&self.session())?
            .into_iter()
            .filter(|turn| !turn.is_summary)
            .collect();
        let mut entries = turns_to_entries(turns);
        let start = entries.len().saturating_sub(limit);
        Ok(entries.split_off(start))
    }

    pub fn load_conversation(&self) -> Result<Vec<StoredConversationEntry>> {
        let turns = self
            .conv_db
            .load_turns(&self.session())?
            .into_iter()
            .filter(|turn| !turn.is_summary)
            .collect();
        Ok(turns_to_entries(turns))
    }

    #[allow(dead_code)]
    pub fn load_turns(&self) -> Result<Vec<Turn>> {
        self.conv_db.load_turns(&self.session())
    }

    #[allow(dead_code)]
    pub fn load_turns_excluding(&self, exclude_turn_id: &str) -> Result<Vec<Turn>> {
        self.conv_db
            .load_turns_excluding(&self.session(), exclude_turn_id)
    }

    pub fn load_visible_turns(&self) -> Result<Vec<Turn>> {
        self.conv_db.load_visible_turns(&self.session())
    }

    /// Display transcripts of this session's last `limit` turns, for redrawing
    /// a reopened REPL.
    pub fn session_replay(&self, limit: usize) -> Result<Vec<conversation_db::TurnReplay>> {
        self.conv_db.session_replay(&self.session(), limit)
    }

    pub fn load_visible_turns_excluding(&self, exclude_turn_id: &str) -> Result<Vec<Turn>> {
        self.conv_db
            .load_visible_turns_excluding(&self.session(), exclude_turn_id)
    }

    #[allow(dead_code)]
    pub fn hide_turns_before_seq(&self, seq: i64) -> Result<usize> {
        self.conv_db.hide_turns_before_seq(&self.session(), seq)
    }

    #[allow(dead_code)]
    pub fn insert_summary_turn(
        &self,
        summary: &str,
        tokens: TurnTokens,
        token_usage_estimated: bool,
    ) -> Result<()> {
        self.conv_db
            .insert_summary_turn(&self.session(), summary, tokens, token_usage_estimated)
    }

    pub fn load_last_summary(&self) -> Result<Option<Turn>> {
        self.conv_db.load_last_summary(&self.session())
    }

    pub fn prune_stale_tool_reports(
        &self,
        protect_recent: usize,
        min_saved_chars: usize,
    ) -> Result<PruneStats> {
        self.conv_db
            .prune_stale_tool_reports(&self.session(), protect_recent, min_saved_chars)
    }

    pub fn session_last_request_at(&self) -> Result<Option<i64>> {
        self.conv_db.session_last_request_at(&self.session())
    }

    pub fn replace_visible_with_summary(
        &self,
        fold_turn_ids: &[String],
        visible_turn_ids: &[String],
        summary: &str,
        tokens: TurnTokens,
        token_usage_estimated: bool,
        footprint_json: Option<&str>,
    ) -> Result<()> {
        self.conv_db.replace_visible_with_summary(
            &self.session(),
            fold_turn_ids,
            visible_turn_ids,
            summary,
            tokens,
            token_usage_estimated,
            footprint_json,
        )
    }

    pub fn merge_turn_footprint(
        &self,
        turn_id: &str,
        delta: &crate::state::ToolFootprint,
    ) -> Result<()> {
        self.conv_db.merge_turn_footprint(turn_id, delta)
    }

    pub fn load_merged_footprint(
        &self,
        turn_ids: &[String],
    ) -> Result<crate::state::ToolFootprint> {
        self.conv_db
            .load_merged_footprint(&self.session(), turn_ids)
    }

    pub fn oldest_evictable_visible_turns(&self, count: usize) -> Result<Vec<Turn>> {
        self.conv_db
            .oldest_evictable_visible_turns(&self.session(), count)
    }

    pub fn delete_visible_turns(&self, turn_ids: &[String]) -> Result<usize> {
        self.conv_db.delete_visible_turns(&self.session(), turn_ids)
    }

    pub fn delete_visible_turns_checked(
        &self,
        turn_ids: &[String],
        expected_loaded_tools: Option<&[(String, Option<String>)]>,
    ) -> Result<usize> {
        self.conv_db
            .delete_visible_turns_checked(&self.session(), turn_ids, expected_loaded_tools)
    }

    pub fn archive_and_delete_visible_turns(
        &self,
        archive_db: &Path,
        turns: &[EvictedTurn],
        turn_ids: &[String],
        expected_loaded_tools: Option<&[(String, Option<String>)]>,
    ) -> Result<usize> {
        self.conv_db.archive_and_delete_visible_turns(
            &self.session(),
            archive_db,
            turns,
            turn_ids,
            expected_loaded_tools,
        )
    }

    pub fn reset_conversation(&self) -> Result<()> {
        self.clear_session_content()?;
        usage::reset_conversation(&self.usage_file())
    }

    pub fn reset_conversation_usage(&self) -> Result<()> {
        usage::reset_conversation(&self.usage_file())
    }

    pub fn reset_persona_contexts(&self, persona: &str, platform: &str) -> Result<Vec<String>> {
        let session_ids = self.conv_db.reset_persona_contexts(persona, platform)?;
        self.remove_artifact_session_dirs(&session_ids)?;
        Ok(session_ids)
    }

    /// Clears only the pinned session's conversation state. Platform commands
    /// use this instead of `reset_conversation` so they cannot reset the
    /// daemon-wide usage counters or another client's current session.
    pub fn clear_session_content(&self) -> Result<()> {
        let session_id = self.session();
        self.conv_db.reset(&session_id)?;
        self.remove_artifact_session_dir(&session_id)
    }

    pub fn remove_artifact_session_dirs(&self, session_ids: &[String]) -> Result<()> {
        for session_id in session_ids {
            self.remove_artifact_session_dir(session_id)?;
        }
        Ok(())
    }

    pub fn remove_artifact_session_dir(&self, session_id: &str) -> Result<()> {
        use std::path::Component;

        let mut components = Path::new(session_id).components();
        if !matches!(components.next(), Some(Component::Normal(_))) || components.next().is_some() {
            anyhow::bail!("invalid session id for Artifact workspace cleanup");
        }
        let path = self.artifacts_dir.join(session_id);
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        if metadata.file_type().is_symlink() || metadata.is_file() {
            std::fs::remove_file(path)?;
        } else {
            std::fs::remove_dir_all(path)?;
        }
        Ok(())
    }

    pub fn recover_journal_assets(&self, session_id: &str, turn_id: &str) -> Result<()> {
        let Some(turn) = self
            .conv_db
            .load_turns(session_id)?
            .into_iter()
            .find(|turn| turn.turn_id == turn_id)
        else {
            return Ok(());
        };
        if turn.journal_events.is_empty() {
            return Ok(());
        }
        let mut images = self
            .conv_db
            .load_image_assets(session_id)?
            .into_iter()
            .filter(|asset| asset.turn_id == turn_id)
            .collect::<Vec<_>>();
        let mut artifacts = self
            .conv_db
            .load_artifact_assets(session_id)?
            .into_iter()
            .filter(|asset| asset.turn_id == turn_id)
            .collect::<Vec<_>>();
        for event in &turn.journal_events {
            let kind = event.kind.as_str();
            if kind != "image" && kind != "artifact" {
                continue;
            }
            let Some(payload) = event
                .text_payload
                .as_deref()
                .and_then(|payload| serde_json::from_str::<serde_json::Value>(payload).ok())
            else {
                continue;
            };
            let Some(raw_path) = payload.get("path").and_then(serde_json::Value::as_str) else {
                continue;
            };
            let path = PathBuf::from(raw_path);
            if !path.is_file() {
                continue;
            }
            if kind == "image" {
                let alt = payload
                    .get("alt")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default();
                if images.iter().any(|asset| {
                    asset.tool_id.as_deref() == event.call_id.as_deref() && asset.alt == alt
                }) {
                    continue;
                }
                match self.save_image_asset(turn_id, event.call_id.as_deref(), &path, alt) {
                    Ok(asset) => images.push(asset),
                    Err(error) => tracing::warn!(
                        turn_id,
                        path = %path.display(),
                        error = %error,
                        "failed to recover an interrupted image asset"
                    ),
                }
            } else {
                let Ok(source_key) = path
                    .canonicalize()
                    .map(|path| path.to_string_lossy().into_owned())
                else {
                    continue;
                };
                if artifacts.iter().any(|asset| asset.source_key == source_key) {
                    continue;
                }
                let title = payload
                    .get("title")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default();
                match self.save_artifact_asset(turn_id, event.call_id.as_deref(), &path, title) {
                    Ok(asset) => artifacts.push(asset),
                    Err(error) => tracing::warn!(
                        turn_id,
                        path = %path.display(),
                        error = %error,
                        "failed to recover an interrupted Artifact asset"
                    ),
                }
            }
        }
        Ok(())
    }

    pub fn reconcile_managed_artifacts_for_turn(
        &self,
        session_id: &str,
        turn_id: &str,
    ) -> Result<()> {
        use std::path::Component;

        let mut components = Path::new(session_id).components();
        if !matches!(components.next(), Some(Component::Normal(_))) || components.next().is_some() {
            bail!("invalid session id for Artifact workspace recovery");
        }
        let restored = self.conv_db.load_artifact_asset_data_for_turn(turn_id)?;
        let session_dir = self.artifacts_dir.join(session_id);
        match std::fs::symlink_metadata(&session_dir) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                bail!(
                    "Artifact recovery path is not a directory: {}",
                    session_dir.display()
                );
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && restored.is_empty() => {
                return Ok(())
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir_all(&session_dir)?;
            }
            Err(error) => return Err(error.into()),
        }
        std::fs::set_permissions(&session_dir, std::fs::Permissions::from_mode(0o700))?;
        let canonical_dir = session_dir.canonicalize()?;
        let managed_target = |source_key: &str| -> Option<PathBuf> {
            let source = Path::new(source_key);
            let file_name = source.file_name()?;
            let parent = source.parent()?.canonicalize().ok()?;
            (parent == canonical_dir).then(|| canonical_dir.join(file_name))
        };

        let keep = self
            .conv_db
            .load_artifact_assets(session_id)?
            .into_iter()
            .filter_map(|asset| managed_target(&asset.source_key))
            .collect::<HashSet<_>>();
        for artifact in restored {
            let Some(target) = managed_target(&artifact.asset.source_key) else {
                continue;
            };
            let mut temp = tempfile::NamedTempFile::new_in(&canonical_dir)?;
            temp.as_file_mut()
                .set_permissions(std::fs::Permissions::from_mode(0o600))?;
            temp.write_all(&artifact.bytes)?;
            temp.as_file_mut().sync_all()?;
            temp.persist(&target)
                .map_err(|error| error.error)
                .with_context(|| format!("restoring Artifact file: {}", target.display()))?;
        }
        for entry in std::fs::read_dir(&canonical_dir)? {
            let entry = entry?;
            let path = entry.path();
            if keep.contains(&path) {
                continue;
            }
            let metadata = std::fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() || metadata.is_file() {
                std::fs::remove_file(path)?;
            }
        }
        Ok(())
    }

    pub fn undo_last_turn(&self) -> Result<(usize, Option<String>)> {
        self.conv_db.undo_last_turn(&self.session())
    }

    pub fn redo_candidate(&self) -> Result<Option<RedoCandidate>> {
        self.conv_db.redo_candidate(&self.session())
    }

    pub fn load_redo_batch_prompts(
        &self,
        turn_id: &str,
        prompt_ids: &[String],
    ) -> Result<Vec<QueuedPrompt>> {
        self.conv_db
            .load_redo_batch_prompts(&self.session(), turn_id, prompt_ids)
    }

    pub fn begin_redo(
        &self,
        turn_id: &str,
        input_id: &str,
        input_kind: RedoInputKind,
        expected_revision: i64,
        content: &str,
        display_content: &str,
        owner_pid: u32,
    ) -> Result<RedoStart> {
        self.conv_db.begin_redo(
            &self.session(),
            turn_id,
            input_id,
            input_kind,
            expected_revision,
            content,
            display_content,
            owner_pid,
            &self.queue_session_id,
        )
    }

    pub fn add_usage(&self, usage: &Usage, meta: UsageMeta<'_>) -> Result<()> {
        self.init_files()?;
        usage::add_usage(&self.usage_file(), usage)?;
        self.record_usage_history(usage, meta, false);
        Ok(())
    }

    pub fn add_auxiliary_usage(&self, usage: &Usage, meta: UsageMeta<'_>) -> Result<()> {
        self.init_files()?;
        usage::add_auxiliary_usage(&self.usage_file(), usage)?;
        self.record_usage_history(usage, meta, true);
        Ok(())
    }

    /// 历史明细落账失败只告警:usage.json 累计是正账,明细缺一行不该
    /// 让整个回合报错。
    pub fn record_usage_history(&self, usage: &Usage, meta: UsageMeta<'_>, aux: bool) {
        if let Err(error) = usage::record_usage(&self.usage_history_file(), usage, meta, aux) {
            tracing::warn!(error = %error, "recording usage history failed");
        }
    }

    pub fn usage_history_file(&self) -> PathBuf {
        self.state_dir.join("usage-history.jsonl")
    }

    /// `config` 提供时按 models.dev 单价做计费估算;None 则费用字段全零。
    pub fn usage_stats(
        &self,
        range: UsageRange,
        config: Option<&crate::config::AppConfig>,
    ) -> Result<usage::UsageStats> {
        match config {
            Some(config) => {
                let price = crate::models_cache::pricing_resolver(config);
                usage::usage_stats(&self.usage_history_file(), range, &price)
            }
            None => usage::usage_stats(&self.usage_history_file(), range, &|_, _| None),
        }
    }

    pub fn usage_details(
        &self,
        limit: usize,
        src: Option<&str>,
        model: Option<&str>,
        config: Option<&crate::config::AppConfig>,
    ) -> Result<Vec<usage::UsageRecord>> {
        match config {
            Some(config) => {
                let price = crate::models_cache::pricing_resolver(config);
                usage::usage_details(&self.usage_history_file(), limit, src, model, &price)
            }
            None => {
                usage::usage_details(&self.usage_history_file(), limit, src, model, &|_, _| None)
            }
        }
    }

    #[allow(dead_code)]
    pub fn usage_snapshot(&self) -> Result<UsageSnapshot> {
        usage::snapshot(&self.usage_file())
    }

    /// Lifetime token total of the current session (survives compaction,
    /// zeroed by /reset). This is the Σ shown in the REPL/WebUI footer.
    pub fn session_cumulative_tokens(&self) -> Result<u64> {
        self.conv_db.session_token_total(&self.session())
    }

    /// Same Σ, plus the prompt and cache-read halves the cumulative cache rate
    /// is computed from.
    pub fn session_cumulative_token_totals(&self) -> Result<TurnTokens> {
        self.conv_db.session_token_totals(&self.session())
    }

    pub fn clear_last_usage(&self) -> Result<()> {
        usage::clear_last_usage(&self.usage_file())
    }

    #[allow(dead_code)]
    pub fn has_running_turns(&self) -> Result<bool> {
        self.conv_db.has_running_turns(&self.session())
    }

    #[allow(dead_code)]
    pub fn running_turn_summaries(&self) -> Result<Vec<String>> {
        self.conv_db.running_turn_summaries(&self.session())
    }

    #[allow(dead_code)]
    pub fn running_turn_summaries_excluding(&self, exclude_turn_id: &str) -> Result<Vec<String>> {
        self.conv_db
            .running_turn_summaries_excluding(&self.session(), exclude_turn_id)
    }

    #[allow(dead_code)]
    pub fn migrate_from_jsonl(&self) -> Result<usize> {
        let jsonl_path = self.conversation_file();
        self.conv_db
            .migrate_from_jsonl(&self.session(), &jsonl_path)
    }

    pub fn conversation_file(&self) -> PathBuf {
        self.state_dir.join("conversation.jsonl")
    }

    pub fn usage_file(&self) -> PathBuf {
        self.state_dir.join("usage.json")
    }

    pub fn profile_file(&self) -> PathBuf {
        self.state_dir.join("profile.md")
    }

    pub fn prompt_fingerprint_file(&self) -> PathBuf {
        let key = blake3::hash(self.session().as_bytes()).to_hex();
        self.state_dir
            .join("prompt-fingerprints")
            .join(format!("{key}.sha256"))
    }
}
