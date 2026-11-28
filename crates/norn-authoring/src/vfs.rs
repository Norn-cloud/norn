use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::SystemTime;

use tsrs_vfs::{Entries, FS, FileInfo, FileMode, bundled};

use crate::{AuthoringError, INTRINSICS_DTS, Profile};

/// Canonical virtual path of the embedded `@norn/authoring` ambient declarations.
pub const INTRINSICS_VIRTUAL_PATH: &str = "/@norn/authoring/index.d.ts";

/// Canonical virtual root where the pinned `effect` package declarations are mounted.
pub const PINNED_EFFECT_VIRTUAL_ROOT: &str = "/node_modules/effect";

/// Canonical virtual prefix for pinned `effect` declaration files under `dist/`.
pub const PINNED_EFFECT_DIST_PREFIX: &str = "/node_modules/effect/dist/";

const BUNDLED_SCHEME: &str = "bundled:///";
const MAX_PINNED_FILES: usize = 4_096;
const MAX_PINNED_FILE_BYTES: usize = 16_usize.saturating_mul(1_024).saturating_mul(1_024);
const MAX_PINNED_TOTAL_BYTES: usize = 64_usize.saturating_mul(1_024).saturating_mul(1_024);

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

fn record_ancestor_dirs(path: &str, dirs: &mut BTreeSet<String>) {
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

/// Read-only, symlink-free, quota-bounded in-memory [`tsrs_vfs::FS`].
///
/// Serves:
/// 1. Embedded TypeScript standard libraries (`bundled:///libs/lib.*.d.ts`)
///    provided by [`tsrs_vfs::bundled`].
/// 2. The pinned `@norn/authoring` ambient declarations at
///    [`INTRINSICS_VIRTUAL_PATH`].
/// 3. Caller-supplied files admitted under [`VfsLimits`].
///
/// Any quota breach permanently marks the filesystem as breached and causes all
/// subsequent insertions and reads to fail closed.
#[derive(Debug)]
pub struct BoundedVfs {
    limits: VfsLimits,
    files: BTreeMap<String, String>,
    pinned_files: BTreeMap<String, String>,
    directories: BTreeSet<String>,
    total_bytes: usize,
    pinned_total_bytes: usize,
    include_intrinsics: bool,
    quota_breached: AtomicBool,
}

impl BoundedVfs {
    /// Creates an empty bounded VFS with the embedded `@norn/authoring`
    /// declarations mounted at [`INTRINSICS_VIRTUAL_PATH`].
    #[must_use]
    pub fn new(limits: VfsLimits) -> Self {
        let mut directories = BTreeSet::new();
        record_ancestor_dirs(INTRINSICS_VIRTUAL_PATH, &mut directories);
        Self {
            limits,
            files: BTreeMap::new(),
            pinned_files: BTreeMap::new(),
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
        Self {
            limits,
            files: BTreeMap::new(),
            pinned_files: BTreeMap::new(),
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

    /// Inserts or replaces a caller-supplied virtual file under [`VfsLimits`].
    ///
    /// Returns the normalized virtual path on success.
    ///
    /// # Errors
    ///
    /// Returns [`AuthoringError::InvalidPath`] if `path` is invalid or collides
    /// with the reserved [`INTRINSICS_VIRTUAL_PATH`], or
    /// [`AuthoringError::VfsQuotaExceeded`] if a quota has been or would be
    /// exceeded.
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

        if self.include_intrinsics && normalized == INTRINSICS_VIRTUAL_PATH {
            return Err(AuthoringError::InvalidPath {
                path: normalized,
                reason: "reserved for pinned @norn/authoring declarations".to_owned(),
            });
        }
        if self.pinned_files.contains_key(&normalized) {
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
    /// enforcing hard pinned safety bounds and preventing caller overwrites.
    ///
    /// # Errors
    ///
    /// Returns [`AuthoringError::InvalidPath`] if `path` is malformed or
    /// collides with [`INTRINSICS_VIRTUAL_PATH`], or
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
        if self.include_intrinsics && normalized == INTRINSICS_VIRTUAL_PATH {
            return Err(AuthoringError::InvalidPath {
                path: normalized,
                reason: "reserved for pinned @norn/authoring declarations".to_owned(),
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

    /// Loads `package.json` and declaration files (`*.d.ts`, `*.d.mts`,
    /// `*.d.cts`) from a host package directory into `virtual_root` in memory.
    ///
    /// Canonicalizes `disk_dir` once (so package-manager symlinks at the
    /// package root resolve cleanly), then walks strictly without following
    /// symlinks or descending into nested `node_modules` directories.
    ///
    /// # Errors
    ///
    /// Returns [`AuthoringError::Io`], [`AuthoringError::InvalidPath`], or
    /// [`AuthoringError::VfsQuotaExceeded`] if reading or mounting fails.
    pub fn mount_pinned_package_dir(
        &mut self,
        virtual_root: &str,
        disk_dir: &Path,
    ) -> Result<usize, AuthoringError> {
        let root_virtual = normalize_virtual_path(virtual_root)?;
        let canonical_root = std::fs::canonicalize(disk_dir).map_err(|err| AuthoringError::Io {
            path: disk_dir.display().to_string(),
            reason: err.to_string(),
        })?;
        let mut stack: Vec<(PathBuf, String)> = vec![(canonical_root, root_virtual)];
        let mut loaded: usize = 0;

        while let Some((dir_path, virtual_prefix)) = stack.pop() {
            let mut entries: Vec<(String, PathBuf, bool)> = Vec::new();
            let read_dir = std::fs::read_dir(&dir_path).map_err(|err| AuthoringError::Io {
                path: dir_path.display().to_string(),
                reason: err.to_string(),
            })?;
            for entry_res in read_dir {
                let entry = entry_res.map_err(|err| AuthoringError::Io {
                    path: dir_path.display().to_string(),
                    reason: err.to_string(),
                })?;
                let file_type = entry.file_type().map_err(|err| AuthoringError::Io {
                    path: entry.path().display().to_string(),
                    reason: err.to_string(),
                })?;
                if file_type.is_symlink() {
                    continue;
                }
                let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                    continue;
                };
                if file_type.is_dir() {
                    if name != "node_modules" {
                        entries.push((name, entry.path(), true));
                    }
                } else if file_type.is_file() && is_declaration_or_manifest(&name) {
                    entries.push((name, entry.path(), false));
                }
            }
            entries.sort_by(|left, right| left.0.cmp(&right.0));
            for (name, full_path, is_dir) in entries {
                let next_virtual = format!("{virtual_prefix}/{name}");
                if is_dir {
                    stack.push((full_path, next_virtual));
                } else {
                    let content =
                        std::fs::read_to_string(&full_path).map_err(|err| AuthoringError::Io {
                            path: full_path.display().to_string(),
                            reason: err.to_string(),
                        })?;
                    self.add_pinned_file(&next_virtual, content)?;
                    loaded = loaded.saturating_add(1);
                }
            }
        }
        Ok(loaded)
    }

    /// Validates that `disk_dir/package.json` matches the profile's pinned
    /// `effect` version and mounts its declarations under
    /// [`PINNED_EFFECT_VIRTUAL_ROOT`].
    ///
    /// # Errors
    ///
    /// Returns [`AuthoringError::PinMismatch`] if the package version differs
    /// from [`Profile::pins`], or [`AuthoringError::Io`] /
    /// [`AuthoringError::VfsQuotaExceeded`] if loading fails.
    pub fn mount_pinned_effect_dir(
        &mut self,
        disk_dir: &Path,
        profile: &Profile,
    ) -> Result<usize, AuthoringError> {
        let pkg_path = disk_dir.join("package.json");
        let pkg_text = std::fs::read_to_string(&pkg_path).map_err(|err| AuthoringError::Io {
            path: pkg_path.display().to_string(),
            reason: err.to_string(),
        })?;
        let pkg_json: serde_json::Value =
            serde_json::from_str(&pkg_text).map_err(|err| AuthoringError::Io {
                path: pkg_path.display().to_string(),
                reason: err.to_string(),
            })?;
        let actual_version = pkg_json
            .get("version")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        if actual_version != profile.pins.effect.version {
            return Err(AuthoringError::PinMismatch {
                package: "effect".to_owned(),
                expected: profile.pins.effect.version.clone(),
                actual: actual_version.to_owned(),
            });
        }
        self.mount_pinned_package_dir(PINNED_EFFECT_VIRTUAL_ROOT, disk_dir)
    }

    fn read_virtual(&self, normalized: &str) -> Option<&str> {
        if self.include_intrinsics && normalized == INTRINSICS_VIRTUAL_PATH {
            return Some(INTRINSICS_DTS);
        }
        if let Some(pinned) = self.pinned_files.get(normalized) {
            return Some(pinned.as_str());
        }
        self.files.get(normalized).map(String::as_str)
    }
}

fn is_declaration_or_manifest(name: &str) -> bool {
    name == "package.json"
        || name.ends_with(".d.ts")
        || name.ends_with(".d.mts")
        || name.ends_with(".d.cts")
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
