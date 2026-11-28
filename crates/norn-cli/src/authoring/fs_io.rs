//! Bounded, symlink-safe filesystem helpers for native Rust authoring commands.
//!
//! All reads on Unix use `openat` with `O_RDONLY | O_NOFOLLOW | O_NONBLOCK | O_CLOEXEC`
//! to avoid symlink traversal, `TOCTOU` directory swaps, and FIFO blocking.

use std::io::Write as _;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Maximum single authoring source file size (`4 MiB`).
pub(crate) const MAX_SOURCE_FILE_BYTES: u64 = 4 * 1024 * 1024;

/// Maximum single emitted output document size (`32 MiB`).
pub(crate) const MAX_OUTPUT_BYTES: usize = 32 * 1024 * 1024;

/// Maximum recursive directory walk depth.
pub(crate) const MAX_WALK_DEPTH: usize = 32;

/// Maximum files admitted during one recursive walk.
pub(crate) const MAX_WALK_FILES: usize = 1_024;

/// Maximum cumulative bytes across one recursive walk (`32 MiB`).
pub(crate) const MAX_WALK_TOTAL_BYTES: u64 = 32 * 1024 * 1024;

static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

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

/// Reads a regular non-symlink source file under [`MAX_SOURCE_FILE_BYTES`].
pub(crate) fn read_source_file_bounded(path: &Path) -> Result<String, String> {
    crate::cell::read_regular_file_bounded(
        path,
        MAX_SOURCE_FILE_BYTES,
        "4 MiB authoring source limit",
    )
}

/// Inspects `path` without following symlinks or blocking on FIFOs.
#[cfg(unix)]
pub(crate) fn inspect_path_no_follow(path: &Path) -> Result<EntryKind, String> {
    use std::os::unix::ffi::OsStrExt as _;
    let shown = path.display();
    let raw = path.as_os_str().as_bytes();
    let must_be_dir = raw.ends_with(b"/") || raw.ends_with(b"/.");
    let target: PathBuf = path.components().collect();
    let opened = open_no_follow(rustix::fs::CWD, &target, path)?;
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

#[cfg(unix)]
fn open_no_follow<P: rustix::path::Arg>(
    dir: impl std::os::fd::AsFd,
    name: P,
    display_path: &Path,
) -> Result<std::fs::File, String> {
    use rustix::fs::{Mode, OFlags};
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    rustix::fs::openat(dir, name, flags, Mode::empty())
        .map(std::fs::File::from)
        .map_err(|err| {
            if err == rustix::io::Errno::LOOP {
                format!("{}: symlinks are refused", display_path.display())
            } else if err == rustix::io::Errno::NXIO || err == rustix::io::Errno::OPNOTSUPP {
                format!("{}: not a regular file", display_path.display())
            } else {
                format!("{}: {err}", display_path.display())
            }
        })
}

fn should_skip_dir(name: &str) -> bool {
    name.starts_with('.') || name == "node_modules" || name == "target"
}

/// Collects regular files directly inside `dir` (non-recursive) in sorted path order.
#[cfg(unix)]
pub(crate) fn collect_top_level_files(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let target: PathBuf = dir.components().collect();
    let dir_handle = open_no_follow(rustix::fs::CWD, &target, dir)?;
    if !dir_handle
        .metadata()
        .map_err(|err| format!("{}: {err}", dir.display()))?
        .is_dir()
    {
        return Err(format!("{}: not a directory", dir.display()));
    }
    let mut files = Vec::new();
    for entry in read_sorted_dir_names(&dir_handle, dir)? {
        let name = entry
            .to_str()
            .map_err(|err| format!("non-UTF-8 name under {}: {err}", dir.display()))?;
        if name.starts_with('.') {
            continue;
        }
        let child = dir.join(name);
        let opened = open_no_follow(&dir_handle, entry.as_c_str(), &child)?;
        let kind = opened
            .metadata()
            .map_err(|err| format!("{}: {err}", child.display()))?
            .file_type();
        if kind.is_file() {
            if files.len() >= MAX_WALK_FILES {
                return Err(format!(
                    "{}: directory exceeds {MAX_WALK_FILES} files",
                    dir.display()
                ));
            }
            files.push(child);
        } else if !kind.is_dir() {
            return Err(format!("{}: not a regular file", child.display()));
        }
    }
    Ok(files)
}

/// Collects regular files directly inside `dir` (non-Unix fallback).
#[cfg(not(unix))]
pub(crate) fn collect_top_level_files(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let meta = std::fs::symlink_metadata(dir).map_err(|err| format!("{}: {err}", dir.display()))?;
    if meta.file_type().is_symlink() {
        return Err(format!("{}: symlinks are refused", dir.display()));
    }
    if !meta.is_dir() {
        return Err(format!("{}: not a directory", dir.display()));
    }
    let mut entries = std::fs::read_dir(dir)
        .map_err(|err| format!("{}: {err}", dir.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err| format!("{}: {err}", dir.display()))?;
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
            if files.len() >= MAX_WALK_FILES {
                return Err(format!(
                    "{}: directory exceeds {MAX_WALK_FILES} files",
                    dir.display()
                ));
            }
            files.push(child);
        } else if !kind.is_dir() {
            return Err(format!("{}: not a regular file", child.display()));
        }
    }
    Ok(files)
}

