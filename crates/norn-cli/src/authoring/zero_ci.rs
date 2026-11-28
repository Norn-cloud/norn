//! Fail-closed downstream Norn app zero-CI enforcement and `norn-checker-abi/check@1` reporting.
//!
//! Checks `.github` and `.github/workflows` relative to an already-pinned app root
//! [`TrustedDir`] using `openat(..., O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)` only when
//! an enclosing regular `norn.json` exists (the Norn monorepo root has no `norn.json`,
//! so no spoofable directory-name heuristic is needed).

use std::path::Path;

use serde_json::{Value, json};

use super::fs_io::ZeroCiStatus;
#[cfg(unix)]
use super::fs_io::{final_dir_flags, format_dir_open_error};
use super::trusted_dir::TrustedDir;

/// Diagnostic code emitted when a downstream Norn app contains `.github/workflows`.
pub(crate) const ZERO_CI_DIAGNOSTIC_CODE: &str = "NORN-APP-ZERO-CI";

/// Maximum number of parent directory steps climbed when searching for `norn.json`.
pub(crate) const MAX_ANCESTOR_DEPTH: usize = 64;

/// Result of locating the enclosing downstream Norn app root from a pinned directory handle.
#[derive(Debug)]
pub(crate) enum AppRootMatch {
    /// The starting pinned directory itself contains a regular `norn.json`.
    SameAsStart,
    /// A pinned ancestor directory contains a regular `norn.json`.
    Ancestor(TrustedDir),
}

impl AppRootMatch {
    /// Borrows the pinned app root directory handle.
    pub(crate) fn as_dir<'a>(&'a self, start_dir: &'a TrustedDir) -> &'a TrustedDir {
        match self {
            Self::SameAsStart => start_dir,
            Self::Ancestor(dir) => dir,
        }
    }
}

/// Result of enforcing zero-CI policy for `norn check --source`.
pub(crate) enum ZeroCiOutcome {
    /// Target is not a downstream app or `.github/workflows` is absent (`ENOENT`).
    Clean,
    /// Downstream app defines `.github/workflows`; contains a `norn-checker-abi/check@1` report.
    Refused(String),
    /// Fail-closed inspection error (`ELOOP`, `ENOTDIR`, `EACCES`, or other OS error).
    IoError(String),
}

/// Builds an exact `norn-checker-abi/check@1` refusal document matching `check_artifact_set_json`.
pub(crate) fn build_zero_ci_report(rel_path: &str) -> String {
    let version_doc: Value =
        serde_json::from_str(&norn_checker_wasm::version_json()).unwrap_or(Value::Null);
    let diagnostics_format = version_doc
        .get("diagnosticsFormat")
        .and_then(Value::as_str)
        .unwrap_or("norn-checker/diagnostics@1");
    let checker_version = version_doc
        .get("checkerVersion")
        .and_then(Value::as_str)
        .unwrap_or("0.2.0");
    let pointer = format!("/{}", rel_path.trim_start_matches('/'));
    json!({
        "format": norn_checker_wasm::CHECK_FORMAT,
        "admitted": false,
        "diagnostics": {
            "format": diagnostics_format,
            "checkerVersion": checker_version,
            "diagnostics": [{
                "code": ZERO_CI_DIAGNOSTIC_CODE,
                "tier": Value::Null,
                "severity": "error",
                "message": "Downstream Norn apps must not define `.github/workflows/` CI pipelines; Norn apps compile, format, and verify locally and in-browser via native Rust `norn` with zero CI.",
                "span": {
                    "pointer": pointer,
                    "source": Value::Null,
                },
                "artifactId": Value::Null,
                "remediation": Value::Null,
            }],
        },
        "receipt": Value::Null,
        "receiptSha256": Value::Null,
    })
    .to_string()
}

/// Enforces zero-CI policy against an already-pinned downstream Norn app root [`TrustedDir`].
pub(crate) fn check_zero_ci_app_root(app_root: &TrustedDir) -> ZeroCiOutcome {
    match inspect_workflows_zero_ci(app_root) {
        ZeroCiStatus::Absent => ZeroCiOutcome::Clean,
        ZeroCiStatus::Present(rel_path) => ZeroCiOutcome::Refused(build_zero_ci_report(&rel_path)),
        ZeroCiStatus::Error(err) => ZeroCiOutcome::IoError(err),
    }
}

fn inspect_workflows_zero_ci(app_root: &TrustedDir) -> ZeroCiStatus {
    #[cfg(unix)]
    {
        inspect_workflows_zero_ci_unix(app_root.as_fd(), app_root.display_path())
    }
    #[cfg(not(unix))]
    {
        inspect_workflows_zero_ci_non_unix(app_root.display_path())
    }
}

