#![cfg(feature = "native")]

//! GRAPH-STATE-1 at the shared canonical commit boundary, through the
//! production `SqliteCell` host: the first write of a lifecycle-bearing
//! object begins every effective lifecycle (inherited ones included) in an
//! admitted initial state, or reaches its written state only through the
//! transitions the same commit records, replayed in order from
//! initialization. A refused first write leaves no effect: no row, no
//! version, no ledger entry.

mod common;

use norn_cell::SqliteCell;
use norn_ir::{DecisionOutput, FactChange, ObjectTypeName, PropertyId, TransitionId};
use norn_kernel::cell_host::JsonObject;
use norn_kernel::snapshot_observe::{
    ObservationSynthesis, ObservedSnapshot, prepare_observed_snapshot,
};
use norn_kernel::{
    CellCommit, CellHost, CommitError, CommitModel, CommitView, DecisionTurn, EnvelopeLimits,
    FactEvent, FactKey, FactMutation, FactVersion, FactWrite, LifecycleInitRefusal, LogicalTime,
    ObservationAuthorization, ObservationEvent, World, check_commit, commit_decision,
    decision_cell_commit_with_host, load_slice,
};
use serde_json::json;
use std::convert::Infallible;

type Cell = SqliteCell<FactEvent, JsonObject, JsonObject>;
type ObservedCell = SqliteCell<ObservationEvent, JsonObject, JsonObject>;
type Commit = CellCommit<FactEvent, JsonObject, JsonObject>;

const SCOPE: &str = "org_demo/dev";

fn cell() -> Cell {
    open()
}

#[allow(
    clippy::expect_used,
    reason = "test fixture: a failed setup step fails the test"
)]
/// A domain-neutral model: guarded lifecycles ([`guarded_types`]), an
/// inherited one ([`hierarchy_types`]), and guards over temporal and
/// rules-only inputs ([`replay_types`]).
fn open<E>() -> SqliteCell<E, JsonObject, JsonObject> {
    let types = guarded_types()
        .into_iter()
        .chain(hierarchy_types())
        .chain(replay_types())
        .map(|value| serde_json::from_value(value).expect("definition decodes"))
        .collect::<Vec<norn_ir::ObjectTypeDef>>();
    SqliteCell::open_in_memory(
        common::model_of(&types),
        SCOPE,
        |command: &norn_kernel::Command<JsonObject>| command.payload.clone(),
    )
    .expect("opens")
}

/// Lifecycles whose transitions are guarded.
fn guarded_types() -> [serde_json::Value; 2] {
    [
        json!({
            "name": "test.Document",
            "properties": [
                { "id": "status", "default": "draft", "type": { "kind": "enum", "values": [
                    { "value": "draft" }, { "value": "submitted" }, { "value": "published" }
                ] } },
                { "id": "pages", "type": { "kind": "integer" } }
            ],
            "lifecycles": [{
                "property": "status",
                "initial": ["draft"],
                "terminal": ["published"],
                "transitions": [
                    { "id": "submit", "from": ["draft"], "to": "submitted",
                      "guards": ["self.pages > 0"] },
                    { "id": "publish", "from": ["submitted"], "to": "published" }
                ]
            }]
        }),
        json!({
            "name": "test.Batch",
            "properties": [
                { "id": "state", "default": "open", "type": { "kind": "enum", "values": [
                    { "value": "open" }, { "value": "sealed" }
                ] } },
                { "id": "size", "default": "1", "type": { "kind": "integer" } }
            ],
            "lifecycles": [{
                "property": "state",
                "initial": ["open"],
                "terminal": ["sealed"],
                "transitions": [{ "id": "seal", "from": ["open"], "to": "sealed",
                                  "guards": ["self.size > 0"] }]
            }]
        }),
    ]
}

/// An inherited lifecycle beside the subtype's own, a sibling subtype that
/// adds none, and a lifecycle with no default state.
fn hierarchy_types() -> [serde_json::Value; 4] {
    [
        json!({ "name": "test.Memo", "extends": "test.Record" }),
        json!({
            "name": "test.Record",
            "properties": [
                { "id": "phase", "default": "open", "type": { "kind": "enum", "values": [
                    { "value": "open" }, { "value": "closed" }
                ] } }
            ],
            "lifecycles": [{
                "property": "phase",
                "initial": ["open"],
                "terminal": ["closed"],
                "transitions": [{ "id": "close", "from": ["open"], "to": "closed" }]
            }]
        }),
        json!({
            "name": "test.Report",
            "extends": "test.Record",
            "properties": [
                { "id": "review", "default": "pending", "type": { "kind": "enum", "values": [
                    { "value": "pending" }, { "value": "approved" }
                ] } }
            ],
            "lifecycles": [{
                "property": "review",
                "initial": ["pending"],
                "terminal": ["approved"],
                "transitions": [{ "id": "approve", "from": ["pending"], "to": "approved" }]
            }]
        }),
        json!({
            "name": "test.Ticket",
            "properties": [
                { "id": "state", "type": { "kind": "enum", "values": [
                    { "value": "new" }, { "value": "done" }
                ] } }
            ],
            "lifecycles": [{
                "property": "state",
                "initial": ["new"],
                "terminal": ["done"],
                "transitions": [{ "id": "finish", "from": ["new"], "to": "done" }]
            }]
        }),
    ]
}

