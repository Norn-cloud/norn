//! BUILTIN-PIN-1: the registry's pin is this crate's HOST-TREE-1 digest. Any
//! edit to the crate's sources fails here until the pin is reviewed and
//! updated (`norn source-digest crates/norn-builtins`).
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]

use std::path::Path;

fn walk(root: &Path, dir: &Path, files: &mut Vec<(String, Vec<u8>)>) {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(Result::unwrap)
        .collect();
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let name = entry.file_name().into_string().expect("UTF-8 name");
        let kind = entry.file_type().unwrap();
        if name.starts_with('.') || (kind.is_dir() && name == "target") {
            continue;
        }
        assert!(!kind.is_symlink(), "symlinks are refused");
        let path = entry.path();
        if kind.is_dir() {
            walk(root, &path, files);
        } else {
            // HOST-TREE-1 hashes regular files only; reading a FIFO could block.
            assert!(kind.is_file(), "{}: not a regular file", path.display());
            let relative = path
                .strip_prefix(root)
                .unwrap()
                .to_str()
                .unwrap()
                .replace('\\', "/");
            files.push((relative, std::fs::read(&path).unwrap()));
        }
    }
}

#[test]
fn the_pinned_source_tree_is_this_crate() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    walk(root, root, &mut files);
    let actual = norn_ir::source_tree::source_tree_digest(
        files
            .iter()
            .map(|(path, bytes)| (path.as_str(), bytes.as_slice())),
    )
    .unwrap();
    assert_eq!(
        norn_builtins::source_tree_pin().unwrap(),
        actual,
        "crates/norn-builtins changed: review it, then pin {} in registry/builtins/norn-builtins.json",
        actual.0
    );
}

#[test]
fn only_the_compiled_in_table_registers_built_ins() {
    let manifest = norn_builtins::resolve("norn.decision", 1)
        .unwrap()
        .expect("registered");
    assert_eq!(
        manifest.implementation.format,
        norn_ir::function::ImplementationFormat::Host
    );
    let pin = norn_builtins::source_tree_pin().unwrap();
    assert_eq!(manifest.implementation.digest, pin);
    assert_eq!(manifest.runtime.id, "norn-builtins");
    assert_eq!(
        manifest.implementation.locator.as_deref(),
        Some(format!("norn-builtins/0.1.0@{}", pin.0).as_str())
    );
    assert!(manifest.content_digest().unwrap().is_well_formed());
    for (id, version) in [
        ("norn.decision", 2),
        ("norn.other", 1),
        ("acme.norn.decision", 1),
    ] {
        assert!(
            norn_builtins::resolve(id, version).unwrap().is_none(),
            "{id}@{version}"
        );
    }
}

/// Sol S3 (#1173): the walk refuses an entry that is not a regular file (here
/// a Unix socket; a FIFO would block a read) instead of reading it.
#[cfg(unix)]
#[test]
fn an_entry_that_is_not_a_regular_file_is_refused() {
    let copy = Path::new(env!("CARGO_TARGET_TMPDIR")).join("norn-builtins-pin-kind");
    let _fresh = std::fs::remove_dir_all(&copy);
    std::fs::create_dir_all(copy.join("src")).unwrap();
    std::fs::write(copy.join("src/lib.rs"), b"// a crate\n").unwrap();
    let _listener = std::os::unix::net::UnixListener::bind(copy.join("src/listener.sock")).unwrap();
    let refused = std::panic::catch_unwind(|| walk(&copy, &copy, &mut Vec::new())).unwrap_err();
    let message = refused
        .downcast_ref::<String>()
        .cloned()
        .unwrap_or_default();
    assert!(message.contains("not a regular file"), "{message}");
    std::fs::remove_dir_all(&copy).unwrap();
}