#[cfg(unix)]
fn inspect_workflows_zero_ci_unix(
    root_fd: std::os::fd::BorrowedFd<'_>,
    root_display: &Path,
) -> ZeroCiStatus {
    use rustix::fs::{Mode, openat};
    use rustix::io::Errno;
    use std::os::fd::AsFd as _;

    let github_display = root_display.join(".github");
    let github_fd = match openat(root_fd, ".github", final_dir_flags(), Mode::empty()) {
        Ok(fd) => fd,
        Err(err) if err == Errno::NOENT => return ZeroCiStatus::Absent,
        Err(err) => {
            return ZeroCiStatus::Error(format_dir_open_error(
                root_fd,
                std::ffi::OsStr::new(".github"),
                &github_display,
                err,
            ));
        }
    };

    let workflows_display = github_display.join("workflows");
    match openat(
        github_fd.as_fd(),
        "workflows",
        final_dir_flags(),
        Mode::empty(),
    ) {
        Ok(_workflows_fd) => ZeroCiStatus::Present(".github/workflows".to_owned()),
        Err(err) if err == Errno::NOENT => ZeroCiStatus::Absent,
        Err(err) => ZeroCiStatus::Error(format_dir_open_error(
            github_fd.as_fd(),
            std::ffi::OsStr::new("workflows"),
            &workflows_display,
            err,
        )),
    }
}

#[cfg(not(unix))]
fn inspect_workflows_zero_ci_non_unix(root: &Path) -> ZeroCiStatus {
    let github = root.join(".github");
    match std::fs::symlink_metadata(&github) {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return ZeroCiStatus::Absent,
        Err(err) => return ZeroCiStatus::Error(format!("{}: {err}", github.display())),
        Ok(meta) => {
            if meta.file_type().is_symlink() {
                return ZeroCiStatus::Error(format!("{}: symlinks are refused", github.display()));
            }
            if !meta.is_dir() {
                return ZeroCiStatus::Error(format!("{}: not a directory", github.display()));
            }
        }
    }
    let workflows = github.join("workflows");
    match std::fs::symlink_metadata(&workflows) {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => ZeroCiStatus::Absent,
        Err(err) => ZeroCiStatus::Error(format!("{}: {err}", workflows.display())),
        Ok(meta) => {
            if meta.file_type().is_symlink() {
                ZeroCiStatus::Error(format!("{}: symlinks are refused", workflows.display()))
            } else if !meta.is_dir() {
                ZeroCiStatus::Error(format!("{}: not a directory", workflows.display()))
            } else {
                ZeroCiStatus::Present(".github/workflows".to_owned())
            }
        }
    }
}

#[cfg(unix)]
fn check_norn_json_in_dir_unix(
    dir_fd: std::os::fd::BorrowedFd<'_>,
    dir_display: &Path,
) -> Result<bool, String> {
    use rustix::fs::{AtFlags, FileType, statat};
    use rustix::io::Errno;

    let candidate = dir_display.join("norn.json");
    match statat(dir_fd, "norn.json", AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) => match FileType::from_raw_mode(stat.st_mode) {
            FileType::RegularFile => Ok(true),
            FileType::Symlink => Err(format!("{}: symlinks are refused", candidate.display())),
            FileType::Directory
            | FileType::Fifo
            | FileType::Socket
            | FileType::CharacterDevice
            | FileType::BlockDevice
            | FileType::Unknown => Err(format!(
                "{}: norn.json is not a regular file",
                candidate.display()
            )),
        },
        Err(err) if err == Errno::NOENT => Ok(false),
        Err(err) => Err(format!("{}: {err}", candidate.display())),
    }
}

/// Climbs from an already-pinned [`TrustedDir`] using FD-relative `..` opens to locate
/// the nearest enclosing downstream Norn app directory containing a regular `norn.json`.
///
/// When `boundary_root` is `Some(root)`, traversal never inspects any directory outside `root`.
pub(crate) fn find_norn_app_root_from_dir(
    start_dir: &TrustedDir,
    boundary_root: Option<&Path>,
) -> Result<Option<AppRootMatch>, String> {
    find_norn_app_root_from_dir_with_limit(start_dir, MAX_ANCESTOR_DEPTH, boundary_root)
}

#[cfg(unix)]
fn resolve_boundary_stat(
    start_dir: &TrustedDir,
    boundary_root: &Path,
) -> Result<rustix::fs::Stat, String> {
    use rustix::fs::fstat;

    let canonical_bound = std::fs::canonicalize(boundary_root)
        .map_err(|err| format!("{}: {err}", boundary_root.display()))?;
    let canonical_start = std::fs::canonicalize(start_dir.display_path())
        .map_err(|err| format!("{}: {err}", start_dir.display_path().display()))?;
    if !canonical_start.starts_with(&canonical_bound) {
        return Err(format!(
            "{}: path is outside workspace boundary {}",
            start_dir.display_path().display(),
            canonical_bound.display()
        ));
    }
    let boundary_dir = TrustedDir::open(&canonical_bound)?;
    fstat(boundary_dir.as_fd()).map_err(|err| format!("{}: {err}", canonical_bound.display()))
}

