use crate::config::{persona_scope_name, AppConfig};
use crate::paths::GQYPaths;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use yaml_rust2::scanner::{Scanner, Token, TokenType};
use yaml_rust2::{Yaml, YamlLoader};

mod parse;
pub(crate) use parse::*;
mod store;
pub(crate) use store::*;
mod install;
pub(crate) use install::*;
#[cfg(test)]
mod tests;
/// Skills compiled into the binary: (name, raw SKILL.md). A user skill of
/// the same name in the persona/global directories overrides the built-in.
const BUILTIN_SKILLS: &[(&str, &str)] = &[];
const DRAFT_MANIFEST: &str = "draft.json";
const DRAFT_PACKAGE_DIR: &str = "package";
const DRAFT_VERSION: u32 = 1;
const DRAFT_RETENTION: Duration = Duration::from_secs(30 * 24 * 60 * 60);
const PUBLISHED_DRAFT_RETENTION: Duration = Duration::from_secs(24 * 60 * 60);
const MAX_SKILL_FILE_BYTES: u64 = 256 * 1024;
const MAX_SKILL_PACKAGE_FILES: usize = 512;
const MAX_SKILL_PACKAGE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_SKILL_PACKAGE_DIRS: usize = 128;
const MAX_SKILL_PACKAGE_DEPTH: usize = 16;
const MAX_DRAFT_MANIFEST_BYTES: u64 = 64 * 1024;
const MAX_YAML_TOKENS: usize = 4_096;
const PUBLISH_LOCK_FILE: &str = ".publish.lock";
const MAX_SKILL_CATALOG_ENTRIES: usize = 256;
const MAX_SKILL_ROOT_DIRECTORIES: usize = 1_024;
const MAX_SKILL_RESOURCE_ENTRIES: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkillMetadata {
    pub name: String,
    pub description: String,
    pub license: Option<String>,
    pub compatibility: Option<String>,
    pub metadata: BTreeMap<String, String>,
    pub allowed_tools: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SkillSource {
    Persona,
    Global,
    BuiltIn,
}

impl SkillSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Persona => "persona",
            Self::Global => "global",
            Self::BuiltIn => "built_in",
        }
    }
}

#[derive(Clone, Debug)]
pub struct SkillEntry {
    pub metadata: SkillMetadata,
    pub source: SkillSource,
    pub directory: Option<PathBuf>,
}

