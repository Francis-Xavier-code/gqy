//! store — 自 src/tools/knowledge_base.rs 拆分。

use super::*;

impl KnowledgeBase {
    pub(crate) fn remove_prefix(&self, prefix: &str) -> Result<()> {
        let conn = self.meta_conn()?;
        let mut stmt = conn.prepare("SELECT name FROM files WHERE name LIKE ?1")?;
        let names = stmt
            .query_map(params![format!("{prefix}%")], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for name in names {
            let path = self.safe_file_path(&name)?;
            if path.exists() {
                std::fs::remove_file(path)?;
            }
            conn.execute("DELETE FROM files WHERE name=?1", params![name])?;
            self.semantic_conn()?.execute(
                "DELETE FROM semantic_chunks WHERE file_name=?1",
                params![name],
            )?;
        }
        Ok(())
    }

    pub async fn reindex_embeddings(&self, quiet: bool) -> Result<usize> {
        self.init()?;
        if !self.config.plugins.knowledge_base.embedding_enabled {
            if !quiet {
                println!("embedding is disabled");
            }
            return Ok(0);
        }
        let Some((provider, model)) = self.embedding_provider()? else {
            if !quiet {
                println!("embedding provider/model is not configured; skipped");
            }
            return Ok(0);
        };
        let lock_path = self.root.join("embedding.lock");
        let lock = match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock_path)
        {
            Ok(lock) => lock,
            Err(_) => {
                if !quiet {
                    println!(
                        "embedding reindex already running; lock file: {}",
                        lock_path.display()
                    );
                    println!(
                        "if no gqy reindex process is running, remove the stale lock file and retry"
                    );
                }
                return Ok(0);
            }
        };
        drop(lock);
        let result = self
            .reindex_embeddings_inner(&provider, &model, quiet)
            .await;
        let _ = std::fs::remove_file(lock_path);
        result
    }

    pub fn stats(&self) -> Result<Value> {
        self.init()?;
        let files = self.list()?;
        let semantic = self.semantic_conn()?;
        let chunks: i64 =
            semantic.query_row("SELECT COUNT(*) FROM semantic_chunks", [], |row| row.get(0))?;
        Ok(json!({
            "ok": true,
            "root": self.root.display().to_string(),
            "files_dir": self.files_dir.display().to_string(),
            "files": files.len(),
            "total_size_kb": (files.iter().map(|file| file.size_bytes).sum::<i64>() as f64 / 1024.0 * 10.0).round() / 10.0,
            "semantic_chunks": chunks,
            "embedding_enabled": self.config.plugins.knowledge_base.embedding_enabled,
            "embedding_provider_id": self.config.plugins.knowledge_base.embedding_provider_id,
            "embedding_model": self.config.plugins.knowledge_base.embedding_model,
        }))
    }

