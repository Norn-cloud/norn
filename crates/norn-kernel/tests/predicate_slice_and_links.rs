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
    ActionReadAuthzRequest, ActionReadTypeAuthzRequest, ActionRefusal, CellCommit, CellHost,
    Command, CommitError, CommitReceipt, DecisionBinding, DecisionRefusal, DecisionTurn, DueTimer,
    EnvelopeKind, EnvelopeLimits, FactEvent, FactKey, FactMutation, FactSlice, FactState,
    FactWrite, LinkKey, LinkMutation, LinkState, LinkWrite, LogicalTime, Observed, Performed,
    PredicateObject, PredicateQuery, SynchronousRule, TurnEnvelopeLimits, TurnEnvelopeMeter,
    TypeCatalog, World, commit_action, commit_action_with_meter, commit_decision,
    commit_decision_with_meter, decision_cell_commit, decision_cell_commit_with_host,
    decision_cell_commit_with_host_and_meter, load_partition_slice, prepare_decision,
    prepare_decision_with_limits, prepare_decision_with_source_owned_targets,
};
use norn_testkit::cell_host::MemoryCell;
use serde_json::{Value, json};

type Cell = MemoryCell<FactEvent, JsonObject, JsonObject>;
type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn object(value: Value) -> TestResult<JsonObject> {
    Ok(serde_json::from_value(value)?)
}

fn schedule_types() -> TestResult<Vec<ObjectTypeDef>> {
    Ok(serde_json::from_value(json!([
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
    ]))?)
}

fn writer_input_schema(include_all_agents: bool) -> Value {
    let mut slice_props = serde_json::Map::from_iter([
        (
            "agent".to_owned(),
            json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {"agent_id": {"type": "string"}},
                "required": ["agent_id"]
            }),
        ),
        (
            "existing".to_owned(),
            json!({
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
            }),
        ),
    ]);
    let mut slice_req = vec!["agent", "existing"];
    if include_all_agents {
        slice_props.insert(
            "all_agents".to_owned(),
            json!({
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {"agent_id": {"type": "string"}},
                    "required": ["agent_id"]
                }
            }),
        );
        slice_req.push("all_agents");
    }
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "slice": {
                "type": "object",
                "additionalProperties": false,
                "properties": slice_props,
                "required": slice_req
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
    })
}

fn writer_output_schema(op_enum: &[&str], link_enum: &[&str], target_enum: &[&str]) -> Value {
    json!({
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
                        "op": {"type": "string", "enum": op_enum},
                        "sourceType": {"type": "string", "enum": ["schedule.Commitment"]},
                        "sourceKey": {"type": "string"},
                        "link": {"type": "string", "enum": link_enum},
                        "targetType": {"type": "string", "enum": target_enum},
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
    })
}

fn admit_writer_rule(
    facts_expr: &str,
    links_expr: &str,
    op_enum: &[&str],
    link_enum: &[&str],
    target_enum: &[&str],
    include_all_agents: bool,
) -> TestResult<SynchronousRule> {
    let root: RuleRoot = serde_json::from_value(json!({
        "ruleId": "schedule.batch_commitments",
        "ruleVersion": "2.0",
        "contract": "decide",
        "scopeRequired": ["orgId", "environmentId"],
        "inputSchema": writer_input_schema(include_all_agents),
        "outputSchema": writer_output_schema(op_enum, link_enum, target_enum),
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
    }))?;

    Ok(SynchronousRule::admit(
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
    )?)
}

fn batch_writer_rule(
    facts_expr: &str,
    links_expr: &str,
    link_enum: &[&str],
) -> TestResult<SynchronousRule> {
    admit_writer_rule(
        facts_expr,
        links_expr,
        &["add"],
        link_enum,
        &["schedule.Agent", "schedule.Plan"],
        false,
    )
}

fn schedule_action(max_items: u32, with_duplicate_aggregate: bool) -> TestResult<ActionRoot> {
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
    Ok(serde_json::from_value(json!({
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
    }))?)
}

fn hhmm_minutes(iso_utc: &str) -> TestResult<i64> {
    let time = iso_utc.split('T').nth(1).ok_or("missing iso time")?;
    let mut parts = time.split(':');
    let hh: i64 = parts.next().ok_or("missing hh")?.parse()?;
    let mm: i64 = parts.next().ok_or("missing mm")?.parse()?;
    Ok(hh.saturating_mul(60).saturating_add(mm))
}

fn schedule_call<'a>(
    action: &'a ActionRoot,
    plan_id: &str,
    slot_start: &str,
    slot_end: &str,
) -> TestResult<ActionCall<'a>> {
    Ok(ActionCall {
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
            "slot_start_min": hhmm_minutes(slot_start)?,
            "slot_end_min": hhmm_minutes(slot_end)?
        }))?,
        idempotency_key: format!("commit-{plan_id}"),
        object_type: None,
        turn: DecisionTurn {
            id: format!("turn-{plan_id}"),
            now: LogicalTime::from_millis(2_000),
            world: World::Main,
            consumes: None,
        },
        trace: None,
    })
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

fn put_fact(cell: &mut Cell, object_type: &str, id: &str, values: Value) -> TestResult {
    let key = FactKey::new(object_type, id);
    let expected = cell.load(&key)?.version;
    let mut commit = CellCommit::empty();
    commit.writes.push(FactWrite {
        key,
        expected,
        mutation: FactMutation::Put {
            values: object(values)?,
        },
    });
    cell.commit(commit)
        .map_err(|err| format!("seed commit failed: {err:?}"))?;
    Ok(())
}

