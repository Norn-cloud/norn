//! Fail-closed downstream Norn app zero-CI enforcement and `norn-checker-abi/check@1` reporting.
//!
//! Checks `.github` and `.github/workflows` relative to the app root's `OwnedFd` using
//! `openat(..., O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)` only when an enclosing regular
//! `norn.json` exists (the Norn monorepo root has no `norn.json`, so no spoofable
//! directory-name heuristic is needed).

use std::path::Path;

use serde_json::{Value, json};

#[cfg(unix)]
use super::fs_io::final_dir_flags;
use super::fs_io::{EntryKind, ZeroCiStatus, inspect_path_no_follow};
use super::trusted_dir::TrustedDir;

/// Diagnostic code emitted when a downstream Norn app contains `.github/workflows`.
pub(crate) const ZERO_CI_DIAGNOSTIC_CODE: &str = "NORN-APP-ZERO-CI";

/// Result of enforcing zero-CI policy for `norn check --source`.
pub(crate) enum ZeroCiOutcome {
    /// Target is not a downstream app or `.github/workflows` is absent (`ENOENT`).
    Clean,
    /// Downstream app defines `.github/workflows`; contains a `norn-checker-abi/check@1` report.
    Refused(String),
    /// Fail-closed inspection error (`ELOOP`, `EACCES`, or other OS error).
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

/// Checks whether `path` belongs to a downstream Norn app (`norn.json`) with `.github/workflows`.
pub(crate) fn check_zero_ci_app_dir(path: &Path) -> ZeroCiOutcome {
    let app_root = match find_norn_app_root(path) {
        Ok(Some(root)) => root,
        Ok(None) => return ZeroCiOutcome::Clean,
        Err(err) => return ZeroCiOutcome::IoError(err),
    };
    match inspect_workflows_zero_ci(&app_root) {
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
    use rustix::fs::{AtFlags, FileType, Mode, openat, statat};
    use rustix::io::Errno;
    use std::os::fd::AsFd as _;

    let github_display = root_display.join(".github");
    let github_fd = match openat(root_fd, ".github", final_dir_flags(), Mode::empty()) {
        Ok(fd) => fd,
        Err(err) if err == Errno::NOENT => return ZeroCiStatus::Absent,
        Err(err) if err == Errno::LOOP => {
            return ZeroCiStatus::Error(format!(
                "{}: symlinks are refused",
                github_display.display()
            ));
        }
        Err(err) if err == Errno::NOTDIR => {
            return match statat(root_fd, ".github", AtFlags::SYMLINK_NOFOLLOW) {
                Ok(stat) => match FileType::from_raw_mode(stat.st_mode) {
                    FileType::RegularFile => ZeroCiStatus::Absent,
                    FileType::Symlink => ZeroCiStatus::Error(format!(
                        "{}: symlinks are refused",
                        github_display.display()
                    )),
                    FileType::Directory
                    | FileType::Fifo
                    | FileType::Socket
                    | FileType::CharacterDevice
                    | FileType::BlockDevice
                    | FileType::Unknown => ZeroCiStatus::Error(format!(
                        "{}: .github is not a directory",
                        github_display.display()
                    )),
                },
                Err(stat_err) => {
                    ZeroCiStatus::Error(format!("{}: {stat_err}", github_display.display()))
                }
            };
        }
        Err(err) => return ZeroCiStatus::Error(format!("{}: {err}", github_display.display())),
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
        Err(err) if err == Errno::LOOP || err == Errno::NOTDIR => {
            ZeroCiStatus::Present(".github/workflows".to_owned())
        }
        Err(err) => ZeroCiStatus::Error(format!("{}: {err}", workflows_display.display())),
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
                return ZeroCiStatus::Absent;
            }
        }
    }
    let workflows = github.join("workflows");
    match std::fs::symlink_metadata(&workflows) {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => ZeroCiStatus::Absent,
        Err(err) => ZeroCiStatus::Error(format!("{}: {err}", workflows.display())),
        Ok(_) => ZeroCiStatus::Present(".github/workflows".to_owned()),
    }
}

/// Climbs from `path` using FD-relative `..` opens to locate the nearest enclosing
/// downstream Norn app directory containing a regular `norn.json`.
#[cfg(unix)]
fn find_norn_app_root(path: &Path) -> Result<Option<TrustedDir>, String> {
    use rustix::fs::{AtFlags, FileType, Mode, fstat, openat, statat};
    use rustix::io::Errno;
    use std::os::fd::AsFd as _;

    let start_dir = match inspect_path_no_follow(path)? {
        EntryKind::Directory => TrustedDir::open(path)?,
        EntryKind::File => {
            let parent = path
                .parent()
                .filter(|dir| !dir.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."));
            TrustedDir::open(parent)?
        }
    };

    let mut current = start_dir;
    for _step in 0..=64_usize {
        match statat(current.as_fd(), "norn.json", AtFlags::SYMLINK_NOFOLLOW) {
            Ok(stat) => match FileType::from_raw_mode(stat.st_mode) {
                FileType::RegularFile => return Ok(Some(current)),
                FileType::Symlink => {
                    return Err(format!(
                        "{}: symlinks are refused",
                        current.display_path().join("norn.json").display()
                    ));
                }
                FileType::Directory
                | FileType::Fifo
                | FileType::Socket
                | FileType::CharacterDevice
                | FileType::BlockDevice
                | FileType::Unknown => {}
            },
            Err(err) if err == Errno::NOENT => {}
            Err(err) => {
                return Err(format!(
                    "{}: {err}",
                    current.display_path().join("norn.json").display()
                ));
            }
        }
        let cur_stat = fstat(current.as_fd())
            .map_err(|err| format!("{}: {err}", current.display_path().display()))?;
        let parent_fd = match openat(current.as_fd(), "..", final_dir_flags(), Mode::empty()) {
            Ok(fd) => fd,
            Err(err) if err == Errno::ACCES => return Ok(None),
            Err(err) => return Err(format!("{}: {err}", current.display_path().display())),
        };
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
        current = TrustedDir::from_fd(parent_fd, next_display);
    }
    Ok(None)
}

#[cfg(not(unix))]
fn find_norn_app_root(path: &Path) -> Result<Option<TrustedDir>, String> {
    let start_path = match inspect_path_no_follow(path)? {
        EntryKind::Directory => path.to_path_buf(),
        EntryKind::File => path
            .parent()
            .filter(|dir| !dir.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf(),
    };
    let resolved = if start_path.is_absolute() {
        start_path
    } else if let Ok(cwd) = std::env::current_dir() {
        cwd.join(start_path)
    } else {
        start_path
    };
    for ancestor in resolved.ancestors() {
        let candidate = ancestor.join("norn.json");
        match std::fs::symlink_metadata(&candidate) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(format!("{}: symlinks are refused", candidate.display()));
            }
            Ok(meta) if meta.is_file() => return Ok(Some(TrustedDir::open(ancestor)?)),
            Ok(_) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(format!("{}: {err}", candidate.display())),
        }
    }
    Ok(None)
}
