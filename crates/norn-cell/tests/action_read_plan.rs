#![cfg(feature = "native")]
//! Lane A3 end-to-end cell tests on production `SqliteCell`:
//! - Naming-series `CreateOnly` field-level IFC and replay after slice mutations.
//! - `FireTransition` (`independentOf`, guards, external slice) and lifecycle-bearing
//!   `UpdateOnly` exact field-level IFC.
//! - Polymorphic subtype dispatch across `FireTransition`, `UpdateOnly`, and `createLinks`.

use std::collections::{BTreeMap, BTreeSet};

use norn_cell::{MAIN_WORLD, Model, SqliteCell, StoreOptions};
use norn_ir::{ActionRoot, ObjectTypeDef};
use norn_kernel::cell_host::JsonObject;
use norn_kernel::{
    ActionAuthorizer, ActionAuthzRequest, ActionCall, ActionDenial, ActionOutcome,
    ActionPolicyReadFootprint, ActionReadAuthzRequest, ActionReadTypeAuthzRequest, CellCommit,
    CellHost, Command, DecisionTurn, FactEvent, FactKey, FactMutation, FactVersion, FactWrite,
    LinkKey, LogicalTime, TypeCatalog, World, commit_action, transition_guards,
};
use serde_json::{Value, json};

type Cell = SqliteCell<FactEvent, JsonObject, JsonObject>;

const PII: &str = "pii.contact";

struct ExactPolicyAuthz {
    target_attributes: BTreeSet<String>,
}

impl ExactPolicyAuthz {
    fn of(attrs: &[&str]) -> Self {
        Self {
            target_attributes: attrs.iter().map(|attr| (*attr).to_owned()).collect(),
        }
    }
}

impl ActionAuthorizer for ExactPolicyAuthz {
    fn authorize(&self, _: &ActionAuthzRequest<'_>) -> Result<(), ActionDenial> {
        Ok(())
    }
    fn authorize_read(&self, _: &ActionReadAuthzRequest<'_>) -> Result<(), ActionDenial> {
        Ok(())
    }
    fn authorize_read_type(&self, _: &ActionReadTypeAuthzRequest<'_>) -> Result<(), ActionDenial> {
        Ok(())
    }
    fn action_policy_read_footprint(&self, _: &ActionRoot) -> ActionPolicyReadFootprint {
        ActionPolicyReadFootprint::Exact {
            target_attributes: self.target_attributes.clone(),
        }
    }
}

fn action_call_type() -> ObjectTypeDef {
    let mut fields: Vec<Value> = ["actionId", "actor", "turn", "transition", "from", "to"]
        .iter()
        .map(|id| json!({"id": id, "type": {"kind": "text"}}))
        .collect();
    fields.extend(
        [
            "actionArtifactId",
            "actionVersion",
            "effectKind",
            "ruleBinding",
        ]
        .iter()
        .map(|id| json!({"id": id, "type": {"kind": "text"}, "constraints": {"nullable": true}})),
    );
    fields.extend(
        [
            "object",
            "input",
            "effectiveInput",
            "derivedValues",
            "preconditionOutcomes",
            "rowOperations",
        ]
        .iter()
        .map(
            |id| json!({"id": id, "type": {"kind": "json-ext"}, "constraints": {"nullable": true}}),
        ),
    );
    serde_json::from_value(json!({"name": "norn.ActionCall", "properties": fields})).unwrap()
}

fn leave_request_type() -> ObjectTypeDef {
    serde_json::from_value(json!({
        "name": "hr.LeaveRequest",
        "identity": [{
            "name": "primary",
            "properties": ["id"],
            "namingSeries": {
                "pattern": "LR-{branch_id}-{####}",
                "timezone": "Etc/UTC",
                "scopeFields": ["branch_id"],
                "resetPolicy": "never",
                "gapPolicy": "gapless_within_commit"
            }
        }],
        "properties": [
            {"id": "id", "type": {"kind": "text"}},
            {"id": "employee_id", "type": {"kind": "text"}},
            {"id": "branch_id", "type": {"kind": "text"}},
            {"id": "days", "type": {"kind": "integer"}},
            {"id": "reason", "type": {"kind": "text"}},
            {"id": "private_notes", "type": {"kind": "text"}, "markings": [PII]},
            {"id": "created_by", "type": {"kind": "text"}, "protected": true},
            {
                "id": "status",
                "type": {"kind": "enum", "values": [{"value": "submitted"}, {"value": "approved"}, {"value": "escalated"}]},
                "default": "submitted",
                "lifecycle": {
                    "property": "status",
                    "initial": ["submitted"],
                    "terminal": ["approved", "escalated"],
                    "states": [
                        {"id": "submitted", "label": "Submitted"},
                        {"id": "approved", "label": "Approved"},
                        {"id": "escalated", "label": "Escalated"}
                    ],
                    "transitions": [
                        {"id": "approve", "from": ["submitted"], "to": "approved", "guards": ["self.days <= 10"]},
                        {"id": "escalate", "from": ["submitted"], "to": "escalated", "guards": ["self.private_notes != ''"]}
                    ]
                }
            }
        ],
        "links": [{
            "id": "employee",
            "target": "hr.Person",
            "cardinality": {"to": "one", "from": "many"}
        }]
    }))
    .unwrap()
}

