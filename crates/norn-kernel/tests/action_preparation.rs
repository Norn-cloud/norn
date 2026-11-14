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

#[test]
fn action_actor_noncanonical_principal_refuses_before_any_write() {
    let action = action();
    let definitions = types();
    let catalog = TypeCatalog::new(&definitions);
    let mut invocation = call(&action, input(" c26 "), "1.0.0");
    invocation.actor = "person:g\u{301}".into();
    let mut storage = cell();
    assert_eq!(
        commit_action(&mut storage, &catalog, &invocation, &Allow)
            .unwrap_err()
            .code(),
        "action.invalid_input"
    );
    assert_eq!(storage.ledger(), []);
    assert!(
        !storage
            .load(&FactKey::new("test.Target", "C26"))
            .unwrap()
            .exists()
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

#[test]
fn featureless_actions_keep_large_inputs_without_allowing_identity_claims() {
    let mut action = action();
    action.derive.clear();
    action.input_schema = serde_json::from_value(json!({
        "type":"object", "additionalProperties":false,
        "properties":{"code":{"type":"string"},"large":{"type":"string"}},
        "required":["code","large"]
    }))
    .unwrap();
    let mut definitions = types();
    definitions[0].properties.push(
        serde_json::from_value(json!({
            "id":"large", "type":{"kind":"text"}
        }))
        .unwrap(),
    );
    let catalog = TypeCatalog::new(&definitions);
    let limit = usize::try_from(
        norn_ir::ResourceLimitProfile::norn_limits_v1()
            .slice
            .max_canonical_bytes,
    )
    .unwrap();
    let mut raw = input("C26");
    raw.insert(
        "large".into(),
        norn_ir::RuntimeValue::String("x".repeat(limit + 1)),
    );
    let mut storage = Cell::new(|_| JsonObject::new()).with_types(definitions.clone());
    assert!(matches!(
        commit_action(
            &mut storage,
            &catalog,
            &call(&action, raw.clone(), "1.0.0"),
            &Allow
        )
        .unwrap(),
        ActionOutcome::Committed { .. }
    ));
    for field in ["actor", "approved"] {
        let mut spoof = raw.clone();
        spoof.insert(field.into(), norn_ir::RuntimeValue::String("forged".into()));
        assert!(
            matches!(prepare_action_input_for_call::<Infallible>(&action, &spoof, "test.target.create.action", "1.0.0", "test:caller"), Err(norn_kernel::ActionRefusal::InvalidInput {field: rejected}) if rejected == field)
        );
    }
    action.derive = vec![norn_ir::ActionInputDerivation {
        field: "code".into(),
        expression: ZenExpression("input.code".into()),
        input_required: true,
    }];
    assert!(matches!(
        prepare_action_input_for_call::<Infallible>(
            &action,
            &raw,
            "test.target.create.action",
            "1.0.0",
            "test:caller"
        ),
        Err(norn_kernel::ActionRefusal::InputBoundExceeded { .. })
    ));
}

#[test]
fn action_actor_decimal_derivation_commits_and_forged_actor_refuses() {
    let mut action = action();
    action.input_schema = serde_json::from_value(json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "code": {"type": "string"},
            "amount": {"type": "number", "nornType": {"kind": "decimal", "scale": 2}},
            "base": {"type": "number", "nornType": {"kind": "decimal", "scale": 2}},
            "discount": {"type": "number", "nornType": {"kind": "decimal", "scale": 2}}
        },
        "required": ["code", "base", "discount"]
    }))
    .unwrap();
    action.derive = vec![norn_ir::ActionInputDerivation {
        field: "amount".into(),
        expression: ZenExpression(
            "actor.subjectId == 'person:vip' ? input.discount : input.base".into(),
        ),
        input_required: false,
    }];
    let definitions: Vec<ObjectTypeDef> = serde_json::from_value(json!([{
        "name": "test.Target",
        "properties": [
            {"id": "code", "type": {"kind": "text"}},
            {"id": "amount", "type": {"kind": "decimal", "scale": 2}}
        ]
    }]))
    .unwrap();
    let catalog = TypeCatalog::new(&definitions);
    let priced = |code: &str| -> JsonObject {
        serde_json::from_value(json!({"code": code, "base": "2.25", "discount": "1.5"})).unwrap()
    };
    for (principal, code, expected) in [
        ("person:vip", "VIP", "1.50"),
        ("person:other", "OTHER", "2.25"),
    ] {
        let mut invocation = call(&action, priced(code), "1.0.0");
        invocation.actor = principal.into();
        let mut storage = cell();
        let outcome = commit_action(
            &mut storage,
            &catalog,
            &invocation,
            &PrincipalOnly(principal),
        );
        assert!(
            matches!(outcome, Ok(ActionOutcome::Committed { .. })),
            "{outcome:?}"
        );
        let retained = storage.load(&FactKey::new("test.Target", code)).unwrap();
        let amount = retained
            .values
            .as_ref()
            .and_then(|values| values.get("amount"));
        assert_eq!(
            amount.and_then(norn_ir::RuntimeValue::as_decimal),
            Some(&norn_ir::Decimal::parse(expected).unwrap())
        );
    }
    let mut forged = priced("FORGED");
    forged.insert(
        "actor".into(),
        serde_json::from_value(json!({"subjectId": "person:vip"})).unwrap(),
    );
    let mut invocation = call(&action, forged, "1.0.0");
    invocation.actor = "person:other".into();
    let mut storage = cell();
    let error = commit_action(
        &mut storage,
        &catalog,
        &invocation,
        &PrincipalOnly("person:other"),
    )
    .unwrap_err();
    assert!(
        matches!(error, norn_kernel::ActionRefusal::InvalidInput { ref field } if field == "actor"),
        "{error:?}"
    );
    assert_eq!(storage.ledger(), []);
}

