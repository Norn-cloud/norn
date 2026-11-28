//! Bounded, symlink-safe filesystem primitives and [`InvocationBudget`] for native Rust authoring.

use std::io::{Read as _, Write as _};
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

/// Maximum JSON nesting depth allowed when streaming canonical JSON values.
const MAX_CANONICAL_JSON_DEPTH: usize = 256;

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

    /// Fast-fails if a reported file length already exceeds per-file or remaining invocation limits.
    pub(crate) fn check_file_size_hint(
        &self,
        path: &Path,
        reported_len: u64,
    ) -> Result<(), String> {
        if reported_len > self.max_file_bytes {
            return Err(format!(
                "{}: exceeds {} byte authoring source limit",
                path.display(),
                self.max_file_bytes
            ));
        }
        if self
            .bytes_read
            .checked_add(reported_len)
            .is_none_or(|total| total > self.max_bytes_read)
        {
            return Err(format!(
                "{}: invocation exceeds {} byte read limit",
                path.display(),
                self.max_bytes_read
            ));
        }
        Ok(())
    }

    /// Charges a streamed chunk against both per-file and invocation-wide byte limits
    /// *before* copying the chunk into memory.
    pub(crate) fn charge_read_chunk(
        &mut self,
        path: &Path,
        file_bytes_so_far: u64,
        chunk_bytes: u64,
    ) -> Result<(), String> {
        if file_bytes_so_far > self.max_file_bytes {
            return Err(format!(
                "{}: exceeds {} byte authoring source limit",
                path.display(),
                self.max_file_bytes
            ));
        }
        let next = self
            .bytes_read
            .checked_add(chunk_bytes)
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
    /// `.github/workflows` exists as a real directory.
    Present(String),
    /// Fail-closed inspection refusal (`ELOOP`, `ENOTDIR`, `EACCES`, or other OS error).
    Error(String),
}

/// Streaming [`std::io::Write`] sink that charges every slice against an
/// [`InvocationBudget`] *before* copying bytes into the output buffer.
pub(crate) struct BoundedJsonWriter<'a> {
    label: &'a str,
    out: &'a mut Vec<u8>,
    budget: &'a mut InvocationBudget,
}

impl<'a> BoundedJsonWriter<'a> {
    /// Creates a bounded streaming writer charging output bytes under `label`.
    pub(crate) fn new(
        label: &'a str,
        out: &'a mut Vec<u8>,
        budget: &'a mut InvocationBudget,
    ) -> Self {
        Self { label, out, budget }
    }
}

impl std::io::Write for BoundedJsonWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.budget
            .charge_output_bytes(self.label, buf.len())
            .map_err(std::io::Error::other)?;
        self.out.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Streaming JSON array accumulator backed by [`BoundedJsonWriter`].
///
/// Serializes each element directly into the bounded byte buffer as soon as it is
/// produced, aborting mid-serialization before exceeding `MAX_OUTPUT_BYTES`.
pub(crate) struct BoundedJsonArray {
    out: Vec<u8>,
    count: usize,
}

impl BoundedJsonArray {
    /// Creates an empty streaming JSON array accumulator.
    pub(crate) const fn new() -> Self {
        Self {
            out: Vec::new(),
            count: 0,
        }
    }

    /// Returns the number of elements streamed so far.
    pub(crate) const fn len(&self) -> usize {
        self.count
    }

    /// Returns `true` when no elements have been streamed yet.
    pub(crate) const fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Borrows a [`BoundedJsonWriter`] targeting this accumulator's output buffer.
    pub(crate) fn writer<'a>(
        &'a mut self,
        label: &'a str,
        budget: &'a mut InvocationBudget,
    ) -> BoundedJsonWriter<'a> {
        BoundedJsonWriter::new(label, &mut self.out, budget)
    }

    /// Writes `[` for the first element or `,` for subsequent elements through [`BoundedJsonWriter`].
    pub(crate) fn push_prefix(
        &mut self,
        label: &str,
        budget: &mut InvocationBudget,
    ) -> Result<(), String> {
        let prefix: &[u8] = if self.count == 0 { b"[" } else { b"," };
        let mut writer = self.writer(label, budget);
        writer.write_all(prefix).map_err(|err| err.to_string())
    }

    /// Increments the streamed element count after writing an element body.
    pub(crate) fn mark_pushed(&mut self) {
        self.count = self.count.saturating_add(1);
    }

    /// Streams one `serde_json::Value` element as compact canonical JSON through [`BoundedJsonWriter`].
    pub(crate) fn push_value(
        &mut self,
        label: &str,
        value: &serde_json::Value,
        budget: &mut InvocationBudget,
    ) -> Result<(), String> {
        self.push_prefix(label, budget)?;
        {
            let mut writer = self.writer(label, budget);
            write_canonical_json_value(&mut writer, value)?;
        }
        self.mark_pushed();
        Ok(())
    }

    /// Closes the JSON array (`[]\n` or `]\n`) through [`BoundedJsonWriter`] and returns UTF-8 text.
    pub(crate) fn finish(
        mut self,
        label: &str,
        budget: &mut InvocationBudget,
    ) -> Result<String, String> {
        let suffix: &[u8] = if self.count == 0 { b"[]\n" } else { b"]\n" };
        {
            let mut writer = BoundedJsonWriter::new(label, &mut self.out, budget);
            writer.write_all(suffix).map_err(|err| err.to_string())?;
        }
        String::from_utf8(self.out).map_err(|err| format!("{label}: invalid UTF-8: {err}"))
    }
}

