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
use norn_kernel::{
    CellCommit, CellHost, CommitError, DecisionTurn, EnvelopeLimits, FactEvent, FactKey,
    FactMutation, FactVersion, FactWrite, LifecycleInitRefusal, LogicalTime, World,
    commit_decision, decision_cell_commit_with_host, load_slice,
};
use serde_json::json;

type Cell = SqliteCell<FactEvent, JsonObject, JsonObject>;
type Commit = CellCommit<FactEvent, JsonObject, JsonObject>;

const SCOPE: &str = "org_demo/dev";

/// A domain-neutral model: a `test.Document` with a guarded lifecycle, and a
/// `test.Report` that inherits `test.Record`'s lifecycle and adds its own.
fn cell() -> Cell {
    let types: [norn_ir::ObjectTypeDef; 4] = [
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
    .map(|value| serde_json::from_value(value).expect("definition decodes"));
    SqliteCell::open_in_memory(
        common::model_of(&types),
        SCOPE,
        |command: &norn_kernel::Command<JsonObject>| command.payload.clone(),
    )
    .expect("opens")
}

fn values(value: serde_json::Value) -> JsonObject {
    serde_json::from_value(value).expect("values")
}

fn document() -> FactKey {
    FactKey::new("test.Document", "D-1")
}

/// A first write of `key` with `values` and the recorded `events`.
fn put(key: &FactKey, values: JsonObject, events: Vec<FactEvent>) -> Commit {
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
fn snapshot(cell: &Cell, key: &FactKey) -> (String, String, FactVersion) {
    let head = cell.store().verified_head().expect("verified head");
    let dump = cell.store().read().dump().expect("dump");
    let state = cell.load(key).expect("loads");
    assert!(state.values.is_none(), "{key} must not exist");
    (format!("{head:?}"), format!("{dump:?}"), state.version)
}

fn refusal(error: CommitError<norn_cell::CellError>) -> (FactKey, LifecycleInitRefusal) {
    match error {
        CommitError::LifecycleInitialization { key, refusal } => (key, refusal),
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
fn forged_transition_sequences_are_refused_with_no_effects() {
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
    let mut dropped = legal.clone();
    dropped
        .events
        .retain(|event| !matches!(event, FactEvent::Transitioned { .. }));
    let (_, stripped) = refusal(
        cell.commit(dropped)
            .expect_err("published with no recorded path"),
    );
    assert!(matches!(
        stripped,
        LifecycleInitRefusal::InvalidInitialState { .. }
    ));

    // A skipped step: publish straight from draft is undeclared.
    let mut skipped = legal.clone();
    skipped.events.retain(|event| {
        !matches!(event, FactEvent::Transitioned { transition, .. } if transition.as_str() == "submit")
    });
    let (_, skipped) = refusal(cell.commit(skipped).expect_err("draft cannot publish"));
    assert!(matches!(
        skipped,
        LifecycleInitRefusal::IllegalTransition { ref transition, .. } if transition == "publish"
    ));

    // A forged creation: the recorded values satisfy no guard the write
    // could have passed (an empty document cannot be submitted).
    let mut guarded = legal.clone();
    for event in &mut guarded.events {
        if let FactEvent::Created { values, .. } = event {
            values.insert(
                "pages".into(),
                serde_json::from_value(json!(0)).expect("int"),
            );
        }
    }
    let (_, guard) = refusal(
        cell.commit(guarded)
            .expect_err("the submit guard does not hold for the recorded creation"),
    );
    assert!(matches!(
        guard,
        LifecycleInitRefusal::IllegalTransition { ref transition, .. } if transition == "submit"
    ));

    // A recorded creation in a non-initial state.
    let mut created = legal.clone();
    for event in &mut created.events {
        if let FactEvent::Created { values, .. } = event {
            values.insert(
                "status".into(),
                serde_json::from_value(json!("submitted")).expect("state"),
            );
        }
    }
    let (_, initial) = refusal(cell.commit(created).expect_err("created submitted"));
    assert!(matches!(
        initial,
        LifecycleInitRefusal::InvalidInitialState { .. }
    ));

    // A written row its recorded changes never produced.
    let mut diverged = legal.clone();
    if let FactMutation::Put { values } = &mut diverged.writes[0].mutation {
        values.insert(
            "pages".into(),
            serde_json::from_value(json!(9)).expect("int"),
        );
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
            value: serde_json::from_value(json!("published")).expect("state"),
        },
        FactEvent::Transitioned {
            key: key.clone(),
            lifecycle: PropertyId("status".into()),
            transition: TransitionId("publish".into()),
            from: "submitted".into(),
            to: "published".into(),
        },
    ];
    let (_, set) = refusal(cell.commit(direct_set).expect_err("lifecycle set"));
    assert!(matches!(set, LifecycleInitRefusal::UnrecordedChange { .. }));

    assert_eq!(snapshot(&cell, &key), before);
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
