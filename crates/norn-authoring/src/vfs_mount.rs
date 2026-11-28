use std::collections::BTreeMap;
use std::fs::{File, Metadata, OpenOptions};
use std::io::Read;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};

use crate::vfs::{
    MAX_PINNED_FILE_BYTES, MAX_PINNED_FILES, MAX_PINNED_TOTAL_BYTES, path_under_prefix,
    record_ancestor_dirs,
};
use crate::{
    AuthoringError, BoundedVfs, PINNED_EFFECT_VIRTUAL_ROOT, Profile, normalize_virtual_path,
};

struct StagedMount {
    prefix_with_slash: String,
    files: BTreeMap<String, String>,
    next_total_bytes: usize,
}

impl BoundedVfs {
    /// Loads `package.json` and declaration files (`*.d.ts`, `*.d.mts`,
    /// `*.d.cts`) from a host package directory into `virtual_root` in memory.
    ///
    /// Canonicalizes `disk_dir` once (so package-manager symlinks at the
    /// package root resolve cleanly), verifies no internal symlinks are
    /// followed, enforces bounded streaming reads, and publishes the mounted
    /// files and reserved namespace prefix atomically only if the entire load
    /// succeeds.
    ///
    /// # Errors
    ///
    /// Returns [`AuthoringError::Io`], [`AuthoringError::InvalidPath`], or
    /// [`AuthoringError::VfsQuotaExceeded`] if reading, validation, or mounting
    /// fails. On error, `self` is left unmodified.
    pub fn mount_pinned_package_dir(
        &mut self,
        virtual_root: &str,
        disk_dir: &Path,
    ) -> Result<usize, AuthoringError> {
        let staged = self.stage_pinned_package_dir(virtual_root, disk_dir)?;
        let count = staged.files.len();
        self.commit_staged_mount(staged);
        Ok(count)
    }

