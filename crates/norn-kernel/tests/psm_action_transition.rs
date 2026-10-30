//! Decider laws and a pre-extraction reference for the ring-0 action
//! transition (T2-KER-2a).
//!
//! The model is a transcription of the check order `norn-engine`'s
//! `commit_versioned_action_effect` applied before the extraction (identity →
//! protocol ownership → exact replay → duplicate/missing preconditions → CAS
//! in owner order → idempotency), over a deliberately small universe so
//! replays, conflicts, and idempotency collisions are frequent. The engine's
//! own differential test (`storage::tests::legacy_action_differential`)
//! checks byte-identical engine outputs against a frozen copy of the
//! pre-extraction code; this suite checks the kernel decider's laws.

use std::collections::{BTreeMap, BTreeSet};

use norn_kernel::action::{
    ActionCommandRejection, ActionCommandShape, ActionRejection, AuthorizedActionRequest,
    AuthorizedActionShell, AuthorizedActionState, AuthorizedBinding, AuthorizedOutcome,
    AuthorizedRejection, OwnerExpectation, TransitionBinding, VersionedAction,
    VersionedActionRequest, check_action_command, decide_authorized,
};
use norn_kernel::{Decider, Entropy, LogicalTime, Observations};
use norn_testkit::proptest::prelude::*;
use norn_testkit::{DeciderModel, DecisionShape, Prediction, Settings, StateOf, check};

type Owner = u8;

// ------------------------------------------------------------------ model

/// The pre-extraction verdict for one request.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Verdict {
    Replay,
    Commit,
    Reject(&'static str),
}

/// The authority slice as the pre-extraction engine kept it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Model {
    versions: BTreeMap<Owner, u64>,
    receipts: BTreeMap<String, (String, String)>,
    idempotency: BTreeSet<String>,
}

fn reference(model: &Model, request: &VersionedActionRequest<Owner>) -> Verdict {
    if request.transition_id.trim().is_empty() || request.binding.input_sha256.trim().is_empty() {
        return Verdict::Reject("invalid_identity");
    }
    // The engine always supplies a non-empty effect digest (it computes it);
    // the kernel refuses an empty one rather than commit an unreplayable id.
    if request.binding.effect_sha256.trim().is_empty() {
        return Verdict::Reject("invalid_identity");
    }
    // (No other protocol exists in this model.)
    if let Some((input, effect)) = model.receipts.get(&request.transition_id) {
        if *input != request.binding.input_sha256
            || effect.is_empty()
            || *effect != request.binding.effect_sha256
        {
            return Verdict::Reject("binding_mismatch");
        }
        return Verdict::Replay;
    }
    let mut expectations = BTreeMap::new();
    for expectation in &request.preconditions {
        if expectations
            .insert(expectation.owner, expectation.expected_version)
            .is_some()
        {
            return Verdict::Reject("duplicate_precondition");
        }
    }
    for owner in &request.mutation_owners {
        if !expectations.contains_key(owner) {
            return Verdict::Reject("missing_precondition");
        }
    }
    for (owner, expected) in &expectations {
        if model.versions.get(owner).copied().unwrap_or(0) != *expected {
            return Verdict::Reject("version_conflict");
        }
    }
    if model.idempotency.contains(&request.idempotency_key) {
        return Verdict::Reject("idempotency_without_receipt");
    }
    Verdict::Commit
}

fn apply(model: &Model, request: &VersionedActionRequest<Owner>) -> Model {
    let mut next = model.clone();
    for owner in &request.mutation_owners {
        let version = next.versions.entry(*owner).or_insert(0);
        *version = version.saturating_add(1);
    }
    next.receipts.insert(
        request.transition_id.clone(),
        (
            request.binding.input_sha256.clone(),
            request.binding.effect_sha256.clone(),
        ),
    );
    next.idempotency.insert(request.idempotency_key.clone());
    next
}