fn seeded_cell() -> TestResult<Cell> {
    let mut cell = Cell::new(|command: &Command<JsonObject>| command.payload.clone())
        .with_types(schedule_types()?);
    put_fact(&mut cell, "schedule.Agent", "A1", json!({"agent_id": "A1"}))?;
    put_fact(
        &mut cell,
        "schedule.Plan",
        "P1",
        json!({"plan_id": "P1", "status": "draft"}),
    )?;
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
    )?;
    Ok(cell)
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
fn atomic_multi_object_rule_commits_above_16_links_under_v1_and_enforces_combined_budget()
-> TestResult {
    let types = schedule_types()?;

    // 1. Under default `EnvelopeLimits::V1` (`max_events = 32`), 10 commitments x 2 links
    //    (20 links > old 16-link cap, total 1 + 10 + 20 = 31 <= 32) commits atomically.
    let tags = (1..=10)
        .map(|idx| format!("'{idx:02}'"))
        .collect::<Vec<_>>()
        .join(", ");
    let facts_10 = format!(
        "map([{tags}], {{op: 'create', objectType: 'schedule.Commitment', key: 'C-batch-' + #, values: {{commitment_id: 'C-batch-' + #, agent_id: event.input.agent_id, starts_at: '2026-11-23T11:' + # + ':00Z', ends_at: '2026-11-23T11:' + # + ':30Z', start_min: 660, end_min: 690, hours: 0.50}}}})"
    );
    let links_20 = format!(
        "flatten([map([{tags}], {{op: 'add', sourceType: 'schedule.Commitment', sourceKey: 'C-batch-' + #, link: 'for_agent', targetType: 'schedule.Agent', targetKey: event.input.agent_id}}), map([{tags}], {{op: 'add', sourceType: 'schedule.Commitment', sourceKey: 'C-batch-' + #, link: 'for_plan', targetType: 'schedule.Plan', targetKey: event.input.plan_id}})])"
    );
    let rule_20 = batch_writer_rule(&facts_10, &links_20, &["for_agent", "for_plan"])?;
    let catalog_20 = TypeCatalog::new(&types).with_action_rules(std::slice::from_ref(&rule_20));
    let mut cell = seeded_cell()?;

    // Also verifies deduplication when both a predicate slice and an aggregate
    // read the exact same PredicateQuery.
    let action = schedule_action(50, true)?;
    let request = schedule_call(
        &action,
        "P1",
        "2026-11-23T11:00:00Z",
        "2026-11-23T12:00:00Z",
    )?;
    let outcome = commit_action(&mut cell, &catalog_20, &request, &ALLOW)?;
    assert!(matches!(outcome, ActionOutcome::Committed { .. }));

    // Primary transition committed.
    let plan = cell.load(&FactKey::new("schedule.Plan", "P1"))?;
    assert_eq!(
        plan.values
            .as_ref()
            .and_then(|v| v.get("status"))
            .and_then(norn_ir::RuntimeValue::as_str),
        Some("committed")
    );
    // All 10 commitments and all 20 typed links persisted atomically (above old 16-link cap).
    for idx in 1..=10 {
        let key = format!("C-batch-{idx:02}");
        assert!(
            cell.load(&FactKey::new("schedule.Commitment", &key))?
                .exists(),
            "commitment {key} should exist"
        );
        for (link_name, target_type, target_key) in [
            ("for_agent", "schedule.Agent", "A1"),
            ("for_plan", "schedule.Plan", "P1"),
        ] {
            let link = LinkKey::new(
                FactKey::new("schedule.Commitment", &key),
                link_name,
                FactKey::new(target_type, target_key),
            );
            assert!(
                cell.load_link(&link)?.exists(),
                "link {link_name} for {key} should exist"
            );
        }
    }

    // 2. Combined `events + links` budget: 1 plan + 18 commitments + 18 links = 37 > 32
    //    refuses under default `EnvelopeLimits::V1` and commits + charges 37 under a
    //    wider per-decision limit with `TurnEnvelopeMeter`.
    verify_combined_events_and_links_budget(&types, &request)?;
    Ok(())
}

fn verify_combined_events_and_links_budget(
    types: &[ObjectTypeDef],
    request: &ActionCall<'_>,
) -> TestResult {
    let (facts_18, links_18) = batch_expressions(18);
    let rule_18 = batch_writer_rule(&facts_18, &links_18, &["for_agent"])?;
    let catalog_18 = TypeCatalog::new(types).with_action_rules(std::slice::from_ref(&rule_18));
    let mut cell_v1 = seeded_cell()?;
    let Err(refusal) = commit_action(&mut cell_v1, &catalog_18, request, &ALLOW) else {
        return Err("20 events + 18 links should exceed V1 max_events 32".into());
    };
    assert!(
        matches!(
            refusal,
            ActionRefusal::Decision(DecisionRefusal::EnvelopeExceeded {
                kind: EnvelopeKind::Events,
                limit: 32,
                actual: 38,
            })
        ),
        "expected combined events+links EnvelopeExceeded, got {refusal:?}"
    );

    let mut cell_wide = seeded_cell()?;
    let mut meter = TurnEnvelopeMeter::with_limits(
        EnvelopeLimits {
            max_events: 64,
            ..EnvelopeLimits::V1
        },
        TurnEnvelopeLimits {
            max_events: 64,
            max_commands: 32,
            max_timers: 32,
        },
    );
    commit_action_with_meter(&mut cell_wide, &catalog_18, request, &ALLOW, &mut meter).map_err(
        |err| format!("18 commitments + 18 links should commit under 64-event cap: {err:?}"),
    )?;
    assert_eq!(meter.used().events, 38);
    Ok(())
}

#[test]
fn overlapping_commitment_in_predicate_slice_refuses_with_zero_writes() -> TestResult {
    let types = schedule_types()?;
    let (facts_expr, links_expr) = batch_expressions(2);
    let rule = batch_writer_rule(&facts_expr, &links_expr, &["for_agent"])?;
    let catalog = TypeCatalog::new(&types).with_action_rules(std::slice::from_ref(&rule));
    let mut cell = seeded_cell()?;

    let action = schedule_action(50, false)?;
    // Overlaps existing C0 (09:00..10:00).
    let request = schedule_call(
        &action,
        "P1",
        "2026-11-23T09:30:00Z",
        "2026-11-23T10:30:00Z",
    )?;
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
            .load(&FactKey::new("schedule.Commitment", "C-batch-01"))?
            .exists()
    );
    let plan = cell.load(&FactKey::new("schedule.Plan", "P1"))?;
    assert_eq!(
        plan.values
            .as_ref()
            .and_then(|v| v.get("status"))
            .and_then(norn_ir::RuntimeValue::as_str),
        Some("draft")
    );
    Ok(())
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
            Some(Race::InsertInsideWindow) => assert!(
                put_fact(
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
                )
                .is_ok()
            ),
            Some(Race::InsertOutsideWindow) => assert!(
                put_fact(
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
                )
                .is_ok()
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
fn concurrent_phantom_insertion_refuses_with_zero_partial_writes() -> TestResult {
    let types = schedule_types()?;
    let (facts_expr, links_expr) = batch_expressions(3);
    let rule = batch_writer_rule(&facts_expr, &links_expr, &["for_agent"])?;
    let catalog = TypeCatalog::new(&types).with_action_rules(std::slice::from_ref(&rule));
    let action = schedule_action(50, false)?;

    let mut inside = RacingCell {
        cell: RefCell::new(seeded_cell()?),
        race: Some(Race::InsertInsideWindow),
    };
    let request = schedule_call(
        &action,
        "P1",
        "2026-11-23T11:00:00Z",
        "2026-11-23T12:00:00Z",
    )?;
    let refusal = commit_action(&mut inside, &catalog, &request, &ALLOW)
        .expect_err("phantom inside window must conflict");
    assert_eq!(refusal.code(), "action.aggregate_stale_read");
    // ZERO partial writes: plan stays draft, none of C-batch-1..3 or links exist.
    let cell = inside.cell.borrow();
    assert_eq!(
        cell.load(&FactKey::new("schedule.Plan", "P1"))?
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
                .load(&FactKey::new("schedule.Commitment", &key))?
                .exists()
        );
        assert!(
            !cell
                .load_link(&LinkKey::new(
                    FactKey::new("schedule.Commitment", &key),
                    "for_agent",
                    FactKey::new("schedule.Agent", "A1"),
                ))?
                .exists()
        );
    }

    // Outside window does not conflict.
    let mut outside = RacingCell {
        cell: RefCell::new(seeded_cell()?),
        race: Some(Race::InsertOutsideWindow),
    };
    commit_action(&mut outside, &catalog, &request, &ALLOW)?;
    assert!(
        outside
            .cell
            .borrow()
            .load(&FactKey::new("schedule.Commitment", "C-batch-01"))?
            .exists()
    );
    Ok(())
}

