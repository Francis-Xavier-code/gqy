//! install — 自 src/skills.rs 拆分。

use super::*;

pub(crate) struct StagedDirectory {
    path: PathBuf,
    armed: bool,
}

impl StagedDirectory {
    pub(crate) fn new(path: PathBuf) -> Self {
        Self { path, armed: true }
    }

    pub(crate) fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for StagedDirectory {
    pub(crate) fn drop(&mut self) {
        if self.armed {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

pub(crate) fn exchange_skill_directories(staged: &Path, target: &Path) -> Result<()> {
    exchange_directories(staged, target).with_context(|| {
        format!(
            "atomically exchanging skill directories {} and {}",
            staged.display(),
            target.display()
        )
    })?;
    if let Some(parent) = target.parent() {
        if let Err(error) = File::open(parent).and_then(|directory| directory.sync_all()) {
            tracing::warn!(path = %parent.display(), error = %error, "failed to sync updated skill directory");
        }
    }
    Ok(())
}

pub(crate) fn install_updated_skill(
    staged: &Path,
    target: &Path,
    expected_revision: &str,
    staged_guard: &mut StagedDirectory,
) -> Result<()> {
    exchange_skill_directories(staged, target)?;
    let replaced_revision = skill_revision(staged);
    let replaced_matches = matches!(
        replaced_revision.as_deref(),
        Ok(revision) if revision == expected_revision
    );
    if !replaced_matches {
        if let Err(rollback_error) = exchange_skill_directories(staged, target) {
            staged_guard.disarm();
            bail!(
                "live skill changed during publication and rollback failed; the old version is preserved at {}: {rollback_error:#}",
                staged.display()
            );
        }
        bail!("skill changed during publication; the live version was restored");
    }
    if let Err(error) = fs::remove_dir_all(staged) {
        tracing::warn!(path = %staged.display(), error = %error, "failed to remove replaced skill directory");
    }
    staged_guard.disarm();
    Ok(())
}

#[cfg(target_os = "macos")]
pub(crate) fn install_new_skill(staged: &Path, target: &Path) -> Result<()> {
    use std::os::fd::AsRawFd;

    let (parent, staged, target) = exchange_operands(staged, target)?;
    let result = unsafe {
        libc::renameatx_np(
            parent.as_raw_fd(),
            staged.as_ptr(),
            parent.as_raw_fd(),
            target.as_ptr(),
            libc::RENAME_EXCL,
        )
    };
    if result != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn install_new_skill(staged: &Path, target: &Path) -> Result<()> {
    if target.exists() {
        bail!("skill already exists; create never overwrites");
    }
    fs::rename(staged, target)?;
    Ok(())
}

#[cfg(target_os = "macos")]
pub(crate) fn exchange_directories(left: &Path, right: &Path) -> Result<()> {
    use std::os::fd::AsRawFd;

    let (parent, left, right) = exchange_operands(left, right)?;
    let result = unsafe {
        libc::renameatx_np(
            parent.as_raw_fd(),
            left.as_ptr(),
            parent.as_raw_fd(),
            right.as_ptr(),
            libc::RENAME_SWAP,
        )
    };
    if result != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn exchange_directories(_left: &Path, _right: &Path) -> Result<()> {
    bail!("atomic skill updates are unsupported on this platform")
}

#[cfg(target_os = "macos")]
pub(crate) fn exchange_operands(
    left: &Path,
    right: &Path,
) -> Result<(File, std::ffi::CString, std::ffi::CString)> {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::OpenOptionsExt;

    let parent = left.parent().context("staged skill has no parent")?;
    if right.parent() != Some(parent) {
        bail!("atomic skill exchange requires a shared parent directory");
    }
    let mut options = OpenOptions::new();
    options
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC);
    let parent = options.open(parent)?;
    let left = std::ffi::CString::new(
        left.file_name()
            .context("staged skill has no file name")?
            .as_bytes(),
    )?;
    let right = std::ffi::CString::new(
        right
            .file_name()
            .context("target skill has no file name")?
            .as_bytes(),
    )?;
    Ok((parent, left, right))
}

pub(crate) struct PublishLease {
    _file: File,
}

pub(crate) fn acquire_publish_lock(paths: &GQYPaths) -> Result<PublishLease> {
    let root = paths.skill_drafts_dir();
    create_private_dir(&root)?;
    let lock_path = root.join(PUBLISH_LOCK_FILE);
    match fs::symlink_metadata(&lock_path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            bail!("skill publish lock path is unsafe: {}", lock_path.display())
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let mut options = OpenOptions::new();
    options.create(true).truncate(false).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(&lock_path)?;
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
        if result != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    Ok(PublishLease { _file: file })
}

pub(crate) enum DraftInspection {
    Valid(SystemTime),
    Invalid,
}

pub(crate) fn latest_modified(path: &Path) -> Result<SystemTime> {
    match inspect_latest_modified(path)? {
        DraftInspection::Valid(modified) => Ok(modified),
        DraftInspection::Invalid => bail!("skill draft exceeds inspection limits"),
    }
}

pub(crate) fn inspect_latest_modified(path: &Path) -> Result<DraftInspection> {
    let mut latest = UNIX_EPOCH;
    let mut visited = 0usize;
    let mut pending = vec![(path.to_path_buf(), 0usize)];
    while let Some((path, depth)) = pending.pop() {
        visited += 1;
        if visited > MAX_SKILL_PACKAGE_FILES + MAX_SKILL_PACKAGE_DIRS + 16 {
            return Ok(DraftInspection::Invalid);
        }
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        let modified = match metadata.modified() {
            Ok(modified) => modified,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        if modified > latest {
            latest = modified;
        }
        if metadata.is_dir() && !metadata.file_type().is_symlink() {
            if depth > MAX_SKILL_PACKAGE_DEPTH + 4 {
                return Ok(DraftInspection::Invalid);
            }
            for entry in fs::read_dir(path)? {
                match entry {
                    Ok(entry) => pending.push((entry.path(), depth + 1)),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
            }
        }
    }
    Ok(DraftInspection::Valid(latest))
}

pub(crate) fn unix_time(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub(crate) fn create_private_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    secure_directory(path)
}

pub(crate) fn secure_directory(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!("expected a regular directory: {}", path.display());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

pub(crate) fn write_private_file(path: &Path, content: &[u8]) -> Result<()> {
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(content)?;
    file.sync_all()?;
    Ok(())
}