fn js_array_index(key: &str) -> Option<u32> {
    if key == "0" {
        return Some(0);
    }
    let first = key.bytes().next()?;
    if !(b'1'..=b'9').contains(&first) || !key.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let idx = key.parse::<u32>().ok()?;
    if idx == u32::MAX { None } else { Some(idx) }
}

fn js_key_cmp(left: &str, right: &str) -> std::cmp::Ordering {
    match (js_array_index(left), js_array_index(right)) {
        (Some(a), Some(b)) => a.cmp(&b),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => left.encode_utf16().cmp(right.encode_utf16()),
    }
}

fn hex_nibble(nibble: u8) -> u8 {
    if nibble < 10 {
        b'0'.saturating_add(nibble)
    } else {
        b'a'.saturating_add(nibble.saturating_sub(10))
    }
}

/// Streams an RFC 8259 JSON string into `writer` without `serde_json::to_writer`.
pub(crate) fn write_json_string(
    writer: &mut BoundedJsonWriter<'_>,
    text: &str,
) -> Result<(), String> {
    writer.write_all(b"\"").map_err(|err| err.to_string())?;
    let bytes = text.as_bytes();
    let mut start = 0_usize;
    for (idx, &byte) in bytes.iter().enumerate() {
        let escaped: Option<&[u8]> = match byte {
            b'"' => Some(b"\\\""),
            b'\\' => Some(b"\\\\"),
            0x08 => Some(b"\\b"),
            0x0c => Some(b"\\f"),
            b'\n' => Some(b"\\n"),
            b'\r' => Some(b"\\r"),
            b'\t' => Some(b"\\t"),
            0x00..=0x1f => None,
            _ => continue,
        };
        if let Some(chunk) = bytes.get(start..idx)
            && !chunk.is_empty()
        {
            writer.write_all(chunk).map_err(|err| err.to_string())?;
        }
        if let Some(seq) = escaped {
            writer.write_all(seq).map_err(|err| err.to_string())?;
        } else {
            let esc = [
                b'\\',
                b'u',
                b'0',
                b'0',
                hex_nibble(byte >> 4),
                hex_nibble(byte & 0x0f),
            ];
            writer.write_all(&esc).map_err(|err| err.to_string())?;
        }
        start = idx.saturating_add(1);
    }
    if let Some(tail) = bytes.get(start..)
        && !tail.is_empty()
    {
        writer.write_all(tail).map_err(|err| err.to_string())?;
    }
    writer.write_all(b"\"").map_err(|err| err.to_string())
}

fn write_canonical_json_value_at(
    writer: &mut BoundedJsonWriter<'_>,
    value: &serde_json::Value,
    depth: usize,
) -> Result<(), String> {
    if depth > MAX_CANONICAL_JSON_DEPTH {
        return Err(format!(
            "canonical JSON nesting exceeds {MAX_CANONICAL_JSON_DEPTH} levels"
        ));
    }
    match value {
        serde_json::Value::Null => writer.write_all(b"null").map_err(|err| err.to_string()),
        serde_json::Value::Bool(true) => writer.write_all(b"true").map_err(|err| err.to_string()),
        serde_json::Value::Bool(false) => writer.write_all(b"false").map_err(|err| err.to_string()),
        serde_json::Value::Number(num) => {
            let rendered =
                norn_authoring::compact_canonical_json(&serde_json::Value::Number(num.clone()))
                    .map_err(|err| err.to_string())?;
            writer
                .write_all(rendered.as_bytes())
                .map_err(|err| err.to_string())
        }
        serde_json::Value::String(text) => write_json_string(writer, text),
        serde_json::Value::Array(items) => {
            writer.write_all(b"[").map_err(|err| err.to_string())?;
            let next_depth = depth.saturating_add(1);
            for (idx, item) in items.iter().enumerate() {
                if idx > 0 {
                    writer.write_all(b",").map_err(|err| err.to_string())?;
                }
                write_canonical_json_value_at(writer, item, next_depth)?;
            }
            writer.write_all(b"]").map_err(|err| err.to_string())
        }
        serde_json::Value::Object(map) => {
            let mut entries: Vec<(&str, &serde_json::Value)> =
                map.iter().map(|(key, val)| (key.as_str(), val)).collect();
            entries.sort_by(|(left, _), (right, _)| js_key_cmp(left, right));
            writer.write_all(b"{").map_err(|err| err.to_string())?;
            let next_depth = depth.saturating_add(1);
            for (idx, (key, val)) in entries.into_iter().enumerate() {
                if key == "__proto__" {
                    return Err("__proto__ is not a portable JSON member name".to_owned());
                }
                if idx > 0 {
                    writer.write_all(b",").map_err(|err| err.to_string())?;
                }
                write_json_string(writer, key)?;
                writer.write_all(b":").map_err(|err| err.to_string())?;
                write_canonical_json_value_at(writer, val, next_depth)?;
            }
            writer.write_all(b"}").map_err(|err| err.to_string())
        }
    }
}