#[test]
fn predicate_slice_bound_and_byte_overflow_refuse_fail_closed_never_truncate() -> TestResult {
    let types = schedule_types()?;
    let (facts_expr, links_expr) = batch_expressions(1);
    let rule = batch_writer_rule(&facts_expr, &links_expr, &["for_agent"])?;
    let catalog = TypeCatalog::new(&types).with_action_rules(std::slice::from_ref(&rule));
    let mut cell = seeded_cell()?;
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
        )?;
    }
    // 3 matching commitments in window, maxItems = 2 -> refuse fail-closed.
    let action = schedule_action(2, false)?;
    let request = schedule_call(
        &action,
        "P1",
        "2026-11-23T11:00:00Z",
        "2026-11-23T12:00:00Z",
    )?;
    let refusal = commit_action(&mut cell, &catalog, &request, &ALLOW)
        .expect_err("3 matches exceed maxItems 2");
    assert_eq!(refusal.code(), "action.slice_unavailable");
    assert!(
        !cell
            .load(&FactKey::new("schedule.Commitment", "C-batch-01"))?
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
        )?;
    }
    let big_action = schedule_action(50, false)?;
    let refusal = commit_action(
        &mut cell,
        &catalog,
        &schedule_call(
            &big_action,
            "P1",
            "2026-11-23T11:00:00Z",
            "2026-11-23T12:00:00Z",
        )?,
        &ALLOW,
    )
    .expect_err("oversized predicate slice must refuse");
    assert_eq!(refusal.code(), "action.slice_unavailable");
    Ok(())
}

#[test]
fn unauthorized_predicate_member_and_invalid_decision_links_refuse_with_zero_writes() -> TestResult
{
    let types = schedule_types()?;
    let (facts_expr, links_expr) = batch_expressions(2);
    let rule = batch_writer_rule(&facts_expr, &links_expr, &["for_agent"])?;
    let catalog = TypeCatalog::new(&types).with_action_rules(std::slice::from_ref(&rule));
    let mut cell = seeded_cell()?;
    let action = schedule_action(50, false)?;
    let request = schedule_call(
        &action,
        "P1",
        "2026-11-23T11:00:00Z",
        "2026-11-23T12:00:00Z",
    )?;

    // 1. Unauthorized predicate member refuses as target_not_found with zero writes.
    let hidden = Authorizer {
        hidden_key: Some("C0"),
    };
    let refusal = commit_action(&mut cell, &catalog, &request, &hidden).expect_err("hidden member");
    assert_eq!(refusal.code(), "action.target_not_found");
    assert!(
        !cell
            .load(&FactKey::new("schedule.Commitment", "C-batch-01"))?
            .exists()
    );

    // 2. Dangling link target (uncaptured agent A-MISSING) refuses with zero writes.
    let bad_target_links = "[{op: 'add', sourceType: 'schedule.Commitment', sourceKey: 'C-batch-01', link: 'for_agent', targetType: 'schedule.Agent', targetKey: 'A-MISSING'}]";
    let bad_rule = batch_writer_rule(&facts_expr, bad_target_links, &["for_agent"])?;
    let bad_catalog = TypeCatalog::new(&types).with_action_rules(std::slice::from_ref(&bad_rule));
    let refusal = commit_action(&mut cell, &bad_catalog, &request, &ALLOW)
        .expect_err("dangling link target must refuse");
    assert_eq!(refusal.code(), "decision.invalid_output");
    assert!(
        !cell
            .load(&FactKey::new("schedule.Commitment", "C-batch-01"))?
            .exists()
    );

    // 3. Cardinality violation: `for_agent` is to-one, emitting two targets for the same commitment refuses with zero writes.
    put_fact(&mut cell, "schedule.Agent", "A2", json!({"agent_id": "A2"}))?;
    let mut two_agent_action = schedule_action(50, false)?;
    two_agent_action.slice.push(serde_json::from_value(json!({
        "name": "all_agents",
        "objectType": "schedule.Agent",
        "key": {"predicate": {"maxItems": 10}}
    }))?);
    let card_links = "[{op: 'add', sourceType: 'schedule.Commitment', sourceKey: 'C-batch-01', link: 'for_agent', targetType: 'schedule.Agent', targetKey: 'A1'}, {op: 'add', sourceType: 'schedule.Commitment', sourceKey: 'C-batch-01', link: 'for_agent', targetType: 'schedule.Agent', targetKey: 'A2'}]";
    let card_rule = admit_writer_rule(
        &facts_expr,
        card_links,
        &["add"],
        &["for_agent"],
        &["schedule.Agent"],
        true,
    )?;
    let card_catalog = TypeCatalog::new(&types).with_action_rules(std::slice::from_ref(&card_rule));
    let card_call = schedule_call(
        &two_agent_action,
        "P1",
        "2026-11-23T11:00:00Z",
        "2026-11-23T12:00:00Z",
    )?;
    let refusal = commit_action(&mut cell, &card_catalog, &card_call, &ALLOW)
        .expect_err("to-one cardinality violation must refuse");
    assert_eq!(refusal.code(), "decision.invalid_output");
    assert!(
        refusal.to_string().contains("cardinality"),
        "expected link cardinality refusal, got: {refusal}"
    );
    assert!(
        !cell
            .load(&FactKey::new("schedule.Commitment", "C-batch-01"))?
            .exists()
    );
    assert_eq!(
        cell.load(&FactKey::new("schedule.Plan", "P1"))?
            .values
            .as_ref()
            .and_then(|v| v.get("status"))
            .and_then(norn_ir::RuntimeValue::as_str),
        Some("draft")
    );
    Ok(())
}

// =========================================================================
// Round-1 adversarial regression tests (Blockers 1, 2, 3, 4, 6, 7)
// =========================================================================

fn put_link(
    cell: &mut Cell,
    source_type: &str,
    source_id: &str,
    link: &str,
    target_type: &str,
    target_id: &str,
) -> TestResult {
    let key = LinkKey::new(
        FactKey::new(source_type, source_id),
        link,
        FactKey::new(target_type, target_id),
    );
    let expected = cell.load_link(&key)?.version;
    let mut commit = CellCommit::empty();
    commit.links.push(LinkWrite {
        key,
        expected,
        mutation: LinkMutation::Add {
            values: JsonObject::new(),
        },
    });
    cell.commit(commit)
        .map_err(|err| format!("seed link failed: {err:?}"))?;
    Ok(())
}

struct ReadOnlySourceAuthorizer {
    forbidden_write_key: &'static str,
}

impl ActionAuthorizer for ReadOnlySourceAuthorizer {
    fn authorize(&self, request: &ActionAuthzRequest<'_>) -> Result<(), ActionDenial> {
        if request.target.key == self.forbidden_write_key {
            return Err(ActionDenial::Forbidden {
                reason: "read-only actor on link source".to_owned(),
            });
        }
        Ok(())
    }
    fn authorize_read(&self, _: &ActionReadAuthzRequest<'_>) -> Result<(), ActionDenial> {
        Ok(())
    }
    fn authorize_read_type(&self, _: &ActionReadTypeAuthzRequest<'_>) -> Result<(), ActionDenial> {
        Ok(())
    }
}

