//! Bounded, symlink-safe filesystem primitives and [`InvocationBudget`] for native Rust authoring.

use std::io::Read as _;
use std::path::{Component, Path, PathBuf};

/// Maximum single authoring source file size (`4 MiB`).
pub(crate) const MAX_SOURCE_FILE_BYTES: u64 = 4 * 1024 * 1024;

/// Maximum single emitted output document size (`32 MiB`).
pub(crate) const MAX_OUTPUT_BYTES: usize = 32 * 1024 * 1024;

/// Maximum recursive directory walk depth.
pub(crate) const MAX_WALK_DEPTH: usize = 32;

/// Maximum raw directory entries scanned across one CLI invocation.
pub(crate) const MAX_SCANNED_ENTRIES: usize = 16_384;

/// Maximum accepted files read across one CLI invocation.
pub(crate) const MAX_TREE_FILES: usize = 1_024;

/// Maximum cumulative source bytes read across one CLI invocation (`32 MiB`).
pub(crate) const MAX_TREE_BYTES: u64 = 32 * 1024 * 1024;

/// Invocation-wide resource budget shared across directory walks, file reads,
/// import closures, manifest batches, and retained outputs.
#[derive(Clone, Debug)]
pub(crate) struct InvocationBudget {
    max_scanned_entries: usize,
    scanned_entries: usize,
    max_accepted_files: usize,
    accepted_files: usize,
    max_file_bytes: u64,
    max_bytes_read: u64,
    bytes_read: u64,
    max_output_bytes: usize,
    retained_output_bytes: usize,
}

impl InvocationBudget {
    /// Creates a new invocation budget with default hard ceilings.
    pub(crate) const fn new() -> Self {
        Self {
            max_scanned_entries: MAX_SCANNED_ENTRIES,
            scanned_entries: 0,
            max_accepted_files: MAX_TREE_FILES,
            accepted_files: 0,
            max_file_bytes: MAX_SOURCE_FILE_BYTES,
            max_bytes_read: MAX_TREE_BYTES,
            bytes_read: 0,
            max_output_bytes: MAX_OUTPUT_BYTES,
            retained_output_bytes: 0,
        }
    }

    /// Creates a budget with custom ceilings for unit testing.
    #[cfg(test)]
    pub(crate) const fn with_limits(
        max_scanned_entries: usize,
        max_accepted_files: usize,
        max_file_bytes: u64,
        max_bytes_read: u64,
        max_output_bytes: usize,
    ) -> Self {
        Self {
            max_scanned_entries,
            scanned_entries: 0,
            max_accepted_files,
            accepted_files: 0,
            max_file_bytes,
            max_bytes_read,
            bytes_read: 0,
            max_output_bytes,
            retained_output_bytes: 0,
        }
    }

    /// Charges one raw directory entry before storing or sorting it.
    pub(crate) fn charge_scanned_entry(&mut self, dir: &Path) -> Result<(), String> {
        let next = self
            .scanned_entries
            .checked_add(1)
            .filter(|&count| count <= self.max_scanned_entries)
            .ok_or_else(|| {
                format!(
                    "{}: directory scan exceeds {} entry invocation limit",
                    dir.display(),
                    self.max_scanned_entries
                )
            })?;
        self.scanned_entries = next;
        Ok(())
    }

    /// Charges one accepted source file before reading it.
    pub(crate) fn charge_accepted_file(&mut self, path: &Path) -> Result<(), String> {
        let next = self
            .accepted_files
            .checked_add(1)
            .filter(|&count| count <= self.max_accepted_files)
            .ok_or_else(|| {
                format!(
                    "{}: invocation exceeds {} file limit",
                    path.display(),
                    self.max_accepted_files
                )
            })?;
        self.accepted_files = next;
        Ok(())
    }