/// A rules-only lifecycle, and guards over a temporal comparison and over a
/// rules-only property.
fn replay_types() -> [serde_json::Value; 3] {
    [
        json!({
            "name": "test.Grant",
            "properties": [
                { "id": "state", "protected": true, "default": "open",
                  "type": { "kind": "enum", "values": [
                    { "value": "open" }, { "value": "closed" }
                ] } }
            ],
            "lifecycles": [{
                "property": "state",
                "initial": ["open"],
                "terminal": ["closed"],
                "transitions": [{ "id": "close", "from": ["open"], "to": "closed" }]
            }]
        }),
        json!({
            "name": "test.Window",
            "properties": [
                { "id": "status", "default": "draft", "type": { "kind": "enum", "values": [
                    { "value": "draft" }, { "value": "accepted" }
                ] } },
                { "id": "a", "type": { "kind": "timestamp", "timezone": "offset" } },
                { "id": "b", "type": { "kind": "timestamp", "timezone": "offset" } }
            ],
            "lifecycles": [{
                "property": "status",
                "initial": ["draft"],
                "terminal": ["accepted"],
                "transitions": [{ "id": "accept", "from": ["draft"], "to": "accepted",
                                  "guards": ["self.a != self.b"] }]
            }]
        }),
        json!({
            "name": "test.Claim",
            "properties": [
                { "id": "status", "default": "draft", "type": { "kind": "enum", "values": [
                    { "value": "draft" }, { "value": "granted" }
                ] } },
                { "id": "verified", "protected": true, "default": false,
                  "type": { "kind": "boolean" } }
            ],
            "lifecycles": [{
                "property": "status",
                "initial": ["draft"],
                "terminal": ["granted"],
                "transitions": [{ "id": "grant", "from": ["draft"], "to": "granted",
                                  "guards": ["self.verified == true"] }]
            }]
        }),
    ]
}

#[allow(
    clippy::expect_used,
    reason = "test fixture: a failed setup step fails the test"
)]
fn values(value: serde_json::Value) -> JsonObject {
    serde_json::from_value(value).expect("values")
}

fn document() -> FactKey {
    FactKey::new("test.Document", "D-1")
}

/// A first write of `key` with `values` and the recorded `events`.
fn put<E>(
    key: &FactKey,
    values: JsonObject,
    events: Vec<E>,
) -> CellCommit<E, JsonObject, JsonObject> {
    CellCommit {
        writes: vec![FactWrite {
            key: key.clone(),
            expected: FactVersion::ABSENT,
            mutation: FactMutation::Put { values },
        }],
        events,
        ..CellCommit::empty()
    }
}

#[allow(
    clippy::expect_used,
    reason = "test fixture: a failed setup step fails the test"
)]
/// Everything a refusal must leave untouched: the verified ledger head, the
/// stored rows, and the object's absence (no burned version).
fn snapshot<E: serde::Serialize + 'static>(
    cell: &SqliteCell<E, JsonObject, JsonObject>,
    key: &FactKey,
) -> (String, String, FactVersion) {
    let head = cell.store().verified_head().expect("verified head");
    let dump = cell.store().read().dump().expect("dump");
    let state = cell.load(key).expect("loads");
    assert!(state.values.is_none(), "{key} must not exist");
    (format!("{head:?}"), format!("{dump:?}"), state.version)
}

/// Whether `refusal` is an unrecorded change whose reason contains `reason`,
/// so a test cannot pass on an earlier, unrelated refusal.
fn unrecorded_because(refusal: &LifecycleInitRefusal, reason: &str) -> bool {
    matches!(refusal, LifecycleInitRefusal::UnrecordedChange { reason: actual } if actual.contains(reason))
}

#[allow(
    clippy::panic,
    reason = "test fixture: a failed setup step fails the test"
)]
fn refusal(error: CommitError<norn_cell::CellError>) -> (FactKey, LifecycleInitRefusal) {
    match error {
        CommitError::LifecycleInitialization { key, refusal } => (key, *refusal),
        other => panic!("expected a lifecycle initialization refusal, got {other:?}"),
    }
}

fn turn(id: &str) -> DecisionTurn {
    DecisionTurn {
        id: id.to_owned(),
        now: LogicalTime::from_millis(1),
        world: World::Main,
        consumes: None,
    }
}

/// Create a document with `pages`, then submit and publish it, as one decision.
fn create_and_publish(pages: u64) -> DecisionOutput {
    let key = document();
    let transition = |id: &str| FactChange::Transition {
        object_type: ObjectTypeName(key.object_type.as_str().into()),
        key: key.key.clone(),
        lifecycle: PropertyId("status".into()),
        transition: TransitionId(id.into()),
    };
    DecisionOutput {
        facts: vec![
            FactChange::Create {
                object_type: ObjectTypeName(key.object_type.as_str().into()),
                key: key.key.clone(),
                values: values(json!({ "pages": pages })),
            },
            transition("submit"),
            transition("publish"),
        ],
        links: vec![],
        commands: vec![],
        timers: vec![],
    }
}

