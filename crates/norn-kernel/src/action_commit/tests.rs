#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "kernel proof assertions"
)]
use std::collections::{BTreeMap, BTreeSet};

use norn_ir::{ActionRoot, Decimal, ObjectTypeName, RuntimeValue};

use crate::cell_host::{
    CellCommit, FactExpectation, FactKey, FactMutation, FactVersion, FactWrite, JsonObject,
    LinkKey, LinkReadExpectation, LinkReadTarget, PredicateQuery, PredicateReadExpectation,
    PredicateReadMember,
};
use crate::decision_commit::DecisionTurn;
use crate::observations::{LogicalTime, World};

use super::approval::{PreparedCommit, bind_approved_intent};
use super::{
    ACTION_CALL_TYPE, ActionCall, ActionCallRecord, ActionParameterPin, ActionReferenceReads,
    ActionRefusal, prepare_action_input,
};

#[test]
fn decimal_input_preparation_keeps_wire_text_and_exact_execution_value() {
    let mut action: ActionRoot = serde_json::from_str(
        r#"{
      "actionId":"test.decimal", "scopeRequired":["orgId","environmentId"],
      "idempotent":true, "ledgered":true,
      "inputSchema":{"type":"object","properties":{"amount":{"type":"number","nornType":{"kind":"decimal","scale":2}}},
        "required":["amount"],"additionalProperties":false},
      "outputSchema":{"type":"object","properties":{},"additionalProperties":false}
    }"#,
    )
    .expect("action");
    action.derive.clear();
    action.slice.clear();
    action.preconditions.clear();
    let raw = JsonObject::from([(
        "amount".to_owned(),
        RuntimeValue::String("9007199254740993.25".to_owned()),
    )]);
    let prepared = prepare_action_input::<()>(&action, &raw).expect("exact input");
    assert_eq!(prepared.raw_input, &raw);
    assert_eq!(
        prepared.recorded_effective_input(),
        Some(prepared.effective_input.clone())
    );
    assert!(prepared.receipt_evidence_bytes > 0);
    assert_eq!(
        prepared.effective_input.get("amount"),
        Some(&RuntimeValue::Number(
            Decimal::parse("9007199254740993.25").expect("decimal")
        ))
    );
    let number = JsonObject::from([(
        "amount".to_owned(),
        RuntimeValue::Number(Decimal::from(1_u64)),
    )]);
    assert!(matches!(
        prepare_action_input::<()>(&action, &number),
        Err(ActionRefusal::InvalidInput { .. })
    ));
    action.derive = serde_json::from_str(
        r#"[{"field":"amount","expression":"input.amount ?? 0","inputRequired":false}]"#,
    )
    .expect("default");
    for raw in [
        JsonObject::new(),
        JsonObject::from([("amount".to_owned(), RuntimeValue::Null)]),
    ] {
        assert_eq!(
            prepare_action_input::<()>(&action, &raw)
                .expect("defaulted decimal")
                .effective_input
                .get("amount"),
            Some(&RuntimeValue::Number(Decimal::ZERO))
        );
    }
}

