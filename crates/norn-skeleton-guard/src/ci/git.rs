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
