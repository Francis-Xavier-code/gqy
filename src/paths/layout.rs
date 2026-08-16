//! layout — 自 src/paths.rs 拆分。

pub(crate) use super::*;

use crate::i18n::text as t;
use anyhow::{bail, Context, Result};
use directories::{BaseDirs, UserDirs};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};

pub(crate) const LAYOUT_MARKER: &str = ".layout-v1";
pub(crate) const RESOURCE_LAYOUT_MARKER: &str = ".resource-layout-v1";
pub(crate) const RESOURCE_MIGRATION_JOURNAL: &str = ".resource-layout-v1.journal.json";

#[derive(Debug, Clone)]
pub struct GQYPaths {
    /// Everything below lives under this root (`~/.gqy`, or `GQY_HOME`).
    /// Kept as its own field because the model is told where GQY's files are
    /// and guessing it back from a child directory would silently break the
    /// day the layout changes.
    pub root_dir: PathBuf,
    pub config_dir: PathBuf,
    pub config_file: PathBuf,
    pub skills_dir: PathBuf,
    pub data_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub state_dir: PathBuf,
    pub pictures_dir: PathBuf,
    pub fish_hook_file: PathBuf,
    pub bash_hook_file: PathBuf,
    pub zsh_hook_file: PathBuf,
    pub scripts_dir: PathBuf,
    pub system_scripts_dir: PathBuf,
}

impl GQYPaths {
    pub fn new() -> Result<Self> {
        let base = BaseDirs::new().context(t(
            "could not determine XDG base directories",
            "无法确定 XDG 基础目录",
        ))?;
        // legacy 目录名保留 miyu:迁移逻辑要能找到改名前的旧数据。
        let legacy_config_dir = base.config_dir().join("miyu");
        let legacy_data_dir = base.data_dir().join("miyu");
        let legacy_cache_dir = base.cache_dir().join("miyu");
        let legacy_state_dir = base
            .state_dir()
            .unwrap_or_else(|| base.data_dir())
            .join("miyu");
        let legacy_documents_dir = UserDirs::new()
            .and_then(|dirs| dirs.document_dir().map(PathBuf::from))
            .unwrap_or_else(|| base.home_dir().join("Documents"))
            .join("Miyu");
        let legacy_pictures_root = std::env::var_os("XDG_PICTURES_DIR")
            .map(PathBuf::from)
            .or_else(|| UserDirs::new().and_then(|dirs| dirs.picture_dir().map(PathBuf::from)))
            .unwrap_or_else(|| base.home_dir().join("Pictures"));
        let explicit_home = std::env::var_os("GQY_HOME").map(PathBuf::from);
        let root_dir = explicit_home
            .clone()
            .unwrap_or_else(|| base.home_dir().join(".gqy"));
        let config_dir = root_dir.join("config");
        let data_dir = root_dir.join("data");
        let cache_dir = root_dir.join("cache");
        let state_dir = root_dir.join("state");

        let legacy = LegacyLayout {
            config_dir: legacy_config_dir.clone(),
            data_dir: legacy_data_dir.clone(),
            cache_dir: legacy_cache_dir.clone(),
            state_dir: legacy_state_dir.clone(),
            documents_dir: legacy_documents_dir,
            pictures_dirs: vec![
                legacy_pictures_root.join("miyu"),
                legacy_pictures_root.join("Miyu"),
            ],
        };
        let next = Layout {
            root_dir: root_dir.clone(),
            config_dir: config_dir.clone(),
            data_dir: data_dir.clone(),
            cache_dir: cache_dir.clone(),
            state_dir: state_dir.clone(),
        };

        // A client from a newly installed binary may start while the previous
        // daemon still has the legacy SQLite files open. Keep that client on
        // the legacy layout; daemon version negotiation will stop the old
        // process, and the newly spawned daemon performs the migration.
        let migration_enabled = explicit_home.is_none() && !cfg!(test);
        let marker_exists = layout_marker_exists(&next)?;
        let use_legacy_temporarily = migration_enabled
            && !marker_exists
            && legacy.exists()?
            && legacy_daemon_is_running(&legacy);
        let (config_dir, data_dir, cache_dir, state_dir) = if use_legacy_temporarily {
            (
                legacy_config_dir,
                legacy_data_dir,
                legacy_cache_dir,
                legacy_state_dir,
            )
        } else {
            if migration_enabled {
                migrate_legacy_layout(&legacy, &next)?;
            } else if explicit_home.is_some() {
                ensure_private_dir(&root_dir)?;
            }
            (config_dir, data_dir, cache_dir, state_dir)
        };
        let resource_layout = Layout {
            root_dir: root_dir.clone(),
            config_dir: config_dir.clone(),
            data_dir: data_dir.clone(),
            cache_dir: cache_dir.clone(),
            state_dir: state_dir.clone(),
        };
        let resource_marker_exists = resource_layout_marker_exists(&resource_layout)?;
        let daemon_process = current_process_is_daemon();
        let resource_migration_deferred =
            if use_legacy_temporarily || resource_marker_exists || cfg!(test) {
                false
            } else {
                !try_migrate_resource_layout(&resource_layout, daemon_process)?
            };
        let pictures_dir = if use_legacy_temporarily {
            legacy_pictures_root.join("miyu")
        } else {
            data_dir.join("pictures")
        };
        let fish_hook_file = base.config_dir().join("fish/conf.d/gqy.fish");
        let bash_hook_file = config_dir.join("shell/bash-hook.sh");
        let zsh_hook_file = config_dir.join("shell/zsh-hook.zsh");
        let resource_config_dir = if use_legacy_temporarily || resource_migration_deferred {
            config_dir.clone()
        } else {
            data_dir.clone()
        };
        let scripts_dir = resource_config_dir.join("scripts");
        let system_scripts_dir = PathBuf::from("/usr/share/gqy/scripts");

        Ok(Self {
            // The canonical home even inside the transient legacy window: that
            // window only exists while an old daemon still holds the XDG files
            // open, and it closes as soon as the new daemon migrates them here.
            root_dir,
            config_file: config_dir.join("config.jsonc"),
            skills_dir: resource_config_dir.join("skills"),
            config_dir,
            data_dir,
            cache_dir,
            state_dir,
            pictures_dir,
            fish_hook_file,
            bash_hook_file,
            zsh_hook_file,
            scripts_dir,
            system_scripts_dir,
        })
    }