#[allow(
    clippy::expect_used,
    reason = "test fixture: a failed setup step fails the test"
)]
/// The kernel-prepared commit of [`create_and_publish`] on `cell`.
fn prepared_create_and_publish(cell: &Cell, pages: u64) -> Commit {
    let model = cell.store().model().clone();
    let catalog = model.type_catalog();
    let slice = load_slice(cell, [document()]).expect("slice");
    decision_cell_commit_with_host(
        cell,
        &catalog,
        &slice,
        &turn("prepare"),
        &create_and_publish(pages),
        &EnvelopeLimits::V1,
    )
    .expect("a legal create-and-transition prepares")
}

#[allow(
    clippy::expect_used,
    reason = "test fixture: a failed setup step fails the test"
)]
fn status(cell: &Cell, key: &FactKey) -> Option<String> {
    cell.load(key).expect("loads").values.and_then(|values| {
        values
            .get("status")
            .and_then(|v| v.as_str().map(str::to_owned))
    })
}

#[test]
fn a_first_put_in_a_non_initial_state_is_refused_with_no_effects() {
    let mut cell = cell();
    let key = document();
    let before = snapshot(&cell, &key);
    let error = cell
        .commit(put(
            &key,
            values(json!({ "status": "published", "pages": 3 })),
            vec![],
        ))
        .expect_err("a document cannot begin published");
    assert_eq!(error.code(), "lifecycle.validation.undeclared_write");
    assert_eq!(
        refusal(error),
        (
            key.clone(),
            LifecycleInitRefusal::InvalidInitialState {
                lifecycle: "status".into(),
                state: Some("published".into()),
                initial: vec!["draft".into()],
            }
        )
    );
    assert_eq!(snapshot(&cell, &key), before);
}

#[test]
fn a_first_put_in_an_initial_state_or_its_default_is_admitted() {
    let mut cell = cell();
    let explicit = document();
    cell.commit(put(
        &explicit,
        values(json!({ "status": "draft", "pages": 1 })),
        vec![],
    ))
    .expect("an explicit initial state is admitted");
    assert_eq!(status(&cell, &explicit).as_deref(), Some("draft"));

    let defaulted = FactKey::new("test.Document", "D-2");
    cell.commit(put(&defaulted, values(json!({ "pages": 1 })), vec![]))
        .expect("an omitted lifecycle takes its initial default");
    assert_eq!(status(&cell, &defaulted).as_deref(), Some("draft"));

    // Updates of existing objects are outside initialization.
    cell.commit(CellCommit {
        writes: vec![FactWrite {
            key: explicit,
            expected: FactVersion(1),
            mutation: FactMutation::Put {
                values: values(json!({ "status": "draft", "pages": 2 })),
            },
        }],
        ..CellCommit::empty()
    })
    .expect("an existing object is not re-initialized");
}

#[test]
fn a_lifecycle_with_no_default_must_be_written_in_an_initial_state() {
    let mut cell = cell();
    let key = FactKey::new("test.Ticket", "T-1");
    let before = snapshot(&cell, &key);
    let (_, unset) = refusal(
        cell.commit(put(&key, JsonObject::new(), vec![]))
            .expect_err("no state and no default"),
    );
    assert_eq!(
        unset,
        LifecycleInitRefusal::InvalidInitialState {
            lifecycle: "state".into(),
            state: None,
            initial: vec!["new".into()],
        }
    );
    refusal(
        cell.commit(put(&key, values(json!({ "state": "done" })), vec![]))
            .expect_err("done is not initial"),
    );
    assert_eq!(snapshot(&cell, &key), before);
    cell.commit(put(&key, values(json!({ "state": "new" })), vec![]))
        .expect("new is initial");
}

#[test]
fn inherited_lifecycles_must_begin_in_their_initial_states() {
    let mut cell = cell();
    let key = FactKey::new("test.Report", "R-1");
    let before = snapshot(&cell, &key);
    let (refused, inherited) = refusal(
        cell.commit(put(
            &key,
            values(json!({ "phase": "closed", "review": "pending" })),
            vec![],
        ))
        .expect_err("the inherited lifecycle cannot begin closed"),
    );
    assert_eq!(refused, key);
    assert_eq!(
        inherited,
        LifecycleInitRefusal::InvalidInitialState {
            lifecycle: "phase".into(),
            state: Some("closed".into()),
            initial: vec!["open".into()],
        }
    );
    let (_, own) = refusal(
        cell.commit(put(
            &key,
            values(json!({ "phase": "open", "review": "approved" })),
            vec![],
        ))
        .expect_err("the subtype's own lifecycle cannot begin approved"),
    );
    assert!(matches!(
        own,
        LifecycleInitRefusal::InvalidInitialState { ref lifecycle, .. } if lifecycle == "review"
    ));
    assert_eq!(snapshot(&cell, &key), before);
    cell.commit(put(&key, JsonObject::new(), vec![]))
        .expect("both lifecycles take their initial defaults");
}

#[test]
fn a_legal_create_and_transition_in_one_commit_is_admitted() {
    let mut cell = cell();
    let model = cell.store().model().clone();
    let catalog = model.type_catalog();
    let slice = load_slice(&cell, [document()]).expect("slice");
    commit_decision(
        &mut cell,
        &catalog,
        &slice,
        &turn("create-and-publish"),
        &create_and_publish(3),
    )
    .expect("create, submit and publish in one decision");
    assert_eq!(status(&cell, &document()).as_deref(), Some("published"));

    // The same legal sequence submitted directly is replayed and admitted.
    let mut direct = self::cell();
    let prepared = prepared_create_and_publish(&direct, 3);
    direct
        .commit(prepared)
        .expect("a legal recorded sequence is admitted on the direct path");
    assert_eq!(status(&direct, &document()).as_deref(), Some("published"));
}

