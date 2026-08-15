use super::{ToolRegistry, ToolSpec};
use crate::config::{AppConfig, KnowledgeBasePluginConfig, ProviderConfig};
use crate::paths::GQYPaths;
use anyhow::{bail, Context, Result};
use chrono::Local;
use reqwest::Client;
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::process::Command;

mod store;

mod kb_tools;
pub use kb_tools::*;
#[cfg(test)]
mod tests;
pub fn register(registry: &mut ToolRegistry, config: AppConfig, paths: GQYPaths) {
    register_readonly(registry, config.clone(), paths.clone());
    if config.plugins.knowledge_base.upload_tool_enabled {
        let upload_config = config.clone();
        let upload_paths = paths.clone();
        registry.register(ToolSpec::new(
                "upload_text_to_knowledge_base",
            "Create a new knowledge-base file or replace an entire existing file. For updating part of an existing file, first search/read it and prefer edit_knowledge_base_file. Never use this for skills, memory, persona, identity, or configuration.",
            json!({
                "type": "object",
                "properties": {
                    "content": { "type": "string", "description": "Text content to save." },
                    "title": { "type": "string", "description": "Optional title used for markdown heading and default file name." },
                    "file_name": { "type": "string", "description": "Optional knowledge base relative path." }
                },
                "required": ["content"],
                "additionalProperties": false
            }),
            move |args| {
                let config = upload_config.clone();
                let paths = upload_paths.clone();
                async move { tool_upload(args, config, paths).await }
            },
        ).writes());
        let edit_config = config.clone();
        let edit_paths = paths.clone();
        registry.register(ToolSpec::new(
            "edit_knowledge_base_file",
            "Edit an existing knowledge-base file by replacing an inclusive 1-based line range. Use after search_knowledge_base/read_knowledge_base_file identifies the exact file and line numbers. This updates metadata and refreshes semantic indexing when embeddings are enabled.",
            json!({
                "type": "object",
                "properties": {
                    "file_name": { "type": "string", "description": "Knowledge base relative path to edit." },
                    "start_line": { "type": "integer", "description": "1-based first line to replace." },
                    "end_line": { "type": "integer", "description": "1-based last line to replace, inclusive." },
                    "replacement": { "type": "string", "description": "Replacement text. May contain multiple lines. Empty text deletes the line range." }
                },
                "required": ["file_name", "start_line", "end_line", "replacement"],
                "additionalProperties": false
            }),
            move |args| {
                let config = edit_config.clone();
                let paths = edit_paths.clone();
                async move { tool_edit(args, config, paths).await }
            },
        ).writes());
        let remove_config = config.clone();
        let remove_paths = paths.clone();
        registry.register(ToolSpec::new(
            "remove_knowledge_base_file",
            "Remove a knowledge-base file by relative path. Use only after the user asks to delete a knowledge-base entry or confirms the exact file. This also removes its metadata and semantic chunks.",
            json!({
                "type": "object",
                "properties": {
                    "file_name": { "type": "string", "description": "Knowledge base relative path to remove." }
                },
                "required": ["file_name"],
                "additionalProperties": false
            }),
            move |args| {
                let config = remove_config.clone();
                let paths = remove_paths.clone();
                async move { tool_remove(args, config, paths).await }
            },
        ).writes());
    }
}

pub fn register_readonly(registry: &mut ToolRegistry, config: AppConfig, paths: GQYPaths) {
    registry.register(ToolSpec::new(
        "search_knowledge_base",
        "Search the local knowledge base content. Returns file paths and original text snippets. Use read_knowledge_base_file if snippets are insufficient. Mention paths only when useful or when the user asks.",
        json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "Search keywords or user question." },
                "max_results": { "type": "integer", "description": "Optional result limit." }
            },
            "required": ["query"],
            "additionalProperties": false
        }),
        {
            let config = config.clone();
            let paths = paths.clone();
            move |args| {
                let config = config.clone();
                let paths = paths.clone();
                async move { tool_search_readonly(args, config, paths).await }
            }
        },
    ));
    registry.register(ToolSpec::new(
        "search_knowledge_base_by_name",
        "Find knowledge base files by file name, directory, extension, or path fragment. Returns relative paths for read_knowledge_base_file. Mention paths only when useful or when the user asks.",
        json!({
            "type": "object",
            "properties": {
                "file_name_query": { "type": "string", "description": "File name, directory, extension, or path fragment." },
                "max_results": { "type": "integer", "description": "Optional result limit." }
            },
            "required": ["file_name_query"],
            "additionalProperties": false
        }),
        {
            let config = config.clone();
            let paths = paths.clone();
            move |args| {
                let config = config.clone();
                let paths = paths.clone();
                async move { tool_find_readonly(args, config, paths).await }
            }
        },
    ));
    registry.register(ToolSpec::new(
        "read_knowledge_base_file",
        "Read a knowledge base file by relative path with line pagination. Prefer paths returned by search_knowledge_base or search_knowledge_base_by_name. Summarize the relevant content without exposing raw tool JSON.",
        json!({
            "type": "object",
            "properties": {
                "file_name": { "type": "string", "description": "Knowledge base relative path." },
                "start_line": { "type": "integer", "description": "1-based start line." },
                "max_lines": { "type": "integer", "description": "Optional line limit." }
            },
            "required": ["file_name"],
            "additionalProperties": false
        }),
        {
            let config = config.clone();
            let paths = paths.clone();
            move |args| {
                let config = config.clone();
                let paths = paths.clone();
                async move { tool_read_readonly(args, config, paths).await }
            }
        },
    ));
}