/// Streams `value` as compact canonical JSON directly into `writer`, charging every
/// byte against [`InvocationBudget`] before allocation.
pub(crate) fn write_canonical_json_value(
    writer: &mut BoundedJsonWriter<'_>,
    value: &serde_json::Value,
) -> Result<(), String> {
    write_canonical_json_value_at(writer, value, 0)
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

/// Reads an already-opened regular file directly from its descriptor in fixed-size chunks,
/// charging every chunk against `budget` *before* copying bytes into memory.
pub(crate) fn read_opened_file_budgeted(
    mut file: std::fs::File,
    display_path: &Path,
    budget: &mut InvocationBudget,
) -> Result<String, String> {
    let meta = file
        .metadata()
        .map_err(|err| format!("{}: {err}", display_path.display()))?;
    if !meta.file_type().is_file() {
        return Err(format!("{}: not a regular file", display_path.display()));
    }
    budget.check_file_size_hint(display_path, meta.len())?;

    let mut bytes = Vec::new();
    let mut file_bytes: u64 = 0;
    let mut chunk = [0_u8; 16_384];
    loop {
        let count = match file.read(&mut chunk) {
            Ok(0) => break,
            Ok(read_len) => read_len,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(format!("{}: {err}", display_path.display())),
        };
        let count_u64 =
            u64::try_from(count).map_err(|err| format!("{}: {err}", display_path.display()))?;
        file_bytes = file_bytes
            .checked_add(count_u64)
            .ok_or_else(|| format!("{}: file size overflow", display_path.display()))?;
        budget.charge_read_chunk(display_path, file_bytes, count_u64)?;
        if let Some(slice) = chunk.get(..count) {
            bytes.extend_from_slice(slice);
        }
    }
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

fn has_pinned_effect_within(root: &Path, canonical_bound: &Path) -> bool {
    let pkg = root.join("node_modules/effect/package.json");
    std::fs::symlink_metadata(&pkg).is_ok_and(|meta| meta.is_file())
        && std::fs::canonicalize(&pkg)
            .is_ok_and(|canonical_pkg| canonical_pkg.starts_with(canonical_bound))
}

fn discover_tool_root_confined(
    start: &Path,
    explicit: Option<&Path>,
    boundary_root: &Path,
) -> Result<Option<PathBuf>, String> {
    let canonical_bound = std::fs::canonicalize(boundary_root)
        .map_err(|err| format!("{}: {err}", boundary_root.display()))?;
    if let Some(root) = explicit {
        let candidate = if root.is_absolute() {
            root.to_path_buf()
        } else {
            canonical_bound.join(root)
        };
        let canonical_explicit = std::fs::canonicalize(&candidate)
            .map_err(|err| format!("{}: {err}", root.display()))?;
        if !canonical_explicit.starts_with(&canonical_bound) {
            return Err(format!(
                "--tool-root `{}` is outside workspace boundary",
                root.display()
            ));
        }
        if !has_pinned_effect_within(&canonical_explicit, &canonical_bound) {
            return Err(format!(
                "--tool-root `{}` is missing pinned node_modules/effect/package.json",
                root.display()
            ));
        }
        return Ok(Some(canonical_explicit));
    }
    let start_candidate = if start.is_absolute() {
        start.to_path_buf()
    } else {
        canonical_bound.join(start)
    };
    let canonical_start = std::fs::canonicalize(&start_candidate)
        .map_err(|err| format!("{}: {err}", start.display()))?;
    if !canonical_start.starts_with(&canonical_bound) {
        return Err(format!(
            "{}: path is outside workspace boundary",
            start.display()
        ));
    }
    let initial = if canonical_start.is_dir() {
        canonical_start.as_path()
    } else {
        canonical_start
            .parent()
            .unwrap_or(canonical_start.as_path())
    };
    for ancestor in initial.ancestors() {
        if !ancestor.starts_with(&canonical_bound) {
            break;
        }
        if has_pinned_effect_within(ancestor, &canonical_bound) {
            return Ok(Some(ancestor.to_path_buf()));
        }
        let repo_tools = ancestor.join("tools/norn-ts");
        if has_pinned_effect_within(&repo_tools, &canonical_bound) {
            return Ok(Some(repo_tools));
        }
    }
    Ok(None)
}

/// Discovers the directory holding pinned `node_modules/effect`.
///
/// When `explicit` is provided, fails closed if `node_modules/effect/package.json` is missing.
/// When `boundary_root` is `Some(root)`, only `start` and its ancestors up to `root` are
/// inspected (never `current_dir()`, `current_exe()`, or paths outside `root`).
pub(crate) fn discover_tool_root(
    start: &Path,
    explicit: Option<&Path>,
    boundary_root: Option<&Path>,
) -> Result<Option<PathBuf>, String> {
    if let Some(bound) = boundary_root {
        return discover_tool_root_confined(start, explicit, bound);
    }
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