fn hr_a3_types() -> Vec<ObjectTypeDef> {
    let mut defs: Vec<ObjectTypeDef> = serde_json::from_value(json!([
        {
            "name": "hr.Person",
            "properties": [
                {"id": "id", "type": {"kind": "text"}},
                {"id": "name", "type": {"kind": "text"}},
                {
                    "id": "status",
                    "type": {"kind": "enum", "values": [{"value": "active"}, {"value": "inactive"}]},
                    "default": "active",
                    "lifecycle": {
                        "property": "status",
                        "initial": ["active"],
                        "terminal": ["inactive"],
                        "states": [
                            {"id": "active", "label": "Active"},
                            {"id": "inactive", "label": "Inactive"}
                        ],
                        "transitions": [{"id": "deactivate", "from": ["active"], "to": "inactive"}]
                    }
                }
            ]
        },
        {
            "name": "hr.Employee",
            "extends": "hr.Person",
            "properties": [
                {"id": "badge_no", "type": {"kind": "text"}},
                {"id": "national_id", "type": {"kind": "text"}, "markings": [PII]}
            ],
            "invariants": [{"id": "badge_non_empty", "expression": "self.badge_no != ''"}]
        }
    ]))
    .unwrap();
    defs.push(leave_request_type());
    defs.push(action_call_type());
    defs
}

fn open_cell(defs: &[ObjectTypeDef], name: &str) -> (tempfile::TempDir, Cell) {
    let dir = tempfile::tempdir().unwrap();
    let cell = Cell::open(
        dir.path().join(format!("{name}.sqlite")),
        Model::from_definitions(defs, &[]).unwrap(),
        StoreOptions::default(),
        "org_demo/dev",
        |_: &Command<JsonObject>| panic!("no command expected"),
    )
    .unwrap();
    (dir, cell)
}

fn seed_employee(cell: &mut Cell, id: &str, status: &str) {
    let mut commit = CellCommit::empty();
    commit.writes.push(FactWrite {
        key: FactKey::new("hr.Employee", id),
        expected: FactVersion::ABSENT,
        mutation: FactMutation::Put {
            values: serde_json::from_value(json!({
                "id": id,
                "name": "Nadia",
                "status": status,
                "badge_no": "B-101",
                "national_id": "123-45-6789"
            }))
            .unwrap(),
        },
    });
    cell.commit(commit).unwrap();
}

fn make_call<'a>(action: &'a ActionRoot, actor: &str, input: Value, key: &str) -> ActionCall<'a> {
    ActionCall {
        action,
        action_artifact_id: "a3.test",
        action_version: "1.0.0",
        scope: BTreeMap::from([
            ("orgId".into(), "org_demo".into()),
            ("environmentId".into(), "dev".into()),
        ]),
        actor: actor.into(),
        approved_intent: None,
        input: serde_json::from_value(input).unwrap(),
        idempotency_key: key.into(),
        object_type: None,
        turn: DecisionTurn {
            id: key.into(),
            now: LogicalTime::from_millis(1_767_225_600_000),
            world: World::Main,
            consumes: None,
        },
        trace: None,
    }
}

