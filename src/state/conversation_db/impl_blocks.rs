//! impl_blocks — ConversationDb 会话/平台/插件实现（自 src/state/conversation_db.rs 拆分）。

use super::*;

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
        if crate::state::migrations::current_version(&conn)?
            < crate::state::migrations::LATEST_VERSION
        {
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
        crate::state::migrations::run_migrations(&mut conn)?;
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
                crate::state::migrations::DEFAULT_SESSION_ID,
                t("Terminal session", "终端集成会话"),
                now
            ],
        )?;
        conn.execute(
            "INSERT INTO app_state (key, value) VALUES ('current_session', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![crate::state::migrations::DEFAULT_SESSION_ID],
        )?;
        Ok(crate::state::migrations::DEFAULT_SESSION_ID.to_string())
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
                params![session_id, persona, crate::state::USER_SESSION_KIND],
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
