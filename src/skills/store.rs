//! store — 自 src/skills.rs 拆分。

pub(crate) use super::*;

pub(crate) fn skill_roots(config: &AppConfig, paths: &GQYPaths) -> Vec<(PathBuf, SkillSource)> {
    vec![
        (
            config.active_persona_skills_dir(paths),
            SkillSource::Persona,
        ),
        (paths.skills_dir.clone(), SkillSource::Global),
    ]
}

pub(crate) fn sorted_skill_directories(root: &Path) -> Result<Vec<PathBuf>> {
    let metadata = match fs::symlink_metadata(root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    if metadata.file_type().is_symlink() {
        bail!("skill root must not be a symbolic link: {}", root.display());
    }
    if !metadata.is_dir() {
        bail!("skill root is not a directory: {}", root.display());
    }
    let mut directories = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() && !entry.file_name().to_string_lossy().starts_with('.') {
            if directories.len() >= MAX_SKILL_ROOT_DIRECTORIES {
                bail!("skill root exceeds the {MAX_SKILL_ROOT_DIRECTORIES} directory-entry limit");
            }
            directories.push(entry.path());
        }
    }
    directories.sort();
    Ok(directories)
}

pub(crate) fn hash_metadata(hasher: &mut blake3::Hasher, path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            hasher.update(&[1]);
            hasher.update(&metadata.len().to_le_bytes());
            if let Ok(modified) = metadata.modified().and_then(|time| {
                time.duration_since(UNIX_EPOCH)
                    .map_err(std::io::Error::other)
            }) {
                hasher.update(&modified.as_secs().to_le_bytes());
                hasher.update(&modified.subsec_nanos().to_le_bytes());
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                hasher.update(&metadata.ino().to_le_bytes());
                hasher.update(&metadata.ctime().to_le_bytes());
                hasher.update(&metadata.ctime_nsec().to_le_bytes());
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            hasher.update(&[0]);
        }
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

pub(crate) fn persona_scope(config: &AppConfig, scope: SkillScope) -> Option<String> {
    (scope == SkillScope::Persona).then(|| config.active_persona_scope())
}

pub(crate) fn target_path(
    paths: &GQYPaths,
    name: &str,
    scope: SkillScope,
    persona: Option<&str>,
) -> Result<PathBuf> {
    validate_skill_name(name)?;
    match scope {
        SkillScope::Global => {
            if persona.is_some() {
                bail!("global skill drafts may not contain a persona scope");
            }
            if name == "personas" {
                bail!("personas is reserved for persona-scoped skills");
            }
            Ok(paths.skills_dir.join(name))
        }
        SkillScope::Persona => {
            let persona = persona.context("persona skill draft is missing its persona scope")?;
            validate_persona_scope(persona)?;
            Ok(paths.skills_dir.join("personas").join(persona).join(name))
        }
    }
}

pub(crate) fn validate_persona_scope(scope: &str) -> Result<()> {
    if scope.is_empty()
        || scope.len() > 64
        || scope == "."
        || scope == ".."
        || scope != persona_scope_name(scope)
        || !scope
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        bail!("invalid persona skill scope");
    }
    Ok(())
}

pub(crate) fn new_manifest(
    name: &str,
    scope: SkillScope,
    persona_scope: Option<String>,
    kind: DraftKind,
    base_revision: Option<String>,
) -> DraftManifest {
    DraftManifest {
        version: DRAFT_VERSION,
        id: format!("draft-{:032x}", rand::random::<u128>()),
        name: name.to_string(),
        scope,
        persona_scope,
        kind,
        base_revision,
        created_at: unix_time(SystemTime::now()),
    }
}

pub(crate) fn create_empty_draft(paths: &GQYPaths, manifest: &DraftManifest) -> Result<PathBuf> {
    let root = paths.skill_drafts_dir();
    create_private_dir(&root)?;
    let draft = root.join(&manifest.id);
    fs::create_dir(&draft)?;
    let result = (|| {
        secure_directory(&draft)?;
        let package = draft.join(DRAFT_PACKAGE_DIR);
        fs::create_dir(&package)?;
        secure_directory(&package)?;
        Ok(package)
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&draft);
    }
    result
}