fn submit_leave_action() -> ActionRoot {
    serde_json::from_value(json!({
        "actionId": "hr.submit_leave",
        "scopeRequired": ["orgId", "environmentId"],
        "idempotent": true,
        "ledgered": true,
        "inputSchema": {
            "type": "object", "additionalProperties": false,
            "properties": {
                "id": {"type": "string"},
                "employee_id": {"type": "string"},
                "branch_id": {"type": "string"},
                "days": {"type": "integer"},
                "reason": {"type": "string"},
                "private_notes": {"type": "string"},
                "created_by": {"type": "string"}
            },
            "required": ["employee_id", "branch_id", "days", "reason", "private_notes", "created_by"]
        },
        "outputSchema": {"type": "object", "additionalProperties": false, "properties": {}},
        "derive": [{"field": "created_by", "expression": "actor.subjectId", "inputRequired": false}],
        "slice": [{
            "name": "emp", "objectType": "hr.Person",
            "key": {"property": {"from": "input", "property": "employee_id"}}
        }],
        "preconditions": ["slice.emp.status == 'active'"],
        "createLinks": [{
            "link": "employee", "targetType": "hr.Person", "targetIdField": "employee_id"
        }],
        "effectProgram": {
            "kind": "upsert_input_object", "objectIdField": "id",
            "objectKind": "hr.LeaveRequest", "entityArtifactId": "hr.LeaveRequest.entity",
            "entityArtifactVersion": "1.0.0", "writeMode": {"kind": "create_only"}
        }
    }))
    .unwrap()
}

fn markings(cell: &Cell, ty: &str, id: &str) -> BTreeMap<String, BTreeSet<String>> {
    cell.store()
        .read()
        .effective_markings(MAIN_WORLD, ty, id, None)
        .unwrap()
        .unwrap()
}

/// Naming-series `CreateOnly` commits with exact field-level IFC (unread PII
/// `national_id` on `hr.Employee` does not taint the new `hr.LeaveRequest`),
/// links polymorphically via `createLinks` (`hr.Person` -> concrete `hr.Employee`),
/// and replays cleanly even after the referenced employee becomes inactive.
#[test]
fn naming_series_create_only_enables_field_ifc_and_replays_after_slice_changes() {
    let defs = hr_a3_types();
    let catalog = TypeCatalog::new(&defs);
    let (_dir, mut cell) = open_cell(&defs, "series-ifc");
    seed_employee(&mut cell, "E1", "active");

    let submit = submit_leave_action();
    let authz = ExactPolicyAuthz::of(&["branch_id"]);
    let payload = json!({
        "employee_id": "E1",
        "branch_id": "cairo",
        "days": 3,
        "reason": "annual",
        "private_notes": "clinic visit"
    });
    let first_call = make_call(&submit, "user:alice", payload.clone(), "leave-1");
    let ActionOutcome::Committed { record, .. } =
        commit_action(&mut cell, &catalog, &first_call, &authz).unwrap()
    else {
        panic!("expected committed");
    };
    assert_eq!(
        record.object,
        FactKey::new("hr.LeaveRequest", "LR-cairo-0001")
    );
    // Unread PII on `hr.Employee.national_id` must NOT taint `reason` or `status`.
    let leave_marks = markings(&cell, "hr.LeaveRequest", "LR-cairo-0001");
    assert!(leave_marks["reason"].is_empty(), "{leave_marks:?}");
    assert!(leave_marks["status"].is_empty(), "{leave_marks:?}");
    assert!(
        leave_marks["private_notes"].contains(PII),
        "{leave_marks:?}"
    );
    // Polymorphic `createLinks` resolved supertype `hr.Person` to concrete `hr.Employee`.
    assert!(
        cell.load_link(&LinkKey::new(
            FactKey::new("hr.LeaveRequest", "LR-cairo-0001"),
            "employee",
            FactKey::new("hr.Employee", "E1"),
        ))
        .unwrap()
        .exists()
    );

    // Deactivate the employee via polymorphic `FireTransition` targeting supertype `hr.Person`.
    let deactivate: ActionRoot = serde_json::from_value(json!({
        "actionId": "hr.deactivate_person",
        "scopeRequired": ["orgId", "environmentId"],
        "idempotent": true, "ledgered": true,
        "inputSchema": {
            "type": "object", "additionalProperties": false,
            "properties": {"id": {"type": "string"}}, "required": ["id"]
        },
        "outputSchema": {"type": "object", "additionalProperties": false, "properties": {}},
        "effectProgram": {
            "kind": "fire_transition", "objectIdField": "id",
            "objectKind": "hr.Person", "lifecycle": "status", "transition": "deactivate"
        }
    }))
    .unwrap();
    let deactivate_call = make_call(&deactivate, "user:admin", json!({"id": "E1"}), "deact-1");
    let ActionOutcome::Committed {
        record: deact_record,
        ..
    } = commit_action(&mut cell, &catalog, &deactivate_call, &authz).unwrap()
    else {
        panic!("expected deactivate commit");
    };
    assert_eq!(deact_record.object, FactKey::new("hr.Employee", "E1"));

    // Idempotent replay of the naming-series create succeeds even though `emp.status` is now `inactive`.
    let retry_call = make_call(&submit, "user:alice", payload, "leave-1");
    let ActionOutcome::Replayed { record: replayed } =
        commit_action(&mut cell, &catalog, &retry_call, &authz).unwrap()
    else {
        panic!("expected replay without re-running slice preconditions");
    };
    assert_eq!(replayed, record);
}

