//! FD-pinned directory handle ([`TrustedDir`]) for traversal, source reads,
//! and single-open target pinning.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

#[cfg(not(unix))]
use super::fs_io::verify_no_symlink_ancestors_non_unix;
use super::fs_io::{
    InvocationBudget, LoadedFile, MAX_WALK_DEPTH, read_opened_file_budgeted, should_skip_dir,
};
#[cfg(unix)]
use super::fs_io::{
    ancestor_dir_flags, final_dir_flags, format_dir_open_error, open_dir_components_no_follow,
    open_entry_no_follow,
};

/// A trusted directory handle pinned to an `OwnedFd` on Unix (or verified path on non-Unix).
#[derive(Debug)]
pub(crate) struct TrustedDir {
    #[cfg(unix)]
    fd: std::os::fd::OwnedFd,
    display_path: PathBuf,
}

/// A regular file read from a trusted directory together with its retained parent handle.
pub(crate) struct PinnedLoadedFile {
    pub(crate) parent: Arc<TrustedDir>,
    pub(crate) file_name: String,
    pub(crate) display_path: PathBuf,
    pub(crate) content: String,
}

/// Result of a single non-recursive root scan: top-level files and pinned child subdirectories.
#[derive(Debug)]
pub(crate) struct TopLevelScan {
    pub(crate) files: Vec<LoadedFile>,
    pub(crate) subdirs: Vec<(PathBuf, Arc<TrustedDir>)>,
}

/// Single-open FD-pinned CLI source target (either a directory or a pinned regular file).
pub(crate) enum PinnedSourceTarget {
    Directory(TrustedDir),
    File {
        parent_dir: TrustedDir,
        file_name: String,
        display_path: PathBuf,
        opened: std::fs::File,
    },
}

impl TrustedDir {
    /// Constructs a [`TrustedDir`] from an already-verified `OwnedFd`.
    #[cfg(unix)]
    pub(crate) const fn from_fd(fd: std::os::fd::OwnedFd, display_path: PathBuf) -> Self {
        Self { fd, display_path }
    }

