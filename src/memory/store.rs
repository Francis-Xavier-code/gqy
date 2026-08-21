//! store — MemoryStore 实现（原 store_impl）。

pub(crate) use super::*;

impl MemoryStore {
    pub fn new(config: &AppConfig, paths: &GQYPaths) -> Self {
        let data_dir = config.active_persona_memory_data_dir(paths).join("memory");
        let state_dir = config.active_persona_memory_state_dir(paths).join("memory");
        Self {
            config: config.memory_config().clone(),
            kb_config: config.plugins.knowledge_base.clone(),
            app_config: config.clone(),
            writes_enabled: true,
            access: MemoryAccess::Privileged,
            writer_principal: None,
            writer_display_name: String::new(),
            data_db: data_dir.join("memory.db"),
            state_db: state_dir.join("evicted_context.db"),
            skills_dir: config.active_persona_skills_dir(paths),
        }
    }

    pub(crate) fn set_writes_enabled(&mut self, enabled: bool) {
        self.writes_enabled = enabled;
    }

    pub(crate) fn set_request_context(
        &mut self,
        access: MemoryAccess,
        writer_principal: Option<String>,
        writer_display_name: impl Into<String>,
    ) {
        self.access = access;
        self.writer_principal = writer_principal.filter(|value| !value.trim().is_empty());
        self.writer_display_name = writer_display_name.into().trim().to_string();
    }

    pub(crate) fn request_context(&self) -> (MemoryAccess, Option<String>, String) {
        (
            self.access.clone(),
            self.writer_principal.clone(),
            self.writer_display_name.clone(),
        )
    }

    pub(crate) fn with_request_context(
        mut self,
        access: MemoryAccess,
        writer_principal: Option<String>,
        writer_display_name: impl Into<String>,
    ) -> Self {
        self.set_request_context(access, writer_principal, writer_display_name);
        self
    }

    pub fn automatic_ownership(&self, origin: &MemoryOrigin) -> MemoryOwnership {
        origin
            .principal_ownership()
            .unwrap_or_else(MemoryOwnership::privileged)
    }

    pub fn writer_ownership(&self) -> MemoryOwnership {
        self.writer_principal
            .as_ref()
            .map(|principal| {
                MemoryOwnership::principal(principal.clone(), self.writer_display_name.clone())
            })
            .unwrap_or_else(MemoryOwnership::privileged)
    }

    pub(crate) fn apply_evicted_ownership(&self, turns: &mut [EvictedTurn]) {
        let ownership = self.writer_ownership();
        for turn in turns {
            turn.visibility = ownership.visibility.to_string();
            turn.owner_principal.clone_from(&ownership.owner_principal);
            turn.owner_display_name
                .clone_from(&ownership.owner_display_name);
        }
    }

    pub fn manual_fact_ownership(&self) -> MemoryOwnership {
        match self.writer_principal.as_ref() {
            Some(principal) => {
                MemoryOwnership::principal(principal.clone(), self.writer_display_name.clone())
            }
            None => MemoryOwnership::privileged(),
        }
    }

    pub fn init(&self) -> Result<()> {
        if let Some(parent) = self.data_db.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if let Some(parent) = self.state_db.parent() {
            std::fs::create_dir_all(parent)?;
        }
        init_data_db(&self.data_conn()?)?;
        init_state_db(&self.state_conn()?)?;
        self.decay_memories()?;
        Ok(())
    }