fn hr_transition_and_update_actions() -> (ActionRoot, ActionRoot, ActionRoot) {
    let approve: ActionRoot = serde_json::from_value(json!({
        "actionId": "hr.approve_leave",
        "scopeRequired": ["orgId", "environmentId"],
        "idempotent": true, "ledgered": true,
        "independentOf": ["created_by"],
        "inputSchema": {
            "type": "object", "additionalProperties": false,
            "properties": {"id": {"type": "string"}, "employee_id": {"type": "string"}},
            "required": ["id", "employee_id"]
        },
        "outputSchema": {"type": "object", "additionalProperties": false, "properties": {}},
        "slice": [{
            "name": "emp", "objectType": "hr.Employee",
            "key": {"property": {"from": "input", "property": "employee_id"}}
        }],
        "preconditions": ["slice.emp.status == 'active'"],
        "effectProgram": {
            "kind": "fire_transition", "objectIdField": "id",
            "objectKind": "hr.LeaveRequest", "lifecycle": "status", "transition": "approve"
        }
    }))
    .unwrap();
    let escalate: ActionRoot = serde_json::from_value(json!({
        "actionId": "hr.escalate_leave",
        "scopeRequired": ["orgId", "environmentId"],
        "idempotent": true, "ledgered": true,
        "inputSchema": {
            "type": "object", "additionalProperties": false,
            "properties": {"id": {"type": "string"}}, "required": ["id"]
        },
        "outputSchema": {"type": "object", "additionalProperties": false, "properties": {}},
        "effectProgram": {
            "kind": "fire_transition", "objectIdField": "id",
            "objectKind": "hr.LeaveRequest", "lifecycle": "status", "transition": "escalate"
        }
    }))
    .unwrap();
    let update_reason: ActionRoot = serde_json::from_value(json!({
        "actionId": "hr.update_leave_reason",
        "scopeRequired": ["orgId", "environmentId"],
        "idempotent": true, "ledgered": true,
        "inputSchema": {
            "type": "object", "additionalProperties": false,
            "properties": {"id": {"type": "string"}, "reason": {"type": "string"}},
            "required": ["id", "reason"]
        },
        "outputSchema": {"type": "object", "additionalProperties": false, "properties": {}},
        "effectProgram": {
            "kind": "upsert_input_object", "objectIdField": "id",
            "objectKind": "hr.LeaveRequest", "entityArtifactId": "hr.LeaveRequest.entity",
            "entityArtifactVersion": "1.0.0", "writeMode": {"kind": "update_only"}
        }
    }))
    .unwrap();
    (approve, escalate, update_reason)
}