norn_testkit::proptest::proptest! {
    #![proptest_config(norn_testkit::proptest::test_runner::Config::with_cases(64))]
    #[test]
    fn exact_action_decimal_wire_matches_the_integer_reference(coefficient in norn_testkit::proptest::prelude::any::<i64>(), fraction in 0_u8..100) {
        let text = format!("{coefficient}.{fraction:02}");
        let wire = serde_json::from_str(r#"{"type":"number","nornType":{"kind":"decimal","scale":2}}"#).unwrap();
        let schema = norn_expr::schema::Schema::parse_action(&wire, "amount").unwrap();
        let decoded = schema.decode_action_input(RuntimeValue::String(text.clone()), "amount").unwrap();
        let expected = if fraction == 0 { coefficient.to_string() } else { text.trim_end_matches('0').to_owned() };
        norn_testkit::proptest::prop_assert_eq!(decoded.as_decimal().unwrap().canonical(), expected);
    }
}

#[test]
fn reference_proof_rejects_changed_outputs_and_keeps_added_guards_full() {
    let reference = FactExpectation {
        key: FactKey::new("example.Customer", "customer"),
        expected: FactVersion(1),
    };
    let commit = CellCommit::<(), (), ()> {
        reads: vec![reference.clone()],
        writes: vec![FactWrite {
            key: FactKey::new("example.Order", "order"),
            expected: FactVersion(0),
            mutation: FactMutation::Put {
                values: JsonObject::new(),
            },
        }],
        ..CellCommit::empty()
    };
    let proof = || ActionReferenceReads {
        reads: commit.reads.clone(),
        references: commit.reads.clone(),
        property_reads: vec![(reference.clone(), vec!["contact".to_owned()])],
        writes: commit.writes.clone(),
        links: Vec::new(),
        link_reads: Vec::new(),
        event_count: 0,
        laws_digest: ActionReferenceReads::governing_laws_digest(&[]).expect("digest"),
    };
    let mut changed = commit.clone();
    changed.writes[0].mutation = FactMutation::Put {
        values: BTreeMap::from([(
            "copiedContact".to_owned(),
            RuntimeValue::String("private".to_owned()),
        )]),
    };
    assert!(!proof().validates(&changed));
    changed = commit.clone();
    changed.reads[0].expected = FactVersion(2);
    assert!(!proof().validates(&changed));
    let mut guarded = commit.clone();
    let guarded_proof = proof()
        .with_read_guards(&mut guarded, std::slice::from_ref(&reference))
        .expect("additional full guard");
    assert_eq!(guarded_proof.reference_reads(), []);
    assert_eq!(guarded_proof.property_reads(), []);
    assert!(guarded_proof.validates(&guarded));
    let mismatch = FactExpectation {
        key: commit.reads[0].key.clone(),
        expected: FactVersion(3),
    };
    assert!(
        proof()
            .with_read_guards(&mut commit.clone(), &[mismatch])
            .is_none()
    );
}

#[test]
#[allow(
    clippy::expect_used,
    reason = "generated receipt schema correspondence"
)]
fn action_call_record_schema_matches_generated_contract() {
    let generated = serde_json::to_string_pretty(&schemars::schema_for!(ActionCallRecord))
        .expect("receipt schema")
        + "\n";
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("schema/action_call_record.schema.json");
    if std::env::var("NORN_KERNEL_UPDATE_SCHEMAS").as_deref() == Ok("1") {
        std::fs::create_dir_all(path.parent().expect("schema directory"))
            .expect("schema directory");
        std::fs::write(&path, &generated).expect("write generated schema");
    }
    assert_eq!(
        generated,
        std::fs::read_to_string(path).expect("generated schema")
    );
}

#[test]
#[allow(
    clippy::expect_used,
    reason = "receipt schema precision guidance assertion"
)]
fn receipt_schema_keeps_exact_number_guidance_on_runtime_value_fields() {
    let schema =
        serde_json::to_value(schemars::schema_for!(ActionCallRecord)).expect("receipt schema");
    for description in [
        &schema["properties"]["effectiveInput"]["description"],
        &schema["$defs"]["ActionDerivedValue"]["properties"]["value"]["description"],
    ] {
        let text = description.as_str().expect("field description");
        assert!(text.contains("exact-number JSON parser"), "{text}");
        assert!(text.contains("JSON.parse"), "{text}");
    }
}

/// #1701: a receipt pin on a supertype and a write of its concrete subtype
/// are one expectation: the same version adds no read (reads stay disjoint
/// from writes), another version is a stale read; an untouched object's pin
/// is a new read.
#[test]
fn receipt_pins_merge_with_writes_by_canonical_identity() -> Result<(), serde_json::Error> {
    let types: Vec<norn_ir::ObjectTypeDef> = serde_json::from_value(serde_json::json!([
        {"name": "receipt.Base"},
        {"name": "receipt.Child", "extends": "receipt.Base"}
    ]))?;
    let catalog = crate::decision_commit::TypeCatalog::new(&types);
    let writes = [FactWrite {
        key: FactKey::new("receipt.Child", "k"),
        expected: FactVersion(3),
        mutation: FactMutation::Put {
            values: JsonObject::new(),
        },
    }];
    let pin = |key: &str, version| FactExpectation {
        key: FactKey::new("receipt.Base", key),
        expected: FactVersion(version),
    };
    let mut reads = Vec::new();
    let merged =
        super::receipt::merge_receipt_pins::<()>(&catalog, &[pin("k", 3)], &writes, &mut reads);
    assert!(merged.is_ok());
    assert_eq!(reads.len(), 0);
    let stale =
        super::receipt::merge_receipt_pins::<()>(&catalog, &[pin("k", 2)], &writes, &mut reads);
    assert!(matches!(
        stale,
        Err(ActionRefusal::StalePreconditionRead { ref key, link: None })
            if *key == FactKey::new("receipt.Base", "k")
    ));
    assert_eq!(reads.len(), 0);
    let fresh =
        super::receipt::merge_receipt_pins::<()>(&catalog, &[pin("other", 1)], &writes, &mut reads);
    assert!(fresh.is_ok());
    assert_eq!(reads, [pin("other", 1)]);
    Ok(())
}

