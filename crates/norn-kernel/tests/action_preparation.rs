#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "focused action preparation regression"
)]

//! Prepared derivations are opaque, single-use tokens bound to one call.

use std::collections::BTreeMap;
use std::convert::Infallible;

use norn_ir::{ActionRoot, ObjectTypeDef, ZenExpression};
use norn_kernel::cell_host::JsonObject;
use norn_kernel::{
    ActionAuthzRequest, ActionCall, ActionDenial, ActionInputPreparation, ActionOutcome,
    CellCommit, CellHost, DecisionTurn, FactEvent, FactKey, FactMutation, FactVersion, FactWrite,
    LogicalTime, TypeCatalog, World, commit_action, commit_action_with_preparation,
    prepare_action_input, prepare_action_input_for_call,
};
use norn_testkit::cell_host::MemoryCell;
use norn_testkit::proptest;
use serde_json::json;

type Cell = MemoryCell<FactEvent, JsonObject, JsonObject>;

struct Allow;

impl norn_kernel::ActionAuthorizer for Allow {
    fn authorize(&self, _request: &ActionAuthzRequest<'_>) -> Result<(), ActionDenial> {
        Ok(())
    }
}

fn action() -> ActionRoot {
    serde_json::from_value(json!({
        "actionId": "test.target.create",
        "scopeRequired": ["orgId", "environmentId"],
        "idempotent": true,
        "ledgered": true,
        "inputSchema": {
            "type": "object",
            "additionalProperties": false,
            "properties": {"code": {"type": "string"}},
            "required": ["code"]
        },
        "outputSchema": {
            "type": "object",
            "additionalProperties": false,
            "properties": {}
        },
        "derive": [{"field": "code", "expression": "upper(trim(input.code))"}],
        "writes": [{"entityKind": "test.Target", "change": "created"}],
        "effectProgram": {
            "kind": "upsert_input_object",
            "objectKind": "test.Target",
            "objectIdField": "code",
            "entityArtifactId": "test.Target.entity",
            "entityArtifactVersion": "1.0.0",
            "writeMode": {"kind": "create_only"}
        }
    }))
    .unwrap()
}

fn types() -> Vec<ObjectTypeDef> {
    serde_json::from_value(json!([{
        "name": "test.Target",
        "properties": [{"id": "code", "type": {"kind": "text"}}]
    }]))
    .unwrap()
}

fn input(code: &str) -> JsonObject {
    serde_json::from_value(json!({"code": code})).unwrap()
}

fn call<'a>(action: &'a ActionRoot, input: JsonObject, action_version: &'a str) -> ActionCall<'a> {
    ActionCall {
        action,
        action_artifact_id: "test.target.create.action",
        action_version,
        scope: BTreeMap::from([
            ("orgId".to_owned(), "org_demo".to_owned()),
            ("environmentId".to_owned(), "dev".to_owned()),
        ]),
        actor: "test:caller".to_owned(),
        input,
        idempotency_key: "prepared-token".to_owned(),
        object_type: None,
        turn: DecisionTurn {
            id: "prepared-token-turn".to_owned(),
            now: LogicalTime::from_millis(1_000),
            world: World::Main,
            consumes: None,
        },
    }
}

fn cell() -> Cell {
    MemoryCell::new(|_| JsonObject::new()).with_types(types())
}

fn assert_rejected_preparation(
    preparation: ActionInputPreparation<'_>,
    call: &ActionCall<'_>,
    catalog: &TypeCatalog<'_>,
) {
    let mut storage = cell();
    let refusal = commit_action_with_preparation(&mut storage, catalog, call, &Allow, preparation)
        .unwrap_err();
    assert_eq!(refusal.code(), "action.invalid_input");
    assert!(
        !storage
            .load(&FactKey::new("test.Target", "C26"))
            .unwrap()
            .exists()
    );
}

#[test]
fn action_actor_preparation_cannot_be_reused_under_a_different_principal() {
    let action = action();
    let raw = input(" c26 ");
    let definitions = types();
    let catalog = TypeCatalog::new(&definitions);
    let preparation = prepare_action_input_for_call::<Infallible>(
        &action,
        &raw,
        "test.target.create.action",
        "1.0.0",
        "test:caller",
    )
    .unwrap();
    let mut other = call(&action, raw.clone(), "1.0.0");
    other.actor = "person:other".into();
    assert_rejected_preparation(preparation, &other, &catalog);
    assert!(
        prepare_action_input_for_call::<Infallible>(
            &action,
            &raw,
            "test.target.create.action",
            "1.0.0",
            ""
        )
        .is_err()
    );
}

struct PrincipalOnly<'a>(&'a str);
impl norn_kernel::ActionAuthorizer for PrincipalOnly<'_> {
    fn authorize(&self, request: &ActionAuthzRequest<'_>) -> Result<(), ActionDenial> {
        if request.actor == self.0 {
            Ok(())
        } else {
            Err(ActionDenial::Forbidden {
                reason: "wrong principal".into(),
            })
        }
    }
}