#[derive(Clone, Debug)]
pub struct LoadedSkill {
    pub metadata: SkillMetadata,
    pub body: String,
    pub source: SkillSource,
    pub base_dir: Option<PathBuf>,
    pub files: Vec<PathBuf>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SkillScope {
    Global,
    Persona,
}

impl SkillScope {
    pub fn parse(value: Option<&str>) -> Result<Self> {
        match value.unwrap_or("global").trim() {
            "" | "global" => Ok(Self::Global),
            "persona" => Ok(Self::Persona),
            other => bail!("invalid skill scope: {other}; expected global or persona"),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Persona => "persona",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DraftKind {
    Create,
    Update,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct DraftManifest {
    version: u32,
    id: String,
    name: String,
    scope: SkillScope,
    persona_scope: Option<String>,
    kind: DraftKind,
    base_revision: Option<String>,
    created_at: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct SkillDraft {
    pub id: String,
    pub name: String,
    pub scope: String,
    pub persona_scope: Option<String>,
    pub kind: String,
    pub skill_dir: String,
    pub skill_file: String,
    pub base_revision: Option<String>,
    pub created_at: u64,
    pub last_modified_at: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct PublishedSkill {
    pub name: String,
    pub scope: String,
    pub persona_scope: Option<String>,
    pub path: String,
    pub revision: String,
    pub operation: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct DeletedSkill {
    pub name: String,
    pub scope: String,
    pub persona_scope: Option<String>,
    pub path: String,
}

pub fn discover(config: &AppConfig, paths: &GQYPaths) -> Result<Vec<SkillEntry>> {
    let mut entries = Vec::new();
    let mut seen = BTreeSet::new();
    for (root, source) in skill_roots(config, paths) {
        for directory in sorted_skill_directories(&root)? {
            if directory.join(".disabled").exists() {
                continue;
            }
            let skill_file = directory.join("SKILL.md");
            if !skill_file.is_file() {
                continue;
            }
            let raw = match read_skill_file(&skill_file) {
                Ok(raw) => raw,
                Err(error) => {
                    tracing::warn!(path = %skill_file.display(), error = %error, "skipping unreadable skill");
                    continue;
                }
            };
            let directory_name = directory
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default();
            let metadata = match parse_skill_metadata(&raw, Some(directory_name)) {
                Ok(metadata) => metadata,
                Err(error) => {
                    tracing::warn!(path = %skill_file.display(), error = %error, "skipping invalid skill");
                    continue;
                }
            };
            if seen.insert(metadata.name.clone()) {
                if entries.len() >= MAX_SKILL_CATALOG_ENTRIES.saturating_sub(1) {
                    bail!("skill catalog exceeds the {MAX_SKILL_CATALOG_ENTRIES} entry limit");
                }
                entries.push(SkillEntry {
                    metadata,
                    source,
                    directory: Some(directory),
                });
            }
        }
    }
    for (name, raw) in BUILTIN_SKILLS {
        if !seen.contains(*name) {
            entries.push(SkillEntry {
                metadata: parse_skill_metadata(raw, Some(name))?,
                source: SkillSource::BuiltIn,
                directory: None,
            });
        }
    }
    Ok(entries)
}

pub fn catalog_fingerprint(config: &AppConfig, paths: &GQYPaths) -> Result<[u8; 32]> {
    let mut hasher = blake3::Hasher::new();
    for (root, source) in skill_roots(config, paths) {
        hasher.update(source.as_str().as_bytes());
        hasher.update(root.as_os_str().as_encoded_bytes());
        for directory in sorted_skill_directories(&root)? {
            hasher.update(directory.as_os_str().as_encoded_bytes());
            hash_metadata(&mut hasher, &directory.join(".disabled"))?;
            hash_metadata(&mut hasher, &directory.join("SKILL.md"))?;
        }
    }
    for (_, raw) in BUILTIN_SKILLS {
        hasher.update(raw.as_bytes());
    }
    Ok(*hasher.finalize().as_bytes())
}

pub fn load(name: &str, config: &AppConfig, paths: &GQYPaths) -> Result<LoadedSkill> {
    let name = name.trim();
    if name.is_empty() {
        bail!("skill name is required");
    }
    let entry = discover(config, paths)?
        .into_iter()
        .find(|entry| entry.metadata.name == name)
        .ok_or_else(|| anyhow::anyhow!("skill not found: {name}"))?;
    if let Some(directory) = entry.directory {
        let raw = read_skill_file(&directory.join("SKILL.md"))?;
        let (metadata, body) = parse_skill_document(&raw, Some(name))?;
        let mut files = Vec::new();
        for entry in fs::read_dir(&directory)? {
            let entry = entry?;
            let name = entry.file_name();
            if name == "SKILL.md" || name.to_string_lossy().starts_with('.') {
                continue;
            }
            if files.len() >= MAX_SKILL_RESOURCE_ENTRIES {
                bail!(
                    "skill resource manifest exceeds the {MAX_SKILL_RESOURCE_ENTRIES} entry limit"
                );
            }
            files.push(entry.path());
        }
        files.sort();
        return Ok(LoadedSkill {
            metadata,
            body,
            source: entry.source,
            base_dir: Some(directory),
            files,
        });
    }
    let raw = BUILTIN_SKILLS
        .iter()
        .find(|(builtin_name, _)| *builtin_name == name)
        .map(|(_, raw)| *raw)
        .with_context(|| format!("skill not found: {name}"))?;
    let (metadata, body) = parse_skill_document(raw, Some(name))?;
    Ok(LoadedSkill {
        metadata,
        body,
        source: SkillSource::BuiltIn,
        base_dir: None,
        files: Vec::new(),
    })
}

pub fn create_draft(
    config: &AppConfig,
    paths: &GQYPaths,
    name: &str,
    description: &str,
    scope: SkillScope,
) -> Result<SkillDraft> {
    prune_expired_drafts(paths)?;
    validate_skill_name(name)?;
    validate_description(description)?;
    let persona_scope = persona_scope(config, scope);
    let target = target_path(paths, name, scope, persona_scope.as_deref())?;
    if target.exists() {
        bail!("skill already exists in {} scope: {name}", scope.as_str());
    }
    let manifest = new_manifest(name, scope, persona_scope, DraftKind::Create, None);
    let package = create_empty_draft(paths, &manifest)?;
    let result = (|| {
        let skill_dir = package.join(name);
        fs::create_dir(&skill_dir)?;
        write_private_file(
            &skill_dir.join("SKILL.md"),
            format!(
                "---\nname: {name}\ndescription: {}\n---\n\n# {name}\n\n## Workflow\n\nDescribe the reusable workflow here.\n",
                serde_json::to_string(description.trim())?
            )
            .as_bytes(),
        )?;
        write_draft_manifest(paths, &manifest)?;
        draft_public(paths, &manifest)
    })();
    cleanup_failed_draft(paths, &manifest, result)
}

pub fn update_draft(
    config: &AppConfig,
    paths: &GQYPaths,
    name: &str,
    scope: SkillScope,
) -> Result<SkillDraft> {
    prune_expired_drafts(paths)?;
    validate_skill_name(name)?;
    let persona_scope = persona_scope(config, scope);
    let source = target_path(paths, name, scope, persona_scope.as_deref())?;
    if !source.join("SKILL.md").is_file() {
        bail!("skill not found in {} scope: {name}", scope.as_str());
    }
    let _lease = acquire_publish_lock(paths)?;
    ensure_directory_chain(&paths.skills_dir, &source)?;
    validate_skill_package(&source, name)?;
    let revision_before = skill_revision(&source)?;
    let manifest = new_manifest(
        name,
        scope,
        persona_scope,
        DraftKind::Update,
        Some(revision_before.clone()),
    );
    let package = create_empty_draft(paths, &manifest)?;
    let result = (|| {
        copy_tree(&source, &package.join(name))?;
        let revision_after = skill_revision(&source)?;
        if revision_after != revision_before {
            bail!("skill changed while its update draft was being created; retry");
        }
        validate_skill_package(&package.join(name), name)?;
        write_draft_manifest(paths, &manifest)?;
        draft_public(paths, &manifest)
    })();
    cleanup_failed_draft(paths, &manifest, result)
}

pub fn publish_draft(paths: &GQYPaths, draft_id: &str) -> Result<PublishedSkill> {
    validate_draft_id(draft_id)?;
    let _lease = acquire_publish_lock(paths)?;
    prune_expired_drafts_unlocked(paths)?;
    let manifest = read_manifest(paths, draft_id)?;
    let draft_root = paths.skill_drafts_dir().join(draft_id);
    let source = draft_root.join(DRAFT_PACKAGE_DIR).join(&manifest.name);
    ensure_directory_chain(&draft_root, &source)?;
    let target = target_path(
        paths,
        &manifest.name,
        manifest.scope,
        manifest.persona_scope.as_deref(),
    )?;
    let parent = target.parent().context("skill target has no parent")?;
    create_private_dir(&paths.skills_dir)?;
    create_private_directory_chain(&paths.skills_dir, parent)?;
    let staged = parent.join(format!(
        ".gqy-skill-stage-{}-{:016x}",
        std::process::id(),
        rand::random::<u64>()
    ));
    let source_revision_before = skill_revision(&source)?;
    copy_tree(&source, &staged)?;
    let source_revision_after = skill_revision(&source)?;
    if source_revision_after != source_revision_before {
        bail!("skill draft changed while it was being published; retry");
    }
    let mut staged_guard = StagedDirectory::new(staged.clone());
    validate_skill_package(&staged, &manifest.name)?;
    let revision = skill_revision(&staged)?;
    if skill_revision(&source)? != source_revision_after {
        bail!("skill draft changed before installation; retry");
    }

    match manifest.kind {
        DraftKind::Create => {
            if target.exists() {
                bail!(
                    "skill already exists; create never overwrites: {}",
                    manifest.name
                );
            }
            install_new_skill(&staged, &target).with_context(|| {
                format!(
                    "publishing skill from {} to {}",
                    staged.display(),
                    target.display()
                )
            })?;
            if let Err(error) = File::open(parent).and_then(|directory| directory.sync_all()) {
                tracing::warn!(path = %parent.display(), error = %error, "failed to sync published skill directory");
            }
            staged_guard.disarm();
        }
        DraftKind::Update => {
            if !target.is_dir() {
                bail!("skill disappeared before update: {}", manifest.name);
            }
            ensure_directory_chain(&paths.skills_dir, &target)?;
            validate_skill_package(&target, &manifest.name)?;
            let expected = manifest
                .base_revision
                .as_deref()
                .context("update draft is missing its base revision")?;
            let current = skill_revision(&target)?;
            if current != expected {
                bail!(
                    "skill changed after the update draft was created; create a new update draft"
                );
            }
            install_updated_skill(&staged, &target, &current, &mut staged_guard)?;
        }
    }
    let archived_draft = paths.skill_drafts_dir().join(format!(
        ".published-{}-{:016x}",
        manifest.id,
        rand::random::<u64>()
    ));
    if let Err(error) = fs::rename(&draft_root, &archived_draft) {
        tracing::warn!(path = %draft_root.display(), error = %error, "failed to archive published skill draft");
    } else if let Err(error) = File::open(paths.skill_drafts_dir()).and_then(|dir| dir.sync_all()) {
        tracing::warn!(path = %archived_draft.display(), error = %error, "failed to sync published skill draft archive");
    }
    Ok(PublishedSkill {
        name: manifest.name,
        scope: manifest.scope.as_str().to_string(),
        persona_scope: manifest.persona_scope,
        path: target.display().to_string(),
        revision,
        operation: match manifest.kind {
            DraftKind::Create => "create",
            DraftKind::Update => "update",
        }
        .to_string(),
    })
}

pub fn delete_skill(
    config: &AppConfig,
    paths: &GQYPaths,
    name: &str,
    scope: SkillScope,
) -> Result<DeletedSkill> {
    validate_skill_name(name)?;
    let persona_scope = persona_scope(config, scope);
    let target = target_path(paths, name, scope, persona_scope.as_deref())?;
    let _lease = acquire_publish_lock(paths)?;
    match fs::symlink_metadata(&target) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            bail!("skill path is unsafe: {}", target.display())
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            bail!("skill not found in {} scope: {name}", scope.as_str())
        }
        Err(error) => return Err(error.into()),
    }
    ensure_directory_chain(&paths.skills_dir, &target)?;
    validate_skill_package(&target, name)?;
    fs::remove_dir_all(&target)?;
    if let Some(parent) = target.parent() {
        if let Err(error) = File::open(parent).and_then(|directory| directory.sync_all()) {
            tracing::warn!(path = %parent.display(), error = %error, "failed to sync deleted skill directory");
        }
    }
    Ok(DeletedSkill {
        name: name.to_string(),
        scope: scope.as_str().to_string(),
        persona_scope,
        path: target.display().to_string(),
    })
}

pub fn list_drafts(paths: &GQYPaths) -> Result<Vec<SkillDraft>> {
    prune_expired_drafts(paths)?;
    let root = paths.skill_drafts_dir();
    if !root.is_dir() {
        return Ok(Vec::new());
    }
    let mut drafts = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let id = entry.file_name().to_string_lossy().to_string();
        if id.starts_with('.') {
            continue;
        }
        match read_manifest(paths, &id).and_then(|manifest| draft_public(paths, &manifest)) {
            Ok(draft) => drafts.push(draft),
            Err(error) => {
                tracing::warn!(draft_id = id, error = %error, "skipping invalid skill draft")
            }
        }
    }
    drafts.sort_by(|left, right| {
        left.created_at
            .cmp(&right.created_at)
            .then(left.id.cmp(&right.id))
    });
    Ok(drafts)
}

pub fn prune_expired_drafts(paths: &GQYPaths) -> Result<usize> {
    let _lease = acquire_publish_lock(paths)?;
    prune_expired_drafts_unlocked(paths)
}

fn prune_expired_drafts_unlocked(paths: &GQYPaths) -> Result<usize> {
    let root = paths.skill_drafts_dir();
    if !root.is_dir() {
        return Ok(0);
    }
    let now = SystemTime::now();
    let mut removed = 0;
    for entry in fs::read_dir(&root)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let published_archive = entry
            .file_name()
            .to_string_lossy()
            .starts_with(".published-");
        let inspection = match inspect_latest_modified(&entry.path()) {
            Ok(inspection) => inspection,
            Err(error) => {
                tracing::warn!(path = %entry.path().display(), error = %error, "failed to inspect skill draft age");
                continue;
            }
        };
        let modified = match inspection {
            DraftInspection::Valid(modified) => modified,
            DraftInspection::Invalid => {
                let error = anyhow::anyhow!("draft exceeds inspection limits");
                tracing::warn!(path = %entry.path().display(), error = %error, "removing invalid skill draft");
                match fs::remove_dir_all(entry.path()) {
                    Ok(()) => removed += 1,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
                continue;
            }
        };
        let age = now.duration_since(modified).unwrap_or_default();
        let retention = if published_archive {
            PUBLISHED_DRAFT_RETENTION
        } else {
            DRAFT_RETENTION
        };
        if age >= retention {
            tracing::info!(path = %entry.path().display(), "removing expired skill draft");
            match fs::remove_dir_all(entry.path()) {
                Ok(()) => removed += 1,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
    }
    Ok(removed)
}

pub fn is_generated_skill(raw: &str) -> bool {
    parse_skill_metadata(raw, None)
        .ok()
        .and_then(|metadata| metadata.metadata.get("gqy.generated").cloned())
        .is_some_and(|value| value.eq_ignore_ascii_case("true"))
        || raw.contains("generated_by: gqy")
        || raw.contains("Auto-learned method from assistant conversation")
        || raw.contains("Auto-learned method from GQY conversation")
}
