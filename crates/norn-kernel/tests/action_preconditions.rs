#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    missing_docs,
    reason = "integration-test fixture and assertion helpers"
)]

//! Pilot contracts: derive before reading/validating/writing, synchronous
//! cross-object validation, CAS, and original-evidence replay.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::convert::Infallible;

use norn_ir::{ActionRoot, ObjectTypeDef};
use norn_kernel::cell_host::{BoundedFact, JsonObject};
use norn_kernel::{
    ACTION_CALL_TYPE, ActionAuthorizer, ActionAuthzRequest, ActionCall, ActionDenial,
    ActionOutcome, ActionReadAuthzRequest, ActionReadTypeAuthzRequest, ActionRefusal, CellCommit,
    CellHost, Command, CommitError, CommitReceipt, DecisionTurn, DueTimer, FactEvent,
    FactExpectation, FactKey, FactMutation, FactState, FactVersion, FactWrite, LinkKey,
    LinkReadTarget, LinkState, LogicalTime, Observed, Performed, TypeCatalog, World, commit_action,
};
use norn_testkit::cell_host::MemoryCell;
use serde_json::json;

type Cell = MemoryCell<FactEvent, JsonObject, JsonObject>;

fn object(value: serde_json::Value) -> JsonObject {
    serde_json::from_value(value).expect("exact fixture object")
}

fn types() -> Vec<ObjectTypeDef> {
    serde_json::from_value(json!([
        {"name": "fashion.Season", "properties": [
            {"id": "code", "type": {"kind": "text"}},
            {"id": "brand", "type": {"kind": "text"}},
            {"id": "notes", "type": {"kind": "text"}},
            {"id": "active", "type": {"kind": "boolean"}}
        ]},
        {"name": "fashion.Collection", "properties": [
            {"id": "code", "type": {"kind": "text"}},
            {"id": "season_code", "type": {"kind": "text"}},
            {"id": "brand", "type": {"kind": "text"}},
            {"id": "label", "type": {"kind": "text"}}
        ]}
    ]))
    .expect("typed entities")
}

fn action() -> ActionRoot {
    serde_json::from_value(json!({
        "actionId": "fashion.collection.create",
        "scopeRequired": ["orgId", "environmentId"],
        "idempotent": true, "ledgered": true,
        "inputSchema": {
            "type": "object", "additionalProperties": false,
            "properties": {
                "code": {"type": "string"}, "season_code": {"type": "string"},
                "brand": {"type": "string"}, "label": {"type": "string"}
            },
            "required": ["code", "season_code", "brand", "label"]
        },
        "outputSchema": {"type": "object", "properties": {}, "additionalProperties": false},
        "derive": [
            {"field": "code", "expression": "trim(input.code)"},
            {"field": "code", "expression": "upper(input.code)"},
            {"field": "season_code", "expression": "upper(trim(input.season_code))"},
            {"field": "label", "expression": "input.label ?? input.code", "inputRequired": false}
        ],
        "slice": [{"name": "season", "objectType": "fashion.Season",
                   "key": {"property": {"from": "input", "property": "season_code"}}}],
        "preconditions": ["code == input.code", "slice.season.brand == input.brand", "slice.season.active"],
        "writes": [{"entityKind": "fashion.Collection", "change": "created"}],
        "effectProgram": {
            "kind": "upsert_input_object", "objectKind": "fashion.Collection", "objectIdField": "code",
            "entityArtifactId": "fashion.Collection.entity", "entityArtifactVersion": "0.1.0",
            "writeMode": {"kind": "create_only"}
        }
    })).expect("typed action")
}