#[test]
fn a_guard_that_fails_at_creation_refuses_the_compound_decision() {
    let mut cell = cell();
    let key = document();
    let before = snapshot(&cell, &key);
    let model = cell.store().model().clone();
    let catalog = model.type_catalog();
    let slice = load_slice(&cell, [key.clone()]).expect("slice");
    commit_decision(
        &mut cell,
        &catalog,
        &slice,
        &turn("empty"),
        &create_and_publish(0),
    )
    .expect_err("submit's guard does not hold for an empty document");
    assert_eq!(snapshot(&cell, &key), before);
}

#[test]
fn stripped_or_skipped_transitions_are_refused_with_no_effects() {
    let mut cell = cell();
    let key = document();
    let before = snapshot(&cell, &key);
    let legal = prepared_create_and_publish(&cell, 3);
    assert!(
        legal
            .events
            .iter()
            .any(|event| matches!(event, FactEvent::Transitioned { .. })),
        "the kernel records its transitions"
    );

    // The final row alone: without its transitions it is a forged state.
    let mut stripped = legal.clone();
    stripped
        .events
        .retain(|event| !matches!(event, FactEvent::Transitioned { .. }));
    let (_, stripped) = refusal(cell.commit(stripped).expect_err("no recorded path"));
    assert!(
        unrecorded_because(
            &stripped,
            "the written values differ from the values its recorded changes produce"
        ),
        "{stripped:?}"
    );

    // A skipped step: publish straight from draft is undeclared.
    let mut skipped = legal;
    skipped.events.retain(|event| {
        !matches!(event, FactEvent::Transitioned { transition, .. } if transition.as_str() == "submit")
    });
    let (_, skipped) = refusal(cell.commit(skipped).expect_err("draft cannot publish"));
    assert!(matches!(
        skipped,
        LifecycleInitRefusal::IllegalTransition { ref transition, .. } if transition == "publish"
    ));
    assert_eq!(snapshot(&cell, &key), before);
}

/// `edit` applied to every recorded `Created` of `commit`.
fn edit_created(commit: &mut Commit, edit: impl Fn(&mut JsonObject)) {
    for event in &mut commit.events {
        if let FactEvent::Created { values, .. } = event {
            edit(values);
        }
    }
}

#[test]
fn forged_creations_and_unrecorded_writes_are_refused_with_no_effects() {
    let mut cell = cell();
    let key = document();
    let before = snapshot(&cell, &key);
    let legal = prepared_create_and_publish(&cell, 3);
    let int = |value: i64| serde_json::from_value(json!(value)).expect("int");

    // The submit guard does not hold for the recorded creation.
    let mut guarded = legal.clone();
    edit_created(&mut guarded, |values| {
        values.insert("pages".into(), int(0));
    });
    let (_, guard) = refusal(cell.commit(guarded).expect_err("empty document submitted"));
    assert!(matches!(
        guard,
        LifecycleInitRefusal::IllegalTransition { ref transition, .. } if transition == "submit"
    ));

    // A recorded creation in a non-initial state.
    let mut created = legal.clone();
    edit_created(&mut created, |values| {
        values.insert("status".into(), "submitted".into());
    });
    let (_, initial) = refusal(cell.commit(created).expect_err("created submitted"));
    assert!(matches!(
        initial,
        LifecycleInitRefusal::InvalidInitialState { .. }
    ));

    // A written row its recorded changes never produced.
    let mut diverged = legal.clone();
    if let FactMutation::Put { values } = &mut diverged.writes[0].mutation {
        values.insert("pages".into(), int(9));
    }
    let (_, unrecorded) = refusal(cell.commit(diverged).expect_err("pages were never set"));
    assert!(
        unrecorded_because(
            &unrecorded,
            "the written values differ from the values its recorded changes produce"
        ),
        "{unrecorded:?}"
    );

    // A direct lifecycle set recorded as an ordinary property write, after a
    // recorded transition so the replay (not the final-row check) refuses it.
    let mut direct_set = legal;
    direct_set.events = vec![
        FactEvent::Created {
            key: key.clone(),
            values: values(json!({ "status": "draft", "pages": 3 })),
        },
        FactEvent::Transitioned {
            key: key.clone(),
            lifecycle: PropertyId("status".into()),
            transition: TransitionId("submit".into()),
            from: "draft".into(),
            to: "submitted".into(),
        },
        FactEvent::Set {
            key: key.clone(),
            property: PropertyId("status".into()),
            value: "published".into(),
        },
    ];
    let (_, set) = refusal(cell.commit(direct_set).expect_err("lifecycle set"));
    assert!(
        unrecorded_because(&set, "status is a lifecycle property"),
        "{set:?}"
    );
    assert_eq!(snapshot(&cell, &key), before);
}