pub(crate) fn write_draft_manifest(paths: &GQYPaths, manifest: &DraftManifest) -> Result<()> {
    let draft = paths.skill_drafts_dir().join(&manifest.id);
    write_private_file(
        &draft.join(DRAFT_MANIFEST),
        format!("{}\n", serde_json::to_string_pretty(manifest)?).as_bytes(),
    )
}

pub(crate) fn cleanup_failed_draft<T>(
    paths: &GQYPaths,
    manifest: &DraftManifest,
    result: Result<T>,
) -> Result<T> {
    if result.is_err() {
        let _ = fs::remove_dir_all(paths.skill_drafts_dir().join(&manifest.id));
    }
    result
}

pub(crate) fn draft_public(paths: &GQYPaths, manifest: &DraftManifest) -> Result<SkillDraft> {
    let skill_dir = paths
        .skill_drafts_dir()
        .join(&manifest.id)
        .join(DRAFT_PACKAGE_DIR)
        .join(&manifest.name);
    Ok(SkillDraft {
        id: manifest.id.clone(),
        name: manifest.name.clone(),
        scope: manifest.scope.as_str().to_string(),
        persona_scope: manifest.persona_scope.clone(),
        kind: match manifest.kind {
            DraftKind::Create => "create",
            DraftKind::Update => "update",
        }
        .to_string(),
        skill_file: skill_dir.join("SKILL.md").display().to_string(),
        skill_dir: skill_dir.display().to_string(),
        base_revision: manifest.base_revision.clone(),
        created_at: manifest.created_at,
        last_modified_at: unix_time(latest_modified(&skill_dir).unwrap_or(UNIX_EPOCH)),
    })
}

pub(crate) fn read_manifest(paths: &GQYPaths, draft_id: &str) -> Result<DraftManifest> {
    validate_draft_id(draft_id)?;
    let draft = paths.skill_drafts_dir().join(draft_id);
    let metadata = fs::symlink_metadata(&draft)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!("skill draft root must be a regular directory");
    }
    let path = draft.join(DRAFT_MANIFEST);
    let metadata = fs::symlink_metadata(&path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("skill draft manifest must be a regular file");
    }
    if metadata.len() > MAX_DRAFT_MANIFEST_BYTES {
        bail!("skill draft manifest exceeds its size limit");
    }
    let manifest: DraftManifest = serde_json::from_slice(&fs::read(&path)?)
        .with_context(|| format!("parsing {}", path.display()))?;
    if manifest.version != DRAFT_VERSION || manifest.id != draft_id {
        bail!("unsupported or mismatched skill draft manifest");
    }
    validate_skill_name(&manifest.name)?;
    target_path(
        paths,
        &manifest.name,
        manifest.scope,
        manifest.persona_scope.as_deref(),
    )?;
    match manifest.kind {
        DraftKind::Create if manifest.base_revision.is_some() => {
            bail!("create draft must not contain a base revision")
        }
        DraftKind::Update => {
            let revision = manifest
                .base_revision
                .as_deref()
                .context("update draft is missing its base revision")?;
            if revision.len() != 64 || !revision.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                bail!("invalid update draft base revision");
            }
        }
        DraftKind::Create => {}
    }
    let now = unix_time(SystemTime::now());
    if manifest.created_at > now.saturating_add(300) {
        bail!("skill draft creation timestamp is in the future");
    }
    Ok(manifest)
}

pub(crate) fn validate_draft_id(id: &str) -> Result<()> {
    let mut components = Path::new(id).components();
    if !id.starts_with("draft-")
        || !matches!(components.next(), Some(Component::Normal(_)))
        || components.next().is_some()
        || !id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '-')
    {
        bail!("invalid skill draft id");
    }
    Ok(())
}