fn invocation(action: &ActionRoot) -> ActionCall<'_> {
    ActionCall {
        action,
        action_artifact_id: "fashion.collection.create.action",
        action_version: "0.1.0",
        scope: BTreeMap::from([
            ("orgId".to_owned(), "org_demo".to_owned()),
            ("environmentId".to_owned(), "dev".to_owned()),
        ]),
        actor: "fashion:manager".to_owned(),
        input: object(json!({"code": " c26 ", "season_code": " ss26 ", "brand": "NORN"})),
        idempotency_key: "create-c26".to_owned(),
        object_type: None,
        turn: DecisionTurn {
            id: "create-c26-turn".to_owned(),
            now: LogicalTime::from_millis(1000),
            world: World::Main,
            consumes: None,
        },
    }
}

struct Allow;
impl ActionAuthorizer for Allow {
    fn authorize(&self, _: &ActionAuthzRequest<'_>) -> Result<(), ActionDenial> {
        Ok(())
    }
    fn authorize_read(&self, _: &ActionReadAuthzRequest<'_>) -> Result<(), ActionDenial> {
        Ok(())
    }
    fn authorize_read_type(&self, _: &ActionReadTypeAuthzRequest<'_>) -> Result<(), ActionDenial> {
        Ok(())
    }
}

fn put(cell: &mut Cell, key: FactKey, values: JsonObject) {
    let expected = cell.load(&key).expect("current version").version;
    let mut commit = CellCommit::empty();
    commit.writes.push(FactWrite {
        key,
        expected,
        mutation: FactMutation::Put { values },
    });
    cell.commit(commit).expect("seed/update commit");
}

fn season(active: bool, brand: &str) -> JsonObject {
    object(json!({"code": "SS26", "brand": brand, "active": active, "notes": ""}))
}

fn cell() -> Cell {
    let mut cell = Cell::new(|command: &Command<JsonObject>| command.payload.clone());
    put(
        &mut cell,
        FactKey::new("fashion.Season", "SS26"),
        season(true, "NORN"),
    );
    cell
}

#[test]
fn derives_in_order_before_lookup_and_validation_then_records_committed_evidence() {
    let types = types();
    let catalog = TypeCatalog::new(&types);
    let action = action();
    let invocation = invocation(&action);
    let mut cell = cell();
    let outcome =
        commit_action(&mut cell, &catalog, &invocation, &Allow).expect("related object is valid");
    let ActionOutcome::Committed { record, receipt } = outcome else {
        panic!("new action must commit")
    };
    assert_eq!(receipt.written.len(), 2);
    assert_eq!(
        record.input, invocation.input,
        "raw input binds idempotency"
    );
    let effective =
        object(json!({"code": "C26", "season_code": "SS26", "brand": "NORN", "label": "C26"}));
    assert_eq!(record.effective_input, Some(effective.clone()));
    assert_eq!(
        record
            .derived_values
            .iter()
            .map(|step| (step.field.as_str(), step.value.clone()))
            .collect::<Vec<_>>(),
        vec![
            ("code", norn_ir::RuntimeValue::from("c26")),
            ("code", norn_ir::RuntimeValue::from("C26")),
            ("season_code", norn_ir::RuntimeValue::from("SS26")),
            ("label", norn_ir::RuntimeValue::from("C26"))
        ]
    );
    assert_eq!(record.precondition_outcomes.len(), 3);
    assert!(
        record
            .precondition_outcomes
            .iter()
            .all(|condition| condition.satisfied)
    );
    assert_eq!(
        cell.load(&FactKey::new("fashion.Collection", "C26"))
            .expect("normalized identity")
            .values,
        Some(effective)
    );
    assert!(
        !cell
            .load(&FactKey::new("fashion.Collection", " c26 "))
            .expect("raw identity")
            .exists()
    );
}