    /// Atomically loads `disk_dir`, validates that the exact staged
    /// `package.json` matches the profile's pinned `effect` version, and
    /// publishes all declarations under [`PINNED_EFFECT_VIRTUAL_ROOT`].
    ///
    /// # Errors
    ///
    /// Returns [`AuthoringError::PinMismatch`] if the package version differs
    /// from [`Profile::pins`], or [`AuthoringError::Io`] /
    /// [`AuthoringError::VfsQuotaExceeded`] / [`AuthoringError::InvalidPath`]
    /// if loading fails. On error, `self` is left unmodified.
    pub fn mount_pinned_effect_dir(
        &mut self,
        disk_dir: &Path,
        profile: &Profile,
    ) -> Result<usize, AuthoringError> {
        let staged = self.stage_pinned_package_dir(PINNED_EFFECT_VIRTUAL_ROOT, disk_dir)?;
        let manifest_path = format!("{PINNED_EFFECT_VIRTUAL_ROOT}/package.json");
        let Some(pkg_text) = staged.files.get(&manifest_path) else {
            return Err(AuthoringError::Io {
                path: disk_dir.join("package.json").display().to_string(),
                reason: "staged effect package is missing regular file package.json".to_owned(),
            });
        };
        let pkg_json: serde_json::Value =
            serde_json::from_str(pkg_text).map_err(|err| AuthoringError::Io {
                path: disk_dir.join("package.json").display().to_string(),
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
        let count = staged.files.len();
        self.commit_staged_mount(staged);
        Ok(count)
    }

    fn stage_pinned_package_dir(
        &self,
        virtual_root: &str,
        disk_dir: &Path,
    ) -> Result<StagedMount, AuthoringError> {
        if self.quota_breached() {
            return Err(AuthoringError::VfsQuotaExceeded(
                "virtual filesystem is locked after an earlier quota breach".to_owned(),
            ));
        }
        let root_virtual = normalize_virtual_path(virtual_root)?;
        let prefix_with_slash = format!("{root_virtual}/");
        if self.include_intrinsics
            && (path_under_prefix(&root_virtual, "/@norn/authoring/")
                || path_under_prefix("/@norn/authoring", &prefix_with_slash))
        {
            return Err(AuthoringError::InvalidPath {
                path: root_virtual,
                reason: "reserved for pinned @norn/authoring declarations".to_owned(),
            });
        }
        if self
            .files
            .keys()
            .any(|existing| path_under_prefix(existing, &prefix_with_slash))
        {
            return Err(AuthoringError::InvalidPath {
                path: root_virtual,
                reason: "cannot mount pinned package over existing caller files".to_owned(),
            });
        }

        let canonical_root = std::fs::canonicalize(disk_dir).map_err(|err| AuthoringError::Io {
            path: disk_dir.display().to_string(),
            reason: err.to_string(),
        })?;
        let root_meta =
            std::fs::symlink_metadata(&canonical_root).map_err(|err| AuthoringError::Io {
                path: canonical_root.display().to_string(),
                reason: err.to_string(),
            })?;
        if !root_meta.file_type().is_dir() {
            return Err(AuthoringError::Io {
                path: canonical_root.display().to_string(),
                reason: "pinned package root is not a directory".to_owned(),
            });
        }

        let mut staged = StagedMount {
            prefix_with_slash,
            files: BTreeMap::new(),
            next_total_bytes: self.pinned_total_bytes,
        };
        let mut total_count = self.pinned_files.len();
        let mut stack: Vec<(PathBuf, String)> = vec![(canonical_root.clone(), root_virtual)];

        while let Some((dir_path, virtual_prefix)) = stack.pop() {
            verify_no_internal_symlinks(&canonical_root, &dir_path)?;
            let entries = read_sorted_dir_entries(&dir_path)?;
            for (name, full_path, is_dir) in entries {
                let next_virtual = format!("{virtual_prefix}/{name}");
                if is_dir {
                    stack.push((full_path, next_virtual));
                } else {
                    self.stage_one_file(
                        &canonical_root,
                        &full_path,
                        &next_virtual,
                        &mut staged,
                        &mut total_count,
                    )?;
                }
            }
        }

        Ok(staged)
    }

    fn stage_one_file(
        &self,
        canonical_root: &Path,
        full_path: &Path,
        next_virtual: &str,
        staged: &mut StagedMount,
        total_count: &mut usize,
    ) -> Result<(), AuthoringError> {
        let content = read_regular_file_bounded(canonical_root, full_path, MAX_PINNED_FILE_BYTES)?;
        let normalized = normalize_virtual_path(next_virtual)?;
        if self.include_intrinsics && path_under_prefix(&normalized, "/@norn/authoring/") {
            return Err(AuthoringError::InvalidPath {
                path: normalized,
                reason: "reserved for pinned @norn/authoring declarations".to_owned(),
            });
        }
        let existing_len = staged
            .files
            .get(&normalized)
            .or_else(|| self.pinned_files.get(&normalized))
            .map_or(0, String::len);
        let is_new =
            !staged.files.contains_key(&normalized) && !self.pinned_files.contains_key(&normalized);
        if is_new {
            let next_count = total_count.saturating_add(1);
            *total_count = next_count;
            if next_count > MAX_PINNED_FILES {
                return Err(AuthoringError::VfsQuotaExceeded(format!(
                    "pinned file count {next_count} exceeds pinned cap ({MAX_PINNED_FILES})"
                )));
            }
        }
        let reduced = staged.next_total_bytes.saturating_sub(existing_len);
        let Some(updated) = reduced.checked_add(content.len()) else {
            return Err(AuthoringError::VfsQuotaExceeded(
                "pinned byte count overflowed usize".to_owned(),
            ));
        };
        if updated > MAX_PINNED_TOTAL_BYTES {
            return Err(AuthoringError::VfsQuotaExceeded(format!(
                "pinned total size {updated} bytes exceeds pinned cap ({MAX_PINNED_TOTAL_BYTES})"
            )));
        }
        staged.next_total_bytes = updated;
        staged.files.insert(normalized, content);
        Ok(())
    }

    fn commit_staged_mount(&mut self, staged: StagedMount) {
        self.reserved_prefixes.insert(staged.prefix_with_slash);
        self.pinned_total_bytes = staged.next_total_bytes;
        for (path, content) in staged.files {
            record_ancestor_dirs(&path, &mut self.directories);
            self.pinned_files.insert(path, content);
        }
    }
}

fn read_sorted_dir_entries(
    dir_path: &Path,
) -> Result<Vec<(String, PathBuf, bool)>, AuthoringError> {
    let mut entries = Vec::new();
    let read_dir = std::fs::read_dir(dir_path).map_err(|err| AuthoringError::Io {
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
    Ok(entries)
}

fn verify_no_internal_symlinks(canonical_root: &Path, target: &Path) -> Result<(), AuthoringError> {
    let rel = target
        .strip_prefix(canonical_root)
        .map_err(|err| AuthoringError::Io {
            path: target.display().to_string(),
            reason: format!("path escaped pinned root: {err}"),
        })?;
    let mut cursor = canonical_root.to_path_buf();
    for component in rel.components() {
        let Component::Normal(segment) = component else {
            return Err(AuthoringError::Io {
                path: target.display().to_string(),
                reason: "unexpected non-normal path component inside pinned root".to_owned(),
            });
        };
        cursor.push(segment);
        let meta = std::fs::symlink_metadata(&cursor).map_err(|err| AuthoringError::Io {
            path: cursor.display().to_string(),
            reason: err.to_string(),
        })?;
        if meta.file_type().is_symlink() {
            return Err(AuthoringError::Io {
                path: cursor.display().to_string(),
                reason: "symlinks inside pinned package directories are forbidden".to_owned(),
            });
        }
    }
    Ok(())
}

fn read_regular_file_bounded(
    canonical_root: &Path,
    full_path: &Path,
    max_bytes: usize,
) -> Result<String, AuthoringError> {
    verify_no_internal_symlinks(canonical_root, full_path)?;
    let lstat = std::fs::symlink_metadata(full_path).map_err(|err| AuthoringError::Io {
        path: full_path.display().to_string(),
        reason: err.to_string(),
    })?;
    if !lstat.file_type().is_file() {
        return Err(AuthoringError::Io {
            path: full_path.display().to_string(),
            reason: "expected regular file inside pinned package".to_owned(),
        });
    }
    let max_u64 = u64::try_from(max_bytes).unwrap_or(u64::MAX);
    if lstat.len() > max_u64 {
        return Err(AuthoringError::VfsQuotaExceeded(format!(
            "pinned file `{}` is {} bytes, exceeding pinned cap ({max_bytes})",
            full_path.display(),
            lstat.len()
        )));
    }

    let file = OpenOptions::new()
        .read(true)
        .open(full_path)
        .map_err(|err| AuthoringError::Io {
            path: full_path.display().to_string(),
            reason: err.to_string(),
        })?;
    verify_same_regular_file(full_path, &lstat, &file)?;

    let limit = max_u64.saturating_add(1);
    let mut bytes = Vec::new();
    file.take(limit)
        .read_to_end(&mut bytes)
        .map_err(|err| AuthoringError::Io {
            path: full_path.display().to_string(),
            reason: err.to_string(),
        })?;
    if bytes.len() > max_bytes {
        return Err(AuthoringError::VfsQuotaExceeded(format!(
            "pinned file `{}` exceeded pinned cap ({max_bytes} bytes) while reading",
            full_path.display()
        )));
    }
    String::from_utf8(bytes).map_err(|err| AuthoringError::Io {
        path: full_path.display().to_string(),
        reason: format!("invalid UTF-8: {err}"),
    })
}

fn verify_same_regular_file(
    full_path: &Path,
    lstat: &Metadata,
    file: &File,
) -> Result<(), AuthoringError> {
    let fstat = file.metadata().map_err(|err| AuthoringError::Io {
        path: full_path.display().to_string(),
        reason: err.to_string(),
    })?;
    if !fstat.file_type().is_file() {
        return Err(AuthoringError::Io {
            path: full_path.display().to_string(),
            reason: "opened path is no longer a regular file".to_owned(),
        });
    }
    #[cfg(unix)]
    if lstat.dev() != fstat.dev() || lstat.ino() != fstat.ino() {
        return Err(AuthoringError::Io {
            path: full_path.display().to_string(),
            reason: "file changed concurrently while opening (possible symlink swap)".to_owned(),
        });
    }
    #[cfg(not(unix))]
    let _ = lstat;
    Ok(())
}

fn is_declaration_or_manifest(name: &str) -> bool {
    name == "package.json"
        || name.ends_with(".d.ts")
        || name.ends_with(".d.mts")
        || name.ends_with(".d.cts")
}
