#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    missing_docs,
    reason = "integration-test fixture and assertion helpers"
)]

//! End-to-end acceptance tests for issue #1586:
//! - Generic bounded `SliceKey::Predicate` fenced reads (`TXN-PHANTOM`)
//! - Checked `DecisionOutput.links: Vec<LinkChange>` for atomic multi-object
//!   `synchronousRule` writers (lifting the static 16-link cap)

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::convert::Infallible;

use norn_ir::{ActionRoot, ActionRuleRef, ObjectTypeDef, RuleRoot};
use norn_kernel::cell_host::JsonObject;
use norn_kernel::{
    ActionAuthorizer, ActionAuthzRequest, ActionCall, ActionDenial, ActionOutcome,
    ActionReadAuthzRequest, ActionReadTypeAuthzRequest, CellCommit, CellHost, Command, CommitError,
    CommitReceipt, DecisionBinding, DecisionTurn, DueTimer, FactEvent, FactKey, FactMutation,
    FactState, FactWrite, LinkKey, LinkState, LogicalTime, Observed, Performed, PredicateObject,
    PredicateQuery, SynchronousRule, TypeCatalog, World, commit_action,
};
use norn_testkit::cell_host::MemoryCell;
use serde_json::{Value, json};

type Cell = MemoryCell<FactEvent, JsonObject, JsonObject>;

fn object(value: Value) -> JsonObject {
    serde_json::from_value(value).expect("exact fixture object")
}

fn schedule_types() -> Vec<ObjectTypeDef> {
    serde_json::from_value(json!([
        {
            "name": "schedule.Agent",
            "properties": [
                {"id": "agent_id", "type": {"kind": "text"}}
            ]
        },
        {
            "name": "schedule.Plan",
            "properties": [
                {"id": "plan_id", "type": {"kind": "text"}},
                {"id": "status", "type": {"kind": "enum", "values": [
                    {"value": "draft"},
                    {"value": "committed"}
                ]}, "default": "draft"}
            ],
            "lifecycles": [{
                "property": "status",
                "initial": ["draft"],
                "terminal": ["committed"],
                "transitions": [{
                    "id": "commit_plan",
                    "from": ["draft"],
                    "to": "committed"
                }]
            }]
        },
        {
            "name": "schedule.Commitment",
            "properties": [
                {"id": "commitment_id", "type": {"kind": "text"}},
                {"id": "agent_id", "type": {"kind": "text"}},
                {"id": "starts_at", "type": {"kind": "timestamp", "timezone": "utc"}},
                {"id": "ends_at", "type": {"kind": "timestamp", "timezone": "utc"}},
                {"id": "start_min", "type": {"kind": "integer"}},
                {"id": "end_min", "type": {"kind": "integer"}},
                {"id": "hours", "type": {"kind": "decimal", "scale": 2, "additivity": {"kind": "additive"}}}
            ],
            "links": [
                {"id": "for_agent", "target": "schedule.Agent", "cardinality": {"to": "one", "from": "many"}},
                {"id": "for_plan", "target": "schedule.Plan", "cardinality": {"to": "one", "from": "many"}}
            ]
        }
    ]))
    .expect("typed schedule entities")
}

