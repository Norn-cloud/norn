use std::collections::BTreeMap;
use std::fs::File;
#[cfg(not(unix))]
use std::fs::OpenOptions;
use std::io::Read;
use std::path::Path;
#[cfg(not(unix))]
use std::path::PathBuf;

#[cfg(unix)]
use rustix::fs::{CWD, Dir, Mode, OFlags, openat};
#[cfg(unix)]
use rustix::io::Errno;

use crate::vfs::{
    INTRINSICS_VIRTUAL_PREFIX, MAX_PINNED_FILE_BYTES, MAX_PINNED_FILES, MAX_PINNED_TOTAL_BYTES,
    PINNED_EFFECT_VIRTUAL_PREFIX, path_under_prefix,
};
use crate::{
    AuthoringError, BoundedVfs, PINNED_EFFECT_VIRTUAL_ROOT, Profile, normalize_virtual_path,
};

const MAX_MOUNT_DEPTH: usize = 64;

struct StagedMount {
    prefix_with_slash: String,
    files: BTreeMap<String, String>,
    total_count: usize,
    next_total_bytes: usize,
    include_intrinsics: bool,
}

fn prefixes_overlap(a_with_slash: &str, b_with_slash: &str) -> bool {
    let a_trimmed = a_with_slash.trim_end_matches('/');
    let b_trimmed = b_with_slash.trim_end_matches('/');
    path_under_prefix(a_trimmed, b_with_slash) || path_under_prefix(b_trimmed, a_with_slash)
}

impl BoundedVfs {
    /// Loads `package.json` and declaration files (`*.d.ts`, `*.d.mts`,
    /// `*.d.cts`) from a host package directory into `virtual_root` in memory.
    ///
    /// Canonicalizes `disk_dir` once (so package-manager symlinks at the
    /// package root resolve cleanly), traverses via fd-relative `openat` with
    /// `O_NOFOLLOW | O_NONBLOCK` on Unix so no internal symlinks or concurrent
    /// ancestor swaps can redirect traversal, enforces bounded streaming reads,
    /// and atomically replaces any prior mount at the exact `virtual_root` only
    /// if the entire load succeeds.
    ///
    /// Rejects any `virtual_root` overlapping `/@norn/authoring/` or
    /// `/node_modules/effect/` (use [`Self::mount_pinned_effect_dir`] for the
    /// version-pinned `effect` package), as well as any strict ancestor/child
    /// overlap with an existing reserved pinned namespace.
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
        let staged = self.stage_pinned_package_dir(virtual_root, disk_dir, false)?;
        let count = staged.files.len();
        self.commit_staged_mount(staged);
        Ok(count)
    }

    /// Atomically loads `disk_dir`, validates that the exact staged
    /// `package.json` matches the profile's pinned `effect` version, and
    /// replaces all mounted declarations under [`PINNED_EFFECT_VIRTUAL_ROOT`].
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
        let staged = self.stage_pinned_package_dir(PINNED_EFFECT_VIRTUAL_ROOT, disk_dir, true)?;
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
        allow_effect_root: bool,
    ) -> Result<StagedMount, AuthoringError> {
        if self.quota_breached() {
            return Err(AuthoringError::VfsQuotaExceeded(
                "virtual filesystem is locked after an earlier quota breach".to_owned(),
            ));
        }
        let root_virtual = normalize_virtual_path(virtual_root)?;
        let prefix_with_slash = format!("{root_virtual}/");
        self.validate_mount_root(&root_virtual, &prefix_with_slash, allow_effect_root)?;

        // Compute pinned quotas excluding any prior files under `prefix_with_slash`
        // that this exact-prefix mount will atomically replace on commit.
        let mut retained_count: usize = 0;
        let mut retained_bytes: usize = 0;
        for (existing_path, existing_content) in &self.pinned_files {
            if !path_under_prefix(existing_path, &prefix_with_slash) {
                retained_count = retained_count.saturating_add(1);
                retained_bytes = retained_bytes.saturating_add(existing_content.len());
            }
        }

        let canonical_root = std::fs::canonicalize(disk_dir).map_err(|err| AuthoringError::Io {
            path: disk_dir.display().to_string(),
            reason: err.to_string(),
        })?;

        let mut staged = StagedMount {
            prefix_with_slash,
            files: BTreeMap::new(),
            total_count: retained_count,
            next_total_bytes: retained_bytes,
            include_intrinsics: self.include_intrinsics,
        };

        #[cfg(unix)]
        {
            let root_fd = openat(
                CWD,
                &canonical_root,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map(File::from)
            .map_err(|err| AuthoringError::Io {
                path: canonical_root.display().to_string(),
                reason: err.to_string(),
            })?;
            walk_unix_dir(&root_fd, &canonical_root, &root_virtual, 0, &mut staged)?;
        }

        #[cfg(not(unix))]
        {
            walk_non_unix_root(&canonical_root, &root_virtual, &mut staged)?;
        }

        Ok(staged)
    }

    fn validate_mount_root(
        &self,
        root_virtual: &str,
        prefix_with_slash: &str,
        allow_effect_root: bool,
    ) -> Result<(), AuthoringError> {
        if self.include_intrinsics && prefixes_overlap(prefix_with_slash, INTRINSICS_VIRTUAL_PREFIX)
        {
            return Err(AuthoringError::InvalidPath {
                path: root_virtual.to_owned(),
                reason: "reserved for pinned @norn/authoring declarations".to_owned(),
            });
        }
        if !allow_effect_root && prefixes_overlap(prefix_with_slash, PINNED_EFFECT_VIRTUAL_PREFIX) {
            return Err(AuthoringError::InvalidPath {
                path: root_virtual.to_owned(),
                reason: "use mount_pinned_effect_dir to mount the version-pinned effect package"
                    .to_owned(),
            });
        }
        for reserved in &self.reserved_prefixes {
            if reserved != prefix_with_slash && prefixes_overlap(prefix_with_slash, reserved) {
                return Err(AuthoringError::InvalidPath {
                    path: root_virtual.to_owned(),
                    reason: format!("overlaps existing reserved pinned namespace `{reserved}`"),
                });
            }
        }
        if self
            .files
            .keys()
            .any(|existing| path_under_prefix(existing, prefix_with_slash))
        {
            return Err(AuthoringError::InvalidPath {
                path: root_virtual.to_owned(),
                reason: "cannot mount pinned package over existing caller files".to_owned(),
            });
        }
        Ok(())
    }

    fn commit_staged_mount(&mut self, staged: StagedMount) {
        self.pinned_files
            .retain(|path, _| !path_under_prefix(path, &staged.prefix_with_slash));
        self.reserved_prefixes.insert(staged.prefix_with_slash);
        self.pinned_total_bytes = staged.next_total_bytes;
        for (path, content) in staged.files {
            self.pinned_files.insert(path, content);
        }
        self.rebuild_directories();
    }
}