#[cfg(unix)]
fn read_sorted_dir_names(
    dir_handle: &std::fs::File,
    display_dir: &Path,
) -> Result<Vec<std::ffi::CString>, String> {
    let mut names = Vec::new();
    for item in rustix::fs::Dir::read_from(dir_handle)
        .map_err(|err| format!("{}: {err}", display_dir.display()))?
    {
        let entry = item.map_err(|err| format!("{}: {err}", display_dir.display()))?;
        let owned = entry.file_name().to_owned();
        if owned.as_bytes() == b"." || owned.as_bytes() == b".." {
            continue;
        }
        names.push(owned);
    }
    names.sort();
    Ok(names)
}

struct WalkState {
    remaining_bytes: u64,
    files: Vec<PathBuf>,
}

/// Recursively walks `root` skipping hidden directories, `node_modules`, and `target`,
/// refusing symlinks and special files, and keeping only entries accepted by `accept`.
#[cfg(unix)]
pub(crate) fn collect_walk_files(
    root: &Path,
    accept: fn(&str, &Path) -> bool,
) -> Result<Vec<PathBuf>, String> {
    let target: PathBuf = root.components().collect();
    let root_handle = open_no_follow(rustix::fs::CWD, &target, root)?;
    if !root_handle
        .metadata()
        .map_err(|err| format!("{}: {err}", root.display()))?
        .is_dir()
    {
        return Err(format!("{}: not a directory", root.display()));
    }
    let mut state = WalkState {
        remaining_bytes: MAX_WALK_TOTAL_BYTES,
        files: Vec::new(),
    };
    walk_unix_recursive(&root_handle, root, 0, accept, &mut state)?;
    Ok(state.files)
}

#[cfg(unix)]
fn walk_unix_recursive(
    dir_handle: &std::fs::File,
    dir_display: &Path,
    depth: usize,
    accept: fn(&str, &Path) -> bool,
    state: &mut WalkState,
) -> Result<(), String> {
    if depth > MAX_WALK_DEPTH {
        return Err(format!(
            "{}: directory nesting exceeds {MAX_WALK_DEPTH}",
            dir_display.display()
        ));
    }
    for entry in read_sorted_dir_names(dir_handle, dir_display)? {
        let name = entry
            .to_str()
            .map_err(|err| format!("non-UTF-8 name under {}: {err}", dir_display.display()))?;
        if name.starts_with('.') {
            continue;
        }
        let child = dir_display.join(name);
        let opened = open_no_follow(dir_handle, entry.as_c_str(), &child)?;
        let meta = opened
            .metadata()
            .map_err(|err| format!("{}: {err}", child.display()))?;
        let kind = meta.file_type();
        if kind.is_dir() {
            if should_skip_dir(name) {
                continue;
            }
            walk_unix_recursive(&opened, &child, depth.saturating_add(1), accept, state)?;
        } else if kind.is_file() {
            if !accept(name, &child) {
                continue;
            }
            record_walk_file(&child, meta.len(), state)?;
        } else {
            return Err(format!("{}: not a regular file", child.display()));
        }
    }
    Ok(())
}