#[test]
fn failing_related_condition_refuses_without_target_or_action_record() {
    let types = types();
    let catalog = TypeCatalog::new(&types);
    let action = action();
    let invocation = invocation(&action);
    let mut cell = cell();
    put(
        &mut cell,
        FactKey::new("fashion.Season", "SS26"),
        season(true, "OTHER"),
    );
    let before = cell.ledger().len();
    let refusal =
        commit_action(&mut cell, &catalog, &invocation, &Allow).expect_err("brand mismatch");
    assert_eq!(refusal.code(), "action.precondition_failed");
    assert!(
        refusal
            .to_string()
            .contains("slice.season.brand == input.brand")
    );
    assert_eq!(cell.ledger().len(), before);
    assert!(
        !cell
            .load(&FactKey::new("fashion.Collection", "C26"))
            .expect("target")
            .exists()
    );
    assert!(
        !cell
            .load(&FactKey::new(
                ACTION_CALL_TYPE,
                "fashion.collection.create/create-c26"
            ))
            .expect("record")
            .exists()
    );
}

#[test]
fn replay_returns_original_evidence_after_related_state_changes_and_binds_raw_input() {
    let types = types();
    let catalog = TypeCatalog::new(&types);
    let action = action();
    let invocation = invocation(&action);
    let mut cell = cell();
    let ActionOutcome::Committed { record, .. } =
        commit_action(&mut cell, &catalog, &invocation, &Allow).expect("initial commit")
    else {
        panic!("first invocation")
    };
    put(
        &mut cell,
        FactKey::new("fashion.Season", "SS26"),
        season(false, "OTHER"),
    );
    let before = cell.ledger().len();
    let ActionOutcome::Replayed { record: replayed } =
        commit_action(&mut cell, &catalog, &invocation, &Allow).expect("exact replay")
    else {
        panic!("exact replay")
    };
    assert_eq!(record, replayed);
    assert_eq!(cell.ledger().len(), before);
    let mut other_raw = invocation.clone();
    other_raw
        .input
        .insert("code".to_owned(), norn_ir::RuntimeValue::from("C26"));
    assert_eq!(
        commit_action(&mut cell, &catalog, &other_raw, &Allow)
            .expect_err("different raw invocation")
            .code(),
        "action.idempotency_conflict"
    );
}

#[test]
fn independent_cells_and_retries_produce_identical_derived_records() {
    let types = types();
    let catalog = TypeCatalog::new(&types);
    let action = action();
    let invocation = invocation(&action);
    let mut first = cell();
    let mut second = cell();
    let first = commit_action(&mut first, &catalog, &invocation, &Allow).expect("first execution");
    let second =
        commit_action(&mut second, &catalog, &invocation, &Allow).expect("second execution");
    assert_eq!(first, second, "same definitions, facts, raw input and turn");
}

#[test]
fn rule_trigger_slice_keys_refuse_without_committing_an_action() {
    let types = types();
    let catalog = TypeCatalog::new(&types);
    for key in [
        norn_ir::SliceKey::TriggerObject,
        norn_ir::SliceKey::TriggerBefore,
        norn_ir::SliceKey::TriggerAfter,
        norn_ir::SliceKey::TriggerChanges,
    ] {
        let mut action = action();
        action.slice.first_mut().expect("declared season").key = key;
        let invocation = invocation(&action);
        let mut cell = cell();
        let before = cell.ledger().len();
        let refusal = commit_action(&mut cell, &catalog, &invocation, &Allow)
            .expect_err("actions have no rule trigger");
        assert_eq!(refusal.code(), "action.slice_unavailable");
        assert_eq!(cell.ledger().len(), before);
        assert!(
            cell.load(&FactKey::new("fashion.Collection", "C26"))
                .expect("target remains absent")
                .values
                .is_none()
        );
    }
}

#[test]
fn duplicate_aliases_cannot_materialize_more_than_the_slice_byte_budget() {
    let types = types();
    let catalog = TypeCatalog::new(&types);
    let mut action = action();
    let mut alias = action.slice.first().expect("declared season").clone();
    alias.name = "season_alias".to_owned();
    action.slice.push(alias);
    let invocation = invocation(&action);
    let mut cell = cell();
    let mut large = season(true, "NORN");
    large.insert(
        "notes".to_owned(),
        norn_ir::RuntimeValue::from("n".repeat(140_000)),
    );
    put(&mut cell, FactKey::new("fashion.Season", "SS26"), large);
    let before = cell.ledger().len();
    assert_eq!(
        commit_action(&mut cell, &catalog, &invocation, &Allow)
            .expect_err("each alias must consume bytes before cloning")
            .code(),
        "action.slice_unavailable"
    );
    assert_eq!(cell.ledger().len(), before);
}

