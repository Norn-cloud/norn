//! FD-pinned directory handle ([`TrustedDir`]) for traversal, source reads,
//! and atomic relative writes.

use std::path::{Component, Path, PathBuf};

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
pub(crate) struct TrustedDir {
    #[cfg(unix)]
    fd: std::os::fd::OwnedFd,
    display_path: PathBuf,
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
                let parent_ref = current_opt.as_ref().map_or(self.fd.as_fd(), AsFd::as_fd);
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

    /// Reads a strict relative regular file under `self`, failing if missing or non-regular.
    pub(crate) fn read_regular_file_rel(
        &self,
        rel: &Path,
        budget: &mut InvocationBudget,
    ) -> Result<String, String> {
        let display = self.display_path.join(rel);
        self.try_read_regular_file_rel(rel, budget)?
            .ok_or_else(|| format!("{}: file not found", display.display()))
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
            let parent_ref = dir_owned.as_ref().map_or(self.fd.as_fd(), AsFd::as_fd);
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
        let parent_ref = dir_owned.as_ref().map_or(self.fd.as_fd(), AsFd::as_fd);
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

    /// Atomically writes `content` to `rel_file` under `self` by resolving parents via `openat`.
    pub(crate) fn write_rel_file_atomic(
        &self,
        rel_file: &Path,
        display_path: &Path,
        content: &str,
    ) -> Result<(), String> {
        let Some((parent_segs, final_seg)) =
            split_rel_components(rel_file, false, &self.display_path)?
        else {
            return Err(format!(
                "{}: empty relative output path",
                display_path.display()
            ));
        };
        let final_name = final_seg.to_str().ok_or_else(|| {
            format!(
                "{}: output path must have a UTF-8 file name",
                display_path.display()
            )
        })?;
        #[cfg(unix)]
        {
            use std::os::fd::AsFd as _;
            if parent_segs.is_empty() {
                return super::atomic_io::write_file_atomic_in_dir(
                    self.fd.as_fd(),
                    final_name,
                    display_path,
                    content,
                );
            }
            let rel_parent: PathBuf = parent_segs.iter().collect();
            let parent_dir = self.open_subdir_rel(&rel_parent)?;
            super::atomic_io::write_file_atomic_in_dir(
                parent_dir.fd.as_fd(),
                final_name,
                display_path,
                content,
            )
        }
        #[cfg(not(unix))]
        {
            let target = self.display_path.join(rel_file);
            let _unused = (parent_segs, final_name, display_path);
            super::atomic_io::write_file_atomic(&target, content)
        }
    }

    /// Reads all non-hidden top-level regular files directly from `self` in sorted name order.
    pub(crate) fn read_top_level_files(
        &self,
        budget: &mut InvocationBudget,
    ) -> Result<Vec<LoadedFile>, String> {
        #[cfg(unix)]
        {
            use std::os::fd::AsFd as _;
            let names =
                read_sorted_dir_names_budgeted(self.fd.as_fd(), &self.display_path, budget)?;
            let mut files = Vec::new();
            for entry in names {
                let name = entry.to_str().map_err(|err| {
                    format!(
                        "non-UTF-8 name under {}: {err}",
                        self.display_path.display()
                    )
                })?;
                if name.starts_with('.') {
                    continue;
                }
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
                } else if !kind.is_dir() {
                    return Err(format!("{}: not a regular file", child.display()));
                }
            }
            Ok(files)
        }
        #[cfg(not(unix))]
        {
            read_top_level_files_non_unix(&self.display_path, budget)
        }
    }