    pub fn create_dirs(&self) -> Result<()> {
        let prompts_dir = self.prompts_dir();
        let identities_dir = self.identities_dir();
        let persona_avatars_dir = self.persona_avatars_dir();
        let skill_drafts_dir = self.skill_drafts_dir();
        for directory in [
            &self.config_dir,
            &self.skills_dir,
            &self.data_dir,
            &self.cache_dir,
            &self.state_dir,
            &self.pictures_dir,
            &self.scripts_dir,
            &prompts_dir,
            &identities_dir,
            &persona_avatars_dir,
            &skill_drafts_dir,
        ] {
            ensure_private_dir(directory)?;
        }
        Ok(())
    }

    /// Returns the root used for GQY-owned, user-authored resources. During
    /// an upgrade this intentionally remains the old config directory until
    /// the resource migration marker has been committed.
    pub fn resource_dir(&self) -> &Path {
        if self.skills_dir == self.data_dir.join("skills") {
            &self.data_dir
        } else {
            &self.config_dir
        }
    }

    pub fn resources_use_config_dir(&self) -> bool {
        self.resource_dir() == self.config_dir
    }

    pub fn legacy_config_dir(&self) -> Option<PathBuf> {
        let base = BaseDirs::new()?;
        if self.config_dir == base.home_dir().join(".miyu/config") {
            Some(base.config_dir().join("miyu"))
        } else if self.config_dir == base.home_dir().join(".gqy/config") {
            Some(base.config_dir().join("gqy"))
        } else {
            None
        }
    }

    pub fn migrated_resource_path(&self, path: &Path) -> Option<PathBuf> {
        let relative = if path.is_absolute() {
            path.strip_prefix(&self.config_dir).ok().or_else(|| {
                self.legacy_config_dir()
                    .as_deref()
                    .and_then(|legacy| path.strip_prefix(legacy).ok())
            })?
        } else {
            path
        };
        let relative = normalize_resource_relative_path(relative)?;
        let namespace = relative.components().next()?.as_os_str().to_str()?;
        if !matches!(
            namespace,
            "skills" | "scripts" | "prompts" | "identities" | "persona-avatars"
        ) {
            return None;
        }
        Some(self.resource_dir().join(relative))
    }

    pub fn prompts_dir(&self) -> PathBuf {
        self.resource_dir().join("prompts")
    }

    pub fn identities_dir(&self) -> PathBuf {
        self.resource_dir().join("identities")
    }

    pub fn persona_avatars_dir(&self) -> PathBuf {
        self.resource_dir().join("persona-avatars")
    }

    pub fn skill_drafts_dir(&self) -> PathBuf {
        self.state_dir.join("skill-drafts")
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.cache_dir.join("logs")
    }

    pub fn runtime_dir(&self) -> PathBuf {
        match std::env::var_os("XDG_RUNTIME_DIR") {
            Some(runtime_dir) => runtime_dir_for(
                Path::new(&runtime_dir),
                std::env::var_os("GQY_HOME").as_deref().map(Path::new),
            ),
            None => self.state_dir.join("gqy"),
        }
    }

    pub fn ipc_socket(&self) -> PathBuf {
        self.runtime_dir().join("core.sock")
    }

    pub fn ipc_lock(&self) -> PathBuf {
        self.runtime_dir().join("core.lock")
    }

    pub fn daemon_start_lock(&self) -> PathBuf {
        self.runtime_dir().join("starter.lock")
    }

    pub fn daemon_launch_state_file(&self) -> PathBuf {
        self.state_dir.join("daemon-launch.json")
    }

    pub fn managed_web_password_dir(&self) -> PathBuf {
        self.state_dir.join("web-passwords")
    }