const fn rejection_label(rejection: &ActionRejection) -> &'static str {
    match rejection {
        ActionRejection::InvalidTransitionIdentity => "invalid_identity",
        ActionRejection::TransitionUsedByOtherProtocol { .. } => "other_protocol",
        ActionRejection::TransitionBindingMismatch { .. } => "binding_mismatch",
        ActionRejection::DuplicatePrecondition => "duplicate_precondition",
        ActionRejection::MissingPrecondition => "missing_precondition",
        ActionRejection::OwnerVersionConflict { .. } => "version_conflict",
        ActionRejection::IdempotencyWithoutReceipt => "idempotency_without_receipt",
        ActionRejection::OwnerVersionOverflow => "overflow",
    }
}

fn request() -> BoxedStrategy<VersionedActionRequest<Owner>> {
    (
        prop_oneof![9 => (0_u8..4).prop_map(|t| format!("t{t}")), 1 => Just(" ".to_string())],
        prop_oneof![9 => (0_u8..2).prop_map(|i| format!("in{i}")), 1 => Just(String::new())],
        prop_oneof![9 => (0_u8..2).prop_map(|e| format!("fx{e}")), 1 => Just(String::new())],
        prop::collection::vec((0_u8..3, 0_u64..3), 0..4),
        prop::collection::btree_set(0_u8..3, 0..3),
        (0_u8..3).prop_map(|k| format!("k{k}")),
        any::<bool>(),
    )
        .prop_map(
            |(transition_id, input, effect, guards, mutation_owners, idempotency_key, exact)| {
                // Usually precondition every mutated owner at a small version
                // so commits happen; `guards` add duplicates, extras, and gaps.
                let mut preconditions: Vec<_> = if exact {
                    mutation_owners
                        .iter()
                        .map(|owner| OwnerExpectation {
                            owner: *owner,
                            expected_version: 0,
                        })
                        .collect()
                } else {
                    Vec::new()
                };
                preconditions.extend(guards.into_iter().map(|(owner, expected_version)| {
                    OwnerExpectation {
                        owner,
                        expected_version,
                    }
                }));
                VersionedActionRequest {
                    transition_id,
                    binding: TransitionBinding {
                        input_sha256: input,
                        effect_sha256: effect,
                    },
                    preconditions,
                    mutation_owners,
                    idempotency_key,
                }
            },
        )
        .boxed()
}

struct ActionModel;

impl DeciderModel for ActionModel {
    type Decider = VersionedAction<Owner>;
    type Model = Model;

    fn decider() -> VersionedAction<Owner> {
        VersionedAction::new()
    }

    fn initial_model() -> Model {
        Model::default()
    }

    fn inputs(_: &Model) -> BoxedStrategy<VersionedActionRequest<Owner>> {
        request()
    }

    fn predict(
        model: &Model,
        input: &VersionedActionRequest<Owner>,
        _: &Observations,
    ) -> Prediction<Model> {
        match reference(model, input) {
            Verdict::Replay => Prediction::Accept {
                next: model.clone(),
                shape: DecisionShape::default(),
            },
            Verdict::Commit => Prediction::Accept {
                next: apply(model, input),
                shape: DecisionShape {
                    events: 1,
                    performs: 0,
                    timers: 0,
                },
            },
            Verdict::Reject(_) => Prediction::Reject,
        }
    }

    fn project(state: &StateOf<Self>) -> Model {
        Model {
            versions: state.owner_versions.clone(),
            receipts: state
                .transitions
                .iter()
                .map(|(id, binding)| {
                    (
                        id.clone(),
                        (binding.input_sha256.clone(), binding.effect_sha256.clone()),
                    )
                })
                .collect(),
            idempotency: state.idempotency_keys.clone(),
        }
    }

    fn input_label(_: &VersionedActionRequest<Owner>) -> &'static str {
        "versioned"
    }

    fn event_label(_: &norn_kernel::action::ActionTransitionCommitted<Owner>) -> &'static str {
        "committed"
    }
}