fn batch_writer_rule(facts_expr: &str, links_expr: &str, link_enum: &[&str]) -> SynchronousRule {
    let root: RuleRoot = serde_json::from_value(json!({
        "ruleId": "schedule.batch_commitments",
        "ruleVersion": "2.0",
        "contract": "decide",
        "scopeRequired": ["orgId", "environmentId"],
        "inputSchema": {
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "slice": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "agent": {
                            "type": "object",
                            "additionalProperties": false,
                            "properties": {
                                "agent_id": {"type": "string"}
                            },
                            "required": ["agent_id"]
                        },
                        "existing": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "additionalProperties": false,
                                "properties": {
                                    "commitment_id": {"type": "string"},
                                    "agent_id": {"type": "string"},
                                    "starts_at": {"type": "string"},
                                    "ends_at": {"type": "string"},
                                    "start_min": {"type": "integer"},
                                    "end_min": {"type": "integer"},
                                    "hours": {"type": "number"}
                                },
                                "required": ["commitment_id", "agent_id", "starts_at", "ends_at", "start_min", "end_min", "hours"]
                            }
                        }
                    },
                    "required": ["agent", "existing"]
                },
                "event": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "input": {
                            "type": "object",
                            "additionalProperties": false,
                            "properties": {
                                "plan_id": {"type": "string"},
                                "agent_id": {"type": "string"},
                                "window_start": {"type": "string"},
                                "window_end": {"type": "string"},
                                "slot_start": {"type": "string"},
                                "slot_end": {"type": "string"},
                                "slot_start_min": {"type": "integer"},
                                "slot_end_min": {"type": "integer"}
                            },
                            "required": ["plan_id", "agent_id", "window_start", "window_end", "slot_start", "slot_end", "slot_start_min", "slot_end_min"]
                        },
                        "actor": {"type": "string"},
                        "operationId": {"type": "string"}
                    },
                    "required": ["input", "actor", "operationId"]
                }
            },
            "required": ["slice", "event"]
        },
        "outputSchema": {
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "facts": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "additionalProperties": false,
                        "properties": {
                            "op": {"type": "string", "enum": ["create"]},
                            "objectType": {"type": "string", "enum": ["schedule.Commitment"]},
                            "key": {"type": "string"},
                            "values": {
                                "type": "object",
                                "additionalProperties": false,
                                "properties": {
                                    "commitment_id": {"type": "string"},
                                    "agent_id": {"type": "string"},
                                    "starts_at": {"type": "string"},
                                    "ends_at": {"type": "string"},
                                    "start_min": {"type": "integer"},
                                    "end_min": {"type": "integer"},
                                    "hours": {"type": "number"}
                                },
                                "required": ["commitment_id", "agent_id", "starts_at", "ends_at", "start_min", "end_min", "hours"]
                            }
                        },
                        "required": ["op", "objectType", "key", "values"]
                    }
                },
                "links": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "additionalProperties": false,
                        "properties": {
                            "op": {"type": "string", "enum": ["add"]},
                            "sourceType": {"type": "string", "enum": ["schedule.Commitment"]},
                            "sourceKey": {"type": "string"},
                            "link": {"type": "string", "enum": link_enum},
                            "targetType": {"type": "string", "enum": ["schedule.Agent", "schedule.Plan"]},
                            "targetKey": {"type": "string"}
                        },
                        "required": ["op", "sourceType", "sourceKey", "link", "targetType", "targetKey"]
                    }
                },
                "commands": {
                    "type": "array",
                    "items": {"type": "object", "additionalProperties": false, "properties": {}}
                },
                "timers": {
                    "type": "array",
                    "items": {"type": "object", "additionalProperties": false, "properties": {}}
                }
            },
            "required": ["facts", "links", "commands", "timers"]
        },
        "graph": {
            "contentType": "application/vnd.gorules.decision",
            "nodes": [
                {"id": "in", "name": "input", "type": "inputNode"},
                {
                    "id": "decision",
                    "name": "decision",
                    "type": "decisionTableNode",
                    "content": {
                        "hitPolicy": "first",
                        "inputs": [{"id": "condition", "name": "condition", "type": "expression"}],
                        "outputs": [
                            {"id": "out_facts", "name": "facts", "field": "facts"},
                            {"id": "out_links", "name": "links", "field": "links"},
                            {"id": "out_commands", "name": "commands", "field": "commands"},
                            {"id": "out_timers", "name": "timers", "field": "timers"}
                        ],
                        "rules": [{
                            "_id": "row_0",
                            "condition": "",
                            "out_facts": facts_expr,
                            "out_links": links_expr,
                            "out_commands": "[]",
                            "out_timers": "[]"
                        }]
                    }
                },
                {"id": "out", "name": "output", "type": "outputNode"}
            ],
            "edges": [
                {"id": "edge_0", "sourceId": "in", "targetId": "decision", "type": "edge"},
                {"id": "edge_1", "sourceId": "decision", "targetId": "out", "type": "edge"}
            ]
        }
    }))
    .expect("typed rule root");

    SynchronousRule::admit(
        ActionRuleRef {
            artifact_id: "schedule.batch_commitments.rule".to_owned(),
            version: "1.0.0".to_owned(),
        },
        &root,
        &BTreeMap::new(),
        DecisionBinding {
            profile_id: "norn.limits/1".to_owned(),
            environment_hash: "env-test".to_owned(),
        },
    )
    .expect("admitted synchronous rule")
}

