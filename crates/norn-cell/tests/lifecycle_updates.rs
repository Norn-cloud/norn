#![cfg(feature = "native")]

//! GRAPH-STATE-1 for objects that already exist (#1687), through the
//! production `SqliteCell` host: a direct `Put` changes a lifecycle state
//! only through the transitions the same commit records, replayed in order
//! from the stored row, and changes nothing else in an immutable state. A
//! refused update leaves no effect: no row, no version, no ledger entry.
//!
//! The same per-write boundary governs a sealed observation (#1752): its
//! authorization exempts exactly the writes it prepared, in every admission
//! stage, so an unsealed write riding an observed commit can neither move a
//! lifecycle nor carry a protected value.

use super::common;

use norn_cell::export::Exporter;
use norn_cell::{CellStore, InstalledDefinitionInput, Model, SqliteCell, StoreOptions};
use norn_ir::mapping::ObservedLifecycleSnapshot;
use norn_ir::{
    DecisionOutput, FactChange, ObjectTypeDef, ObjectTypeName, PropertyId, TransitionId,
};
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

const SCOPE: &str = "org_demo/dev";
const UNDECLARED: &str = "lifecycle.validation.undeclared_write";

/// A domain-neutral model. A document's `submitted` and `published` states
/// are immutable, `published` is terminal, `submit` is guarded, and `reopen`
/// leaves an immutable state. A memo inherits a record's lifecycle. A badge
/// has no lifecycle and one rules-only property.
fn definitions(extra: &[serde_json::Value]) -> Vec<serde_json::Value> {
    let mut document = vec![
        json!({ "id": "status", "default": "draft", "type": { "kind": "enum", "values": [
            { "value": "draft" }, { "value": "submitted" }, { "value": "published" }
        ] } }),
        json!({ "id": "pages", "type": { "kind": "integer" } }),
    ];
    document.extend(extra.iter().cloned());
    vec![
        json!({
            "name": "test.Document",
            "properties": document,
            "lifecycles": [{
                "property": "status",
                "initial": ["draft"],
                "terminal": ["published"],
                "immutable": ["submitted", "published"],
                "transitions": [
                    { "id": "submit", "from": ["draft"], "to": "submitted",
                      "guards": ["self.pages > 0"] },
                    { "id": "publish", "from": ["submitted"], "to": "published" },
                    { "id": "reopen", "from": ["submitted"], "to": "draft" }
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
            "name": "test.Memo",
            "extends": "test.Record",
            "properties": [
                { "id": "body", "type": { "kind": "text" }, "constraints": { "nullable": true } }
            ]
        }),
        json!({
            "name": "test.Badge",
            "properties": [
                { "id": "label", "type": { "kind": "text" }, "constraints": { "nullable": true } },
                { "id": "issuer", "protected": true, "type": { "kind": "text" },
                  "constraints": { "nullable": true } }
            ]
        }),
    ]
}

#[allow(
    clippy::expect_used,
    reason = "test fixture: a failed setup step fails the test"
)]
fn types(extra: &[serde_json::Value]) -> Vec<ObjectTypeDef> {
    definitions(extra)
        .into_iter()
        .map(|value| serde_json::from_value(value).expect("definition decodes"))
        .collect()
}

#[allow(
    clippy::expect_used,
    reason = "test fixture: a failed setup step fails the test"
)]
fn open<E>() -> SqliteCell<E, JsonObject, JsonObject> {
    SqliteCell::open_in_memory(
        common::model_of(&types(&[])),
        SCOPE,
        |command: &norn_kernel::Command<JsonObject>| command.payload.clone(),
    )
    .expect("opens")
}

#[allow(
    clippy::expect_used,
    reason = "test fixture: a failed setup step fails the test"
)]
fn values(value: serde_json::Value) -> JsonObject {
    serde_json::from_value(value).expect("values")
}

fn document(key: &str) -> FactKey {
    FactKey::new("test.Document", key)
}

/// One `Put` of `key` over version `expected`.
fn write(key: &FactKey, expected: FactVersion, written: serde_json::Value) -> FactWrite {
    FactWrite {
        key: key.clone(),
        expected,
        mutation: FactMutation::Put {
            values: values(written),
        },
    }
}

/// A commit of one `Put` recording `events`.
fn put<E>(
    key: &FactKey,
    expected: FactVersion,
    written: serde_json::Value,
    events: Vec<E>,
) -> CellCommit<E, JsonObject, JsonObject> {
    CellCommit {
        writes: vec![write(key, expected, written)],
        events,
        ..CellCommit::empty()
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

fn set(key: &FactKey, property: &str, value: serde_json::Value) -> FactEvent {
    FactEvent::Set {
        key: key.clone(),
        property: PropertyId(property.into()),
        value: norn_ir::RuntimeValue::from_json(value).unwrap_or(norn_ir::RuntimeValue::Null),
    }
}

#[allow(
    clippy::expect_used,
    reason = "test fixture: a failed setup step fails the test"
)]
/// Create `key` in its initial state by a direct first `Put`.
fn create<E: serde::Serialize + 'static>(
    cell: &mut SqliteCell<E, JsonObject, JsonObject>,
    key: &FactKey,
    written: serde_json::Value,
) -> FactVersion {
    cell.commit(put(key, FactVersion::ABSENT, written, Vec::new()))
        .expect("a first write in an initial state");
    cell.load(key).expect("loads").version
}