#[test]
fn blocker1_read_only_actor_on_link_source_is_refused_for_add_and_remove() -> TestResult {
    let types = schedule_types()?;
    let action = schedule_action(50, false)?;
    let request = schedule_call(
        &action,
        "P1",
        "2026-11-23T11:00:00Z",
        "2026-11-23T12:00:00Z",
    )?;
    let authz = ReadOnlySourceAuthorizer {
        forbidden_write_key: "C0",
    };

    // 1. LinkChange::Add with source C0 (in slice, not in prepared_writes): actor has
    //    execute on P1 and read on C0, but Forbidden on write to C0.
    let add_rule = batch_writer_rule(
        "[]",
        "[{op: 'add', sourceType: 'schedule.Commitment', sourceKey: 'C0', link: 'for_agent', targetType: 'schedule.Agent', targetKey: 'A1'}]",
        &["for_agent"],
    )?;
    let add_catalog = TypeCatalog::new(&types).with_action_rules(std::slice::from_ref(&add_rule));
    let mut cell = seeded_cell()?;
    let refusal = commit_action(&mut cell, &add_catalog, &request, &authz)
        .expect_err("read-only actor on link source must be refused for Add");
    assert!(
        matches!(refusal, ActionRefusal::PermissionDenied { .. }),
        "expected PermissionDenied on Add, got {refusal:?}"
    );
    assert!(
        !cell
            .load_link(&LinkKey::new(
                FactKey::new("schedule.Commitment", "C0"),
                "for_agent",
                FactKey::new("schedule.Agent", "A1"),
            ))?
            .exists()
    );

    // 2. LinkChange::Remove with source C0 (existing link seeded): same read-only actor
    //    must be refused with PermissionDenied and zero writes.
    let remove_rule = admit_writer_rule(
        "[]",
        "[{op: 'remove', sourceType: 'schedule.Commitment', sourceKey: 'C0', link: 'for_agent', targetType: 'schedule.Agent', targetKey: 'A1'}]",
        &["remove"],
        &["for_agent"],
        &["schedule.Agent"],
        false,
    )?;
    let remove_catalog =
        TypeCatalog::new(&types).with_action_rules(std::slice::from_ref(&remove_rule));
    let mut cell2 = seeded_cell()?;
    put_link(
        &mut cell2,
        "schedule.Commitment",
        "C0",
        "for_agent",
        "schedule.Agent",
        "A1",
    )?;
    let refusal = commit_action(&mut cell2, &remove_catalog, &request, &authz)
        .expect_err("read-only actor on link source must be refused for Remove");
    assert!(
        matches!(refusal, ActionRefusal::PermissionDenied { .. }),
        "expected PermissionDenied on Remove, got {refusal:?}"
    );
    assert!(
        cell2
            .load_link(&LinkKey::new(
                FactKey::new("schedule.Commitment", "C0"),
                "for_agent",
                FactKey::new("schedule.Agent", "A1"),
            ))?
            .exists(),
        "seed link must remain intact after PermissionDenied"
    );
    Ok(())
}

struct NonePredicateCell {
    cell: RefCell<Cell>,
}