    pub(crate) fn identity(&self) -> Result<(String, i64)> {
        if !self.data_db.is_file() {
            self.init()?;
        }
        Ok(self.data_conn_existing()?.query_row(
            "SELECT database_id, generation FROM memory_meta WHERE id=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?)
    }

    pub fn init_existing(&self) -> Result<()> {
        let conn = self.data_conn_existing()?;
        init_data_db(&conn)?;
        self.decay_memories_with_conn(&conn)
    }

    #[allow(dead_code)]
    pub fn remember_evicted_turns(&self, turns: &[EvictedTurn]) -> Result<()> {
        if !self.config.enabled
            || !self.writes_enabled
            || !self.config.evicted_context_enabled
            || turns.is_empty()
        {
            return Ok(());
        }
        self.init()?;
        let fallback = self.writer_ownership();
        let mut conn = self.state_conn()?;
        let tx = conn.transaction()?;
        for turn in turns {
            let visibility = if turn.visibility.trim().is_empty() {
                fallback.visibility
            } else {
                turn.visibility.as_str()
            };
            let owner_principal = if turn.owner_principal.trim().is_empty() {
                fallback.owner_principal.as_str()
            } else {
                turn.owner_principal.as_str()
            };
            let owner_display_name = if turn.owner_display_name.trim().is_empty() {
                fallback.owner_display_name.as_str()
            } else {
                turn.owner_display_name.as_str()
            };
            tx.execute(
                "INSERT OR IGNORE INTO evicted_turns (
                    source_id, timestamp, role, content, created_at,
                    visibility, owner_principal, owner_display_name
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    turn.source_id,
                    turn.timestamp,
                    turn.role,
                    turn.content,
                    now(),
                    visibility,
                    owner_principal,
                    owner_display_name,
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn prepare_evicted_context_db(&self) -> Result<Option<PathBuf>> {
        if !self.config.enabled || !self.writes_enabled || !self.config.evicted_context_enabled {
            return Ok(None);
        }
        self.init()?;
        Ok(Some(self.state_db.clone()))
    }

    pub fn clear_evicted_context(&self) -> Result<()> {
        self.init()?;
        self.state_conn()?
            .execute("DELETE FROM evicted_turns", [])?;
        Ok(())
    }

    pub fn clear_pending_events(&self) -> Result<()> {
        self.init()?;
        let data = self.data_conn()?;
        data.execute("DELETE FROM pending_events", [])?;
        data.execute(
            "DELETE FROM sqlite_sequence WHERE name = 'pending_events'",
            [],
        )?;
        Ok(())
    }

    #[allow(dead_code)]
    pub fn search_evicted_context(&self, query: &str, limit: usize) -> Result<Value> {
        self.init()?;
        self.search_evicted_context_existing(query, limit)
    }

    pub fn search_evicted_context_readonly(
        &self,
        query: &str,
        limit: usize,
        start: Option<&str>,
        end: Option<&str>,
    ) -> Result<Value> {
        if !self.state_db.is_file() {
            return Ok(json!({ "ok": true, "query": query, "results": [] }));
        }
        self.search_evicted_context_filtered(query, limit, start, end)
    }

    /// Keyword first, semantics only when the keywords came back weak — the
    /// same shape the knowledge base uses. Exact terms (error codes, package
    /// names) are what keyword matching is best at and what most of these
    /// lookups are; the embedding pass is for "what were we talking about",
    /// where the record says `[ERRO]` and the question says 报错.
    ///
    /// Every embedding step is best effort. The service being unreachable, or
    /// having produced no vectors yet, must never turn a working keyword search
    /// into a failure.
    pub async fn search_evicted_context_hybrid(
        &self,
        query: &str,
        limit: usize,
        start: Option<&str>,
        end: Option<&str>,
    ) -> Result<Value> {
        let mut base = self.search_evicted_context_readonly(query, limit, start, end)?;
        let strongest = base["results"]
            .as_array()
            .and_then(|hits| hits.first())
            .and_then(|hit| hit["score"].as_f64())
            .unwrap_or(0.0);
        if !self.semantic_enabled() || strongest >= SEMANTIC_SKIP_SCORE {
            return Ok(base);
        }
        let semantic = match self.semantic_evicted_hits(query, limit, start, end).await {
            Ok(hits) => hits,
            Err(error) => {
                tracing::debug!(error = %error, "evicted-context semantic pass unavailable");
                return Ok(base);
            }
        };
        if semantic.is_empty() {
            return Ok(base);
        }
        merge_evicted_hits(&mut base, semantic, limit);
        Ok(base)
    }

    /// Rows are embedded on demand rather than at eviction time: pop must not
    /// wait on a network round trip, and a record nobody ever searches for
    /// never costs an embedding. Each call tops up a bounded slice of the
    /// backlog, so coverage fills in over successive searches.
    pub async fn semantic_evicted_hits(
        &self,
        query: &str,
        limit: usize,
        start: Option<&str>,
        end: Option<&str>,
    ) -> Result<Vec<Value>> {
        let embedding = &self.app_config.embedding;
        let mut provider = self
            .config_provider(embedding.provider_id.trim())
            .context("embedding provider is not configured")?;
        let model = embedding.model.trim().to_string();
        provider.default_model = model.clone();

        let corpus = self.semantic_corpus(start, end)?;
        let missing: Vec<(i64, String)> = {
            let conn = self.state_conn()?;
            let mut pending = Vec::new();
            for (id, content) in &corpus {
                if pending.len() >= SEMANTIC_EMBED_BATCH {
                    break;
                }
                let known: Option<String> = conn
                    .query_row(
                        "SELECT model FROM evicted_embeddings WHERE id = ?1",
                        params![id],
                        |row| row.get(0),
                    )
                    .ok();
                if known.as_deref() != Some(model.as_str()) {
                    pending.push((*id, content.clone()));
                }
            }
            pending
        };
        for (id, content) in missing {
            let Ok(vector) = crate::tools::knowledge_base::embed_text(
                &self.app_config,
                &provider,
                &model,
                &content,
            )
            .await
            else {
                break;
            };
            let conn = self.state_conn()?;
            conn.execute(
                "INSERT INTO evicted_embeddings (id, model, embedding_json, created_at)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (id) DO UPDATE SET
                    model = excluded.model,
                    embedding_json = excluded.embedding_json,
                    created_at = excluded.created_at",
                params![id, model, serde_json::to_string(&vector)?, now()],
            )?;
        }

        let query_vector =
            crate::tools::knowledge_base::embed_text(&self.app_config, &provider, &model, query)
                .await?;
        let conn = self.state_conn()?;
        let mut hits = Vec::new();
        for (id, content) in &corpus {
            let stored: Option<String> = conn
                .query_row(
                    "SELECT embedding_json FROM evicted_embeddings WHERE id = ?1 AND model = ?2",
                    params![id, model],
                    |row| row.get(0),
                )
                .ok();
            let Some(stored) = stored else { continue };
            let Ok(vector) = serde_json::from_str::<Vec<f32>>(&stored) else {
                continue;
            };
            let score = cosine_similarity(&query_vector, &vector);
            if score < self.app_config.embedding.min_score {
                continue;
            }
            hits.push(json!({
                "id": id,
                "score": score * SEMANTIC_SCORE_WEIGHT,
                "semantic": true,
                "snippet": truncate_chars(&compact_line(content), 400),
            }));
        }
        sort_json_hits(&mut hits);
        hits.truncate(limit);
        Ok(hits)
    }

    pub fn config_provider(&self, id: &str) -> Option<crate::config::ProviderConfig> {
        if id.is_empty() {
            return None;
        }
        self.app_config.provider(Some(id)).ok().cloned()
    }

    /// Newest rows only, and bounded: this pass answers "what were we talking
    /// about", which is a recency question, and an unbounded corpus would make
    /// every miss pay for the whole archive.
    pub fn semantic_corpus(
        &self,
        start: Option<&str>,
        end: Option<&str>,
    ) -> Result<Vec<(i64, String)>> {
        let conn = self.state_conn()?;
        let mut clauses = Vec::new();
        let mut params: Vec<String> = Vec::new();
        if let Some(principal) = self.access.principal_key() {
            params.push(principal.to_string());
            clauses.push(format!(
                "(visibility='public' OR (visibility='principal' AND owner_principal=?{}))",
                params.len()
            ));
        }
        if let Some(start) = start {
            params.push(start.to_string());
            clauses.push(format!("timestamp >= ?{}", params.len()));
        }
        if let Some(end) = end {
            params.push(end.to_string());
            clauses.push(format!("timestamp <= ?{}", params.len()));
        }
        let where_clause = if clauses.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", clauses.join(" AND "))
        };
        let mut stmt = conn.prepare(&format!(
            "SELECT id, content FROM evicted_turns {where_clause}
              ORDER BY id DESC LIMIT {SEMANTIC_CORPUS_LIMIT}"
        ))?;
        let rows = stmt.query_map(rusqlite::params_from_iter(params.iter()), |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// No switch of its own: an embedding model being configured is what makes
    /// the semantic pass available, and the keyword path stands on its own when
    /// it is not.
    pub fn semantic_enabled(&self) -> bool {
        self.app_config.embedding.is_configured()
    }

    pub fn search_evicted_context_existing(&self, query: &str, limit: usize) -> Result<Value> {
        self.search_evicted_context_filtered(query, limit, None, None)
    }

    /// `start`/`end` are RFC 3339 bounds on the stored timestamp. "What were we
    /// talking about this morning" is a question about *when*, and time is a
    /// far stronger signal there than any keyword — the log says `[ERRO]` where
    /// the question says 报错.
    pub fn search_evicted_context_filtered(
        &self,
        query: &str,
        limit: usize,
        start: Option<&str>,
        end: Option<&str>,
    ) -> Result<Value> {
        let tokens = query_tokens(query);
        let conn = self.state_conn()?;
        let mut clauses = Vec::new();
        let mut params: Vec<String> = Vec::new();
        if let Some(principal) = self.access.principal_key() {
            params.push(principal.to_string());
            clauses.push(format!(
                "(visibility='public' OR (visibility='principal' AND owner_principal=?{}))",
                params.len()
            ));
        }
        if let Some(start) = start {
            params.push(start.to_string());
            clauses.push(format!("timestamp >= ?{}", params.len()));
        }
        if let Some(end) = end {
            params.push(end.to_string());
            clauses.push(format!("timestamp <= ?{}", params.len()));
        }
        // The trigram index does the filtering, so the scan no longer has to be
        // capped at the newest 1000 rows — those beyond it used to be stored
        // forever and reachable never.
        if !tokens.is_empty() {
            // Trigram index: terms shorter than three characters cannot be
            // matched by it, so those fall through to the scoring pass below
            // rather than narrowing the candidate set.
            let indexed: Vec<String> = tokens
                .iter()
                .filter(|token| token.chars().count() >= 3)
                .cloned()
                .collect();
            if !indexed.is_empty() {
                params.push(build_evicted_fts_query(&indexed));
                clauses.push(format!(
                    "id IN (SELECT rowid FROM evicted_turns_fts WHERE evicted_turns_fts MATCH ?{})",
                    params.len()
                ));
            }
        }
        let where_clause = if clauses.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", clauses.join(" AND "))
        };
        let mut stmt = conn.prepare(&format!(
            "SELECT id, timestamp, role, content, visibility,
                    owner_principal, owner_display_name
               FROM evicted_turns {where_clause}
              ORDER BY id DESC"
        ))?;
        let mut rows = stmt.query(rusqlite::params_from_iter(params.iter()))?;
        let normalized_query = compact_line(query).to_ascii_lowercase();
        let mut hits = Vec::new();
        while let Some(row) = rows.next()? {
            let id = row.get::<_, i64>(0)?;
            let timestamp = row.get::<_, String>(1)?;
            let role = row.get::<_, String>(2)?;
            let content = row.get::<_, String>(3)?;
            let visibility = row.get::<_, String>(4)?;
            let owner_principal = row.get::<_, String>(5)?;
            let owner_display_name = row.get::<_, String>(6)?;
            let score = score_text(&content, &normalized_query, &tokens);
            if score <= 0.0 {
                continue;
            }
            hits.push(json!({
                "id": id,
                "timestamp": timestamp,
                "role": role,
                "score": score,
                "visibility": visibility,
                "owner_principal": owner_principal,
                "owner_display_name": truncate_chars(&compact_line(&owner_display_name), 128),
                "snippet": snippet(&content, &tokens, self.kb_config.snippet_context_chars),
            }));
        }
        sort_json_hits(&mut hits);
        hits.truncate(limit.clamp(1, 50));
        Ok(json!({ "ok": true, "query": query, "results": hits }))
    }

    pub fn remember_fact(&self, content: &str, source: &str) -> Result<i64> {
        if !self.config.enabled || !self.writes_enabled || content.trim().is_empty() {
            return Ok(0);
        }
        self.init()?;
        let ownership = self.manual_fact_ownership();
        let subjects = ownership_subjects_json(&ownership);
        let conn = self.data_conn()?;
        conn.execute(
            "INSERT INTO facts (
                content, source, status, confidence, recall_count, created_at, updated_at,
                visibility, owner_principal, owner_display_name, subjects
             ) VALUES (?1, ?2, 'active', 1.0, 0, ?3, ?3, ?4, ?5, ?6, ?7)",
            params![
                content.trim(),
                source.trim(),
                now(),
                ownership.visibility,
                ownership.owner_principal,
                ownership.owner_display_name,
                subjects,
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn remember_pending_event(
        &self,
        user_message: &str,
        assistant_message: &str,
    ) -> Result<()> {
        if !self.config.enabled || !self.writes_enabled || !self.config.auto_diary_enabled {
            return Ok(());
        }
        self.init()?;
        self.data_conn()?.execute(
            "INSERT INTO pending_events (user_message, assistant_message, created_at) VALUES (?1, ?2, ?3)",
            params![user_message.trim(), assistant_message.trim(), now()],
        )?;
        Ok(())
    }

    pub fn process_after_turn(
        &self,
        user_message: &str,
        assistant_message: &str,
        origin: &MemoryOrigin,
        expected_database_id: &str,
        expected_generation: i64,
    ) -> Result<bool> {
        if !self.writes_enabled || !self.config.enabled || !self.config.auto_diary_enabled {
            return Ok(false);
        }
        if !self.data_db.is_file() {
            self.init()?;
        }
        let created_at = now();
        let expires_at = (Utc::now()
            + ChronoDuration::days(self.config.short_diary_retention_days as i64))
        .to_rfc3339();
        let content = diary_content(&created_at, user_message, assistant_message);
        let ownership = self.automatic_ownership(origin);
        let subjects = ownership_subjects_json(&ownership);
        let mut conn = self.data_conn_existing()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (current_database_id, current_generation) = tx.query_row(
            "SELECT database_id, generation FROM memory_meta WHERE id=1",
            [],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
        )?;
        if current_database_id != expected_database_id || current_generation != expected_generation
        {
            return Ok(false);
        }
        tx.execute(
            "INSERT INTO episodes (
                content, source, status, strength, recall_count, created_at, updated_at,
                retention, user_message, assistant_message, expires_at,
                origin_kind, origin_platform, origin_account_id, origin_conversation_kind,
                origin_conversation_id, origin_sender_id, origin_sender_display_name,
                origin_session_id, origin_message_id,
                visibility, owner_principal, owner_display_name, subjects
             ) VALUES (?1, 'episode', 'active', 1.0, 0, ?2, ?2, ?3, ?4, ?5, ?6,
                       ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)",
            params![
                content,
                created_at,
                SHORT_TERM,
                user_message.trim(),
                assistant_message.trim(),
                expires_at,
                origin.kind,
                origin.platform,
                origin.account_id,
                origin.conversation_kind,
                origin.conversation_id,
                origin.sender_id,
                origin.sender_display_name,
                origin.session_id,
                origin.message_id,
                ownership.visibility,
                ownership.owner_principal,
                ownership.owner_display_name,
                subjects,
            ],
        )?;
        tx.commit()?;
        self.cleanup_expired_short_diaries()?;
        Ok(true)
    }

    pub fn stats(&self) -> Result<Value> {
        self.init()?;
        self.prune_missing_skill_records()?;
        let data = self.data_conn()?;
        let state = self.state_conn()?;
        Ok(json!({
            "ok": true,
            "data_db": self.data_db.display().to_string(),
            "state_db": self.state_db.display().to_string(),
            "skills_dir": self.skills_dir.display().to_string(),
            "facts": count_rows(&data, "facts")?,
            "episodes": count_rows(&data, "episodes")?,
            "short_diaries": count_where(&data, "episodes", "retention='short_term'")?,
            "long_diaries": count_where(&data, "episodes", "retention='long_term'")?,
            "unconsolidated_diaries": count_where(&data, "episodes", "retention='short_term' AND consolidated_at IS NULL")?,
            "unprocessed_pending_events": count_where(&data, "pending_events", "processed_at IS NULL")?,
            "total_pending_events": count_rows(&data, "pending_events")?,
            "skill_records": count_rows(&data, "skill_records")?,
            "skill_dirs": count_skill_dirs(&self.skills_dir)?,
            "evicted_turns": count_rows(&state, "evicted_turns")?,
        }))
    }

    pub fn reset_all(&self, include_skills: bool) -> Result<()> {
        self.init()?;
        let mut data = self.data_conn()?;
        let tx = data.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "UPDATE memory_meta SET generation=generation+1 WHERE id=1",
            [],
        )?;
        tx.execute("DELETE FROM facts", [])?;
        tx.execute("DELETE FROM episodes", [])?;
        tx.execute("DELETE FROM pending_events", [])?;
        tx.execute("DELETE FROM skill_records", [])?;
        tx.execute("DELETE FROM memory_revisions", [])?;
        tx.execute(
            "DELETE FROM sqlite_sequence WHERE name IN ('facts', 'episodes', 'pending_events', 'skill_records', 'memory_revisions')",
            [],
        )?;
        tx.commit()?;
        self.clear_evicted_context()?;
        if include_skills {
            self.remove_auto_skills()?;
        }
        Ok(())
    }

    pub fn remove_auto_skills(&self) -> Result<()> {
        if !self.skills_dir.exists() {
            return Ok(());
        }
        for entry in std::fs::read_dir(&self.skills_dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let skill_file = entry.path().join("SKILL.md");
            let raw = std::fs::read_to_string(&skill_file).unwrap_or_default();
            if crate::skills::is_generated_skill(&raw) {
                std::fs::remove_dir_all(entry.path())?;
            }
        }
        Ok(())
    }

    pub fn flush_pending_events(&self) -> Result<()> {
        if !self.config.enabled || !self.config.auto_diary_enabled {
            return Ok(());
        }
        self.init()?;
        let conn = self.data_conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, user_message, assistant_message, created_at FROM pending_events WHERE processed_at IS NULL ORDER BY id LIMIT 20",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?;
        for row in rows {
            let (id, user, assistant, created_at) = row?;
            let content = diary_content(&created_at, &user, &assistant);
            let expires_at = (Utc::now()
                + ChronoDuration::days(self.config.short_diary_retention_days as i64))
            .to_rfc3339();
            conn.execute(
                "INSERT INTO episodes (
                    content, source, status, recall_count, created_at, updated_at,
                    retention, user_message, assistant_message, expires_at
                 ) VALUES (?1, 'episode', 'active', 0, ?2, ?2, ?3, ?4, ?5, ?6)",
                params![content, created_at, SHORT_TERM, user, assistant, expires_at],
            )?;
            conn.execute(
                "UPDATE pending_events SET processed_at=?1 WHERE id=?2",
                params![now(), id],
            )?;
        }
        Ok(())
    }

    pub(crate) fn next_organization_batch(&self) -> Result<Option<OrganizationBatch>> {
        if !self.config.enabled || !self.config.auto_diary_enabled {
            return Ok(None);
        }
        if !self.data_db.is_file() {
            return Ok(None);
        }
        self.init_existing()?;
        self.cleanup_expired_short_diaries()?;
        let conn = self.data_conn_existing()?;
        let (database_id, generation) = conn.query_row(
            "SELECT database_id, generation FROM memory_meta WHERE id=1",
            [],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
        )?;
        let forced = count_where(
            &conn,
            "episodes",
            "retention='short_term' AND promotion_pending=1",
        )?;
        let unconsolidated = count_where(
            &conn,
            "episodes",
            "retention='short_term' AND consolidated_at IS NULL",
        )?;
        if forced == 0 && unconsolidated < self.config.diary_batch_size as i64 {
            return Ok(None);
        }

        let (sql, limit) = if forced > 0 {
            (
                "SELECT id, created_at, user_message, assistant_message, 1,
                        origin_kind, origin_platform, origin_account_id,
                        origin_conversation_kind, origin_conversation_id, origin_sender_id,
                        origin_sender_display_name, origin_session_id, origin_message_id
                 FROM episodes
                 WHERE retention='short_term' AND promotion_pending=1
                 ORDER BY id LIMIT ?1",
                self.config.diary_batch_size.max(1),
            )
        } else {
            (
                "SELECT id, created_at, user_message, assistant_message, 0,
                        origin_kind, origin_platform, origin_account_id,
                        origin_conversation_kind, origin_conversation_id, origin_sender_id,
                        origin_sender_display_name, origin_session_id, origin_message_id
                 FROM episodes
                 WHERE retention='short_term' AND consolidated_at IS NULL
                 ORDER BY id LIMIT ?1",
                self.config.diary_batch_size,
            )
        };
        let mut stmt = conn.prepare(sql)?;
        let diaries = stmt
            .query_map([limit as i64], |row| {
                let origin = MemoryOrigin {
                    kind: row.get(5)?,
                    platform: row.get(6)?,
                    account_id: row.get(7)?,
                    conversation_kind: row.get(8)?,
                    conversation_id: row.get(9)?,
                    sender_id: row.get(10)?,
                    sender_display_name: row.get(11)?,
                    session_id: row.get(12)?,
                    message_id: row.get(13)?,
                };
                Ok(ShortDiaryRecord {
                    id: row.get(0)?,
                    created_at: row.get(1)?,
                    user_message: row.get(2)?,
                    assistant_message: row.get(3)?,
                    force_long_term: row.get::<_, i64>(4)? != 0,
                    owner_principal: origin
                        .principal_ownership()
                        .map(|ownership| ownership.owner_principal),
                    origin,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if diaries.is_empty() {
            return Ok(None);
        }
        let existing = load_existing_memory_candidates(&conn, &diaries)?;
        Ok(Some(OrganizationBatch {
            database_id,
            generation,
            diaries,
            existing,
        }))
    }

    pub(crate) fn apply_organized_batch(
        &self,
        batch: &OrganizationBatch,
        output: OrganizedOutput,
    ) -> Result<()> {
        if !self.data_db.is_file() {
            bail!("memory database moved or removed while organization was running");
        }
        if output.knowledge.len() + output.long_diaries.len() > MAX_ORGANIZED_ITEMS {
            bail!("memory organizer returned too many items");
        }
        let diary_ids = batch
            .diaries
            .iter()
            .map(|diary| diary.id)
            .collect::<BTreeSet<_>>();
        let forced_ids = batch
            .diaries
            .iter()
            .filter(|diary| diary.force_long_term)
            .map(|diary| diary.id)
            .collect::<BTreeSet<_>>();
        let candidate_fact_ids = batch
            .existing
            .iter()
            .filter(|memory| memory.kind == "knowledge")
            .map(|memory| memory.id)
            .collect::<BTreeSet<_>>();
        let candidate_facts = batch
            .existing
            .iter()
            .filter(|memory| memory.kind == "knowledge")
            .map(|memory| (memory.id, memory))
            .collect::<BTreeMap<_, _>>();
        for action in &output.knowledge {
            validate_knowledge_action(action, &diary_ids, &candidate_fact_ids)?;
            validate_knowledge_visibility(batch, action)?;
            validate_knowledge_update_scope(batch, action, &candidate_facts)?;
        }
        let mut promoted_ids = BTreeSet::new();
        for diary in &output.long_diaries {
            validate_long_diary(batch, diary, &diary_ids)?;
            promoted_ids.extend(diary.diary_ids.iter().copied());
        }
        if !forced_ids.is_subset(&promoted_ids) {
            bail!("memory organizer did not promote every required diary");
        }

        let mut conn = self.data_conn_existing()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (current_database_id, current_generation) = tx.query_row(
            "SELECT database_id, generation FROM memory_meta WHERE id=1",
            [],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
        )?;
        if current_database_id != batch.database_id || current_generation != batch.generation {
            bail!("memory database was moved, replaced, or reset while organization was running");
        }
        let timestamp = now();
        if self.config.auto_fact_enabled {
            for action in output.knowledge {
                let source_ids = normalized_ids_json(&action.diary_ids);
                let tags = normalized_tags_json(&action.tags);
                let ownership = knowledge_ownership(batch, &action);
                let subjects =
                    organized_subjects_json(batch, &action.diary_ids, &action.subjects, &ownership);
                match action.operation.as_str() {
                    "create" => {
                        tx.execute(
                            "INSERT INTO facts (
                                content, source, status, confidence, strength, recall_count,
                                created_at, updated_at, memory_type, truth_status, importance,
                                tags, source_episode_ids,
                                visibility, owner_principal, owner_display_name, subjects
                             ) SELECT ?1, 'diary-organizer', 'active', ?2, 1.0, 0,
                                      ?3, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12
                               WHERE NOT EXISTS (
                                    SELECT 1 FROM facts
                                     WHERE content=?1 AND truth_status!='rejected'
                                       AND visibility=?9 AND owner_principal=?10
                                )",
                            params![
                                action.content.trim(),
                                action.confidence,
                                timestamp,
                                action.memory_type,
                                action.truth_status,
                                action.importance,
                                tags,
                                source_ids,
                                ownership.visibility,
                                ownership.owner_principal,
                                ownership.owner_display_name,
                                subjects,
                            ],
                        )?;
                    }
                    "update" => {
                        let target = action
                            .target_id
                            .context("missing knowledge update target")?;
                        let old_content = tx.query_row(
                            "SELECT content FROM facts WHERE id=?1",
                            [target],
                            |row| row.get::<_, String>(0),
                        )?;
                        tx.execute(
                            "INSERT INTO memory_revisions (
                                memory_id, old_content, new_content, source_episode_ids, created_at
                             ) VALUES (?1, ?2, ?3, ?4, ?5)",
                            params![
                                target,
                                old_content,
                                action.content.trim(),
                                source_ids,
                                timestamp
                            ],
                        )?;
                        tx.execute(
                            "UPDATE facts SET content=?1, source='diary-organizer', status='active',
                                confidence=?2, strength=1.0, updated_at=?3, memory_type=?4,
                                truth_status=?5, importance=?6, tags=?7, source_episode_ids=?8,
                                visibility=?9, owner_principal=?10, owner_display_name=?11,
                                subjects=?12
                              WHERE id=?13",
                            params![
                                action.content.trim(),
                                action.confidence,
                                timestamp,
                                action.memory_type,
                                action.truth_status,
                                action.importance,
                                tags,
                                source_ids,
                                ownership.visibility,
                                ownership.owner_principal,
                                ownership.owner_display_name,
                                subjects,
                                target,
                            ],
                        )?;
                    }
                    _ => unreachable!("validated operation"),
                }
            }
        }

        for diary in output.long_diaries {
            let source_ids = normalized_ids_json(&diary.diary_ids);
            let tags = normalized_tags_json(&diary.tags);
            let source_key = format!(
                "{}:{}",
                diary
                    .diary_ids
                    .iter()
                    .copied()
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .map(|id| id.to_string())
                    .collect::<Vec<_>>()
                    .join(","),
                blake3::hash(diary.content.trim().as_bytes()).to_hex()
            );
            let ownership = diary_ownership(batch, &diary.diary_ids);
            let subjects =
                organized_subjects_json(batch, &diary.diary_ids, &diary.subjects, &ownership);
            tx.execute(
                "INSERT OR IGNORE INTO episodes (
                    content, source, status, strength, recall_count, created_at, updated_at,
                    retention, consolidated_at, importance, confidence, tags,
                    source_episode_ids, source_key,
                    visibility, owner_principal, owner_display_name, subjects
                 ) VALUES (?1, 'diary-organizer', 'active', 1.0, 0, ?2, ?2,
                           ?3, ?2, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![
                    diary.content.trim(),
                    timestamp,
                    LONG_TERM,
                    diary.importance,
                    diary.confidence,
                    tags,
                    source_ids,
                    source_key,
                    ownership.visibility,
                    ownership.owner_principal,
                    ownership.owner_display_name,
                    subjects,
                ],
            )?;
        }

        for diary in &batch.diaries {
            tx.execute(
                "UPDATE episodes SET consolidated_at=COALESCE(consolidated_at, ?1),
                    promotion_pending=CASE WHEN ?2 THEN 0 ELSE promotion_pending END,
                    promoted_at=CASE WHEN ?2 THEN COALESCE(promoted_at, ?1) ELSE promoted_at END
                 WHERE id=?3 AND retention='short_term'",
                params![timestamp, promoted_ids.contains(&diary.id), diary.id],
            )?;
        }
        tx.commit()?;
        self.cleanup_expired_short_diaries()?;
        Ok(())
    }

    pub fn cleanup_expired_short_diaries(&self) -> Result<usize> {
        if !self.data_db.is_file() {
            return Ok(0);
        }
        let conn = self.data_conn_existing()?;
        conn.execute(
            "UPDATE episodes SET status='forgotten'
             WHERE retention='short_term'
               AND consolidated_at IS NULL
               AND promotion_pending=0
               AND expires_at IS NOT NULL
               AND unixepoch(expires_at) IS NOT NULL
               AND unixepoch(expires_at) <= unixepoch('now')",
            [],
        )?;
        Ok(conn.execute(
            "DELETE FROM episodes
             WHERE retention='short_term'
               AND consolidated_at IS NOT NULL
               AND promotion_pending=0
               AND expires_at IS NOT NULL
               AND unixepoch(expires_at) IS NOT NULL
               AND unixepoch(expires_at) <= unixepoch('now')",
            [],
        )?)
    }

    pub fn prune_missing_skill_records(&self) -> Result<()> {
        let conn = self.data_conn()?;
        let mut stmt = conn.prepare("SELECT id, path FROM skill_records")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut missing = Vec::new();
        for row in rows {
            let (id, path) = row?;
            if !PathBuf::from(path).exists() {
                missing.push(id);
            }
        }
        drop(stmt);
        for id in missing {
            conn.execute("DELETE FROM skill_records WHERE id=?1", params![id])?;
        }
        Ok(())
    }

    pub fn recall_memories(
        &self,
        query: &str,
        limit: usize,
        include_forgotten: bool,
    ) -> Result<Value> {
        self.init()?;
        self.recall_memories_existing(query, limit, include_forgotten)
    }

    pub fn recall_memories_readonly(
        &self,
        query: &str,
        limit: usize,
        include_forgotten: bool,
    ) -> Result<Value> {
        if !self.data_db.is_file() {
            return Ok(json!({ "ok": true, "query": query, "facts": [], "episodes": [] }));
        }
        self.recall_memories_existing(query, limit, include_forgotten)
    }

    pub fn recall_memories_existing(
        &self,
        query: &str,
        limit: usize,
        include_forgotten: bool,
    ) -> Result<Value> {
        let conn = self.data_conn()?;
        let facts = self.search_facts(&conn, query, limit, include_forgotten)?;
        let episodes = self.search_episodes(&conn, query, limit, include_forgotten)?;
        Ok(json!({
            "ok": true,
            "query": query,
            "facts": facts.iter().map(memory_hit_json).collect::<Vec<_>>(),
            "episodes": episodes.iter().map(memory_hit_json).collect::<Vec<_>>(),
        }))
    }

    #[allow(dead_code)]
    pub fn recall_past_events(&self, query: &str, limit: usize) -> Result<Value> {
        self.init()?;
        self.recall_past_events_existing(query, limit)
    }

    pub fn recall_past_events_readonly(&self, query: &str, limit: usize) -> Result<Value> {
        if !self.data_db.is_file() {
            return Ok(json!({ "ok": true, "query": query, "episodes": [] }));
        }
        self.recall_past_events_existing(query, limit)
    }

    pub fn recall_past_events_existing(&self, query: &str, limit: usize) -> Result<Value> {
        let conn = self.data_conn()?;
        let episodes = self.search_episodes(&conn, query, limit, true)?;
        Ok(json!({
            "ok": true,
            "query": query,
            "episodes": episodes.iter().map(memory_hit_json).collect::<Vec<_>>(),
        }))
    }

    pub fn association(
        &self,
        query: &str,
        exclude: Option<&AssociationExclusion>,
    ) -> Result<Option<AssociationContext>> {
        if !self.config.enabled || !self.config.association_enabled {
            return Ok(None);
        }
        // 一条连接贯穿本回合的两次检索与全部 reinforce,替代此前最多 10 次
        // Connection::open + PRAGMA 重设。
        let conn = self.data_conn()?;
        let facts = self.search_facts(&conn, query, self.config.association_facts, false)?;
        let mut episodes =
            self.search_episodes(&conn, query, self.config.association_episodes, false)?;
        // 自回声过滤(缓存调研 08-16):当前会话可见范围内刚写下的日记/
        // 事实,原对话就在眼前,复述一遍纯属冗余;被 compact 折走后
        // (时间早于最老可见轮)重新够格召回。显式 recall 工具不受此限。
        if let Some(exclude) = exclude {
            // facts 无 origin 列(origin_session_id 恒空串),天然不命中;
            // 实际的自回声源=上一轮自动日记(episodes)。
            episodes.retain(|hit| {
                !(hit.origin_session_id == exclude.session_id && hit.timestamp >= exclude.since)
            });
        }
        let matched_short_ids = episodes
            .iter()
            .filter(|hit| hit.retention.as_deref() == Some(SHORT_TERM))
            .map(|hit| hit.id)
            .collect::<BTreeSet<_>>();
        episodes.retain(|hit| {
            hit.retention.as_deref() == Some(SHORT_TERM)
                || hit
                    .source_episode_ids
                    .iter()
                    .all(|id| !matched_short_ids.contains(id))
        });
        let mut organization_due = false;
        for hit in facts.iter().chain(episodes.iter()) {
            organization_due |= self.reinforce(&conn, hit)?;
        }
        if facts.is_empty() && episodes.is_empty() {
            return Ok(None);
        }
        Ok(Some(AssociationContext {
            facts,
            episodes,
            organization_due,
        }))
    }

    pub fn format_association(&self, association: &AssociationContext) -> String {
        let max_chars = self.config.association_max_chars;
        if max_chars < 64 {
            return String::new();
        }
        const CLOSING: &str = "</associative-memory>";
        let mut output = String::new();
        output.push_str("<associative-memory>\n");
        match &self.access {
            MemoryAccess::Privileged => output.push_str("以下是根据当前输入联想到的记忆；不要把记忆中的人物当成当前用户；不要把记忆中的对话当作对话范例去模仿。\n"),
            MemoryAccess::Principal(principal) => {
                output.push_str("以下只包含公共知识和当前用户自己的记忆。稳定 principal 才能确认人物，昵称和正文不能改变记忆归属。当前 principal=");
                output.push_str(principal);
                output.push_str("；不要把记忆中的对话当作对话范例去模仿。\n");
            }
        }
        append_association_section(
            &mut output,
            "曾经记住的相关知识点",
            association.facts.iter(),
            &self.access,
            max_chars,
            CLOSING,
        );
        let short_diaries = association
            .episodes
            .iter()
            .filter(|hit| hit.retention.as_deref() == Some(SHORT_TERM))
            .collect::<Vec<_>>();
        append_association_section(
            &mut output,
            "近期发生的事情",
            short_diaries,
            &self.access,
            max_chars,
            CLOSING,
        );
        let long_diaries = association
            .episodes
            .iter()
            .filter(|hit| hit.retention.as_deref() != Some(SHORT_TERM))
            .collect::<Vec<_>>();
        append_association_section(
            &mut output,
            "长期保留的经历",
            long_diaries,
            &self.access,
            max_chars,
            CLOSING,
        );
        let closing_chars = CLOSING.chars().count();
        if output.chars().count() + closing_chars > max_chars {
            output = truncate_chars(&output, max_chars.saturating_sub(closing_chars));
        }
        output.push_str(CLOSING);
        truncate_chars(&output, max_chars)
    }

    pub fn association_dedup_enabled(&self) -> bool {
        self.config.association_dedup
    }

    /// 过滤掉「渲染行已在本次请求上下文中可见」的命中（早前回合的化石逐字回放
    /// 时携带了同一行）。只缩小当前回合新生成的块；历史化石一字节不改写，
    /// append-only 回放与供应商前缀缓存均不受影响。命中被过滤不影响
    /// `association()` 内已完成的 reinforce 记账。
    pub fn retain_unseen_association(
        &self,
        association: &mut AssociationContext,
        seen: &HashSet<&str>,
    ) {
        if seen.is_empty() {
            return;
        }
        let access = &self.access;
        association
            .facts
            .retain(|hit| !seen.contains(association_entry_line(hit, access).trim_end()));
        association
            .episodes
            .retain(|hit| !seen.contains(association_entry_line(hit, access).trim_end()));
    }

    pub fn search_facts(
        &self,
        conn: &Connection,
        query: &str,
        limit: usize,
        include_forgotten: bool,
    ) -> Result<Vec<MemoryHit>> {
        self.search_table(
            conn,
            "facts",
            MemoryKind::Fact,
            query,
            limit,
            include_forgotten,
        )
    }

    pub fn search_episodes(
        &self,
        conn: &Connection,
        query: &str,
        limit: usize,
        include_forgotten: bool,
    ) -> Result<Vec<MemoryHit>> {
        self.search_table(
            conn,
            "episodes",
            MemoryKind::Diary,
            query,
            limit,
            include_forgotten,
        )
    }

    pub fn search_table(
        &self,
        conn: &Connection,
        table: &str,
        kind: MemoryKind,
        query: &str,
        limit: usize,
        include_forgotten: bool,
    ) -> Result<Vec<MemoryHit>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let tokens = query_tokens(query);
        // 归一化与行无关,提到 5000 行循环外做一次。
        let normalized_query = compact_line(query).to_ascii_lowercase();
        let status_filter = if kind == MemoryKind::Fact && include_forgotten {
            "WHERE truth_status!='rejected'"
        } else if kind == MemoryKind::Fact {
            "WHERE status!='forgotten' AND truth_status!='rejected'"
        } else if include_forgotten {
            ""
        } else {
            "WHERE status!='forgotten'"
        };
        let access_filter = if self.access.principal_key().is_some() && status_filter.is_empty() {
            "WHERE visibility='public' OR (visibility='principal' AND owner_principal=?1)"
        } else if self.access.principal_key().is_some() {
            " AND (visibility='public' OR (visibility='principal' AND owner_principal=?1))"
        } else {
            ""
        };
        let sql = format!(
            "SELECT id, content, source, status, created_at, strength,
                     COALESCE(importance, 3), {}, COALESCE(source_episode_ids, '[]'),
                     visibility, owner_principal, owner_display_name, subjects,
                     {}
             FROM {table} {}{} ORDER BY updated_at DESC LIMIT 5000",
            if kind == MemoryKind::Diary {
                "retention"
            } else {
                "NULL"
            },
            // 自回声排除只针对自动日记;facts 表没有 origin 列。
            if kind == MemoryKind::Diary {
                "COALESCE(origin_session_id, '')"
            } else {
                "''"
            },
            status_filter,
            access_filter,
        );
        let mut stmt = conn.prepare(&sql)?;
        let mut rows = match self.access.principal_key() {
            Some(principal) => stmt.query([principal])?,
            None => stmt.query([])?,
        };
        let mut hits = Vec::new();
        while let Some(row) = rows.next()? {
            let id = row.get::<_, i64>(0)?;
            let content = row.get::<_, String>(1)?;
            let source = row.get::<_, String>(2)?;
            let status = row.get::<_, String>(3)?;
            let timestamp = row.get::<_, String>(4)?;
            let strength = row.get::<_, f64>(5)?;
            let importance = row.get::<_, i64>(6)?;
            let retention = row.get::<_, Option<String>>(7)?;
            let source_episode_ids = row.get::<_, String>(8)?;
            let visibility = row.get::<_, String>(9)?;
            let owner_principal = row.get::<_, String>(10)?;
            let owner_display_name = row.get::<_, String>(11)?;
            let subjects = row.get::<_, String>(12)?;
            let origin_session_id = row.get::<_, String>(13)?;
            if !include_forgotten && status == "forgotten" {
                continue;
            }
            let lexical_score = score_text(&content, &normalized_query, &tokens);
            if lexical_score <= 0.0 {
                continue;
            }
            let score = lexical_score
                + strength.clamp(0.0, 1.0) as f32 * 5.0
                + importance.clamp(1, 5) as f32;
            hits.push(MemoryHit {
                id,
                origin_session_id,
                kind,
                content,
                score,
                timestamp,
                source,
                retention,
                visibility,
                owner_principal,
                owner_display_name,
                subjects,
                source_episode_ids: serde_json::from_str(&source_episode_ids).unwrap_or_default(),
            });
        }
        hits.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        hits.truncate(limit.min(50));
        Ok(hits)
    }

    pub fn reinforce(&self, conn: &Connection, hit: &MemoryHit) -> Result<bool> {
        let timestamp = now();
        if hit.kind == MemoryKind::Fact {
            conn.execute(
                "UPDATE facts SET recall_count=recall_count+1,
                    strength=MIN(1.0, strength+?1), last_recalled_at=?2,
                    updated_at=?2, status='active' WHERE id=?3",
                params![self.config.forgetting_review_boost, timestamp, hit.id],
            )?;
            return Ok(false);
        }

        let refreshed_expiry = (Utc::now()
            + ChronoDuration::days(self.config.short_diary_retention_days as i64))
        .to_rfc3339();
        conn.execute(
            "UPDATE episodes SET
                recall_count=recall_count+1,
                strength=MIN(1.0, strength+?1),
                last_recalled_at=?2,
                updated_at=?2,
                status='active',
                expires_at=CASE
                    WHEN retention='short_term' AND promoted_at IS NULL THEN ?3
                    ELSE expires_at END,
                promotion_pending=CASE
                    WHEN retention='short_term' AND promoted_at IS NULL
                         AND recall_count+1>=?4 THEN 1
                    ELSE promotion_pending END
             WHERE id=?5",
            params![
                self.config.forgetting_review_boost,
                timestamp,
                refreshed_expiry,
                self.config.diary_promotion_recalls as i64,
                hit.id
            ],
        )?;
        Ok(conn.query_row(
            "SELECT retention='short_term' AND promotion_pending=1
             FROM episodes WHERE id=?1",
            [hit.id],
            |row| row.get::<_, bool>(0),
        )?)
    }

    pub fn decay_memories(&self) -> Result<()> {
        if !self.config.enabled || !self.config.forgetting_enabled {
            return Ok(());
        }
        let conn = self.data_conn()?;
        self.decay_memories_with_conn(&conn)
    }

    pub fn decay_memories_with_conn(&self, conn: &Connection) -> Result<()> {
        if !self.config.enabled || !self.config.forgetting_enabled {
            return Ok(());
        }
        decay_table(conn, "facts", &self.config)?;
        decay_table(conn, "episodes", &self.config)?;
        Ok(())
    }

    pub fn data_conn(&self) -> Result<Connection> {
        let conn = Connection::open(&self.data_db)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")?;
        Ok(conn)
    }

    pub fn data_conn_existing(&self) -> Result<Connection> {
        let conn = Connection::open_with_flags(
            &self.data_db,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")?;
        Ok(conn)
    }

    pub fn state_conn(&self) -> Result<Connection> {
        let conn = Connection::open(&self.state_db)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")?;
        Ok(conn)
    }
}
