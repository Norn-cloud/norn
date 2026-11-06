//! HOST-TREE-1: the source-tree digest is order-independent, changes with
//! any file's path or bytes, and refuses ambiguous trees.
#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test assertions"
)]

use norn_ir::source_tree::{SourceTreeError, is_valid_path, source_tree_digest};
use proptest::prelude::*;

fn digest(files: &[(&str, &[u8])]) -> Result<String, SourceTreeError> {
    source_tree_digest(files.iter().copied()).map(|digest| digest.0)
}

const TREE: &[(&str, &[u8])] = &[
    ("Cargo.toml", b"[package]\nname = \"norn-builtins\"\n"),
    ("src/lib.rs", b"//! built-ins\n"),
    ("src/decision.rs", b"pub fn decide() {}\n"),
];

#[test]
fn the_digest_is_pinned_and_well_formed() {
    let pinned = digest(TREE).unwrap();
    assert!(norn_ir::function::Digest(pinned.clone()).is_well_formed());
    // A change to the algorithm (domain, entry shape, order or hashing) moves
    // this vector; update it only with a new SOURCE_TREE_DOMAIN version.
    assert_eq!(pinned, include_str!("fixtures/source-tree.digest").trim());
}

#[test]
fn every_change_to_a_path_or_its_bytes_changes_the_digest() {
    let base = digest(TREE).unwrap();
    let edited: &[(&str, &[u8])] = &[
        TREE[0],
        TREE[1],
        ("src/decision.rs", b"pub fn decide() { }\n"),
    ];
    let renamed: &[(&str, &[u8])] = &[TREE[0], TREE[1], ("src/decide.rs", TREE[2].1)];
    let removed: &[(&str, &[u8])] = &[TREE[0], TREE[1]];
    let added: &[(&str, &[u8])] = &[TREE[0], TREE[1], TREE[2], ("README.md", b"")];
    let crlf: &[(&str, &[u8])] = &[TREE[0], ("src/lib.rs", b"//! built-ins\r\n"), TREE[2]];
    for changed in [edited, renamed, removed, added, crlf] {
        assert_ne!(digest(changed).unwrap(), base);
    }
    // Moving bytes between files is a different tree too.
    let swapped: &[(&str, &[u8])] = &[
        TREE[0],
        ("src/lib.rs", TREE[2].1),
        ("src/decision.rs", TREE[1].1),
    ];
    assert_ne!(digest(swapped).unwrap(), base);
}

#[test]
fn ambiguous_trees_refuse() {
    assert_eq!(digest(&[]), Err(SourceTreeError::Empty));
    for path in [
        "", "/abs", "a//b", "./a", "a/../b", "a\\b", "a/", "a\tb", "..",
    ] {
        assert!(!is_valid_path(path), "{path:?}");
        assert_eq!(
            digest(&[(path, b"x")]),
            Err(SourceTreeError::InvalidPath(path.to_owned()))
        );
    }
    assert_eq!(
        digest(&[("a", b"1"), ("a", b"2")]),
        Err(SourceTreeError::DuplicatePath("a".to_owned()))
    );
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]
    #[test]
    fn the_digest_does_not_depend_on_listing_order(
        files in prop::collection::btree_map("[a-z]{1,6}(/[a-z]{1,6}){0,2}", prop::collection::vec(any::<u8>(), 0..32), 1..8),
        seed in any::<u64>(),
    ) {
        let mut listed: Vec<(&str, &[u8])> = files.iter().map(|(path, bytes)| (path.as_str(), bytes.as_slice())).collect();
        let forward = digest(&listed).unwrap();
        let len = listed.len();
        let shift = seed.checked_rem(u64::try_from(len).unwrap()).unwrap();
        listed.rotate_left(usize::try_from(shift).unwrap());
        listed.reverse();
        prop_assert_eq!(digest(&listed).unwrap(), forward);
    }
}

/// ST-1: paths are hashed by their exact bytes, never NFC-normalized, and
/// sorted by raw bytes.
const UNICODE_TREE: &[(&str, &[u8])] = &[
    ("\u{e9}.rs", b"same\n"),
    ("e\u{301}.rs", b"same\n"),
    ("z.rs", b"z\n"),
    ("Z.rs", b"Z\n"),
    ("\u{ff61}.rs", b"half\n"),
    ("\u{1f600}.rs", b"smile\n"),
];

#[test]
fn unicode_paths_hash_by_their_exact_bytes() {
    let precomposed: &[(&str, &[u8])] = &[("\u{e9}.rs", b"same\n")];
    let decomposed: &[(&str, &[u8])] = &[("e\u{301}.rs", b"same\n")];
    assert_ne!(
        digest(precomposed).unwrap(),
        digest(decomposed).unwrap(),
        "an NFC-equivalent rename is a different tree"
    );
    // Both spellings in one tree are two files, not a duplicate path.
    assert!(digest(&UNICODE_TREE[..2]).is_ok());
}

#[test]
fn the_unicode_vector_is_pinned_and_sorted_by_raw_bytes() {
    // The vector was computed independently: Python sorting entries by UTF-8
    // path bytes (the decomposed `e\u{301}` sorts before `z`, while its NFC form
    // sorts after it; U+FF61 sorts before U+1F600, unlike UTF-16 order) and
    // encoding them with `json.dumps(sort_keys=True, ensure_ascii=False)`.
    assert_eq!(
        digest(UNICODE_TREE).unwrap(),
        include_str!("fixtures/source-tree-unicode.digest").trim()
    );
    let mut reversed = UNICODE_TREE.to_vec();
    reversed.reverse();
    assert_eq!(digest(&reversed).unwrap(), digest(UNICODE_TREE).unwrap());
}
