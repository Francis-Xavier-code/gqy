//! kb_tools — 自 src/tools/knowledge_base.rs 拆分。

pub(crate) use super::*;

pub(crate) async fn tool_search_readonly(
    args: Value,
    config: AppConfig,
    paths: GQYPaths,
) -> Result<String> {
    ensure_enabled(&config)?;
    let query = args
        .get("query")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    if query.is_empty() {
        bail!("query is required")
    }
    let max_results = args
        .get("max_results")
        .and_then(Value::as_u64)
        .map(|value| value as usize);
    Ok(KnowledgeBase::new(config, paths)?
        .search_readonly(query, max_results)
        .await?
        .to_string())
}

pub(crate) async fn tool_find_readonly(
    args: Value,
    config: AppConfig,
    paths: GQYPaths,
) -> Result<String> {
    ensure_enabled(&config)?;
    let query = args
        .get("file_name_query")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    if query.is_empty() {
        bail!("file_name_query is required")
    }
    let max_results = args
        .get("max_results")
        .and_then(Value::as_u64)
        .map(|value| value as usize);
    Ok(KnowledgeBase::new(config, paths)?
        .find_by_name_readonly(query, max_results)?
        .to_string())
}

pub(crate) async fn tool_read_readonly(
    args: Value,
    config: AppConfig,
    paths: GQYPaths,
) -> Result<String> {
    ensure_enabled(&config)?;
    let name = args
        .get("file_name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    if name.is_empty() {
        bail!("file_name is required")
    }
    let start_line = args.get("start_line").and_then(Value::as_u64).unwrap_or(1) as usize;
    let max_lines = args
        .get("max_lines")
        .and_then(Value::as_u64)
        .map(|value| value as usize);
    KnowledgeBase::new(config, paths)?.read_file_readonly(name, start_line, max_lines)
}

