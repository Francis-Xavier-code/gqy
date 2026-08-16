//! store — 自 src/tools/memes.rs 拆分。

pub(crate) use super::*;

pub(crate) fn load_library(paths: &GQYPaths, library: &str) -> Result<Vec<LoadedMeme>> {
    let builtin_dir = builtin_library_dir(library);
    let user_dir = user_library_dir(paths, library);
    let builtin_index = builtin_dir.join("index.json");
    let user_index = user_dir.join("index.json");
    let key = MemeLibraryCacheKey {
        library: sanitize_library(library),
        builtin_mtime: index_mtime(&builtin_index),
        user_mtime: index_mtime(&user_index),
        builtin_index: builtin_index.clone(),
        user_index: user_index.clone(),
    };
    let cache = MEME_LIBRARY_CACHE.get_or_init(|| RwLock::new(None));
    if let Some(cached) = cache.read().unwrap().as_ref() {
        if cached.key == key {
            return Ok(cached.memes.clone());
        }
    }
    let builtin = load_index(&builtin_index)?.unwrap_or_default();
    let user = load_index(&user_index)?.unwrap_or_default();
    let disabled = user.disabled_ids;
    let mut user_ids = Vec::new();
    let mut result = Vec::new();
    for item in user.memes {
        if disabled.iter().any(|id| ids_match(id, &item.id)) {
            continue;
        }
        user_ids.push(item.id.clone());
        result.push(LoadedMeme {
            path: user_dir.join(&item.file),
            item,
            source: MemeSource::User,
        });
    }
    for item in builtin.memes {
        if disabled.iter().any(|id| ids_match(id, &item.id))
            || user_ids.iter().any(|id| ids_match(id, &item.id))
        {
            continue;
        }
        result.push(LoadedMeme {
            path: builtin_dir.join(&item.file),
            item,
            source: MemeSource::Builtin,
        });
    }
    *cache.write().unwrap() = Some(MemeLibraryCache {
        key,
        memes: result.clone(),
    });
    Ok(result)
}

fn index_mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
}

pub(crate) fn find_meme(paths: &GQYPaths, library: &str, id: &str) -> Result<Option<LoadedMeme>> {
    find_meme_in(load_library(paths, library)?, id)
}

pub(crate) fn find_meme_in(memes: Vec<LoadedMeme>, id: &str) -> Result<Option<LoadedMeme>> {
    let requested = id_hash_part(id);
    if requested.is_empty() {
        return Ok(None);
    }
    if !is_full_hash(requested) && requested.len() < MIN_SHORT_MEME_ID_LEN {
        bail!(
            "meme id prefix is too short: {requested}; use at least {MIN_SHORT_MEME_ID_LEN} hex characters"
        );
    }
    let mut matches = memes
        .into_iter()
        .filter(|meme| id_hash_part(&meme.item.id).starts_with(requested))
        .collect::<Vec<_>>();
    match matches.len() {
        0 => Ok(None),
        1 => Ok(matches.pop()),
        _ => bail!("meme id prefix is ambiguous: {requested}; use a longer id"),
    }
}

pub(crate) fn is_full_hash(value: &str) -> bool {
    value.len() == 64 && value.chars().all(|ch| ch.is_ascii_hexdigit())
}

pub(crate) fn meme_ids(memes: &[LoadedMeme]) -> Vec<String> {
    memes.iter().map(|meme| meme.item.id.clone()).collect()
}

pub(crate) fn id_hash_part(value: &str) -> &str {
    let value = value.trim();
    value.strip_prefix("sha256:").unwrap_or(value)
}

pub(crate) fn ids_match(stored: &str, requested: &str) -> bool {
    let stored = id_hash_part(stored);
    let requested = id_hash_part(requested);
    !requested.is_empty() && stored.starts_with(requested)
}