/// Create `test.Batch` `B-1` recording `created`, then seal it.
fn seal_batch(created: JsonObject) -> Commit {
    let key = FactKey::new("test.Batch", "B-1");
    let events = vec![
        FactEvent::Created {
            key: key.clone(),
            values: created,
        },
        FactEvent::Transitioned {
            key: key.clone(),
            lifecycle: PropertyId("state".into()),
            transition: TransitionId("seal".into()),
            from: "open".into(),
            to: "sealed".into(),
        },
    ];
    put(
        &key,
        values(json!({ "state": "sealed", "size": 1 })),
        events,
    )
}

#[test]
fn replayed_guards_read_the_effective_created_values() {
    let mut cell = cell();
    let key = FactKey::new("test.Batch", "B-1");
    let before = snapshot(&cell, &key);
    // An explicit `size` of 0 fails the seal guard.
    let (_, zero) = refusal(
        cell.commit(seal_batch(values(json!({ "size": 0 }))))
            .expect_err("a zero-size batch cannot be sealed"),
    );
    assert!(matches!(
        zero,
        LifecycleInitRefusal::IllegalTransition { ref transition, .. } if transition == "seal"
    ));
    assert_eq!(snapshot(&cell, &key), before);
    // An omitted defaulted property is guarded on its effective default (1),
    // the same value the written row is compared with, as a decision would.
    cell.commit(seal_batch(JsonObject::new()))
        .expect("the guard reads the effective default");
    assert_eq!(
        cell.load(&key)
            .expect("loads")
            .values
            .and_then(|values| values.get("state").cloned()),
        Some(norn_ir::RuntimeValue::from("sealed"))
    );
}

#[test]
fn an_empty_observation_authorization_does_not_exempt_a_forged_first_write() {
    let mut cell: ObservedCell = open();
    let key = document();
    let before = snapshot(&cell, &key);
    let forged = put(
        &key,
        values(json!({ "status": "published", "pages": 1 })),
        Vec::new(),
    );
    let (refused, _) = refusal(
        cell.commit_observed(forged, ObservationAuthorization::empty())
            .expect_err("no sealed write covers the forged put"),
    );
    assert_eq!(refused, key);
    assert_eq!(snapshot(&cell, &key), before);
}

#[allow(
    clippy::expect_used,
    reason = "test fixture: a failed setup step fails the test"
)]
/// The synthetic source snapshot the observation fixtures cite.
fn source_provenance() -> norn_ir::provenance::InstanceProvenance {
    serde_json::from_value(json!({
        "source": { "system": "synthetic", "record": "document/D-1" },
        "mappingVersion": "fixture.document@1",
        "recordedAt": "2026-11-01T00:00:00Z",
        "sourceFields": ["pages"]
    }))
    .expect("source provenance")
}

#[allow(
    clippy::expect_used,
    reason = "test fixture: a failed setup step fails the test"
)]
/// A sealed observation that creates `D-1` with `pages`.
fn observe_document(
    cell: &ObservedCell,
) -> (
    CellCommit<ObservationEvent, JsonObject, JsonObject>,
    ObservationAuthorization,
) {
    let model = cell.store().model().clone();
    let catalog = model.type_catalog();
    let key = document();
    let slice = load_slice(cell, [key.clone()]).expect("slice");
    let provenance = source_provenance();
    prepare_observed_snapshot(
        &catalog,
        &slice,
        &turn("observe"),
        ObservedSnapshot {
            key: &key,
            values: &values(json!({ "pages": 2 })),
            lifecycles: &[],
            provenance: &provenance,
        },
    )
    .expect("sealed observed write")
    .into_parts()
}

#[test]
fn a_sealed_observation_exempts_only_its_own_writes() {
    let mut cell: ObservedCell = open();
    let observed = document();
    let forged = FactKey::new("test.Document", "D-2");
    let before = (snapshot(&cell, &observed), snapshot(&cell, &forged));

    let (mut commit, authorization) = observe_document(&cell);
    commit.writes.push(FactWrite {
        key: forged.clone(),
        expected: FactVersion::ABSENT,
        mutation: FactMutation::Put {
            values: values(json!({ "status": "published", "pages": 1 })),
        },
    });
    let (refused, _) = refusal(
        cell.commit_observed(commit, authorization)
            .expect_err("the unsealed write is an ordinary first write"),
    );
    assert_eq!(refused, forged);
    assert_eq!(
        (snapshot(&cell, &observed), snapshot(&cell, &forged)),
        before
    );

    // The sealed write alone is admitted under its own contract.
    let (commit, authorization) = observe_document(&cell);
    cell.commit_observed(commit, authorization)
        .expect("the sealed observation commits");
    assert!(cell.load(&observed).expect("loads").values.is_some());
}

