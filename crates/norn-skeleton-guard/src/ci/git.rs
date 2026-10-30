//! The git operations the gates need, with fixed arguments.

use std::path::Path;
use std::process::Command;

/// Tracked files of a checkout.
///
/// # Errors
/// Returns a message when git fails.
#[expect(
    clippy::disallowed_methods,
    reason = "the CI gate runner's job is to run git with fixed arguments"
)]
pub fn tracked(root: &Path) -> Result<Vec<String>, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["ls-files", "-z"])
        .output()
        .map_err(|e| format!("git ls-files: {e}"))?;
    if !out.status.success() {
        return Err(format!("git ls-files failed in {}", root.display()));
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .split('\0')
        .filter(|p| !p.is_empty())
        .map(str::to_owned)
        .collect())
}

/// The commit a checkout has at HEAD.
///
/// # Errors
/// Returns a message when git fails.
#[expect(
    clippy::disallowed_methods,
    reason = "the CI gate runner's job is to run git with fixed arguments"
)]
pub fn head(root: &Path) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--verify", "HEAD^{commit}"])
        .output()
        .map_err(|e| format!("git rev-parse: {e}"))?;
    if !out.status.success() {
        return Err(format!("git rev-parse failed in {}", root.display()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// The parents of `commit`, in order.
///
/// # Errors
/// Returns a message when git fails.
#[expect(
    clippy::disallowed_methods,
    reason = "the CI gate runner's job is to run git with fixed arguments"
)]
pub fn parents(root: &Path, commit: &str) -> Result<Vec<String>, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-list", "--parents", "-n", "1", commit, "--"])
        .output()
        .map_err(|e| format!("git rev-list: {e}"))?;
    if !out.status.success() {
        return Err(format!("git rev-list failed for {commit}"));
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .skip(1)
        .map(str::to_owned)
        .collect())
}

/// Whether `paths` have identical contents (including presence) at two commits.
///
/// # Errors
/// Returns a message when git fails.
#[expect(
    clippy::disallowed_methods,
    reason = "the CI gate runner's job is to run git with fixed arguments"
)]
pub fn same_paths(root: &Path, a: &str, b: &str, paths: &[&str]) -> Result<bool, String> {
    let status = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["diff", "--quiet", "--no-ext-diff", "--no-textconv", a, b, "--"])
        .args(paths)
        .status()
        .map_err(|e| format!("git diff: {e}"))?;
    match status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(format!("git diff failed between {a} and {b}")),
    }
}
