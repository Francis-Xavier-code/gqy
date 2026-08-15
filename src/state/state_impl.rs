//! state_impl — 自 src/state/mod.rs 拆分。

use super::*;

impl StateStore {
    pub fn new(paths: &GQYPaths) -> Result<Self> {
        let state_dir = paths.state_dir.clone();
        let conv_db = Arc::new(ConversationDb::open(&state_dir)?);
        let platform_access = shared_platform_access_index(&state_dir, &conv_db)?;
        let session_id = Arc::new(std::sync::RwLock::new(Arc::<str>::from(
            conv_db.resolve_current_session()?,
        )));
        let queue_owner_pid = std::process::id();
        let queue_session_id: Arc<str> = format!(
            "queue_{}_{}_{}",
            queue_owner_pid,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0),
            rand::random::<u64>()
        )
        .into();
        conv_db.discard_stale_queued_prompts(&queue_session_id, queue_owner_pid)?;
        Ok(Self {
            state_dir,
            artifacts_dir: paths.data_dir.join("artifacts"),
            conv_db,
            platform_access,
            session_id,
            queue_session_id,
            queue_owner_pid,
        })
    }

    pub fn session_id(&self) -> Arc<str> {
        self.session_id.read().unwrap().clone()
    }

    pub(crate) fn state_dir(&self) -> &Path {
        &self.state_dir
    }

    pub fn session(&self) -> Arc<str> {
        self.session_id.read().unwrap().clone()
    }

    /// Points this store (and every clone sharing it) at another session.
    /// The caller is responsible for persisting the current-session pointer.
    pub fn adopt_session(&self, session_id: &str) {
        *self.session_id.write().unwrap() = session_id.into();
    }

    /// A clone pinned to the given session: it shares the database but holds
    /// its own session pointer, unaffected by later `switch_session` /
    /// `adopt_session` calls on other clones. Used by concurrently running
    /// turns so each keeps writing to the session it started in.
    pub fn pinned(&self, session_id: &str) -> Self {
        Self {
            state_dir: self.state_dir.clone(),
            artifacts_dir: self.artifacts_dir.clone(),
            conv_db: self.conv_db.clone(),
            platform_access: self.platform_access.clone(),
            session_id: Arc::new(std::sync::RwLock::new(session_id.into())),
            queue_session_id: self.queue_session_id.clone(),
            queue_owner_pid: self.queue_owner_pid,
        }
    }

    /// Like [`pinned`], but with a fresh queue identity so concurrently
    /// running turns in the same session never consume each other's queued
    /// follow-up prompts. Callers should `discard_queued_prompts()` when the
    /// turn finishes.
    pub fn pinned_for_turn(&self, session_id: &str) -> Self {
        let mut store = self.pinned(session_id);
        store.queue_session_id = format!(
            "queue_{}_{}_{}",
            store.queue_owner_pid,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0),
            rand::random::<u64>()
        )
        .into();
        store
    }

    pub(crate) fn queue_target(&self, turn_id: impl Into<String>) -> RunningTurnQueueTarget {
        RunningTurnQueueTarget {
            turn_id: turn_id.into(),
            queue_session_id: Some(self.queue_session_id.to_string()),
            owner_pid: Some(self.queue_owner_pid),
        }
    }

    /// Whether any session has a running turn (global admin guard).
    pub fn has_any_running_turns(&self) -> Result<bool> {
        self.conv_db.has_any_running_turns()
    }

    /// Switches the active session and persists the current-session pointer.
    pub fn switch_session(&self, session_id: &str) -> Result<()> {
        self.conv_db.set_current_session(session_id)?;
        self.adopt_session(session_id);
        Ok(())
    }

    pub fn has_platform_access_grant(
        &self,
        platform: &str,
        account_id: &str,
        permission: &str,
        subject_kind: &str,
        subject_id: &str,
    ) -> bool {
        let access = self.platform_access.index.read().unwrap();
        access.contains(
            platform,
            GLOBAL_PLATFORM_ACCOUNT_SCOPE,
            permission,
            subject_kind,
            subject_id,
        ) || (account_id != GLOBAL_PLATFORM_ACCOUNT_SCOPE
            && access.contains(platform, account_id, permission, subject_kind, subject_id))
    }

    pub fn platform_access_grants(&self, platform: &str) -> Result<Vec<PlatformAccessGrant>> {
        let _mutation = self.platform_access.mutations.lock().unwrap();
        self.conv_db.platform_access_grants(Some(platform))
    }

    pub(crate) fn platform_access_grants_if_authorized(
        &self,
        platform: &str,
        authorization: &PlatformAccessAuthorization,
    ) -> Result<Option<Vec<PlatformAccessGrant>>> {
        let _mutation = self.platform_access.mutations.lock().unwrap();
        if !self.platform_access_authorized(authorization) {
            return Ok(None);
        }
        self.conv_db
            .platform_access_grants(Some(platform))
            .map(Some)
    }

    pub(crate) fn mutate_platform_access_grant_if_authorized(
        &self,
        key: &PlatformAccessGrantKey,
        actor: &PlatformAccessActor,
        operation: PlatformAccessMutation,
        authorization: &PlatformAccessAuthorization,
    ) -> Result<PlatformAccessMutationResult> {
        let _mutation = self.platform_access.mutations.lock().unwrap();
        if !self.platform_access_authorized(authorization) {
            return Ok(PlatformAccessMutationResult::Unauthorized);
        }
        match operation {
            PlatformAccessMutation::Grant => {
                let inserted = self.conv_db.add_platform_access_grant(key, actor)?;
                if inserted {
                    self.platform_access.index.write().unwrap().insert(key);
                    Ok(PlatformAccessMutationResult::Changed)
                } else {
                    Ok(PlatformAccessMutationResult::Unchanged)
                }
            }
            PlatformAccessMutation::Revoke => {
                let was_cached = self.platform_access.index.write().unwrap().remove(key);
                match self.conv_db.remove_platform_access_grant(key, actor) {
                    Ok(true) => Ok(PlatformAccessMutationResult::Changed),
                    Ok(false) => Ok(PlatformAccessMutationResult::Unchanged),
                    Err(error) => {
                        if was_cached {
                            self.platform_access.index.write().unwrap().insert(key);
                        }
                        Err(error)
                    }
                }
            }
        }
    }

    /// Runs an operation while holding the platform-access mutation lock.
    /// The callback must not call another access-control mutation method.
    pub(crate) fn with_platform_access_authorization<T>(
        &self,
        authorization: &PlatformAccessAuthorization,
        operation: impl FnOnce() -> Result<T>,
    ) -> Result<Option<T>> {
        let _mutation = self.platform_access.mutations.lock().unwrap();
        if !self.platform_access_authorized(authorization) {
            return Ok(None);
        }
        operation().map(Some)
    }

    pub fn add_platform_access_grant(
        &self,
        key: &PlatformAccessGrantKey,
        actor: &PlatformAccessActor,
    ) -> Result<bool> {
        let _mutation = self.platform_access.mutations.lock().unwrap();
        let inserted = self.conv_db.add_platform_access_grant(key, actor)?;
        if inserted {
            self.platform_access.index.write().unwrap().insert(key);
        }
        Ok(inserted)
    }

    pub fn remove_platform_access_grant(
        &self,
        key: &PlatformAccessGrantKey,
        actor: &PlatformAccessActor,
    ) -> Result<bool> {
        let _mutation = self.platform_access.mutations.lock().unwrap();
        let was_cached = self.platform_access.index.write().unwrap().remove(key);
        match self.conv_db.remove_platform_access_grant(key, actor) {
            Ok(deleted) => Ok(deleted),
            Err(error) => {
                if was_cached {
                    self.platform_access.index.write().unwrap().insert(key);
                }
                Err(error)
            }
        }
    }

    pub fn platform_access_authorized(&self, authorization: &PlatformAccessAuthorization) -> bool {
        if authorization.statically_authorized {
            return true;
        }
        let key = &authorization.dynamic_key;
        let access = self.platform_access.index.read().unwrap();
        access.contains(
            &key.platform,
            GLOBAL_PLATFORM_ACCOUNT_SCOPE,
            &key.permission,
            &key.subject_kind,
            &key.subject_id,
        ) || (key.account_scope != GLOBAL_PLATFORM_ACCOUNT_SCOPE
            && access.contains(
                &key.platform,
                &key.account_scope,
                &key.permission,
                &key.subject_kind,
                &key.subject_id,
            ))
    }

    pub fn persona_current_session(&self, persona: &str) -> Result<Option<String>> {
        self.conv_db.persona_current_session(persona)
    }

    pub fn set_persona_current_session(&self, persona: &str, session_id: &str) -> Result<()> {
        self.conv_db
            .set_persona_current_session(persona, session_id)
    }

    /// Session the REPL was last on, or `None` when that pointer is unset or
    /// stale (deleted, archived, or another persona's).
    pub fn repl_session(&self, persona: &str) -> Result<Option<String>> {
        self.conv_db.repl_session(persona)
    }

    pub fn set_repl_session(&self, persona: &str, session_id: &str) -> Result<()> {
        self.conv_db.set_repl_session(persona, session_id)
    }

    /// Claims persona-less sessions (schema-v2 migrated rows) for the active
    /// persona scope.
    pub fn adopt_sessions_for_persona(&self, persona: &str) -> Result<()> {
        self.conv_db.adopt_sessions_for_persona(persona)
    }

    pub fn rename_persona_scope(&self, old_scope: &str, new_scope: &str) -> Result<()> {
        self.conv_db.rename_persona_scope(old_scope, new_scope)
    }

    pub fn delete_persona_scope(&self, scope: &str) -> Result<()> {
        let session_ids = self
            .conv_db
            .list_sessions(scope)?
            .into_iter()
            .map(|session| session.record.session_id)
            .collect::<Vec<_>>();
        self.conv_db.delete_persona_scope(scope)?;
        self.remove_artifact_session_dirs(&session_ids)
    }

    pub fn session_record(&self, session_id: &str) -> Result<Option<SessionRecord>> {
        self.conv_db.session_record(session_id)
    }

    pub fn list_sessions(&self, persona: &str) -> Result<Vec<SessionOverview>> {
        self.conv_db.list_sessions(persona)
    }

    pub fn list_local_sessions(&self, persona: &str) -> Result<Vec<SessionOverview>> {
        self.conv_db.list_local_sessions(persona)
    }

    pub fn background_report_replies_after(
        &self,
        session_id: &str,
        after_seq: i64,
    ) -> Result<Vec<(i64, String, String, String)>> {
        self.conv_db
            .background_report_replies_after(session_id, after_seq)
    }

    pub fn latest_turn_seq(&self, session_id: &str) -> Result<i64> {
        self.conv_db.latest_turn_seq(session_id)
    }

    pub fn oldest_visible_turn_timestamp(
        &self,
        excluding_turn_id: &str,
    ) -> Result<Option<String>> {
        self.conv_db
            .oldest_visible_turn_timestamp(&self.session(), excluding_turn_id)
    }

    pub fn is_platform_session(&self, session_id: &str) -> Result<bool> {
        self.conv_db.is_platform_session(session_id)
    }

    pub fn persona_reset_session_ids(&self, persona: &str, platform: &str) -> Result<Vec<String>> {
        self.conv_db.persona_reset_session_ids(persona, platform)
    }

    pub fn platform_session_bindings(
        &self,
        persona: &str,
        platform: &str,
    ) -> Result<Vec<PlatformSessionBinding>> {
        self.conv_db.platform_session_bindings(persona, platform)
    }

    pub fn create_session(
        &self,
        persona: &str,
        name: &str,
        kind: &str,
        parent_session_id: Option<&str>,
    ) -> Result<SessionRecord> {
        self.conv_db
            .create_session(persona, name, kind, parent_session_id)
    }

    pub fn create_or_get_platform_session(
        &self,
        key: &PlatformSessionBindingKey,
        name: &str,
    ) -> Result<(SessionRecord, bool)> {
        self.conv_db.create_or_get_platform_session(key, name)
    }

    pub fn rename_session(&self, session_id: &str, name: &str) -> Result<()> {
        self.conv_db.rename_session(session_id, name)
    }

    pub fn set_session_workspace(&self, session_id: &str, workspace: Option<&str>) -> Result<()> {
        self.conv_db.set_session_workspace(session_id, workspace)
    }

    /// Per-session model pool override. None follows the global active pool.
    pub fn session_model_override(
        &self,
        session_id: &str,
    ) -> Result<Option<Vec<crate::config::ActiveProviderModelConfig>>> {
        let Some(encoded) = self.conv_db.session_model_override(session_id)? else {
            return Ok(None);
        };
        let models =
            serde_json::from_str::<Vec<crate::config::ActiveProviderModelConfig>>(&encoded)
                .with_context(|| format!("invalid session model override for {session_id}"))?;
        Ok((!models.is_empty()).then_some(models))
    }

    pub fn set_session_model_override(
        &self,
        session_id: &str,
        models: Option<&[crate::config::ActiveProviderModelConfig]>,
    ) -> Result<()> {
        let encoded = match models {
            Some(models) if !models.is_empty() => Some(serde_json::to_string(models)?),
            _ => None,
        };
        self.conv_db
            .set_session_model_override(session_id, encoded.as_deref())
    }

    pub fn delete_session(&self, session_id: &str) -> Result<()> {
        self.conv_db.delete_session(session_id)?;
        self.remove_artifact_session_dir(session_id)
    }

    pub fn touch_session(&self, session_id: &str) -> Result<()> {
        self.conv_db.touch_session(session_id)
    }

    pub fn find_session_by_name(&self, persona: &str, name: &str) -> Result<Option<SessionRecord>> {
        self.conv_db.find_session_by_name(persona, name)
    }

    pub fn find_local_session_by_name(
        &self,
        persona: &str,
        name: &str,
    ) -> Result<Option<SessionRecord>> {
        self.conv_db.find_local_session_by_name(persona, name)
    }

    pub fn find_platform_session_binding(
        &self,
        key: &PlatformSessionBindingKey,
    ) -> Result<Option<String>> {
        self.conv_db.find_platform_session_binding(key)
    }

    pub fn bind_platform_session(
        &self,
        key: &PlatformSessionBindingKey,
        session_id: &str,
    ) -> Result<()> {
        self.conv_db.bind_platform_session(key, session_id)
    }

    pub fn claim_platform_session(
        &self,
        key: &PlatformSessionBindingKey,
        candidate_session_id: &str,
    ) -> Result<String> {
        self.conv_db
            .claim_platform_session(key, candidate_session_id)
    }

    pub fn unbind_platform_session(&self, key: &PlatformSessionBindingKey) -> Result<bool> {
        self.conv_db.unbind_platform_session(key)
    }

    pub fn plugin_get_json<T: serde::de::DeserializeOwned>(
        &self,
        scope: &PlatformPluginScopeKey,
        key: &str,
    ) -> Result<Option<T>> {
        self.conv_db.plugin_get_json(scope, key)
    }

    pub(crate) fn plugin_json_revision(
        &self,
        scope: &PlatformPluginScopeKey,
        key: &str,
    ) -> Result<Option<String>> {
        self.conv_db.plugin_json_revision(scope, key)
    }

    pub(crate) fn plugin_get_json_with_revision<T: serde::de::DeserializeOwned>(
        &self,
        scope: &PlatformPluginScopeKey,
        key: &str,
    ) -> Result<Option<(T, String)>> {
        self.conv_db.plugin_get_json_with_revision(scope, key)
    }

    pub fn plugin_put_json<T: Serialize + ?Sized>(
        &self,
        scope: &PlatformPluginScopeKey,
        key: &str,
        value: &T,
    ) -> Result<()> {
        self.conv_db.plugin_put_json(scope, key, value)
    }

    /// Atomically reads and replaces one platform-plugin JSON value.
    pub fn plugin_update_json<T, F>(
        &self,
        scope: &PlatformPluginScopeKey,
        key: &str,
        update: F,
    ) -> Result<Option<T>>
    where
        T: serde::de::DeserializeOwned + Serialize,
        F: FnOnce(Option<T>) -> Result<Option<T>>,
    {
        self.conv_db.plugin_update_json(scope, key, update)
    }

    pub fn plugin_delete_key(&self, scope: &PlatformPluginScopeKey, key: &str) -> Result<bool> {
        self.conv_db.plugin_delete_key(scope, key)
    }

    pub fn plugin_delete_scope(&self, scope: &PlatformPluginScopeKey) -> Result<usize> {
        self.conv_db.plugin_delete_scope(scope)
    }

    pub fn put_platform_meme_ref(&self, record: &PlatformMemeRefRecord) -> Result<()> {
        self.conv_db.put_platform_meme_ref(record)
    }

    pub fn platform_meme_refs_for_message(
        &self,
        platform: &str,
        account_id: &str,
        conversation_kind: &str,
        conversation_id: &str,
        message_id: &str,
    ) -> Result<Vec<PlatformMemeRefRecord>> {
        self.conv_db.platform_meme_refs_for_message(
            platform,
            account_id,
            conversation_kind,
            conversation_id,
            message_id,
        )
    }

    pub fn delete_platform_meme_ref(&self, library: &str, meme_id: &str) -> Result<usize> {
        self.conv_db.delete_platform_meme_ref(library, meme_id)
    }

    #[allow(clippy::too_many_arguments)]
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
        self.conv_db.record_subagent_usage(
            session_id,
            provider_id,
            model,
            context_window,
            prompt_tokens,
            completion_tokens,
            total_tokens,
            cache_read_tokens,
        )
    }

    pub fn delete_subagent_sessions_older_than(&self, days: i64) -> Result<usize> {
        self.conv_db.delete_subagent_sessions_older_than(days)
    }

    pub fn delete_ask_sessions_older_than(&self, hours: i64) -> Result<usize> {
        self.conv_db.delete_ask_sessions_older_than(hours)
    }

    pub fn init_files(&self) -> Result<()> {
        std::fs::create_dir_all(&self.state_dir)?;
        if !self.usage_file().exists() {
            std::fs::write(self.usage_file(), "{\n  \"requests\": 0,\n  \"prompt_tokens\": 0,\n  \"completion_tokens\": 0,\n  \"total_tokens\": 0,\n  \"conversation_tokens\": 0\n}\n")?;
        }
        if !self.profile_file().exists() {
            std::fs::write(self.profile_file(), "# GQY Profile\n\n")?;
        }
        Ok(())
    }

    pub fn reset_if_prompt_changed(&self, system_prompt: &str) -> Result<()> {
        self.reset_if_prompt_changed_with_compatible(system_prompt, None)
    }

    pub(crate) fn reset_if_prompt_changed_with_compatible(
        &self,
        system_prompt: &str,
        // Kept for call-site compatibility; since the v7 no-delete semantics
        // every previous prompt is effectively compatible.
        _compatible_previous_prompt: Option<&str>,
    ) -> Result<()> {
        self.init_files()?;
        let fingerprint = prompt_fingerprint(system_prompt);
        let file = self.prompt_fingerprint_file();
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if !file.exists() && self.state_dir.join("prompt.sha256").exists() {
            std::fs::write(file, format!("{fingerprint}\n"))?;
            return Ok(());
        }
        let previous = std::fs::read_to_string(&file).unwrap_or_default();
        if previous.trim() != fingerprint {
            // v7 Release 3: a persona prompt text change is a planned cache
            // cold start, not a reason to destroy data. Earlier versions
            // physically deleted every turn and the session's artifacts here,
            // which meant "upgrade the binary → conversations silently
            // vanish". History and artifacts are kept; only the fingerprint
            // advances. Users who want a clean slate still have /clear.
            tracing::info!(
                "persona prompt fingerprint changed; keeping session history (cache cold start)"
            );
            self.clear_last_usage()?;
            std::fs::write(file, format!("{fingerprint}\n"))?;
        }
        Ok(())
    }

    #[allow(dead_code)]
    pub fn conv_db(&self) -> &ConversationDb {
        &self.conv_db
    }

    pub fn start_turn(&self, turn_id: &str, user_content: &str, owner_pid: u32) -> Result<()> {
        self.start_turn_with_display(turn_id, user_content, user_content, owner_pid, None)
    }

    pub fn start_turn_with_display(
        &self,
        turn_id: &str,
        user_content: &str,
        display_content: &str,
        owner_pid: u32,
        attachment_run_id: Option<&str>,
    ) -> Result<()> {
        // Record the ambient turn workspace (if any) so the turn row captures
        // where its tools operated; NULL outside a turn workspace scope.
        let workspace =
            crate::tools::workspace::try_workspace().map(|path| path.display().to_string());
        self.conv_db.start_turn(
            &self.session(),
            turn_id,
            user_content,
            display_content,
            owner_pid,
            &self.queue_session_id,
            workspace.as_deref(),
            attachment_run_id,
        )
    }

    #[allow(dead_code)]
    pub fn complete_turn(
        &self,
        turn_id: &str,
        content: &str,
        reasoning: Option<&str>,
    ) -> Result<()> {
        self.conv_db.complete_turn(turn_id, content, reasoning)
    }

    pub fn interrupt_turn(&self, turn_id: &str) -> Result<()> {
        self.conv_db.interrupt_turn(turn_id)?;
        let session_id = self.session_id();
        self.recover_journal_assets(&session_id, turn_id)
    }

    pub fn interrupt_turn_revision(&self, turn_id: &str, revision: i64) -> Result<()> {
        let restored = self.conv_db.interrupt_turn_revision(turn_id, revision)?;
        if restored {
            let session_id = self
                .conv_db
                .turn_session_id(turn_id)?
                .context("restored redo turn no longer exists")?;
            self.reconcile_managed_artifacts_for_turn(&session_id, turn_id)?;
        } else {
            let session_id = self
                .conv_db
                .turn_session_id(turn_id)?
                .context("interrupted turn no longer exists")?;
            self.recover_journal_assets(&session_id, turn_id)?;
        }
        Ok(())
    }

    pub fn complete_turn_with_usage_and_model(
        &self,
        turn_id: &str,
        content: &str,
        reasoning: Option<&str>,
        provider_id: Option<&str>,
        model: Option<&str>,
        tokens: TurnTokens,
        token_usage_estimated: bool,
    ) -> Result<()> {
        self.conv_db.complete_turn_with_usage(
            turn_id,
            content,
            reasoning,
            provider_id,
            model,
            tokens,
            token_usage_estimated,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn complete_turn_revision_with_usage_and_model(
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
        self.conv_db.complete_turn_revision_with_usage(
            turn_id,
            revision,
            content,
            reasoning,
            provider_id,
            model,
            tokens,
            token_usage_estimated,
        )
    }

    pub fn append_persisted_context(&self, turn_id: &str, report: &str) -> Result<()> {
        self.conv_db.append_tool_report(turn_id, report.trim())
    }

    pub fn append_persisted_contexts(&self, turn_id: &str, reports: &[String]) -> Result<()> {
        self.conv_db.append_tool_reports(turn_id, reports)
    }

    /// Archives the transient system tail that was sent after the user message
    /// of this turn (v7 append-only fossilization). Replayed verbatim by
    /// history rendering so the byte stream stays a pure extension.
    pub fn set_turn_tool_flow(
        &self,
        turn_id: &str,
        flow: &[conversation_db::ToolFlowRound],
    ) -> Result<()> {
        self.conv_db.set_turn_tool_flow(turn_id, flow)
    }

    pub fn set_turn_context_messages(
        &self,
        turn_id: &str,
        messages: &[crate::llm::ChatMessage],
    ) -> Result<()> {
        self.conv_db.set_turn_context_messages(turn_id, messages)
    }

    #[allow(clippy::too_many_arguments)]
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
        self.conv_db.append_turn_journal_event(
            turn_id,
            revision,
            segment_index,
            kind,
            call_id,
            name,
            text_payload,
            blob_payload,
            ok,
        )
    }

    pub fn supersede_turn_journal_segment(
        &self,
        turn_id: &str,
        revision: i64,
        segment_index: i64,
    ) -> Result<()> {
        self.conv_db
            .supersede_turn_journal_segment(turn_id, revision, segment_index)
    }

    pub fn save_image_asset(
        &self,
        turn_id: &str,
        tool_id: Option<&str>,
        path: &Path,
        alt: &str,
    ) -> Result<ImageAsset> {
        const MAX_STORED_IMAGE_BYTES: u64 = 20 * 1024 * 1024;
        let metadata = std::fs::metadata(path)
            .with_context(|| format!("reading image metadata: {}", path.display()))?;
        if !metadata.is_file() {
            bail!("image path is not a file: {}", path.display());
        }
        if metadata.len() > MAX_STORED_IMAGE_BYTES {
            bail!("image exceeds the 20 MiB WebUI storage limit");
        }
        let bytes = std::fs::read(path)
            .with_context(|| format!("reading image for WebUI: {}", path.display()))?;
        let reader = image::ImageReader::new(Cursor::new(&bytes))
            .with_guessed_format()
            .context("detecting image format")?;
        let format = reader.format().context("unsupported image format")?;
        let (width, height) = reader
            .into_dimensions()
            .context("reading image dimensions")?;
        if width == 0
            || height == 0
            || width > 40_000
            || height > 40_000
            || u64::from(width) * u64::from(height) > 40_000_000
        {
            bail!("image dimensions are outside the WebUI safety limit");
        }
        let fallback_alt = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("image");
        let alt = if alt.trim().is_empty() {
            fallback_alt
        } else {
            alt.trim()
        }
        .chars()
        .filter(|character| !character.is_control())
        .take(500)
        .collect::<String>();
        let asset = ImageAsset {
            asset_id: format!(
                "img_{:032x}_{:016x}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|duration| duration.as_nanos())
                    .unwrap_or(0),
                rand::random::<u64>()
            ),
            turn_id: turn_id.to_string(),
            tool_id: tool_id.map(str::to_string),
            mime: format.to_mime_type().to_string(),
            width,
            height,
            alt,
            created_at: chrono::Utc::now().to_rfc3339(),
        };
        self.conv_db.insert_image_asset(&asset, &bytes)?;
        Ok(asset)
    }

    pub fn load_image_assets(&self) -> Result<Vec<ImageAsset>> {
        self.conv_db.load_image_assets(&self.session())
    }

    pub fn load_image_asset(&self, asset_id: &str) -> Result<Option<ImageAssetData>> {
        self.conv_db.load_image_asset(asset_id)
    }

    pub fn save_artifact_asset(
        &self,
        turn_id: &str,
        tool_id: Option<&str>,
        path: &Path,
        title: &str,
    ) -> Result<ArtifactAsset> {
        const MAX_ARTIFACT_BYTES: u64 = 20 * 1024 * 1024;
        let canonical = path
            .canonicalize()
            .with_context(|| format!("resolving artifact path: {}", path.display()))?;
        let metadata = std::fs::metadata(&canonical)
            .with_context(|| format!("reading artifact metadata: {}", canonical.display()))?;
        if !metadata.is_file() {
            bail!("artifact path is not a file: {}", canonical.display());
        }
        if metadata.len() == 0 || metadata.len() > MAX_ARTIFACT_BYTES {
            bail!("artifact must be between 1 byte and 20 MiB");
        }
        let bytes = std::fs::read(&canonical)
            .with_context(|| format!("reading artifact: {}", canonical.display()))?;
        let fallback_name = canonical
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("artifact");
        let requested_name = if title.trim().is_empty() {
            fallback_name
        } else {
            title.trim()
        };
        let file_name = requested_name
            .chars()
            .filter(|character| !character.is_control() && !matches!(character, '/' | '\\'))
            .take(180)
            .collect::<String>();
        let file_name = if file_name.trim().is_empty() {
            "artifact".to_string()
        } else {
            file_name
        };
        let (mime, kind) = artifact_media_type(&canonical);
        let source_key = canonical.to_string_lossy().to_string();
        let session_id = self.session();
        let managed_session_dir = self.artifacts_dir.join(session_id.as_ref());
        let identity_scope = if canonical.starts_with(&managed_session_dir) {
            session_id.as_ref()
        } else {
            turn_id
        };
        let hash = blake3::hash(format!("{identity_scope}\0{source_key}").as_bytes());
        let now = chrono::Utc::now().to_rfc3339();
        let asset = ArtifactAsset {
            asset_id: format!("art_{}", &hash.to_hex()[..32]),
            turn_id: turn_id.to_string(),
            tool_id: tool_id.map(str::to_string),
            source_key,
            file_name,
            mime: mime.to_string(),
            kind: kind.to_string(),
            size_bytes: bytes.len() as u64,
            created_at: now.clone(),
            updated_at: now,
        };
        self.conv_db.upsert_artifact_asset(&asset, &bytes)?;
        Ok(asset)
    }

    pub fn load_artifact_assets(&self) -> Result<Vec<ArtifactAsset>> {
        self.conv_db.load_artifact_assets(&self.session())
    }

    pub fn load_artifact_asset(&self, asset_id: &str) -> Result<Option<ArtifactAssetData>> {
        self.conv_db.load_artifact_asset(asset_id)
    }

    pub fn save_user_attachment(&self, attachment: &UserAttachment, data: &[u8]) -> Result<()> {
        self.conv_db
            .insert_user_attachment(&self.session(), attachment, data)
    }

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

    #[allow(clippy::too_many_arguments)]
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
        self.conv_db.load_merged_footprint(&self.session(), turn_ids)
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

    pub fn reconcile_managed_artifacts_for_turn(&self, session_id: &str, turn_id: &str) -> Result<()> {
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

    #[allow(clippy::too_many_arguments)]
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
            None => usage::usage_details(&self.usage_history_file(), limit, src, model, &|_, _| None),
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