#[allow(
    clippy::expect_used,
    reason = "test fixture: a failed setup step fails the test"
)]
/// A document moved to `submitted` by recorded transitions.
fn submitted(cell: &mut Cell, key: &FactKey, pages: u64) -> FactVersion {
    let created = create(cell, key, json!({ "status": "draft", "pages": pages }));
    cell.commit(put(
        key,
        created,
        json!({ "status": "submitted", "pages": pages }),
        vec![fired(key, "status", "submit", "draft", "submitted")],
    ))
    .expect("a recorded legal transition");
    cell.load(key).expect("loads").version
}

#[allow(
    clippy::expect_used,
    reason = "test fixture: a failed setup step fails the test"
)]
/// Everything a refusal must leave untouched: the verified ledger head and
/// every stored row with its version.
fn frozen<E: serde::Serialize + 'static>(
    cell: &SqliteCell<E, JsonObject, JsonObject>,
) -> (String, String) {
    let head = cell.store().verified_head().expect("verified head");
    let dump = cell.store().read().dump().expect("dump");
    (format!("{head:?}"), format!("{dump:?}"))
}

#[allow(
    clippy::expect_used,
    reason = "test fixture: a failed setup step fails the test"
)]
fn property<E: serde::Serialize + 'static>(
    cell: &SqliteCell<E, JsonObject, JsonObject>,
    key: &FactKey,
    property: &str,
) -> Option<serde_json::Value> {
    cell.load(key)
        .expect("loads")
        .values
        .and_then(|values| values.get(property).cloned())
        .map(|value| serde_json::to_value(value).expect("json"))
}

#[allow(
    clippy::panic,
    reason = "test fixture: an unexpected refusal fails the test"
)]
fn refusal(error: CommitError<norn_cell::CellError>) -> (FactKey, LifecycleInitRefusal) {
    assert_eq!(error.code(), UNDECLARED, "{error}");
    let CommitError::LifecycleInitialization { key, refusal } = error else {
        panic!("expected a lifecycle refusal, got {error:?}");
    };
    (key, *refusal)
}

