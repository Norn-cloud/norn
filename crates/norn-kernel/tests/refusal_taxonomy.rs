#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::pedantic,
    reason = "integration-test crate: fixture helpers unwrap and index"
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

//! The runtime refusal taxonomy (T1-L4, #2420): every kernel refusal has a
//! stable code, a `category()`, and a safe `{code, category, message}` form.
//! Visibility and internal messages are fixed, wrapper refusals preserve the
//! domain code, classification, and redaction, and the existence-never-leaks
//! matrix holds through the action path.

use std::collections::{BTreeMap, BTreeSet};
use std::convert::Infallible;
use std::path::Path;

use norn_ir::{
    ActionRoot, DataQualityOutcome, InvariantId, InvariantOwner, ObjectTypeDef, ObjectTypeName,
    posting::AccountingError,
};
use norn_kernel::action::{
    ACTION_IDEMPOTENCY_KEY_MAX_BYTES, ActionAuthorityState, ActionCommandRejection,
    ActionCommandShape, ActionRejection, AuthorizedRejection, OwnerExpectation, TransitionBinding,
    VersionedAction, VersionedActionRequest, check_action_command,
};
use norn_kernel::cell_host::{JsonObject, JsonValue};
use norn_kernel::{
    ActionAuthorizer, ActionAuthzRequest, ActionCall, ActionDenial, ActionRefusal, CellCommit,
    CellHost, Command, CommitError, DecisionRefusal, DecisionTurn, Entropy, EnvelopeKind,
    EnvelopeLimits, FactEvent, FactKey, FactMutation, FactVersion, FactWrite, INTERNAL_MESSAGE,
    LogicalTime, NOT_FOUND_MESSAGE, Observations, Refusable, Refusal, RefusalCategory, StepError,
    TypeCatalog, World, commit_action, step,
};
use norn_testkit::cell_host::MemoryCell;
use serde_json::{Value, json};

type Cell = MemoryCell<FactEvent, JsonObject, JsonObject>;

fn fixture(path: &str) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .join(path);
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

#[derive(Debug)]
struct HostDown;

impl std::fmt::Display for HostDown {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("disk /var/lib/cell/tenant-42.sqlite is corrupt")
    }
}

impl std::error::Error for HostDown {}

