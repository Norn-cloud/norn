use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::SystemTime;

use tsrs_vfs::{Entries, FS, FileInfo, FileMode, bundled};

use crate::{AuthoringError, INTRINSICS_DTS};

/// Canonical virtual path of the embedded `@norn/authoring` ambient declarations.
pub const INTRINSICS_VIRTUAL_PATH: &str = "/@norn/authoring/index.d.ts";

/// Canonical virtual root reserved for `@norn/authoring` declarations.
pub(crate) const INTRINSICS_VIRTUAL_PREFIX: &str = "/@norn/authoring/";

/// Canonical virtual root where the pinned `effect` package declarations are mounted.
pub const PINNED_EFFECT_VIRTUAL_ROOT: &str = "/node_modules/effect";

/// Canonical virtual prefix for pinned `effect` declaration files under `dist/`.
pub const PINNED_EFFECT_DIST_PREFIX: &str = "/node_modules/effect/dist/";

pub(crate) const PINNED_EFFECT_VIRTUAL_PREFIX: &str = "/node_modules/effect/";
const BUNDLED_SCHEME: &str = "bundled:///";

pub(crate) const MAX_PINNED_FILES: usize = 4_096;
pub(crate) const MAX_PINNED_FILE_BYTES: usize =
    16_usize.saturating_mul(1_024).saturating_mul(1_024);
pub(crate) const MAX_PINNED_TOTAL_BYTES: usize =
    64_usize.saturating_mul(1_024).saturating_mul(1_024);

/// Hard resource ceilings enforced by [`BoundedVfs`] on caller-supplied files.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VfsLimits {
    /// Maximum number of caller-supplied files admitted into the virtual filesystem.
    pub max_files: usize,
    /// Maximum cumulative `UTF-8` byte length across all caller-supplied files.
    pub max_total_bytes: usize,
    /// Maximum `UTF-8` byte length of any single caller-supplied file.
    pub max_file_bytes: usize,
    /// Maximum `UTF-8` byte length of any virtual file path.
    pub max_path_bytes: usize,
    /// Maximum number of path segments (directory nesting depth plus filename).
    pub max_path_depth: usize,
}

impl Default for VfsLimits {
    fn default() -> Self {
        Self {
            max_files: 1_024,
            max_total_bytes: 32_usize.saturating_mul(1_024).saturating_mul(1_024),
            max_file_bytes: 4_usize.saturating_mul(1_024).saturating_mul(1_024),
            max_path_bytes: 512,
            max_path_depth: 32,
        }
    }
}

/// Normalizes a caller-supplied virtual path to a canonical POSIX absolute path.
///
/// Rejects empty paths, NUL bytes, and parent-directory traversal (`..`) so the
/// virtual filesystem stays strictly confined and symlink-free.
///
/// # Errors
///
/// Returns [`AuthoringError::InvalidPath`] if `path` is empty, contains NUL
/// bytes, or contains a `..` path component.
pub fn normalize_virtual_path(path: &str) -> Result<String, AuthoringError> {
    if path.is_empty() {
        return Err(AuthoringError::InvalidPath {
            path: path.to_owned(),
            reason: "path must not be empty".to_owned(),
        });
    }
    if path.bytes().any(|byte| byte == 0) {
        return Err(AuthoringError::InvalidPath {
            path: path.to_owned(),
            reason: "path must not contain NUL bytes".to_owned(),
        });
    }
    let replaced = path.replace('\\', "/");
    let mut parts: Vec<&str> = Vec::new();
    for segment in replaced.split('/') {
        if segment.is_empty() || segment == "." {
            continue;
        }
        if segment == ".." {
            return Err(AuthoringError::InvalidPath {
                path: path.to_owned(),
                reason: "parent directory traversal (`..`) is forbidden".to_owned(),
            });
        }
        parts.push(segment);
    }
    if parts.is_empty() {
        return Err(AuthoringError::InvalidPath {
            path: path.to_owned(),
            reason: "path must name a file, not root `/`".to_owned(),
        });
    }
    let mut normalized = String::new();
    for part in parts {
        normalized.push('/');
        normalized.push_str(part);
    }
    Ok(normalized)
}