    /// Borrows the underlying directory descriptor.
    #[cfg(unix)]
    pub(crate) fn as_fd(&self) -> std::os::fd::BorrowedFd<'_> {
        use std::os::fd::AsFd as _;
        self.fd.as_fd()
    }

    /// Opens `dir` component-by-component without following symlinks.
    pub(crate) fn open(dir: &Path) -> Result<Self, String> {
        #[cfg(unix)]
        {
            let fd = open_dir_components_no_follow(dir)?;
            Ok(Self {
                fd,
                display_path: dir.to_path_buf(),
            })
        }
        #[cfg(not(unix))]
        {
            verify_no_symlink_ancestors_non_unix(dir)?;
            let meta = std::fs::symlink_metadata(dir)
                .map_err(|err| format!("{}: {err}", dir.display()))?;
            if meta.file_type().is_symlink() {
                return Err(format!("{}: symlinks are refused", dir.display()));
            }
            if !meta.is_dir() {
                return Err(format!("{}: not a directory", dir.display()));
            }
            Ok(Self {
                display_path: dir.to_path_buf(),
            })
        }
    }

    /// Returns the display path associated with this trusted directory.
    pub(crate) fn display_path(&self) -> &Path {
        &self.display_path
    }

    /// Opens a strict relative subdirectory (only `Component::Normal` segments, no symlinks).
    pub(crate) fn open_subdir_rel(&self, rel: &Path) -> Result<Self, String> {
        let mut segments: Vec<&std::ffi::OsStr> = Vec::new();
        for comp in rel.components() {
            match comp {
                Component::Normal(seg) => segments.push(seg),
                Component::CurDir => {}
                Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                    return Err(format!(
                        "{}: relative path `{}` must stay inside directory",
                        self.display_path.display(),
                        rel.display()
                    ));
                }
            }
        }
        if segments.is_empty() {
            return Err(format!(
                "{}: relative subdirectory path must not be empty",
                self.display_path.display()
            ));
        }
        #[cfg(unix)]
        {
            use rustix::fs::{Mode, openat};
            use std::os::fd::AsFd;

            let mut walked = self.display_path.clone();
            let mut current_opt: Option<std::os::fd::OwnedFd> = None;
            let last_idx = segments.len().saturating_sub(1);
            for (idx, seg) in segments.iter().enumerate() {
                let flags = if idx == last_idx {
                    final_dir_flags()
                } else {
                    ancestor_dir_flags()
                };
                walked.push(seg);
                let parent_ref = current_opt
                    .as_ref()
                    .map_or_else(|| self.fd.as_fd(), AsFd::as_fd);
                let opened = openat(parent_ref, *seg, flags, Mode::empty())
                    .map_err(|err| format_dir_open_error(parent_ref, seg, &walked, err))?;
                current_opt = Some(opened);
            }
            let fd = current_opt.ok_or_else(|| {
                format!(
                    "{}: failed to open relative subdirectory",
                    self.display_path.display()
                )
            })?;
            Ok(Self {
                fd,
                display_path: walked,
            })
        }
        #[cfg(not(unix))]
        {
            let mut walked = self.display_path.clone();
            for seg in segments {
                walked.push(seg);
                let meta = std::fs::symlink_metadata(&walked)
                    .map_err(|err| format!("{}: {err}", walked.display()))?;
                if meta.file_type().is_symlink() {
                    return Err(format!("{}: symlinks are refused", walked.display()));
                }
                if !meta.is_dir() {
                    return Err(format!("{}: not a directory", walked.display()));
                }
            }
            Ok(Self {
                display_path: walked,
            })
        }
    }

    /// Reads a strict relative regular file under `self`, returning `Ok(None)` only on `ENOENT`.
    pub(crate) fn try_read_regular_file_rel(
        &self,
        rel: &Path,
        budget: &mut InvocationBudget,
    ) -> Result<Option<String>, String> {
        let Some((parent_segs, final_seg)) = split_rel_components(rel, false, &self.display_path)?
        else {
            return Err(format!(
                "{}: empty relative file path",
                self.display_path.display()
            ));
        };
        self.open_and_read_segments(&parent_segs, final_seg, false, budget)
    }

    /// Probes a relative import path (`./` or `../`) under `self`, refusing symlinks and
    /// returning `Ok(None)` when absent or when the probe points to a directory.
    pub(crate) fn probe_import_file_rel(
        &self,
        rel: &Path,
        budget: &mut InvocationBudget,
    ) -> Result<Option<String>, String> {
        let Some((parent_segs, final_seg)) = split_rel_components(rel, true, &self.display_path)?
        else {
            return Ok(None);
        };
        self.open_and_read_segments(&parent_segs, final_seg, true, budget)
    }

    #[cfg(unix)]
    fn open_and_read_segments(
        &self,
        parent_segs: &[&std::ffi::OsStr],
        final_seg: &std::ffi::OsStr,
        ignore_dir_target: bool,
        budget: &mut InvocationBudget,
    ) -> Result<Option<String>, String> {
        use rustix::fs::{AtFlags, FileType, Mode, OFlags, openat, statat};
        use rustix::io::Errno;
        use std::os::fd::AsFd;

        let mut walked = self.display_path.clone();
        let mut dir_owned: Option<std::os::fd::OwnedFd> = None;
        for seg in parent_segs {
            walked.push(seg);
            let parent_ref = dir_owned
                .as_ref()
                .map_or_else(|| self.fd.as_fd(), AsFd::as_fd);
            match openat(parent_ref, *seg, ancestor_dir_flags(), Mode::empty()) {
                Ok(opened) => dir_owned = Some(opened),
                Err(err) if err == Errno::NOENT => return Ok(None),
                Err(err) if err == Errno::NOTDIR => {
                    if let Ok(stat) = statat(parent_ref, *seg, AtFlags::SYMLINK_NOFOLLOW)
                        && FileType::from_raw_mode(stat.st_mode) == FileType::Symlink
                    {
                        return Err(format!("{}: symlinks are refused", walked.display()));
                    }
                    if ignore_dir_target {
                        return Ok(None);
                    }
                    return Err(format!("{}: not a directory", walked.display()));
                }
                Err(err) => return Err(format_dir_open_error(parent_ref, seg, &walked, err)),
            }
        }
        walked.push(final_seg);
        let parent_ref = dir_owned
            .as_ref()
            .map_or_else(|| self.fd.as_fd(), AsFd::as_fd);
        let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
        let opened = match openat(parent_ref, final_seg, flags, Mode::empty()) {
            Ok(fd) => std::fs::File::from(fd),
            Err(err) if err == Errno::NOENT => return Ok(None),
            Err(err) if err == Errno::LOOP => {
                return Err(format!("{}: symlinks are refused", walked.display()));
            }
            Err(err) if err == Errno::NXIO || err == Errno::OPNOTSUPP => {
                return Err(format!("{}: not a regular file", walked.display()));
            }
            Err(err) => return Err(format!("{}: {err}", walked.display())),
        };
        let meta = opened
            .metadata()
            .map_err(|err| format!("{}: {err}", walked.display()))?;
        if meta.file_type().is_dir() {
            if ignore_dir_target {
                return Ok(None);
            }
            return Err(format!("{}: not a regular file", walked.display()));
        }
        if !meta.file_type().is_file() {
            return Err(format!("{}: not a regular file", walked.display()));
        }
        budget.charge_accepted_file(&walked)?;
        read_opened_file_budgeted(opened, &walked, budget).map(Some)
    }

    #[cfg(not(unix))]
    fn open_and_read_segments(
        &self,
        parent_segs: &[&std::ffi::OsStr],
        final_seg: &std::ffi::OsStr,
        ignore_dir_target: bool,
        budget: &mut InvocationBudget,
    ) -> Result<Option<String>, String> {
        let mut walked = self.display_path.clone();
        for seg in parent_segs {
            walked.push(seg);
            let meta = match std::fs::symlink_metadata(&walked) {
                Ok(meta) => meta,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(err) => return Err(format!("{}: {err}", walked.display())),
            };
            if meta.file_type().is_symlink() {
                return Err(format!("{}: symlinks are refused", walked.display()));
            }
            if !meta.is_dir() {
                if ignore_dir_target {
                    return Ok(None);
                }
                return Err(format!("{}: not a directory", walked.display()));
            }
        }
        walked.push(final_seg);
        let meta = match std::fs::symlink_metadata(&walked) {
            Ok(meta) => meta,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(format!("{}: {err}", walked.display())),
        };
        if meta.file_type().is_symlink() {
            return Err(format!("{}: symlinks are refused", walked.display()));
        }
        if meta.is_dir() {
            if ignore_dir_target {
                return Ok(None);
            }
            return Err(format!("{}: not a regular file", walked.display()));
        }
        if !meta.is_file() {
            return Err(format!("{}: not a regular file", walked.display()));
        }
        let opened =
            std::fs::File::open(&walked).map_err(|err| format!("{}: {err}", walked.display()))?;
        budget.charge_accepted_file(&walked)?;
        read_opened_file_budgeted(opened, &walked, budget).map(Some)
    }

    /// Scans immediate entries of `self` once in sorted name order, loading top-level
    /// regular files and retaining pinned child [`TrustedDir`] handles for non-skipped
    /// subdirectories.
    ///
    /// Inspects every non-`.`/`..` entry (including hidden dotfiles/dotdirs) without
    /// following symlinks so hidden symlinks and special files always fail closed.
    #[cfg(unix)]
    pub(crate) fn scan_top_level(
        &self,
        budget: &mut InvocationBudget,
    ) -> Result<TopLevelScan, String> {
        use std::os::fd::AsFd as _;

        let mut files = Vec::new();
        let mut subdirs = Vec::new();
        for entry in read_sorted_dir_names_budgeted(self.fd.as_fd(), &self.display_path, budget)? {
            let name = entry.to_str().map_err(|err| {
                format!(
                    "non-UTF-8 name under {}: {err}",
                    self.display_path.display()
                )
            })?;
            let child = self.display_path.join(name);
            let opened = open_entry_no_follow(self.fd.as_fd(), entry.as_c_str(), &child)?;
            let kind = opened
                .metadata()
                .map_err(|err| format!("{}: {err}", child.display()))?
                .file_type();
            if kind.is_file() {
                budget.charge_accepted_file(&child)?;
                let content = read_opened_file_budgeted(opened, &child, budget)?;
                files.push(LoadedFile {
                    display_path: child,
                    rel_path: PathBuf::from(name),
                    content,
                });
            } else if kind.is_dir() {
                if !should_skip_dir(name) {
                    let child_dir = Self::from_fd(std::os::fd::OwnedFd::from(opened), child);
                    subdirs.push((PathBuf::from(name), Arc::new(child_dir)));
                }
            } else {
                return Err(format!("{}: not a regular file", child.display()));
            }
        }
        Ok(TopLevelScan { files, subdirs })
    }

    /// Scans immediate entries of `self` once in sorted name order, loading top-level
    /// regular files and retaining pinned child [`TrustedDir`] handles for non-skipped
    /// subdirectories.
    ///
    /// Inspects every non-`.`/`..` entry (including hidden dotfiles/dotdirs) without
    /// following symlinks so hidden symlinks and special files always fail closed.
    #[cfg(not(unix))]
    pub(crate) fn scan_top_level(
        &self,
        budget: &mut InvocationBudget,
    ) -> Result<TopLevelScan, String> {
        let mut files = Vec::new();
        let mut subdirs = Vec::new();
        for entry in read_sorted_entries_non_unix(&self.display_path, budget)? {
            let raw_name = entry.file_name();
            let Some(name) = raw_name.to_str() else {
                return Err(format!(
                    "non-UTF-8 name under {}",
                    self.display_path.display()
                ));
            };
            let child = entry.path();
            let kind = std::fs::symlink_metadata(&child)
                .map_err(|err| format!("{}: {err}", child.display()))?
                .file_type();
            if kind.is_symlink() {
                return Err(format!("{}: symlinks are refused", child.display()));
            }
            if kind.is_file() {
                budget.charge_accepted_file(&child)?;
                let opened = std::fs::File::open(&child)
                    .map_err(|err| format!("{}: {err}", child.display()))?;
                let content = read_opened_file_budgeted(opened, &child, budget)?;
                files.push(LoadedFile {
                    display_path: child,
                    rel_path: PathBuf::from(name),
                    content,
                });
            } else if kind.is_dir() {
                if !should_skip_dir(name) {
                    let child_dir = Self {
                        display_path: child,
                    };
                    subdirs.push((PathBuf::from(name), Arc::new(child_dir)));
                }
            } else {
                return Err(format!("{}: not a regular file", child.display()));
            }
        }
        Ok(TopLevelScan { files, subdirs })
    }

    /// Recursively walks `self`, retaining the exact opened parent [`TrustedDir`] handle
    /// alongside each accepted regular file so callers can write back without reopening paths.
    pub(crate) fn walk_files_pinned(
        self: &Arc<Self>,
        accept: fn(&str, &Path) -> bool,
        budget: &mut InvocationBudget,
    ) -> Result<Vec<PinnedLoadedFile>, String> {
        let mut files = Vec::new();
        walk_dir_shared(
            self,
            Path::new(""),
            0,
            accept,
            budget,
            &mut |parent, name, display_path, _rel_path, content| {
                files.push(PinnedLoadedFile {
                    parent: Arc::clone(parent),
                    file_name: name.to_owned(),
                    display_path,
                    content,
                });
            },
        )?;
        Ok(files)
    }
}