/// `FireTransition` (`independentOf`, guards, external slice) and lifecycle-bearing
/// `UpdateOnly` preserve exact field-level IFC, while transition guards reading
/// PII propagate that marking onto the transitioned lifecycle property.
#[test]
fn fire_transition_and_lifecycle_update_preserve_field_ifc_and_propagate_guard_reads() {
    let defs = hr_a3_types();
    let submit = submit_leave_action();
    let (approve, escalate, update_reason) = hr_transition_and_update_actions();
    let guards = transition_guards([&approve, &escalate]);
    let catalog = TypeCatalog::new(&defs).with_transition_guards(&guards);
    let (_dir, mut cell) = open_cell(&defs, "transition-ifc");
    seed_employee(&mut cell, "E1", "active");
    let authz = ExactPolicyAuthz::of(&["branch_id"]);

    let payload = json!({
        "employee_id": "E1", "branch_id": "cairo", "days": 4,
        "reason": "annual", "private_notes": "medical note"
    });
    commit_action(
        &mut cell,
        &catalog,
        &make_call(&submit, "user:alice", payload.clone(), "sub-1"),
        &authz,
    )
    .unwrap();
    commit_action(
        &mut cell,
        &catalog,
        &make_call(&submit, "user:alice", payload, "sub-2"),
        &authz,
    )
    .unwrap();

    // Lifecycle-bearing UpdateOnly keeps `reason` unmarked despite PII in `private_notes`.
    let upd = make_call(
        &update_reason,
        "user:alice",
        json!({"id": "LR-cairo-0001", "reason": "updated"}),
        "upd-1",
    );
    commit_action(&mut cell, &catalog, &upd, &authz).unwrap();
    let after_upd = markings(&cell, "hr.LeaveRequest", "LR-cairo-0001");
    assert!(after_upd["reason"].is_empty(), "{after_upd:?}");
    assert!(after_upd["private_notes"].contains(PII), "{after_upd:?}");

    // Approve transition reads `days`, `created_by`, `branch_id`, `status`, and `emp.status` — none PII.
    let app = make_call(
        &approve,
        "user:bob",
        json!({"id": "LR-cairo-0001", "employee_id": "E1"}),
        "app-1",
    );
    commit_action(&mut cell, &catalog, &app, &authz).unwrap();
    let after_app = markings(&cell, "hr.LeaveRequest", "LR-cairo-0001");
    assert!(after_app["status"].is_empty(), "{after_app:?}");

    // Escalate transition guard reads `self.private_notes` (PII) -> propagates `pii.contact` to `status`.
    let esc = make_call(
        &escalate,
        "user:bob",
        json!({"id": "LR-cairo-0002"}),
        "esc-1",
    );
    commit_action(&mut cell, &catalog, &esc, &authz).unwrap();
    let after_esc = markings(&cell, "hr.LeaveRequest", "LR-cairo-0002");
    assert!(
        after_esc["status"].contains(PII),
        "guard reading PII must propagate onto status: {after_esc:?}"
    );
}

/// Polymorphic `UpdateOnly` targeting supertype `hr.Person` resolves concrete
/// `hr.Employee`, enforces subtype invariants, and records concrete `hr.Employee`.
#[test]
fn polymorphic_update_only_enforces_concrete_subtype_invariants() {
    let defs = hr_a3_types();
    let catalog = TypeCatalog::new(&defs);
    let (_dir, mut cell) = open_cell(&defs, "poly-update");
    seed_employee(&mut cell, "E1", "active");
    let authz = ExactPolicyAuthz::of(&[]);

    let update_person: ActionRoot = serde_json::from_value(json!({
        "actionId": "hr.update_person",
        "scopeRequired": ["orgId", "environmentId"],
        "idempotent": true, "ledgered": true,
        "inputSchema": {
            "type": "object", "additionalProperties": false,
            "properties": {"id": {"type": "string"}, "name": {"type": "string"}, "badge_no": {"type": "string"}},
            "required": ["id", "name", "badge_no"]
        },
        "outputSchema": {"type": "object", "additionalProperties": false, "properties": {}},
        "effectProgram": {
            "kind": "upsert_input_object", "objectIdField": "id",
            "objectKind": "hr.Person", "entityArtifactId": "hr.Person.entity",
            "entityArtifactVersion": "1.0.0", "writeMode": {"kind": "update_only"}
        }
    }))
    .unwrap();

    // Violating `hr.Employee`'s `badge_non_empty` invariant must refuse even when calling `hr.Person`.
    let bad = make_call(
        &update_person,
        "user:admin",
        json!({"id": "E1", "name": "Nadia", "badge_no": ""}),
        "upd-bad",
    );
    assert!(commit_action(&mut cell, &catalog, &bad, &authz).is_err());

    let ok = make_call(
        &update_person,
        "user:admin",
        json!({"id": "E1", "name": "Nadia K.", "badge_no": "B-202"}),
        "upd-ok",
    );
    let ActionOutcome::Committed { record, .. } =
        commit_action(&mut cell, &catalog, &ok, &authz).unwrap()
    else {
        panic!("expected committed");
    };
    assert_eq!(record.object, FactKey::new("hr.Employee", "E1"));
    let emp_marks = markings(&cell, "hr.Employee", "E1");
    assert!(emp_marks["name"].is_empty(), "{emp_marks:?}");
    assert!(emp_marks["national_id"].contains(PII), "{emp_marks:?}");
}
