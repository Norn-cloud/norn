#![allow(
    clippy::expect_used,
    clippy::print_stderr,
    missing_docs,
    reason = "conformance corpus test: fixed vectors, and the tally is printed"
)]

//! RFC 8785 (JCS) conformance of `norn_api::jcs` against the reference
//! implementation's test data (#1429).
//!
//! The vectors are vendored byte-identical under
//! `third_party/json-canonicalization/testdata` (Apache-2.0, pinned in
//! `registry/standards/cyberphone-json-canonicalization.source.json`): each
//! `input/<name>.json` canonicalizes to exactly the bytes of
//! `output/<name>.json`. They cover member order by UTF-16 code units, string
//! escaping, ECMAScript number spelling, and that JCS never normalizes Unicode.
//!
//! Scope: `norn_api::jcs` is the encoder that claims RFC 8785 (connector
//! manifest digests). Norn1 (`norn_canonical`) is a different format by design
//! (exact decimals, NFC strings, code-point order) and is not run here.
//! Not vendored: the 100-million-line ES6 number file (`numgen`), which is
//! generated, not a fixed vector; the in-crate unit test pins number spelling
//! edges instead.

use std::path::Path;

use norn_api::jcs::canonicalize_text;

#[test]
fn every_reference_vector_canonicalizes_to_its_exact_output() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../third_party/json-canonicalization/testdata");
    let mut names = std::fs::read_dir(root.join("input"))
        .expect("vendored JCS inputs")
        .map(|entry| entry.expect("input entry").file_name())
        .collect::<Vec<_>>();
    names.sort();
    let mut failures = Vec::new();
    for name in &names {
        let input = std::fs::read_to_string(root.join("input").join(name)).expect("input text");
        let expected = std::fs::read(root.join("output").join(name)).expect("output bytes");
        match canonicalize_text(&input) {
            Ok(text) if text.as_bytes() == expected => {}
            Ok(text) => failures.push(format!(
                "{}: got {text:?}, want {:?}",
                name.to_string_lossy(),
                String::from_utf8_lossy(&expected)
            )),
            Err(error) => failures.push(format!("{}: refused: {error}", name.to_string_lossy())),
        }
    }
    eprintln!(
        "RFC 8785: run {}, passed {}, failed {}, skipped 0",
        names.len(),
        names.len().saturating_sub(failures.len()),
        failures.len()
    );
    assert!(
        failures.is_empty(),
        "JCS failures:\n{}",
        failures.join("\n")
    );
    assert_eq!(names.len(), 6, "the six pinned reference vectors ran");
}