/// Recursively reads accepted files from already-pinned child [`TrustedDir`] handles
/// without re-enumerating the parent root directory or reopening any top-level files.
pub(crate) fn read_files_in_pinned_subdirs(
    subdirs: &[(PathBuf, Arc<TrustedDir>)],
    accept: fn(&str, &Path) -> bool,
    budget: &mut InvocationBudget,
) -> Result<Vec<LoadedFile>, String> {
    let mut files = Vec::new();
    for (rel_dir, child_dir) in subdirs {
        walk_dir_shared(
            child_dir,
            rel_dir,
            1,
            accept,
            budget,
            &mut |_parent, _fname, display_path, rel_path, content| {
                files.push(LoadedFile {
                    display_path,
                    rel_path,
                    content,
                });
            },
        )?;
    }
    Ok(files)
}

type OnWalkFile = dyn FnMut(&Arc<TrustedDir>, &str, PathBuf, PathBuf, String);

fn walk_dir_shared(
    root: &Arc<TrustedDir>,
    rel_prefix: &Path,
    depth: usize,
    accept: fn(&str, &Path) -> bool,
    budget: &mut InvocationBudget,
    on_file: &mut OnWalkFile,
) -> Result<(), String> {
    #[cfg(unix)]
    {
        walk_unix_dir(root, rel_prefix, depth, accept, budget, on_file)
    }
    #[cfg(not(unix))]
    {
        walk_non_unix_dir(root, rel_prefix, depth, accept, budget, on_file)
    }
}