pub(crate) async fn tool_upload(args: Value, config: AppConfig, paths: GQYPaths) -> Result<String> {
    ensure_enabled(&config)?;
    if !config.plugins.knowledge_base.upload_tool_enabled {
        bail!("knowledge base upload tool is disabled")
    }
    let content = args
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    if content.is_empty() {
        bail!("content is required")
    }
    let title = args
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("knowledge note")
        .trim();
    let file_name = args
        .get("file_name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    reject_non_kb_upload(content, title, file_name)?;
    let rel = if file_name.is_empty() {
        format!(
            "chat_uploads/{}/{}.md",
            Local::now().format("%Y-%m-%d"),
            slug(title)
        )
    } else {
        normalize_relative_path(file_name)?
    };
    let body = format!(
        "# {}\n\n> 来源：用户要求保存到本地知识库\n> 上传时间：{}\n\n{}\n",
        if title.is_empty() {
            Path::new(&rel)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("knowledge note")
        } else {
            title
        },
        Local::now().format("%Y-%m-%d %H:%M:%S"),
        content
    );
    let kb = KnowledgeBase::new(config, paths)?;
    kb.init()?;
    let temp = tempfile::NamedTempFile::new()?;
    std::fs::write(temp.path(), body.as_bytes())?;
    let saved = kb.import_file(temp.path(), &rel)?;
    kb.spawn_embedding_reindex()?;
    Ok(json!({
        "ok": true,
        "path": saved,
    })
    .to_string())
}

pub(crate) async fn tool_edit(args: Value, config: AppConfig, paths: GQYPaths) -> Result<String> {
    ensure_enabled(&config)?;
    let name = args
        .get("file_name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    if name.is_empty() {
        bail!("file_name is required")
    }
    let start_line = args
        .get("start_line")
        .and_then(Value::as_u64)
        .context("start_line is required")? as usize;
    let end_line = args
        .get("end_line")
        .and_then(Value::as_u64)
        .context("end_line is required")? as usize;
    let replacement = args
        .get("replacement")
        .and_then(Value::as_str)
        .context("replacement is required")?;
    let result =
        KnowledgeBase::new(config, paths)?.edit_lines(name, start_line, end_line, replacement)?;
    Ok(json!({
        "ok": true,
        "path": result.path,
        "old_line_count": result.old_line_count,
        "new_line_count": result.new_line_count,
        "semantic_refreshed": result.semantic_refreshed,
        "warning": if name.starts_with("default-kb/") { Some("default-kb files may be overwritten by gqy update-default-kb") } else { None::<&str> },
    })
    .to_string())
}

pub(crate) async fn tool_remove(args: Value, config: AppConfig, paths: GQYPaths) -> Result<String> {
    ensure_enabled(&config)?;
    let name = args
        .get("file_name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    if name.is_empty() {
        bail!("file_name is required")
    }
    let rel = normalize_relative_path(name)?;
    KnowledgeBase::new(config, paths)?.remove(&rel)?;
    Ok(json!({
        "ok": true,
        "path": rel,
        "warning": if name.starts_with("default-kb/") { Some("default-kb files may be restored by gqy update-default-kb") } else { None::<&str> },
    })
    .to_string())
}

pub(crate) fn reject_non_kb_upload(content: &str, title: &str, file_name: &str) -> Result<()> {
    let text = format!("{content}\n{title}\n{file_name}").to_ascii_lowercase();
    let forbidden = [
        "skill", "skills/", "skll", "记忆", "memory", "persona", "identity", "prompt", "配置",
        "config",
    ];
    if forbidden.iter().any(|needle| text.contains(needle)) {
        bail!("this content looks like a skill, memory, prompt, identity, or config request; do not upload it to the knowledge base")
    }
    Ok(())
}

pub async fn embed_text(
    config: &AppConfig,
    provider: &ProviderConfig,
    model: &str,
    text: &str,
) -> Result<Vec<f32>> {
    let api_key = provider.api_key.as_deref().unwrap_or_default().trim();
    if api_key.is_empty() {
        bail!("embedding provider {} has no api_key", provider.id)
    }
    let client = Client::builder()
        .timeout(Duration::from_secs(config.embedding.timeout_seconds.max(1)))
        .build()?;
    let url = format!("{}/embeddings", provider.base_url.trim_end_matches('/'));
    let response = client
        .post(&url)
        .bearer_auth(api_key)
        .json(&json!({ "model": model, "input": text }))
        .send()
        .await?;
    let status = response.status();
    if !status.is_success() {
        let text = response.text().await.unwrap_or_default();
        bail!(
            "embedding API error at {url} ({status}): {}",
            compact_whitespace(&text)
        );
    }
    let data: Value = response.json().await?;
    let embedding = data
        .get("data")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .and_then(|item| item.get("embedding"))
        .and_then(Value::as_array)
        .context("embedding response missing data[0].embedding")?;
    Ok(embedding
        .iter()
        .filter_map(Value::as_f64)
        .map(|value| value as f32)
        .collect())
}

pub(crate) fn ensure_enabled(config: &AppConfig) -> Result<()> {
    if !config.plugins.knowledge_base.enabled {
        bail!("knowledge base plugin is disabled")
    }
    Ok(())
}

pub(crate) fn init_meta_db(conn: &Connection) -> Result<()> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS files (name TEXT PRIMARY KEY, path TEXT NOT NULL, size_bytes INTEGER NOT NULL, mtime REAL NOT NULL, content_sha256 TEXT NOT NULL, updated_at REAL NOT NULL)",
        [],
    )?;
    Ok(())
}

pub(crate) fn init_semantic_db(conn: &Connection) -> Result<()> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS semantic_chunks (id INTEGER PRIMARY KEY AUTOINCREMENT, provider_id TEXT NOT NULL, model TEXT NOT NULL, file_name TEXT NOT NULL, content_sha256 TEXT NOT NULL, chunk_index INTEGER NOT NULL, start_char INTEGER NOT NULL, end_char INTEGER NOT NULL, text TEXT NOT NULL, embedding_json TEXT NOT NULL, created_at REAL NOT NULL)",
        [],
    )?;
    conn.execute("CREATE INDEX IF NOT EXISTS idx_semantic_file ON semantic_chunks(file_name, content_sha256)", [])?;
    Ok(())
}

pub(crate) fn kb_root(config: &KnowledgeBasePluginConfig, paths: &GQYPaths) -> PathBuf {
    let configured = config.data_dir.trim();
    if configured.is_empty() {
        paths.data_dir.join("kb")
    } else {
        expand_path(configured)
    }
}