fn unrecorded_transition(reached: &str, written: &str) -> LifecycleInitRefusal {
    LifecycleInitRefusal::UnrecordedTransition {
        lifecycle: "status".into(),
        reached: Some(reached.into()),
        written: Some(written.into()),
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

/// Submit and publish `key`, as one decision over an existing document.
fn submit_and_publish(key: &FactKey) -> DecisionOutput {
    let transition = |id: &str| FactChange::Transition {
        object_type: ObjectTypeName(key.object_type.as_str().into()),
        key: key.key.clone(),
        lifecycle: PropertyId("status".into()),
        transition: TransitionId(id.into()),
    };
    DecisionOutput {
        facts: vec![transition("submit"), transition("publish")],
        links: vec![],
        commands: vec![],
        timers: vec![],
    }
}

#[test]
fn a_direct_put_cannot_jump_an_existing_object_to_another_state() {
    let mut cell: Cell = open();
    let key = document("D-1");
    let rider = document("D-9");
    let version = create(&mut cell, &key, json!({ "status": "draft", "pages": 3 }));
    let before = frozen(&cell);
    // `published` is terminal and not adjacent; `submitted` is adjacent, but
    // nothing records the transition that reaches it.
    for target in ["published", "submitted"] {
        let mut commit = put(
            &key,
            version,
            json!({ "status": target, "pages": 3 }),
            Vec::new(),
        );
        // A legal first write in the same commit is not applied either.
        commit.writes.push(write(
            &rider,
            FactVersion::ABSENT,
            json!({ "status": "draft", "pages": 1 }),
        ));
        let refused = refusal(cell.commit(commit).expect_err("a state jump"));
        assert_eq!(
            refused,
            (key.clone(), unrecorded_transition("draft", target)),
            "{target}"
        );
        assert_eq!(frozen(&cell), before, "{target}");
        assert_eq!(cell.load(&key).expect("loads").version, version);
        assert!(cell.load(&rider).expect("loads").values.is_none());
    }
    // The object stays where it was, and a later legal update still applies.
    assert_eq!(property(&cell, &key, "status"), Some(json!("draft")));
    cell.commit(put(
        &key,
        version,
        json!({ "status": "draft", "pages": 4 }),
        Vec::new(),
    ))
    .expect("properties outside lifecycles stay writable in a mutable state");
    assert_eq!(property(&cell, &key, "pages"), Some(json!(4)));
}

#[test]
fn a_forged_skipped_or_guard_failing_transition_record_is_refused() {
    let mut cell: Cell = open();
    let key = document("D-1");
    let empty = document("D-2");
    let version = create(&mut cell, &key, json!({ "status": "draft", "pages": 3 }));
    let unguarded = create(&mut cell, &empty, json!({ "status": "draft", "pages": 0 }));
    let before = frozen(&cell);
    let illegal = |refused: &LifecycleInitRefusal, transition: &str| {
        matches!(
            refused,
            LifecycleInitRefusal::IllegalTransition { lifecycle, transition: actual, .. }
                if lifecycle == "status" && actual == transition
        )
    };

    // `publish` leaves `submitted`: it cannot fire from the stored `draft`,
    // whichever source state the record claims.
    for from in ["submitted", "draft"] {
        let (_, skipped) = refusal(
            cell.commit(put(
                &key,
                version,
                json!({ "status": "published", "pages": 3 }),
                vec![fired(&key, "status", "publish", from, "published")],
            ))
            .expect_err("a skipped transition"),
        );
        assert!(illegal(&skipped, "publish"), "{from}: {skipped:?}");
    }
    // A transition the lifecycle does not declare.
    let (_, undeclared) = refusal(
        cell.commit(put(
            &key,
            version,
            json!({ "status": "published", "pages": 3 }),
            vec![fired(&key, "status", "leap", "draft", "published")],
        ))
        .expect_err("an undeclared transition"),
    );
    assert!(illegal(&undeclared, "leap"), "{undeclared:?}");
    // A legal record of one step does not justify a row two steps away.
    let (_, short) = refusal(
        cell.commit(put(
            &key,
            version,
            json!({ "status": "published", "pages": 3 }),
            vec![fired(&key, "status", "submit", "draft", "submitted")],
        ))
        .expect_err("a row beyond its recorded transitions"),
    );
    assert_eq!(short, unrecorded_transition("submitted", "published"));
    // `submit` is guarded by `pages > 0`, read from the stored row.
    let (refused, guarded) = refusal(
        cell.commit(put(
            &empty,
            unguarded,
            json!({ "status": "submitted", "pages": 0 }),
            vec![fired(&empty, "status", "submit", "draft", "submitted")],
        ))
        .expect_err("a guard that does not hold"),
    );
    assert_eq!(refused, empty);
    assert!(illegal(&guarded, "submit"), "{guarded:?}");
    // A recorded `set` never moves a lifecycle property.
    let (_, direct) = refusal(
        cell.commit(put(
            &key,
            version,
            json!({ "status": "submitted", "pages": 3 }),
            vec![set(&key, "status", json!("submitted"))],
        ))
        .expect_err("a lifecycle property set directly"),
    );
    assert!(
        matches!(&direct, LifecycleInitRefusal::UnrecordedChange { reason }
            if reason.contains("changes only through a declared transition")),
        "{direct:?}"
    );
    assert_eq!(frozen(&cell), before);
}

#[test]
fn a_legal_transition_with_its_recorded_event_is_admitted() {
    let mut cell: Cell = open();
    // One step per commit.
    let key = document("D-1");
    let version = submitted(&mut cell, &key, 3);
    assert_eq!(property(&cell, &key, "status"), Some(json!("submitted")));
    cell.commit(put(
        &key,
        version,
        json!({ "status": "published", "pages": 3 }),
        vec![fired(&key, "status", "publish", "submitted", "published")],
    ))
    .expect("the next recorded transition");
    assert_eq!(property(&cell, &key, "status"), Some(json!("published")));

    // Two steps in one commit, replayed in order from the stored row.
    let both = document("D-2");
    let version = create(&mut cell, &both, json!({ "status": "draft", "pages": 1 }));
    cell.commit(put(
        &both,
        version,
        json!({ "status": "published", "pages": 1 }),
        vec![
            fired(&both, "status", "submit", "draft", "submitted"),
            fired(&both, "status", "publish", "submitted", "published"),
        ],
    ))
    .expect("both recorded transitions");
    assert_eq!(property(&cell, &both, "status"), Some(json!("published")));

    // The decision path over an existing object, and its prepared commit
    // submitted directly: the kernel's own evidence replays.
    let decided = document("D-3");
    create(
        &mut cell,
        &decided,
        json!({ "status": "draft", "pages": 2 }),
    );
    let model = cell.store().model().clone();
    let catalog = model.type_catalog();
    let slice = load_slice(&cell, [decided.clone()]).expect("slice");
    commit_decision(
        &mut cell,
        &catalog,
        &slice,
        &turn("decide"),
        &submit_and_publish(&decided),
    )
    .expect("a decision transitions an existing object");
    assert_eq!(
        property(&cell, &decided, "status"),
        Some(json!("published"))
    );

    let prepared_key = document("D-4");
    create(
        &mut cell,
        &prepared_key,
        json!({ "status": "draft", "pages": 2 }),
    );
    let slice = load_slice(&cell, [prepared_key.clone()]).expect("slice");
    let prepared = decision_cell_commit_with_host(
        &cell,
        &catalog,
        &slice,
        &turn("prepare"),
        &submit_and_publish(&prepared_key),
        &EnvelopeLimits::V1,
    )
    .expect("the decision prepares");
    assert!(
        prepared
            .events
            .iter()
            .any(|event| matches!(event, FactEvent::Modified { .. })),
        "the kernel records its aggregate beside the transitions"
    );
    cell.commit(prepared)
        .expect("the kernel's prepared commit is admitted on the direct path");
    assert_eq!(
        property(&cell, &prepared_key, "status"),
        Some(json!("published"))
    );
}

#[test]
fn an_object_in_an_immutable_or_terminal_state_refuses_unrecorded_changes() {
    let mut cell: Cell = open();
    let key = document("D-1");
    let version = submitted(&mut cell, &key, 3);
    let before = frozen(&cell);
    let immutable = |property: &str, state: &str| LifecycleInitRefusal::ImmutableState {
        lifecycle: "status".into(),
        state: state.into(),
        property: property.into(),
    };

    // Any other property of an object in an immutable state.
    let (refused, changed) = refusal(
        cell.commit(put(
            &key,
            version,
            json!({ "status": "submitted", "pages": 9 }),
            Vec::new(),
        ))
        .expect_err("a change in an immutable state"),
    );
    assert_eq!(
        (refused, changed),
        (key.clone(), immutable("pages", "submitted"))
    );
    // Recording the change does not admit it: a `set` there is refused.
    let (_, recorded) = refusal(
        cell.commit(put(
            &key,
            version,
            json!({ "status": "submitted", "pages": 9 }),
            vec![set(&key, "pages", json!(9))],
        ))
        .expect_err("a recorded set in an immutable state"),
    );
    assert!(
        matches!(&recorded, LifecycleInitRefusal::UnrecordedChange { reason }
            if reason.contains("immutable status state submitted")),
        "{recorded:?}"
    );
    // A row that omits the state would fall back to the default.
    let (_, defaulted) = refusal(
        cell.commit(put(&key, version, json!({ "pages": 3 }), Vec::new()))
            .expect_err("an omitted state is the default state"),
    );
    assert_eq!(defaulted, unrecorded_transition("submitted", "draft"));
    assert_eq!(frozen(&cell), before);

    // Publish it: the terminal state is never left, with or without a
    // record, and it is immutable too.
    cell.commit(put(
        &key,
        version,
        json!({ "status": "published", "pages": 3 }),
        vec![fired(&key, "status", "publish", "submitted", "published")],
    ))
    .expect("a recorded transition leaves an immutable state");
    let version = cell.load(&key).expect("loads").version;
    let before = frozen(&cell);
    let (_, left) = refusal(
        cell.commit(put(
            &key,
            version,
            json!({ "status": "draft", "pages": 3 }),
            Vec::new(),
        ))
        .expect_err("a terminal state is never left"),
    );
    assert_eq!(left, unrecorded_transition("published", "draft"));
    let (_, forged) = refusal(
        cell.commit(put(
            &key,
            version,
            json!({ "status": "draft", "pages": 3 }),
            vec![fired(&key, "status", "reopen", "published", "draft")],
        ))
        .expect_err("no transition leaves a terminal state"),
    );
    assert!(
        matches!(&forged, LifecycleInitRefusal::IllegalTransition { transition, .. }
            if transition == "reopen"),
        "{forged:?}"
    );
    let (_, changed) = refusal(
        cell.commit(put(
            &key,
            version,
            json!({ "status": "published", "pages": 1 }),
            Vec::new(),
        ))
        .expect_err("a change in a terminal immutable state"),
    );
    assert_eq!(changed, immutable("pages", "published"));
    assert_eq!(frozen(&cell), before);

    // Writing the row it already holds changes nothing a lifecycle governs.
    cell.commit(put(
        &key,
        version,
        json!({ "status": "published", "pages": 3 }),
        Vec::new(),
    ))
    .expect("an identical row is not a change");
}

#[test]
fn a_recorded_transition_cannot_carry_an_unrecorded_change_into_an_immutable_state() {
    let mut cell: Cell = open();
    let key = document("D-1");
    let version = create(&mut cell, &key, json!({ "status": "draft", "pages": 3 }));
    let before = frozen(&cell);
    // The guard held on the stored row, but the written row holds a value
    // nothing recorded, in a state where it could not be set.
    let (_, smuggled) = refusal(
        cell.commit(put(
            &key,
            version,
            json!({ "status": "submitted", "pages": 0 }),
            vec![fired(&key, "status", "submit", "draft", "submitted")],
        ))
        .expect_err("an unrecorded change entering an immutable state"),
    );
    assert_eq!(
        smuggled,
        LifecycleInitRefusal::ImmutableState {
            lifecycle: "status".into(),
            state: "submitted".into(),
            property: "pages".into(),
        }
    );
    assert_eq!(frozen(&cell), before);

    // Recorded before the transition, the change is a set in a mutable
    // state, and the guard reads it.
    cell.commit(put(
        &key,
        version,
        json!({ "status": "submitted", "pages": 5 }),
        vec![
            set(&key, "pages", json!(5)),
            fired(&key, "status", "submit", "draft", "submitted"),
        ],
    ))
    .expect("a recorded set, then the transition");
    assert_eq!(property(&cell, &key, "pages"), Some(json!(5)));

    // Leaving the immutable state, a change beside the recorded transition
    // is a write in the mutable state it reaches.
    let version = cell.load(&key).expect("loads").version;
    cell.commit(put(
        &key,
        version,
        json!({ "status": "draft", "pages": 7 }),
        vec![fired(&key, "status", "reopen", "submitted", "draft")],
    ))
    .expect("a change after leaving the immutable state");
    assert_eq!(property(&cell, &key, "status"), Some(json!("draft")));
    assert_eq!(property(&cell, &key, "pages"), Some(json!(7)));
}

#[test]
fn an_inherited_lifecycle_is_governed_on_update() {
    let mut cell: Cell = open();
    let key = FactKey::new("test.Memo", "M-1");
    let version = create(&mut cell, &key, json!({ "body": "first" }));
    let before = frozen(&cell);
    let (refused, inherited) = refusal(
        cell.commit(put(
            &key,
            version,
            json!({ "phase": "closed", "body": "first" }),
            Vec::new(),
        ))
        .expect_err("the inherited lifecycle cannot jump"),
    );
    assert_eq!(refused, key);
    assert_eq!(
        inherited,
        LifecycleInitRefusal::UnrecordedTransition {
            lifecycle: "phase".into(),
            reached: Some("open".into()),
            written: Some("closed".into()),
        }
    );
    // Evidence naming the supertype is not evidence for the memo.
    let record = FactKey::new("test.Record", "M-1");
    let (_, sibling) = refusal(
        cell.commit(put(
            &key,
            version,
            json!({ "phase": "closed", "body": "first" }),
            vec![fired(&record, "phase", "close", "open", "closed")],
        ))
        .expect_err("evidence for another type"),
    );
    assert!(
        matches!(&sibling, LifecycleInitRefusal::UnrecordedChange { reason }
            if reason.contains("names another type")),
        "{sibling:?}"
    );
    assert_eq!(frozen(&cell), before);
    cell.commit(put(
        &key,
        version,
        json!({ "phase": "closed", "body": "second" }),
        vec![fired(&key, "phase", "close", "open", "closed")],
    ))
    .expect("the inherited transition, recorded for the memo");
    assert_eq!(property(&cell, &key, "phase"), Some(json!("closed")));
}

/// A pre-state that knows a document exists but cannot read its row.
struct Unreadable;

impl CommitView for Unreadable {
    type Error = Infallible;

    fn pending(&self, _: &norn_kernel::Consumed) -> Result<bool, Infallible> {
        Ok(false)
    }

    fn version(&self, _: &FactKey) -> Result<FactVersion, Infallible> {
        Ok(FactVersion(1))
    }

    fn concrete(&self, key: &FactKey) -> Result<Option<ObjectTypeName>, Infallible> {
        Ok(Some(key.object_type.clone()))
    }

    fn link_version(&self, _: &norn_kernel::LinkKey) -> Result<FactVersion, Infallible> {
        Ok(FactVersion::ABSENT)
    }

    fn links_of(&self, _: &FactKey) -> Result<Vec<norn_kernel::LinkKey>, Infallible> {
        Ok(Vec::new())
    }
}

#[test]
fn a_host_that_cannot_prove_the_stored_row_refuses_the_update() {
    let definitions = types(&[]);
    let model = common::model_of(&definitions);
    let catalog = model.type_catalog();
    let key = document("D-1");
    let commit: CellCommit<FactEvent, JsonObject, JsonObject> = put(
        &key,
        FactVersion(1),
        json!({ "status": "draft", "pages": 1 }),
        Vec::new(),
    );
    let refused = check_commit(&commit, &CommitModel::new(&catalog), &Unreadable)
        .expect_err("the stored row cannot be read");
    assert_eq!(refused.code(), UNDECLARED, "{refused}");
    assert!(
        matches!(&refused, CommitError::LifecycleInitialization { key: refused, refusal }
            if *refused == key && matches!(&**refusal,
                LifecycleInitRefusal::UnrecordedChange { reason }
                    if reason.contains("cannot prove the object's stored row"))),
        "{refused:?}"
    );
}

#[allow(
    clippy::expect_used,
    reason = "test fixture: a failed setup step fails the test"
)]
fn source_provenance() -> norn_ir::provenance::InstanceProvenance {
    serde_json::from_value(json!({
        "source": { "system": "synthetic", "record": "document/D-1" },
        "mappingVersion": "fixture.document@1",
        "recordedAt": "2026-11-01T00:00:00Z",
        "sourceFields": ["pages", "status"]
    }))
    .expect("source provenance")
}