    /// Returns the maximum bytes that may be read from the next single file.
    pub(crate) fn remaining_file_read_limit(&self) -> u64 {
        let remaining_total = self.max_bytes_read.saturating_sub(self.bytes_read);
        self.max_file_bytes.min(remaining_total)
    }

    /// Charges the actual bytes read from a single file descriptor.
    pub(crate) fn charge_bytes_read(&mut self, path: &Path, actual: u64) -> Result<(), String> {
        if actual > self.max_file_bytes {
            return Err(format!(
                "{}: exceeds {} byte authoring source limit",
                path.display(),
                self.max_file_bytes
            ));
        }
        let next = self
            .bytes_read
            .checked_add(actual)
            .filter(|&total| total <= self.max_bytes_read)
            .ok_or_else(|| {
                format!(
                    "{}: invocation exceeds {} byte read limit",
                    path.display(),
                    self.max_bytes_read
                )
            })?;
        self.bytes_read = next;
        Ok(())
    }

    /// Charges retained output bytes emitted or staged during one invocation.
    pub(crate) fn charge_output_bytes(&mut self, label: &str, bytes: usize) -> Result<(), String> {
        let next = self
            .retained_output_bytes
            .checked_add(bytes)
            .filter(|&total| total <= self.max_output_bytes)
            .ok_or_else(|| {
                format!(
                    "{label}: invocation exceeds {} byte output limit",
                    self.max_output_bytes
                )
            })?;
        self.retained_output_bytes = next;
        Ok(())
    }

    /// Returns cumulative source bytes charged so far (test inspection helper).
    #[cfg(test)]
    pub(crate) const fn bytes_read(&self) -> u64 {
        self.bytes_read
    }

    /// Returns cumulative raw directory entries scanned so far (test inspection helper).
    #[cfg(test)]
    pub(crate) const fn scanned_entries(&self) -> usize {
        self.scanned_entries
    }
}

/// Regular filesystem entry kind after a no-follow open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EntryKind {
    /// Regular file.
    File,
    /// Directory.
    Directory,
}

/// Supported native authoring languages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SourceLang {
    /// Typed `TypeScript` (`.ts` or `.tsx`, excluding declaration `.d.ts` files).
    TypeScript,
    /// Typed Python (`.py`).
    Python,
}

/// Result of inspecting `.github/workflows` relative to a trusted Norn app root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ZeroCiStatus {
    /// Neither `.github` nor `.github/workflows` exists (verified via `ENOENT`).
    Absent,
    /// `.github/workflows` exists (directory, non-directory, or symlink).
    Present(String),
    /// Fail-closed inspection refusal (`ELOOP`, `EACCES`, or other OS error).
    Error(String),
}

/// A regular file already opened and read directly from a trusted descriptor.
#[derive(Clone, Debug)]
pub(crate) struct LoadedFile {
    /// Logical display path (used for diagnostics and provenance).
    pub(crate) display_path: PathBuf,
    /// Relative path from the owning directory handle.
    pub(crate) rel_path: PathBuf,
    /// UTF-8 contents read directly from the opened descriptor.
    pub(crate) content: String,
}

/// Returns `true` if `file_name` is a `TypeScript` declaration file (`.d.ts`, `.d.mts`, `.d.cts`).
pub(crate) fn is_declaration_file(file_name: &str) -> bool {
    let lower = file_name.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    bytes.ends_with(b".d.ts") || bytes.ends_with(b".d.mts") || bytes.ends_with(b".d.cts")
}

/// Classifies a source file path by case-insensitive extension.
pub(crate) fn classify_source_file(path: &Path) -> Option<SourceLang> {
    let ext = path.extension()?;
    if ext.eq_ignore_ascii_case("py") {
        return Some(SourceLang::Python);
    }
    if ext.eq_ignore_ascii_case("ts") || ext.eq_ignore_ascii_case("tsx") {
        let name = path.file_name().and_then(std::ffi::OsStr::to_str)?;
        if is_declaration_file(name) {
            return None;
        }
        return Some(SourceLang::TypeScript);
    }
    None
}