pub(crate) fn normalize_relative_path(value: &str) -> Result<String> {
    let path = Path::new(value.trim());
    if path.is_absolute() {
        bail!("knowledge base path must be relative")
    }
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => {
                let part = part.to_string_lossy();
                if part.contains('\0') || part.trim().is_empty() {
                    bail!("invalid path component")
                }
                parts.push(part.to_string());
            }
            Component::CurDir => {}
            _ => bail!("knowledge base path contains illegal component"),
        }
    }
    if parts.is_empty() {
        bail!("knowledge base path is empty")
    }
    Ok(parts.join("/"))
}

pub(crate) fn collect_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            out.extend(collect_files(&path)?);
        } else if path.is_file() {
            out.push(path);
        }
    }
    Ok(out)
}

pub(crate) fn split_csv(value: &str) -> HashSet<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_ascii_lowercase())
        .collect()
}

pub(crate) fn query_tokens(value: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut ascii = String::new();
    let mut chinese = Vec::new();
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            ascii.push(ch.to_ascii_lowercase());
            flush_chinese(&mut chinese, &mut tokens);
        } else if ('\u{4e00}'..='\u{9fff}').contains(&ch) {
            if !ascii.is_empty() {
                tokens.push(std::mem::take(&mut ascii));
            }
            chinese.push(ch);
        } else {
            if !ascii.is_empty() {
                tokens.push(std::mem::take(&mut ascii));
            }
            flush_chinese(&mut chinese, &mut tokens);
        }
    }
    if !ascii.is_empty() {
        tokens.push(ascii);
    }
    flush_chinese(&mut chinese, &mut tokens);
    let mut seen = HashSet::new();
    tokens
        .into_iter()
        .filter(|token| token.chars().count() > 1 || !token.is_ascii())
        .filter(|token| seen.insert(token.clone()))
        .collect()
}

pub(crate) fn flush_chinese(chars: &mut Vec<char>, tokens: &mut Vec<String>) {
    if chars.is_empty() {
        return;
    }
    let text = chars.iter().collect::<String>();
    tokens.push(text);
    for window in chars.windows(2) {
        tokens.push(window.iter().collect());
    }
    chars.clear();
}

pub(crate) fn find_positions(content: &str, needle: &str, limit: usize) -> Vec<usize> {
    let mut positions = Vec::new();
    let mut start = 0;
    while let Some(pos) = content[start..].find(needle) {
        let absolute = start + pos;
        positions.push(absolute);
        if positions.len() >= limit {
            break;
        }
        start = absolute + needle.len().max(1);
    }
    positions
}

pub(crate) fn best_window(
    positions_by_token: &HashMap<String, Vec<usize>>,
    tokens: &[String],
    window_chars: usize,
) -> Option<(usize, usize, f32)> {
    let mut events = Vec::new();
    for token in tokens {
        for pos in positions_by_token.get(token).into_iter().flatten() {
            events.push((*pos, token.as_str()));
        }
    }
    events.sort_by_key(|event| event.0);
    let mut best = None;
    for left in 0..events.len() {
        let mut seen = HashSet::new();
        let start = events[left].0;
        let mut end = start;
        for (pos, token) in events.iter().skip(left) {
            if *pos - start > window_chars {
                break;
            }
            seen.insert(*token);
            end = *pos + token.len();
        }
        let coverage = seen.len() as f32 / tokens.len().max(1) as f32;
        if best.map(|(_, _, score)| coverage > score).unwrap_or(true) {
            best = Some((start, end, coverage));
        }
    }
    best.filter(|(_, _, coverage)| *coverage > 0.0)
}

pub(crate) fn extract_snippets(
    content: &str,
    content_lower: &str,
    tokens: &[String],
    context: usize,
) -> Vec<String> {
    let mut snippets = Vec::new();
    for token in tokens {
        if let Some(pos) = content_lower.find(token) {
            snippets.push(snippet_chars(content, pos, pos + token.len(), context));
        }
        if snippets.len() >= 3 {
            break;
        }
    }
    if snippets.is_empty() && !content.trim().is_empty() {
        snippets.push(compact_whitespace(
            &content.chars().take(context * 2).collect::<String>(),
        ));
    }
    snippets
}

pub(crate) fn snippet_chars(content: &str, start: usize, end: usize, context: usize) -> String {
    let start = content[..start.min(content.len())]
        .char_indices()
        .rev()
        .nth(context)
        .map(|(idx, _)| idx)
        .unwrap_or(0);
    let end = content[end.min(content.len())..]
        .char_indices()
        .nth(context)
        .map(|(idx, _)| end.min(content.len()) + idx)
        .unwrap_or(content.len());
    compact_whitespace(&content[start..end])
}