/// The evidence a commit was decided on: its pinned parameters, fact reads,
/// link reads and predicate memberships.
type Evidence = (
    Vec<ActionParameterPin>,
    Vec<FactExpectation>,
    Vec<LinkReadExpectation>,
    Vec<PredicateReadExpectation>,
);

fn decided_on<'a>(
    keys: &'a (FactKey, FactKey),
    input: &'a JsonObject,
    evidence: &'a Evidence,
) -> PreparedCommit<'a> {
    PreparedCommit {
        target: &keys.0,
        effective_input: input,
        record: &keys.1,
        parameter_pins: &evidence.0,
        consumes: None,
        reads: &evidence.1,
        link_reads: &evidence.2,
        predicate_reads: &evidence.3,
        writes: &[],
        links: &[],
        effects: &[],
        quantity: None,
    }
}

/// The evidence of one commit, with each piece at the version `at` names:
/// `[pinned, read, linked, member]`.
fn evidence_at(setting: &FactKey, [pinned, read, linked, member]: [u64; 4]) -> Evidence {
    let owner = LinkKey::new(setting.clone(), "owner", FactKey::new("test.Owner", "W-1"));
    let selected = PredicateQuery {
        object_type: ObjectTypeName("test.Setting".to_owned()),
        filter: BTreeMap::new(),
        window: None,
    };
    (
        vec![ActionParameterPin {
            entry: "rate".to_owned(),
            setting_key: "test.rate".to_owned(),
            effective_at: "2026-06-30T00:00:00Z".to_owned(),
            setting: setting.clone(),
            version: pinned,
            digest: format!("sha256:{}", "0".repeat(64)),
        }],
        vec![FactExpectation {
            key: setting.clone(),
            expected: FactVersion(read),
        }],
        vec![LinkReadExpectation {
            source: setting.clone(),
            link: "owner".to_owned(),
            targets: vec![LinkReadTarget {
                key: owner,
                version: FactVersion(linked),
            }],
        }],
        vec![PredicateReadExpectation {
            predicate: selected,
            members: vec![PredicateReadMember {
                key: setting.clone(),
                version: FactVersion(member),
            }],
        }],
    )
}

/// A call of `action` with no input, on a fixed turn.
fn bind_call(action: &ActionRoot) -> ActionCall<'_> {
    ActionCall {
        action,
        action_artifact_id: "test.bind.action",
        action_version: "1.0.0",
        scope: BTreeMap::new(),
        actor: "operator".to_owned(),
        approved_intent: None,
        input: JsonObject::new(),
        idempotency_key: "bind-1".to_owned(),
        object_type: None,
        turn: DecisionTurn {
            id: "turn-bind".to_owned(),
            now: LogicalTime::from_millis(1_000),
            world: World::Main,
            consumes: None,
        },
        trace: None,
    }
}

/// AUTH-INDEPENDENT-PERSON (#618), exhaustive over the evidence a commit was
/// decided on: with everything else equal, a changed pinned parameter, fact
/// read, link read or predicate membership alone is another prepared commit,
/// and a commit under the unchanged one's approval is refused.
#[test]
fn approved_intent_binds_each_piece_of_decision_evidence() {
    let action: ActionRoot = serde_json::from_str(
        r#"{"actionId":"test.bind","scopeRequired":[],"idempotent":true,"ledgered":true,
          "inputSchema":{"type":"object","properties":{},"additionalProperties":false},
          "outputSchema":{"type":"object","properties":{},"additionalProperties":false}}"#,
    )
    .expect("action");
    let call = bind_call(&action);
    let setting = FactKey::new("test.Setting", "S-1");
    let keys = (
        FactKey::new("test.Order", "O-1"),
        FactKey::new(ACTION_CALL_TYPE, "test.bind/bind-1"),
    );
    let evidence: Vec<Evidence> = [
        [1, 1, 1, 1],
        [2, 1, 1, 1],
        [1, 2, 1, 1],
        [1, 1, 2, 1],
        [1, 1, 1, 2],
    ]
    .into_iter()
    .map(|at| evidence_at(&setting, at))
    .collect();
    let digests: Vec<String> = evidence
        .iter()
        .map(|decided| {
            let mut slot = None;
            let captured = bind_approved_intent::<()>(
                &call,
                &decided_on(&keys, &call.input, decided),
                Some(&mut slot),
            );
            assert!(matches!(captured, Err(ActionRefusal::TargetNotFound)));
            slot.map(|intent| intent.digest).expect("captured")
        })
        .collect();
    assert_eq!(
        digests.iter().collect::<BTreeSet<_>>().len(),
        evidence.len()
    );
    let approved = ActionCall {
        approved_intent: digests.first().cloned(),
        ..call.clone()
    };
    for (index, decided) in evidence.iter().enumerate() {
        let bound =
            bind_approved_intent::<()>(&approved, &decided_on(&keys, &call.input, decided), None);
        if index == 0 {
            assert!(bound.is_ok(), "{bound:?}");
        } else {
            assert!(
                matches!(bound, Err(ActionRefusal::ApprovedIntentChanged)),
                "{index}: {bound:?}"
            );
        }
    }
}