impl CellHost for NonePredicateCell {
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
        _: &PredicateQuery,
        _: usize,
        _: u64,
    ) -> Result<Option<Vec<PredicateObject>>, Self::Error> {
        // Simulate host byte-overflow or unenumerable scan before per-record authz.
        Ok(None)
    }
    fn commit(
        &mut self,
        commit: CellCommit<FactEvent, JsonObject, JsonObject>,
    ) -> Result<CommitReceipt, CommitError<Self::Error>> {
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
fn blocker2_hidden_row_over_max_items_and_unenumerable_scan_refuse_as_target_not_found()
-> TestResult {
    let types = schedule_types()?;
    let (facts_expr, links_expr) = batch_expressions(1);
    let rule = batch_writer_rule(&facts_expr, &links_expr, &["for_agent"])?;
    let catalog = TypeCatalog::new(&types).with_action_rules(std::slice::from_ref(&rule));
    let mut cell = seeded_cell()?;
    // Add a 2nd matching commitment C1 that is hidden from the actor.
    put_fact(
        &mut cell,
        "schedule.Commitment",
        "C1",
        json!({
            "commitment_id": "C1",
            "agent_id": "A1",
            "starts_at": "2026-11-23T08:00:00Z",
            "ends_at": "2026-11-23T08:30:00Z",
            "start_min": 480,
            "end_min": 510,
            "hours": 0.50
        }),
    )?;

    // 1 readable row (C0) + 1 hidden row (C1) with maxItems = 1:
    // Must conceal as TargetNotFound, NEVER leak existence via SliceUnavailable.
    let action = schedule_action(1, false)?;
    let request = schedule_call(
        &action,
        "P1",
        "2026-11-23T11:00:00Z",
        "2026-11-23T12:00:00Z",
    )?;
    let hide_c1 = Authorizer {
        hidden_key: Some("C1"),
    };
    let refusal = commit_action(&mut cell, &catalog, &request, &hide_c1)
        .expect_err("hidden overflow row must refuse as TargetNotFound");
    assert!(
        matches!(refusal, ActionRefusal::TargetNotFound),
        "expected TargetNotFound (no hidden-row existence oracle), got {refusal:?}"
    );

    // 2. Host returning Ok(None) (byte overflow during scan before per-record authz)
    //    must also conceal as TargetNotFound.
    let mut none_host = NonePredicateCell {
        cell: RefCell::new(seeded_cell()?),
    };
    let refusal = commit_action(&mut none_host, &catalog, &request, &ALLOW)
        .expect_err("Ok(None) scan must conceal as TargetNotFound");
    assert!(
        matches!(refusal, ActionRefusal::TargetNotFound),
        "expected TargetNotFound on Ok(None) scan, got {refusal:?}"
    );
    Ok(())
}

fn doc_node_fixture() -> TestResult<(Vec<ObjectTypeDef>, Cell, FactSlice)> {
    let types: Vec<ObjectTypeDef> = serde_json::from_value(json!([
        {
            "name": "doc.Node",
            "properties": [{"id": "node_id", "type": {"kind": "text"}}],
            "links": [{
                "id": "rel",
                "target": "doc.Node",
                "cardinality": {"to": "many", "from": "many"},
                "properties": [{"id": "note", "type": {"kind": "text"}}]
            }]
        }
    ]))?;
    let mut cell = Cell::new(|command: &Command<JsonObject>| command.payload.clone())
        .with_types(types.clone());
    for id in ["N1", "N2", "N3", "N4"] {
        put_fact(&mut cell, "doc.Node", id, json!({"node_id": id}))?;
    }
    let mut slice = FactSlice::new();
    for id in ["N1", "N2", "N3", "N4"] {
        let key = FactKey::new("doc.Node", id);
        let state = cell.load(&key)?;
        slice.insert(key, state);
    }
    Ok((types, cell, slice))
}

#[test]
fn blocker3_decision_links_enforce_max_output_bytes_and_charge_turn_envelope_meter() -> TestResult {
    let (types, mut cell, slice) = doc_node_fixture()?;
    let catalog = TypeCatalog::new(&types);

    // 1. Oversized LinkChange::Add.values (> 32 KiB EnvelopeLimits::V1.max_output_bytes) refuses.
    let huge_note = "x".repeat(40_000);
    let oversized: norn_ir::DecisionOutput = serde_json::from_value(json!({
        "facts": [],
        "links": [{
            "op": "add",
            "sourceType": "doc.Node",
            "sourceKey": "N1",
            "link": "rel",
            "targetType": "doc.Node",
            "targetKey": "N2",
            "values": {"note": huge_note}
        }],
        "commands": [],
        "timers": []
    }))?;
    let turn = DecisionTurn {
        id: "turn-oversized".to_owned(),
        now: LogicalTime::from_millis(1_000),
        world: World::Main,
        consumes: None,
    };
    let refusal = commit_decision(&mut cell, &catalog, &slice, &turn, &oversized)
        .expect_err("oversized link values must refuse");
    assert_eq!(refusal.code(), "decision.invalid_output");
    assert!(
        refusal.to_string().contains("envelope bytes"),
        "expected envelope bytes refusal, got: {refusal}"
    );

    // 2. Link-only decisions charge TurnEnvelopeMeter and refuse once exhausted.
    let mut meter = TurnEnvelopeMeter::new(TurnEnvelopeLimits {
        max_events: 2,
        max_commands: 4,
        max_timers: 4,
    });
    let two_links: norn_ir::DecisionOutput = serde_json::from_value(json!({
        "facts": [],
        "links": [
            {"op": "add", "sourceType": "doc.Node", "sourceKey": "N1", "link": "rel", "targetType": "doc.Node", "targetKey": "N2", "values": {"note": "a"}},
            {"op": "add", "sourceType": "doc.Node", "sourceKey": "N1", "link": "rel", "targetType": "doc.Node", "targetKey": "N3", "values": {"note": "b"}}
        ],
        "commands": [],
        "timers": []
    }))?;
    commit_decision_with_meter(&mut cell, &catalog, &slice, &turn, &two_links, &mut meter)?;
    assert_eq!(meter.used().events, 2);

    let third_link: norn_ir::DecisionOutput = serde_json::from_value(json!({
        "facts": [],
        "links": [
            {"op": "add", "sourceType": "doc.Node", "sourceKey": "N1", "link": "rel", "targetType": "doc.Node", "targetKey": "N4", "values": {"note": "c"}}
        ],
        "commands": [],
        "timers": []
    }))?;
    let turn2 = DecisionTurn {
        id: "turn-link-exhausted".to_owned(),
        now: LogicalTime::from_millis(2_000),
        world: World::Main,
        consumes: None,
    };
    let refusal =
        commit_decision_with_meter(&mut cell, &catalog, &slice, &turn2, &third_link, &mut meter)
            .expect_err("link-only commit past turn meter must refuse");
    assert!(
        matches!(
            refusal,
            DecisionRefusal::TurnEnvelopeExceeded {
                kind: EnvelopeKind::Events,
                limit: 2,
                actual: 3,
            }
        ),
        "expected TurnEnvelopeExceeded, got {refusal:?}"
    );
    assert_eq!(meter.used().events, 2);
    Ok(())
}

#[test]
fn blocker4_rejects_unknown_and_forged_computed_keys_in_link_values() -> TestResult {
    let types: Vec<ObjectTypeDef> = serde_json::from_value(json!([
        {
            "name": "doc.Node",
            "properties": [{"id": "node_id", "type": {"kind": "text"}}],
            "links": [
                {
                    "id": "annotated",
                    "target": "doc.Node",
                    "cardinality": {"to": "many", "from": "many"},
                    "properties": [
                        {"id": "role", "type": {"kind": "text"}},
                        {"id": "computed_tag", "type": {"kind": "text"}, "computed": "self.role"}
                    ]
                },
                {
                    "id": "bare",
                    "target": "doc.Node",
                    "cardinality": {"to": "many", "from": "many"}
                }
            ]
        }
    ]))?;
    let catalog = TypeCatalog::new(&types);
    let mut cell = Cell::new(|command: &Command<JsonObject>| command.payload.clone())
        .with_types(types.clone());
    put_fact(&mut cell, "doc.Node", "N1", json!({"node_id": "N1"}))?;
    put_fact(&mut cell, "doc.Node", "N2", json!({"node_id": "N2"}))?;
    let slice: FactSlice = ["N1", "N2"]
        .into_iter()
        .map(|id| {
            let key = FactKey::new("doc.Node", id);
            let state = cell.load(&key).expect("load");
            (key, state)
        })
        .collect();
    let turn = DecisionTurn {
        id: "turn-bad-values".to_owned(),
        now: LogicalTime::from_millis(1_000),
        world: World::Main,
        consumes: None,
    };

    // 1. Unknown key on link with properties.
    let unknown_on_annotated: norn_ir::DecisionOutput = serde_json::from_value(json!({
        "facts": [],
        "links": [{
            "op": "add", "sourceType": "doc.Node", "sourceKey": "N1",
            "link": "annotated", "targetType": "doc.Node", "targetKey": "N2",
            "values": {"role": "owner", "sneaky": "injected"}
        }],
        "commands": [], "timers": []
    }))?;
    let refusal = commit_decision(&mut cell, &catalog, &slice, &turn, &unknown_on_annotated)
        .expect_err("unknown key must refuse");
    assert_eq!(refusal.code(), "decision.invalid_output");

    // 2. Forged computed key on link with properties.
    let computed_forged: norn_ir::DecisionOutput = serde_json::from_value(json!({
        "facts": [],
        "links": [{
            "op": "add", "sourceType": "doc.Node", "sourceKey": "N1",
            "link": "annotated", "targetType": "doc.Node", "targetKey": "N2",
            "values": {"role": "owner", "computed_tag": "forged"}
        }],
        "commands": [], "timers": []
    }))?;
    let refusal = commit_decision(&mut cell, &catalog, &slice, &turn, &computed_forged)
        .expect_err("forged computed key must refuse");
    assert_eq!(refusal.code(), "decision.invalid_output");

    // 3. Unknown key on link with zero properties.
    let unknown_on_bare: norn_ir::DecisionOutput = serde_json::from_value(json!({
        "facts": [],
        "links": [{
            "op": "add", "sourceType": "doc.Node", "sourceKey": "N1",
            "link": "bare", "targetType": "doc.Node", "targetKey": "N2",
            "values": {"unexpected": 1}
        }],
        "commands": [], "timers": []
    }))?;
    let refusal = commit_decision(&mut cell, &catalog, &slice, &turn, &unknown_on_bare)
        .expect_err("unknown key on bare link must refuse");
    assert_eq!(refusal.code(), "decision.invalid_output");
    Ok(())
}

#[test]
fn blocker6_decision_cell_commit_never_silently_drops_links() -> TestResult {
    let types = schedule_types()?;
    let catalog = TypeCatalog::new(&types);
    let cell = seeded_cell()?;
    let slice: FactSlice = [
        FactKey::new("schedule.Commitment", "C0"),
        FactKey::new("schedule.Agent", "A1"),
    ]
    .into_iter()
    .map(|key| {
        let state = cell.load(&key).expect("load");
        (key, state)
    })
    .collect();
    let output: norn_ir::DecisionOutput = serde_json::from_value(json!({
        "facts": [],
        "links": [{
            "op": "add",
            "sourceType": "schedule.Commitment",
            "sourceKey": "C0",
            "link": "for_agent",
            "targetType": "schedule.Agent",
            "targetKey": "A1"
        }],
        "commands": [],
        "timers": []
    }))?;
    let turn = DecisionTurn {
        id: "turn-cell-commit".to_owned(),
        now: LogicalTime::from_millis(1_000),
        world: World::Main,
        consumes: None,
    };

    // Hostless `decision_cell_commit` must fail closed instead of silently returning empty links.
    let refusal = decision_cell_commit::<FactEvent, JsonObject, JsonObject, Infallible>(
        &catalog,
        &slice,
        &turn,
        &output,
        &EnvelopeLimits::V1,
    )
    .expect_err("decision_cell_commit must fail closed on non-empty links");
    assert_eq!(refusal.code(), "decision.invalid_output");

    // Host-backed `decision_cell_commit_with_host` materializes the link writes with CAS versions.
    let commit = decision_cell_commit_with_host(
        &cell,
        &catalog,
        &slice,
        &turn,
        &output,
        &EnvelopeLimits::V1,
    )?;
    assert_eq!(commit.links.len(), 1);
    assert_eq!(commit.links[0].key.link, "for_agent");
    Ok(())
}

struct SpyLoadLinkCell {
    cell: RefCell<Cell>,
    load_link_calls: RefCell<usize>,
}

impl CellHost for SpyLoadLinkCell {
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
        let mut count = self.load_link_calls.borrow_mut();
        *count = count.saturating_add(1);
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

fn hierarchy_fixture() -> TestResult<(Vec<ObjectTypeDef>, Cell, FactSlice)> {
    let types: Vec<ObjectTypeDef> = serde_json::from_value(json!([
        {
            "name": "hier.Base",
            "properties": [{"id": "id", "type": {"kind": "text"}}],
            "links": [
                {"id": "peer", "target": "hier.Base", "cardinality": {"to": "many", "from": "many"}}
            ]
        },
        {
            "name": "hier.Sub",
            "extends": "hier.Base",
            "properties": [{"id": "sub_note", "type": {"kind": "text"}}]
        },
        {
            "name": "hier.OwnedSub",
            "extends": "hier.Base",
            "properties": [{"id": "seq", "type": {"kind": "integer"}}]
        },
        {
            "name": "hier.Parent",
            "properties": [{"id": "id", "type": {"kind": "text"}}],
            "links": [
                {"id": "children", "target": "hier.OwnedSub", "cardinality": {"to": "many", "from": "one"}},
                {"id": "related", "target": "hier.Base", "cardinality": {"to": "many", "from": "many"}}
            ],
            "ownedCollections": [{
                "id": "children",
                "childType": "hier.OwnedSub",
                "rowIdProperty": "id",
                "orderingKey": "seq",
                "cascadeOnParentDelete": true,
                "maxRows": 10
            }]
        }
    ]))?;
    let mut base_cell = Cell::new(|command: &Command<JsonObject>| command.payload.clone())
        .with_types(types.clone());
    put_fact(&mut base_cell, "hier.Base", "B1", json!({"id": "B1"}))?;
    put_fact(&mut base_cell, "hier.Base", "B2", json!({"id": "B2"}))?;
    put_fact(&mut base_cell, "hier.Parent", "P1", json!({"id": "P1"}))?;
    let mut slice = FactSlice::new();
    for (ty, id) in [("hier.Base", "B1"), ("hier.Parent", "P1")] {
        let key = FactKey::new(ty, id);
        let state = base_cell.load(&key)?;
        slice.insert(key, state);
    }
    slice.insert(
        FactKey::new("hier.OwnedSub", "O1"),
        FactState {
            version: norn_kernel::FactVersion(1),
            values: Some(object(json!({"id": "O1", "seq": 1}))?),
        },
    );
    Ok((types, base_cell, slice))
}

#[test]
fn blocker7_remove_validates_capture_subtypes_and_concrete_protected_and_owned_links() -> TestResult
{
    let (types, base_cell, slice) = hierarchy_fixture()?;
    let catalog = TypeCatalog::new(&types);

    let turn = DecisionTurn {
        id: "turn-b7".to_owned(),
        now: LogicalTime::from_millis(1_000),
        world: World::Main,
        consumes: None,
    };

    // 1. Uncaptured `LinkChange::Remove` (B2 is NOT in slice) must refuse BEFORE host.load_link.
    let mut spy = SpyLoadLinkCell {
        cell: RefCell::new(base_cell),
        load_link_calls: RefCell::new(0),
    };
    let uncaptured_remove: norn_ir::DecisionOutput = serde_json::from_value(json!({
        "facts": [],
        "links": [{
            "op": "remove", "sourceType": "hier.Base", "sourceKey": "B1",
            "link": "peer", "targetType": "hier.Base", "targetKey": "B2"
        }],
        "commands": [], "timers": []
    }))?;
    let refusal = commit_decision(&mut spy, &catalog, &slice, &turn, &uncaptured_remove)
        .expect_err("uncaptured Remove must refuse");
    assert_eq!(refusal.code(), "decision.invalid_output");
    assert_eq!(
        *spy.load_link_calls.borrow(),
        0,
        "host.load_link must not be called for uncaptured Remove"
    );

    // 2. Subtype mismatch on `LinkChange::Remove`: slice has `hier.Base/B1`, Remove claims `hier.Sub/B1`.
    let subtype_mismatch_remove: norn_ir::DecisionOutput = serde_json::from_value(json!({
        "facts": [],
        "links": [{
            "op": "remove", "sourceType": "hier.Sub", "sourceKey": "B1",
            "link": "peer", "targetType": "hier.Base", "targetKey": "B1"
        }],
        "commands": [], "timers": []
    }))?;
    let refusal = commit_decision(&mut spy, &catalog, &slice, &turn, &subtype_mismatch_remove)
        .expect_err("subtype mismatch on Remove must refuse");
    assert_eq!(refusal.code(), "decision.invalid_output");
    assert_eq!(*spy.load_link_calls.borrow(), 0);

    // 3. Protected concrete subtype: link declares `hier.Base` target, but concrete slice
    //    object `O1` is `hier.OwnedSub` (an owned child type) -> refused for both Add and Remove.
    for op in ["add", "remove"] {
        let protected_concrete: norn_ir::DecisionOutput = serde_json::from_value(json!({
            "facts": [],
            "links": [{
                "op": op, "sourceType": "hier.Parent", "sourceKey": "P1",
                "link": "related", "targetType": "hier.Base", "targetKey": "O1"
            }],
            "commands": [], "timers": []
        }))?;
        let refusal = commit_decision(&mut spy, &catalog, &slice, &turn, &protected_concrete)
            .expect_err("protected concrete owned-child subtype must refuse");
        assert_eq!(refusal.code(), "decision.invalid_output");
        assert_eq!(*spy.load_link_calls.borrow(), 0);
    }

    // 4. Owned-collection owning link (`children` on `hier.Parent`) refused for both Add and Remove.
    for op in ["add", "remove"] {
        let owned_link: norn_ir::DecisionOutput = serde_json::from_value(json!({
            "facts": [],
            "links": [{
                "op": op, "sourceType": "hier.Parent", "sourceKey": "P1",
                "link": "children", "targetType": "hier.OwnedSub", "targetKey": "O1"
            }],
            "commands": [], "timers": []
        }))?;
        let refusal = commit_decision(&mut spy, &catalog, &slice, &turn, &owned_link)
            .expect_err("owned-collection owning link must refuse");
        assert_eq!(refusal.code(), "decision.invalid_output");
        assert_eq!(*spy.load_link_calls.borrow(), 0);
    }
    Ok(())
}

// =========================================================================
// Round-2 adversarial regression tests (Blockers 3b, 6, 7a, 7b)
// =========================================================================

fn r2_agg_only_action(max_items: u32) -> TestResult<ActionRoot> {
    Ok(serde_json::from_value(json!({
        "actionId": "schedule.plan.agg_only",
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
                "window_end": {"type": "string"}
            },
            "required": ["plan_id", "agent_id", "window_start", "window_end"]
        },
        "outputSchema": {"type": "object", "additionalProperties": false, "properties": {}},
        "aggregates": [{
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
        }],
        "preconditions": ["aggregates.existing_count >= 0"],
        "writes": [{"entityKind": "schedule.Plan", "change": "modified"}],
        "effectProgram": {
            "kind": "fire_transition",
            "objectKind": "schedule.Plan",
            "objectIdField": "plan_id",
            "entityArtifactId": "schedule.Plan.entity",
            "entityArtifactVersion": "1.0.0",
            "lifecycle": "status",
            "transition": "commit_plan"
        }
    }))?)
}

