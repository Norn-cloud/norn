#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::pedantic,
    reason = "integration-test crate"
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

//! Wrapper refusals preserve the domain code, classification, and redaction.

use std::collections::{BTreeMap, BTreeSet};
use std::convert::Infallible;

use norn_kernel::action::{
    ActionAuthorityState, ActionRejection, AuthorizedRejection, OwnerExpectation,
    TransitionBinding, VersionedAction, VersionedActionRequest,
};
use norn_kernel::{
    CommitError, Entropy, EnvelopeLimits, INTERNAL_MESSAGE, LogicalTime, Observations, Refusable,
    RefusalCategory, StepError, step,
};

#[test]
fn actual_step_preserves_versioned_action_refusals() {
    let observations = Observations::main(LogicalTime::EPOCH, Entropy::from_bytes([0; 32]));
    for (current, expected, transition, code, category) in [
        (
            2,
            1,
            "transition",
            "action.owner_version_conflict",
            RefusalCategory::Conflict,
        ),
        (
            u64::MAX,
            u64::MAX,
            "transition",
            "action.owner_version_overflow",
            RefusalCategory::Limit,
        ),
        (
            0,
            0,
            "",
            "action.invalid_transition_identity",
            RefusalCategory::Invalid,
        ),
    ] {
        let state = ActionAuthorityState {
            owner_versions: BTreeMap::from([(1u8, current)]),
            ..Default::default()
        };
        let request = VersionedActionRequest {
            transition_id: transition.to_owned(),
            binding: TransitionBinding {
                input_sha256: "input".to_owned(),
                effect_sha256: "effect".to_owned(),
            },
            preconditions: vec![OwnerExpectation {
                owner: 1,
                expected_version: expected,
            }],
            mutation_owners: BTreeSet::from([1]),
            idempotency_key: "idempotency".to_owned(),
        };
        let error = step(
            &VersionedAction::new(),
            &state,
            &request,
            &observations,
            &EnvelopeLimits::V1,
        )
        .expect_err("the domain rejects this request");
        let refusal = error.refusal();
        assert_eq!(refusal.code, code);
        assert_eq!(refusal.category, category);
        assert_eq!(error.code(), refusal.code);
        assert_eq!(error.category(), refusal.category);
    }
}

#[test]
fn nested_wrappers_preserve_internal_redaction() {
    let host = CommitError::Host("private tenant database path");
    let expected = host.refusal();
    let step = StepError::Rejected(host);
    assert_eq!(step.refusal(), expected);
    assert_eq!(step.refusal().message, INTERNAL_MESSAGE);
    let shell = AuthorizedRejection::Shell(step);
    assert_eq!(shell.refusal(), expected);
    assert_eq!(shell.category(), RefusalCategory::Internal);
    assert_eq!(shell.code(), expected.code);
}

#[test]
fn outer_failures_keep_their_own_codes() {
    let step = StepError::<ActionRejection>::ReadOnlyWorld;
    assert_eq!(step.code(), "decision.read_only_world");
    assert_eq!(step.category(), RefusalCategory::Invalid);
    let shell = AuthorizedRejection::<CommitError<Infallible>>::LogicalTimeRollback;
    assert_eq!(shell.code(), "action.logical_time_rollback");
    assert_eq!(shell.category(), RefusalCategory::Invalid);
}