#[allow(
    clippy::expect_used,
    reason = "test fixture: a failed setup step fails the test"
)]
/// A sealed observation of `key` with `pages`, observed in lifecycle `state`.
fn observe(
    cell: &ObservedCell,
    key: &FactKey,
    pages: u64,
    state: Option<&str>,
) -> (
    CellCommit<ObservationEvent, JsonObject, JsonObject>,
    ObservationAuthorization,
) {
    let model = cell.store().model().clone();
    let catalog = model.type_catalog();
    let slice = load_slice(cell, [key.clone()]).expect("slice");
    let provenance = source_provenance();
    let lifecycles: Vec<ObservedLifecycleSnapshot> = state
        .into_iter()
        .map(|state| ObservedLifecycleSnapshot {
            property: PropertyId("status".into()),
            state: state.to_owned(),
            provenance: provenance.clone(),
        })
        .collect();
    prepare_observed_snapshot(
        &catalog,
        &slice,
        &turn("observe"),
        ObservedSnapshot {
            key,
            values: &values(json!({ "pages": pages })),
            lifecycles: &lifecycles,
            provenance: &provenance,
        },
    )
    .expect("sealed observed write")
    .into_parts()
}

#[test]
fn a_sealed_observation_reconciles_an_existing_lifecycle_and_exempts_only_its_own_writes() {
    let mut cell: ObservedCell = open();
    let observed = document("D-1");
    let other = document("D-2");
    create(
        &mut cell,
        &observed,
        json!({ "status": "draft", "pages": 2 }),
    );
    let version = create(&mut cell, &other, json!({ "status": "draft", "pages": 2 }));
    let before = frozen(&cell);

    // The source reports the document published. Without its sealed
    // authorization the same commit is an ordinary one: its observed
    // transitions are no evidence for an unsealed write.
    let (commit, _unused) = observe(&cell, &observed, 2, Some("published"));
    assert!(
        commit
            .events
            .iter()
            .any(|event| matches!(event, ObservationEvent::ObservedTransition { .. })),
        "the observation reconciles the lifecycle by observed transitions"
    );
    let (refused, unsealed) = refusal(cell.commit(commit).expect_err("an unsealed write"));
    assert_eq!(refused, observed);
    assert_eq!(unsealed, unrecorded_transition("draft", "published"));
    assert_eq!(frozen(&cell), before);

    // Sealed, with an unsealed write of another object riding along: only
    // the prepared write is exempt, so the commit is refused whole.
    let (mut commit, authorization) = observe(&cell, &observed, 2, Some("published"));
    commit.writes.push(write(
        &other,
        version,
        json!({ "status": "published", "pages": 2 }),
    ));
    let (refused, rider) = refusal(
        cell.commit_observed(commit, authorization)
            .expect_err("the unsealed write is an ordinary update"),
    );
    assert_eq!(refused, other);
    assert_eq!(rider, unrecorded_transition("draft", "published"));
    assert_eq!(frozen(&cell), before);

    // The sealed write alone reconciles under its own evidence contract.
    let (commit, authorization) = observe(&cell, &observed, 2, Some("published"));
    cell.commit_observed(commit, authorization)
        .expect("the sealed observation commits");
    assert_eq!(
        property(&cell, &observed, "status"),
        Some(json!("published"))
    );
    assert_eq!(property(&cell, &other, "status"), Some(json!("draft")));
}