proptest::proptest! {
    #[test]
    fn action_actor_is_identical_in_derivation_pdp_and_retained_receipt(principal in "person:[a-z0-9]{1,24}") {
        let mut action = action();
        action.derive = vec![norn_ir::ActionInputDerivation {field:"code".into(),expression:ZenExpression("actor.subjectId".into()),input_required:false}];
        let mut invocation = call(&action,JsonObject::new(),"1.0.0"); invocation.actor = principal.clone();
        let definitions = types(); let catalog = TypeCatalog::new(&definitions); let mut storage = cell();
        let outcome = commit_action(&mut storage,&catalog,&invocation,&PrincipalOnly(&principal)).unwrap();
        let ActionOutcome::Committed {record,..} = outcome else { proptest::prop_assert!(false,"fresh call must commit"); return Ok(()); };
        proptest::prop_assert_eq!(&record.actor,&principal);
        let expected = norn_ir::RuntimeValue::String(principal.clone());
        proptest::prop_assert_eq!(record.derived_values.first().map(|d| &d.value),Some(&expected));
        let retained = storage.load(&FactKey::new("test.Target",principal.as_str())).unwrap();
        proptest::prop_assert_eq!(retained.values.as_ref().and_then(|v| v.get("code")).and_then(norn_ir::RuntimeValue::as_string),Some(principal.as_str()));
    }

    #[test]
    fn action_actor_and_approval_spoofs_refuse_at_every_input_depth(actor_field in proptest::bool::ANY, depth in 0usize..6, forgery in "[a-zA-Z0-9]{0,24}") {
        let action = action(); let field = if actor_field {"actor"} else {"approved"};
        let mut payload = json!({field:forgery});
        for _ in 0..depth { payload = json!({"nested":[payload]}); }
        let raw: JsonObject = serde_json::from_value(payload).unwrap();
        let error = prepare_action_input_for_call::<Infallible>(&action,&raw,"test.target.create.action","1.0.0","person:authenticated").unwrap_err();
        proptest::prop_assert!(matches!(error,norn_kernel::ActionRefusal::InvalidInput {field: ref rejected} if rejected == field), "caller authority field must refuse");
    }
}

#[test]
fn prepared_input_token_rejects_changed_raw_input() {
    let action = action();
    let types = types();
    let catalog = TypeCatalog::new(&types);
    let raw = input(" c26 ");

    let preparation = prepare_action_input_for_call::<Infallible>(
        &action,
        &raw,
        "test.target.create.action",
        "1.0.0",
        "test:caller",
    )
    .unwrap();
    let wrong_input = call(&action, input("different"), "1.0.0");
    assert_rejected_preparation(preparation, &wrong_input, &catalog);
}

#[test]
fn prepared_input_token_rejects_changed_action_version() {
    let action = action();
    let types = types();
    let catalog = TypeCatalog::new(&types);
    let raw = input(" c26 ");
    let preparation = prepare_action_input_for_call::<Infallible>(
        &action,
        &raw,
        "test.target.create.action",
        "1.0.0",
        "test:caller",
    )
    .unwrap();
    let wrong_version = call(&action, raw.clone(), "2.0.0");
    assert_rejected_preparation(preparation, &wrong_version, &catalog);
}

#[test]
fn prepared_input_token_rejects_changed_action_artifact() {
    let action = action();
    let types = types();
    let catalog = TypeCatalog::new(&types);
    let raw = input(" c26 ");
    let preparation = prepare_action_input_for_call::<Infallible>(
        &action,
        &raw,
        "test.target.create.action",
        "1.0.0",
        "test:caller",
    )
    .unwrap();
    let mut wrong_artifact = call(&action, raw.clone(), "1.0.0");
    wrong_artifact.action_artifact_id = "test.other.action";
    assert_rejected_preparation(preparation, &wrong_artifact, &catalog);
}

#[test]
fn prepared_input_token_rejects_changed_action_definition() {
    let action = action();
    let types = types();
    let catalog = TypeCatalog::new(&types);
    let raw = input(" c26 ");
    let preparation = prepare_action_input_for_call::<Infallible>(
        &action,
        &raw,
        "test.target.create.action",
        "1.0.0",
        "test:caller",
    )
    .unwrap();
    let mut changed_action = action.clone();
    changed_action
        .derive
        .first_mut()
        .expect("fixture has a derivation")
        .expression = ZenExpression("lower(trim(input.code))".to_owned());
    let wrong_action = call(&changed_action, raw.clone(), "1.0.0");
    assert_rejected_preparation(preparation, &wrong_action, &catalog);
}