fn split_rel_components<'a>(
    rel: &'a Path,
    allow_parent_dir: bool,
    base_display: &Path,
) -> Result<Option<(Vec<&'a std::ffi::OsStr>, &'a std::ffi::OsStr)>, String> {
    let mut segments: Vec<&'a std::ffi::OsStr> = Vec::new();
    for comp in rel.components() {
        match comp {
            Component::CurDir => {}
            Component::Normal(seg) => segments.push(seg),
            Component::ParentDir => {
                if !allow_parent_dir {
                    return Err(format!(
                        "{}: relative path `{}` must stay inside directory",
                        base_display.display(),
                        rel.display()
                    ));
                }
                segments.push(std::ffi::OsStr::new(".."));
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(format!(
                    "{}: expected relative path, got `{}`",
                    base_display.display(),
                    rel.display()
                ));
            }
        }
    }
    let Some(final_seg) = segments.pop() else {
        return Ok(None);
    };
    if final_seg == std::ffi::OsStr::new("..") {
        return Err(format!(
            "{}: relative file path `{}` cannot end with `..`",
            base_display.display(),
            rel.display()
        ));
    }
    Ok(Some((segments, final_seg)))
}

#[cfg(unix)]
fn read_sorted_dir_names_budgeted(
    dir_fd: std::os::fd::BorrowedFd<'_>,
    display_dir: &Path,
    budget: &mut InvocationBudget,
) -> Result<Vec<std::ffi::CString>, String> {
    let mut names = Vec::new();
    for item in rustix::fs::Dir::read_from(dir_fd)
        .map_err(|err| format!("{}: {err}", display_dir.display()))?
    {
        let entry = item.map_err(|err| format!("{}: {err}", display_dir.display()))?;
        let owned = entry.file_name().to_owned();
        if owned.as_bytes() == b"." || owned.as_bytes() == b".." {
            continue;
        }
        budget.charge_scanned_entry(display_dir)?;
        names.push(owned);
    }
    names.sort();
    Ok(names)
}