proptest::proptest! {
    #[test]
    fn action_actor_is_identical_in_derivation_pdp_and_retained_receipt(principal in "person:[a-z0-9é\u{301}]{1,24}") {
        let mut action = action();
        action.derive = vec![norn_ir::ActionInputDerivation {field:"code".into(),expression:ZenExpression("actor.subjectId".into()),input_required:false}];
        let mut invocation = call(&action,JsonObject::new(),"1.0.0"); invocation.actor = principal.clone();
        let definitions = types(); let catalog = TypeCatalog::new(&definitions); let mut storage = cell();
        let outcome = commit_action(&mut storage,&catalog,&invocation,&PrincipalOnly(&principal));
        if norn_ir::canonical::nfc(&principal) != principal {
            proptest::prop_assert!(matches!(outcome,Err(norn_kernel::ActionRefusal::InvalidInput {..})), "identity outside the expression profile must refuse");
            proptest::prop_assert!(storage.ledger().is_empty());
            return Ok(());
        }
        let outcome = outcome.unwrap();
        let ActionOutcome::Committed {record,..} = outcome else { proptest::prop_assert!(false,"fresh call must commit"); return Ok(()); };
        proptest::prop_assert_eq!(&record.actor,&principal);
        let expected = norn_ir::RuntimeValue::String(principal.clone());
        proptest::prop_assert_eq!(record.derived_values.first().map(|d| &d.value),Some(&expected));
        let retained = storage.load(&FactKey::new("test.Target",principal.as_str())).unwrap();
        proptest::prop_assert_eq!(retained.values.as_ref().and_then(|v| v.get("code")).and_then(norn_ir::RuntimeValue::as_str),Some(principal.as_str()));
    }

    #[test]
    fn action_actor_and_approval_spoofs_refuse_at_every_input_depth(actor_field in proptest::bool::ANY, depth in 0usize..6, forgery in "[a-zA-Z0-9]{0,24}") {
        let action = action(); let field = if actor_field {"actor"} else {"approved"};
        let mut payload = json!({field:forgery});
        for _ in 0..depth { payload = json!({"nested":[payload]}); }
        let raw: JsonObject = serde_json::from_value(payload).unwrap();
        let error = prepare_action_input_for_call::<Infallible>(&action,&raw,"test.target.create.action","1.0.0","person:authenticated").unwrap_err();
        proptest::prop_assert!(matches!(error,norn_kernel::ActionRefusal::InvalidInput {field: ref rejected} if rejected == field), "caller authority field must refuse");
        let mut invocation = call(&action,raw,"1.0.0"); invocation.actor = "person:authenticated".into();
        let definitions = types(); let catalog = TypeCatalog::new(&definitions); let mut storage = cell();
        let error = commit_action(&mut storage,&catalog,&invocation,&Allow).unwrap_err();
        proptest::prop_assert!(matches!(error,norn_kernel::ActionRefusal::InvalidInput {field: ref rejected} if rejected == field), "governed call must refuse the spoof");
        proptest::prop_assert!(storage.ledger().is_empty());
        proptest::prop_assert!(!storage.load(&FactKey::new("test.Target","C26")).unwrap().exists());
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
fn described_decimal_action_input_prepares_exactly_and_rejects_unknown_constraints() {
    let mut action = action();
    action.derive.clear();
    let schema = json!({
        "type": "object", "additionalProperties": false,
        "properties": {"amount": {
            "type": "number", "nornType": {"kind": "decimal", "scale": 2},
            "description": "Exact decimal text"
        }},
        "required": ["amount"]
    });
    action.input_schema = serde_json::from_value(schema.clone()).unwrap();
    let raw: JsonObject = serde_json::from_value(json!({"amount": "9007199254740993.25"})).unwrap();
    let prepared = prepare_action_input::<Infallible>(&action, &raw).unwrap();
    assert_eq!(
        serde_json::to_string(prepared.effective_input()).unwrap(),
        r#"{"amount":9007199254740993.25}"#
    );
    assert_eq!(
        raw["amount"],
        norn_ir::RuntimeValue::String("9007199254740993.25".to_owned())
    );
    for (key, value) in [("maximum", json!(1)), ("description", json!(false))] {
        let mut invalid = schema.clone();
        invalid["properties"]["amount"][key] = value;
        action.input_schema = serde_json::from_value(invalid).unwrap();
        assert_eq!(
            prepare_action_input::<Infallible>(&action, &raw)
                .unwrap_err()
                .code(),
            "action.invalid_input"
        );
    }
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
    assert_eq!(record.derived_values, []);
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

#[test]
fn oversized_raw_input_is_refused_by_its_bound_before_normalization() {
    let action = action();
    let max = usize::try_from(
        norn_kernel::ResourceLimitProfile::norn_limits_v1()
            .slice
            .max_canonical_bytes,
    )
    .unwrap();
    // `extra` is not in the closed input schema, so normalizing this input
    // would refuse it as invalid input. Only the pre-normalization bound
    // check yields `input_bound_exceeded`.
    let raw: JsonObject =
        serde_json::from_value(json!({"code": "x".repeat(max + 1), "extra": 1})).unwrap();
    let refusal = prepare_action_input::<Infallible>(&action, &raw).unwrap_err();
    assert_eq!(refusal.code(), "action.input_bound_exceeded");
    let within: JsonObject = serde_json::from_value(json!({"code": "x", "extra": 1})).unwrap();
    assert_eq!(
        prepare_action_input::<Infallible>(&action, &within)
            .unwrap_err()
            .code(),
        "action.invalid_input"
    );
}