struct HiddenRead;
impl ActionAuthorizer for HiddenRead {
    fn authorize(&self, _: &ActionAuthzRequest<'_>) -> Result<(), ActionDenial> {
        Ok(())
    }
    fn authorize_read_type(&self, _: &ActionReadTypeAuthzRequest<'_>) -> Result<(), ActionDenial> {
        Ok(())
    }
}

#[test]
fn hidden_absent_and_oversized_slice_objects_have_the_same_opaque_refusal() {
    let types = types();
    let catalog = TypeCatalog::new(&types);
    let action = action();
    let invocation = invocation(&action);
    let mut normal = cell();
    let mut oversized = cell();
    let mut absent = Cell::new(|command: &Command<JsonObject>| command.payload.clone());
    let mut large = season(true, "NORN");
    large.insert(
        "notes".to_owned(),
        norn_ir::RuntimeValue::from("n".repeat(300_000)),
    );
    put(
        &mut oversized,
        FactKey::new("fashion.Season", "SS26"),
        large,
    );
    for cell in [&mut normal, &mut oversized, &mut absent] {
        let before = cell.ledger().len();
        let refused =
            commit_action(cell, &catalog, &invocation, &HiddenRead).expect_err("hidden read");
        assert_eq!(refused.code(), "action.target_not_found");
        assert_eq!(refused.to_string(), "the target object was not found");
        assert_eq!(cell.ledger().len(), before);
    }
}

enum CommitRace {
    RelatedFact,
    Tombstone,
    LinkSetTooLarge,
    LinkSetWinner,
}