fn schedule_action(max_items: u32, with_duplicate_aggregate: bool) -> ActionRoot {
    let aggregates = if with_duplicate_aggregate {
        json!([{
            "name": "existing_count",
            "objectType": "schedule.Commitment",
            "filter": {"agent_id": "agent_id"},
            "window": {
                "property": "starts_at",
                "start": "window_start",
                "end": "window_end",
                "startInclusive": true,
                "endInclusive": false
            },
            "aggregate": {"kind": "count"},
            "maxItems": max_items
        }])
    } else {
        json!([])
    };
    let mut preconditions = vec![
        "intervals_overlap(timestamp(input.slot_start), timestamp(input.slot_end), timestamp(input.window_start), timestamp(input.window_end))",
        "all(slice.existing, seconds_between(timestamp(#.starts_at), timestamp(#.ends_at)) > 0 and (#.end_min <= input.slot_start_min or #.start_min >= input.slot_end_min))",
    ];
    if with_duplicate_aggregate {
        preconditions.push("aggregates.existing_count >= 0");
    }
    serde_json::from_value(json!({
        "actionId": "schedule.plan.commit",
        "scopeRequired": ["orgId", "environmentId"],
        "idempotent": true,
        "ledgered": true,
        "inputSchema": {
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "plan_id": {"type": "string"},
                "agent_id": {"type": "string"},
                "window_start": {"type": "string"},
                "window_end": {"type": "string"},
                "slot_start": {"type": "string"},
                "slot_end": {"type": "string"},
                "slot_start_min": {"type": "integer"},
                "slot_end_min": {"type": "integer"}
            },
            "required": ["plan_id", "agent_id", "window_start", "window_end", "slot_start", "slot_end", "slot_start_min", "slot_end_min"]
        },
        "outputSchema": {"type": "object", "properties": {}, "additionalProperties": false},
        "slice": [
            {
                "name": "agent",
                "objectType": "schedule.Agent",
                "key": {"property": {"from": "input", "property": "agent_id"}}
            },
            {
                "name": "existing",
                "objectType": "schedule.Commitment",
                "key": {
                    "predicate": {
                        "filter": {"agent_id": "agent_id"},
                        "window": {
                            "property": "starts_at",
                            "start": "window_start",
                            "end": "window_end",
                            "startInclusive": true,
                            "endInclusive": false
                        },
                        "maxItems": max_items
                    }
                }
            }
        ],
        "aggregates": aggregates,
        "preconditions": preconditions,
        "writes": [
            {"entityKind": "schedule.Plan", "change": "modified"},
            {"entityKind": "schedule.Commitment", "change": "created"}
        ],
        "synchronousRule": {
            "artifactId": "schedule.batch_commitments.rule",
            "version": "1.0.0"
        },
        "effectProgram": {
            "kind": "fire_transition",
            "objectKind": "schedule.Plan",
            "objectIdField": "plan_id",
            "entityArtifactId": "schedule.Plan.entity",
            "entityArtifactVersion": "1.0.0",
            "lifecycle": "status",
            "transition": "commit_plan"
        }
    }))
    .expect("typed schedule action")
}

fn hhmm_minutes(iso_utc: &str) -> i64 {
    let time = iso_utc.split('T').nth(1).expect("iso time");
    let hh: i64 = time[0..2].parse().expect("hh");
    let mm: i64 = time[3..5].parse().expect("mm");
    hh * 60 + mm
}

fn schedule_call<'a>(
    action: &'a ActionRoot,
    plan_id: &str,
    slot_start: &str,
    slot_end: &str,
) -> ActionCall<'a> {
    ActionCall {
        action,
        action_artifact_id: "schedule.plan.commit.action",
        action_version: "1.0.0",
        scope: BTreeMap::from([
            ("orgId".to_owned(), "org_demo".to_owned()),
            ("environmentId".to_owned(), "dev".to_owned()),
        ]),
        actor: "scheduler:lead".to_owned(),
        input: object(json!({
            "plan_id": plan_id,
            "agent_id": "A1",
            "window_start": "2026-11-23T00:00:00Z",
            "window_end": "2026-11-24T00:00:00Z",
            "slot_start": slot_start,
            "slot_end": slot_end,
            "slot_start_min": hhmm_minutes(slot_start),
            "slot_end_min": hhmm_minutes(slot_end)
        })),
        idempotency_key: format!("commit-{plan_id}"),
        object_type: None,
        turn: DecisionTurn {
            id: format!("turn-{plan_id}"),
            now: LogicalTime::from_millis(2_000),
            world: World::Main,
            consumes: None,
        },
        trace: None,
    }
}

struct Authorizer {
    hidden_key: Option<&'static str>,
}