#[test]
fn refusal_categories_are_stable() {
    let key = FactKey::new("commerce.SalesOrder", "SO-1");
    let decision = |refusal: DecisionRefusal<Infallible>| refusal.refusal();
    let cases: Vec<(Refusal, &str, RefusalCategory)> = vec![
        (
            decision(DecisionRefusal::NotFromState {
                key: key.clone(),
                transition: "invoice".into(),
                current: Some("draft".into()),
                from: vec!["approved".into()],
            }),
            "lifecycle.validation.undeclared_write",
            RefusalCategory::Precondition,
        ),
        (
            decision(DecisionRefusal::GuardFalse {
                key: key.clone(),
                transition: "submit".into(),
                guard: "self.amount > 0".into(),
            }),
            "decision.guard_false",
            RefusalCategory::Precondition,
        ),
        (
            decision(DecisionRefusal::Stale {
                key: key.clone(),
                expected: FactVersion(1),
                current: FactVersion(2),
            }),
            "decision.stale_state",
            RefusalCategory::Conflict,
        ),
        (
            decision(DecisionRefusal::AccountingPosting {
                key: key.clone(),
                reason: AccountingError::JournalUnbalancedByUnit,
            }),
            "journal.unbalanced_by_unit",
            RefusalCategory::Invalid,
        ),
        (
            decision(DecisionRefusal::InvariantViolation {
                key: Box::new(key.clone()),
                owner: InvariantOwner::Object(ObjectTypeName("commerce.SalesOrder".into())),
                id: InvariantId("positive_total".into()),
                message: "total must be positive".into(),
                outcome: Box::new(DataQualityOutcome::NotSatisfied),
            }),
            "decision.invariant_violation",
            RefusalCategory::Precondition,
        ),
        (
            decision(DecisionRefusal::InvariantBudgetExceeded {
                key: key.clone(),
                limit: 4096,
            }),
            "decision.invariant_budget_exceeded",
            RefusalCategory::Limit,
        ),
        (
            decision(DecisionRefusal::EnvelopeExceeded {
                kind: EnvelopeKind::Events,
                limit: 32,
                actual: 33,
            }),
            "decision.envelope_exceeded",
            RefusalCategory::Limit,
        ),
        (
            decision(DecisionRefusal::TurnEnvelopeExceeded {
                kind: EnvelopeKind::Events,
                limit: 32,
                actual: 33,
            }),
            "decision.turn_envelope_exceeded",
            RefusalCategory::Limit,
        ),
        (
            decision(DecisionRefusal::UnknownTransition {
                key: key.clone(),
                lifecycle: "status".into(),
                transition: "ship".into(),
            }),
            "artifact_set.lifecycle.unknown_transition",
            RefusalCategory::Invalid,
        ),
        (
            CommitError::<Infallible>::Conflict {
                key: key.clone(),
                expected: FactVersion(1),
                current: FactVersion(2),
            }
            .refusal(),
            "commit.conflict",
            RefusalCategory::Conflict,
        ),
        (
            CommitError::<Infallible>::InvariantViolation {
                key: Box::new(key.clone()),
                owner: InvariantOwner::Object(ObjectTypeName("commerce.SalesOrder".into())),
                id: InvariantId("positive_total".into()),
                message: "total must be positive".into(),
                outcome: Box::new(DataQualityOutcome::NotSatisfied),
            }
            .refusal(),
            "commit.invariant_violation",
            RefusalCategory::Precondition,
        ),
        (
            CommitError::<Infallible>::InvariantBudgetExceeded {
                key: key.clone(),
                limit: 4096,
            }
            .refusal(),
            "commit.invariant_budget_exceeded",
            RefusalCategory::Limit,
        ),
        (
            CommitError::<Infallible>::EffectiveValues {
                key: key.clone(),
                reason: "an exact runtime default is unavailable".into(),
            }
            .refusal(),
            "commit.effective_values",
            RefusalCategory::Invalid,
        ),
        (
            CommitError::<Infallible>::EventLimitExceeded {
                limit: 32,
                actual: 33,
            }
            .refusal(),
            "commit.event_limit",
            RefusalCategory::Limit,
        ),
        (
            ActionRefusal::<Infallible>::TargetNotFound.refusal(),
            "action.target_not_found",
            RefusalCategory::Visibility,
        ),
        (
            ActionRefusal::<Infallible>::PermissionDenied {
                reason: "cedar_deny".into(),
            }
            .refusal(),
            "action.permission_denied",
            RefusalCategory::Permission,
        ),
        (
            ActionRefusal::<Infallible>::IdempotencyConflict { key: "k".into() }.refusal(),
            "action.idempotency_conflict",
            RefusalCategory::Conflict,
        ),
        (
            ActionCommandRejection::MissingOrgScope.refusal(),
            "action.missing_scope",
            RefusalCategory::Invalid,
        ),
        (
            ActionCommandRejection::IdempotencyKeyTooLong {
                actual_bytes: 257,
                max_bytes: ACTION_IDEMPOTENCY_KEY_MAX_BYTES,
            }
            .refusal(),
            "action.idempotency_key_too_long",
            RefusalCategory::Limit,
        ),
        (
            ActionRejection::OwnerVersionConflict {
                expected: 1,
                current: 2,
            }
            .refusal(),
            "action.owner_version_conflict",
            RefusalCategory::Conflict,
        ),
        (
            StepError::<ActionCommandRejection>::ReadOnlyWorld.refusal(),
            "decision.read_only_world",
            RefusalCategory::Invalid,
        ),
    ];
    for (refusal, code, category) in cases {
        assert_eq!(
            (refusal.code.as_str(), refusal.category),
            (code, category),
            "{refusal}"
        );
        assert!(!refusal.message.is_empty());
    }
    // A wrapped decision refusal keeps its code and category.
    let wrapped = ActionRefusal::<Infallible>::Decision(DecisionRefusal::Stale {
        key,
        expected: FactVersion(1),
        current: FactVersion(2),
    });
    assert_eq!(
        (wrapped.code(), wrapped.category()),
        ("decision.stale_state", RefusalCategory::Conflict)
    );
    // The wire form.
    let refusal = ActionCommandRejection::BlankField.refusal();
    assert_eq!(
        serde_json::to_value(&refusal).unwrap(),
        json!({ "code": "action.blank_field", "category": "invalid", "message": refusal.message })
    );
}