/// #1752: `check_protected` exempts a sealed observation per write, as the
/// lifecycle, version, invariant and law stages do. A type with no lifecycle
/// reaches no other stage that would refuse the protected value.
#[test]
fn an_unsealed_write_on_an_observed_commit_cannot_carry_a_protected_value() {
    let mut cell: ObservedCell = open();
    let observed = document("D-1");
    let forged = FactKey::new("test.Badge", "B-1");
    let existing = FactKey::new("test.Badge", "B-2");
    let version = create(&mut cell, &existing, json!({ "label": "plain" }));
    let before = frozen(&cell);
    let riders = [
        // A first write that carries a protected value.
        write(
            &forged,
            FactVersion::ABSENT,
            json!({ "label": "forged", "issuer": "person:anyone" }),
        ),
        // An update that gives an existing object one.
        write(
            &existing,
            version,
            json!({ "label": "plain", "issuer": "person:anyone" }),
        ),
        // A delete of an object whose type has protected properties.
        FactWrite {
            key: existing.clone(),
            expected: version,
            mutation: FactMutation::Delete,
        },
    ];
    for rider in riders {
        let (mut commit, authorization) = observe(&cell, &observed, 2, None);
        commit.writes.push(rider.clone());
        let refused = cell
            .commit_observed(commit, authorization)
            .expect_err("the unsealed write is checked");
        assert_eq!(refused.code(), "commit.immutable_object", "{refused}");
        assert!(
            matches!(&refused, CommitError::ImmutableObject { key } if *key == rider.key),
            "{refused:?}"
        );
        assert_eq!(frozen(&cell), before);
    }
    // An empty authorization exempts nothing either.
    let mut commit = CellCommit::empty();
    commit.writes.push(write(
        &forged,
        FactVersion::ABSENT,
        json!({ "issuer": "person:anyone" }),
    ));
    let refused = cell
        .commit_observed(commit, ObservationAuthorization::empty())
        .expect_err("no sealed write covers the forged put");
    assert_eq!(refused.code(), "commit.immutable_object", "{refused}");
    assert_eq!(frozen(&cell), before);

    // The sealed write alone is admitted under its own contract, and an
    // ordinary unprotected write may ride with it.
    let (mut commit, authorization) = observe(&cell, &observed, 2, None);
    commit
        .writes
        .push(write(&existing, version, json!({ "label": "renamed" })));
    cell.commit_observed(commit, authorization)
        .expect("the sealed observation and an ordinary write commit");
    assert!(cell.load(&observed).expect("loads").values.is_some());
    assert_eq!(property(&cell, &existing, "label"), Some(json!("renamed")));
    assert!(cell.load(&forged).expect("loads").values.is_none());
}

