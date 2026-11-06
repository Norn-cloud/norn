//! BUILTIN-PIN-1, shared by `build.rs` and `tests/pin.rs`: walk the crate's
//! source tree and check its HOST-TREE-1 digest against the reviewed pin.

use std::path::Path;

/// The crate's regular files (path relative to `root`, exact bytes), skipping
/// dot-entries and a `target` directory; a symlink is refused.
///
/// # Errors
/// An unreadable entry, a non-UTF-8 name or a symlink.
pub(crate) fn files(root: &Path) -> Result<Vec<(String, Vec<u8>)>, String> {
    let mut files = Vec::new();
    walk(root, root, &mut files)?;
    Ok(files)
}

fn walk(root: &Path, dir: &Path, files: &mut Vec<(String, Vec<u8>)>) -> Result<(), String> {
    let mut entries = std::fs::read_dir(dir)
        .and_then(Iterator::collect::<Result<Vec<_>, _>>)
        .map_err(|error| format!("{}: {error}", dir.display()))?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_name| format!("{}: not a UTF-8 name", path.display()))?;
        let kind = entry
            .file_type()
            .map_err(|error| format!("{}: {error}", path.display()))?;
        if name.starts_with('.') || (kind.is_dir() && name == "target") {
            continue;
        }
        if kind.is_symlink() {
            return Err(format!("{}: symlinks are refused", path.display()));
        }
        if kind.is_dir() {
            walk(root, &path, files)?;
        } else {
            let relative = path
                .strip_prefix(root)
                .ok()
                .and_then(Path::to_str)
                .ok_or_else(|| format!("{}: outside the crate", path.display()))?
                .replace('\\', "/");
            let bytes =
                std::fs::read(&path).map_err(|error| format!("{}: {error}", path.display()))?;
            files.push((relative, bytes));
        }
    }
    Ok(())
}

/// The crate's digest when `pin` (the registry pin file's text) pins it.
///
/// # Errors
/// The walk fails, the pin names no `sourceTree`, or the pin is stale; the
/// message names the digest to pin after review.
pub(crate) fn check(root: &Path, pin: &str) -> Result<String, String> {
    let files = files(root)?;
    let actual = norn_ir::source_tree::source_tree_digest(
        files
            .iter()
            .map(|(path, bytes)| (path.as_str(), bytes.as_slice())),
    )
    .map_err(|error| error.to_string())?
    .0;
    let pinned = serde_json::from_str::<serde_json::Value>(pin)
        .ok()
        .and_then(|pin| pin.get("sourceTree")?.as_str().map(str::to_owned))
        .ok_or("builtin.pin_mismatch: the pin names no sourceTree")?;
    if pinned == actual {
        Ok(actual)
    } else {
        Err(format!(
            "builtin.pin_mismatch: crates/norn-builtins changed (pinned {pinned}); review it, then pin {actual} in registry/builtins/norn-builtins.json"
        ))
    }
}