    pub fn print(&self) {
        println!(
            "{}: {}",
            t("config directory", "配置目录"),
            self.config_dir.display()
        );
        println!(
            "{}: {}",
            t("config file", "配置文件"),
            self.config_file.display()
        );
        println!(
            "{}: {}",
            t("skills directory", "skills 目录"),
            self.skills_dir.display()
        );
        println!(
            "{}: {}",
            t("skill drafts directory", "skill 草稿目录"),
            self.skill_drafts_dir().display()
        );
        println!(
            "{}: {}",
            t("prompts directory", "prompts 目录"),
            self.prompts_dir().display()
        );
        println!(
            "{}: {}",
            t("identities directory", "identities 目录"),
            self.identities_dir().display()
        );
        println!(
            "{}: {}",
            t("persona avatars directory", "人格头像目录"),
            self.persona_avatars_dir().display()
        );
        println!(
            "{}: {}",
            t("data directory", "数据目录"),
            self.data_dir.display()
        );
        println!(
            "{}: {}",
            t("cache directory", "缓存目录"),
            self.cache_dir.display()
        );
        println!(
            "{}: {}",
            t("state directory", "状态目录"),
            self.state_dir.display()
        );
        println!(
            "{}: {}",
            t("log directory", "日志目录"),
            self.logs_dir().display()
        );
        println!(
            "{}: {}",
            t("pictures directory", "图片目录"),
            self.pictures_dir.display()
        );
        println!(
            "{}: {}",
            t("fish hook file", "fish hook 文件"),
            self.fish_hook_file.display()
        );
        println!(
            "{}: {}",
            t("bash hook file", "bash hook 文件"),
            self.bash_hook_file.display()
        );
        println!(
            "{}: {}",
            t("zsh hook file", "zsh hook 文件"),
            self.zsh_hook_file.display()
        );
        println!(
            "{}: {}",
            t("scripts directory", "scripts 目录"),
            self.scripts_dir.display()
        );
        println!(
            "{}: {}",
            t("system scripts directory", "系统 scripts 目录"),
            self.system_scripts_dir.display()
        );
    }
}

pub(crate) fn runtime_dir_for(runtime_root: &Path, explicit_home: Option<&Path>) -> PathBuf {
    let name = explicit_home.map_or_else(
        || "gqy".to_string(),
        |home| {
            let normalized = normalize_home(home);
            let digest = blake3::hash(normalized.as_os_str().as_encoded_bytes());
            format!("gqy-{}", &digest.to_hex()[..12])
        },
    );
    runtime_root.join(name)
}