/// Simulates another cell writer committing after evaluation and before CAS.
struct RacingCell {
    cell: RefCell<Cell>,
    race: Option<CommitRace>,
    bounded_race: RefCell<Option<(FactKey, JsonObject)>>,
}
impl CellHost for RacingCell {
    type Event = FactEvent;
    type Command = JsonObject;
    type Input = JsonObject;
    type Error = Infallible;
    fn load(&self, key: &FactKey) -> Result<FactState, Self::Error> {
        self.cell.borrow().load(key)
    }
    fn supports_reference_reads(&self) -> bool {
        true
    }
    fn load_reference(&self, key: &FactKey, _: u64) -> Result<Option<BoundedFact>, Self::Error> {
        let mut state = self.cell.borrow().load(key)?;
        let concrete_type = state.exists().then(|| key.object_type.clone());
        state.values = state.values.map(|_| JsonObject::new());
        Ok(Some(BoundedFact {
            concrete_type,
            state,
        }))
    }
    fn commit_action_references(
        &mut self,
        _: CellCommit<FactEvent, JsonObject, JsonObject>,
        _: norn_kernel::action_commit::ActionReferenceReads,
        _: Option<(&norn_ir::quantity_ledger::QuantityRequest, &str)>,
    ) -> Result<CommitReceipt, CommitError<Self::Error>> {
        panic!("a changed policy proof must never retroactively grant reference precision")
    }
    fn load_bounded(
        &self,
        key: &FactKey,
        max_bytes: u64,
    ) -> Result<Option<BoundedFact>, Self::Error> {
        if self
            .bounded_race
            .borrow()
            .as_ref()
            .is_some_and(|(target, _)| target == key)
        {
            let (target, values) = self.bounded_race.borrow_mut().take().expect("pending race");
            put(&mut self.cell.borrow_mut(), target, values);
        }
        self.cell.borrow().load_bounded(key, max_bytes)
    }
    fn load_link(&self, key: &LinkKey) -> Result<LinkState, Self::Error> {
        self.cell.borrow().load_link(key)
    }
    fn read_link_targets(
        &self,
        source: &FactKey,
        link: &str,
        max_targets: usize,
        max_key_bytes: u64,
    ) -> Result<Option<Vec<LinkReadTarget>>, Self::Error> {
        self.cell
            .borrow()
            .read_link_targets(source, link, max_targets, max_key_bytes)
    }
    #[expect(
        clippy::unwrap_in_result,
        reason = "This test fixture must panic on failed prerequisites rather than simulate a recoverable domain refusal"
    )]
    fn commit(
        &mut self,
        mut commit: CellCommit<FactEvent, JsonObject, JsonObject>,
    ) -> Result<CommitReceipt, CommitError<Self::Error>> {
        match self.race.take() {
            Some(CommitRace::RelatedFact) => put(
                self.cell.get_mut(),
                FactKey::new("fashion.Season", "SS26"),
                season(false, "OTHER"),
            ),
            Some(CommitRace::Tombstone) => {
                let marker = norn_kernel::snapshot_observe::tombstone_key(&FactKey::new(
                    "fashion.Season",
                    "SS26",
                ));
                // The API adapter binds this visibility dependency; a source
                // deletion changes only the marker, not the retained season.
                commit.reads.push(FactExpectation {
                    key: marker.clone(),
                    expected: FactVersion::ABSENT,
                });
                put(self.cell.get_mut(), marker, JsonObject::new());
            }
            Some(race @ (CommitRace::LinkSetTooLarge | CommitRace::LinkSetWinner)) => {
                let read = commit
                    .link_reads
                    .first()
                    .expect("complete prepared link read");
                assert!(
                    read.targets.is_empty(),
                    "initial snapshot was complete and bounded"
                );
                let refusal = CommitError::UnsupportedLinkRead {
                    source_key: read.source.clone(),
                    link: read.link.clone(),
                };
                if matches!(race, CommitRace::LinkSetWinner) {
                    self.cell
                        .get_mut()
                        .commit(commit)
                        .expect("same call won the race");
                }
                return Err(refusal);
            }
            None => {}
        }
        self.cell.get_mut().commit(commit)
    }
    fn perform(&mut self) -> Result<Option<Performed<Self::Input>>, Self::Error> {
        self.cell.get_mut().perform()
    }
    fn observe(&mut self) -> Result<Option<Observed<Self::Input>>, Self::Error> {
        self.cell.get_mut().observe()
    }
    fn clock(&mut self) -> Result<Option<DueTimer<Self::Input>>, Self::Error> {
        self.cell.get_mut().clock()
    }
    fn publish(&mut self, receipt: &CommitReceipt) -> Result<(), Self::Error> {
        self.cell.get_mut().publish(receipt)
    }
}

