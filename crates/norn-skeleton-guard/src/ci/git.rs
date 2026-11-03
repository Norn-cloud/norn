//! The git operations the gates need, with fixed arguments.

use sha2::{Digest, Sha256};
use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

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
        .arg("--no-replace-objects")
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
        .arg("--no-replace-objects")
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

/// Return the immutable tree object named by a full commit SHA.
///
/// # Errors
/// Refuses non-commit inputs and malformed Git responses.
#[expect(
    clippy::disallowed_methods,
    reason = "fixed Git plumbing over immutable objects"
)]
pub fn tree_oid(root: &Path, commit: &str) -> Result<String, String> {
    if commit.len() != 40
        || !commit
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err("rights target requires a full lowercase commit SHA".into());
    }
    let out = Command::new("git")
        .arg("--no-replace-objects")
        .arg("-C")
        .arg(root)
        .args(["show", "--no-patch", "--format=%T", commit, "--"])
        .output()
        .map_err(|e| e.to_string())?;
    let tree = String::from_utf8(out.stdout)
        .map_err(|e| e.to_string())?
        .trim()
        .to_owned();
    if !out.status.success()
        || tree.len() != 40
        || !tree
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err("unable to resolve immutable rights tree".into());
    }
    Ok(tree)
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
        .arg("--no-replace-objects")
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
        .arg("--no-replace-objects")
        .arg("-C")
        .arg(root)
        .args([
            "diff",
            "--quiet",
            "--no-ext-diff",
            "--no-textconv",
            a,
            b,
            "--",
        ])
        .args(paths)
        .status()
        .map_err(|e| format!("git diff: {e}"))?;
    match status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        Some(_) | None => Err(format!("git diff failed between {a} and {b}")),
    }
}

/// An immutable tree entry, including executable mode and observed content digest.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct TreeEntry {
    /// Git mode, restricted to regular files.
    pub mode: String,
    /// SHA256 computed from the blob, never from a provenance declaration.
    pub sha256: String,
    /// Bytes read from the immutable blob; excluded from the inventory encoding.
    #[serde(skip)]
    pub bytes: Vec<u8>,
}

/// Enumerate an exact commit's blobs without consulting the writable index.
///
/// # Errors
/// Rejects symbolic revisions, non-regular members, invalid paths and Git failures.
#[expect(
    clippy::disallowed_methods,
    reason = "fixed Git plumbing over immutable objects"
)]
pub fn tree(
    root: &Path,
    commit: &str,
) -> Result<std::collections::BTreeMap<String, TreeEntry>, String> {
    if commit.len() != 40
        || !commit
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err("binding requires a full lowercase commit SHA".into());
    }
    let kind = Command::new("git")
        .arg("--no-replace-objects")
        .arg("-C")
        .arg(root)
        .args(["cat-file", "-t", commit])
        .output()
        .map_err(|e| e.to_string())?;
    if !kind.status.success() || kind.stdout != b"commit\n" {
        return Err("binding is not an immutable commit".into());
    }
    let out = Command::new("git")
        .arg("--no-replace-objects")
        .arg("-C")
        .arg(root)
        .args([
            "ls-tree",
            "-r",
            "-l",
            "-z",
            "--full-tree",
            &format!("{commit}^{{tree}}"),
        ])
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err("cannot read bound Git tree".into());
    }
    let mut members = Vec::new();
    for entry in out.stdout.split(|b| *b == 0).filter(|e| !e.is_empty()) {
        let entry = std::str::from_utf8(entry).map_err(|e| e.to_string())?;
        let (header, path) = entry.split_once('\t').ok_or("malformed Git tree entry")?;
        let fields: Vec<_> = header.split_whitespace().collect();
        let [mode, "blob", oid, size] = fields.as_slice() else {
            return Err("non-blob tree member".into());
        };
        if !matches!(*mode, "100644" | "100755") || !super::provenance::safe_path(path) {
            return Err(format!("unsafe tree member {path}"));
        }
        if size.parse::<usize>().map_err(|e| e.to_string())? > 32 * 1024 * 1024 {
            return Err(format!("oversized bound blob {path}"));
        }
        members.push(((*mode).to_owned(), path.to_owned(), (*oid).to_owned()));
    }
    let mut child = Command::new("git")
        .arg("--no-replace-objects")
        .arg("-C")
        .arg(root)
        .args(["cat-file", "--batch"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    let mut input = child.stdin.take().ok_or("missing Git input")?;
    let mut ids = String::new();
    for (_, _, oid) in &members {
        ids.push_str(oid);
        ids.push('\n');
    }
    let writer = std::thread::spawn(move || input.write_all(ids.as_bytes()));
    let output = child.wait_with_output().map_err(|e| e.to_string())?;
    writer
        .join()
        .map_err(|_panic| "Git input writer failed")?
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err("cannot read bound blobs".into());
    }
    parse_blobs(&output.stdout, members)
}