#[test]
fn psm_action_transition_obeys_decider_laws() {
    let coverage = check::<ActionModel>(&Settings::new(file!()).cases(128));
    coverage.assert_covered(&[
        "outcome:accepted",
        "outcome:rejected",
        "outcome:read_only_world",
        "outcome:envelope_exceeded:events",
        "event:committed",
    ]);
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 512,
        failure_persistence: None,
        ..ProptestConfig::default()
    })]

    /// Beyond the harness's accept/reject agreement: the exact rejection
    /// matches the pre-extraction check order, on arbitrary reachable slices.
    #[test]
    fn versioned_rejection_matches_pre_extraction_order(
        history in prop::collection::vec(request(), 0..8),
        probe in request(),
    ) {
        let decider = VersionedAction::<Owner>::new();
        let observations = Observations::main(LogicalTime::EPOCH, Entropy::from_bytes([0; 32]));
        let mut state = decider.initial_state();
        let mut model = Model::default();
        for input in history.iter().chain(std::iter::once(&probe)) {
            let actual = decider.decide(&state, input, &observations);
            let expected = reference(&model, input);
            match (&actual, &expected) {
                (Ok(decision), Verdict::Replay) => prop_assert!(decision.events.is_empty()),
                (Ok(decision), Verdict::Commit) => prop_assert_eq!(decision.events.len(), 1),
                (Err(rejection), Verdict::Reject(label)) => {
                    prop_assert_eq!(rejection_label(rejection), *label);
                }
                _ => prop_assert!(false, "kernel {:?} vs reference {:?}", actual, expected),
            }
            if let Ok(decision) = actual {
                for event in &decision.events {
                    state = decider.evolve(&state, event);
                }
            }
            if expected == Verdict::Commit {
                model = apply(&model, input);
            }
        }
    }
}

#[test]
fn foreign_transition_ids_are_refused_before_replay() {
    let decider = VersionedAction::<Owner>::new();
    let mut state = decider.initial_state();
    state.foreign_transitions.insert("t0".to_string());
    state.transitions.insert(
        "t0".to_string(),
        TransitionBinding {
            input_sha256: "in".to_string(),
            effect_sha256: "fx".to_string(),
        },
    );
    let request = VersionedActionRequest {
        transition_id: "t0".to_string(),
        binding: TransitionBinding {
            input_sha256: "in".to_string(),
            effect_sha256: "fx".to_string(),
        },
        preconditions: Vec::new(),
        mutation_owners: BTreeSet::new(),
        idempotency_key: "k".to_string(),
    };
    let observations = Observations::main(LogicalTime::EPOCH, Entropy::from_bytes([0; 32]));
    assert_eq!(
        decider.decide(&state, &request, &observations),
        Err(ActionRejection::TransitionUsedByOtherProtocol {
            transition_id: "t0".to_string()
        })
    );
}

#[test]
fn action_command_scope_fails_closed_before_identity_fields() {
    let shape = |org_id, action_id, action_version, idempotency_key| ActionCommandShape {
        org_id,
        action_id,
        action_version,
        idempotency_key,
    };
    assert_eq!(
        check_action_command(&shape(None, "", None, "")),
        Err(ActionCommandRejection::MissingOrgScope)
    );
    assert_eq!(
        check_action_command(&shape(Some("  "), "a", None, "k")),
        Err(ActionCommandRejection::MissingOrgScope)
    );
    assert_eq!(
        check_action_command(&shape(Some("org"), " ", None, "k")),
        Err(ActionCommandRejection::BlankField)
    );
    assert_eq!(
        check_action_command(&shape(Some("org"), "a", Some(""), "k")),
        Err(ActionCommandRejection::BlankField)
    );
    assert_eq!(
        check_action_command(&shape(Some("org"), "a", None, "\t")),
        Err(ActionCommandRejection::BlankField)
    );
    assert_eq!(
        check_action_command(&shape(Some("org"), "a", Some("1"), "k")),
        Ok(())
    );
}

// ------------------------------------------------- authorization stage

/// A shell that records which checks the kernel asked for, in order.
struct RecordingShell {
    integrity: Result<(), &'static str>,
    authorization: Result<u8, &'static str>,
    log: Vec<&'static str>,
}