#[test]
fn reference_precision_is_bound_to_the_read_mode_and_revocation_refuses_atomically() {
    struct ChangingProof {
        precision: std::cell::Cell<bool>,
        after: bool,
    }
    impl ActionAuthorizer for ChangingProof {
        fn supports_reference_lineage(&self) -> bool {
            self.precision.get()
        }
        fn authorize(&self, _: &ActionAuthzRequest<'_>) -> Result<(), ActionDenial> {
            Ok(())
        }
        fn authorize_read_type(
            &self,
            _: &ActionReadTypeAuthzRequest<'_>,
        ) -> Result<(), ActionDenial> {
            Ok(())
        }
        fn authorize_read(&self, _: &ActionReadAuthzRequest<'_>) -> Result<(), ActionDenial> {
            self.precision.set(self.after);
            Ok(())
        }
    }
    let mut types = types();
    types[1].links.push(serde_json::from_value(json!({"id":"season","target":"fashion.Season","cardinality":{"to":"one","from":"many"}})).expect("link"));
    let catalog = TypeCatalog::new(&types);
    let mut action = action();
    action.derive.clear();
    action.preconditions.clear();
    action.create_links = vec![
        serde_json::from_value(
            json!({"link":"season","targetType":"fashion.Season","targetIdField":"season_code"}),
        )
        .expect("reference"),
    ];
    for (initial, after) in [(false, true), (true, false)] {
        let mut host = RacingCell {
            cell: RefCell::new(cell().with_types(types.clone())),
            race: None,
            bounded_race: RefCell::new(None),
        };
        let before = host.cell.borrow().ledger().len();
        let mut call = invocation(&action);
        call.input = object(
            json!({"code":"C26","season_code":"SS26","brand":"NORN","label":"public input"}),
        );
        let authorizer = ChangingProof {
            precision: std::cell::Cell::new(initial),
            after,
        };
        let outcome = commit_action(&mut host, &catalog, &call, &authorizer);
        if initial {
            assert!(
                matches!(outcome, Err(ActionRefusal::TargetNotFound)),
                "{outcome:?}"
            );
            assert_eq!(host.cell.borrow().ledger().len(), before);
        } else {
            assert!(
                matches!(outcome, Ok(ActionOutcome::Committed { .. })),
                "{outcome:?}"
            );
            assert_eq!(host.cell.borrow().ledger().len(), before + 1);
        }
    }
}

#[test]
fn stale_related_version_refuses_at_atomic_commit_after_condition_passes() {
    let types = types();
    let catalog = TypeCatalog::new(&types);
    let action = action();
    let invocation = invocation(&action);
    let mut host = RacingCell {
        cell: RefCell::new(cell()),
        race: Some(CommitRace::RelatedFact),
        bounded_race: RefCell::new(None),
    };
    let before = host.cell.borrow().ledger().len();
    let refusal =
        commit_action(&mut host, &catalog, &invocation, &Allow).expect_err("related object raced");
    assert_eq!(refusal.code(), "action.precondition_stale_read");
    assert_eq!(
        host.cell.borrow().ledger().len(),
        before + 1,
        "only the competing commit exists"
    );
    assert!(
        !host
            .cell
            .borrow()
            .load(&FactKey::new("fashion.Collection", "C26"))
            .expect("target")
            .exists()
    );
}

#[test]
fn target_alias_refuses_a_change_between_action_authorization_and_slice_read() {
    let types = types();
    let catalog = TypeCatalog::new(&types);
    let mut action = action();
    action.slice.push(
        serde_json::from_value(json!({
            "name": "self",
            "objectType": "fashion.Collection",
            "key": {"property": {"from": "input", "property": "code"}}
        }))
        .expect("target alias"),
    );
    let invocation = invocation(&action);
    let target = FactKey::new("fashion.Collection", "C26");
    let competing_values = object(json!({
        "code": "C26", "season_code": "SS26", "brand": "OTHER", "label": "competing"
    }));
    let mut host = RacingCell {
        cell: RefCell::new(cell()),
        race: None,
        bounded_race: RefCell::new(Some((target.clone(), competing_values.clone()))),
    };
    let before = host.cell.borrow().ledger().len();
    let refusal = commit_action(&mut host, &catalog, &invocation, &Allow)
        .expect_err("target changed after initial action authorization");
    assert_eq!(refusal.code(), "action.precondition_stale_read");
    let current = host.cell.borrow();
    assert_eq!(current.ledger().len(), before + 1);
    assert_eq!(
        current.load(&target).expect("competing target").values,
        Some(competing_values)
    );
    assert!(
        !current
            .load(&FactKey::new(
                ACTION_CALL_TYPE,
                format!("{}/{}", action.action_id, invocation.idempotency_key)
            ))
            .expect("no action receipt")
            .exists()
    );
}