fn stage_file_content(
    next_virtual: &str,
    content: String,
    staged: &mut StagedMount,
) -> Result<(), AuthoringError> {
    let normalized = normalize_virtual_path(next_virtual)?;
    if staged.include_intrinsics && path_under_prefix(&normalized, INTRINSICS_VIRTUAL_PREFIX) {
        return Err(AuthoringError::InvalidPath {
            path: normalized,
            reason: "reserved for pinned @norn/authoring declarations".to_owned(),
        });
    }
    let existing_len = staged.files.get(&normalized).map_or(0, String::len);
    if !staged.files.contains_key(&normalized) {
        let next_count = staged.total_count.saturating_add(1);
        staged.total_count = next_count;
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

fn read_opened_regular_file_bounded(
    opened: File,
    display_path: &Path,
    max_bytes: usize,
) -> Result<String, AuthoringError> {
    let meta = opened.metadata().map_err(|err| AuthoringError::Io {
        path: display_path.display().to_string(),
        reason: err.to_string(),
    })?;
    if !meta.file_type().is_file() {
        return Err(AuthoringError::Io {
            path: display_path.display().to_string(),
            reason: "expected regular file inside pinned package".to_owned(),
        });
    }
    let max_u64 = u64::try_from(max_bytes).unwrap_or(u64::MAX);
    if meta.len() > max_u64 {
        return Err(AuthoringError::VfsQuotaExceeded(format!(
            "pinned file `{}` is {} bytes, exceeding pinned cap ({max_bytes})",
            display_path.display(),
            meta.len()
        )));
    }
    let limit = max_u64.saturating_add(1);
    let mut bytes = Vec::new();
    opened
        .take(limit)
        .read_to_end(&mut bytes)
        .map_err(|err| AuthoringError::Io {
            path: display_path.display().to_string(),
            reason: err.to_string(),
        })?;
    if bytes.len() > max_bytes {
        return Err(AuthoringError::VfsQuotaExceeded(format!(
            "pinned file `{}` exceeded pinned cap ({max_bytes} bytes) while reading",
            display_path.display()
        )));
    }
    String::from_utf8(bytes).map_err(|err| AuthoringError::Io {
        path: display_path.display().to_string(),
        reason: format!("invalid UTF-8: {err}"),
    })
}

#[cfg(unix)]
fn walk_unix_dir(
    dir_fd: &File,
    dir_display: &Path,
    virtual_prefix: &str,
    depth: usize,
    staged: &mut StagedMount,
) -> Result<(), AuthoringError> {
    if depth > MAX_MOUNT_DEPTH {
        return Err(AuthoringError::VfsQuotaExceeded(format!(
            "pinned package directory nesting at `{}` exceeds {MAX_MOUNT_DEPTH}",
            dir_display.display()
        )));
    }
    let mut entries = Vec::new();
    let dir_iter = Dir::read_from(dir_fd).map_err(|err| AuthoringError::Io {
        path: dir_display.display().to_string(),
        reason: err.to_string(),
    })?;
    for entry_res in dir_iter {
        let entry = entry_res.map_err(|err| AuthoringError::Io {
            path: dir_display.display().to_string(),
            reason: err.to_string(),
        })?;
        let name = entry.file_name().to_owned();
        if name.as_bytes() == b"." || name.as_bytes() == b".." {
            continue;
        }
        entries.push(name);
    }
    entries.sort();

    let open_flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    for entry in entries {
        let Ok(name) = entry.to_str() else {
            continue;
        };
        if name == "node_modules" {
            continue;
        }
        let child_display = dir_display.join(name);
        let opened = match openat(dir_fd, entry.as_c_str(), open_flags, Mode::empty()) {
            Ok(fd) => File::from(fd),
            Err(err) => {
                if err == Errno::LOOP || err == Errno::NXIO || err == Errno::OPNOTSUPP {
                    continue;
                }
                return Err(AuthoringError::Io {
                    path: child_display.display().to_string(),
                    reason: err.to_string(),
                });
            }
        };
        let file_type = opened
            .metadata()
            .map_err(|err| AuthoringError::Io {
                path: child_display.display().to_string(),
                reason: err.to_string(),
            })?
            .file_type();
        let next_virtual = format!("{virtual_prefix}/{name}");
        if file_type.is_dir() {
            walk_unix_dir(
                &opened,
                &child_display,
                &next_virtual,
                depth.saturating_add(1),
                staged,
            )?;
        } else if file_type.is_file() && is_declaration_or_manifest(name) {
            let content =
                read_opened_regular_file_bounded(opened, &child_display, MAX_PINNED_FILE_BYTES)?;
            stage_file_content(&next_virtual, content, staged)?;
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn walk_non_unix_root(
    canonical_root: &Path,
    root_virtual: &str,
    staged: &mut StagedMount,
) -> Result<(), AuthoringError> {
    let root_meta =
        std::fs::symlink_metadata(canonical_root).map_err(|err| AuthoringError::Io {
            path: canonical_root.display().to_string(),
            reason: err.to_string(),
        })?;
    if !root_meta.file_type().is_dir() {
        return Err(AuthoringError::Io {
            path: canonical_root.display().to_string(),
            reason: "pinned package root is not a directory".to_owned(),
        });
    }
    let mut stack: Vec<(PathBuf, String, usize)> =
        vec![(canonical_root.to_path_buf(), root_virtual.to_owned(), 0)];
    while let Some((dir_path, virtual_prefix, depth)) = stack.pop() {
        if depth > MAX_MOUNT_DEPTH {
            return Err(AuthoringError::VfsQuotaExceeded(format!(
                "pinned package directory nesting at `{}` exceeds {MAX_MOUNT_DEPTH}",
                dir_path.display()
            )));
        }
        let mut entries = Vec::new();
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
            if file_type.is_dir() && name != "node_modules" {
                entries.push((name, entry.path(), true));
            } else if file_type.is_file() && is_declaration_or_manifest(&name) {
                entries.push((name, entry.path(), false));
            }
        }
        entries.sort_by(|left, right| left.0.cmp(&right.0));
        for (name, full_path, is_dir) in entries {
            let next_virtual = format!("{virtual_prefix}/{name}");
            if is_dir {
                stack.push((full_path, next_virtual, depth.saturating_add(1)));
            } else {
                let lstat =
                    std::fs::symlink_metadata(&full_path).map_err(|err| AuthoringError::Io {
                        path: full_path.display().to_string(),
                        reason: err.to_string(),
                    })?;
                if !lstat.file_type().is_file() {
                    continue;
                }
                let opened = OpenOptions::new()
                    .read(true)
                    .open(&full_path)
                    .map_err(|err| AuthoringError::Io {
                        path: full_path.display().to_string(),
                        reason: err.to_string(),
                    })?;
                let content =
                    read_opened_regular_file_bounded(opened, &full_path, MAX_PINNED_FILE_BYTES)?;
                stage_file_content(&next_virtual, content, staged)?;
            }
        }
    }
    Ok(())
}

fn is_declaration_or_manifest(name: &str) -> bool {
    name == "package.json"
        || name.ends_with(".d.ts")
        || name.ends_with(".d.mts")
        || name.ends_with(".d.cts")
}