#[test]
fn action_idempotency_key_limit_counts_utf8_bytes() {
    let check = |idempotency_key: &str| {
        check_action_command(&ActionCommandShape {
            org_id: Some("org_demo"),
            action_id: "commerce.sales_order.approve",
            action_version: Some("0.1.0"),
            idempotency_key,
        })
    };
    let boundary = "k".repeat(ACTION_IDEMPOTENCY_KEY_MAX_BYTES);
    assert_eq!(check(&boundary), Ok(()));

    let over_limit = "k".repeat(ACTION_IDEMPOTENCY_KEY_MAX_BYTES + 1);
    assert_eq!(
        check(&over_limit),
        Err(ActionCommandRejection::IdempotencyKeyTooLong {
            actual_bytes: ACTION_IDEMPOTENCY_KEY_MAX_BYTES + 1,
            max_bytes: ACTION_IDEMPOTENCY_KEY_MAX_BYTES,
        })
    );

    let multibyte = "€".repeat(86);
    assert_eq!(multibyte.chars().count(), 86);
    assert_eq!(multibyte.len(), 258);
    assert_eq!(
        check(&multibyte),
        Err(ActionCommandRejection::IdempotencyKeyTooLong {
            actual_bytes: 258,
            max_bytes: ACTION_IDEMPOTENCY_KEY_MAX_BYTES,
        })
    );
}

#[test]
fn internal_refusals_never_carry_details() {
    for refusal in [
        DecisionRefusal::Host(HostDown).refusal(),
        CommitError::Host(HostDown).refusal(),
        ActionRefusal::Decision(DecisionRefusal::Host(HostDown)).refusal(),
    ] {
        assert_eq!(refusal.category, RefusalCategory::Internal);
        assert_eq!(refusal.message, INTERNAL_MESSAGE);
        assert!(!refusal.message.contains("tenant-42"));
    }
}

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

struct Policy(Option<ActionDenial>);

impl ActionAuthorizer for Policy {
    fn authorize(&self, _: &ActionAuthzRequest<'_>) -> Result<(), ActionDenial> {
        self.0.clone().map_or(Ok(()), Err)
    }
}

/// The matrix: an invisible existing object and an absent object give the
/// identical refusal; a forbidden call on an absent object is not-found too;
/// permission is reported only for a visible target.
#[test]
fn existence_never_leaks_matrix() {
    let entity = fixture("ir/entity-v2/artifact-set/commerce-sales-order.entity.json");
    let types: Vec<ObjectTypeDef> =
        vec![serde_json::from_value(entity["payload"]["objectType"].clone()).unwrap()];
    let catalog = TypeCatalog::new(&types);
    let action: ActionRoot = serde_json::from_value(
        fixture("ir/commerce-sales-order-submit-invoice.action.json")["payload"].clone(),
    )
    .unwrap();
    let mut cell = Cell::new(|command: &Command<JsonObject>| command.payload.clone());
    cell.commit(CellCommit {
        writes: vec![FactWrite {
            key: FactKey::new("commerce.SalesOrder", "SO-HIDDEN"),
            expected: FactVersion::ABSENT,
            mutation: FactMutation::Put {
                values: serde_json::from_value(
                    json!({ "so_number": "SO-HIDDEN", "status": "approved" }),
                )
                .unwrap(),
            },
        }],
        ..CellCommit::empty()
    })
    .unwrap();
    let call = |so_number: &str| ActionCall {
        action: &action,
        action_artifact_id: "fixture.action",
        action_version: "0.1.0",
        scope: BTreeMap::from([
            ("orgId".to_owned(), "org_demo".to_owned()),
            ("environmentId".to_owned(), "dev".to_owned()),
        ]),
        actor: "agent:outsider".to_owned(),
        input: JsonObject::from([(
            "so_number".to_owned(),
            JsonValue::String(so_number.to_owned()),
        )]),
        idempotency_key: "k".to_owned(),
        object_type: None,
        turn: DecisionTurn {
            id: "t".to_owned(),
            now: LogicalTime::from_millis(0),
            world: World::Main,
            consumes: None,
        },
    };
    let invisible = Policy(Some(ActionDenial::Invisible));
    let forbidden = Policy(Some(ActionDenial::Forbidden {
        reason: "cedar_deny".to_owned(),
    }));
    let allow = Policy(None);
    let mut refuse = |so_number: &str, policy: &Policy| {
        commit_action(&mut cell, &catalog, &call(so_number), policy)
            .unwrap_err()
            .refusal()
    };
    let hidden = refuse("SO-HIDDEN", &invisible);
    let absent = refuse("SO-MISSING", &allow);
    let absent_forbidden = refuse("SO-MISSING", &forbidden);
    assert_eq!(
        hidden, absent,
        "an invisible object must read exactly like an absent one"
    );
    assert_eq!(absent, absent_forbidden);
    assert_eq!(hidden.code, "action.target_not_found");
    assert_eq!(hidden.category, RefusalCategory::Visibility);
    assert_eq!(hidden.message, NOT_FOUND_MESSAGE);
    let denied = refuse("SO-HIDDEN", &forbidden);
    assert_eq!(denied.category, RefusalCategory::Permission);
    assert_eq!(denied.code, "action.permission_denied");
    assert_eq!(cell.ledger().len(), 1, "no refusal committed anything");
}