#[cfg(unix)]
fn walk_unix_dir(
    dir: &Arc<TrustedDir>,
    rel_prefix: &Path,
    depth: usize,
    accept: fn(&str, &Path) -> bool,
    budget: &mut InvocationBudget,
    on_file: &mut OnWalkFile,
) -> Result<(), String> {
    let dir_display = dir.display_path();
    if depth > MAX_WALK_DEPTH {
        return Err(format!(
            "{}: directory nesting exceeds {MAX_WALK_DEPTH}",
            dir_display.display()
        ));
    }
    for entry in read_sorted_dir_names_budgeted(dir.as_fd(), dir_display, budget)? {
        let name = entry
            .to_str()
            .map_err(|err| format!("non-UTF-8 name under {}: {err}", dir_display.display()))?;
        let child_display = dir_display.join(name);
        let child_rel = rel_prefix.join(name);
        let opened = open_entry_no_follow(dir.as_fd(), entry.as_c_str(), &child_display)?;
        let kind = opened
            .metadata()
            .map_err(|err| format!("{}: {err}", child_display.display()))?
            .file_type();
        if kind.is_dir() {
            if should_skip_dir(name) {
                continue;
            }
            let child_dir = Arc::new(TrustedDir::from_fd(
                std::os::fd::OwnedFd::from(opened),
                child_display,
            ));
            walk_unix_dir(
                &child_dir,
                &child_rel,
                depth.saturating_add(1),
                accept,
                budget,
                on_file,
            )?;
        } else if kind.is_file() {
            if !accept(name, &child_display) {
                continue;
            }
            budget.charge_accepted_file(&child_display)?;
            let content = read_opened_file_budgeted(opened, &child_display, budget)?;
            on_file(dir, name, child_display, child_rel, content);
        } else {
            return Err(format!("{}: not a regular file", child_display.display()));
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn read_sorted_entries_non_unix(
    dir: &Path,
    budget: &mut InvocationBudget,
) -> Result<Vec<std::fs::DirEntry>, String> {
    let mut entries = Vec::new();
    for item in std::fs::read_dir(dir).map_err(|err| format!("{}: {err}", dir.display()))? {
        let entry = item.map_err(|err| format!("{}: {err}", dir.display()))?;
        budget.charge_scanned_entry(dir)?;
        entries.push(entry);
    }
    entries.sort_by_key(std::fs::DirEntry::file_name);
    Ok(entries)
}

#[cfg(not(unix))]
fn walk_non_unix_dir(
    dir: &Arc<TrustedDir>,
    rel_prefix: &Path,
    depth: usize,
    accept: fn(&str, &Path) -> bool,
    budget: &mut InvocationBudget,
    on_file: &mut OnWalkFile,
) -> Result<(), String> {
    let dir_path = dir.display_path();
    if depth > MAX_WALK_DEPTH {
        return Err(format!(
            "{}: directory nesting exceeds {MAX_WALK_DEPTH}",
            dir_path.display()
        ));
    }
    let entries = read_sorted_entries_non_unix(dir_path, budget)?;
    for entry in entries {
        let raw_name = entry.file_name();
        let Some(name) = raw_name.to_str() else {
            return Err(format!("non-UTF-8 name under {}", dir_path.display()));
        };
        let child = entry.path();
        let child_rel = rel_prefix.join(name);
        let meta = std::fs::symlink_metadata(&child)
            .map_err(|err| format!("{}: {err}", child.display()))?;
        let kind = meta.file_type();
        if kind.is_symlink() {
            return Err(format!("{}: symlinks are refused", child.display()));
        }
        if kind.is_dir() {
            if should_skip_dir(name) {
                continue;
            }
            let child_dir = Arc::new(TrustedDir {
                display_path: child,
            });
            walk_non_unix_dir(
                &child_dir,
                &child_rel,
                depth.saturating_add(1),
                accept,
                budget,
                on_file,
            )?;
        } else if kind.is_file() {
            if !accept(name, &child) {
                continue;
            }
            budget.charge_accepted_file(&child)?;
            let opened =
                std::fs::File::open(&child).map_err(|err| format!("{}: {err}", child.display()))?;
            let content = read_opened_file_budgeted(opened, &child, budget)?;
            on_file(dir, name, child, child_rel, content);
        } else {
            return Err(format!("{}: not a regular file", child.display()));
        }
    }
    Ok(())
}

/// Opens and pins `path` ONCE without following symlinks, returning either a pinned
/// [`TrustedDir`] (for directories) or a pinned parent [`TrustedDir`] plus opened regular [`std::fs::File`].
pub(crate) fn open_source_target_pinned(path: &Path) -> Result<PinnedSourceTarget, String> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;

        let raw = path.as_os_str().as_bytes();
        let must_be_dir = raw.ends_with(b"/") || raw.ends_with(b"/.");
        let Some(final_name) = path.file_name() else {
            return Ok(PinnedSourceTarget::Directory(TrustedDir::open(path)?));
        };
        let parent = path
            .parent()
            .filter(|dir| !dir.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let parent_dir = TrustedDir::open(parent)?;
        let opened = open_entry_no_follow(parent_dir.as_fd(), final_name, path)?;
        let kind = opened
            .metadata()
            .map_err(|err| format!("{}: {err}", path.display()))?
            .file_type();
        if kind.is_dir() {
            return Ok(PinnedSourceTarget::Directory(TrustedDir::from_fd(
                std::os::fd::OwnedFd::from(opened),
                path.to_path_buf(),
            )));
        }
        if kind.is_file() {
            if must_be_dir {
                return Err(format!("{}: not a directory", path.display()));
            }
            let file_name = final_name.to_str().ok_or_else(|| {
                format!("{}: path must end with a UTF-8 file name", path.display())
            })?;
            return Ok(PinnedSourceTarget::File {
                parent_dir,
                file_name: file_name.to_owned(),
                display_path: path.to_path_buf(),
                opened,
            });
        }
        Err(format!(
            "{}: not a regular file or directory",
            path.display()
        ))
    }
    #[cfg(not(unix))]
    {
        if let Some(parent) = path.parent().filter(|dir| !dir.as_os_str().is_empty()) {
            verify_no_symlink_ancestors_non_unix(parent)?;
        }
        let meta =
            std::fs::symlink_metadata(path).map_err(|err| format!("{}: {err}", path.display()))?;
        if meta.file_type().is_symlink() {
            return Err(format!("{}: symlinks are refused", path.display()));
        }
        if meta.is_dir() {
            return Ok(PinnedSourceTarget::Directory(TrustedDir::open(path)?));
        }
        if meta.is_file() {
            let file_name = path
                .file_name()
                .and_then(std::ffi::OsStr::to_str)
                .ok_or_else(|| {
                    format!("{}: path must end with a UTF-8 file name", path.display())
                })?;
            let parent = path
                .parent()
                .filter(|dir| !dir.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."));
            let parent_dir = TrustedDir::open(parent)?;
            let opened =
                std::fs::File::open(path).map_err(|err| format!("{}: {err}", path.display()))?;
            return Ok(PinnedSourceTarget::File {
                parent_dir,
                file_name: file_name.to_owned(),
                display_path: path.to_path_buf(),
                opened,
            });
        }
        Err(format!(
            "{}: not a regular file or directory",
            path.display()
        ))
    }
}

/// Opens a single regular file through its verified parent directory `OwnedFd` and reads its content.
pub(crate) fn open_regular_file_pinned(
    path: &Path,
    budget: &mut InvocationBudget,
) -> Result<(TrustedDir, String, String), String> {
    match open_source_target_pinned(path)? {
        PinnedSourceTarget::File {
            parent_dir,
            file_name,
            display_path,
            opened,
        } => {
            budget.charge_accepted_file(&display_path)?;
            let content = read_opened_file_budgeted(opened, &display_path, budget)?;
            Ok((parent_dir, file_name, content))
        }
        PinnedSourceTarget::Directory(_) => Err(format!("{}: not a regular file", path.display())),
    }
}