#[test]
fn r2_blocker3b_fresh_aggregate_authorizes_before_bound_and_byte_overflow() -> TestResult {
    let types = schedule_types()?;
    let catalog = TypeCatalog::new(&types);
    let mut cell = seeded_cell()?;
    put_fact(
        &mut cell,
        "schedule.Commitment",
        "C1",
        json!({
            "commitment_id": "C1", "agent_id": "A1",
            "starts_at": "2026-11-23T08:00:00Z", "ends_at": "2026-11-23T08:30:00Z",
            "start_min": 480, "end_min": 510, "hours": 0.50
        }),
    )?;
    let action = r2_agg_only_action(1)?;
    let request = ActionCall {
        action: &action,
        action_artifact_id: "schedule.plan.agg_only.action",
        action_version: "1.0.0",
        scope: BTreeMap::from([
            ("orgId".to_owned(), "org_demo".to_owned()),
            ("environmentId".to_owned(), "dev".to_owned()),
        ]),
        actor: "scheduler:lead".to_owned(),
        input: object(json!({
            "plan_id": "P1", "agent_id": "A1",
            "window_start": "2026-11-23T00:00:00Z", "window_end": "2026-11-24T00:00:00Z"
        }))?,
        idempotency_key: "agg-only-1".to_owned(),
        object_type: None,
        turn: DecisionTurn {
            id: "turn-agg-only".to_owned(),
            now: LogicalTime::from_millis(2_000),
            world: World::Main,
            consumes: None,
        },
        trace: None,
    };
    let hide_c1 = Authorizer {
        hidden_key: Some("C1"),
    };
    // Hidden overflow row MUST refuse as TargetNotFound, NEVER AggregateBoundExceeded.
    let refusal = commit_action(&mut cell, &catalog, &request, &hide_c1)
        .expect_err("hidden aggregate overflow row must conceal as TargetNotFound");
    assert!(
        matches!(refusal, ActionRefusal::TargetNotFound),
        "got {refusal:?}"
    );

    // Hidden oversized row MUST also conceal as TargetNotFound, NEVER AggregateBytesExceeded.
    let big_action = r2_agg_only_action(10)?;
    let big_request = ActionCall {
        action: &big_action,
        idempotency_key: "agg-only-bytes".to_owned(),
        ..request
    };
    put_fact(
        &mut cell,
        "schedule.Commitment",
        "C1",
        json!({
            "commitment_id": "C1", "agent_id": "A1",
            "starts_at": "2026-11-23T08:00:00Z", "ends_at": "2026-11-23T08:30:00Z",
            "start_min": 480, "end_min": 510, "hours": 0.50,
            "note": "z".repeat(300_000)
        }),
    )?;
    let refusal = commit_action(&mut cell, &catalog, &big_request, &hide_c1)
        .expect_err("hidden oversized aggregate row must conceal as TargetNotFound");
    assert!(
        matches!(refusal, ActionRefusal::TargetNotFound),
        "got {refusal:?}"
    );
    Ok(())
}