fn normalize_lookup_path(path: &str) -> Option<String> {
    if path.is_empty() || path.bytes().any(|byte| byte == 0) {
        return None;
    }
    let replaced = path.replace('\\', "/");
    let mut parts: Vec<&str> = Vec::new();
    for segment in replaced.split('/') {
        if segment.is_empty() || segment == "." {
            continue;
        }
        if segment == ".." {
            return None;
        }
        parts.push(segment);
    }
    if parts.is_empty() {
        return Some("/".to_owned());
    }
    let mut normalized = String::new();
    for part in parts {
        normalized.push('/');
        normalized.push_str(part);
    }
    Some(normalized)
}

pub(crate) fn record_ancestor_dirs(path: &str, dirs: &mut BTreeSet<String>) {
    dirs.insert("/".to_owned());
    let mut current = path;
    while let Some((parent, _)) = current.rsplit_once('/') {
        if parent.is_empty() {
            dirs.insert("/".to_owned());
            break;
        }
        dirs.insert(parent.to_owned());
        current = parent;
    }
}

fn file_basename(path: &str) -> &str {
    path.rsplit_once('/').map_or(path, |(_, name)| name)
}

pub(crate) fn path_under_prefix(normalized: &str, prefix_with_slash: &str) -> bool {
    let trimmed = prefix_with_slash.trim_end_matches('/');
    normalized == trimmed || normalized.starts_with(prefix_with_slash)
}

/// Read-only, symlink-free, quota-bounded in-memory [`tsrs_vfs::FS`].
///
/// Serves:
/// 1. Embedded TypeScript standard libraries (`bundled:///libs/lib.*.d.ts`)
///    provided by [`tsrs_vfs::bundled`].
/// 2. The pinned `@norn/authoring` ambient declarations at
///    [`INTRINSICS_VIRTUAL_PATH`] (reserving the entire `/@norn/authoring/`
///    namespace).
/// 3. Explicitly mounted pinned package declarations (reserving
///    `/node_modules/effect/` and every mounted pinned root against caller
///    writes or fallback reads).
/// 4. Caller-supplied files admitted under [`VfsLimits`].
///
/// Any quota breach permanently marks the filesystem as breached and causes all
/// subsequent insertions and reads to fail closed.
#[derive(Debug)]
pub struct BoundedVfs {
    limits: VfsLimits,
    pub(crate) files: BTreeMap<String, String>,
    pub(crate) pinned_files: BTreeMap<String, String>,
    pub(crate) reserved_prefixes: BTreeSet<String>,
    pub(crate) directories: BTreeSet<String>,
    total_bytes: usize,
    pub(crate) pinned_total_bytes: usize,
    pub(crate) include_intrinsics: bool,
    quota_breached: AtomicBool,
}

impl BoundedVfs {
    /// Creates an empty bounded VFS with the embedded `@norn/authoring`
    /// declarations mounted at [`INTRINSICS_VIRTUAL_PATH`] and the pinned
    /// `/@norn/authoring/` and `/node_modules/effect/` namespaces reserved.
    #[must_use]
    pub fn new(limits: VfsLimits) -> Self {
        let mut directories = BTreeSet::new();
        record_ancestor_dirs(INTRINSICS_VIRTUAL_PATH, &mut directories);
        let mut reserved_prefixes = BTreeSet::new();
        reserved_prefixes.insert(INTRINSICS_VIRTUAL_PREFIX.to_owned());
        reserved_prefixes.insert(PINNED_EFFECT_VIRTUAL_PREFIX.to_owned());
        Self {
            limits,
            files: BTreeMap::new(),
            pinned_files: BTreeMap::new(),
            reserved_prefixes,
            directories,
            total_bytes: 0,
            pinned_total_bytes: 0,
            include_intrinsics: true,
            quota_breached: AtomicBool::new(false),
        }
    }