/// Bounded ancestor `norn.json` discovery failing closed on every OS error, non-regular
/// `norn.json`, or depth-limit exhaustion (returns `Ok(None)` only on reaching filesystem root
/// or `boundary_root`).
#[cfg(unix)]
pub(crate) fn find_norn_app_root_from_dir_with_limit(
    start_dir: &TrustedDir,
    max_depth: usize,
    boundary_root: Option<&Path>,
) -> Result<Option<AppRootMatch>, String> {
    use rustix::fs::{Mode, fstat, openat};
    use std::os::fd::AsFd as _;

    let boundary_stat = boundary_root
        .map(|root| resolve_boundary_stat(start_dir, root))
        .transpose()?;

    if check_norn_json_in_dir_unix(start_dir.as_fd(), start_dir.display_path())? {
        return Ok(Some(AppRootMatch::SameAsStart));
    }

    let mut ancestor: Option<TrustedDir> = None;
    for _step in 0..max_depth {
        let current = ancestor.as_ref().unwrap_or(start_dir);
        let cur_stat = fstat(current.as_fd())
            .map_err(|err| format!("{}: {err}", current.display_path().display()))?;
        if let Some(bound) = boundary_stat.as_ref()
            && cur_stat.st_dev == bound.st_dev
            && cur_stat.st_ino == bound.st_ino
        {
            return Ok(None);
        }
        let parent_fd = openat(current.as_fd(), "..", final_dir_flags(), Mode::empty())
            .map_err(|err| format!("{}: {err}", current.display_path().display()))?;
        let parent_stat = fstat(parent_fd.as_fd())
            .map_err(|err| format!("{}: {err}", current.display_path().display()))?;
        if parent_stat.st_dev == cur_stat.st_dev && parent_stat.st_ino == cur_stat.st_ino {
            return Ok(None);
        }
        let next_display = current
            .display_path()
            .parent()
            .filter(|dir| !dir.as_os_str().is_empty())
            .map_or_else(|| current.display_path().join(".."), Path::to_path_buf);
        let next_dir = TrustedDir::from_fd(parent_fd, next_display);
        if check_norn_json_in_dir_unix(next_dir.as_fd(), next_dir.display_path())? {
            return Ok(Some(AppRootMatch::Ancestor(next_dir)));
        }
        ancestor = Some(next_dir);
    }

    Err(format!(
        "{}: ancestor norn.json search exceeded {max_depth} levels",
        start_dir.display_path().display()
    ))
}

/// Bounded ancestor `norn.json` discovery (non-Unix fallback).
#[cfg(not(unix))]
pub(crate) fn find_norn_app_root_from_dir_with_limit(
    start_dir: &TrustedDir,
    max_depth: usize,
    boundary_root: Option<&Path>,
) -> Result<Option<AppRootMatch>, String> {
    let start_path = start_dir.display_path();
    let resolved = if start_path.is_absolute() {
        start_path.to_path_buf()
    } else {
        let cwd =
            std::env::current_dir().map_err(|err| format!("{}: {err}", start_path.display()))?;
        cwd.join(start_path)
    };
    let canonical_bound = boundary_root
        .map(|root| std::fs::canonicalize(root).map_err(|err| format!("{}: {err}", root.display())))
        .transpose()?;
    let search_start = if let Some(bound) = canonical_bound.as_deref() {
        let canonical_start = std::fs::canonicalize(&resolved)
            .map_err(|err| format!("{}: {err}", start_path.display()))?;
        if !canonical_start.starts_with(bound) {
            return Err(format!(
                "{}: path is outside workspace boundary {}",
                start_path.display(),
                bound.display()
            ));
        }
        canonical_start
    } else {
        resolved
    };
    for (step, ancestor) in search_start.ancestors().enumerate() {
        if let Some(bound) = canonical_bound.as_deref()
            && !ancestor.starts_with(bound)
        {
            return Ok(None);
        }
        if step > max_depth {
            return Err(format!(
                "{}: ancestor norn.json search exceeded {max_depth} levels",
                start_path.display()
            ));
        }
        let candidate = ancestor.join("norn.json");
        match std::fs::symlink_metadata(&candidate) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(format!("{}: symlinks are refused", candidate.display()));
            }
            Ok(meta) if meta.is_file() => {
                if step == 0 {
                    return Ok(Some(AppRootMatch::SameAsStart));
                }
                return Ok(Some(AppRootMatch::Ancestor(TrustedDir::open(ancestor)?)));
            }
            Ok(_) => {
                return Err(format!(
                    "{}: norn.json is not a regular file",
                    candidate.display()
                ));
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(format!("{}: {err}", candidate.display())),
        }
    }
    Ok(None)
}