#[test]
fn r2_blocker6_hostless_prepare_decision_and_partition_slice_refuse_non_empty_links() -> TestResult
{
    let types = schedule_types()?;
    let catalog = TypeCatalog::new(&types);
    let cell = seeded_cell()?;
    let mut slice = FactSlice::new();
    for key in [
        FactKey::new("schedule.Commitment", "C0"),
        FactKey::new("schedule.Agent", "A1"),
    ] {
        let state = cell.load(&key)?;
        slice.insert(key, state);
    }
    let output: norn_ir::DecisionOutput = serde_json::from_value(json!({
        "facts": [],
        "links": [{
            "op": "add", "sourceType": "schedule.Commitment", "sourceKey": "C0",
            "link": "for_agent", "targetType": "schedule.Agent", "targetKey": "A1"
        }],
        "commands": [], "timers": []
    }))?;
    let turn = DecisionTurn {
        id: "turn-hostless-prepare".to_owned(),
        now: LogicalTime::from_millis(1_000),
        world: World::Main,
        consumes: None,
    };

    let err1 = prepare_decision::<Infallible>(&catalog, &slice, &turn, &output)
        .expect_err("prepare_decision must refuse non-empty output.links");
    assert_eq!(err1.code(), "decision.invalid_output");

    let err2 = prepare_decision_with_limits::<Infallible>(
        &catalog,
        &slice,
        &turn,
        &output,
        &EnvelopeLimits::V1,
    )
    .expect_err("prepare_decision_with_limits must refuse non-empty output.links");
    assert_eq!(err2.code(), "decision.invalid_output");

    let err3 = prepare_decision_with_source_owned_targets::<Infallible>(
        &catalog,
        &slice,
        &turn,
        &output,
        &std::collections::BTreeSet::new(),
    )
    .expect_err("prepare_decision_with_source_owned_targets must refuse non-empty output.links");
    assert_eq!(err3.code(), "decision.invalid_output");

    let partition = load_partition_slice(
        &cell,
        &catalog,
        [
            FactKey::new("schedule.Commitment", "C0"),
            FactKey::new("schedule.Agent", "A1"),
        ],
        &[],
        norn_ir::ResourceLimitProfile::norn_limits_v1().slice,
    )?;
    let err4 = partition
        .prepare::<Infallible>(&catalog, &turn, &output, &EnvelopeLimits::V1)
        .expect_err("PartitionSlice::prepare must refuse non-empty output.links");
    assert_eq!(err4.code(), "decision.invalid_output");
    Ok(())
}

fn r2_default_link_node_fixture(
    default_val: &str,
) -> TestResult<(Vec<ObjectTypeDef>, Cell, FactSlice)> {
    let types: Vec<ObjectTypeDef> = serde_json::from_value(json!([{
        "name": "doc.Node",
        "properties": [{"id": "node_id", "type": {"kind": "text"}}],
        "links": [{
            "id": "rel",
            "target": "doc.Node",
            "cardinality": {"to": "many", "from": "many"},
            "properties": [{"id": "note", "type": {"kind": "text"}, "default": default_val}]
        }]
    }]))?;
    let mut cell = Cell::new(|command: &Command<JsonObject>| command.payload.clone())
        .with_types(types.clone());
    let mut slice = FactSlice::new();
    for id in ["N1", "N2", "N3", "N4"] {
        put_fact(&mut cell, "doc.Node", id, json!({"node_id": id}))?;
        let key = FactKey::new("doc.Node", id);
        let state = cell.load(&key)?;
        slice.insert(key, state);
    }
    Ok((types, cell, slice))
}