    /// Creates an empty bounded VFS without mounting `@norn/authoring`
    /// declarations (used by standalone formatting passes).
    #[must_use]
    pub(crate) fn without_intrinsics(limits: VfsLimits) -> Self {
        let mut directories = BTreeSet::new();
        directories.insert("/".to_owned());
        let mut reserved_prefixes = BTreeSet::new();
        reserved_prefixes.insert(PINNED_EFFECT_VIRTUAL_PREFIX.to_owned());
        Self {
            limits,
            files: BTreeMap::new(),
            pinned_files: BTreeMap::new(),
            reserved_prefixes,
            directories,
            total_bytes: 0,
            pinned_total_bytes: 0,
            include_intrinsics: false,
            quota_breached: AtomicBool::new(false),
        }
    }

    /// Returns the configured resource limits.
    #[must_use]
    pub fn limits(&self) -> VfsLimits {
        self.limits
    }

    /// Returns the number of caller-supplied files currently stored.
    #[must_use]
    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    /// Returns the cumulative `UTF-8` byte length of caller-supplied files.
    #[must_use]
    pub fn total_bytes(&self) -> usize {
        self.total_bytes
    }

    /// Returns `true` if any quota limit was breached; when `true`, the VFS
    /// fails closed on every subsequent mutation and read.
    #[must_use]
    pub fn quota_breached(&self) -> bool {
        self.quota_breached.load(Ordering::Acquire)
    }

    /// Returns `true` iff `path` resolves to the embedded `@norn/authoring`
    /// declaration or a verified pinned package file (never a caller file).
    #[must_use]
    pub fn is_pinned_file(&self, path: &str) -> bool {
        if self.quota_breached() {
            return false;
        }
        normalize_lookup_path(path).is_some_and(|norm| {
            (self.include_intrinsics && norm == INTRINSICS_VIRTUAL_PATH)
                || self.pinned_files.contains_key(&norm)
        })
    }

    /// Returns `true` if `normalized` lies inside any reserved pinned namespace.
    #[must_use]
    pub(crate) fn is_reserved_prefix(&self, normalized: &str) -> bool {
        self.reserved_prefixes
            .iter()
            .any(|prefix| path_under_prefix(normalized, prefix))
    }