pub(crate) fn build_chunks(content: &str, chunk_chars: usize, overlap: usize) -> Vec<Chunk> {
    let chars = content.char_indices().collect::<Vec<_>>();
    let mut chunks = Vec::new();
    let mut start_char = 0usize;
    let mut index = 0usize;
    let total_chars = content.chars().count();
    while start_char < total_chars {
        let end_char = (start_char + chunk_chars).min(total_chars);
        let start_byte = chars.get(start_char).map(|(idx, _)| *idx).unwrap_or(0);
        let end_byte = chars
            .get(end_char)
            .map(|(idx, _)| *idx)
            .unwrap_or(content.len());
        let text = content[start_byte..end_byte].to_string();
        if !text.trim().is_empty() {
            chunks.push(Chunk {
                index,
                start: start_byte,
                end: end_byte,
                text,
            });
            index += 1;
        }
        if end_char >= total_chars {
            break;
        }
        start_char = end_char.saturating_sub(overlap).max(start_char + 1);
    }
    chunks
}

pub(crate) fn merge_results(
    results: &mut Vec<SearchResult>,
    semantic: Vec<SearchResult>,
    limit: usize,
) {
    for item in semantic {
        if let Some(existing) = results.iter_mut().find(|result| result.path == item.path) {
            existing.score += item.score * 0.6;
            existing.snippets.extend(item.snippets);
            existing.snippets.truncate(4);
        } else {
            results.push(item);
        }
    }
    results.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    results.truncate(limit);
}

pub(crate) fn score_file_name(query: &str, name: &str) -> (f64, &'static str) {
    let query = query.replace('\\', "/").to_ascii_lowercase();
    let name = name.replace('\\', "/").to_ascii_lowercase();
    let base = file_name(&name);
    if query == name {
        (1000.0, "exact_path")
    } else if query == base {
        (950.0, "exact_file_name")
    } else if name.contains(&query) {
        (820.0 + query.len().min(60) as f64, "path_contains")
    } else if base.contains(&query) {
        (760.0 + query.len().min(60) as f64, "file_name_contains")
    } else {
        let tokens = query_tokens(&query);
        let matched = tokens.iter().filter(|token| name.contains(*token)).count();
        if matched == 0 {
            (0.0, "")
        } else {
            (300.0 + matched as f64 * 80.0, "partial_name_terms")
        }
    }
}

pub(crate) fn cosine(left: &[f32], right: &[f32]) -> f32 {
    if left.len() != right.len() || left.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0;
    let mut left_norm = 0.0;
    let mut right_norm = 0.0;
    for (a, b) in left.iter().zip(right) {
        dot += a * b;
        left_norm += a * a;
        right_norm += b * b;
    }
    if left_norm <= 0.0 || right_norm <= 0.0 {
        0.0
    } else {
        dot / (left_norm.sqrt() * right_norm.sqrt())
    }
}

pub(crate) fn file_name(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_string()
}

pub(crate) fn directory_name(path: &str) -> String {
    path.rsplit_once('/')
        .map(|(dir, _)| dir.to_string())
        .unwrap_or_default()
}

pub(crate) fn compact_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

pub(crate) fn now_secs() -> f64 {
    unix_time(SystemTime::now())
}

pub(crate) fn unix_time(time: SystemTime) -> f64 {
    time.duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

pub(crate) fn expand_path(value: &str) -> PathBuf {
    if let Some(rest) = value.trim().strip_prefix("~/") {
        if let Some(home) = directories::BaseDirs::new().map(|dirs| dirs.home_dir().to_path_buf()) {
            return home.join(rest);
        }
    }
    PathBuf::from(value.trim())
}

pub(crate) fn slug(value: &str) -> String {
    let mut slug = value
        .chars()
        .filter_map(|ch| {
            if ch.is_ascii_alphanumeric() {
                Some(ch.to_ascii_lowercase())
            } else if ch.is_whitespace() || matches!(ch, '-' | '_') {
                Some('-')
            } else {
                None
            }
        })
        .collect::<String>();
    while slug.contains("--") {
        slug = slug.replace("--", "-");
    }
    let slug = slug.trim_matches('-');
    if slug.is_empty() {
        format!("note-{}", Local::now().format("%H%M%S"))
    } else {
        slug.chars().take(48).collect()
    }
}