#[test]
fn concurrent_first_writes_admit_one_legal_creation_and_leave_no_partial_effects() {
    let mut cell = cell();
    let key = document();
    let before = snapshot(&cell, &key);

    // Two turns read the same absent pre-state. The forged one commits first
    // and is refused without effect or a burned version, so the legal one
    // still creates the object at version 1.
    let forged = put(
        &key,
        values(json!({ "status": "published", "pages": 1 })),
        vec![],
    );
    let legal = put(
        &key,
        values(json!({ "status": "draft", "pages": 1 })),
        vec![],
    );
    let racing = put(
        &key,
        values(json!({ "status": "draft", "pages": 2 })),
        vec![],
    );
    refusal(cell.commit(forged.clone()).expect_err("forged first write"));
    assert_eq!(snapshot(&cell, &key), before);
    let receipt = cell.commit(legal).expect("the legal first write wins");
    assert_eq!(receipt.written[0].version, FactVersion(1));

    // A second first write against the same absent pre-state is stale.
    assert!(matches!(
        cell.commit(racing).expect_err("the object now exists"),
        CommitError::Conflict { .. }
    ));
    // A forged first write that lost the race is stale too, never applied.
    assert!(matches!(
        cell.commit(forged).expect_err("the object now exists"),
        CommitError::Conflict { .. }
    ));
    assert_eq!(status(&cell, &key).as_deref(), Some("draft"));
    assert_eq!(cell.load(&key).expect("loads").version, FactVersion(1));
}

/// A first `Put` of `key` with `written`, recording `events` for it.
fn evidence(key: &FactKey, written: serde_json::Value, events: Vec<FactEvent>) -> Commit {
    put(key, values(written), events)
}

fn created(key: &FactKey, created: serde_json::Value) -> FactEvent {
    FactEvent::Created {
        key: key.clone(),
        values: values(created),
    }
}

#[allow(
    clippy::expect_used,
    reason = "test fixture: a failed setup step fails the test"
)]
fn set(key: &FactKey, property: &str, value: serde_json::Value) -> FactEvent {
    FactEvent::Set {
        key: key.clone(),
        property: PropertyId(property.into()),
        value: serde_json::from_value(value).expect("value"),
    }
}

fn fired(key: &FactKey, lifecycle: &str, transition: &str, from: &str, to: &str) -> FactEvent {
    FactEvent::Transitioned {
        key: key.clone(),
        lifecycle: PropertyId(lifecycle.into()),
        transition: TransitionId(transition.into()),
        from: from.into(),
        to: to.into(),
    }
}

#[test]
fn a_replayed_temporal_set_is_canonical_before_the_next_guard() {
    let mut cell = cell();
    let key = FactKey::new("test.Window", "W-1");
    let before = snapshot(&cell, &key);
    // `a` is set to the same instant as `b` in another offset: the guard
    // `self.a != self.b` must read the canonical instant and refuse.
    let commit = evidence(
        &key,
        json!({ "status": "accepted", "a": "2024-02-29T10:00:00Z", "b": "2024-02-29T10:00:00Z" }),
        vec![
            created(
                &key,
                json!({ "a": "2024-02-29T09:00:00Z", "b": "2024-02-29T10:00:00Z" }),
            ),
            set(&key, "a", json!("2024-02-29T12:00:00+02:00")),
            fired(&key, "status", "accept", "draft", "accepted"),
        ],
    );
    let (_, guard) = refusal(cell.commit(commit).expect_err("equal instants"));
    assert!(
        matches!(
            &guard,
            LifecycleInitRefusal::IllegalTransition { transition, reason, .. }
                if transition == "accept" && reason.contains("does not hold")
        ),
        "{guard:?}"
    );
    assert_eq!(snapshot(&cell, &key), before);
    // Positive control: unequal instants satisfy the guard.
    let unequal = FactKey::new("test.Window", "W-2");
    cell.commit(evidence(
        &unequal,
        json!({ "status": "accepted", "a": "2024-02-29T09:00:00Z", "b": "2024-02-29T10:00:00Z" }),
        vec![
            created(
                &unequal,
                json!({ "a": "2024-02-29T09:00:00Z", "b": "2024-02-29T10:00:00Z" }),
            ),
            fired(&unequal, "status", "accept", "draft", "accepted"),
        ],
    ))
    .expect("unequal instants accept");
}

#[test]
fn a_replayed_set_cannot_write_a_rules_only_guard_input() {
    let mut cell = cell();
    let key = FactKey::new("test.Claim", "C-1");
    let before = snapshot(&cell, &key);
    // Set the rules-only input for the guard, fire, then restore it so the
    // final row matches: the transient write itself is refused.
    let commit = evidence(
        &key,
        json!({ "status": "granted", "verified": false }),
        vec![
            created(&key, json!({})),
            set(&key, "verified", json!(true)),
            fired(&key, "status", "grant", "draft", "granted"),
            set(&key, "verified", json!(false)),
        ],
    );
    let (_, rules) = refusal(cell.commit(commit).expect_err("rules-only write"));
    assert!(
        unrecorded_because(&rules, "verified is writable only by rules"),
        "{rules:?}"
    );
    assert_eq!(snapshot(&cell, &key), before);
}

#[test]
fn a_sibling_subtypes_evidence_cannot_justify_another_subtype() {
    let mut cell = cell();
    // `test.Memo` and `test.Report` share the root `test.Record` and the key.
    let written = FactKey::new("test.Memo", "R-9");
    let sibling = FactKey::new("test.Report", "R-9");
    let before = snapshot(&cell, &written);
    let commit = evidence(
        &written,
        json!({ "phase": "closed" }),
        vec![
            created(&sibling, json!({})),
            fired(&sibling, "phase", "close", "open", "closed"),
        ],
    );
    let (refused, spoofed) = refusal(cell.commit(commit).expect_err("sibling evidence"));
    assert_eq!(refused, written);
    assert!(
        unrecorded_because(
            &spoofed,
            "the recorded evidence names another type than the written object"
        ),
        "{spoofed:?}"
    );
    assert_eq!(snapshot(&cell, &written), before);
    // The same evidence under the written type is a legal sequence.
    cell.commit(evidence(
        &written,
        json!({ "phase": "closed" }),
        vec![
            created(&written, json!({})),
            fired(&written, "phase", "close", "open", "closed"),
        ],
    ))
    .expect("the memo's own creation and close");
}