/// Returns `true` if `path` has a `.json` extension (case-insensitive).
pub(crate) fn is_json_file(path: &Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
}

/// Returns `true` if `path` has a `.ts` (non-`.d.ts`) or `.json` extension.
pub(crate) fn is_ts_or_json_support(name: &str, path: &Path) -> bool {
    if is_declaration_file(name) {
        return false;
    }
    path.extension().is_some_and(|ext| {
        ext.eq_ignore_ascii_case("ts")
            || ext.eq_ignore_ascii_case("tsx")
            || ext.eq_ignore_ascii_case("json")
    })
}

/// Returns `true` if `path` is formattable by `norn fmt` (`.ts`, `.tsx`, or `.py`).
pub(crate) fn is_formattable_file(name: &str, path: &Path) -> bool {
    if is_declaration_file(name) {
        return false;
    }
    classify_source_file(path).is_some()
}

/// Formats `path` as a POSIX relative-style string (`/` separators, no leading `./`).
pub(crate) fn display_posix_path(path: &Path) -> String {
    let raw = path.to_string_lossy().replace('\\', "/");
    raw.strip_prefix("./").unwrap_or(&raw).to_owned()
}

/// Returns `true` for directories skipped during recursive walks.
pub(crate) fn should_skip_dir(name: &str) -> bool {
    name.starts_with('.') || name == "node_modules" || name == "target"
}

/// Reads an already-opened regular file directly from its descriptor, charging actual bytes read.
pub(crate) fn read_opened_file_budgeted(
    file: std::fs::File,
    display_path: &Path,
    budget: &mut InvocationBudget,
) -> Result<String, String> {
    let meta = file
        .metadata()
        .map_err(|err| format!("{}: {err}", display_path.display()))?;
    if !meta.file_type().is_file() {
        return Err(format!("{}: not a regular file", display_path.display()));
    }
    let read_cap = budget.remaining_file_read_limit();
    if meta.len() > read_cap {
        budget.charge_bytes_read(display_path, meta.len())?;
    }
    let mut bytes = Vec::new();
    file.take(read_cap.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|err| format!("{}: {err}", display_path.display()))?;
    let actual =
        u64::try_from(bytes.len()).map_err(|err| format!("{}: {err}", display_path.display()))?;
    budget.charge_bytes_read(display_path, actual)?;
    String::from_utf8(bytes)
        .map_err(|err| format!("{}: invalid UTF-8: {err}", display_path.display()))
}

#[cfg(unix)]
pub(crate) fn ancestor_dir_flags() -> rustix::fs::OFlags {
    use rustix::fs::OFlags;
    #[cfg(any(target_os = "linux", target_os = "android", target_os = "freebsd"))]
    {
        OFlags::PATH | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
    }
    #[cfg(not(any(target_os = "linux", target_os = "android", target_os = "freebsd")))]
    {
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
    }
}

#[cfg(unix)]
pub(crate) fn final_dir_flags() -> rustix::fs::OFlags {
    use rustix::fs::OFlags;
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
}

#[cfg(unix)]
pub(crate) fn format_dir_open_error(
    parent_fd: std::os::fd::BorrowedFd<'_>,
    segment: &std::ffi::OsStr,
    walked: &Path,
    err: rustix::io::Errno,
) -> String {
    use rustix::fs::{AtFlags, FileType, statat};
    use rustix::io::Errno;

    if err == Errno::LOOP {
        return format!("{}: symlinks are refused", walked.display());
    }
    if err == Errno::NOTDIR
        && let Ok(stat) = statat(parent_fd, segment, AtFlags::SYMLINK_NOFOLLOW)
        && FileType::from_raw_mode(stat.st_mode) == FileType::Symlink
    {
        return format!("{}: symlinks are refused", walked.display());
    }
    if err == Errno::NOTDIR {
        return format!("{}: not a directory", walked.display());
    }
    format!("{}: {err}", walked.display())
}