pub(crate) fn validate_skill_package(root: &Path, expected_name: &str) -> Result<()> {
    let metadata = fs::symlink_metadata(root)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!("skill package root must be a regular directory");
    }
    let raw = read_skill_file(&root.join("SKILL.md"))?;
    parse_skill_metadata(&raw, Some(expected_name))?;
    let mut stats = PackageStats {
        directories: 1,
        ..PackageStats::default()
    };
    validate_package_tree(root, 0, &mut stats)?;
    Ok(())
}

#[derive(Default)]
pub(crate) struct PackageStats {
    files: usize,
    directories: usize,
    bytes: u64,
}

pub(crate) fn validate_package_tree(
    path: &Path,
    depth: usize,
    stats: &mut PackageStats,
) -> Result<()> {
    if depth > MAX_SKILL_PACKAGE_DEPTH {
        bail!("skill package exceeds the directory depth limit");
    }
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let metadata = fs::symlink_metadata(entry.path())?;
        if metadata.file_type().is_symlink() {
            bail!(
                "skill packages may not contain symbolic links: {}",
                entry.path().display()
            );
        }
        if metadata.is_dir() {
            stats.directories += 1;
            if stats.directories > MAX_SKILL_PACKAGE_DIRS {
                bail!("skill package exceeds the directory-count limit");
            }
            validate_package_tree(&entry.path(), depth + 1, stats)?;
        } else if metadata.is_file() {
            stats.files += 1;
            stats.bytes = stats
                .bytes
                .checked_add(metadata.len())
                .context("skill package size overflow")?;
            if stats.files > MAX_SKILL_PACKAGE_FILES || stats.bytes > MAX_SKILL_PACKAGE_BYTES {
                bail!("skill package exceeds file-count or total-size limits");
            }
        } else {
            bail!(
                "skill package contains an unsupported file type: {}",
                entry.path().display()
            );
        }
    }
    Ok(())
}

pub(crate) fn skill_revision(root: &Path) -> Result<String> {
    let mut entries = Vec::new();
    let mut stats = PackageStats {
        directories: 1,
        ..PackageStats::default()
    };
    collect_revision_entries(root, root, 0, &mut stats, &mut entries)?;
    entries.sort_by(|left, right| left.relative.cmp(&right.relative));
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0u8; 64 * 1024];
    for entry in entries {
        hasher.update(&[entry.kind]);
        hash_length_prefixed(&mut hasher, entry.relative.as_os_str().as_encoded_bytes());
        hasher.update(&entry.mode.to_le_bytes());
        hasher.update(&entry.length.to_le_bytes());
        if entry.kind == b'f' {
            let mut file = File::open(entry.path)?;
            loop {
                let read = file.read(&mut buffer)?;
                if read == 0 {
                    break;
                }
                hasher.update(&buffer[..read]);
            }
        }
    }
    Ok(hasher.finalize().to_hex().to_string())
}

pub(crate) struct RevisionEntry {
    relative: PathBuf,
    path: PathBuf,
    kind: u8,
    mode: u32,
    length: u64,
}

pub(crate) fn collect_revision_entries(
    root: &Path,
    path: &Path,
    depth: usize,
    stats: &mut PackageStats,
    entries: &mut Vec<RevisionEntry>,
) -> Result<()> {
    if depth > MAX_SKILL_PACKAGE_DEPTH {
        bail!("skill package exceeds the directory depth limit");
    }
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let metadata = fs::symlink_metadata(entry.path())?;
        if metadata.file_type().is_symlink() {
            bail!("skill packages may not contain symbolic links");
        }
        let relative = entry.path().strip_prefix(root)?.to_path_buf();
        #[cfg(unix)]
        let mode = {
            use std::os::unix::fs::MetadataExt;
            metadata.mode()
        };
        #[cfg(not(unix))]
        let mode = 0;
        if metadata.is_dir() {
            stats.directories += 1;
            if stats.directories > MAX_SKILL_PACKAGE_DIRS {
                bail!("skill package exceeds the directory-count limit");
            }
            entries.push(RevisionEntry {
                relative,
                path: entry.path(),
                kind: b'd',
                mode,
                length: 0,
            });
            collect_revision_entries(root, &entry.path(), depth + 1, stats, entries)?;
        } else if metadata.is_file() {
            stats.files += 1;
            stats.bytes = stats
                .bytes
                .checked_add(metadata.len())
                .context("skill package size overflow")?;
            if stats.files > MAX_SKILL_PACKAGE_FILES || stats.bytes > MAX_SKILL_PACKAGE_BYTES {
                bail!("skill package exceeds file-count or total-size limits");
            }
            entries.push(RevisionEntry {
                relative,
                path: entry.path(),
                kind: b'f',
                mode,
                length: metadata.len(),
            });
        } else {
            bail!("skill package contains an unsupported file type");
        }
    }
    Ok(())
}