impl ActionAuthorizer for Authorizer {
    fn authorize(&self, _: &ActionAuthzRequest<'_>) -> Result<(), ActionDenial> {
        Ok(())
    }
    fn authorize_read(&self, request: &ActionReadAuthzRequest<'_>) -> Result<(), ActionDenial> {
        if self.hidden_key == Some(request.target.key.as_str()) {
            return Err(ActionDenial::Invisible);
        }
        Ok(())
    }
    fn authorize_read_type(&self, _: &ActionReadTypeAuthzRequest<'_>) -> Result<(), ActionDenial> {
        Ok(())
    }
}

const ALLOW: Authorizer = Authorizer { hidden_key: None };

fn put_fact(cell: &mut Cell, object_type: &str, id: &str, values: Value) {
    let key = FactKey::new(object_type, id);
    let expected = cell.load(&key).expect("load").version;
    let mut commit = CellCommit::empty();
    commit.writes.push(FactWrite {
        key,
        expected,
        mutation: FactMutation::Put {
            values: object(values),
        },
    });
    cell.commit(commit).expect("seed commit");
}

fn seeded_cell() -> Cell {
    let mut cell = Cell::new(|command: &Command<JsonObject>| command.payload.clone())
        .with_types(schedule_types());
    put_fact(&mut cell, "schedule.Agent", "A1", json!({"agent_id": "A1"}));
    put_fact(
        &mut cell,
        "schedule.Plan",
        "P1",
        json!({"plan_id": "P1", "status": "draft"}),
    );
    put_fact(
        &mut cell,
        "schedule.Commitment",
        "C0",
        json!({
            "commitment_id": "C0",
            "agent_id": "A1",
            "starts_at": "2026-11-23T09:00:00Z",
            "ends_at": "2026-11-23T10:00:00Z",
            "start_min": 540,
            "end_min": 600,
            "hours": 1.00
        }),
    );
    cell
}

fn batch_expressions(count: usize) -> (String, String) {
    let tags = (1..=count)
        .map(|idx| format!("'{idx:02}'"))
        .collect::<Vec<_>>()
        .join(", ");
    let facts = format!(
        "map([{tags}], {{op: 'create', objectType: 'schedule.Commitment', key: 'C-batch-' + #, values: {{commitment_id: 'C-batch-' + #, agent_id: event.input.agent_id, starts_at: '2026-11-23T11:' + # + ':00Z', ends_at: '2026-11-23T11:' + # + ':30Z', start_min: 660, end_min: 690, hours: 0.50}}}})"
    );
    let links = format!(
        "map([{tags}], {{op: 'add', sourceType: 'schedule.Commitment', sourceKey: 'C-batch-' + #, link: 'for_agent', targetType: 'schedule.Agent', targetKey: event.input.agent_id}})"
    );
    (facts, links)
}

#[test]
fn atomic_multi_object_rule_commits_18_commitments_and_18_links_under_fenced_predicate_slice() {
    let types = schedule_types();
    let (facts_expr, links_expr) = batch_expressions(18);
    let rule = batch_writer_rule(&facts_expr, &links_expr, &["for_agent"]);
    let catalog = TypeCatalog::new(&types).with_action_rules(std::slice::from_ref(&rule));
    let mut cell = seeded_cell();

    // Also verifies deduplication when both a predicate slice and an aggregate
    // read the exact same PredicateQuery.
    let action = schedule_action(50, true);
    let request = schedule_call(
        &action,
        "P1",
        "2026-11-23T11:00:00Z",
        "2026-11-23T12:00:00Z",
    );
    let outcome =
        commit_action(&mut cell, &catalog, &request, &ALLOW).expect("atomic batch commit");
    assert!(matches!(outcome, ActionOutcome::Committed { .. }));

    // Primary transition committed.
    let plan = cell
        .load(&FactKey::new("schedule.Plan", "P1"))
        .expect("plan");
    assert_eq!(
        plan.values
            .as_ref()
            .and_then(|v| v.get("status"))
            .and_then(norn_ir::RuntimeValue::as_str),
        Some("committed")
    );
    // All 18 commitments and all 18 typed links persisted atomically (above old 16-link cap).
    for idx in 1..=18 {
        let key = format!("C-batch-{idx:02}");
        assert!(
            cell.load(&FactKey::new("schedule.Commitment", &key))
                .expect("commitment")
                .exists(),
            "commitment {key} should exist"
        );
        let link = LinkKey::new(
            FactKey::new("schedule.Commitment", &key),
            "for_agent",
            FactKey::new("schedule.Agent", "A1"),
        );
        assert!(
            cell.load_link(&link).expect("link").exists(),
            "link for {key} should exist"
        );
    }
}

