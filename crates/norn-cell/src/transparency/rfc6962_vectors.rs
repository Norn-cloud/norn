//! RFC 6962 conformance against transparency-dev/merkle's vectors (#1429).
//!
//! The corpus is vendored byte-identical under `third_party/transparency-dev-merkle`
//! (Apache-2.0, pinned in `registry/standards/transparency-dev-merkle.source.json`):
//! `testonly/constants.go` gives eight leaves and the root of every prefix tree,
//! `testdata/{inclusion,consistency}` give accepted and refused proofs. They
//! run against the production receipt log (`ReceiptLog`) and Norn's checked
//! verifiers (`Checkpoint`, `InclusionProof`, `ConsistencyProof`).
//!
//! Claimed: RFC 6962 §2.1 tree hash, inclusion (§2.1.1) and consistency (§2.1.2)
//! proofs, and the RFC 9162 §2.1.3/§2.1.4 verification algorithms. Not claimed,
//! so not vendored: the `subtree*` probes (C2SP tlog subtree proofs). Skipped
//! with a reason: probes whose leaf hash has no preimage among the eight leaves
//! (Norn verifies leaf values, never bare leaf hashes), probes whose proof list
//! holds an element that is not one hash, or whose root is not one SHA-256
//! digest (Norn's proofs are concatenated hashes and its checkpoints hold a
//! digest, so neither can be expressed), and the deliberate divergence below.
//!
//! Deliberate divergence: Norn refuses a consistency proof from the empty tree
//! (`old_size == 0`), even where RFC 9162 accepts an empty proof between two
//! empty-tree roots; an empty prefix carries no commitment
//! (`Checkpoint::verify_consistency`).
#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::arithmetic_side_effects,
    clippy::print_stderr,
    reason = "conformance corpus test: fixed vectors, and the tally is printed"
)]

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::Deserialize;
use transparency_sha2::{Digest as _, Sha256};

use super::{Checkpoint, ConsistencyProof, InclusionProof, ReceiptLog};

fn corpus() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../third_party/transparency-dev-merkle")
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _infallible = write!(out, "{byte:02x}");
        out
    })
}

fn unhex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&text[at..at + 2], 16).expect("hex constant"))
        .collect()
}

/// The `hd("…")` constants of one Go function in `constants.go`, in order.
fn go_constants(source: &str, function: &str) -> Vec<Vec<u8>> {
    let body = source
        .split(&format!("func {function}()"))
        .nth(1)
        .and_then(|rest| rest.split("\n}\n").next())
        .unwrap_or_else(|| panic!("constants.go defines {function}"));
    body.split("hd(\"")
        .skip(1)
        .map(|rest| unhex(rest.split('"').next().expect("closing quote")))
        .collect()
}

struct Constants {
    leaves: Vec<String>,
    roots: Vec<Vec<u8>>,
}

fn constants() -> Constants {
    let source = std::fs::read_to_string(corpus().join("testonly/constants.go"))
        .expect("vendored constants.go");
    let leaves = go_constants(&source, "LeafInputs")
        .into_iter()
        .map(|leaf| String::from_utf8(leaf).expect("the vector leaves are ASCII"))
        .collect::<Vec<_>>();
    // `RootHashes` starts with `EmptyRootHash()`, which is defined below it.
    let mut roots = go_constants(&source, "EmptyRootHash");
    roots.extend(go_constants(&source, "RootHashes"));
    assert_eq!(leaves.len(), 8, "eight vector leaves");
    assert_eq!(roots.len(), 9, "a root for every tree size 0..=8");
    Constants { leaves, roots }
}

fn leaf_hash(leaf: &str) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update([0_u8]);
    hasher.update(leaf.as_bytes());
    hasher.finalize().to_vec()
}

fn log_of(leaves: &[String]) -> ReceiptLog {
    let mut log = ReceiptLog::new();
    for leaf in leaves {
        log.push(leaf.clone());
    }
    log
}

fn bytes64(text: &str) -> Vec<u8> {
    STANDARD.decode(text).expect("probe base64")
}

/// A probe's proof as Norn's wire form, concatenated SHA-256 hashes. A list
/// element that is not one hash (the `*-garbage` probes add an empty one)
/// has no such encoding, so the probe cannot be posed to Norn.
fn wire_path(proof: Option<Vec<String>>) -> Option<Vec<u8>> {
    let mut path = Vec::new();
    for hash in proof.unwrap_or_default() {
        let hash = bytes64(&hash);
        if hash.len() != 32 {
            return None;
        }
        path.extend(hash);
    }
    Some(path)
}