    /// Inserts or replaces a caller-supplied virtual file under [`VfsLimits`].
    ///
    /// Returns the normalized virtual path on success.
    ///
    /// # Errors
    ///
    /// Returns [`AuthoringError::InvalidPath`] if `path` is invalid or falls
    /// inside a reserved pinned namespace (such as `/@norn/authoring/` or
    /// `/node_modules/effect/`), or [`AuthoringError::VfsQuotaExceeded`] if a
    /// quota has been or would be exceeded.
    pub fn add_file(
        &mut self,
        path: &str,
        content: impl Into<String>,
    ) -> Result<String, AuthoringError> {
        if self.quota_breached() {
            return Err(AuthoringError::VfsQuotaExceeded(
                "virtual filesystem is locked after an earlier quota breach".to_owned(),
            ));
        }

        if path.len() > self.limits.max_path_bytes {
            self.quota_breached.store(true, Ordering::Release);
            return Err(AuthoringError::VfsQuotaExceeded(format!(
                "path length {} bytes exceeds max_path_bytes ({})",
                path.len(),
                self.limits.max_path_bytes
            )));
        }

        let normalized = normalize_virtual_path(path)?;
        if normalized.len() > self.limits.max_path_bytes {
            self.quota_breached.store(true, Ordering::Release);
            return Err(AuthoringError::VfsQuotaExceeded(format!(
                "normalized path `{normalized}` is {} bytes, exceeding max_path_bytes ({})",
                normalized.len(),
                self.limits.max_path_bytes
            )));
        }

        let depth = normalized
            .split('/')
            .filter(|segment| !segment.is_empty())
            .count();
        if depth > self.limits.max_path_depth {
            self.quota_breached.store(true, Ordering::Release);
            return Err(AuthoringError::VfsQuotaExceeded(format!(
                "path depth {depth} exceeds max_path_depth ({})",
                self.limits.max_path_depth
            )));
        }

        if self.is_reserved_prefix(&normalized) || self.pinned_files.contains_key(&normalized) {
            return Err(AuthoringError::InvalidPath {
                path: normalized,
                reason: "reserved for pinned package declarations".to_owned(),
            });
        }

        let text = content.into();
        let new_len = text.len();
        if new_len > self.limits.max_file_bytes {
            self.quota_breached.store(true, Ordering::Release);
            return Err(AuthoringError::VfsQuotaExceeded(format!(
                "file `{normalized}` is {new_len} bytes, exceeding max_file_bytes ({})",
                self.limits.max_file_bytes
            )));
        }

        let existing_len = self.files.get(&normalized).map_or(0, String::len);
        let is_new_file = !self.files.contains_key(&normalized);
        let next_file_count = if is_new_file {
            self.files.len().saturating_add(1)
        } else {
            self.files.len()
        };
        if next_file_count > self.limits.max_files {
            self.quota_breached.store(true, Ordering::Release);
            return Err(AuthoringError::VfsQuotaExceeded(format!(
                "file count {next_file_count} exceeds max_files ({})",
                self.limits.max_files
            )));
        }

        let reduced_total = self.total_bytes.saturating_sub(existing_len);
        let Some(next_total) = reduced_total.checked_add(new_len) else {
            self.quota_breached.store(true, Ordering::Release);
            return Err(AuthoringError::VfsQuotaExceeded(
                "total byte count overflowed usize".to_owned(),
            ));
        };
        if next_total > self.limits.max_total_bytes {
            self.quota_breached.store(true, Ordering::Release);
            return Err(AuthoringError::VfsQuotaExceeded(format!(
                "total size {next_total} bytes exceeds max_total_bytes ({})",
                self.limits.max_total_bytes
            )));
        }

        self.total_bytes = next_total;
        record_ancestor_dirs(&normalized, &mut self.directories);
        self.files.insert(normalized.clone(), text);
        Ok(normalized)
    }