#[test]
fn overlapping_commitment_in_predicate_slice_refuses_with_zero_writes() {
    let types = schedule_types();
    let (facts_expr, links_expr) = batch_expressions(2);
    let rule = batch_writer_rule(&facts_expr, &links_expr, &["for_agent"]);
    let catalog = TypeCatalog::new(&types).with_action_rules(std::slice::from_ref(&rule));
    let mut cell = seeded_cell();

    let action = schedule_action(50, false);
    // Overlaps existing C0 (09:00..10:00).
    let request = schedule_call(
        &action,
        "P1",
        "2026-11-23T09:30:00Z",
        "2026-11-23T10:30:00Z",
    );
    let refusal =
        commit_action(&mut cell, &catalog, &request, &ALLOW).expect_err("overlap refused");
    assert!(
        matches!(
            refusal,
            norn_kernel::ActionRefusal::PreconditionFailed { reason: None, .. }
        ),
        "expected runtime overlap refusal (reason: None), got {refusal:?}"
    );
    assert!(
        !cell
            .load(&FactKey::new("schedule.Commitment", "C-batch-01"))
            .expect("load")
            .exists()
    );
    let plan = cell
        .load(&FactKey::new("schedule.Plan", "P1"))
        .expect("plan");
    assert_eq!(
        plan.values
            .as_ref()
            .and_then(|v| v.get("status"))
            .and_then(norn_ir::RuntimeValue::as_str),
        Some("draft")
    );
}

enum Race {
    InsertInsideWindow,
    InsertOutsideWindow,
}

struct RacingCell {
    cell: RefCell<Cell>,
    race: Option<Race>,
}