fn checkpoint(root: &[u8], size: u64) -> Option<Checkpoint> {
    // A root that is not one SHA-256 digest cannot be a Norn checkpoint.
    <[u8; 32]>::try_from(root)
        .ok()
        .map(|digest| Checkpoint::new(digest, size))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct InclusionProbe {
    leaf_idx: u64,
    tree_size: u64,
    root: String,
    leaf_hash: String,
    proof: Option<Vec<String>>,
    desc: String,
    want_err: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConsistencyProbe {
    size1: u64,
    size2: u64,
    root1: String,
    root2: String,
    proof: Option<Vec<String>>,
    desc: String,
    want_err: bool,
}

fn probes(kind: &str) -> Vec<(String, Vec<u8>)> {
    fn walk(dir: &Path, root: &Path, out: &mut Vec<(String, Vec<u8>)>) {
        let mut entries = std::fs::read_dir(dir)
            .expect("probe directory")
            .map(|entry| entry.expect("probe entry").path())
            .collect::<Vec<_>>();
        entries.sort();
        for path in entries {
            if path.is_dir() {
                walk(&path, root, out);
            } else if path.extension().is_some_and(|ext| ext == "json") {
                let name = path
                    .strip_prefix(root)
                    .expect("under the corpus")
                    .display()
                    .to_string();
                out.push((name, std::fs::read(&path).expect("probe bytes")));
            }
        }
    }
    let root = corpus().join("testdata").join(kind);
    let mut out = Vec::new();
    walk(&root, &root, &mut out);
    out
}

/// Outcome tally of one corpus: run, passed, and skipped by reason.
#[derive(Default)]
struct Tally {
    passed: usize,
    failed: Vec<String>,
    skipped: BTreeMap<&'static str, usize>,
}

impl Tally {
    fn check(&mut self, name: &str, ok: bool, detail: impl FnOnce() -> String) {
        if ok {
            self.passed += 1;
        } else {
            self.failed.push(format!("{name}: {}", detail()));
        }
    }

    fn skip(&mut self, reason: &'static str) {
        *self.skipped.entry(reason).or_default() += 1;
    }

    fn finish(self, corpus: &str, minimum: usize) {
        let skipped: usize = self.skipped.values().sum();
        eprintln!(
            "{corpus}: run {}, passed {}, failed {}, skipped {skipped} {:?}",
            self.passed + self.failed.len(),
            self.passed,
            self.failed.len(),
            self.skipped
        );
        assert!(
            self.failed.is_empty(),
            "{corpus} failures:\n{}",
            self.failed.join("\n")
        );
        assert!(
            self.passed >= minimum,
            "{corpus}: only {} vectors ran; the corpus is missing or its parser regressed",
            self.passed
        );
    }
}

#[test]
fn receipt_log_roots_match_every_vector_tree_size() {
    let vectors = constants();
    for size in 0..=vectors.leaves.len() {
        let log = log_of(&vectors.leaves[..size]);
        let root = log.root();
        assert_eq!(root.num_leaves(), u64::try_from(size).expect("small tree"));
        assert_eq!(
            hex(root.as_bytes()),
            hex(&vectors.roots[size]),
            "RFC 6962 root of the first {size} vector leaves"
        );
    }
}

#[test]
fn inclusion_vectors_verify_and_regenerate() {
    let vectors = constants();
    let preimages: BTreeMap<Vec<u8>, &String> = vectors
        .leaves
        .iter()
        .map(|leaf| (leaf_hash(leaf), leaf))
        .collect();
    let mut tally = Tally::default();
    for (name, bytes) in probes("inclusion") {
        let probe: InclusionProbe = serde_json::from_slice(&bytes).expect("inclusion probe");
        let Some(leaf) = preimages.get(&bytes64(&probe.leaf_hash)) else {
            tally.skip("leaf hash has no preimage among the vector leaves");
            continue;
        };
        let Some(path) = wire_path(probe.proof) else {
            tally.skip("a proof element is not one hash: no Norn wire encoding");
            continue;
        };
        let Some(root) = checkpoint(&bytes64(&probe.root), probe.tree_size) else {
            tally.skip("a root is not one SHA-256 digest: no Norn checkpoint holds it");
            continue;
        };
        let accepted = InclusionProof::try_from_bytes(path.clone())
            .is_ok_and(|proof| root.verify_inclusion(leaf, probe.leaf_idx, &proof).is_ok());
        tally.check(&name, accepted != probe.want_err, || {
            format!(
                "{}: wantErr {} but Norn accepted={accepted}",
                probe.desc, probe.want_err
            )
        });
        // An accepted vector is also what the production log proves.
        if !probe.want_err && probe.tree_size <= 8 {
            let size = usize::try_from(probe.tree_size).expect("small tree");
            let index = usize::try_from(probe.leaf_idx).expect("small index");
            let ours = log_of(&vectors.leaves[..size]).prove_inclusion(index);
            tally.check(
                &format!("{name} (regenerated)"),
                ours.as_bytes() == path,
                || "the production log proves a different path".to_owned(),
            );
        }
    }
    tally.finish("RFC 6962 inclusion", 30);
}

#[test]
fn consistency_vectors_verify_and_regenerate() {
    let vectors = constants();
    let mut tally = Tally::default();
    for (name, bytes) in probes("consistency") {
        let probe: ConsistencyProbe = serde_json::from_slice(&bytes).expect("consistency probe");
        if probe.size1 == 0 && !probe.want_err {
            tally.skip("deliberate divergence: Norn refuses consistency from the empty tree");
            continue;
        }
        let Some(path) = wire_path(probe.proof) else {
            tally.skip("a proof element is not one hash: no Norn wire encoding");
            continue;
        };
        let (Some(old), Some(new)) = (
            checkpoint(&bytes64(&probe.root1), probe.size1),
            checkpoint(&bytes64(&probe.root2), probe.size2),
        ) else {
            tally.skip("a root is not one SHA-256 digest: no Norn checkpoint holds it");
            continue;
        };
        let accepted = ConsistencyProof::try_from_bytes(path.clone())
            .is_ok_and(|proof| new.verify_consistency(&old, &proof).is_ok());
        tally.check(&name, accepted != probe.want_err, || {
            format!(
                "{}: wantErr {} but Norn accepted={accepted}",
                probe.desc, probe.want_err
            )
        });
        if !probe.want_err && probe.size2 <= 8 {
            let size = usize::try_from(probe.size2).expect("small tree");
            let additions = usize::try_from(probe.size2 - probe.size1).expect("small tree");
            let ours = log_of(&vectors.leaves[..size]).prove_consistency(additions);
            tally.check(
                &format!("{name} (regenerated)"),
                ours.as_bytes() == path,
                || "the production log proves a different path".to_owned(),
            );
        }
    }
    tally.finish("RFC 6962 consistency", 30);
}