#[test]
fn tombstone_dependency_conflict_names_the_original_slice_fact() {
    let types = types();
    let catalog = TypeCatalog::new(&types);
    let action = action();
    let invocation = invocation(&action);
    let mut host = RacingCell {
        cell: RefCell::new(cell()),
        race: Some(CommitRace::Tombstone),
        bounded_race: RefCell::new(None),
    };
    let before = host.cell.borrow().ledger().len();
    let refusal = commit_action(&mut host, &catalog, &invocation, &Allow)
        .expect_err("source deletion invalidates the slice read");
    assert!(
        matches!(refusal, ActionRefusal::StalePreconditionRead { key, link: None }
        if key == FactKey::new("fashion.Season", "SS26"))
    );
    assert_eq!(host.cell.borrow().ledger().len(), before + 1);
    assert!(
        !host
            .load(&FactKey::new("fashion.Collection", "C26"))
            .expect("target")
            .exists()
    );
}

#[test]
fn commit_time_unbounded_link_reads_are_stale_and_still_replay_a_winner() {
    let mut types = types();
    types.first_mut().expect("season").links = serde_json::from_value(json!([{
        "id":"collections", "target":"fashion.Collection",
        "cardinality":{"to":"many", "from":"one"}
    }]))
    .expect("link declaration");
    let catalog = TypeCatalog::new(&types);
    let mut action = action();
    action.slice.push(
        serde_json::from_value(json!({
            "name":"collections", "objectType":"fashion.Collection",
            "key":{"link":{"from":"season", "link":"collections"}}
        }))
        .expect("link slice"),
    );
    action.preconditions.push(norn_ir::ZenExpression(
        "len(slice.collections) == 0".to_owned(),
    ));
    let invocation = invocation(&action);
    for race in [CommitRace::LinkSetTooLarge, CommitRace::LinkSetWinner] {
        let winner = matches!(race, CommitRace::LinkSetWinner);
        let mut host = RacingCell {
            cell: RefCell::new(cell().with_types(types.clone())),
            race: Some(race),
            bounded_race: RefCell::new(None),
        };
        let before = host.cell.borrow().ledger().len();
        let outcome = commit_action(&mut host, &catalog, &invocation, &Allow);
        if winner {
            assert!(matches!(outcome, Ok(ActionOutcome::Replayed { .. })));
            assert_eq!(host.cell.borrow().ledger().len(), before + 1);
        } else {
            assert!(
                matches!(outcome, Err(ActionRefusal::StalePreconditionRead { key, link: Some(link) })
                if key == FactKey::new("fashion.Season", "SS26") && link == "collections")
            );
            assert_eq!(host.cell.borrow().ledger().len(), before);
            assert!(
                !host
                    .load(&FactKey::new("fashion.Collection", "C26"))
                    .expect("target")
                    .exists()
            );
        }
    }
}

struct DenyCollectionReads {
    attempts: std::cell::Cell<usize>,
}

impl ActionAuthorizer for DenyCollectionReads {
    fn authorize(&self, _: &ActionAuthzRequest<'_>) -> Result<(), ActionDenial> {
        Ok(())
    }

    fn authorize_read(&self, _: &ActionReadAuthzRequest<'_>) -> Result<(), ActionDenial> {
        Ok(())
    }

    fn authorize_read_type(
        &self,
        request: &ActionReadTypeAuthzRequest<'_>,
    ) -> Result<(), ActionDenial> {
        if request.object_type.as_str() == "fashion.Collection" {
            self.attempts.set(self.attempts.get().saturating_add(1));
            Err(ActionDenial::Invisible)
        } else {
            Ok(())
        }
    }
}

fn optional_link_action() -> ActionRoot {
    let mut document = serde_json::to_value(action()).expect("action JSON");
    document["inputSchema"]["properties"]["optional_season"] = json!({"type":["string", "null"]});
    document["slice"] = json!([
        {"name":"season", "objectType":"fashion.Season", "optional":true,
         "key":{"property":{"from":"input", "property":"optional_season"}}},
        {"name":"collections", "objectType":"fashion.Collection",
         "key":{"link":{"from":"season", "link":"collections"}}}
    ]);
    document["preconditions"] = json!(["len(slice.collections) == 0"]);
    serde_json::from_value(document).expect("optional-source action")
}