impl AuthorizedActionShell for RecordingShell {
    type Error = &'static str;
    type Authorized = u8;

    fn verify_recorded_receipt(&mut self) -> Result<(), &'static str> {
        self.log.push("verify");
        self.integrity
    }

    fn authorize(&mut self) -> Result<u8, &'static str> {
        self.log.push("authorize");
        self.authorization
    }
}

fn binding(tag: &str) -> AuthorizedBinding {
    AuthorizedBinding {
        authorization_input_sha256: format!("auth-{tag}"),
        transition: TransitionBinding {
            input_sha256: format!("in-{tag}"),
            effect_sha256: format!("fx-{tag}"),
        },
    }
}

fn shell(
    integrity: Result<(), &'static str>,
    authorization: Result<u8, &'static str>,
) -> RecordingShell {
    RecordingShell {
        integrity,
        authorization,
        log: Vec::new(),
    }
}

#[test]
fn authorized_stage_orders_binding_protocol_time_then_authorization() {
    let request = |tag: &str, time| AuthorizedActionRequest {
        transition_id: "t".to_string(),
        binding: binding(tag),
        commit_logical_time: time,
    };
    let recorded = AuthorizedActionState {
        existing: Some(binding("a")),
        used_by_authority_protocol: true,
        used_by_versioned_protocol: true,
        last_commit_logical_time: Some(100),
    };

    // A mismatched binding is refused before any shell check.
    let mut s = shell(Err("integrity"), Err("denied"));
    assert_eq!(
        decide_authorized(&recorded, &request("b", 50), &mut s),
        Err(AuthorizedRejection::BindingMismatch {
            transition_id: "t".to_string()
        })
    );
    assert!(s.log.is_empty());

    // An exact retry verifies the recorded receipt first...
    let mut s = shell(Err("integrity"), Ok(1));
    assert_eq!(
        decide_authorized(&recorded, &request("a", 150), &mut s),
        Err(AuthorizedRejection::Shell("integrity"))
    );
    // ...then time monotonicity, then re-authorizes before replaying.
    let mut s = shell(Ok(()), Ok(1));
    assert_eq!(
        decide_authorized(&recorded, &request("a", 50), &mut s),
        Err(AuthorizedRejection::LogicalTimeRollback)
    );
    assert_eq!(s.log, ["verify"]);
    let mut s = shell(Ok(()), Err("revoked"));
    assert_eq!(
        decide_authorized(&recorded, &request("a", 150), &mut s),
        Err(AuthorizedRejection::Shell("revoked"))
    );
    let mut s = shell(Ok(()), Ok(1));
    assert_eq!(
        decide_authorized(&recorded, &request("a", 150), &mut s),
        Ok(AuthorizedOutcome::Replay)
    );
    assert_eq!(s.log, ["verify", "authorize"]);

    // A fresh transition id: protocol ownership, then time, then authorization.
    let mut fresh = AuthorizedActionState {
        existing: None,
        ..recorded
    };
    let mut s = shell(Ok(()), Ok(1));
    assert_eq!(
        decide_authorized(&fresh, &request("a", 50), &mut s),
        Err(AuthorizedRejection::UsedByAuthorityProtocol {
            transition_id: "t".to_string()
        })
    );
    fresh.used_by_authority_protocol = false;
    assert_eq!(
        decide_authorized(&fresh, &request("a", 50), &mut s),
        Err(AuthorizedRejection::LegacyVersionedReceipt)
    );
    fresh.used_by_versioned_protocol = false;
    assert_eq!(
        decide_authorized(&fresh, &request("a", 50), &mut s),
        Err(AuthorizedRejection::LogicalTimeRollback)
    );
    assert!(s.log.is_empty());
    assert_eq!(
        decide_authorized(&fresh, &request("a", 100), &mut s),
        Ok(AuthorizedOutcome::Proceed(1))
    );
    assert_eq!(s.log, ["authorize"]);
}
