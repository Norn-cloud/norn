//! FD-pinned atomic file writer for native Rust authoring commands.
//!
//! On Unix, every operation (`openat` parent traversal, `statat` target check,
//! exclusive temp-file creation, `renameat`, and `unlinkat` cleanup) runs
//! through a single parent directory `OwnedFd` opened component-by-component
//! with `O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC`.

use std::io::Write as _;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use super::fs_io::MAX_OUTPUT_BYTES;

static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

fn validate_single_file_name(name: &str, display_path: &Path) -> Result<(), String> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.contains('\\')
        || name.contains('\0')
    {
        return Err(format!(
            "{}: output path must end with a safe regular file name",
            display_path.display()
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn refuse_unsafe_existing_target(
    parent_fd: std::os::fd::BorrowedFd<'_>,
    final_name: &str,
    display_path: &Path,
) -> Result<(), String> {
    use rustix::fs::{AtFlags, FileType, statat};
    use rustix::io::Errno;

    match statat(parent_fd, final_name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) => match FileType::from_raw_mode(stat.st_mode) {
            FileType::RegularFile => Ok(()),
            FileType::Symlink => Err(format!(
                "{}: refusing to overwrite symlink",
                display_path.display()
            )),
            FileType::Directory => Err(format!(
                "{}: target path is a directory",
                display_path.display()
            )),
            FileType::Fifo
            | FileType::Socket
            | FileType::CharacterDevice
            | FileType::BlockDevice
            | FileType::Unknown => Err(format!(
                "{}: target path is not a regular file",
                display_path.display()
            )),
        },
        Err(err) if err == Errno::NOENT => Ok(()),
        Err(err) => Err(format!("{}: {err}", display_path.display())),
    }
}

/// Atomically writes `content` to `final_name` inside an already-verified parent directory fd.
#[cfg(unix)]
pub(crate) fn write_file_atomic_in_dir(
    parent_fd: std::os::fd::BorrowedFd<'_>,
    final_name: &str,
    display_path: &Path,
    content: &str,
) -> Result<(), String> {
    use rustix::fs::{AtFlags, Mode, OFlags, openat, renameat, unlinkat};

    if content.len() > MAX_OUTPUT_BYTES {
        return Err(format!(
            "{}: output exceeds {MAX_OUTPUT_BYTES} byte limit",
            display_path.display()
        ));
    }
    validate_single_file_name(final_name, display_path)?;
    refuse_unsafe_existing_target(parent_fd, final_name, display_path)?;

    let seq = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp_name = format!(".{final_name}.norn-tmp-{}-{seq}", std::process::id());
    let create_flags =
        OFlags::CREATE | OFlags::EXCL | OFlags::WRONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mode = Mode::RUSR | Mode::WUSR | Mode::RGRP | Mode::ROTH;
    let tmp_fd = openat(parent_fd, tmp_name.as_str(), create_flags, mode)
        .map_err(|err| format!("{}: {err}", display_path.display()))?;
    let mut handle = std::fs::File::from(tmp_fd);
    let write_res = handle
        .write_all(content.as_bytes())
        .and_then(|()| handle.flush())
        .and_then(|()| handle.sync_data());
    drop(handle);
    if let Err(err) = write_res {
        if let Err(_cleanup) = unlinkat(parent_fd, tmp_name.as_str(), AtFlags::empty()) {}
        return Err(format!("{}: {err}", display_path.display()));
    }
    if let Err(err) = refuse_unsafe_existing_target(parent_fd, final_name, display_path) {
        if let Err(_cleanup) = unlinkat(parent_fd, tmp_name.as_str(), AtFlags::empty()) {}
        return Err(err);
    }
    if let Err(err) = renameat(parent_fd, tmp_name.as_str(), parent_fd, final_name) {
        if let Err(_cleanup) = unlinkat(parent_fd, tmp_name.as_str(), AtFlags::empty()) {}
        return Err(format!("{}: {err}", display_path.display()));
    }
    Ok(())
}

/// Atomically writes `content` to `path`, refusing symlinks along parent directories or the target.
#[cfg(unix)]
pub(crate) fn write_file_atomic(path: &Path, content: &str) -> Result<(), String> {
    use std::os::fd::AsFd as _;
    use std::os::unix::ffi::OsStrExt as _;

    if content.len() > MAX_OUTPUT_BYTES {
        return Err(format!(
            "{}: output exceeds {MAX_OUTPUT_BYTES} byte limit",
            path.display()
        ));
    }
    let raw = path.as_os_str().as_bytes();
    if raw.ends_with(b"/") || raw.ends_with(b"/.") {
        return Err(format!("{}: target path is a directory", path.display()));
    }
    let final_name = path
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .ok_or_else(|| {
            format!(
                "{}: output path must have a UTF-8 file name",
                path.display()
            )
        })?;
    validate_single_file_name(final_name, path)?;
    let parent = path
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let parent_fd = super::fs_io::open_dir_components_no_follow(parent)?;
    write_file_atomic_in_dir(parent_fd.as_fd(), final_name, path, content)
}

/// Atomically writes `content` to `path`, refusing symlinks, directories, and oversized writes.
#[cfg(not(unix))]
pub(crate) fn write_file_atomic(path: &Path, content: &str) -> Result<(), String> {
    if content.len() > MAX_OUTPUT_BYTES {
        return Err(format!(
            "{}: output exceeds {MAX_OUTPUT_BYTES} byte limit",
            path.display()
        ));
    }
    let final_name = path
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .ok_or_else(|| {
            format!(
                "{}: output path must have a UTF-8 file name",
                path.display()
            )
        })?;
    validate_single_file_name(final_name, path)?;
    let parent = path
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    super::fs_io::verify_no_symlink_ancestors_non_unix(parent)?;
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        if meta.file_type().is_symlink() {
            return Err(format!("{}: refusing to overwrite symlink", path.display()));
        }
        if meta.is_dir() {
            return Err(format!("{}: target path is a directory", path.display()));
        }
    }
    let seq = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp_name = format!(".{final_name}.norn-tmp-{}-{seq}", std::process::id());
    let tmp_path = parent.join(tmp_name);
    let mut handle = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp_path)
        .map_err(|err| format!("{}: {err}", tmp_path.display()))?;
    let write_res = handle
        .write_all(content.as_bytes())
        .and_then(|()| handle.flush())
        .and_then(|()| handle.sync_data());
    drop(handle);
    if let Err(err) = write_res {
        if let Err(_cleanup) = std::fs::remove_file(&tmp_path) {}
        return Err(format!("{}: {err}", path.display()));
    }
    if let Err(err) = std::fs::rename(&tmp_path, path) {
        if let Err(_cleanup) = std::fs::remove_file(&tmp_path) {}
        return Err(format!("{}: {err}", path.display()));
    }
    Ok(())
}