/// The observed-transition event of an unsealed write is ignored, not
/// trusted: it neither moves the lifecycle nor refuses an update that moves
/// nothing.
#[test]
fn an_observed_transition_is_no_evidence_for_an_unsealed_update() {
    let mut cell: ObservedCell = open();
    let key = document("D-1");
    let version = create(&mut cell, &key, json!({ "status": "draft", "pages": 2 }));
    let before = frozen(&cell);
    let observed = || ObservationEvent::ObservedTransition {
        key: key.clone(),
        property: PropertyId("status".into()),
        transition: TransitionId("submit".into()),
        from: "draft".into(),
        to: "submitted".into(),
        synthesized: ObservationSynthesis::SnapshotCatchUp,
        provenance: source_provenance(),
    };
    let (_, moved) = refusal(
        cell.commit(put(
            &key,
            version,
            json!({ "status": "submitted", "pages": 2 }),
            vec![observed()],
        ))
        .expect_err("an observed transition proves nothing unsealed"),
    );
    assert_eq!(moved, unrecorded_transition("draft", "submitted"));
    assert_eq!(frozen(&cell), before);
}

#[allow(
    clippy::expect_used,
    reason = "test fixture: a failed setup step fails the test"
)]
/// The model of `definitions` and its canonical definition set.
fn installable(definitions: &[serde_json::Value]) -> (Model, String) {
    let typed: Vec<ObjectTypeDef> = definitions
        .iter()
        .map(|value| serde_json::from_value(value.clone()).expect("typed"))
        .collect();
    let model = Model::from_definitions(&typed, &[]).expect("model");
    let artifacts: Vec<serde_json::Value> = definitions
        .iter()
        .map(|definition| {
            let name = definition["name"].as_str().expect("name");
            let (namespace, local) = name.split_once('.').expect("qualified name");
            json!({
                "irVersion": "0.1", "artifactId": format!("{name}.entity"), "kind": "entity",
                "namespace": namespace, "name": local, "version": "0.1.0", "lifecycle": "draft",
                "scope": { "orgId": "org_demo", "environmentId": "dev" }, "dependencies": [],
                "provenance": { "source": "authoring_sdk", "sourcePath": "test" },
                "payload": { "entityVersion": "2.0", "objectType": definition }
            })
        })
        .collect();
    let canonical = norn_ir::RuntimeValue::from_json(json!(artifacts))
        .expect("exact")
        .canonical_json()
        .expect("canonical");
    (model, String::from_utf8(canonical).expect("utf8"))
}

fn input(canonical_set: &str, at: u64) -> InstalledDefinitionInput {
    InstalledDefinitionInput {
        canonical_set: canonical_set.to_owned(),
        registry: Vec::new(),
        receipt_sha256: "a".repeat(64),
        admission_receipt: None,
        readmission: None,
        installed_at: at,
        diff: json!({ "added": [], "removed": [], "changed": [], "narrowing": [] }),
        layout: None,
    }
}