#[test]
fn contradictory_evidence_is_refused_even_when_the_row_ends_initial() {
    let mut cell = cell();
    let key = document();
    let before = snapshot(&cell, &key);
    let written = json!({ "status": "draft", "pages": 1 });
    // A recorded creation in a non-initial state.
    let (_, created_late) = refusal(
        cell.commit(evidence(
            &key,
            written.clone(),
            vec![created(&key, json!({ "status": "published", "pages": 1 }))],
        ))
        .expect_err("created published"),
    );
    assert!(matches!(
        created_late,
        LifecycleInitRefusal::InvalidInitialState { .. }
    ));
    // A direct lifecycle set that ends back in the initial state.
    let (_, direct) = refusal(
        cell.commit(evidence(
            &key,
            written.clone(),
            vec![
                created(&key, json!({ "pages": 1 })),
                set(&key, "status", json!("published")),
                set(&key, "status", json!("draft")),
            ],
        ))
        .expect_err("direct lifecycle sets"),
    );
    assert!(
        unrecorded_because(
            &direct,
            "status is a lifecycle property and changes only through a declared transition"
        ),
        "{direct:?}"
    );
    assert_eq!(snapshot(&cell, &key), before);
    // Consistent evidence ending in the initial state is admitted.
    cell.commit(evidence(
        &key,
        written,
        vec![created(&key, json!({ "pages": 1 }))],
    ))
    .expect("a recorded creation in its initial state");
}

#[test]
fn a_replayed_transition_of_a_rules_only_lifecycle_is_refused() {
    let mut cell = cell();
    let key = FactKey::new("test.Grant", "G-1");
    let before = snapshot(&cell, &key);
    let commit = evidence(
        &key,
        json!({ "state": "closed" }),
        vec![
            created(&key, json!({})),
            fired(&key, "state", "close", "open", "closed"),
        ],
    );
    let (_, rules) = refusal(cell.commit(commit).expect_err("rules-only lifecycle"));
    assert!(
        matches!(
            &rules,
            LifecycleInitRefusal::IllegalTransition { transition, reason, .. }
                if transition == "close" && reason == "the lifecycle is writable only by rules"
        ),
        "{rules:?}"
    );
    assert_eq!(snapshot(&cell, &key), before);
}

#[test]
fn a_created_event_cannot_supply_a_rules_only_property() {
    let mut cell = cell();
    let key = FactKey::new("test.Claim", "C-2");
    let before = snapshot(&cell, &key);
    let commit = evidence(
        &key,
        json!({ "status": "granted", "verified": true }),
        vec![
            created(&key, json!({ "verified": true })),
            fired(&key, "status", "grant", "draft", "granted"),
        ],
    );
    let (_, supplied) = refusal(
        cell.commit(commit)
            .expect_err("caller-supplied rules-only input"),
    );
    assert!(
        unrecorded_because(&supplied, "verified is writable only by rules"),
        "{supplied:?}"
    );
    assert_eq!(snapshot(&cell, &key), before);
    // A kernel creation already holds the canonical default: admitted.
    cell.commit(evidence(
        &key,
        json!({ "status": "draft", "verified": false }),
        vec![created(
            &key,
            json!({ "status": "draft", "verified": false }),
        )],
    ))
    .expect("a recorded creation with the materialized default");
}

fn deleted(key: &FactKey) -> FactEvent {
    FactEvent::Deleted { key: key.clone() }
}

#[test]
fn a_replayed_deletion_follows_the_decision_deletion_rule() {
    let mut cell = cell();
    let grant = FactKey::new("test.Grant", "G-2");
    let before = snapshot(&cell, &grant);
    let commit = evidence(
        &grant,
        json!({ "state": "open" }),
        vec![
            created(&grant, json!({})),
            deleted(&grant),
            created(&grant, json!({})),
        ],
    );
    let (_, protected) = refusal(cell.commit(commit).expect_err("rules-only deletion"));
    assert!(
        unrecorded_because(
            &protected,
            "an object with rules-only properties cannot be deleted"
        ),
        "{protected:?}"
    );
    assert_eq!(snapshot(&cell, &grant), before);
    // An unprotected type may be created, deleted and created again. The
    // SQLite ledger proves each `Created` against its own stored emission,
    // so this sequence is checked against the shared admission directly.
    let model = cell.store().model().clone();
    let catalog = model.type_catalog();
    let key = document();
    let commit = evidence(
        &key,
        json!({ "status": "draft", "pages": 1 }),
        vec![
            created(&key, json!({ "pages": 2 })),
            deleted(&key),
            created(&key, json!({ "pages": 1 })),
        ],
    );
    check_commit(&commit, &CommitModel::new(&catalog), &EmptyView)
        .expect("create, delete and create again");
}