    pub(crate) fn import_file(&self, source: &Path, name: &str) -> Result<String> {
        let bytes = std::fs::read(source)?;
        self.validate_file(name, &bytes)?;
        let dest = self.safe_file_path(name)?;
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&dest, &bytes)?;
        let hash = sha256_hex(&bytes);
        let mtime = unix_time(std::fs::metadata(&dest)?.modified()?);
        let conn = self.meta_conn()?;
        init_meta_db(&conn)?;
        conn.execute(
            "INSERT INTO files (name, path, size_bytes, mtime, content_sha256, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6) ON CONFLICT(name) DO UPDATE SET path=excluded.path, size_bytes=excluded.size_bytes, mtime=excluded.mtime, content_sha256=excluded.content_sha256, updated_at=excluded.updated_at",
            params![name, dest.display().to_string(), bytes.len() as i64, mtime, hash, now_secs()],
        )?;
        Ok(name.to_string())
    }

    pub(crate) fn refresh_semantic_after_write(&self, name: &str) -> Result<bool> {
        if !self.config.plugins.knowledge_base.embedding_enabled {
            return Ok(false);
        }
        self.semantic_conn()?.execute(
            "DELETE FROM semantic_chunks WHERE file_name=?1",
            params![name],
        )?;
        self.spawn_embedding_reindex()?;
        Ok(true)
    }

    pub(crate) fn keyword_search(&self, query: &str, limit: usize) -> Result<Vec<SearchResult>> {
        let tokens = query_tokens(query);
        let phrase = query.to_ascii_lowercase();
        let mut results = Vec::new();
        for record in self.list()? {
            let path = PathBuf::from(&record.path);
            let Ok(content) = std::fs::read_to_string(&path) else {
                continue;
            };
            let content_lower = content.to_ascii_lowercase();
            let name_lower = record.name.to_ascii_lowercase();
            let mut score = 0.0;
            let mut positions_by_token: HashMap<String, Vec<usize>> = HashMap::new();
            let mut matched = HashSet::new();
            if phrase.len() > 1 && content_lower.contains(&phrase) {
                score += 90.0;
                matched.insert(phrase.clone());
            }
            if phrase.len() > 1 && name_lower.contains(&phrase) {
                score += 140.0;
            }
            for token in &tokens {
                let positions = find_positions(&content_lower, token, 100);
                if !positions.is_empty() {
                    score += 20.0 + positions.len().min(10) as f32 * 2.0;
                    matched.insert(token.clone());
                    positions_by_token.insert(token.clone(), positions);
                }
                if name_lower.contains(token) {
                    score += 45.0;
                    matched.insert(token.clone());
                }
            }
            if !tokens.is_empty() {
                score += (matched.len() as f32 / tokens.len() as f32) * 55.0;
            }
            if let Some((start, end, coverage)) = best_window(
                &positions_by_token,
                &tokens,
                self.config.plugins.knowledge_base.proximity_window_chars,
            ) {
                score += coverage * 120.0;
                let snippet = snippet_chars(
                    &content,
                    start,
                    end,
                    self.config.plugins.knowledge_base.snippet_context_chars,
                );
                results.push(SearchResult::new(
                    record.name,
                    score,
                    vec![snippet],
                    "keyword",
                ));
                continue;
            }
            if score > 0.0 {
                let snippets = extract_snippets(
                    &content,
                    &content_lower,
                    &tokens,
                    self.config.plugins.knowledge_base.snippet_context_chars,
                );
                results.push(SearchResult::new(record.name, score, snippets, "keyword"));
            }
        }
        results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        results.truncate(limit);
        Ok(results)
    }

    pub(crate) async fn semantic_search(&self, query: &str) -> Result<Vec<SearchResult>> {
        let Some((provider, model)) = self.embedding_provider()? else {
            return Ok(Vec::new());
        };
        let query_embedding = embed_text(&self.config, &provider, &model, query).await?;
        let semantic = self.semantic_conn()?;
        let mut stmt = semantic.prepare(
            "SELECT file_name, start_char, end_char, text, embedding_json FROM semantic_chunks",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, usize>(1)?,
                row.get::<_, usize>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?;
        let mut results = Vec::new();
        for row in rows {
            let (file_name, _start, _end, text, embedding_json) = row?;
            let Ok(embedding) = serde_json::from_str::<Vec<f32>>(&embedding_json) else {
                continue;
            };
            let score = cosine(&query_embedding, &embedding);
            if score < self.config.embedding.min_score {
                continue;
            }
            results.push(SearchResult::new(
                file_name,
                score * 200.0,
                vec![compact_whitespace(&text)],
                "semantic",
            ));
        }
        results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        results.truncate(self.config.plugins.knowledge_base.semantic_top_k);
        Ok(results)
    }

    pub(crate) async fn reindex_embeddings_inner(
        &self,
        provider: &ProviderConfig,
        model: &str,
        quiet: bool,
    ) -> Result<usize> {
        let files = self.list()?;
        let semantic = self.semantic_conn()?;
        init_semantic_db(&semantic)?;
        let mut indexed = 0usize;
        for record in files {
            let content = match std::fs::read_to_string(&record.path) {
                Ok(content) => content,
                Err(_) => continue,
            };
            let chunks = build_chunks(
                &content,
                self.config.plugins.knowledge_base.semantic_chunk_chars,
                self.config.plugins.knowledge_base.semantic_chunk_overlap,
            );
            semantic.execute(
                "DELETE FROM semantic_chunks WHERE file_name=?1",
                params![record.name],
            )?;
            for chunk in chunks {
                let embedding = match embed_text(&self.config, provider, model, &chunk.text).await {
                    Ok(value) => value,
                    Err(err) => {
                        if !quiet {
                            eprintln!(
                                "embedding failed for {} chunk {}: {err}",
                                record.name, chunk.index
                            );
                        }
                        continue;
                    }
                };
                semantic.execute(
                    "INSERT INTO semantic_chunks (provider_id, model, file_name, content_sha256, chunk_index, start_char, end_char, text, embedding_json, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                    params![provider.id, model, record.name, record.content_sha256, chunk.index as i64, chunk.start as i64, chunk.end as i64, chunk.text, serde_json::to_string(&embedding)?, now_secs()],
                )?;
                indexed += 1;
            }
        }
        if !quiet {
            println!("indexed semantic chunks: {indexed}");
        }
        Ok(indexed)
    }

    pub(crate) fn spawn_embedding_reindex(&self) -> Result<()> {
        if !self.config.plugins.knowledge_base.embedding_enabled {
            return Ok(());
        }
        if self
            .config
            .plugins
            .knowledge_base
            .embedding_provider_id
            .trim()
            .is_empty()
            || self
                .config
                .plugins
                .knowledge_base
                .embedding_model
                .trim()
                .is_empty()
        {
            return Ok(());
        }
        let exe = std::env::current_exe()?;
        Command::new(exe)
            .args(["kb", "embed", "reindex", "--quiet"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        Ok(())
    }

    pub(crate) fn validate_file(&self, name: &str, bytes: &[u8]) -> Result<()> {
        if bytes.is_empty() {
            bail!("file is empty")
        }
        if bytes.len() > self.config.plugins.knowledge_base.max_file_size_kb * 1024 {
            bail!("file too large: {} bytes", bytes.len())
        }
        std::str::from_utf8(bytes).context("file is not valid UTF-8 text")?;
        let file_name = file_name(name).to_ascii_lowercase();
        let ext = Path::new(&file_name)
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| format!(".{ext}"));
        let allowed_ext = split_csv(&self.config.plugins.knowledge_base.allowed_extensions);
        let allowed_names = split_csv(&self.config.plugins.knowledge_base.allowed_filenames);
        if ext.as_ref().is_some_and(|ext| allowed_ext.contains(ext))
            || allowed_names.contains(&file_name)
        {
            Ok(())
        } else {
            bail!("unsupported file type or name: {file_name}")
        }
    }

    pub(crate) fn embedding_provider(&self) -> Result<Option<(ProviderConfig, String)>> {
        let embedding = &self.config.embedding;
        if !embedding.is_configured() {
            return Ok(None);
        }
        let mut provider = self
            .config
            .provider(Some(embedding.provider_id.trim()))?
            .clone();
        let model = embedding.model.trim().to_string();
        provider.default_model = model.clone();
        Ok(Some((provider, model)))
    }

    pub(crate) fn meta_conn(&self) -> Result<Connection> {
        if let Some(parent) = self.meta_db.parent() {
            std::fs::create_dir_all(parent)?;
        }
        Ok(Connection::open(&self.meta_db)?)
    }

    pub(crate) fn semantic_conn(&self) -> Result<Connection> {
        if let Some(parent) = self.semantic_db.parent() {
            std::fs::create_dir_all(parent)?;
        }
        Ok(Connection::open(&self.semantic_db)?)
    }

    pub(crate) fn safe_file_path(&self, rel: &str) -> Result<PathBuf> {
        let rel = normalize_relative_path(rel)?;
        let path = self.files_dir.join(&rel);
        let base = self
            .files_dir
            .canonicalize()
            .unwrap_or_else(|_| self.files_dir.clone());
        let parent = path.parent().unwrap_or(&self.files_dir);
        std::fs::create_dir_all(parent)?;
        let resolved_parent = parent.canonicalize()?;
        if !resolved_parent.starts_with(&base) {
            bail!("knowledge base path escapes files dir")
        }
        Ok(path)
    }

    pub(crate) fn existing_file_path(&self, rel: &str) -> Result<PathBuf> {
        let rel = normalize_relative_path(rel)?;
        let path = self.files_dir.join(&rel);
        let base = self
            .files_dir
            .canonicalize()
            .unwrap_or_else(|_| self.files_dir.clone());
        let parent = path.parent().unwrap_or(&self.files_dir);
        let resolved_parent = parent.canonicalize()?;
        if !resolved_parent.starts_with(&base) {
            bail!("knowledge base path escapes files dir")
        }
        Ok(path)
    }
}