/// Opens `dir` component-by-component with `O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC`,
/// refusing any symlink along the path.
#[cfg(unix)]
pub(crate) fn open_dir_components_no_follow(dir: &Path) -> Result<std::os::fd::OwnedFd, String> {
    use rustix::fs::{CWD, Mode, openat};
    use std::os::fd::AsFd as _;

    let mut is_absolute = false;
    let mut segments: Vec<&std::ffi::OsStr> = Vec::new();
    for comp in dir.components() {
        match comp {
            Component::RootDir => is_absolute = true,
            Component::CurDir => {}
            Component::ParentDir => segments.push(std::ffi::OsStr::new("..")),
            Component::Normal(seg) => segments.push(seg),
            Component::Prefix(_) => {
                return Err(format!("{}: unsupported path prefix", dir.display()));
            }
        }
    }

    let start_name = if is_absolute { "/" } else { "." };
    if segments.is_empty() {
        return openat(CWD, start_name, final_dir_flags(), Mode::empty())
            .map_err(|err| format!("{}: {err}", dir.display()));
    }

    let mut current = openat(CWD, start_name, ancestor_dir_flags(), Mode::empty())
        .map_err(|err| format!("{start_name}: {err}"))?;
    let mut walked = if is_absolute {
        PathBuf::from("/")
    } else {
        PathBuf::new()
    };
    let last_idx = segments.len().saturating_sub(1);
    for (idx, segment) in segments.iter().enumerate() {
        let flags = if idx == last_idx {
            final_dir_flags()
        } else {
            ancestor_dir_flags()
        };
        walked.push(segment);
        current = openat(current.as_fd(), *segment, flags, Mode::empty())
            .map_err(|err| format_dir_open_error(current.as_fd(), segment, &walked, err))?;
    }
    Ok(current)
}

#[cfg(unix)]
pub(crate) fn open_entry_no_follow<P: rustix::path::Arg>(
    dir: impl std::os::fd::AsFd,
    name: P,
    display_path: &Path,
) -> Result<std::fs::File, String> {
    use rustix::fs::{Mode, OFlags, openat};
    use rustix::io::Errno;

    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    openat(dir, name, flags, Mode::empty())
        .map(std::fs::File::from)
        .map_err(|err| {
            if err == Errno::LOOP {
                format!("{}: symlinks are refused", display_path.display())
            } else if err == Errno::NXIO || err == Errno::OPNOTSUPP {
                format!("{}: not a regular file", display_path.display())
            } else {
                format!("{}: {err}", display_path.display())
            }
        })
}

/// Inspects `path` component-by-component without following symlinks or blocking on FIFOs.
#[cfg(unix)]
pub(crate) fn inspect_path_no_follow(path: &Path) -> Result<EntryKind, String> {
    use std::os::fd::AsFd as _;
    use std::os::unix::ffi::OsStrExt as _;

    let shown = path.display();
    let raw = path.as_os_str().as_bytes();
    let must_be_dir = raw.ends_with(b"/") || raw.ends_with(b"/.");
    let Some(final_name) = path.file_name() else {
        let _dir_fd = open_dir_components_no_follow(path)?;
        return Ok(EntryKind::Directory);
    };
    let parent = path
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let parent_fd = open_dir_components_no_follow(parent)?;
    let opened = open_entry_no_follow(parent_fd.as_fd(), final_name, path)?;
    let kind = opened
        .metadata()
        .map_err(|err| format!("{shown}: {err}"))?
        .file_type();
    if kind.is_dir() {
        return Ok(EntryKind::Directory);
    }
    if kind.is_file() {
        if must_be_dir {
            return Err(format!("{shown}: not a directory"));
        }
        return Ok(EntryKind::File);
    }
    Err(format!("{shown}: not a regular file or directory"))
}