#[test]
fn omitted_optional_sources_skip_link_target_authorization_but_present_sources_do_not() {
    let mut types = types();
    types.first_mut().expect("season").links = serde_json::from_value(json!([{
        "id":"collections", "target":"fashion.Collection",
        "cardinality":{"to":"many", "from":"one"}
    }]))
    .expect("declared link");
    types.last_mut().expect("collection").properties.push(
        serde_json::from_value(json!({
            "id":"optional_season", "type":{"kind":"text"}, "constraints":{"nullable":true}
        }))
        .expect("optional input property"),
    );
    let catalog = TypeCatalog::new(&types);
    let action = optional_link_action();
    for selector in [
        None,
        Some(norn_ir::RuntimeValue::Null),
        Some(norn_ir::RuntimeValue::from("SS26")),
    ] {
        let present = selector
            .as_ref()
            .is_some_and(|value| value != &norn_ir::RuntimeValue::Null);
        let mut invocation = invocation(&action);
        if let Some(selector) = selector {
            invocation
                .input
                .insert("optional_season".to_owned(), selector);
        }
        let authorizer = DenyCollectionReads {
            attempts: std::cell::Cell::new(0),
        };
        let mut storage = cell();
        let before = storage.ledger().len();
        let result = commit_action(&mut storage, &catalog, &invocation, &authorizer);
        if present {
            assert_eq!(authorizer.attempts.get(), 1);
            assert_eq!(
                result
                    .expect_err("present source still requires target collection access")
                    .code(),
                "action.target_not_found"
            );
            assert_eq!(storage.ledger().len(), before);
        } else {
            assert_eq!(
                authorizer.attempts.get(),
                0,
                "an empty selection authorizes no target type"
            );
            let ActionOutcome::Committed { record, .. } =
                result.expect("omitted source produces an empty list")
            else {
                panic!("first call commits");
            };
            assert_eq!(record.precondition_outcomes.len(), 1);
            let outcome = record
                .precondition_outcomes
                .first()
                .expect("empty-list condition");
            assert_eq!(outcome.expression, "len(slice.collections) == 0");
            assert!(outcome.satisfied);
        }
    }
}

#[test]
fn create_link_consumes_only_fields_without_a_declared_scalar_property() {
    for stored in [false, true] {
        let mut definitions = types();
        if !stored {
            definitions[1]
                .properties
                .retain(|property| property.id.as_str() != "season_code");
        }
        definitions[1].links.push(
            serde_json::from_value(json!({
                "id":"season", "target":"fashion.Season", "cardinality":{"to":"one","from":"many"}
            }))
            .expect("link"),
        );
        let catalog = TypeCatalog::new(&definitions);
        let mut create = action();
        create.derive.clear();
        create.preconditions.clear();
        create.create_links = vec![
            serde_json::from_value(json!({
                "link":"season", "targetType":"fashion.Season", "targetIdField":"season_code"
            }))
            .expect("binding"),
        ];
        let mut call = invocation(&create);
        call.input = object(
            json!({"code":"C26","season_code":"SS26","brand":"NORN","label":"public input"}),
        );
        let mut host = cell().with_types(definitions.clone());
        let outcome = commit_action(&mut host, &catalog, &call, &Allow);
        assert!(
            matches!(outcome, Ok(ActionOutcome::Committed { .. })),
            "{outcome:?}"
        );
        let target = FactKey::new("fashion.Collection", "C26");
        let state = host.load(&target).expect("stored object");
        assert_eq!(
            state
                .values()
                .expect("object values")
                .contains_key("season_code"),
            stored
        );
        assert!(
            host.load_link(&LinkKey::new(
                target,
                "season",
                FactKey::new("fashion.Season", "SS26")
            ))
            .expect("stored link")
            .exists()
        );
    }
}