/// A pre-state with no objects, links or deliveries.
struct EmptyView;

impl CommitView for EmptyView {
    type Error = Infallible;

    fn pending(&self, _: &norn_kernel::Consumed) -> Result<bool, Infallible> {
        Ok(false)
    }

    fn version(&self, _: &FactKey) -> Result<FactVersion, Infallible> {
        Ok(FactVersion::ABSENT)
    }

    fn concrete(&self, _: &FactKey) -> Result<Option<ObjectTypeName>, Infallible> {
        Ok(None)
    }

    fn link_version(&self, _: &norn_kernel::LinkKey) -> Result<FactVersion, Infallible> {
        Ok(FactVersion::ABSENT)
    }

    fn links_of(&self, _: &FactKey) -> Result<Vec<norn_kernel::LinkKey>, Infallible> {
        Ok(Vec::new())
    }
}

fn modified(key: &FactKey, before: serde_json::Value, after: serde_json::Value) -> FactEvent {
    FactEvent::Modified {
        key: key.clone(),
        before_version: FactVersion(1),
        after_version: FactVersion(2),
        before: values(before),
        after: values(after),
        changed_properties: vec![PropertyId("pages".into())],
    }
}

#[test]
fn a_modification_aggregate_cannot_describe_a_first_write() {
    let mut cell = cell();
    let existing = FactKey::new("test.Document", "D-0");
    let stored = json!({ "status": "draft", "pages": 1 });
    let updated = json!({ "status": "draft", "pages": 2 });
    cell.commit(put(&existing, values(stored.clone()), vec![]))
        .expect("an existing document");
    let fresh = document();
    let before = snapshot(&cell, &fresh);
    let commit = evidence(
        &fresh,
        json!({ "status": "draft", "pages": 1 }),
        vec![modified(
            &fresh,
            json!({ "pages": 1 }),
            json!({ "pages": 2 }),
        )],
    );
    let (_, aggregate) = refusal(cell.commit(commit).expect_err("aggregate of a new object"));
    assert!(
        unrecorded_because(
            &aggregate,
            "a modification aggregate names an object the commit creates"
        ),
        "{aggregate:?}"
    );
    assert_eq!(snapshot(&cell, &fresh), before);
    // An aggregate of an existing object beside a first write is unaffected.
    // The host proves it against the stored and written snapshots.
    let mut commit = put(
        &FactKey::new("test.Document", "D-2"),
        values(json!({ "pages": 1 })),
        vec![modified(&existing, stored, updated.clone())],
    );
    commit.writes.push(FactWrite {
        key: existing,
        expected: FactVersion(1),
        mutation: FactMutation::Put {
            values: values(updated),
        },
    });
    cell.commit(commit)
        .expect("the existing object's aggregate");
}

#[test]
fn wrapped_observed_fact_evidence_is_validated_for_an_unsealed_write() {
    let mut cell: ObservedCell = open();
    let key = document();
    let before = snapshot(&cell, &key);
    let provenance = source_provenance();
    let commit = put(
        &key,
        values(json!({ "status": "draft", "pages": 1 })),
        vec![ObservationEvent::ObservedFact {
            event: created(&key, json!({ "status": "published", "pages": 1 })),
            provenance,
        }],
    );
    let (_, wrapped) = refusal(
        cell.commit(commit)
            .expect_err("contradictory wrapped evidence"),
    );
    assert_eq!(
        wrapped,
        LifecycleInitRefusal::InvalidInitialState {
            lifecycle: "status".into(),
            state: Some("published".into()),
            initial: vec!["draft".into()],
        }
    );
    assert_eq!(snapshot(&cell, &key), before);
}

#[allow(
    clippy::expect_used,
    reason = "test fixture: a failed setup step fails the test"
)]
/// An unsealed first write of `D-1` in its initial state, carrying `event`:
/// refused for `reason` with no row, version or ledger entry.
fn assert_observed_history_refused(event: ObservationEvent, reason: &str) {
    let mut cell: ObservedCell = open();
    let key = document();
    let before = snapshot(&cell, &key);
    let commit = put(
        &key,
        values(json!({ "status": "draft", "pages": 1 })),
        vec![event],
    );
    let (refused, history) = refusal(cell.commit(commit).expect_err("observed history"));
    assert_eq!(refused, key);
    assert!(unrecorded_because(&history, reason), "{history:?}");
    assert_eq!(snapshot(&cell, &key), before);
}

#[test]
fn an_observed_transition_cannot_describe_an_unsealed_first_write() {
    assert_observed_history_refused(
        ObservationEvent::ObservedTransition {
            key: document(),
            property: PropertyId("status".into()),
            transition: TransitionId("submit".into()),
            from: "draft".into(),
            to: "submitted".into(),
            synthesized: ObservationSynthesis::SnapshotCatchUp,
            provenance: source_provenance(),
        },
        "an observed transition names an unsealed first write",
    );
}

#[test]
fn an_observed_deletion_cannot_describe_an_unsealed_first_write() {
    assert_observed_history_refused(
        ObservationEvent::ObservedDeleted {
            key: document(),
            tombstoned: true,
            provenance: source_provenance(),
        },
        "an observed deletion names an unsealed first write",
    );
}