/// Inspects `path` without following symlinks (non-Unix fallback).
#[cfg(not(unix))]
pub(crate) fn inspect_path_no_follow(path: &Path) -> Result<EntryKind, String> {
    let shown = path.display();
    if let Some(parent) = path.parent().filter(|dir| !dir.as_os_str().is_empty()) {
        verify_no_symlink_ancestors_non_unix(parent)?;
    }
    let meta = std::fs::symlink_metadata(path).map_err(|err| format!("{shown}: {err}"))?;
    if meta.file_type().is_symlink() {
        return Err(format!("{shown}: symlinks are refused"));
    }
    if meta.is_dir() {
        return Ok(EntryKind::Directory);
    }
    if meta.is_file() {
        return Ok(EntryKind::File);
    }
    Err(format!("{shown}: not a regular file or directory"))
}

/// Verifies no ancestor directory of `dir` is a symlink (non-Unix fallback).
#[cfg(not(unix))]
pub(crate) fn verify_no_symlink_ancestors_non_unix(dir: &Path) -> Result<(), String> {
    let mut cursor = PathBuf::new();
    for comp in dir.components() {
        cursor.push(comp.as_os_str());
        if matches!(comp, Component::Normal(_)) {
            let meta = std::fs::symlink_metadata(&cursor)
                .map_err(|err| format!("{}: {err}", cursor.display()))?;
            if meta.file_type().is_symlink() {
                return Err(format!("{}: symlinks are refused", cursor.display()));
            }
            if !meta.is_dir() {
                return Err(format!("{}: not a directory", cursor.display()));
            }
        }
    }
    Ok(())
}

/// Validates a relative path from `norn.json` (`sources`) so it stays inside the project root.
pub(crate) fn validate_safe_relative(raw: &str) -> Result<PathBuf, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed != raw {
        return Err(format!("invalid relative source path `{raw}`"));
    }
    let path = Path::new(trimmed);
    if path.is_absolute() {
        return Err(format!("source path `{raw}` must be relative"));
    }
    let mut clean = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::Normal(seg) => clean.push(seg),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(format!("source path `{raw}` must stay inside the project"));
            }
        }
    }
    if clean.as_os_str().is_empty() {
        return Err(format!("source path `{raw}` must not be empty"));
    }
    Ok(clean)
}

fn has_pinned_effect(root: &Path) -> bool {
    root.join("node_modules/effect/package.json").is_file()
}

/// Discovers the directory holding pinned `node_modules/effect` (and optional `typescript`).
///
/// When `explicit` is provided, fails closed if `node_modules/effect/package.json` is missing.
pub(crate) fn discover_tool_root(
    start: &Path,
    explicit: Option<&Path>,
) -> Result<Option<PathBuf>, String> {
    if let Some(root) = explicit {
        if !has_pinned_effect(root) {
            return Err(format!(
                "--tool-root `{}` is missing pinned node_modules/effect/package.json",
                root.display()
            ));
        }
        return Ok(Some(root.to_path_buf()));
    }
    let mut seeds = Vec::new();
    let abs_start = if start.is_absolute() {
        start.to_path_buf()
    } else if let Ok(cwd) = std::env::current_dir() {
        cwd.join(start)
    } else {
        start.to_path_buf()
    };
    seeds.push(abs_start);
    if let Ok(cwd) = std::env::current_dir() {
        seeds.push(cwd);
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(parent) = exe.parent()
    {
        seeds.push(parent.to_path_buf());
    }
    for seed in seeds {
        let initial = if seed.is_dir() {
            seed.as_path()
        } else {
            seed.parent().unwrap_or(seed.as_path())
        };
        for ancestor in initial.ancestors() {
            if has_pinned_effect(ancestor) {
                return Ok(Some(ancestor.to_path_buf()));
            }
            let repo_tools = ancestor.join("tools/norn-ts");
            if has_pinned_effect(&repo_tools) {
                return Ok(Some(repo_tools));
            }
        }
    }
    Ok(None)
}