#[test]
fn r2_blocker7a_omitted_link_values_default_overhead_charges_materialized_bytes() -> TestResult {
    let default_val = "v".repeat(40);
    let (types, cell, slice) = r2_default_link_node_fixture(&default_val)?;
    let catalog = TypeCatalog::new(&types);
    let omitted: norn_ir::DecisionOutput = serde_json::from_value(json!({
        "facts": [],
        "links": [{
            "op": "add", "sourceType": "doc.Node", "sourceKey": "N1",
            "link": "rel", "targetType": "doc.Node", "targetKey": "N2"
        }],
        "commands": [], "timers": []
    }))?;
    let materialized: norn_ir::DecisionOutput = serde_json::from_value(json!({
        "facts": [],
        "links": [{
            "op": "add", "sourceType": "doc.Node", "sourceKey": "N1",
            "link": "rel", "targetType": "doc.Node", "targetKey": "N2",
            "values": {"note": default_val}
        }],
        "commands": [], "timers": []
    }))?;
    let exact_materialized_bytes =
        u64::try_from(norn_ir::canonical::canonical_json_of(&materialized)?.len())?;
    let turn = DecisionTurn {
        id: "turn-default-overhead".to_owned(),
        now: LogicalTime::from_millis(1_000),
        world: World::Main,
        consumes: None,
    };
    let too_tight = EnvelopeLimits {
        max_output_bytes: exact_materialized_bytes.saturating_sub(1),
        ..EnvelopeLimits::V1
    };
    let refusal =
        decision_cell_commit_with_host(&cell, &catalog, &slice, &turn, &omitted, &too_tight)
            .expect_err("omitted values default expansion must exceed tight cap");
    assert_eq!(refusal.code(), "decision.invalid_output");
    assert!(refusal.to_string().contains("envelope bytes"));

    let exact_cap = EnvelopeLimits {
        max_output_bytes: exact_materialized_bytes,
        ..EnvelopeLimits::V1
    };
    let admitted =
        decision_cell_commit_with_host(&cell, &catalog, &slice, &turn, &omitted, &exact_cap)?;
    assert_eq!(admitted.links.len(), 1);
    Ok(())
}

#[test]
fn r2_blocker7b_metered_host_commit_charges_turn_envelope() -> TestResult {
    let (types, cell, slice) = r2_default_link_node_fixture("ok")?;
    let catalog = TypeCatalog::new(&types);
    let turn = DecisionTurn {
        id: "turn-metered-host".to_owned(),
        now: LogicalTime::from_millis(1_000),
        world: World::Main,
        consumes: None,
    };
    let mut meter = TurnEnvelopeMeter::new(TurnEnvelopeLimits {
        max_events: 2,
        max_commands: 4,
        max_timers: 4,
    });
    let two_links: norn_ir::DecisionOutput = serde_json::from_value(json!({
        "facts": [],
        "links": [
            {"op": "add", "sourceType": "doc.Node", "sourceKey": "N1", "link": "rel", "targetType": "doc.Node", "targetKey": "N2"},
            {"op": "add", "sourceType": "doc.Node", "sourceKey": "N1", "link": "rel", "targetType": "doc.Node", "targetKey": "N3"}
        ],
        "commands": [], "timers": []
    }))?;
    let first = decision_cell_commit_with_host_and_meter(
        &cell, &catalog, &slice, &turn, &two_links, &mut meter,
    )?;
    assert_eq!(first.links.len(), 2);
    assert_eq!(meter.used().events, 2);

    let third_link: norn_ir::DecisionOutput = serde_json::from_value(json!({
        "facts": [],
        "links": [
            {"op": "add", "sourceType": "doc.Node", "sourceKey": "N1", "link": "rel", "targetType": "doc.Node", "targetKey": "N4"}
        ],
        "commands": [], "timers": []
    }))?;
    let refusal = decision_cell_commit_with_host_and_meter(
        &cell,
        &catalog,
        &slice,
        &turn,
        &third_link,
        &mut meter,
    )
    .expect_err("metered host commit past aggregate limit must refuse");
    assert!(matches!(
        refusal,
        DecisionRefusal::TurnEnvelopeExceeded {
            kind: EnvelopeKind::Events,
            limit: 2,
            actual: 3,
        }
    ));
    assert_eq!(meter.used().events, 2);
    Ok(())
}

fn r3_blocker4_actions() -> TestResult<[ActionRoot; 2]> {
    let base = |id: &str, extra: Value| -> TestResult<ActionRoot> {
        let mut obj = json!({
            "actionId": id,
            "scopeRequired": ["orgId", "environmentId"],
            "idempotent": true,
            "ledgered": true,
            "inputSchema": {
                "type": "object",
                "additionalProperties": false,
                "properties": {"item_id": {"type": "string"}, "grp": {"type": "string"}},
                "required": ["item_id", "grp"]
            },
            "outputSchema": {"type": "object", "additionalProperties": false, "properties": {}},
            "writes": [{"entityKind": "doc.Item", "change": "created"}],
            "effectProgram": {
                "kind": "upsert_input_object",
                "objectIdField": "item_id",
                "objectKind": "doc.Item",
                "entityArtifactId": "doc.Item.entity",
                "entityArtifactVersion": "1.0.0"
            }
        });
        if let (Some(dst), Some(src)) = (obj.as_object_mut(), extra.as_object()) {
            for (k, v) in src {
                dst.insert(k.clone(), v.clone());
            }
        }
        Ok(serde_json::from_value(obj)?)
    };
    let pred = base(
        "doc.item.pred_check",
        json!({"slice": [{"name": "rows", "objectType": "doc.Item", "key": {"predicate": {"filter": {"grp": "grp"}, "maxItems": 1}}}]}),
    )?;
    let agg = base(
        "doc.item.agg_check",
        json!({
            "aggregates": [{"name": "cnt", "objectType": "doc.Item", "filter": {"grp": "grp"}, "aggregate": {"kind": "count"}, "maxItems": 1}],
            "preconditions": ["aggregates.cnt <= 1"]
        }),
    )?;
    Ok([pred, agg])
}

#[test]
fn r3_blocker4_hidden_row_past_max_items_plus_one_conceals_as_target_not_found() -> TestResult {
    let authorizer = Authorizer {
        hidden_key: Some("R3"),
    };
    let types: Vec<ObjectTypeDef> = serde_json::from_value(json!([{
        "name": "doc.Item",
        "properties": [
            {"id": "item_id", "type": {"kind": "text"}},
            {"id": "grp", "type": {"kind": "text"}}
        ]
    }]))?;
    let catalog = TypeCatalog::new(&types);
    let mut cell = Cell::new(|command: &Command<JsonObject>| command.payload.clone())
        .with_types(types.clone());
    for id in ["R1", "R2", "R3"] {
        put_fact(
            &mut cell,
            "doc.Item",
            id,
            json!({"item_id": id, "grp": "g"}),
        )?;
    }
    let actions = r3_blocker4_actions()?;
    for (idx, action) in actions.iter().enumerate() {
        let request = ActionCall {
            action,
            action_artifact_id: if idx == 0 {
                "doc.item.pred_check.action"
            } else {
                "doc.item.agg_check.action"
            },
            action_version: "1.0.0",
            scope: BTreeMap::from([
                ("orgId".to_owned(), "org_demo".to_owned()),
                ("environmentId".to_owned(), "dev".to_owned()),
            ]),
            actor: "operator".to_owned(),
            input: serde_json::from_value(json!({"item_id": format!("W{idx}"), "grp": "g"}))?,
            idempotency_key: format!("r3-b4-mem-{idx}"),
            object_type: None,
            turn: DecisionTurn {
                id: format!("t-b4-mem-{idx}"),
                now: LogicalTime::from_millis(1_000),
                world: World::Main,
                consumes: None,
            },
            trace: None,
        };
        let refusal = commit_action(&mut cell, &catalog, &request, &authorizer)
            .expect_err("hidden row past maxItems + 1 must conceal");
        assert!(
            matches!(refusal, ActionRefusal::TargetNotFound),
            "case {idx}: expected TargetNotFound, got {refusal:?}"
        );
    }
    Ok(())
}