#[allow(
    clippy::expect_used,
    reason = "test fixture: a failed setup step fails the test"
)]
fn open_at(path: &std::path::Path, model: Model) -> Cell {
    Cell::open(
        path,
        model,
        StoreOptions::default(),
        SCOPE,
        |command: &norn_kernel::Command<JsonObject>| command.payload.clone(),
    )
    .expect("opens")
}

/// A pinned definition migration writes existing rows outside commit
/// admission: a published document gains a defaulted member and keeps its
/// state. Afterwards the same rule governs it under the new model.
#[test]
fn a_definition_migration_keeps_lifecycle_states_and_the_rule_still_governs_them() {
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("cell.sqlite");
    let (first, first_set) = installable(&definitions(&[]));
    let (second, second_set) = installable(&definitions(&[
        json!({ "id": "edition", "type": { "kind": "text" }, "default": "first" }),
    ]));
    let mut cell = open_at(&path, first.clone());
    cell.install_definitions(first, input(&first_set, 10), Some(0))
        .expect("first definitions");
    let key = document("D-1");
    let version = submitted(&mut cell, &key, 3);
    cell.commit(put(
        &key,
        version,
        json!({ "status": "published", "pages": 3 }),
        vec![fired(&key, "status", "publish", "submitted", "published")],
    ))
    .expect("published");
    let draft = document("D-2");
    create(&mut cell, &draft, json!({ "status": "draft", "pages": 1 }));

    cell.install_definitions(second, input(&second_set, 20), Some(1))
        .expect("the migration installs over rows in non-initial states");
    assert_eq!(property(&cell, &key, "status"), Some(json!("published")));
    assert_eq!(property(&cell, &key, "edition"), Some(json!("first")));
    assert_eq!(property(&cell, &draft, "edition"), Some(json!("first")));

    let version = cell.load(&key).expect("loads").version;
    let before = frozen(&cell);
    let (_, left) = refusal(
        cell.commit(put(
            &key,
            version,
            json!({ "status": "draft", "pages": 3, "edition": "first" }),
            Vec::new(),
        ))
        .expect_err("a terminal state is never left"),
    );
    assert_eq!(left, unrecorded_transition("published", "draft"));
    let (_, changed) = refusal(
        cell.commit(put(
            &key,
            version,
            json!({ "status": "published", "pages": 3, "edition": "second" }),
            Vec::new(),
        ))
        .expect_err("the migrated member is immutable in an immutable state"),
    );
    assert!(
        matches!(&changed, LifecycleInitRefusal::ImmutableState { property, .. }
            if property == "edition"),
        "{changed:?}"
    );
    assert_eq!(frozen(&cell), before);
    // A row written before the migration that omits the new member is the
    // row the migration left: its default is not a change.
    cell.commit(put(
        &key,
        version,
        json!({ "status": "published", "pages": 3 }),
        Vec::new(),
    ))
    .expect("the migrated row, rewritten");
    // The migrated draft still transitions by a recorded event.
    let version = cell.load(&draft).expect("loads").version;
    cell.commit(put(
        &draft,
        version,
        json!({ "status": "submitted", "pages": 1, "edition": "first" }),
        vec![fired(&draft, "status", "submit", "draft", "submitted")],
    ))
    .expect("a recorded transition after the migration");
}

/// An authenticated history restore replays committed history outside commit
/// admission: objects in non-initial states are rebuilt as they were, and
/// the restored cell enforces the same rule.
#[tokio::test]
async fn a_history_restore_rebuilds_lifecycle_states_and_the_rule_still_governs_them() {
    let directory = tempfile::tempdir().expect("directory");
    let source = directory.path().join("cell.sqlite");
    let target = directory.path().join("restored.sqlite");
    let model = common::model_of(&types(&[]));
    let mut cell = open_at(&source, model.clone());
    let key = document("D-1");
    let version = submitted(&mut cell, &key, 3);
    cell.commit(put(
        &key,
        version,
        json!({ "status": "published", "pages": 3 }),
        vec![fired(&key, "status", "publish", "submitted", "published")],
    ))
    .expect("published");
    let held = document("D-2");
    submitted(&mut cell, &held, 2);

    let mut exporter = Exporter::local(&directory.path().join("export")).expect("exporter");
    let store = cell.store();
    let journaled = exporter.verify().await.expect("verify").head_commit_seq;
    for seq in journaled.saturating_add(1)..=store.export_commit_count().expect("count") {
        exporter
            .append(&store.export_record(seq).expect("journal").expect("record"))
            .await
            .expect("append");
    }
    let manifest = exporter.verify().await.expect("verified export");
    CellStore::restore_from(
        &mut exporter,
        &target,
        model.clone(),
        SCOPE,
        &manifest.bindings,
        StoreOptions::default(),
    )
    .await
    .expect("the history restores");
    let dump = store.read().dump().expect("dump");
    drop(cell);

    let mut restored = open_at(&target, model);
    assert_eq!(restored.store().read().dump().expect("dump"), dump);
    assert_eq!(
        property(&restored, &key, "status"),
        Some(json!("published"))
    );
    assert_eq!(
        property(&restored, &held, "status"),
        Some(json!("submitted"))
    );
    let version = restored.load(&held).expect("loads").version;
    let before = frozen(&restored);
    let (_, jumped) = refusal(
        restored
            .commit(put(
                &held,
                version,
                json!({ "status": "draft", "pages": 2 }),
                Vec::new(),
            ))
            .expect_err("a restored object's state does not move without a record"),
    );
    assert_eq!(jumped, unrecorded_transition("submitted", "draft"));
    assert_eq!(frozen(&restored), before);
    restored
        .commit(put(
            &held,
            version,
            json!({ "status": "published", "pages": 2 }),
            vec![fired(&held, "status", "publish", "submitted", "published")],
        ))
        .expect("a recorded transition on the restored cell");
}