pub(crate) fn hash_length_prefixed(hasher: &mut blake3::Hasher, value: &[u8]) {
    hasher.update(&(value.len() as u64).to_le_bytes());
    hasher.update(value);
}

pub(crate) fn copy_tree(source: &Path, destination: &Path) -> Result<()> {
    let mut stats = PackageStats {
        directories: 1,
        ..PackageStats::default()
    };
    fs::create_dir(destination)?;
    let result = (|| {
        secure_directory(destination)?;
        copy_tree_inner(source, destination, 0, &mut stats)?;
        if let Some(parent) = destination.parent() {
            File::open(parent)?.sync_all()?;
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(destination);
    }
    result
}

pub(crate) fn copy_tree_inner(
    source: &Path,
    destination: &Path,
    depth: usize,
    stats: &mut PackageStats,
) -> Result<()> {
    if depth > MAX_SKILL_PACKAGE_DEPTH {
        bail!("skill package exceeds the directory depth limit");
    }
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let metadata = fs::symlink_metadata(entry.path())?;
        let target = destination.join(entry.file_name());
        if metadata.file_type().is_symlink() {
            bail!("skill packages may not contain symbolic links");
        }
        if metadata.is_dir() {
            stats.directories += 1;
            if stats.directories > MAX_SKILL_PACKAGE_DIRS {
                bail!("skill package exceeds the directory-count limit");
            }
            fs::create_dir(&target)?;
            secure_directory(&target)?;
            copy_tree_inner(&entry.path(), &target, depth + 1, stats)?;
        } else if metadata.is_file() {
            stats.files += 1;
            stats.bytes = stats
                .bytes
                .checked_add(metadata.len())
                .context("skill package size overflow")?;
            if stats.files > MAX_SKILL_PACKAGE_FILES || stats.bytes > MAX_SKILL_PACKAGE_BYTES {
                bail!("skill package exceeds file-count or total-size limits");
            }
            fs::copy(entry.path(), &target)?;
            File::open(&target)?.sync_all()?;
        } else {
            bail!("skill package contains an unsupported file type");
        }
    }
    File::open(destination)?.sync_all()?;
    Ok(())
}

pub(crate) fn ensure_directory_chain(base: &Path, directory: &Path) -> Result<()> {
    let relative = directory
        .strip_prefix(base)
        .with_context(|| format!("path escapes skill root: {}", directory.display()))?;
    let mut current = base.to_path_buf();
    let metadata = fs::symlink_metadata(&current)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!(
            "skill path contains an unsafe directory: {}",
            current.display()
        );
    }
    for component in relative.components() {
        current.push(component);
        let metadata = fs::symlink_metadata(&current)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            bail!(
                "skill path contains an unsafe directory: {}",
                current.display()
            );
        }
    }
    Ok(())
}

pub(crate) fn create_private_directory_chain(base: &Path, directory: &Path) -> Result<()> {
    let relative = directory
        .strip_prefix(base)
        .with_context(|| format!("path escapes skill root: {}", directory.display()))?;
    secure_directory(base)?;
    let mut current = base.to_path_buf();
    for component in relative.components() {
        current.push(component);
        match fs::create_dir(&current) {
            Ok(()) => secure_directory(&current)?,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                secure_directory(&current)?
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}