pub struct KnowledgeBase {
    config: AppConfig,
    root: PathBuf,
    files_dir: PathBuf,
    meta_db: PathBuf,
    semantic_db: PathBuf,
}

impl KnowledgeBase {
    pub fn new(config: AppConfig, paths: GQYPaths) -> Result<Self> {
        let root = kb_root(&config.plugins.knowledge_base, &paths);
        let files_dir = root.join("files");
        let meta_db = root.join("kb_meta.db");
        let semantic_db = root.join("semantic_index.db");
        Ok(Self {
            config,
            root,
            files_dir,
            meta_db,
            semantic_db,
        })
    }

    pub fn init(&self) -> Result<()> {
        std::fs::create_dir_all(&self.files_dir)?;
        let conn = self.meta_conn()?;
        init_meta_db(&conn)?;
        let semantic = self.semantic_conn()?;
        init_semantic_db(&semantic)?;
        Ok(())
    }

    fn readonly_available(&self) -> bool {
        self.root.is_dir() && self.files_dir.is_dir() && self.meta_db.is_file()
    }

    pub async fn add_path(&self, source: &Path) -> Result<Vec<String>> {
        self.init()?;
        let mut added = Vec::new();
        if source.is_dir() {
            let root_name = source
                .file_name()
                .and_then(|name| name.to_str())
                .context("source directory has no valid directory name")?;
            for file in collect_files(source)? {
                let rel = file.strip_prefix(source).unwrap_or(&file);
                let name = normalize_relative_path(&format!(
                    "{}/{}",
                    root_name,
                    rel.display().to_string().replace('\\', "/")
                ))?;
                if let Ok(name) = self.import_file(&file, &name) {
                    added.push(name);
                }
            }
        } else {
            let name = normalize_relative_path(
                source
                    .file_name()
                    .and_then(|name| name.to_str())
                    .context("source file has no valid file name")?,
            )?;
            added.push(self.import_file(source, &name)?);
        }
        self.spawn_embedding_reindex()?;
        Ok(added)
    }

    pub fn replace_default_files(&self, source: &Path) -> Result<Vec<String>> {
        self.init()?;
        self.remove_prefix("default-kb/")?;
        let mut added = Vec::new();
        for file in collect_files(source)? {
            let rel = file.strip_prefix(source).unwrap_or(&file);
            let rel = rel.display().to_string().replace('\\', "/");
            let name = normalize_relative_path(&format!("default-kb/{rel}"))?;
            if let Ok(name) = self.import_file(&file, &name) {
                added.push(name);
            }
        }
        self.spawn_embedding_reindex()?;
        Ok(added)
    }

    pub fn list(&self) -> Result<Vec<FileRecord>> {
        self.init()?;
        self.list_existing()
    }