    /// Inserts a trusted pinned package declaration or manifest file (for
    /// example under [`PINNED_EFFECT_VIRTUAL_ROOT`]).
    ///
    /// Pinned package files are isolated from caller [`VfsLimits`] so large
    /// declaration bundles do not starve authored sources, while still
    /// enforcing hard pinned safety bounds and preventing caller collisions.
    ///
    /// # Errors
    ///
    /// Returns [`AuthoringError::InvalidPath`] if `path` is malformed,
    /// collides with `/@norn/authoring/`, collides with `/node_modules/effect/`
    /// (which must be mounted via [`Self::mount_pinned_effect_dir`]), or
    /// collides with an existing caller file, or
    /// [`AuthoringError::VfsQuotaExceeded`] if pinned safety limits are
    /// exceeded.
    pub fn add_pinned_file(
        &mut self,
        path: &str,
        content: impl Into<String>,
    ) -> Result<String, AuthoringError> {
        if self.quota_breached() {
            return Err(AuthoringError::VfsQuotaExceeded(
                "virtual filesystem is locked after an earlier quota breach".to_owned(),
            ));
        }
        let normalized = normalize_virtual_path(path)?;
        if self.include_intrinsics && path_under_prefix(&normalized, INTRINSICS_VIRTUAL_PREFIX) {
            return Err(AuthoringError::InvalidPath {
                path: normalized,
                reason: "reserved for pinned @norn/authoring declarations".to_owned(),
            });
        }
        if path_under_prefix(&normalized, PINNED_EFFECT_VIRTUAL_PREFIX) {
            return Err(AuthoringError::InvalidPath {
                path: normalized,
                reason: "use mount_pinned_effect_dir to mount the version-pinned effect package"
                    .to_owned(),
            });
        }
        if self.files.contains_key(&normalized) {
            return Err(AuthoringError::InvalidPath {
                path: normalized,
                reason: "cannot mount pinned file over an existing caller file".to_owned(),
            });
        }
        let text = content.into();
        let new_len = text.len();
        if new_len > MAX_PINNED_FILE_BYTES {
            return Err(AuthoringError::VfsQuotaExceeded(format!(
                "pinned file `{normalized}` is {new_len} bytes, exceeding pinned cap ({MAX_PINNED_FILE_BYTES})"
            )));
        }
        let existing_len = self.pinned_files.get(&normalized).map_or(0, String::len);
        let next_count = if self.pinned_files.contains_key(&normalized) {
            self.pinned_files.len()
        } else {
            self.pinned_files.len().saturating_add(1)
        };
        if next_count > MAX_PINNED_FILES {
            return Err(AuthoringError::VfsQuotaExceeded(format!(
                "pinned file count {next_count} exceeds pinned cap ({MAX_PINNED_FILES})"
            )));
        }
        let reduced = self.pinned_total_bytes.saturating_sub(existing_len);
        let Some(next_total) = reduced.checked_add(new_len) else {
            return Err(AuthoringError::VfsQuotaExceeded(
                "pinned byte count overflowed usize".to_owned(),
            ));
        };
        if next_total > MAX_PINNED_TOTAL_BYTES {
            return Err(AuthoringError::VfsQuotaExceeded(format!(
                "pinned total size {next_total} bytes exceeds pinned cap ({MAX_PINNED_TOTAL_BYTES})"
            )));
        }
        self.pinned_total_bytes = next_total;
        record_ancestor_dirs(&normalized, &mut self.directories);
        self.pinned_files.insert(normalized.clone(), text);
        Ok(normalized)
    }

    pub(crate) fn rebuild_directories(&mut self) {
        self.directories.clear();
        self.directories.insert("/".to_owned());
        if self.include_intrinsics {
            record_ancestor_dirs(INTRINSICS_VIRTUAL_PATH, &mut self.directories);
        }
        for path in self.files.keys().chain(self.pinned_files.keys()) {
            record_ancestor_dirs(path, &mut self.directories);
        }
    }

    fn read_virtual(&self, normalized: &str) -> Option<&str> {
        if self.include_intrinsics && normalized == INTRINSICS_VIRTUAL_PATH {
            return Some(INTRINSICS_DTS);
        }
        if let Some(pinned) = self.pinned_files.get(normalized) {
            return Some(pinned.as_str());
        }
        // Never fall back to caller files inside any reserved pinned namespace.
        if self.is_reserved_prefix(normalized) {
            return None;
        }
        self.files.get(normalized).map(String::as_str)
    }
}

impl Default for BoundedVfs {
    fn default() -> Self {
        Self::new(VfsLimits::default())
    }
}

impl FS for BoundedVfs {
    fn use_case_sensitive_file_names(&self) -> bool {
        true
    }

    fn file_exists(&self, path: &str) -> bool {
        if self.quota_breached() {
            return false;
        }
        if bundled::is_bundled(path) {
            return bundled::embedded_file(path).is_some();
        }
        normalize_lookup_path(path).is_some_and(|norm| self.read_virtual(&norm).is_some())
    }

    fn read_file(&self, path: &str) -> Option<String> {
        if self.quota_breached() {
            return None;
        }
        if bundled::is_bundled(path) {
            return bundled::embedded_file(path).map(str::to_owned);
        }
        let norm = normalize_lookup_path(path)?;
        self.read_virtual(&norm).map(str::to_owned)
    }

    fn write_file(&self, _path: &str, _data: &str) -> Result<(), String> {
        Err("BoundedVfs is read-only".to_owned())
    }

    fn append_file(&self, _path: &str, _data: &str) -> Result<(), String> {
        Err("BoundedVfs is read-only".to_owned())
    }

