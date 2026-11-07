//! BUILTIN-PIN-1: the registry's pin is this crate's HOST-TREE-1 digest.
//! `build.rs` refuses to build a registry from a stale pin; these tests check
//! the same walk and check, and that an edit without a new pin is refused
//! (`norn source-digest crates/norn-builtins` prints the digest to pin).
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]

use std::path::Path;

#[path = "../pin_tree.rs"]
mod pin_tree;

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn the_pinned_source_tree_is_this_crate() {
    let actual = pin_tree::check(root(), norn_builtins::PIN).unwrap();
    assert_eq!(norn_builtins::source_tree_pin().unwrap().0, actual);
    assert_eq!(norn_builtins::BUILT_SOURCE_TREE, actual);
}

#[test]
fn an_edit_without_a_new_pin_is_refused() {
    // Sol S1 on #1046: the check that gates the build refuses a copy of this
    // crate whose implementation changed while the pin stayed.
    let copy = Path::new(env!("CARGO_TARGET_TMPDIR")).join("norn-builtins-pin-drift");
    let _fresh = std::fs::remove_dir_all(&copy);
    for (path, bytes) in pin_tree::files(root()).unwrap() {
        let target = copy.join(&path);
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(&target, bytes).unwrap();
    }
    assert!(pin_tree::check(&copy, norn_builtins::PIN).is_ok());
    let source = copy.join("src/decision.rs");
    let mut edited = std::fs::read(&source).unwrap();
    edited.extend_from_slice(b"// an unreviewed edit\n");
    std::fs::write(&source, edited).unwrap();
    let refused = pin_tree::check(&copy, norn_builtins::PIN).unwrap_err();
    assert!(refused.starts_with("builtin.pin_mismatch"), "{refused}");
    let repinned = pin_tree::check(&copy, "{}").unwrap_err();
    assert!(repinned.contains("names no sourceTree"), "{repinned}");
    // The message names the digest to pin after review.
    let files = pin_tree::files(&copy).unwrap();
    let digest = norn_ir::source_tree::source_tree_digest(
        files
            .iter()
            .map(|(path, bytes)| (path.as_str(), bytes.as_slice())),
    )
    .unwrap();
    assert!(refused.contains(&digest.0), "{refused}");
    std::fs::remove_dir_all(&copy).unwrap();
}

#[test]
fn only_the_compiled_in_table_registers_built_ins() {
    let registered = norn_builtins::resolve("norn.decision", 1)
        .unwrap()
        .expect("registered");
    let manifest = registered.manifest();
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
    assert_eq!(registered.digest(), &manifest.content_digest().unwrap());
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
