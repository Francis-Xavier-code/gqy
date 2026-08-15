//! ops — 自 src/memory/mod.rs 拆分。

pub(crate) use super::*;

pub(crate) fn init_data_db(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS facts (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            content TEXT NOT NULL,
            source TEXT NOT NULL DEFAULT '',
            status TEXT NOT NULL DEFAULT 'active',
            confidence REAL NOT NULL DEFAULT 1.0,
            strength REAL NOT NULL DEFAULT 1.0,
            recall_count INTEGER NOT NULL DEFAULT 0,
            last_recalled_at TEXT,
            last_decay_at TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            visibility TEXT NOT NULL DEFAULT 'privileged',
            owner_principal TEXT NOT NULL DEFAULT '',
            owner_display_name TEXT NOT NULL DEFAULT '',
            subjects TEXT NOT NULL DEFAULT '[]'
        );
        CREATE TABLE IF NOT EXISTS episodes (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            content TEXT NOT NULL,
            source TEXT NOT NULL DEFAULT 'episode',
            status TEXT NOT NULL DEFAULT 'active',
            strength REAL NOT NULL DEFAULT 1.0,
            recall_count INTEGER NOT NULL DEFAULT 0,
            last_recalled_at TEXT,
            last_decay_at TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            visibility TEXT NOT NULL DEFAULT 'privileged',
            owner_principal TEXT NOT NULL DEFAULT '',
            owner_display_name TEXT NOT NULL DEFAULT '',
            subjects TEXT NOT NULL DEFAULT '[]'
        );
        CREATE TABLE IF NOT EXISTS pending_events (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            user_message TEXT NOT NULL,
            assistant_message TEXT NOT NULL,
            created_at TEXT NOT NULL,
            processed_at TEXT
        );
        CREATE TABLE IF NOT EXISTS skill_records (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL UNIQUE,
            path TEXT NOT NULL,
            summary TEXT NOT NULL DEFAULT '',
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS memory_revisions (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            memory_id INTEGER NOT NULL,
            old_content TEXT NOT NULL,
            new_content TEXT NOT NULL,
            source_episode_ids TEXT NOT NULL DEFAULT '[]',
            created_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS memory_meta (
            id INTEGER PRIMARY KEY CHECK(id=1),
            generation INTEGER NOT NULL DEFAULT 0,
            database_id TEXT NOT NULL DEFAULT '',
            access_schema_version INTEGER NOT NULL DEFAULT 2
        );",
    )?;
    add_column_if_missing(
        conn,
        "memory_meta",
        "database_id",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    add_column_if_missing(
        conn,
        "memory_meta",
        "access_schema_version",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    conn.execute(
        "INSERT OR IGNORE INTO memory_meta (
            id, generation, database_id, access_schema_version
         ) VALUES (1, 0, '', 2)",
        [],
    )?;
    let database_id = conn.query_row(
        "SELECT database_id FROM memory_meta WHERE id=1",
        [],
        |row| row.get::<_, String>(0),
    )?;
    if database_id.is_empty() {
        conn.execute(
            "UPDATE memory_meta SET database_id=?1 WHERE id=1 AND database_id=''",
            [format!("mem-{:032x}", rand::random::<u128>())],
        )?;
    }
    add_column_if_missing(conn, "facts", "strength", "REAL NOT NULL DEFAULT 1.0")?;
    add_column_if_missing(conn, "facts", "last_decay_at", "TEXT")?;
    add_column_if_missing(conn, "facts", "memory_type", "TEXT NOT NULL DEFAULT 'fact'")?;
    add_column_if_missing(
        conn,
        "facts",
        "truth_status",
        "TEXT NOT NULL DEFAULT 'reported'",
    )?;
    add_column_if_missing(conn, "facts", "importance", "INTEGER NOT NULL DEFAULT 3")?;
    add_column_if_missing(conn, "facts", "tags", "TEXT NOT NULL DEFAULT '[]'")?;
    add_column_if_missing(
        conn,
        "facts",
        "source_episode_ids",
        "TEXT NOT NULL DEFAULT '[]'",
    )?;
    for table in ["facts", "episodes"] {
        add_column_if_missing(
            conn,
            table,
            "visibility",
            "TEXT NOT NULL DEFAULT 'privileged'",
        )?;
        add_column_if_missing(conn, table, "owner_principal", "TEXT NOT NULL DEFAULT ''")?;
        add_column_if_missing(
            conn,
            table,
            "owner_display_name",
            "TEXT NOT NULL DEFAULT ''",
        )?;
        add_column_if_missing(conn, table, "subjects", "TEXT NOT NULL DEFAULT '[]'")?;
    }
    add_column_if_missing(conn, "episodes", "strength", "REAL NOT NULL DEFAULT 1.0")?;
    add_column_if_missing(conn, "episodes", "last_decay_at", "TEXT")?;
    // Existing episodes predate the short/long split and must remain durable.
    add_column_if_missing(
        conn,
        "episodes",
        "retention",
        "TEXT NOT NULL DEFAULT 'long_term'",
    )?;
    add_column_if_missing(conn, "episodes", "user_message", "TEXT NOT NULL DEFAULT ''")?;
    add_column_if_missing(
        conn,
        "episodes",
        "assistant_message",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    add_column_if_missing(conn, "episodes", "expires_at", "TEXT")?;
    add_column_if_missing(conn, "episodes", "consolidated_at", "TEXT")?;
    add_column_if_missing(
        conn,
        "episodes",
        "promotion_pending",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    add_column_if_missing(conn, "episodes", "promoted_at", "TEXT")?;
    add_column_if_missing(conn, "episodes", "importance", "INTEGER NOT NULL DEFAULT 3")?;
    add_column_if_missing(conn, "episodes", "confidence", "REAL NOT NULL DEFAULT 1.0")?;
    add_column_if_missing(conn, "episodes", "tags", "TEXT NOT NULL DEFAULT '[]'")?;
    add_column_if_missing(
        conn,
        "episodes",
        "source_episode_ids",
        "TEXT NOT NULL DEFAULT '[]'",
    )?;
    add_column_if_missing(conn, "episodes", "source_key", "TEXT NOT NULL DEFAULT ''")?;
    add_column_if_missing(conn, "episodes", "origin_kind", "TEXT NOT NULL DEFAULT ''")?;
    add_column_if_missing(
        conn,
        "episodes",
        "origin_platform",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    add_column_if_missing(
        conn,
        "episodes",
        "origin_account_id",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    add_column_if_missing(
        conn,
        "episodes",
        "origin_conversation_kind",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    add_column_if_missing(
        conn,
        "episodes",
        "origin_conversation_id",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    add_column_if_missing(
        conn,
        "episodes",
        "origin_sender_id",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    add_column_if_missing(
        conn,
        "episodes",
        "origin_sender_display_name",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    add_column_if_missing(
        conn,
        "episodes",
        "origin_session_id",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    add_column_if_missing(
        conn,
        "episodes",
        "origin_message_id",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    migrate_memory_access_v1(conn)?;
    migrate_memory_subjects_v2(conn)?;
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_episodes_retention_created
             ON episodes(retention, created_at);
         CREATE INDEX IF NOT EXISTS idx_episodes_organization
             ON episodes(retention, promotion_pending, consolidated_at, id);
         CREATE INDEX IF NOT EXISTS idx_memory_revisions_memory
             ON memory_revisions(memory_id, id);
         CREATE UNIQUE INDEX IF NOT EXISTS idx_long_diary_source_key
             ON episodes(source_key) WHERE retention='long_term' AND source_key!='';
         CREATE INDEX IF NOT EXISTS idx_facts_access_updated
             ON facts(visibility, owner_principal, updated_at DESC);
         CREATE INDEX IF NOT EXISTS idx_episodes_access_updated
             ON episodes(visibility, owner_principal, updated_at DESC);",
    )?;
    Ok(())
}

pub(crate) fn init_state_db(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS evicted_turns (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            source_id TEXT,
            timestamp TEXT NOT NULL,
            role TEXT NOT NULL,
            content TEXT NOT NULL,
            created_at TEXT NOT NULL,
            visibility TEXT NOT NULL DEFAULT 'privileged',
            owner_principal TEXT NOT NULL DEFAULT '',
            owner_display_name TEXT NOT NULL DEFAULT ''
        );
        CREATE TABLE IF NOT EXISTS evicted_embeddings (
            id INTEGER PRIMARY KEY,
            model TEXT NOT NULL,
            embedding_json TEXT NOT NULL,
            created_at TEXT NOT NULL
        );
        CREATE VIRTUAL TABLE IF NOT EXISTS evicted_turns_fts USING fts5(
            content,
            content='evicted_turns',
            content_rowid='id',
            tokenize='trigram'
        );
        CREATE TRIGGER IF NOT EXISTS evicted_turns_fts_insert AFTER INSERT ON evicted_turns BEGIN
            INSERT INTO evicted_turns_fts(rowid, content) VALUES (new.id, new.content);
        END;
        CREATE TRIGGER IF NOT EXISTS evicted_turns_fts_delete AFTER DELETE ON evicted_turns BEGIN
            INSERT INTO evicted_turns_fts(evicted_turns_fts, rowid, content)
            VALUES ('delete', old.id, old.content);
        END;
        CREATE TRIGGER IF NOT EXISTS evicted_turns_fts_update AFTER UPDATE OF content ON evicted_turns BEGIN
            INSERT INTO evicted_turns_fts(evicted_turns_fts, rowid, content)
            VALUES ('delete', old.id, old.content);
            INSERT INTO evicted_turns_fts(rowid, content) VALUES (new.id, new.content);
        END;",
    )?;
    add_column_if_missing(conn, "evicted_turns", "source_id", "TEXT")?;
    add_column_if_missing(
        conn,
        "evicted_turns",
        "visibility",
        "TEXT NOT NULL DEFAULT 'privileged'",
    )?;
    add_column_if_missing(
        conn,
        "evicted_turns",
        "owner_principal",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    add_column_if_missing(
        conn,
        "evicted_turns",
        "owner_display_name",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    conn.execute(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_evicted_turns_source_id
         ON evicted_turns(source_id) WHERE source_id IS NOT NULL",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_evicted_turns_access
         ON evicted_turns(visibility, owner_principal, id DESC)",
        [],
    )?;
    Ok(())
}

pub(crate) fn migrate_memory_access_v1(conn: &Connection) -> Result<()> {
    let version = conn.query_row(
        "SELECT access_schema_version FROM memory_meta WHERE id=1",
        [],
        |row| row.get::<_, i64>(0),
    )?;
    if version >= 1 {
        return Ok(());
    }

    #[derive(Clone)]
    pub(crate) struct LegacyEpisode {
        id: i64,
        source_episode_ids: String,
        origin: MemoryOrigin,
    }

    let tx = conn.unchecked_transaction()?;
    let episodes = {
        let mut stmt = tx.prepare(
            "SELECT id, source_episode_ids,
                    origin_kind, origin_platform, origin_account_id,
                    origin_conversation_kind, origin_conversation_id, origin_sender_id,
                    origin_sender_display_name, origin_session_id, origin_message_id
               FROM episodes ORDER BY id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(LegacyEpisode {
                id: row.get(0)?,
                source_episode_ids: row.get(1)?,
                origin: MemoryOrigin {
                    kind: row.get(2)?,
                    platform: row.get(3)?,
                    account_id: row.get(4)?,
                    conversation_kind: row.get(5)?,
                    conversation_id: row.get(6)?,
                    sender_id: row.get(7)?,
                    sender_display_name: row.get(8)?,
                    session_id: row.get(9)?,
                    message_id: row.get(10)?,
                },
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };
    let mut ownerships = BTreeMap::<i64, MemoryOwnership>::new();
    for episode in &episodes {
        if let Some(ownership) = episode.origin.principal_ownership() {
            ownerships.insert(episode.id, ownership);
        } else if episode.origin.kind == "local" {
            ownerships.insert(episode.id, MemoryOwnership::privileged());
        }
    }
    for episode in &episodes {
        if ownerships.contains_key(&episode.id) {
            continue;
        }
        if let Some(ownership) = ownership_from_source_ids(&episode.source_episode_ids, &ownerships)
        {
            ownerships.insert(episode.id, ownership);
        }
    }
    for episode in &episodes {
        let ownership = ownerships
            .get(&episode.id)
            .cloned()
            .unwrap_or_else(MemoryOwnership::privileged);
        let subjects = ownership_subjects_json(&ownership);
        tx.execute(
            "UPDATE episodes SET visibility=?1, owner_principal=?2, owner_display_name=?3,
                                 subjects=?4
              WHERE id=?5",
            params![
                ownership.visibility,
                ownership.owner_principal,
                ownership.owner_display_name,
                subjects,
                episode.id,
            ],
        )?;
    }

    let facts = {
        let mut stmt = tx.prepare("SELECT id, source_episode_ids FROM facts ORDER BY id")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };
    for (id, source_ids) in facts {
        let ownership = ownership_from_source_ids(&source_ids, &ownerships)
            .unwrap_or_else(MemoryOwnership::privileged);
        let subjects = ownership_subjects_json(&ownership);
        tx.execute(
            "UPDATE facts SET visibility=?1, owner_principal=?2, owner_display_name=?3,
                              subjects=?4
              WHERE id=?5",
            params![
                ownership.visibility,
                ownership.owner_principal,
                ownership.owner_display_name,
                subjects,
                id,
            ],
        )?;
    }
    tx.execute(
        "UPDATE memory_meta SET access_schema_version=1 WHERE id=1",
        [],
    )?;
    tx.commit()?;
    Ok(())
}

pub(crate) fn migrate_memory_subjects_v2(conn: &Connection) -> Result<()> {
    let version = conn.query_row(
        "SELECT access_schema_version FROM memory_meta WHERE id=1",
        [],
        |row| row.get::<_, i64>(0),
    )?;
    if version >= 2 {
        return Ok(());
    }
    let tx = conn.unchecked_transaction()?;
    for table in ["facts", "episodes"] {
        let sql = format!(
            "SELECT id, visibility, owner_principal, owner_display_name
               FROM {table} WHERE subjects='[]' OR subjects=''"
        );
        let rows = {
            let mut stmt = tx.prepare(&sql)?;
            let mapped = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })?;
            mapped.collect::<std::result::Result<Vec<_>, _>>()?
        };
        let update = format!("UPDATE {table} SET subjects=?1 WHERE id=?2");
        for (id, visibility, owner_principal, owner_display_name) in rows {
            let ownership = MemoryOwnership {
                visibility: match visibility.as_str() {
                    VISIBILITY_PUBLIC => VISIBILITY_PUBLIC,
                    VISIBILITY_PRINCIPAL => VISIBILITY_PRINCIPAL,
                    _ => VISIBILITY_PRIVILEGED,
                },
                owner_principal,
                owner_display_name,
            };
            tx.execute(&update, params![ownership_subjects_json(&ownership), id])?;
        }
    }
    tx.execute(
        "UPDATE memory_meta SET access_schema_version=2 WHERE id=1",
        [],
    )?;
    tx.commit()?;
    Ok(())
}

pub(crate) fn ownership_from_source_ids(
    encoded: &str,
    ownerships: &BTreeMap<i64, MemoryOwnership>,
) -> Option<MemoryOwnership> {
    let ids = serde_json::from_str::<Vec<i64>>(encoded).ok()?;
    if ids.is_empty() {
        return None;
    }
    let mut principal: Option<MemoryOwnership> = None;
    for id in ids {
        let ownership = ownerships.get(&id)?;
        if ownership.visibility != VISIBILITY_PRINCIPAL {
            return Some(MemoryOwnership::privileged());
        }
        if principal
            .as_ref()
            .is_some_and(|existing| existing.owner_principal != ownership.owner_principal)
        {
            return Some(MemoryOwnership::privileged());
        }
        principal.get_or_insert_with(|| ownership.clone());
    }
    principal
}

pub(crate) fn add_column_if_missing(
    conn: &Connection,
    table: &str,
    column: &str,
    definition: &str,
) -> Result<()> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
    for row in rows {
        if row? == column {
            return Ok(());
        }
    }
    conn.execute(
        &format!("ALTER TABLE {table} ADD COLUMN {column} {definition}"),
        [],
    )?;
    Ok(())
}

pub(crate) fn decay_table(conn: &Connection, table: &str, config: &MemoryConfig) -> Result<()> {
    let now = Utc::now();
    let mut stmt = conn.prepare(&format!(
        "SELECT id, strength, COALESCE(last_recalled_at, updated_at, created_at), last_decay_at FROM {table} WHERE status='active'{}",
        if table == "episodes" { " AND retention='long_term'" } else { "" }
    ))?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, f64>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, Option<String>>(3)?,
        ))
    })?;
    let mut updates = Vec::new();
    for row in rows {
        let (id, strength, recalled_at, last_decay_at) = row?;
        let anchor = last_decay_at.as_deref().unwrap_or(&recalled_at);
        let Ok(anchor) = DateTime::parse_from_rfc3339(anchor) else {
            continue;
        };
        let days = (now - anchor.with_timezone(&Utc)).num_seconds().max(0) as f64 / 86_400.0;
        if days < 0.25 {
            continue;
        }
        let half_life = config.forgetting_half_life_days.max(0.1);
        let new_strength = strength * 2f64.powf(-days / half_life);
        let status = if new_strength < config.forgetting_min_strength {
            "forgotten"
        } else {
            "active"
        };
        updates.push((id, new_strength, status.to_string()));
    }
    drop(stmt);
    for (id, strength, status) in updates {
        conn.execute(
            &format!("UPDATE {table} SET strength=?1, status=?2, last_decay_at=?3 WHERE id=?4"),
            params![strength, status, now.to_rfc3339(), id],
        )?;
    }
    Ok(())
}

pub(crate) fn memory_hit_json(hit: &MemoryHit) -> Value {
    json!({
        "id": hit.id,
        "kind": match hit.kind { MemoryKind::Fact => "knowledge", MemoryKind::Diary => "diary" },
        "retention": hit.retention,
        "timestamp": hit.timestamp,
        "score": hit.score,
        "source": hit.source,
        "visibility": hit.visibility,
        "owner_principal": hit.owner_principal,
        "owner_display_name": truncate_chars(&compact_line(&hit.owner_display_name), 128),
        "subjects": serde_json::from_str::<Value>(&hit.subjects).unwrap_or_else(|_| json!([])),
        "content": hit.content,
    })
}

/// 渲染单条联想记忆行（含结尾换行），与注入块中的字节完全一致。
/// 整行同时充当跨回合去重键：内容或日期变化的记忆会渲染出不同的行，
/// 因而被视为新条目重新注入。
pub(crate) fn association_entry_line(hit: &MemoryHit, access: &MemoryAccess) -> String {
    let label = match (access, hit.visibility.as_str()) {
        (_, VISIBILITY_PUBLIC) => "公共知识".to_string(),
        (MemoryAccess::Privileged, VISIBILITY_PRINCIPAL) => format!(
            "归属={}{}",
            hit.owner_principal,
            if hit.owner_display_name.trim().is_empty() {
                String::new()
            } else {
                format!(
                    "，记录昵称={}",
                    truncate_chars(&compact_line(&hit.owner_display_name), 128)
                )
            }
        ),
        (MemoryAccess::Principal(_), VISIBILITY_PRINCIPAL) => "当前用户记忆".to_string(),
        _ => "仅管理员".to_string(),
    };
    let mut content = compact_line(&hit.content);
    // 短期日记正文自带 RFC3339 前缀（diary_content），加日期标签后去重
    if let Some(rest) = content
        .strip_prefix(hit.timestamp.as_str())
        .and_then(|rest| rest.strip_prefix('，'))
    {
        content = rest.to_string();
    }
    let date = association_date(&hit.timestamp);
    // organizer 写的日记常以「YYYY-MM-DD，」开头，与日期标签相同时也去重
    if let Some(date) = date.as_deref() {
        if let Some(rest) = content
            .strip_prefix(date)
            .and_then(|rest| rest.strip_prefix('，'))
        {
            content = rest.to_string();
        }
    }
    match date {
        Some(date) => format!("- [{date}] [{label}] {content}\n"),
        None => format!("- [{label}] {content}\n"),
    }
}

pub(crate) fn append_association_section<'a>(
    output: &mut String,
    title: &str,
    hits: impl IntoIterator<Item = &'a MemoryHit>,
    access: &MemoryAccess,
    max_chars: usize,
    closing: &str,
) {
    let heading = format!("\n{title}：\n");
    let mut section = String::new();
    for hit in hits {
        let line = association_entry_line(hit, access);
        let total = output.chars().count()
            + heading.chars().count()
            + section.chars().count()
            + line.chars().count()
            + closing.chars().count();
        if total <= max_chars {
            section.push_str(&line);
        }
    }
    if !section.is_empty() {
        output.push_str(&heading);
        output.push_str(&section);
    }
}

pub(crate) fn load_existing_memory_candidates(
    conn: &Connection,
    source_diaries: &[ShortDiaryRecord],
) -> Result<Vec<ExistingMemoryRecord>> {
    let mut allowed_principals = BTreeSet::new();
    let mut privileged_source = false;
    for diary in source_diaries {
        match diary.origin.principal_ownership() {
            Some(ownership) => {
                allowed_principals.insert(ownership.owner_principal);
            }
            None => privileged_source = true,
        }
    }
    let query = source_diaries
        .iter()
        .flat_map(|diary| [&diary.user_message, &diary.assistant_message])
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join("\n");
    let tokens = query_tokens_with_limit(&query, 256);
    let mut scored = Vec::<(f32, ExistingMemoryRecord)>::new();
    let mut facts = conn.prepare(
        "SELECT id, content, truth_status, visibility, owner_principal, owner_display_name FROM facts
         WHERE status!='forgotten' AND truth_status!='rejected'
         ORDER BY updated_at DESC LIMIT 5000",
    )?;
    let rows = facts.query_map([], |row| {
        Ok(ExistingMemoryRecord {
            id: row.get(0)?,
            kind: "knowledge".to_string(),
            content: row.get(1)?,
            truth_status: row.get(2)?,
            visibility: row.get(3)?,
            owner_principal: row.get(4)?,
            owner_display_name: row.get(5)?,
        })
    })?;
    for row in rows {
        let memory = row?;
        if !organizer_candidate_is_visible(&memory, &allowed_principals, privileged_source) {
            continue;
        }
        let score = score_text(&memory.content, "", &tokens);
        if score > 0.0 {
            scored.push((score, memory));
        }
    }
    drop(facts);

    let mut diaries = conn.prepare(
        "SELECT id, content, visibility, owner_principal, owner_display_name FROM episodes
         WHERE retention='long_term' AND status!='forgotten'
         ORDER BY updated_at DESC LIMIT 5000",
    )?;
    let rows = diaries.query_map([], |row| {
        Ok(ExistingMemoryRecord {
            id: row.get(0)?,
            kind: "long_diary".to_string(),
            content: row.get(1)?,
            truth_status: "accepted".to_string(),
            visibility: row.get(2)?,
            owner_principal: row.get(3)?,
            owner_display_name: row.get(4)?,
        })
    })?;
    for row in rows {
        let memory = row?;
        if !organizer_candidate_is_visible(&memory, &allowed_principals, privileged_source) {
            continue;
        }
        let score = score_text(&memory.content, "", &tokens);
        if score > 0.0 {
            scored.push((score, memory));
        }
    }
    scored.sort_by(|left, right| {
        right
            .0
            .partial_cmp(&left.0)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut fact_count = 0usize;
    let mut diary_count = 0usize;
    Ok(scored
        .into_iter()
        .filter_map(|(_, memory)| match memory.kind.as_str() {
            "knowledge" if fact_count < 30 => {
                fact_count += 1;
                Some(memory)
            }
            "long_diary" if diary_count < 20 => {
                diary_count += 1;
                Some(memory)
            }
            _ => None,
        })
        .collect())
}

pub(crate) fn organizer_candidate_is_visible(
    memory: &ExistingMemoryRecord,
    allowed_principals: &BTreeSet<String>,
    privileged_source: bool,
) -> bool {
    match memory.visibility.as_str() {
        VISIBILITY_PUBLIC => true,
        VISIBILITY_PRINCIPAL => allowed_principals.contains(&memory.owner_principal),
        VISIBILITY_PRIVILEGED => privileged_source,
        _ => false,
    }
}

pub(crate) fn validate_knowledge_action(
    action: &KnowledgeAction,
    diary_ids: &BTreeSet<i64>,
    candidate_fact_ids: &BTreeSet<i64>,
) -> Result<()> {
    if !matches!(action.operation.as_str(), "create" | "update") {
        bail!("invalid knowledge operation");
    }
    if action.operation == "update"
        && !action
            .target_id
            .is_some_and(|id| candidate_fact_ids.contains(&id))
    {
        bail!("knowledge update target is not an allowed candidate");
    }
    if action.operation == "create" && action.target_id.is_some() {
        bail!("new knowledge must not have a target id");
    }
    if !matches!(
        action.memory_type.as_str(),
        "fact" | "preference" | "relationship" | "task" | "self" | "other"
    ) {
        bail!("invalid knowledge type");
    }
    if !matches!(
        action.truth_status.as_str(),
        "accepted" | "reported" | "uncertain" | "fictional" | "rejected"
    ) {
        bail!("invalid knowledge truth status");
    }
    validate_organized_content(&action.content, 2_000)?;
    validate_evidence_ids(&action.diary_ids, diary_ids)?;
    if !(1..=5).contains(&action.importance)
        || !action.confidence.is_finite()
        || !(0.0..=1.0).contains(&action.confidence)
    {
        bail!("knowledge importance or confidence is out of range");
    }
    Ok(())
}

pub(crate) fn validate_knowledge_visibility(
    batch: &OrganizationBatch,
    action: &KnowledgeAction,
) -> Result<()> {
    if !matches!(
        action.visibility.as_str(),
        "" | VISIBILITY_PUBLIC | VISIBILITY_PRINCIPAL | VISIBILITY_PRIVILEGED
    ) {
        bail!("invalid knowledge visibility");
    }
    let target_visibility = action.target_id.and_then(|target_id| {
        batch
            .existing
            .iter()
            .find(|memory| memory.kind == "knowledge" && memory.id == target_id)
            .map(|memory| memory.visibility.as_str())
    });
    if target_visibility
        .is_some_and(|target| !action.visibility.is_empty() && action.visibility != target)
    {
        bail!("knowledge updates cannot change memory visibility");
    }
    let effective_visibility = target_visibility.unwrap_or(action.visibility.as_str());
    if effective_visibility == VISIBILITY_PUBLIC && action.memory_type != "fact" {
        bail!("only general facts may become public memories");
    }
    validate_memory_subjects(batch, &action.diary_ids, &action.subjects)?;
    if effective_visibility == VISIBILITY_PUBLIC {
        if !action.subjects.is_empty() {
            bail!("public memories cannot contain person subjects");
        }
        let content = action.content.to_lowercase();
        for diary in batch
            .diaries
            .iter()
            .filter(|diary| action.diary_ids.contains(&diary.id))
        {
            for marker in [
                diary.origin.sender_id.trim(),
                diary.origin.sender_display_name.trim(),
            ] {
                if marker.chars().count() >= 2 && content.contains(&marker.to_lowercase()) {
                    bail!("public memory content contains a source identity marker");
                }
            }
        }
    }
    Ok(())
}

pub(crate) fn validate_knowledge_update_scope(
    batch: &OrganizationBatch,
    action: &KnowledgeAction,
    candidates: &BTreeMap<i64, &ExistingMemoryRecord>,
) -> Result<()> {
    let Some(target_id) = action.target_id else {
        return Ok(());
    };
    let target = candidates
        .get(&target_id)
        .context("knowledge update target disappeared from candidates")?;
    let evidence = diary_ownership(batch, &action.diary_ids);
    let allowed = match target.visibility.as_str() {
        VISIBILITY_PUBLIC => true,
        VISIBILITY_PRINCIPAL => {
            evidence.visibility == VISIBILITY_PRINCIPAL
                && evidence.owner_principal == target.owner_principal
        }
        VISIBILITY_PRIVILEGED => evidence.visibility == VISIBILITY_PRIVILEGED,
        _ => false,
    };
    if !allowed {
        bail!("knowledge update evidence belongs to a different principal");
    }
    Ok(())
}

pub(crate) fn validate_long_diary(
    batch: &OrganizationBatch,
    diary: &LongDiaryDraft,
    diary_ids: &BTreeSet<i64>,
) -> Result<()> {
    validate_organized_content(&diary.content, 3_000)?;
    validate_evidence_ids(&diary.diary_ids, diary_ids)?;
    if !(1..=5).contains(&diary.importance)
        || !diary.confidence.is_finite()
        || !(0.0..=1.0).contains(&diary.confidence)
    {
        bail!("long diary importance or confidence is out of range");
    }
    if !matches!(
        diary.visibility.as_str(),
        "" | VISIBILITY_PRINCIPAL | VISIBILITY_PRIVILEGED
    ) {
        bail!("long diaries cannot be public memories");
    }
    validate_memory_subjects(batch, &diary.diary_ids, &diary.subjects)?;
    Ok(())
}

pub(crate) fn validate_memory_subjects(
    batch: &OrganizationBatch,
    diary_ids: &[i64],
    subjects: &[MemorySubject],
) -> Result<()> {
    if subjects.len() > 32 {
        bail!("organized memory contains too many subjects");
    }
    let allowed_principals = batch
        .diaries
        .iter()
        .filter(|diary| diary_ids.contains(&diary.id))
        .filter_map(|diary| diary.owner_principal.as_deref())
        .collect::<BTreeSet<_>>();
    for subject in subjects {
        let principal = subject
            .principal
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let name = subject
            .name
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());
        if principal.is_none() && name.is_none() {
            bail!("memory subject must contain a principal or name");
        }
        if principal.is_some_and(|value| !allowed_principals.contains(value)) {
            bail!("memory subject references an untrusted principal");
        }
        if name
            .is_some_and(|value| value.chars().count() > 128 || value.chars().any(char::is_control))
        {
            bail!("memory subject name is invalid");
        }
    }
    Ok(())
}

pub(crate) fn knowledge_ownership(
    batch: &OrganizationBatch,
    action: &KnowledgeAction,
) -> MemoryOwnership {
    if let Some(target) = action.target_id.and_then(|target| {
        batch
            .existing
            .iter()
            .find(|memory| memory.kind == "knowledge" && memory.id == target)
    }) {
        return MemoryOwnership {
            visibility: match target.visibility.as_str() {
                VISIBILITY_PUBLIC => VISIBILITY_PUBLIC,
                VISIBILITY_PRINCIPAL => VISIBILITY_PRINCIPAL,
                _ => VISIBILITY_PRIVILEGED,
            },
            owner_principal: target.owner_principal.clone(),
            owner_display_name: target.owner_display_name.clone(),
        };
    }
    if action.visibility == VISIBILITY_PUBLIC && action.memory_type == "fact" {
        return MemoryOwnership::public();
    }
    diary_ownership(batch, &action.diary_ids)
}

pub(crate) fn diary_ownership(batch: &OrganizationBatch, diary_ids: &[i64]) -> MemoryOwnership {
    let mut principals = BTreeMap::<String, String>::new();
    let mut privileged_source = false;
    for id in diary_ids {
        let Some(diary) = batch.diaries.iter().find(|diary| diary.id == *id) else {
            privileged_source = true;
            continue;
        };
        match diary.origin.principal_ownership() {
            Some(ownership) => {
                principals
                    .entry(ownership.owner_principal)
                    .or_insert(ownership.owner_display_name);
            }
            None => privileged_source = true,
        }
    }
    if !privileged_source && principals.len() == 1 {
        let (principal, display_name) = principals
            .into_iter()
            .next()
            .expect("one principal was checked");
        MemoryOwnership::principal(principal, display_name)
    } else {
        MemoryOwnership::privileged()
    }
}

pub(crate) fn validate_organized_content(content: &str, max_chars: usize) -> Result<()> {
    let content = content.trim();
    if content.is_empty() || content.chars().count() > max_chars || content.contains('\0') {
        bail!("organized memory content is empty or too long");
    }
    Ok(())
}

pub(crate) fn validate_evidence_ids(ids: &[i64], allowed: &BTreeSet<i64>) -> Result<()> {
    if ids.is_empty() || ids.iter().any(|id| !allowed.contains(id)) {
        bail!("organized memory references invalid diary ids");
    }
    Ok(())
}

pub(crate) fn normalized_ids_json(ids: &[i64]) -> String {
    serde_json::to_string(&ids.iter().copied().collect::<BTreeSet<_>>()).unwrap_or("[]".to_string())
}

pub(crate) fn ownership_subjects_json(ownership: &MemoryOwnership) -> String {
    if ownership.visibility != VISIBILITY_PRINCIPAL {
        return "[]".to_string();
    }
    serde_json::to_string(&[MemorySubject {
        principal: Some(ownership.owner_principal.clone()),
        name: (!ownership.owner_display_name.trim().is_empty())
            .then(|| truncate_chars(&compact_line(&ownership.owner_display_name), 128)),
    }])
    .unwrap_or_else(|_| "[]".to_string())
}

pub(crate) fn organized_subjects_json(
    batch: &OrganizationBatch,
    diary_ids: &[i64],
    declared: &[MemorySubject],
    ownership: &MemoryOwnership,
) -> String {
    if ownership.visibility == VISIBILITY_PUBLIC {
        return "[]".to_string();
    }
    let mut subjects = declared
        .iter()
        .map(|subject| MemorySubject {
            principal: subject
                .principal
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string),
            name: subject
                .name
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string),
        })
        .collect::<BTreeSet<_>>();
    for diary in batch
        .diaries
        .iter()
        .filter(|diary| diary_ids.contains(&diary.id))
    {
        if let Some(principal) = diary.owner_principal.as_ref() {
            subjects.insert(MemorySubject {
                principal: Some(principal.clone()),
                name: (!diary.origin.sender_display_name.trim().is_empty())
                    .then(|| truncate_chars(&compact_line(&diary.origin.sender_display_name), 128)),
            });
        }
    }
    serde_json::to_string(&subjects).unwrap_or_else(|_| "[]".to_string())
}

pub(crate) fn normalized_tags_json(tags: &[String]) -> String {
    let tags = tags
        .iter()
        .map(|tag| compact_line(tag))
        .filter(|tag| !tag.is_empty() && tag.chars().count() <= 32)
        .take(8)
        .collect::<BTreeSet<_>>();
    serde_json::to_string(&tags).unwrap_or("[]".to_string())
}

pub(crate) fn sort_json_hits(hits: &mut [Value]) {
    hits.sort_by(|a, b| {
        b.get("score")
            .and_then(Value::as_f64)
            .unwrap_or_default()
            .partial_cmp(&a.get("score").and_then(Value::as_f64).unwrap_or_default())
            .unwrap_or(std::cmp::Ordering::Equal)
    });
}

/// FTS5 terms are OR-ed: a paraphrase usually shares only part of its wording
/// with the record, and requiring every term would push recall to zero on the
/// exact queries this is for.
/// Keyword hits at or above this are already good enough that the embedding
/// round trip would only add latency.
pub(crate) const SEMANTIC_SKIP_SCORE: f64 = 40.0;
/// Rows embedded per search; the backlog fills in over successive calls rather
/// than making one unlucky search pay for the whole archive.
pub(crate) const SEMANTIC_EMBED_BATCH: usize = 64;
pub(crate) const SEMANTIC_CORPUS_LIMIT: usize = 500;
/// Semantic hits are supporting evidence, not the primary ranking; keyword
/// scores run an order of magnitude higher and should keep the top slots when
/// they matched at all.
pub(crate) const SEMANTIC_SCORE_WEIGHT: f32 = 30.0;

pub(crate) fn cosine_similarity(left: &[f32], right: &[f32]) -> f32 {
    if left.len() != right.len() || left.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0;
    let mut left_norm = 0.0;
    let mut right_norm = 0.0;
    for (a, b) in left.iter().zip(right.iter()) {
        dot += a * b;
        left_norm += a * a;
        right_norm += b * b;
    }
    if left_norm == 0.0 || right_norm == 0.0 {
        0.0
    } else {
        dot / (left_norm.sqrt() * right_norm.sqrt())
    }
}

/// Semantic hits reinforce a record the keywords already found rather than
/// displacing it; a record only the embedding saw joins on its own.
pub(crate) fn merge_evicted_hits(base: &mut Value, semantic: Vec<Value>, limit: usize) {
    let Some(hits) = base["results"].as_array_mut() else {
        return;
    };
    for item in semantic {
        let id = item["id"].clone();
        if let Some(existing) = hits.iter_mut().find(|hit| hit["id"] == id) {
            let boost = item["score"].as_f64().unwrap_or(0.0) * 0.6;
            let score = existing["score"].as_f64().unwrap_or(0.0) + boost;
            existing["score"] = json!(score);
            existing["semantic"] = json!(true);
        } else {
            hits.push(item);
        }
    }
    sort_json_hits(hits);
    hits.truncate(limit);
}

pub(crate) fn build_evicted_fts_query(terms: &[String]) -> String {
    terms
        .iter()
        .map(|term| format!("\"{}\"", term.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" OR ")
}

/// `normalized_query` 需已 `compact_line` + 小写化:归一化与被打分的行无关,
/// 调用方在循环外做一次,而不是在每一行上重复三次分配。
pub(crate) fn score_text(text: &str, normalized_query: &str, tokens: &[String]) -> f32 {
    if tokens.is_empty() {
        return 0.0;
    }
    let lower = text.to_ascii_lowercase();
    let mut score = 0.0;
    let mut matched = HashSet::new();
    for token in tokens {
        if lower.contains(token) {
            score += 8.0 + token.chars().count().min(8) as f32;
            matched.insert(token);
        }
    }
    if !normalized_query.is_empty() && lower.contains(normalized_query) {
        score += 20.0;
    }
    score + matched.len() as f32 / tokens.len() as f32 * 24.0
}

pub(crate) fn query_tokens(query: &str) -> Vec<String> {
    query_tokens_with_limit(query, 64)
}

pub(crate) fn query_tokens_with_limit(query: &str, limit: usize) -> Vec<String> {
    let mut tokens = BTreeSet::new();
    for token in JIEBA.cut(query) {
        let token = token.trim().to_ascii_lowercase();
        if token.is_empty()
            || !token
                .chars()
                .any(|character| character.is_alphanumeric() || !character.is_ascii())
        {
            continue;
        }
        let chars = token.chars().count();
        if chars >= 2 || (chars == 1 && !token.is_ascii()) {
            tokens.insert(token);
        }
    }
    for token in
        query.split(|character: char| character.is_whitespace() || character.is_ascii_punctuation())
    {
        let token = token.trim().to_ascii_lowercase();
        if token.chars().count() >= 2 {
            tokens.insert(token);
        }
    }
    tokens.into_iter().take(limit).collect()
}

pub(crate) fn snippet(text: &str, tokens: &[String], max_chars: usize) -> String {
    let lower = text.to_ascii_lowercase();
    let start = tokens
        .iter()
        .filter_map(|token| lower.find(token))
        .min()
        .unwrap_or(0);
    let start = text[..start.min(text.len())]
        .char_indices()
        .rev()
        .nth(max_chars / 4)
        .map(|(index, _)| index)
        .unwrap_or(0);
    truncate_chars(&text[start..], max_chars)
}

pub(crate) fn compact_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(crate) fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    format!(
        "{}...",
        text.chars()
            .take(max_chars.saturating_sub(3))
            .collect::<String>()
    )
}

pub(crate) fn count_rows(conn: &Connection, table: &str) -> Result<i64> {
    let sql = format!("SELECT COUNT(*) FROM {table}");
    Ok(conn.query_row(&sql, [], |row| row.get(0))?)
}

pub(crate) fn count_where(conn: &Connection, table: &str, condition: &str) -> Result<i64> {
    let sql = format!("SELECT COUNT(*) FROM {table} WHERE {condition}");
    Ok(conn.query_row(&sql, [], |row| row.get(0))?)
}

pub(crate) fn count_skill_dirs(skills_dir: &PathBuf) -> Result<usize> {
    if !skills_dir.exists() {
        return Ok(0);
    }
    let mut count = 0usize;
    for entry in std::fs::read_dir(skills_dir)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() && entry.path().join("SKILL.md").is_file() {
            count += 1;
        }
    }
    Ok(count)
}

pub(crate) fn now() -> String {
    Utc::now().to_rfc3339()
}

/// RFC3339 时间戳 → 本地日期（用于关联记忆展示；解析失败返回 None）
pub(crate) fn association_date(timestamp: &str) -> Option<String> {
    DateTime::parse_from_rfc3339(timestamp).ok().map(|value| {
        value
            .with_timezone(&chrono::Local)
            .format("%Y-%m-%d")
            .to_string()
    })
}

pub(crate) fn diary_content(
    created_at: &str,
    user_message: &str,
    assistant_message: &str,
) -> String {
    // 第一人称的互动记忆,不是工单:归属(谁说的)由注入行的 [归属=…] 标签
    // 承担,昵称是可改的不可信字段,不进正文。
    format!(
        "{}，对方说：{}；我回：{}",
        created_at,
        truncate_chars(&compact_line(user_message), 260),
        truncate_chars(&compact_line(assistant_message), 520)
    )
}