#[test]
fn prepared_input_commits_derived_evidence_and_replays_the_same_record() {
    let action = action();
    let types = types();
    let catalog = TypeCatalog::new(&types);
    let raw = input(" c26 ");
    let call = call(&action, raw.clone(), "1.0.0");
    let preparation = prepare_action_input_for_call::<Infallible>(
        &action,
        &raw,
        call.action_artifact_id,
        call.action_version,
        "test:caller",
    )
    .unwrap();
    let mut storage = cell();

    let committed =
        commit_action_with_preparation(&mut storage, &catalog, &call, &Allow, preparation).unwrap();
    let ActionOutcome::Committed { record, .. } = committed else {
        panic!("the bound preparation should commit");
    };
    assert_eq!(record.input, raw);
    assert_eq!(
        record
            .effective_input
            .as_ref()
            .and_then(|input| input.get("code")),
        Some(&norn_kernel::cell_host::JsonValue::String("C26".to_owned()))
    );
    assert_eq!(record.derived_values.len(), 1);
    let derived = record
        .derived_values
        .first()
        .expect("the action records its declared derivation");
    assert_eq!(derived.field, "code");
    assert_eq!(
        derived.value,
        norn_kernel::cell_host::JsonValue::String("C26".to_owned())
    );

    let replayed = commit_action(&mut storage, &catalog, &call, &Allow).unwrap();
    let ActionOutcome::Replayed {
        record: replayed_record,
    } = replayed
    else {
        panic!("the same raw call should replay");
    };
    assert_eq!(record, replayed_record);
}

#[test]
fn unbound_input_preparation_cannot_be_committed() {
    let action = action();
    let types = types();
    let catalog = TypeCatalog::new(&types);
    let raw = input(" c26 ");
    let preparation = prepare_action_input::<Infallible>(&action, &raw).unwrap();
    let mut storage = cell();
    let call = call(&action, raw.clone(), "1.0.0");

    let refusal =
        commit_action_with_preparation(&mut storage, &catalog, &call, &Allow, preparation)
            .unwrap_err();
    assert_eq!(refusal.code(), "action.invalid_input");
    assert!(
        !storage
            .load(&FactKey::new("test.Target", "C26"))
            .unwrap()
            .exists()
    );
}

#[test]
fn underived_twenty_kib_input_is_retained_once_and_commits_a_small_transition() {
    let mut action = action();
    action.derive.clear();
    action.writes =
        serde_json::from_value(json!([{"entityKind":"test.Target", "change":"modified"}]))
            .expect("transition write effect");
    action.preconditions = vec![ZenExpression("true".to_owned())];
    action.input_schema = serde_json::from_value(json!({
        "type":"object", "additionalProperties":false,
        "properties":{"code":{"type":"string"}, "memo":{"type":"string"}},
        "required":["code", "memo"]
    }))
    .expect("input schema");
    action.effect_program = Some(
        serde_json::from_value(json!({
            "kind":"fire_transition", "objectKind":"test.Target", "objectIdField":"code",
            "entityArtifactId":"test.Target.entity", "entityArtifactVersion":"1.0.0",
            "lifecycle":"status", "transition":"approve"
        }))
        .expect("transition effect"),
    );
    let types: Vec<ObjectTypeDef> = serde_json::from_value(json!([{
        "name":"test.Target", "properties":[
            {"id":"code", "type":{"kind":"text"}},
            {"id":"status", "type":{"kind":"enum", "values":[{"value":"draft"}, {"value":"approved"}]}}
        ], "lifecycles":[{"property":"status", "initial":["draft"], "terminal":["approved"],
            "transitions":[{"id":"approve", "from":["draft"], "to":"approved"}]}]
    }])).expect("transition entity");
    let catalog = TypeCatalog::new(&types);
    let raw: JsonObject =
        serde_json::from_value(json!({"code":"C26", "memo":"m".repeat(20 * 1024)}))
            .expect("raw input");
    let invocation = call(&action, raw.clone(), "1.0.0");
    let preparation = prepare_action_input_for_call::<Infallible>(
        &action,
        &raw,
        invocation.action_artifact_id,
        invocation.action_version,
        "test:caller",
    )
    .expect("only the retained raw input counts toward receipt evidence");
    assert_eq!(preparation.effective_input(), &raw);
    let mut storage: Cell = MemoryCell::new(|_| JsonObject::new()).with_types(types.clone());
    let target = FactKey::new("test.Target", "C26");
    storage
        .commit(CellCommit {
            writes: vec![FactWrite {
                key: target.clone(),
                expected: FactVersion::ABSENT,
                mutation: FactMutation::Put {
                    values: serde_json::from_value(json!({"code":"C26", "status":"draft"}))
                        .expect("target values"),
                },
            }],
            ..CellCommit::empty()
        })
        .expect("seed target");
    let outcome =
        commit_action_with_preparation(&mut storage, &catalog, &invocation, &Allow, preparation)
            .expect("the exact record and small transition fit the output envelope");
    let ActionOutcome::Committed { record, .. } = outcome else {
        panic!("first call commits");
    };
    assert_eq!(record.input, raw);
    assert_eq!(record.effective_input, None);
    assert!(record.derived_values.is_empty());
    assert_eq!(record.precondition_outcomes.len(), 1);
    assert!(
        record
            .precondition_outcomes
            .first()
            .expect("true precondition")
            .satisfied
    );
    assert_eq!(
        storage.load(&target).expect("target").values,
        Some(
            serde_json::from_value(json!({"code":"C26", "status":"approved"}))
                .expect("expected values")
        )
    );
}