    /// Recursively walks `self`, skipping hidden entries, `node_modules`, and `target`,
    /// reading accepted regular files directly from their opened descriptors.
    pub(crate) fn walk_files(
        &self,
        accept: fn(&str, &Path) -> bool,
        budget: &mut InvocationBudget,
    ) -> Result<Vec<LoadedFile>, String> {
        let mut files = Vec::new();
        #[cfg(unix)]
        {
            use std::os::fd::AsFd as _;
            walk_unix_dir(
                self.fd.as_fd(),
                &self.display_path,
                Path::new(""),
                0,
                accept,
                budget,
                &mut files,
            )?;
        }
        #[cfg(not(unix))]
        {
            walk_non_unix_dir(
                &self.display_path,
                Path::new(""),
                0,
                accept,
                budget,
                &mut files,
            )?;
        }
        Ok(files)
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
    dir_fd: std::os::fd::BorrowedFd<'_>,
    dir_display: &Path,
    rel_prefix: &Path,
    depth: usize,
    accept: fn(&str, &Path) -> bool,
    budget: &mut InvocationBudget,
    out: &mut Vec<LoadedFile>,
) -> Result<(), String> {
    use std::os::fd::AsFd as _;

    if depth > MAX_WALK_DEPTH {
        return Err(format!(
            "{}: directory nesting exceeds {MAX_WALK_DEPTH}",
            dir_display.display()
        ));
    }
    for entry in read_sorted_dir_names_budgeted(dir_fd, dir_display, budget)? {
        let name = entry
            .to_str()
            .map_err(|err| format!("non-UTF-8 name under {}: {err}", dir_display.display()))?;
        if name.starts_with('.') {
            continue;
        }
        let child_display = dir_display.join(name);
        let child_rel = rel_prefix.join(name);
        let opened = open_entry_no_follow(dir_fd, entry.as_c_str(), &child_display)?;
        let kind = opened
            .metadata()
            .map_err(|err| format!("{}: {err}", child_display.display()))?
            .file_type();
        if kind.is_dir() {
            if should_skip_dir(name) {
                continue;
            }
            walk_unix_dir(
                opened.as_fd(),
                &child_display,
                &child_rel,
                depth.saturating_add(1),
                accept,
                budget,
                out,
            )?;
        } else if kind.is_file() {
            if !accept(name, &child_display) {
                continue;
            }
            budget.charge_accepted_file(&child_display)?;
            let content = read_opened_file_budgeted(opened, &child_display, budget)?;
            out.push(LoadedFile {
                display_path: child_display,
                rel_path: child_rel,
                content,
            });
        } else {
            return Err(format!("{}: not a regular file", child_display.display()));
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn read_top_level_files_non_unix(
    dir: &Path,
    budget: &mut InvocationBudget,
) -> Result<Vec<LoadedFile>, String> {
    let mut entries = Vec::new();
    for item in std::fs::read_dir(dir).map_err(|err| format!("{}: {err}", dir.display()))? {
        let entry = item.map_err(|err| format!("{}: {err}", dir.display()))?;
        budget.charge_scanned_entry(dir)?;
        entries.push(entry);
    }
    entries.sort_by_key(std::fs::DirEntry::file_name);
    let mut files = Vec::new();
    for entry in entries {
        let raw_name = entry.file_name();
        let Some(name) = raw_name.to_str() else {
            return Err(format!("non-UTF-8 name under {}", dir.display()));
        };
        if name.starts_with('.') {
            continue;
        }
        let child = entry.path();
        let kind = entry
            .file_type()
            .map_err(|err| format!("{}: {err}", child.display()))?;
        if kind.is_symlink() {
            return Err(format!("{}: symlinks are refused", child.display()));
        }
        if kind.is_file() {
            budget.charge_accepted_file(&child)?;
            let opened =
                std::fs::File::open(&child).map_err(|err| format!("{}: {err}", child.display()))?;
            let content = read_opened_file_budgeted(opened, &child, budget)?;
            files.push(LoadedFile {
                display_path: child,
                rel_path: PathBuf::from(name),
                content,
            });
        } else if !kind.is_dir() {
            return Err(format!("{}: not a regular file", child.display()));
        }
    }
    Ok(files)
}

#[cfg(not(unix))]
fn walk_non_unix_dir(
    dir: &Path,
    rel_prefix: &Path,
    depth: usize,
    accept: fn(&str, &Path) -> bool,
    budget: &mut InvocationBudget,
    out: &mut Vec<LoadedFile>,
) -> Result<(), String> {
    if depth > MAX_WALK_DEPTH {
        return Err(format!(
            "{}: directory nesting exceeds {MAX_WALK_DEPTH}",
            dir.display()
        ));
    }
    let mut entries = Vec::new();
    for item in std::fs::read_dir(dir).map_err(|err| format!("{}: {err}", dir.display()))? {
        let entry = item.map_err(|err| format!("{}: {err}", dir.display()))?;
        budget.charge_scanned_entry(dir)?;
        entries.push(entry);
    }
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let raw_name = entry.file_name();
        let Some(name) = raw_name.to_str() else {
            return Err(format!("non-UTF-8 name under {}", dir.display()));
        };
        if name.starts_with('.') {
            continue;
        }
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
            walk_non_unix_dir(
                &child,
                &child_rel,
                depth.saturating_add(1),
                accept,
                budget,
                out,
            )?;
        } else if kind.is_file() {
            if !accept(name, &child) {
                continue;
            }
            budget.charge_accepted_file(&child)?;
            let opened =
                std::fs::File::open(&child).map_err(|err| format!("{}: {err}", child.display()))?;
            let content = read_opened_file_budgeted(opened, &child, budget)?;
            out.push(LoadedFile {
                display_path: child,
                rel_path: child_rel,
                content,
            });
        } else {
            return Err(format!("{}: not a regular file", child.display()));
        }
    }
    Ok(())
}

/// Opens a single regular file through its verified parent directory `OwnedFd` and reads its content.
pub(crate) fn open_regular_file_pinned(
    path: &Path,
    budget: &mut InvocationBudget,
) -> Result<(TrustedDir, String, String), String> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        let raw = path.as_os_str().as_bytes();
        if raw.ends_with(b"/") || raw.ends_with(b"/.") {
            return Err(format!("{}: not a regular file", path.display()));
        }
    }
    let file_name = path
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .ok_or_else(|| format!("{}: path must end with a UTF-8 file name", path.display()))?;
    let parent = path
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let parent_dir = TrustedDir::open(parent)?;
    let content = parent_dir.read_regular_file_rel(Path::new(file_name), budget)?;
    Ok((parent_dir, file_name.to_owned(), content))
}