fn hr_create_types() -> Vec<norn_ir::ObjectTypeDef> {
    serde_json::from_value(serde_json::json!([
        {
            "name": "hr.Person",
            "identity": [{"name": "primary", "properties": ["id"]}],
            "properties": [
                {"id": "id", "type": {"kind": "text"}},
                {"id": "status", "type": {"kind": "text"}},
                {"id": "department", "type": {"kind": "text"}},
                {"id": "national_id", "type": {"kind": "text"}}
            ]
        },
        {
            "name": "hr.Employee",
            "extends": "hr.Person",
            "properties": [
                {"id": "badge", "type": {"kind": "text"}},
                {"id": "computed_summary", "type": {"kind": "text"}, "computed": "self.national_id"}
            ]
        },
        {
            "name": "hr.LeaveRequest",
            "identity": [{"name": "primary", "properties": ["id"]}],
            "properties": [
                {"id": "id", "type": {"kind": "text"}},
                {"id": "days", "type": {"kind": "integer"}}
            ],
            "links": [{
                "id": "employee",
                "target": "hr.Person",
                "inverse": "leave_requests",
                "cardinality": {"to": "one", "from": "many"}
            }]
        }
    ]))
    .expect("types")
}

fn hr_create_action(
    preconditions: &serde_json::Value,
    slice: &serde_json::Value,
    create_links: &serde_json::Value,
) -> ActionRoot {
    serde_json::from_value(serde_json::json!({
        "actionId": "hr.submit_leave",
        "scopeRequired": ["orgId", "environmentId"],
        "idempotent": true,
        "ledgered": true,
        "inputSchema": {
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "id": {"type": "string"},
                "employee_id": {"type": "string"},
                "days": {"type": "integer"}
            },
            "required": ["id", "employee_id", "days"]
        },
        "outputSchema": {"type": "object", "additionalProperties": false, "properties": {}},
        "slice": slice,
        "preconditions": preconditions,
        "createLinks": create_links,
        "effectProgram": {
            "kind": "upsert_input_object",
            "objectIdField": "id",
            "objectKind": "hr.LeaveRequest",
            "entityArtifactId": "hr.LeaveRequest.entity",
            "entityArtifactVersion": "1.0.0",
            "writeMode": {"kind": "create_only"}
        }
    }))
    .expect("action")
}

fn hr_law(id: &str, subject: &str, expression: &str) -> norn_ir::ArtifactEnvelope {
    serde_json::from_value(serde_json::json!({
        "artifactId": format!("hr.{id}.law"),
        "version": "1.0.0",
        "kind": "law",
        "name": id,
        "namespace": "hr",
        "irVersion": "0.1",
        "lifecycle": "draft",
        "dependencies": [],
        "scope": {"orgId": "org_demo", "environmentId": "dev"},
        "provenance": {"source": "system", "sourcePath": "test"},
        "payload": {
            "id": id,
            "lawVersion": "0.1",
            "modality": {"kind": "always"},
            "owner": "hr",
            "provenance": {"source": "system", "sourcePath": "test"},
            "rationale": id,
            "response": {"kind": "predicate", "expression": expression},
            "scope": {"objectTypes": [subject], "root": {"kind": "entity", "id": subject}},
            "severity": "error",
            "subject": subject
        }
    }))
    .expect("law")
}

