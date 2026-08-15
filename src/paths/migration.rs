//! migration — 自 src/paths.rs 拆分。

use super::*;

pub(crate) fn ensure_no_conflicts(source: &Path, destination: &Path) -> Result<()> {
    let source_meta = fs::symlink_metadata(source)?;
    match fs::symlink_metadata(destination) {
        Ok(destination_meta) => {
            if source_meta.is_dir() && destination_meta.is_dir() {
                for entry in fs::read_dir(source)? {
                    let entry = entry?;
                    ensure_no_conflicts(&entry.path(), &destination.join(entry.file_name()))?;
                }
                return Ok(());
            }
            if entries_identical(source, &source_meta, destination, &destination_meta)? {
                return Ok(());
            }
            bail!(
                "GQY directory migration found conflicting entries: {} and {}; move or rename one of them and retry",
                source.display(),
                destination.display()
            );
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

pub(crate) fn migrate_entry_unchecked(source: &Path, destination: &Path) -> Result<()> {
    let source_meta = fs::symlink_metadata(source)?;
    match fs::symlink_metadata(destination) {
        Ok(destination_meta) => {
            if source_meta.is_dir() && destination_meta.is_dir() {
                for entry in fs::read_dir(source)? {
                    let entry = entry?;
                    migrate_entry_unchecked(&entry.path(), &destination.join(entry.file_name()))?;
                }
                remove_empty_dir(source)?;
                return Ok(());
            }
            if entries_identical(source, &source_meta, destination, &destination_meta)? {
                remove_entry(source, &source_meta)?;
                return Ok(());
            }
            bail!(
                "GQY directory migration found a conflict that appeared after preflight: {} and {}",
                source.display(),
                destination.display()
            );
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }

    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    match fs::rename(source, destination) {
        Ok(()) => {
            sync_parent(destination)?;
            if source.parent() != destination.parent() {
                sync_parent(source)?;
            }
            Ok(())
        }
        Err(error) if error.raw_os_error() == Some(libc::EXDEV) => {
            copy_entry(source, &source_meta, destination)?;
            remove_entry(source, &source_meta)
        }
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn copy_entry(source: &Path, metadata: &fs::Metadata, destination: &Path) -> Result<()> {
    if metadata.file_type().is_symlink() {
        let target = fs::read_link(source)?;
        symlink(target, destination)?;
        sync_parent(destination)?;
        return Ok(());
    }
    if metadata.is_dir() {
        fs::create_dir(destination)?;
        fs::set_permissions(destination, metadata.permissions())?;
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            let child_meta = fs::symlink_metadata(entry.path())?;
            copy_entry(
                &entry.path(),
                &child_meta,
                &destination.join(entry.file_name()),
            )?;
        }
        File::open(destination)?.sync_all()?;
        sync_parent(destination)?;
        return Ok(());
    }
    if !metadata.is_file() {
        bail!("unsupported file type while migrating {}", source.display());
    }
    let temporary = destination.with_extension(format!(
        "gqy-migrate-{}-{}",
        std::process::id(),
        rand::random::<u64>()
    ));
    let mut input = File::open(source)?;
    let mut output = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(metadata.permissions().mode())
        .open(&temporary)?;
    std::io::copy(&mut input, &mut output)?;
    output.sync_all()?;
    fs::rename(&temporary, destination)?;
    sync_parent(destination)?;
    Ok(())
}

pub(crate) fn sync_parent(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        File::open(parent)?.sync_all()?;
    }
    Ok(())
}

pub(crate) fn entries_identical(
    left: &Path,
    left_meta: &fs::Metadata,
    right: &Path,
    right_meta: &fs::Metadata,
) -> Result<bool> {
    if left_meta.file_type().is_symlink() || right_meta.file_type().is_symlink() {
        return Ok(left_meta.file_type().is_symlink()
            && right_meta.file_type().is_symlink()
            && fs::read_link(left)? == fs::read_link(right)?);
    }
    if !left_meta.is_file()
        || !right_meta.is_file()
        || left_meta.len() != right_meta.len()
        || left_meta.permissions().mode() & 0o7777 != right_meta.permissions().mode() & 0o7777
    {
        return Ok(false);
    }
    let mut left_file = File::open(left)?;
    let mut right_file = File::open(right)?;
    let mut left_buffer = [0u8; 64 * 1024];
    let mut right_buffer = [0u8; 64 * 1024];
    loop {
        let left_count = left_file.read(&mut left_buffer)?;
        let right_count = right_file.read(&mut right_buffer)?;
        if left_count != right_count || left_buffer[..left_count] != right_buffer[..right_count] {
            return Ok(false);
        }
        if left_count == 0 {
            return Ok(true);
        }
    }
}

pub(crate) fn remove_entry(path: &Path, metadata: &fs::Metadata) -> Result<()> {
    if metadata.is_dir() {
        fs::remove_dir_all(path)?;
    } else {
        fs::remove_file(path)?;
    }
    sync_parent(path)
}

pub(crate) fn remove_empty_dir(path: &Path) -> Result<()> {
    match fs::remove_dir(path) {
        Ok(()) => sync_parent(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