pub(crate) fn normalize_home(home: &Path) -> PathBuf {
    if let Ok(canonical) = fs::canonicalize(home) {
        return canonical;
    }
    let absolute = if home.is_absolute() {
        home.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("/"))
            .join(home)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

#[derive(Clone, Debug)]
pub(crate) struct LegacyLayout {
    pub(crate) config_dir: PathBuf,
    pub(crate) data_dir: PathBuf,
    pub(crate) cache_dir: PathBuf,
    pub(crate) state_dir: PathBuf,
    pub(crate) documents_dir: PathBuf,
    pub(crate) pictures_dirs: Vec<PathBuf>,
}

impl LegacyLayout {
    pub(crate) fn exists(&self) -> Result<bool> {
        for path in [
            &self.config_dir,
            &self.data_dir,
            &self.cache_dir,
            &self.state_dir,
            &self.documents_dir,
        ]
        .into_iter()
        .chain(self.pictures_dirs.iter())
        {
            if entry_exists(path)? {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Layout {
    pub(crate) root_dir: PathBuf,
    pub(crate) config_dir: PathBuf,
    pub(crate) data_dir: PathBuf,
    pub(crate) cache_dir: PathBuf,
    pub(crate) state_dir: PathBuf,
}

impl Layout {
    pub(crate) fn marker(&self) -> PathBuf {
        self.root_dir.join(LAYOUT_MARKER)
    }

    pub(crate) fn resource_marker(&self) -> PathBuf {
        self.root_dir.join(RESOURCE_LAYOUT_MARKER)
    }

    pub(crate) fn resource_journal(&self) -> PathBuf {
        self.root_dir.join(RESOURCE_MIGRATION_JOURNAL)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct ResourceMigrationEntry {
    pub(crate) source: PathBuf,
    pub(crate) destination: PathBuf,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct ResourceMigrationJournal {
    pub(crate) entries: Vec<ResourceMigrationEntry>,
    pub(crate) moved: usize,
    #[serde(default)]
    pub(crate) pending: Option<usize>,
}

pub(crate) fn resource_layout_entries(layout: &Layout) -> Vec<ResourceMigrationEntry> {
    vec![
        ResourceMigrationEntry {
            source: layout.config_dir.join("skills"),
            destination: layout.data_dir.join("skills"),
        },
        ResourceMigrationEntry {
            source: layout.config_dir.join("scripts"),
            destination: layout.data_dir.join("scripts"),
        },
        ResourceMigrationEntry {
            source: layout.config_dir.join("prompts"),
            destination: layout.data_dir.join("prompts"),
        },
        ResourceMigrationEntry {
            source: layout.config_dir.join("identities"),
            destination: layout.data_dir.join("identities"),
        },
        ResourceMigrationEntry {
            source: layout.config_dir.join("persona-avatars"),
            destination: layout.data_dir.join("persona-avatars"),
        },
        ResourceMigrationEntry {
            source: layout.config_dir.join("system-prompt.md"),
            destination: layout.data_dir.join("prompts/system-prompt.md"),
        },
        ResourceMigrationEntry {
            source: layout.config_dir.join("user-identity.md"),
            destination: layout.data_dir.join("identities/user-identity.md"),
        },
    ]
}

pub(crate) fn current_process_is_daemon() -> bool {
    std::env::args_os()
        .nth(1)
        .is_some_and(|argument| argument == "__daemon")
}

pub(crate) fn resource_runtime_dir(layout: &Layout) -> PathBuf {
    if cfg!(test) {
        return layout.state_dir.join("gqy");
    }
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(runtime_root) => runtime_dir_for(
            Path::new(&runtime_root),
            std::env::var_os("GQY_HOME").as_deref().map(Path::new),
        ),
        None => layout.state_dir.join("gqy"),
    }
}

pub(crate) fn daemon_is_running_at(runtime_dir: &Path, current_process_is_daemon: bool) -> bool {
    std::os::unix::net::UnixStream::connect(runtime_dir.join("core.sock")).is_ok()
        || runtime_lock_is_held(&runtime_dir.join("core.lock"))
        || (!current_process_is_daemon && runtime_lock_is_held(&runtime_dir.join("starter.lock")))
}

pub(crate) fn normalize_resource_relative_path(path: &Path) -> Option<PathBuf> {
    if path.is_absolute() {
        return None;
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(component) => normalized.push(component),
            Component::ParentDir => {
                if !normalized.pop() {
                    return None;
                }
            }
            _ => return None,
        }
    }
    (!normalized.as_os_str().is_empty()).then_some(normalized)
}

pub(crate) fn resource_layout_marker_exists(layout: &Layout) -> Result<bool> {
    marker_exists_at(&layout.resource_marker(), "resource layout marker")
}

pub(crate) fn marker_exists_at(path: &Path, label: &str) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            bail!(
                "GQY {label} must not be a symbolic link: {}",
                path.display()
            )
        }
        Ok(metadata) if !metadata.is_file() => {
            bail!("GQY {label} is not a regular file: {}", path.display())
        }
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn migrate_resource_layout(layout: &Layout) -> Result<()> {
    if !try_migrate_resource_layout(layout, false)? {
        bail!("GQY resource migration is deferred while another daemon or starter is active");
    }
    Ok(())
}

pub(crate) fn try_migrate_resource_layout(
    layout: &Layout,
    current_process_is_daemon: bool,
) -> Result<bool> {
    if resource_layout_marker_exists(layout)? {
        remove_resource_journal_if_present(layout)?;
        return Ok(true);
    }

    ensure_private_dir(&layout.root_dir)?;
    ensure_private_dir(&layout.config_dir)?;
    ensure_private_dir(&layout.data_dir)?;
    let _lease = acquire_migration_lock(&layout.root_dir)?;
    if resource_layout_marker_exists(layout)? {
        remove_resource_journal_if_present(layout)?;
        return Ok(true);
    }
    let Some(_daemon_guard) = try_acquire_resource_daemon_guard(layout, current_process_is_daemon)?
    else {
        return Ok(false);
    };

    recover_resource_migration(layout)?;
    let mut entries = Vec::new();
    for entry in resource_layout_entries(layout) {
        if entry_exists(&entry.source)? {
            entries.push(entry);
        }
    }
    preflight_resource_entries(layout, &entries)?;

    let mut journal = ResourceMigrationJournal {
        entries,
        moved: 0,
        pending: None,
    };
    write_resource_journal(layout, &journal)?;
    while journal.moved < journal.entries.len() {
        let index = journal.moved;
        let entry = journal.entries[index].clone();
        journal.pending = Some(index);
        write_resource_journal(layout, &journal)?;
        if let Err(error) = atomic_resource_move(&entry.source, &entry.destination) {
            let recovery = recover_resource_migration(layout);
            return match recovery {
                Ok(()) => Err(error).with_context(|| {
                    format!(
                        "migrating GQY resource from {} to {}",
                        entry.source.display(),
                        entry.destination.display()
                    )
                }),
                Err(recovery_error) => Err(anyhow::anyhow!(
                    "resource migration failed: {error:#}; rollback also failed: {recovery_error:#}"
                )),
            };
        }
        journal.moved = index + 1;
        journal.pending = None;
        write_resource_journal(layout, &journal)?;
    }

    write_marker(&layout.resource_marker())?;
    remove_resource_journal_if_present(layout)?;
    Ok(true)
}

pub(crate) struct ResourceDaemonGuard {
    _starter: Option<File>,
    _core: File,
}

pub(crate) fn try_acquire_resource_daemon_guard(
    layout: &Layout,
    current_process_is_daemon: bool,
) -> Result<Option<ResourceDaemonGuard>> {
    let runtime_dir = resource_runtime_dir(layout);
    ensure_private_dir(&runtime_dir)?;
    let starter_path = runtime_dir.join("starter.lock");
    let starter_already_held = current_process_is_daemon && runtime_lock_is_held(&starter_path);
    let starter = if starter_already_held {
        None
    } else {
        let Some(lock) = try_acquire_runtime_lock(&starter_path)? else {
            return Ok(None);
        };
        Some(lock)
    };
    let Some(core) = try_acquire_runtime_lock(&runtime_dir.join("core.lock"))? else {
        return Ok(None);
    };
    if std::os::unix::net::UnixStream::connect(runtime_dir.join("core.sock")).is_ok() {
        return Ok(None);
    }
    Ok(Some(ResourceDaemonGuard {
        _starter: starter,
        _core: core,
    }))
}

pub(crate) fn try_acquire_runtime_lock(path: &Path) -> Result<Option<File>> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(path)?;
    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if result == 0 {
        return Ok(Some(file));
    }
    let error = std::io::Error::last_os_error();
    if matches!(error.raw_os_error(), Some(code) if code == libc::EWOULDBLOCK || code == libc::EAGAIN)
    {
        Ok(None)
    } else {
        Err(error.into())
    }
}

pub(crate) fn preflight_resource_entries(
    layout: &Layout,
    entries: &[ResourceMigrationEntry],
) -> Result<()> {
    let projections = entries
        .iter()
        .map(|entry| (entry.source.clone(), entry.destination.clone()))
        .collect::<Vec<_>>();
    for entry in entries {
        let source_metadata = fs::symlink_metadata(&entry.source)?;
        if source_metadata.file_type().is_symlink() {
            bail!(
                "GQY resource migration refuses symbolic-link source {}",
                entry.source.display()
            );
        }
        ensure_supported_entry_tree(&entry.source)?;
        ensure_absolute_symlink_targets_stable(&entry.source, &projections)?;
        ensure_destination_ancestors(layout, &entry.destination)?;
        ensure_resource_same_filesystem(&entry.source, &entry.destination)?;
        match fs::symlink_metadata(&entry.destination) {
            Ok(_) => bail!(
                "GQY resource migration destination already exists: {}; move or remove it and retry",
                entry.destination.display()
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    for (index, parent) in entries.iter().enumerate() {
        for child in entries.iter().skip(index + 1) {
            ensure_resource_targets_do_not_overlap(parent, child)?;
            ensure_resource_targets_do_not_overlap(child, parent)?;
        }
    }
    Ok(())
}

pub(crate) fn ensure_destination_ancestors(layout: &Layout, destination: &Path) -> Result<()> {
    let relative = destination
        .strip_prefix(&layout.data_dir)
        .with_context(|| {
            format!(
                "resource destination escapes GQY data directory: {}",
                destination.display()
            )
        })?;
    let mut current = layout.data_dir.clone();
    for component in relative.components() {
        current.push(component);
        if current == destination {
            break;
        }
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => bail!(
                "GQY resource migration refuses symbolic-link destination ancestor {}",
                current.display()
            ),
            Ok(metadata) if !metadata.is_dir() => bail!(
                "GQY resource migration destination ancestor is not a directory: {}",
                current.display()
            ),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

#[cfg(unix)]
pub(crate) fn ensure_resource_same_filesystem(source: &Path, destination: &Path) -> Result<()> {
    use std::os::unix::fs::MetadataExt;

    let source_device = fs::symlink_metadata(source)?.dev();
    let mut ancestor = destination
        .parent()
        .context("resource destination has no parent")?;
    let destination_device = loop {
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) => break metadata.dev(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                ancestor = ancestor
                    .parent()
                    .context("resource destination has no existing ancestor")?;
            }
            Err(error) => return Err(error.into()),
        }
    };
    if source_device != destination_device {
        bail!(
            "GQY resource migration requires source and destination on the same filesystem: {} -> {}",
            source.display(),
            destination.display()
        );
    }
    Ok(())
}

#[cfg(not(unix))]
pub(crate) fn ensure_resource_same_filesystem(_source: &Path, _destination: &Path) -> Result<()> {
    Ok(())
}

pub(crate) fn ensure_resource_targets_do_not_overlap(
    parent: &ResourceMigrationEntry,
    child: &ResourceMigrationEntry,
) -> Result<()> {
    let Ok(relative) = child.destination.strip_prefix(&parent.destination) else {
        return Ok(());
    };
    if relative.as_os_str().is_empty() {
        return Ok(());
    }
    let nested_source = parent.source.join(relative);
    if entry_exists(&nested_source)? && entry_exists(&child.source)? {
        bail!(
            "GQY resource migration found overlapping sources for {} and {}; remove one duplicate and retry",
            nested_source.display(),
            child.source.display()
        );
    }
    Ok(())
}

pub(crate) fn atomic_resource_move(source: &Path, destination: &Path) -> Result<()> {
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::rename(source, destination)?;
    sync_parent(destination)?;
    if source.parent() != destination.parent() {
        sync_parent(source)?;
    }
    Ok(())
}

pub(crate) fn write_resource_journal(
    layout: &Layout,
    journal: &ResourceMigrationJournal,
) -> Result<()> {
    let path = layout.resource_journal();
    let temporary = path.with_extension(format!(
        "tmp-{}-{}",
        std::process::id(),
        rand::random::<u64>()
    ));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&temporary)
        .with_context(|| format!("creating {}", temporary.display()))?;
    serde_json::to_writer_pretty(&mut file, journal)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    fs::rename(&temporary, &path)
        .with_context(|| format!("installing resource migration journal {}", path.display()))?;
    sync_parent(&path)
}

pub(crate) fn recover_resource_migration(layout: &Layout) -> Result<()> {
    let path = layout.resource_journal();
    let raw = match fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let mut journal: ResourceMigrationJournal = serde_json::from_str(&raw)
        .with_context(|| format!("parsing resource migration journal {}", path.display()))?;
    if journal.moved > journal.entries.len() {
        bail!("invalid GQY resource migration journal: moved count is out of range");
    }
    if let Some(index) = journal.pending {
        if index != journal.moved || index >= journal.entries.len() {
            bail!("invalid GQY resource migration journal: pending index is out of range");
        }
        let entry = &journal.entries[index];
        match (
            entry_exists(&entry.source)?,
            entry_exists(&entry.destination)?,
        ) {
            (true, false) => journal.pending = None,
            (false, true) => {
                journal.moved = index + 1;
                journal.pending = None;
            }
            (true, true) => bail!(
                "cannot recover GQY resource migration because both paths exist: {} and {}",
                entry.source.display(),
                entry.destination.display()
            ),
            (false, false) => bail!(
                "cannot recover GQY resource migration because both paths are missing: {} and {}",
                entry.source.display(),
                entry.destination.display()
            ),
        }
        write_resource_journal(layout, &journal)?;
    }
    while journal.moved > 0 {
        let index = journal.moved - 1;
        let entry = journal.entries[index].clone();
        match (
            entry_exists(&entry.source)?,
            entry_exists(&entry.destination)?,
        ) {
            (false, true) => {
                atomic_resource_move(&entry.destination, &entry.source).with_context(|| {
                    format!(
                        "rolling back GQY resource migration from {} to {}",
                        entry.destination.display(),
                        entry.source.display()
                    )
                })?
            }
            (true, false) => {}
            (true, true) => bail!(
                "cannot recover GQY resource migration because both paths exist: {} and {}",
                entry.source.display(),
                entry.destination.display()
            ),
            (false, false) => bail!(
                "cannot recover GQY resource migration because both paths are missing: {} and {}",
                entry.source.display(),
                entry.destination.display()
            ),
        }
        journal.moved = index;
        write_resource_journal(layout, &journal)?;
    }
    remove_resource_journal_if_present(layout)
}

pub(crate) fn remove_resource_journal_if_present(layout: &Layout) -> Result<()> {
    let path = layout.resource_journal();
    match fs::remove_file(&path) {
        Ok(()) => sync_parent(&path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

#[derive(Clone, Debug)]
pub(crate) struct MigrationMapping {
    source: PathBuf,
    destination: PathBuf,
}

impl MigrationMapping {
    pub(crate) fn new(source: impl Into<PathBuf>, destination: impl Into<PathBuf>) -> Self {
        Self {
            source: source.into(),
            destination: destination.into(),
        }
    }
}

pub(crate) struct MigrationLease(File);

impl Drop for MigrationLease {
    fn drop(&mut self) {
        unsafe {
            libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

pub(crate) fn legacy_daemon_is_running(legacy: &LegacyLayout) -> bool {
    legacy_daemon_is_running_at(
        legacy,
        std::env::var_os("XDG_RUNTIME_DIR")
            .as_deref()
            .map(Path::new),
    )
}

pub(crate) fn legacy_daemon_is_running_at(
    legacy: &LegacyLayout,
    xdg_runtime_dir: Option<&Path>,
) -> bool {
    let mut runtime_dirs = vec![legacy.state_dir.clone()];
    if let Some(runtime_dir) = xdg_runtime_dir {
        runtime_dirs.push(runtime_dir.to_path_buf());
    }
    runtime_dirs
        .into_iter()
        .map(|runtime_dir| runtime_dir.join("gqy"))
        .any(|runtime_dir| {
            std::os::unix::net::UnixStream::connect(runtime_dir.join("core.sock")).is_ok()
                || runtime_lock_is_held(&runtime_dir.join("core.lock"))
                || runtime_lock_is_held(&runtime_dir.join("starter.lock"))
        })
}

pub(crate) fn runtime_lock_is_held(path: &Path) -> bool {
    let file = match OpenOptions::new().read(true).write(true).open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return false,
        // Failure to inspect an existing lock must not authorize migration.
        Err(_) => return true,
    };
    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if result == 0 {
        unsafe {
            libc::flock(file.as_raw_fd(), libc::LOCK_UN);
        }
        false
    } else {
        true
    }
}

pub(crate) fn migrate_legacy_layout(legacy: &LegacyLayout, next: &Layout) -> Result<()> {
    if layout_marker_exists(next)? {
        return Ok(());
    }
    let mappings = legacy_migration_mappings(legacy, next);
    preflight_with_disposable_cache(&mappings, &legacy.cache_dir)?;

    ensure_private_dir(&next.root_dir)?;
    for directory in [
        &next.config_dir,
        &next.data_dir,
        &next.cache_dir,
        &next.state_dir,
    ] {
        ensure_existing_directory(directory)?;
    }
    let _lease = acquire_migration_lock(&next.root_dir)?;
    if layout_marker_exists(next)? {
        return Ok(());
    }
    // Repeat the full preflight while holding the layout lock. The first pass
    // guarantees a known conflict does not even create ~/.gqy; this pass
    // closes the race with another new GQY process before any data moves.
    let active = preflight_with_disposable_cache(&mappings, &legacy.cache_dir)?;
    let next_bash_hook = next.config_dir.join("shell/bash-hook.sh");
    let next_zsh_hook = next.config_dir.join("shell/zsh-hook.zsh");
    let had_bash_hook = entry_exists(&legacy.config_dir.join("shell/bash-hook.sh"))?
        || entry_exists(&next_bash_hook)?;
    let had_zsh_hook = entry_exists(&legacy.config_dir.join("shell/zsh-hook.zsh"))?
        || entry_exists(&next_zsh_hook)?;
    for mapping in &active {
        migrate_entry_unchecked(&mapping.source, &mapping.destination).with_context(|| {
            format!(
                "migrating GQY user files from {} to {}",
                mapping.source.display(),
                mapping.destination.display()
            )
        })?;
    }

    if had_bash_hook || had_zsh_hook {
        let home = next
            .root_dir
            .parent()
            .context("the GQY home directory has no parent")?;
        let bash_hook = had_bash_hook.then_some(next_bash_hook);
        let zsh_hook = had_zsh_hook.then_some(next_zsh_hook);
        crate::shell::refresh_migrated_hook_sources(
            home,
            bash_hook.as_deref(),
            zsh_hook.as_deref(),
        )
        .context("refreshing shell hook paths after GQY directory migration")?;
    }

    write_marker(&next.marker())?;
    Ok(())
}

pub(crate) fn legacy_migration_mappings(
    legacy: &LegacyLayout,
    next: &Layout,
) -> Vec<MigrationMapping> {
    let mut mappings = vec![
        MigrationMapping::new(&legacy.config_dir, &next.config_dir),
        MigrationMapping::new(&legacy.data_dir, &next.data_dir),
        MigrationMapping::new(&legacy.cache_dir, &next.cache_dir),
        MigrationMapping::new(&legacy.documents_dir, next.data_dir.join("documents")),
    ];
    // Older installations may have no XDG state directory and therefore use
    // the data directory for both values. Mapping the same source twice would
    // make the migration appear self-conflicting; keep its contents under the
    // unified data directory and leave the new state directory empty.
    if legacy.state_dir != legacy.data_dir {
        mappings.push(MigrationMapping::new(&legacy.state_dir, &next.state_dir));
    }
    mappings.extend(
        legacy
            .pictures_dirs
            .iter()
            .map(|source| MigrationMapping::new(source, next.data_dir.join("pictures"))),
    );
    mappings
}

pub(crate) fn existing_mappings(mappings: &[MigrationMapping]) -> Result<Vec<MigrationMapping>> {
    let mut active = Vec::with_capacity(mappings.len());
    let mut seen_sources = Vec::new();
    for mapping in mappings {
        if mapping.source == mapping.destination || !entry_exists(&mapping.source)? {
            continue;
        }
        if mapping.destination.starts_with(&mapping.source)
            || mapping.source.starts_with(&mapping.destination)
        {
            bail!(
                "GQY directory migration cannot move overlapping paths: {} and {}",
                mapping.source.display(),
                mapping.destination.display()
            );
        }
        // macOS filesystems are often case-insensitive, so `Pictures/miyu`
        // and `Pictures/Miyu` can be the same physical directory. Deduplicate
        // by device/inode so the legacy migration does not try to move one
        // source twice.
        let source_key = fs::metadata(&mapping.source)
            .map(|metadata| format!("{}:{}", metadata.dev(), metadata.ino()))
            .unwrap_or_else(|_| format!("path:{}", mapping.source.display()));
        if seen_sources.iter().any(|seen| seen == &source_key) {
            continue;
        }
        seen_sources.push(source_key);
        active.push(mapping.clone());
    }
    for (index, left) in active.iter().enumerate() {
        for right in active.iter().skip(index + 1) {
            if left.source == right.source
                || left.source.starts_with(&right.source)
                || right.source.starts_with(&left.source)
            {
                bail!(
                    "GQY directory migration has overlapping legacy sources: {} and {}",
                    left.source.display(),
                    right.source.display()
                );
            }
        }
    }
    Ok(active)
}

pub(crate) fn entry_exists(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn acquire_migration_lock(root: &Path) -> Result<MigrationLease> {
    // Lock the directory itself so a failed preflight never leaves a lock file
    // behind in an otherwise untouched destination layout.
    let file = File::open(root)
        .with_context(|| format!("opening migration lock directory {}", root.display()))?;
    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
    if result != 0 {
        return Err(std::io::Error::last_os_error()).context("locking GQY directory migration");
    }
    Ok(MigrationLease(file))
}

pub(crate) fn ensure_private_dir(path: &Path) -> Result<()> {
    ensure_existing_directory(path)?;
    if !entry_exists(path)? {
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true).mode(0o700);
        builder
            .create(path)
            .with_context(|| format!("creating {}", path.display()))?;
    }
    ensure_existing_directory(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .with_context(|| format!("securing {}", path.display()))?;
    Ok(())
}

pub(crate) fn ensure_existing_directory(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => bail!(
            "GQY refuses to use a symbolic-link directory: {}",
            path.display()
        ),
        Ok(metadata) if !metadata.is_dir() => bail!(
            "GQY expected a directory but found another file: {}",
            path.display()
        ),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn layout_marker_exists(layout: &Layout) -> Result<bool> {
    let marker = layout.marker();
    match fs::symlink_metadata(&marker) {
        Ok(metadata) if metadata.file_type().is_symlink() => bail!(
            "GQY layout marker must not be a symbolic link: {}",
            marker.display()
        ),
        Ok(metadata) if !metadata.is_file() => bail!(
            "GQY layout marker is not a regular file: {}",
            marker.display()
        ),
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn write_marker(path: &Path) -> Result<()> {
    let temporary = path.with_extension(format!(
        "tmp-{}-{}",
        std::process::id(),
        rand::random::<u64>()
    ));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&temporary)
        .with_context(|| format!("creating {}", temporary.display()))?;
    file.write_all(b"1\n")?;
    file.sync_all()?;
    fs::rename(&temporary, path)
        .with_context(|| format!("installing migration marker {}", path.display()))?;
    sync_parent(path)?;
    Ok(())
}

pub(crate) fn migrate_entry(source: &Path, destination: &Path) -> Result<()> {
    let mappings = existing_mappings(&[MigrationMapping::new(source, destination)])?;
    preflight_mappings(&mappings)?;
    let Some(mapping) = mappings.first() else {
        return Ok(());
    };
    migrate_entry_unchecked(&mapping.source, &mapping.destination)
}

/// Runs `existing_mappings` + `preflight_mappings`, except that the cache is
/// treated as disposable: caches routinely contain relative symlinks (for
/// example HuggingFace-style blob layouts), and refusing to move one would
/// otherwise brick startup forever over data GQY can rebuild. When the cache
/// tree alone fails preflight it is discarded and dropped from the migration
/// instead of failing it.
pub(crate) fn preflight_with_disposable_cache(
    mappings: &[MigrationMapping],
    legacy_cache: &Path,
) -> Result<Vec<MigrationMapping>> {
    let mut active = existing_mappings(mappings)?;
    if let Some(index) = active
        .iter()
        .position(|mapping| mapping.source == legacy_cache)
    {
        let projections = active
            .iter()
            .map(|mapping| (mapping.source.clone(), mapping.destination.clone()))
            .collect::<Vec<_>>();
        let cache = &active[index];
        let migratable = ensure_supported_entry_tree(&cache.source)
            .and_then(|_| ensure_absolute_symlink_targets_stable(&cache.source, &projections))
            .and_then(|_| ensure_no_conflicts(&cache.source, &cache.destination));
        if let Err(reason) = migratable {
            eprintln!(
                "{}: {reason:#}",
                t(
                    "discarding the legacy GQY cache instead of migrating it",
                    "旧版 GQY 缓存无法迁移，已直接丢弃"
                )
            );
            discard_legacy_cache(&cache.source);
            active.remove(index);
        }
    }
    preflight_mappings(&active)?;
    Ok(active)
}

/// Best effort: a leftover legacy cache is only cold-start overhead, never a
/// reason to fail the migration. `symlink_metadata` keeps a symlinked cache
/// directory from being deleted through the link.
pub(crate) fn discard_legacy_cache(path: &Path) {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return;
    };
    let _ = if metadata.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    };
}

pub(crate) fn preflight_mappings(mappings: &[MigrationMapping]) -> Result<()> {
    let projections = mappings
        .iter()
        .map(|mapping| (mapping.source.clone(), mapping.destination.clone()))
        .collect::<Vec<_>>();
    for mapping in mappings {
        ensure_supported_entry_tree(&mapping.source)?;
        ensure_absolute_symlink_targets_stable(&mapping.source, &projections)?;
        ensure_no_conflicts(&mapping.source, &mapping.destination)?;
    }
    for (index, left) in mappings.iter().enumerate() {
        for right in mappings.iter().skip(index + 1) {
            ensure_mapping_pair_compatible(left, right)?;
        }
    }
    Ok(())
}

pub(crate) fn ensure_supported_entry_tree(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        let target = fs::read_link(path)?;
        if target.is_relative() {
            bail!(
                "GQY directory migration refuses relative symbolic link {}; its target would change after moving",
                path.display()
            );
        }
        return Ok(());
    }
    if metadata.is_file() {
        return Ok(());
    }
    if metadata.is_dir() {
        for entry in fs::read_dir(path)? {
            ensure_supported_entry_tree(&entry?.path())?;
        }
        return Ok(());
    }
    bail!("unsupported file type while migrating {}", path.display())
}

pub(crate) fn ensure_absolute_symlink_targets_stable(
    path: &Path,
    projections: &[(PathBuf, PathBuf)],
) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        let target = fs::read_link(path)?;
        if target.is_absolute() {
            for (source, destination) in projections {
                if let Ok(relative) = target.strip_prefix(source) {
                    bail!(
                        "GQY directory migration refuses symbolic link {} because its absolute target moves from {} to {}",
                        path.display(),
                        target.display(),
                        destination.join(relative).display()
                    );
                }
            }
        }
        return Ok(());
    }
    if metadata.is_dir() {
        for entry in fs::read_dir(path)? {
            ensure_absolute_symlink_targets_stable(&entry?.path(), projections)?;
        }
    }
    Ok(())
}

pub(crate) fn ensure_mapping_pair_compatible(
    left: &MigrationMapping,
    right: &MigrationMapping,
) -> Result<()> {
    if left.destination == right.destination {
        return ensure_projected_entries_compatible(&left.source, &right.source, &left.destination);
    }
    if let Ok(relative) = right.destination.strip_prefix(&left.destination) {
        return ensure_nested_mapping_compatible(left, relative, right);
    }
    if let Ok(relative) = left.destination.strip_prefix(&right.destination) {
        return ensure_nested_mapping_compatible(right, relative, left);
    }
    Ok(())
}

pub(crate) fn ensure_nested_mapping_compatible(
    outer: &MigrationMapping,
    relative_destination: &Path,
    inner: &MigrationMapping,
) -> Result<()> {
    let Some(projected_source) = projected_source_entry(&outer.source, relative_destination)?
    else {
        return Ok(());
    };
    ensure_projected_entries_compatible(&projected_source, &inner.source, &inner.destination)
}

/// Locates the source entry which an outer mapping would project onto a nested
/// destination. A non-directory ancestor is already a conflict because the
/// inner mapping needs that destination path to remain traversable.
pub(crate) fn projected_source_entry(
    source_root: &Path,
    relative: &Path,
) -> Result<Option<PathBuf>> {
    let mut current = source_root.to_path_buf();
    for component in relative.components() {
        let metadata = fs::symlink_metadata(&current)?;
        if !metadata.is_dir() {
            bail!(
                "GQY directory migration found a projected path conflict at {}",
                current.display()
            );
        }
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        }
    }
    Ok(Some(current))
}

pub(crate) fn ensure_projected_entries_compatible(
    left: &Path,
    right: &Path,
    projected_destination: &Path,
) -> Result<()> {
    let left_meta = fs::symlink_metadata(left)?;
    let right_meta = fs::symlink_metadata(right)?;
    if left_meta.is_dir() && right_meta.is_dir() {
        for entry in fs::read_dir(left)? {
            let entry = entry?;
            let right_child = right.join(entry.file_name());
            match fs::symlink_metadata(&right_child) {
                Ok(_) => ensure_projected_entries_compatible(
                    &entry.path(),
                    &right_child,
                    &projected_destination.join(entry.file_name()),
                )?,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        return Ok(());
    }
    if entries_identical(left, &left_meta, right, &right_meta)? {
        return Ok(());
    }
    bail!(
        "GQY directory migration found conflicting legacy entries {} and {} projected to {}",
        left.display(),
        right.display(),
        projected_destination.display()
    )
}