pub(crate) fn unique_short_id_from_ids(ids: &[String], id: &str) -> String {
    let hash = id_hash_part(id);
    if hash.len() <= MIN_SHORT_MEME_ID_LEN {
        return hash.to_string();
    }
    for len in MIN_SHORT_MEME_ID_LEN..=hash.len() {
        let prefix = &hash[..len];
        let matches = ids
            .iter()
            .filter(|candidate| id_hash_part(candidate).starts_with(prefix))
            .count();
        if matches <= 1 {
            return prefix.to_string();
        }
    }
    hash.to_string()
}

pub(crate) fn load_index(path: &Path) -> Result<Option<MemeIndex>> {
    if !path.is_file() {
        return Ok(None);
    }
    Ok(Some(serde_json::from_str(&std::fs::read_to_string(path)?)?))
}

pub(crate) fn save_index(path: &Path, index: &MemeIndex) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
        let mut temp = tempfile::NamedTempFile::new_in(parent)?;
        serde_json::to_writer_pretty(&mut temp, index)?;
        temp.write_all(b"\n")?;
        temp.as_file().sync_all()?;
        temp.persist(path)
            .map_err(|error| error.error)
            .with_context(|| format!("atomically replacing meme index {}", path.display()))?;
        return Ok(());
    }
    bail!("meme index path has no parent: {}", path.display())
}

pub(crate) fn selected_library(args: &Value, config: &AppConfig) -> String {
    args.get("library")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(sanitize_library)
        .unwrap_or_else(|| current_persona_library(config))
}

pub(crate) fn current_persona_library(config: &AppConfig) -> String {
    sanitize_library(
        &config
            .plugins
            .memes
            .library_for_persona(&config.prompt.active_persona),
    )
}

pub(crate) fn meme_ref_exists(paths: &GQYPaths, meme: &MemeRef) -> Result<bool> {
    Ok(find_meme(paths, &meme.library, &meme.id)?.is_some())
}

pub(crate) async fn delete_meme_reference(
    meme: &MemeRef,
    config: &AppConfig,
    paths: &GQYPaths,
) -> Result<()> {
    let result = delete_meme(
        json!({
            "library": meme.library,
            "id": meme.id,
            "hard_delete": false,
        }),
        config,
        paths,
    )
    .await?;
    let result: Value = serde_json::from_str(&result)?;
    if result.get("success").and_then(Value::as_bool) == Some(true) {
        Ok(())
    } else {
        bail!("meme deletion did not succeed")
    }
}

pub(crate) fn library_lock(library: &str) -> Arc<tokio::sync::Mutex<()>> {
    let key = sanitize_library(library);
    let mut locks = MEME_LIBRARY_LOCKS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap();
    locks
        .entry(key)
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone()
}

pub(crate) fn sanitize_library(value: &str) -> String {
    let value = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string();
    if value.is_empty() {
        "default".to_string()
    } else {
        value
    }
}

fn builtin_library_dir(library: &str) -> PathBuf {
    if let Some(path) = std::env::var_os("GQY_MEMES_DIR") {
        return PathBuf::from(path).join(library);
    }
    let dev = PathBuf::from("src/memes").join(library);
    if dev.is_dir() {
        return dev;
    }
    // 可执行文件相对解析,与渲染字体一致:brew/手动安装的
    // <prefix>/share/gqy/memes。/usr/share/gqy 为系统级兜底。
    if let Ok(executable) = std::env::current_exe() {
        if let Some(prefix) = executable.parent().and_then(std::path::Path::parent) {
            let rel = prefix.join("share/gqy/memes").join(library);
            if rel.is_dir() {
                return rel;
            }
        }
    }
    PathBuf::from(BUILTIN_MEMES_DIR).join(library)
}

pub(crate) fn user_library_dir(paths: &GQYPaths, library: &str) -> PathBuf {
    paths.data_dir.join("memes").join(sanitize_library(library))
}

pub(crate) fn expand_path(value: &str) -> PathBuf {
    if let Some(rest) = value.trim().strip_prefix("~/") {
        if let Some(home) = directories::BaseDirs::new().map(|dirs| dirs.home_dir().to_path_buf()) {
            return home.join(rest);
        }
    }
    let path = Path::new(value.trim());
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        crate::tools::workspace::effective_workdir().join(path)
    }
}