    fn list_existing(&self) -> Result<Vec<FileRecord>> {
        let conn = self.meta_conn()?;
        let mut stmt =
            conn.prepare("SELECT name, path, size_bytes, content_sha256 FROM files ORDER BY name")?;
        let rows = stmt.query_map([], |row| {
            Ok(FileRecord {
                name: row.get(0)?,
                path: row.get(1)?,
                size_bytes: row.get(2)?,
                content_sha256: row.get(3)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub async fn search(&self, query: &str, max_results: Option<usize>) -> Result<Value> {
        self.init()?;
        self.search_existing(query, max_results, true).await
    }

    pub async fn search_readonly(&self, query: &str, max_results: Option<usize>) -> Result<Value> {
        if !self.readonly_available() {
            return Ok(
                json!({"ok": true, "query": query, "total_matches": 0, "semantic_used": false, "results": []}),
            );
        }
        self.search_existing(query, max_results, self.semantic_db.is_file())
            .await
    }

    async fn search_existing(
        &self,
        query: &str,
        max_results: Option<usize>,
        allow_semantic: bool,
    ) -> Result<Value> {
        let limit = max_results
            .unwrap_or(self.config.plugins.knowledge_base.max_search_results)
            .clamp(1, 50);
        let mut results = self.keyword_search(query, limit)?;
        let strongest = results.first().map(|item| item.score).unwrap_or(0.0);
        let mut semantic_used = false;
        if allow_semantic
            && self.config.plugins.knowledge_base.embedding_enabled
            && strongest
                < self
                    .config
                    .plugins
                    .knowledge_base
                    .keyword_strong_score_threshold
        {
            if let Ok(semantic) = self.semantic_search(query).await {
                semantic_used = !semantic.is_empty();
                merge_results(&mut results, semantic, limit);
            }
        }
        Ok(json!({
            "ok": true,
            "query": query,
            "total_matches": results.len(),
            "semantic_used": semantic_used,
            "results": results.iter().map(SearchResult::to_json).collect::<Vec<_>>(),
        }))
    }

    pub fn find_by_name(&self, query: &str, max_results: Option<usize>) -> Result<Value> {
        self.init()?;
        self.find_by_name_existing(query, max_results)
    }

    pub fn find_by_name_readonly(&self, query: &str, max_results: Option<usize>) -> Result<Value> {
        if !self.readonly_available() {
            return Ok(json!({"ok": true, "query": query, "total_matches": 0, "results": []}));
        }
        self.find_by_name_existing(query, max_results)
    }

    fn find_by_name_existing(&self, query: &str, max_results: Option<usize>) -> Result<Value> {
        let limit = max_results
            .unwrap_or(self.config.plugins.knowledge_base.max_search_results)
            .clamp(1, 50);
        let mut results = Vec::new();
        for record in self.list()? {
            let (score, reason) = score_file_name(query, &record.name);
            if score <= 0.0 {
                continue;
            }
            results.push(json!({
                "path": record.name,
                "name": file_name(&record.name),
                "directory": directory_name(&record.name),
                "score": score,
                "match_reason": reason,
                "size_kb": (record.size_bytes as f64 / 1024.0 * 10.0).round() / 10.0,
            }));
        }
        results.sort_by(|a, b| {
            b.get("score")
                .and_then(Value::as_f64)
                .unwrap_or_default()
                .partial_cmp(&a.get("score").and_then(Value::as_f64).unwrap_or_default())
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        results.truncate(limit);
        Ok(json!({
            "ok": true,
            "query": query,
            "total_matches": results.len(),
            "results": results,
        }))
    }

    pub fn read_file(
        &self,
        name: &str,
        start_line: usize,
        max_lines: Option<usize>,
    ) -> Result<String> {
        self.init()?;
        self.read_file_existing(name, start_line, max_lines, true)
    }

    pub fn read_file_readonly(
        &self,
        name: &str,
        start_line: usize,
        max_lines: Option<usize>,
    ) -> Result<String> {
        if !self.readonly_available() {
            bail!("knowledge base is not initialized")
        }
        self.read_file_existing(name, start_line, max_lines, false)
    }

    /// Resolve a caller-supplied path to a stored record name, tolerating
    /// omitted directory prefixes (e.g. `4. xx/文件.md` for
    /// `default-kb/kb/4. xx/文件.md`): exact match first, then a unique
    /// suffix match; otherwise fail with concrete candidates so the model
    /// can self-correct in one step.
    fn resolve_stored_name(&self, rel: &str) -> Result<String> {
        let records = self.list_existing()?;
        if records.iter().any(|record| record.name == rel) {
            return Ok(rel.to_string());
        }
        let suffix = format!("/{rel}");
        let matches = records
            .iter()
            .filter(|record| record.name.ends_with(&suffix))
            .map(|record| record.name.clone())
            .collect::<Vec<_>>();
        match matches.len() {
            1 => Ok(matches.into_iter().next().unwrap()),
            0 => {
                let mut scored = records
                    .iter()
                    .map(|record| (score_file_name(rel, &record.name).0, &record.name))
                    .filter(|(score, _)| *score > 0.0)
                    .collect::<Vec<_>>();
                scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
                let hints = scored
                    .iter()
                    .take(3)
                    .map(|(_, name)| name.as_str())
                    .collect::<Vec<_>>();
                if hints.is_empty() {
                    bail!("knowledge base file not found: {rel}")
                }
                bail!(
                    "knowledge base file not found: {rel}；相近的文件: {}",
                    hints.join("、")
                )
            }
            _ => bail!(
                "path {rel} matches multiple knowledge base files: {}；请用完整路径",
                matches.join("、")
            ),
        }
    }

    fn read_file_existing(
        &self,
        name: &str,
        start_line: usize,
        max_lines: Option<usize>,
        create_parent: bool,
    ) -> Result<String> {
        let mut rel = normalize_relative_path(name)?;
        let build_path = |rel: &str| -> Result<PathBuf> {
            if create_parent {
                self.safe_file_path(rel)
            } else {
                // Fails with ENOENT when the parent directory itself is
                // missing — treat that the same as "file not found" so the
                // prefix-tolerant resolution below still gets its chance.
                self.existing_file_path(rel)
            }
        };
        let mut path = build_path(&rel).unwrap_or_default();
        if !path.exists() {
            rel = self.resolve_stored_name(&rel)?;
            path = build_path(&rel)?;
        }
        if !path.exists() {
            bail!("knowledge base file not found: {rel}")
        }
        let content = std::fs::read_to_string(&path)?;
        let start = start_line.max(1);
        let max_lines = max_lines
            .unwrap_or(self.config.plugins.knowledge_base.max_read_lines)
            .clamp(1, 5000);
        let mut total = 0usize;
        let mut selected = Vec::new();
        for (index, line) in content.lines().enumerate() {
            let line_no = index + 1;
            total = line_no;
            if line_no >= start && selected.len() < max_lines {
                selected.push(line);
            }
        }
        if start > total.max(1) {
            return Ok(format!(
                "=== {rel} | start_line {start} out of range / {total} lines ==="
            ));
        }
        let end = (start + max_lines - 1).min(total);
        let mut output = format!("=== {rel} | lines {start}-{end} / {total} ===\n");
        output.push_str(&selected.join("\n"));
        if end < total {
            output.push_str(&format!(
                "\n\n... {remaining} more lines; continue with start_line={next}",
                remaining = total - end,
                next = end + 1
            ));
        }
        Ok(output)
    }

    pub fn remove(&self, name: &str) -> Result<()> {
        self.init()?;
        let rel = normalize_relative_path(name)?;
        let path = self.safe_file_path(&rel)?;
        if path.exists() {
            std::fs::remove_file(&path)?;
        }
        let conn = self.meta_conn()?;
        conn.execute("DELETE FROM files WHERE name=?1", params![rel])?;
        let semantic = self.semantic_conn()?;
        semantic.execute(
            "DELETE FROM semantic_chunks WHERE file_name=?1",
            params![rel],
        )?;
        Ok(())
    }

    pub fn edit_lines(
        &self,
        name: &str,
        start_line: usize,
        end_line: usize,
        replacement: &str,
    ) -> Result<EditResult> {
        self.init()?;
        let rel = normalize_relative_path(name)?;
        if start_line == 0 || end_line == 0 {
            bail!("line numbers must be 1-based")
        }
        if start_line > end_line {
            bail!("start_line must be less than or equal to end_line")
        }
        let path = self.existing_file_path(&rel)?;
        if !path.exists() {
            bail!("knowledge base file not found: {rel}")
        }
        let original = std::fs::read_to_string(&path)?;
        let had_trailing_newline = original.ends_with('\n');
        let mut lines = original.lines().map(str::to_string).collect::<Vec<_>>();
        let total_lines = lines.len();
        if start_line > total_lines || end_line > total_lines {
            bail!("line range {start_line}-{end_line} out of range: {total_lines} lines")
        }
        let replacement = replacement.replace("\r\n", "\n").replace('\r', "\n");
        let replacement_lines = if replacement.is_empty() {
            Vec::new()
        } else {
            replacement.lines().map(str::to_string).collect::<Vec<_>>()
        };
        lines.splice(start_line - 1..end_line, replacement_lines);
        let mut updated = lines.join("\n");
        if had_trailing_newline && !updated.is_empty() {
            updated.push('\n');
        }
        let temp = tempfile::NamedTempFile::new()?;
        std::fs::write(temp.path(), updated.as_bytes())?;
        self.import_file(temp.path(), &rel)?;
        let semantic_refreshed = self.refresh_semantic_after_write(&rel)?;
        Ok(EditResult {
            path: rel,
            old_line_count: total_lines,
            new_line_count: lines.len(),
            semantic_refreshed,
        })
    }
}

#[derive(Clone)]
pub struct FileRecord {
    pub name: String,
    path: String,
    pub size_bytes: i64,
    content_sha256: String,
}

#[derive(Debug)]
pub struct EditResult {
    path: String,
    old_line_count: usize,
    new_line_count: usize,
    semantic_refreshed: bool,
}

struct SearchResult {
    path: String,
    score: f32,
    snippets: Vec<String>,
    source: &'static str,
}

impl SearchResult {
    fn new(path: String, score: f32, snippets: Vec<String>, source: &'static str) -> Self {
        Self {
            path,
            score,
            snippets,
            source,
        }
    }

    fn to_json(&self) -> Value {
        json!({
            "path": self.path,
            "name": file_name(&self.path),
            "directory": directory_name(&self.path),
            "score": (self.score * 10.0).round() / 10.0,
            "source": self.source,
            "snippets": self.snippets,
        })
    }
}

struct Chunk {
    index: usize,
    start: usize,
    end: usize,
    text: String,
}