#[allow(
    clippy::expect_used,
    reason = "test fixture: a failed setup step fails the test"
)]
fn open_dual() -> Cell {
    let dual_def: ObjectTypeDef = serde_json::from_value(json!({
        "name": "test.Dual",
        "properties": [
            { "id": "workflow", "default": "draft", "type": { "kind": "enum", "values": [
                { "value": "draft" }, { "value": "active" }, { "value": "archived" }
            ] } },
            { "id": "audit", "default": "pending", "type": { "kind": "enum", "values": [
                { "value": "pending" }, { "value": "locked" }
            ] } },
            { "id": "title", "type": { "kind": "text" } }
        ],
        "lifecycles": [
            {
                "property": "workflow",
                "initial": ["draft"],
                "terminal": ["archived"],
                "immutable": ["archived"],
                "transitions": [
                    { "id": "activate", "from": ["draft"], "to": "active" },
                    { "id": "archive", "from": ["active"], "to": "archived" }
                ]
            },
            {
                "property": "audit",
                "initial": ["pending"],
                "terminal": ["locked"],
                "immutable": ["locked"],
                "transitions": [
                    { "id": "lock", "from": ["pending"], "to": "locked" }
                ]
            }
        ]
    }))
    .expect("dual definition decodes");
    SqliteCell::open_in_memory(
        common::model_of(&[dual_def]),
        SCOPE,
        |command: &norn_kernel::Command<JsonObject>| command.payload.clone(),
    )
    .expect("opens")
}

/// N1 (#1687): an object with two lifecycles can transition lifecycle A while
/// lifecycle B stays fixed, and once lifecycle B enters an immutable state,
/// unrecorded non-lifecycle writes are refused even while lifecycle A remains
/// in a mutable state.
#[test]
fn a_two_lifecycle_object_governs_both_lifecycles_and_enforces_immutable_intersection() {
    let mut cell = open_dual();
    let key = FactKey::new("test.Dual", "DUAL-1");
    let v1 = create(
        &mut cell,
        &key,
        json!({ "workflow": "draft", "audit": "pending", "title": "initial" }),
    );

    // Transition lifecycle A (`workflow`: draft -> active) while lifecycle B
    // (`audit`: pending) stays fixed; both are mutable so `title` may change.
    cell.commit(put(
        &key,
        v1,
        json!({ "workflow": "active", "audit": "pending", "title": "active-title" }),
        vec![fired(&key, "workflow", "activate", "draft", "active")],
    ))
    .expect("transitioning lifecycle A while B stays fixed succeeds");
    assert_eq!(property(&cell, &key, "workflow"), Some(json!("active")));
    assert_eq!(property(&cell, &key, "audit"), Some(json!("pending")));
    assert_eq!(property(&cell, &key, "title"), Some(json!("active-title")));

    // Move lifecycle B (`audit`) to immutable `locked` while lifecycle A
    // (`workflow`) remains in mutable `active`.
    let v2 = cell.load(&key).expect("loads").version;
    cell.commit(put(
        &key,
        v2,
        json!({ "workflow": "active", "audit": "locked", "title": "active-title" }),
        vec![fired(&key, "audit", "lock", "pending", "locked")],
    ))
    .expect("locking audit succeeds");
    assert_eq!(property(&cell, &key, "audit"), Some(json!("locked")));

    // Now A (`active`) is mutable, but B (`locked`) is immutable: a
    // non-lifecycle write to `title` must be refused with zero partial effects.
    let v3 = cell.load(&key).expect("loads").version;
    let before = frozen(&cell);
    let (refused_key, err) = refusal(
        cell.commit(put(
            &key,
            v3,
            json!({ "workflow": "active", "audit": "locked", "title": "mutated" }),
            Vec::new(),
        ))
        .expect_err("non-lifecycle write must refuse when lifecycle B is immutable"),
    );
    assert_eq!(refused_key, key);
    assert_eq!(
        err,
        LifecycleInitRefusal::ImmutableState {
            lifecycle: "audit".into(),
            state: "locked".into(),
            property: "title".into(),
        }
    );
    assert_eq!(frozen(&cell), before);

    // An unrecorded jump on lifecycle A is still refused while B is fixed.
    let (_, jumped) = refusal(
        cell.commit(put(
            &key,
            v3,
            json!({ "workflow": "archived", "audit": "locked", "title": "active-title" }),
            Vec::new(),
        ))
        .expect_err("unrecorded transition on lifecycle A refuses"),
    );
    assert_eq!(
        jumped,
        LifecycleInitRefusal::UnrecordedTransition {
            lifecycle: "workflow".into(),
            reached: Some("active".into()),
            written: Some("archived".into()),
        }
    );
    assert_eq!(frozen(&cell), before);

    // A recorded legal transition on lifecycle A (`active -> archived`) that
    // keeps `title` and `audit` unchanged is admitted.
    cell.commit(put(
        &key,
        v3,
        json!({ "workflow": "archived", "audit": "locked", "title": "active-title" }),
        vec![fired(&key, "workflow", "archive", "active", "archived")],
    ))
    .expect("recorded transition on lifecycle A succeeds with no other mutation");
    assert_eq!(property(&cell, &key, "workflow"), Some(json!("archived")));
    assert_eq!(property(&cell, &key, "audit"), Some(json!("locked")));
}
