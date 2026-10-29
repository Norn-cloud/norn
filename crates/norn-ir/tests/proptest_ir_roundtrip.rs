//! Example property test for the IR wire contract (rust-safety baseline).
//!
//! Pattern to copy for new IR surfaces: generate arbitrary values of a
//! canonical IR type, assert the serde wire roundtrip is lossless. See
//! docs/platform/rust-safety-baseline.md for the adoption plan.

#![allow(
    clippy::pedantic,
    missing_docs,
    reason = "test-only helpers; not production API surface"
)]
#![deny(
    clippy::match_wildcard_for_single_variants,
    clippy::format_collect,
    clippy::format_push_string,
    clippy::inefficient_to_string,
    clippy::large_types_passed_by_value,
    clippy::needless_pass_by_value,
    clippy::large_futures
)]

use norn_ir::{Diagnostic, DiagnosticRemediation, DiagnosticSeverity};
use proptest::prelude::*;

fn severity_strategy() -> impl Strategy<Value = DiagnosticSeverity> {
    prop_oneof![
        Just(DiagnosticSeverity::Error),
        Just(DiagnosticSeverity::Warning),
        Just(DiagnosticSeverity::Info),
    ]
}

fn remediation_strategy() -> impl Strategy<Value = DiagnosticRemediation> {
    prop_oneof![
        Just(DiagnosticRemediation::FixDefinition),
        Just(DiagnosticRemediation::UpgradeIr),
        Just(DiagnosticRemediation::ResolveDependency),
        Just(DiagnosticRemediation::CheckScope),
        Just(DiagnosticRemediation::Internal),
    ]
}

fn diagnostic_strategy() -> impl Strategy<Value = Diagnostic> {
    (
        "[a-z][a-z0-9_.]{0,40}",
        severity_strategy(),
        ".{0,120}",
        proptest::option::of("[a-z][a-z0-9_.]{0,40}"),
        proptest::option::of("[a-zA-Z][a-zA-Z0-9_.\\[\\]]{0,40}"),
        proptest::option::of(remediation_strategy()),
    )
        .prop_map(
            |(code, severity, message, artifact_id, path, remediation)| Diagnostic {
                code,
                severity,
                message,
                artifact_id,
                path,
                remediation,
            },
        )
}

proptest! {
    /// Every diagnostic must survive a JSON wire roundtrip unchanged: the
    /// diagnostic channel is how the platform reports validation failures, so
    /// silent lossiness here would corrupt the audit trail.
    #[test]
    fn diagnostic_json_roundtrip_is_lossless(diagnostic in diagnostic_strategy()) {
        let encoded = serde_json::to_string(&diagnostic)
            .map_err(|error| TestCaseError::fail(format!("encode failed: {error}")))?;
        let decoded: Diagnostic = serde_json::from_str(&encoded)
            .map_err(|error| TestCaseError::fail(format!("decode failed: {error}")))?;
        prop_assert_eq!(diagnostic, decoded);
    }
}