impl CellHost for RacingCell {
    type Event = FactEvent;
    type Command = JsonObject;
    type Input = JsonObject;
    type Error = Infallible;
    fn load(&self, key: &FactKey) -> Result<FactState, Self::Error> {
        self.cell.borrow().load(key)
    }
    fn load_bounded(
        &self,
        key: &FactKey,
        max_bytes: u64,
    ) -> Result<Option<norn_kernel::BoundedFact>, Self::Error> {
        self.cell.borrow().load_bounded(key, max_bytes)
    }
    fn load_link(&self, key: &LinkKey) -> Result<LinkState, Self::Error> {
        self.cell.borrow().load_link(key)
    }
    fn read_predicate(
        &self,
        predicate: &PredicateQuery,
        max_members: usize,
        max_bytes: u64,
    ) -> Result<Option<Vec<PredicateObject>>, Self::Error> {
        self.cell
            .borrow()
            .read_predicate(predicate, max_members, max_bytes)
    }
    fn commit(
        &mut self,
        commit: CellCommit<FactEvent, JsonObject, JsonObject>,
    ) -> Result<CommitReceipt, CommitError<Self::Error>> {
        match self.race.take() {
            Some(Race::InsertInsideWindow) => put_fact(
                self.cell.get_mut(),
                "schedule.Commitment",
                "C-phantom",
                json!({
                    "commitment_id": "C-phantom",
                    "agent_id": "A1",
                    "starts_at": "2026-11-23T11:15:00Z",
                    "ends_at": "2026-11-23T11:45:00Z",
                    "start_min": 675,
                    "end_min": 705,
                    "hours": 0.50
                }),
            ),
            Some(Race::InsertOutsideWindow) => put_fact(
                self.cell.get_mut(),
                "schedule.Commitment",
                "C-next-day",
                json!({
                    "commitment_id": "C-next-day",
                    "agent_id": "A1",
                    "starts_at": "2026-11-25T11:15:00Z",
                    "ends_at": "2026-11-25T11:45:00Z",
                    "start_min": 675,
                    "end_min": 705,
                    "hours": 0.50
                }),
            ),
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
fn concurrent_phantom_insertion_refuses_with_zero_partial_writes() {
    let types = schedule_types();
    let (facts_expr, links_expr) = batch_expressions(3);
    let rule = batch_writer_rule(&facts_expr, &links_expr, &["for_agent"]);
    let catalog = TypeCatalog::new(&types).with_action_rules(std::slice::from_ref(&rule));
    let action = schedule_action(50, false);

    let mut inside = RacingCell {
        cell: RefCell::new(seeded_cell()),
        race: Some(Race::InsertInsideWindow),
    };
    let request = schedule_call(
        &action,
        "P1",
        "2026-11-23T11:00:00Z",
        "2026-11-23T12:00:00Z",
    );
    let refusal = commit_action(&mut inside, &catalog, &request, &ALLOW)
        .expect_err("phantom inside window must conflict");
    assert_eq!(refusal.code(), "action.aggregate_stale_read");
    // ZERO partial writes: plan stays draft, none of C-batch-1..3 or links exist.
    let cell = inside.cell.borrow();
    assert_eq!(
        cell.load(&FactKey::new("schedule.Plan", "P1"))
            .expect("plan")
            .values
            .as_ref()
            .and_then(|v| v.get("status"))
            .and_then(norn_ir::RuntimeValue::as_str),
        Some("draft")
    );
    for idx in 1..=3 {
        let key = format!("C-batch-{idx:02}");
        assert!(
            !cell
                .load(&FactKey::new("schedule.Commitment", &key))
                .expect("load")
                .exists()
        );
        assert!(
            !cell
                .load_link(&LinkKey::new(
                    FactKey::new("schedule.Commitment", &key),
                    "for_agent",
                    FactKey::new("schedule.Agent", "A1"),
                ))
                .expect("link")
                .exists()
        );
    }

    // Outside window does not conflict.
    let mut outside = RacingCell {
        cell: RefCell::new(seeded_cell()),
        race: Some(Race::InsertOutsideWindow),
    };
    commit_action(&mut outside, &catalog, &request, &ALLOW)
        .expect("insertion outside window must not conflict");
    assert!(
        outside
            .cell
            .borrow()
            .load(&FactKey::new("schedule.Commitment", "C-batch-01"))
            .expect("load")
            .exists()
    );
}

#[test]
fn predicate_slice_bound_and_byte_overflow_refuse_fail_closed_never_truncate() {
    let types = schedule_types();
    let (facts_expr, links_expr) = batch_expressions(1);
    let rule = batch_writer_rule(&facts_expr, &links_expr, &["for_agent"]);
    let catalog = TypeCatalog::new(&types).with_action_rules(std::slice::from_ref(&rule));
    let mut cell = seeded_cell();
    for id in ["C1", "C2"] {
        put_fact(
            &mut cell,
            "schedule.Commitment",
            id,
            json!({
                "commitment_id": id,
                "agent_id": "A1",
                "starts_at": "2026-11-23T08:00:00Z",
                "ends_at": "2026-11-23T08:30:00Z",
                "start_min": 480,
                "end_min": 510,
                "hours": 0.50
            }),
        );
    }
    // 3 matching commitments in window, maxItems = 2 -> refuse fail-closed.
    let action = schedule_action(2, false);
    let request = schedule_call(
        &action,
        "P1",
        "2026-11-23T11:00:00Z",
        "2026-11-23T12:00:00Z",
    );
    let refusal = commit_action(&mut cell, &catalog, &request, &ALLOW)
        .expect_err("3 matches exceed maxItems 2");
    assert_eq!(refusal.code(), "action.slice_unavailable");
    assert!(
        !cell
            .load(&FactKey::new("schedule.Commitment", "C-batch-01"))
            .expect("load")
            .exists()
    );

    // Byte bound overflow: 3 records with ~100 KiB payload > 256 KiB slice cap.
    let padding = "x".repeat(100_000);
    for id in ["C0", "C1", "C2"] {
        put_fact(
            &mut cell,
            "schedule.Commitment",
            id,
            json!({
                "commitment_id": id,
                "agent_id": "A1",
                "starts_at": "2026-11-23T08:00:00Z",
                "ends_at": "2026-11-23T08:30:00Z",
                "start_min": 480,
                "end_min": 510,
                "hours": 0.50,
                "note": padding
            }),
        );
    }
    let big_action = schedule_action(50, false);
    let refusal = commit_action(
        &mut cell,
        &catalog,
        &schedule_call(
            &big_action,
            "P1",
            "2026-11-23T11:00:00Z",
            "2026-11-23T12:00:00Z",
        ),
        &ALLOW,
    )
    .expect_err("oversized predicate slice must refuse");
    assert_eq!(refusal.code(), "action.slice_unavailable");
}

#[test]
fn unauthorized_predicate_member_and_invalid_decision_links_refuse_with_zero_writes() {
    let types = schedule_types();
    let (facts_expr, links_expr) = batch_expressions(2);
    let rule = batch_writer_rule(&facts_expr, &links_expr, &["for_agent"]);
    let catalog = TypeCatalog::new(&types).with_action_rules(std::slice::from_ref(&rule));
    let mut cell = seeded_cell();
    let action = schedule_action(50, false);
    let request = schedule_call(
        &action,
        "P1",
        "2026-11-23T11:00:00Z",
        "2026-11-23T12:00:00Z",
    );

    // 1. Unauthorized predicate member refuses as target_not_found with zero writes.
    let hidden = Authorizer {
        hidden_key: Some("C0"),
    };
    let refusal = commit_action(&mut cell, &catalog, &request, &hidden).expect_err("hidden member");
    assert_eq!(refusal.code(), "action.target_not_found");
    assert!(
        !cell
            .load(&FactKey::new("schedule.Commitment", "C-batch-01"))
            .expect("load")
            .exists()
    );

    // 2. Dangling link target (uncaptured agent A-MISSING) refuses with zero writes.
    let bad_target_links = "[{op: 'add', sourceType: 'schedule.Commitment', sourceKey: 'C-batch-01', link: 'for_agent', targetType: 'schedule.Agent', targetKey: 'A-MISSING'}]";
    let bad_rule = batch_writer_rule(&facts_expr, bad_target_links, &["for_agent"]);
    let bad_catalog = TypeCatalog::new(&types).with_action_rules(std::slice::from_ref(&bad_rule));
    let refusal = commit_action(&mut cell, &bad_catalog, &request, &ALLOW)
        .expect_err("dangling link target must refuse");
    assert_eq!(refusal.code(), "decision.invalid_output");
    assert!(
        !cell
            .load(&FactKey::new("schedule.Commitment", "C-batch-01"))
            .expect("load")
            .exists()
    );

    // 3. Cardinality violation: `for_agent` is to-one, emitting two targets for the same commitment refuses with zero writes.
    put_fact(&mut cell, "schedule.Agent", "A2", json!({"agent_id": "A2"}));
    // Capture A2 by putting it in the slice via an extra slice entry? Even simpler: use `for_plan` with two plans?
    // Or emit two links from C-batch-1 to A1 and A2 via `commit_decision` directly, AND test in action via two captured agents!
    let mut two_agent_action = schedule_action(50, false);
    two_agent_action.slice.push(
        serde_json::from_value(json!({
            "name": "all_agents",
            "objectType": "schedule.Agent",
            "key": {"predicate": {"maxItems": 10}}
        }))
        .expect("predicate slice for all agents"),
    );
    let card_links = "[{op: 'add', sourceType: 'schedule.Commitment', sourceKey: 'C-batch-01', link: 'for_agent', targetType: 'schedule.Agent', targetKey: 'A1'}, {op: 'add', sourceType: 'schedule.Commitment', sourceKey: 'C-batch-01', link: 'for_agent', targetType: 'schedule.Agent', targetKey: 'A2'}]";
    // Build a rule whose inputSchema includes `all_agents` in slice.
    let mut card_rule_json: Value = serde_json::to_value(
        serde_json::from_value::<RuleRoot>(json!({
            "ruleId": "schedule.batch_commitments",
            "ruleVersion": "2.0",
            "contract": "decide",
            "scopeRequired": ["orgId", "environmentId"],
            "inputSchema": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "slice": {
                        "type": "object",
                        "additionalProperties": false,
                        "properties": {
                            "agent": {"type": "object", "additionalProperties": false, "properties": {"agent_id": {"type": "string"}}, "required": ["agent_id"]},
                            "existing": {"type": "array", "items": {"type": "object", "additionalProperties": false, "properties": {"commitment_id": {"type": "string"}, "agent_id": {"type": "string"}, "starts_at": {"type": "string"}, "ends_at": {"type": "string"}, "start_min": {"type": "integer"}, "end_min": {"type": "integer"}, "hours": {"type": "number"}}, "required": ["commitment_id", "agent_id", "starts_at", "ends_at", "start_min", "end_min", "hours"]}},
                            "all_agents": {"type": "array", "items": {"type": "object", "additionalProperties": false, "properties": {"agent_id": {"type": "string"}}, "required": ["agent_id"]}}
                        },
                        "required": ["agent", "existing", "all_agents"]
                    },
                    "event": {
                        "type": "object",
                        "additionalProperties": false,
                        "properties": {
                            "input": {"type": "object", "additionalProperties": false, "properties": {"plan_id": {"type": "string"}, "agent_id": {"type": "string"}, "window_start": {"type": "string"}, "window_end": {"type": "string"}, "slot_start": {"type": "string"}, "slot_end": {"type": "string"}, "slot_start_min": {"type": "integer"}, "slot_end_min": {"type": "integer"}}, "required": ["plan_id", "agent_id", "window_start", "window_end", "slot_start", "slot_end", "slot_start_min", "slot_end_min"]},
                            "actor": {"type": "string"},
                            "operationId": {"type": "string"}
                        },
                        "required": ["input", "actor", "operationId"]
                    }
                },
                "required": ["slice", "event"]
            },
            "outputSchema": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "facts": {"type": "array", "items": {"type": "object", "additionalProperties": false, "properties": {"op": {"type": "string", "enum": ["create"]}, "objectType": {"type": "string", "enum": ["schedule.Commitment"]}, "key": {"type": "string"}, "values": {"type": "object", "additionalProperties": false, "properties": {"commitment_id": {"type": "string"}, "agent_id": {"type": "string"}, "starts_at": {"type": "string"}, "ends_at": {"type": "string"}, "start_min": {"type": "integer"}, "end_min": {"type": "integer"}, "hours": {"type": "number"}}, "required": ["commitment_id", "agent_id", "starts_at", "ends_at", "start_min", "end_min", "hours"]}}, "required": ["op", "objectType", "key", "values"]}},
                    "links": {"type": "array", "items": {"type": "object", "additionalProperties": false, "properties": {"op": {"type": "string", "enum": ["add"]}, "sourceType": {"type": "string", "enum": ["schedule.Commitment"]}, "sourceKey": {"type": "string"}, "link": {"type": "string", "enum": ["for_agent"]}, "targetType": {"type": "string", "enum": ["schedule.Agent"]}, "targetKey": {"type": "string"}}, "required": ["op", "sourceType", "sourceKey", "link", "targetType", "targetKey"]}},
                    "commands": {"type": "array", "items": {"type": "object", "additionalProperties": false, "properties": {}}},
                    "timers": {"type": "array", "items": {"type": "object", "additionalProperties": false, "properties": {}}}
                },
                "required": ["facts", "links", "commands", "timers"]
            },
            "graph": {
                "contentType": "application/vnd.gorules.decision",
                "nodes": [
                    {"id": "in", "name": "input", "type": "inputNode"},
                    {"id": "decision", "name": "decision", "type": "decisionTableNode", "content": {"hitPolicy": "first", "inputs": [{"id": "condition", "name": "condition", "type": "expression"}], "outputs": [{"id": "out_facts", "name": "facts", "field": "facts"}, {"id": "out_links", "name": "links", "field": "links"}, {"id": "out_commands", "name": "commands", "field": "commands"}, {"id": "out_timers", "name": "timers", "field": "timers"}], "rules": [{"_id": "row_0", "condition": "", "out_facts": facts_expr, "out_links": card_links, "out_commands": "[]", "out_timers": "[]"}]}},
                    {"id": "out", "name": "output", "type": "outputNode"}
                ],
                "edges": [
                    {"id": "edge_0", "sourceId": "in", "targetId": "decision", "type": "edge"},
                    {"id": "edge_1", "sourceId": "decision", "targetId": "out", "type": "edge"}
                ]
            }
        }))
        .expect("card rule root"),
    )
    .expect("json");
    let card_root: RuleRoot = serde_json::from_value(card_rule_json.take()).expect("card rule");
    let card_rule = SynchronousRule::admit(
        ActionRuleRef {
            artifact_id: "schedule.batch_commitments.rule".to_owned(),
            version: "1.0.0".to_owned(),
        },
        &card_root,
        &BTreeMap::new(),
        DecisionBinding {
            profile_id: "norn.limits/1".to_owned(),
            environment_hash: "env-test".to_owned(),
        },
    )
    .expect("admitted card rule");
    let card_catalog = TypeCatalog::new(&types).with_action_rules(std::slice::from_ref(&card_rule));
    let card_call = schedule_call(
        &two_agent_action,
        "P1",
        "2026-11-23T11:00:00Z",
        "2026-11-23T12:00:00Z",
    );
    let refusal = commit_action(&mut cell, &card_catalog, &card_call, &ALLOW)
        .expect_err("to-one cardinality violation must refuse");
    assert_eq!(refusal.code(), "decision.invalid_output");
    assert!(
        refusal.to_string().contains("cardinality"),
        "expected link cardinality refusal, got: {refusal}"
    );
    // Zero partial writes.
    assert!(
        !cell
            .load(&FactKey::new("schedule.Commitment", "C-batch-01"))
            .expect("load")
            .exists()
    );
    assert_eq!(
        cell.load(&FactKey::new("schedule.Plan", "P1"))
            .expect("plan")
            .values
            .as_ref()
            .and_then(|v| v.get("status"))
            .and_then(norn_ir::RuntimeValue::as_str),
        Some("draft")
    );
}