#[test]
fn create_slice_property_reads_merges_polymorphic_aliases_and_link_laws() {
    let types = hr_create_types();
    let target = FactKey::new("hr.LeaveRequest", "L1");
    let catalog = crate::decision_commit::TypeCatalog::new(&types);
    let aliased_action = hr_create_action(
        &serde_json::json!([
            "slice.first.status == 'active'",
            "slice.second.department == 'eng'"
        ]),
        &serde_json::json!([
            {"name": "first", "objectType": "hr.Person", "key": {"property": {"from": "input", "property": "employee_id"}}},
            {"name": "second", "objectType": "hr.Employee", "key": {"property": {"from": "input", "property": "employee_id"}}}
        ]),
        &serde_json::json!([
            {"link": "employee", "targetType": "hr.Person", "targetIdField": "employee_id"}
        ]),
    );
    let emp_ref = FactExpectation {
        key: FactKey::new("hr.Employee", "E1"),
        expected: FactVersion(1),
    };
    let input = JsonObject::from([
        ("id".to_owned(), RuntimeValue::String("L1".to_owned())),
        (
            "employee_id".to_owned(),
            RuntimeValue::String("E1".to_owned()),
        ),
    ]);
    let merged = super::receipt::create_reference_property_reads(
        &catalog,
        &aliased_action,
        &target,
        &input,
        std::slice::from_ref(&emp_ref),
    )
    .expect("aliased polymorphic reads proved");
    assert_eq!(
        merged,
        vec![(
            emp_ref.clone(),
            vec!["department".to_owned(), "status".to_owned()]
        )]
    );

    let extra_ref = FactExpectation {
        key: FactKey::new("hr.Person", "E2"),
        expected: FactVersion(1),
    };
    assert!(
        super::receipt::create_reference_property_reads(
            &catalog,
            &aliased_action,
            &target,
            &input,
            &[emp_ref.clone(), extra_ref],
        )
        .is_none()
    );

    let forward_law = hr_law(
        "leave_checks_nid",
        "hr.LeaveRequest",
        "self.employee.national_id != '000'",
    );
    let law_catalog = crate::decision_commit::TypeCatalog::new(&types)
        .with_governing_laws(std::slice::from_ref(&forward_law));
    let empty_precond = hr_create_action(
        &serde_json::json!([]),
        &serde_json::json!([
            {"name": "emp", "objectType": "hr.Person", "key": {"property": {"from": "input", "property": "employee_id"}}}
        ]),
        &serde_json::json!([
            {"link": "employee", "targetType": "hr.Person", "targetIdField": "employee_id"}
        ]),
    );
    let law_reads = super::receipt::create_reference_property_reads(
        &law_catalog,
        &empty_precond,
        &target,
        &input,
        std::slice::from_ref(&emp_ref),
    )
    .expect("forward link law read attributed");
    assert_eq!(law_reads, vec![(emp_ref, vec!["national_id".to_owned()])]);
    assert_ne!(
        ActionReferenceReads::governing_laws_digest(&[]),
        ActionReferenceReads::governing_laws_digest(std::slice::from_ref(&forward_law))
    );
}

#[test]
fn create_slice_property_reads_fails_closed_on_whole_object_computed_and_inverse_laws() {
    let types = hr_create_types();
    let target = FactKey::new("hr.LeaveRequest", "L1");
    let catalog = crate::decision_commit::TypeCatalog::new(&types);
    let emp_slice = serde_json::json!([
        {"name": "emp", "objectType": "hr.Employee", "key": {"property": {"from": "input", "property": "employee_id"}}}
    ]);
    for precond in [
        serde_json::json!(["slice.emp != null"]),
        serde_json::json!(["slice.emp.computed_summary != ''"]),
    ] {
        let action = hr_create_action(&precond, &emp_slice, &serde_json::json!([]));
        assert!(super::receipt::create_slice_property_reads(&catalog, &action, &target).is_none());
    }
    let sourced_link = hr_create_action(
        &serde_json::json!([]),
        &emp_slice,
        &serde_json::json!([{
            "source": {"objectType": "hr.LeaveRequest", "idField": "id"},
            "link": "employee",
            "targetType": "hr.Employee",
            "targetIdField": "employee_id"
        }]),
    );
    assert!(
        super::receipt::create_slice_property_reads(&catalog, &sourced_link, &target).is_none()
    );
    let inverse_law = hr_law(
        "person_leave_cap",
        "hr.Person",
        "sum(map(self.leave_requests, #.days)) <= 30",
    );
    let inverse_catalog = crate::decision_commit::TypeCatalog::new(&types)
        .with_governing_laws(std::slice::from_ref(&inverse_law));
    let empty_precond = hr_create_action(
        &serde_json::json!([]),
        &serde_json::json!([
            {"name": "emp", "objectType": "hr.Person", "key": {"property": {"from": "input", "property": "employee_id"}}}
        ]),
        &serde_json::json!([
            {"link": "employee", "targetType": "hr.Person", "targetIdField": "employee_id"}
        ]),
    );
    assert!(
        super::receipt::create_slice_property_reads(&inverse_catalog, &empty_precond, &target)
            .is_none()
    );
}
