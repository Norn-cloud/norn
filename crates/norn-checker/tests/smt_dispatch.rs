#![cfg(not(target_family = "wasm"))]
#![allow(
    clippy::expect_used,
    clippy::panic,
    missing_docs,
    reason = "pinned native solver correspondence"
)]
use norn_checker::obligations::smt::{SmtBound, SmtResult, dispatch_smt};
use sha2::{Digest, Sha256};
use std::{path::PathBuf, time::Duration};
fn backend() -> (PathBuf, String, SmtBound) {
    let path = PathBuf::from(
        std::env::var_os("NORN_VERIFICATION_TOOL_ROOT").expect("install pinned tools"),
    )
    .join("bin/z3");
    let hash = format!(
        "{:x}",
        Sha256::digest(std::fs::read(&path).expect("attested binary"))
    );
    (
        path,
        hash,
        SmtBound {
            timeout: Duration::from_secs(3),
            bytes: 65_536,
        },
    )
}
#[test]
#[ignore = "requires pinned Z3; explicitly qualified on Boat with --include-ignored"]
fn smt_sat_retains_model_and_exact_tool_obligation_identity() {
    let (path, hash, bound) = backend();
    let query = "(set-logic QF_LIA)(declare-const x Int)(assert (= x 7))";
    let evidence = dispatch_smt(query, bound, &path, &hash);
    assert!(matches!(evidence.result, SmtResult::Sat(ref model) if model.contains('7')));
    assert_eq!(evidence.z3_sha256.as_ref(), Some(&hash));
    assert!(
        evidence
            .z3_version
            .expect("observed version")
            .contains("5.1.0")
    );
    assert_eq!(
        evidence.obligation_sha256,
        format!("{:x}", Sha256::digest(query.as_bytes()))
    );
}
#[test]
#[ignore = "requires pinned Z3; explicitly qualified on Boat with --include-ignored"]
fn smt_unsat_requires_a_clean_bounded_attested_answer() {
    let (path, hash, bound) = backend();
    assert_eq!(
        dispatch_smt(
            "(set-logic QF_LIA)(declare-const x Int)(assert (> x 1))(assert (< x 0))",
            bound,
            &path,
            &hash
        )
        .result,
        SmtResult::Unsat
    );
}
#[test]
#[ignore = "requires pinned Z3; explicitly qualified on Boat with --include-ignored"]
fn smt_unknown_covers_unsupported_timeout_missing_and_attestation_failures() {
    let (path, hash, bound) = backend();
    for result in [
        dispatch_smt("(set-logic NORN_UNSUPPORTED)", bound, &path, &hash).result,
        dispatch_smt(
            "(assert true)",
            SmtBound {
                timeout: Duration::ZERO,
                ..bound
            },
            &path,
            &hash,
        )
        .result,
        dispatch_smt("", bound, &path.with_extension("missing"), &hash).result,
        dispatch_smt("", bound, &path, &"0".repeat(64)).result,
        dispatch_smt("(echo \"unsat\")", bound, &path, &hash).result,
    ] {
        assert!(matches!(result, SmtResult::Unknown(_)));
    }
}