fn parse_blobs(
    output: &[u8],
    members: Vec<(String, String, String)>,
) -> Result<std::collections::BTreeMap<String, TreeEntry>, String> {
    let mut remainder = output;
    let mut entries = std::collections::BTreeMap::new();
    for (mode, path, oid) in members {
        let end = remainder
            .iter()
            .position(|b| *b == b'\n')
            .ok_or("missing blob header")?;
        let header = std::str::from_utf8(remainder.get(..end).ok_or("blob header")?)
            .map_err(|e| e.to_string())?;
        let size = header
            .strip_prefix(&format!("{oid} blob "))
            .ok_or("wrong blob identity")?
            .parse::<usize>()
            .map_err(|e| e.to_string())?;
        if size > 32 * 1024 * 1024 {
            return Err(format!("oversized bound blob {path}"));
        }
        remainder = remainder
            .get(end.checked_add(1).ok_or("blob offset overflow")?..)
            .ok_or("missing blob content")?;
        let data = remainder.get(..size).ok_or("truncated blob")?;
        entries.insert(
            path,
            TreeEntry {
                mode,
                sha256: hex::encode(Sha256::digest(data)),
                bytes: data.to_vec(),
            },
        );
        if remainder.get(size) != Some(&b'\n') {
            return Err("bad blob terminator".into());
        }
        remainder = remainder
            .get(size.checked_add(1).ok_or("blob offset overflow")?..)
            .ok_or("truncated blob stream")?;
    }
    if !remainder.is_empty() {
        return Err("extra bound blob data".into());
    }
    if entries.is_empty() {
        return Err("empty bound tree".into());
    }
    Ok(entries)
}

/// Recompute a clean integration with Git's merge machinery, never a candidate driver.
/// Git configuration belongs to the trusted checkout; no candidate commands run.
///
/// # Errors
/// Refuses conflicts and any integration-only changes.
#[expect(
    clippy::disallowed_methods,
    reason = "fixed Git merge-tree with no working-tree or custom merge drivers"
)]
pub fn clean_merge(root: &Path, previous: &str, pull: &str, merged: &str) -> Result<(), String> {
    let common = Command::new("git")
        .arg("--no-replace-objects")
        .arg("-C")
        .arg(root)
        .args(["merge-base", previous, pull])
        .output()
        .map_err(|e| e.to_string())?;
    if !common.status.success() {
        return Err("no constituent merge base".into());
    }
    let common = std::str::from_utf8(&common.stdout)
        .map_err(|e| e.to_string())?
        .trim();
    let ancestor = tree(root, common)?;
    let before = tree(root, previous)?;
    let incoming = tree(root, pull)?;
    for path in ancestor
        .keys()
        .chain(before.keys())
        .chain(incoming.keys())
        .filter(|p| p.starts_with(super::provenance::CONTROL))
    {
        if before.get(path) != ancestor.get(path)
            && incoming.get(path) != ancestor.get(path)
            && before.get(path) != incoming.get(path)
        {
            return Err(format!(
                "metadata disagreement requires renewed review: {path}"
            ));
        }
    }
    let out = Command::new("git")
        .arg("--no-replace-objects")
        .arg("-C")
        .arg(root)
        .args([
            "-c",
            "core.attributesFile=/dev/null",
            "-c",
            "merge.renormalize=false",
            "merge-tree",
            "--write-tree",
            previous,
            pull,
        ])
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err("integration requires renewed rights review: merge conflict".into());
    }
    let actual = Command::new("git")
        .arg("--no-replace-objects")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--verify", &format!("{merged}^{{tree}}")])
        .output()
        .map_err(|e| e.to_string())?;
    if !actual.status.success()
        || out.stdout.split(|b| *b == b'\n').next() != actual.stdout.split(|b| *b == b'\n').next()
    {
        return Err(
            "integration-only content or metadata change requires renewed rights review".into(),
        );
    }
    let admitted = tree(root, merged)?;
    for path in before
        .keys()
        .chain(incoming.keys())
        .filter(|p| !p.starts_with(super::provenance::CONTROL))
    {
        let record = format!("{}artifacts/{path}.json", super::provenance::CONTROL);
        // With differing declarations, retain an already admitted or reviewed
        // pair. Equal declarations allow independent payload hunks to combine.
        if before.get(&record) != incoming.get(&record)
            && ![&before, &incoming].into_iter().any(|side| {
                side.get(path) == admitted.get(path) && side.get(&record) == admitted.get(&record)
            })
        {
            return Err(format!(
                "unreviewed payload/rights combination requires renewed review: {path}"
            ));
        }
    }
    Ok(())
}

/// Verify the writable index against the independently bound commit, never use it as authority.
///
/// # Errors
/// Refuses staged changes, even when working bytes have been restored.
#[expect(
    clippy::disallowed_methods,
    reason = "fixed Git index comparison against immutable commit"
)]
pub fn clean_index(root: &Path, head: &str) -> Result<(), String> {
    let status = Command::new("git")
        .arg("--no-replace-objects")
        .arg("-C")
        .arg(root)
        .args([
            "diff",
            "--cached",
            "--quiet",
            "--no-ext-diff",
            "--no-textconv",
            head,
            "--",
        ])
        .status()
        .map_err(|e| e.to_string())?;
    if !status.success() {
        return Err("index differs from the bound candidate commit".into());
    }
    Ok(())
}
