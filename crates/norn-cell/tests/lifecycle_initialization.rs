#![cfg(feature = "native")]
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    missing_docs,
    reason = "integration-test crate"
)]

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
use norn_kernel::snapshot_observe::{ObservedSnapshot, prepare_observed_snapshot};
use norn_kernel::{
    CellCommit, CellHost, CommitError, DecisionTurn, EnvelopeLimits, FactEvent, FactKey,
    FactMutation, FactVersion, FactWrite, LifecycleInitRefusal, LogicalTime,
    ObservationAuthorization, ObservationEvent, World, commit_decision,
    decision_cell_commit_with_host, load_slice,
};
use serde_json::json;

type Cell = SqliteCell<FactEvent, JsonObject, JsonObject>;
type ObservedCell = SqliteCell<ObservationEvent, JsonObject, JsonObject>;
type Commit = CellCommit<FactEvent, JsonObject, JsonObject>;

const SCOPE: &str = "org_demo/dev";

fn cell() -> Cell {
    open()
}

/// A domain-neutral model: a `test.Document` with a guarded lifecycle, a
/// `test.Report` that inherits `test.Record`'s lifecycle and adds its own, a
/// `test.Ticket` with no lifecycle default, and a `test.Batch` whose guard
/// reads a defaulted property.
fn open<E>() -> SqliteCell<E, JsonObject, JsonObject> {
    let types = guarded_types()
        .into_iter()
        .chain(hierarchy_types())
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

/// An inherited lifecycle beside the subtype's own, and a lifecycle with no
/// default state.
fn hierarchy_types() -> [serde_json::Value; 3] {
    [
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
            key: explicit.clone(),
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
    assert!(matches!(
        stripped,
        LifecycleInitRefusal::InvalidInitialState { .. }
    ));

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
    assert!(matches!(
        unrecorded,
        LifecycleInitRefusal::UnrecordedChange { .. }
    ));

    // A direct lifecycle set recorded as an ordinary property write.
    let mut direct_set = legal;
    direct_set.events = vec![
        FactEvent::Created {
            key: key.clone(),
            values: values(json!({ "status": "draft", "pages": 3 })),
        },
        FactEvent::Set {
            key: key.clone(),
            property: PropertyId("status".into()),
            value: "published".into(),
        },
    ];
    let (_, set) = refusal(cell.commit(direct_set).expect_err("lifecycle set"));
    assert!(matches!(set, LifecycleInitRefusal::UnrecordedChange { .. }));
    assert_eq!(snapshot(&cell, &key), before);
}

/// Create `test.Batch` `B-1` recording `created`, then seal it.
fn seal_batch(created: JsonObject) -> Commit {
    let key = FactKey::new("test.Batch", "B-1");
    put(
        &key,
        values(json!({ "state": "sealed", "size": 1 })),
        vec![
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
        ],
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
    let provenance = serde_json::from_value(json!({
        "source": { "system": "synthetic", "record": "document/D-1" },
        "mappingVersion": "fixture.document@1",
        "recordedAt": "2026-11-01T00:00:00Z",
        "sourceFields": ["pages"]
    }))
    .expect("source provenance");
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