    fn remove(&self, _path: &str) -> Result<(), String> {
        Err("BoundedVfs is read-only".to_owned())
    }

    fn chtimes(&self, _path: &str, _a_time: SystemTime, _m_time: SystemTime) -> Result<(), String> {
        Err("BoundedVfs is read-only".to_owned())
    }

    fn directory_exists(&self, path: &str) -> bool {
        if self.quota_breached() {
            return false;
        }
        if let Some(rest) = path.strip_prefix(BUNDLED_SCHEME) {
            return rest.is_empty() || rest == "libs";
        }
        normalize_lookup_path(path).is_some_and(|norm| self.directories.contains(&norm))
    }

    fn get_accessible_entries(&self, path: &str) -> Entries {
        if self.quota_breached() {
            return Entries::default();
        }
        if let Some(rest) = path.strip_prefix(BUNDLED_SCHEME) {
            let mut entries = Entries::default();
            if rest.is_empty() {
                entries.directories.push("libs".to_owned());
            } else if rest == "libs" {
                entries.files = bundled::LIB_NAMES
                    .iter()
                    .copied()
                    .map(str::to_owned)
                    .collect();
            }
            return entries;
        }

        let Some(dir) = normalize_lookup_path(path) else {
            return Entries::default();
        };
        if !self.directories.contains(&dir) {
            return Entries::default();
        }

        let prefix = if dir == "/" {
            "/".to_owned()
        } else {
            format!("{dir}/")
        };

        let mut files = BTreeSet::new();
        let mut directories = BTreeSet::new();

        for file_path in self
            .files
            .keys()
            .map(String::as_str)
            .filter(|candidate| !self.is_reserved_prefix(candidate))
            .chain(self.pinned_files.keys().map(String::as_str))
            .chain(self.include_intrinsics.then_some(INTRINSICS_VIRTUAL_PATH))
        {
            if let Some(rest) = file_path.strip_prefix(&prefix)
                && !rest.contains('/')
            {
                files.insert(rest.to_owned());
            }
        }

        for dir_path in &self.directories {
            if let Some(rest) = dir_path.strip_prefix(&prefix)
                && !rest.is_empty()
                && !rest.contains('/')
            {
                directories.insert(rest.to_owned());
            }
        }

        Entries {
            files: files.into_iter().collect(),
            directories: directories.into_iter().collect(),
            symlinks: None,
        }
    }

    fn stat(&self, path: &str) -> Option<FileInfo> {
        if self.quota_breached() {
            return None;
        }
        if let Some(rest) = path.strip_prefix(BUNDLED_SCHEME) {
            if rest.is_empty() || rest == "libs" {
                return Some(FileInfo {
                    name: rest.to_owned(),
                    size: 0,
                    mode: FileMode::Dir,
                    mod_time: None,
                });
            }
            let lib = bundled::embedded_file(path)?;
            let name = rest.strip_prefix("libs/").unwrap_or(rest).to_owned();
            return Some(FileInfo {
                name,
                size: i64::try_from(lib.len()).unwrap_or(i64::MAX),
                mode: FileMode::None,
                mod_time: None,
            });
        }

        let norm = normalize_lookup_path(path)?;
        if let Some(content) = self.read_virtual(&norm) {
            return Some(FileInfo {
                name: file_basename(&norm).to_owned(),
                size: i64::try_from(content.len()).unwrap_or(i64::MAX),
                mode: FileMode::None,
                mod_time: None,
            });
        }
        if self.directories.contains(&norm) {
            return Some(FileInfo {
                name: file_basename(&norm).to_owned(),
                size: 0,
                mode: FileMode::Dir,
                mod_time: None,
            });
        }
        None
    }

    fn realpath(&self, path: &str) -> String {
        if bundled::is_bundled(path) {
            return path.to_owned();
        }
        normalize_lookup_path(path).unwrap_or_else(|| path.to_owned())
    }
}