/// Recursively walks `root` without following symlinks (non-Unix fallback).
#[cfg(not(unix))]
pub(crate) fn collect_walk_files(
    root: &Path,
    accept: fn(&str, &Path) -> bool,
) -> Result<Vec<PathBuf>, String> {
    let meta =
        std::fs::symlink_metadata(root).map_err(|err| format!("{}: {err}", root.display()))?;
    if meta.file_type().is_symlink() {
        return Err(format!("{}: symlinks are refused", root.display()));
    }
    if !meta.is_dir() {
        return Err(format!("{}: not a directory", root.display()));
    }
    let mut state = WalkState {
        remaining_bytes: MAX_WALK_TOTAL_BYTES,
        files: Vec::new(),
    };
    walk_non_unix_recursive(root, 0, accept, &mut state)?;
    Ok(state.files)
}

#[cfg(not(unix))]
fn walk_non_unix_recursive(
    dir: &Path,
    depth: usize,
    accept: fn(&str, &Path) -> bool,
    state: &mut WalkState,
) -> Result<(), String> {
    if depth > MAX_WALK_DEPTH {
        return Err(format!(
            "{}: directory nesting exceeds {MAX_WALK_DEPTH}",
            dir.display()
        ));
    }
    let mut entries = std::fs::read_dir(dir)
        .map_err(|err| format!("{}: {err}", dir.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err| format!("{}: {err}", dir.display()))?;
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
            walk_non_unix_recursive(&child, depth.saturating_add(1), accept, state)?;
        } else if kind.is_file() {
            if !accept(name, &child) {
                continue;
            }
            record_walk_file(&child, meta.len(), state)?;
        } else {
            return Err(format!("{}: not a regular file", child.display()));
        }
    }
    Ok(())
}

fn record_walk_file(path: &Path, size: u64, state: &mut WalkState) -> Result<(), String> {
    if state.files.len() >= MAX_WALK_FILES {
        return Err(format!(
            "{}: source tree exceeds {MAX_WALK_FILES} files",
            path.display()
        ));
    }
    if size > MAX_SOURCE_FILE_BYTES {
        return Err(format!(
            "{}: exceeds 4 MiB authoring source limit",
            path.display()
        ));
    }
    state.remaining_bytes = state
        .remaining_bytes
        .checked_sub(size)
        .ok_or_else(|| format!("{}: source tree exceeds 32 MiB limit", path.display()))?;
    state.files.push(path.to_path_buf());
    Ok(())
}

/// Atomically writes `content` to `path`, refusing symlinks, directories, and oversized writes.
pub(crate) fn write_file_atomic(path: &Path, content: &str) -> Result<(), String> {
    if content.len() > MAX_OUTPUT_BYTES {
        return Err(format!(
            "{}: output exceeds {MAX_OUTPUT_BYTES} byte limit",
            path.display()
        ));
    }
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        if meta.file_type().is_symlink() {
            return Err(format!("{}: refusing to overwrite symlink", path.display()));
        }
        if meta.is_dir() {
            return Err(format!("{}: target path is a directory", path.display()));
        }
    }
    let parent = path
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let base = path
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .ok_or_else(|| {
            format!(
                "{}: output path must have a UTF-8 file name",
                path.display()
            )
        })?;
    let seq = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp_name = format!(".{base}.norn-tmp-{}-{seq}", std::process::id());
    let tmp_path = parent.join(tmp_name);
    let mut handle = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp_path)
        .map_err(|err| format!("{}: {err}", tmp_path.display()))?;
    let write_res = handle
        .write_all(content.as_bytes())
        .and_then(|()| handle.flush());
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
