//! Declaration lowering correspondence, not solver evidence.
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "bounded test fixtures"
)]
use super::quint::*;
use super::*;
use proptest::prelude::*;

fn fixture() -> Vec<Value> {
    let mut entities: Vec<Value> =
        serde_json::from_str(include_str!("../../../../fixtures/law-ir/entities.json"))
            .expect("generated entities");
    let mut laws: Vec<Value> =
        serde_json::from_str(include_str!("../../../../fixtures/law-ir/laws.json"))
            .expect("generated laws");
    let mut object: norn_ir::EntityRootV2 =
        serde_json::from_value(entities[0]["payload"].clone()).expect("entity");
    object.object_type.lifecycles.clear();
    object.object_type.properties = ["id", "amount", "remaining", "difference"]
        .into_iter()
        .map(|id| norn_ir::Property {
            id: norn_ir::PropertyId(id.into()),
            description: None,
            property_type: if id == "id" {
                norn_ir::PropertyType::Text {}
            } else {
                norn_ir::PropertyType::Decimal {
                    scale: norn_ir::WholeNumber(0),
                    rounding: None,
                    unit: None,
                    currency_source: None,
                    per_unit: None,
                }
            },
            constraints: norn_ir::PropertyConstraints::default(),
            default: if id == "difference" {
                Some(norn_ir::DefaultValue(
                    serde_json::from_value(json!("0")).expect("default"),
                ))
            } else {
                None
            },
            markings: vec![],
            computed: None,
        })
        .collect();
    entities[0]["payload"] = serde_json::to_value(object).expect("typed object");
    let mut law: norn_ir::LawIR =
        serde_json::from_value(laws.remove(0)["payload"].clone()).expect("law");
    law.modality = norn_ir::LawModality::Always {};
    law.scope.root = norn_ir::LawRoot::Entity("erpnext.SalesOrder".into());
    law.response = norn_ir::LawResponse::Predicate {
        expression: "self.amount == self.remaining + self.difference".into(),
    };
    let mut envelope = entities[0].clone();
    envelope["artifactId"] = json!("test.conservation.law");
    envelope["kind"] = json!("law");
    envelope["payload"] = serde_json::to_value(law).expect("typed law");
    let mut action: Value = serde_json::from_str(include_str!(
        "../../../../fixtures/ir/retail-reorder.action.json"
    ))
    .expect("generated action");
    action["scope"] = entities[0]["scope"].clone();
    action["dependencies"] = json!([]);
    action["payload"] = json!({"actionId":"test.create","scopeRequired":["orgId","environmentId"],"idempotent":true,"ledgered":true,
        "inputSchema":{"type":"object","additionalProperties":false,"properties":{"id":{"type":"string"},"amount":{"type":"number","nornType":{"kind":"decimal","scale":0}},"remaining":{"type":"number","nornType":{"kind":"decimal","scale":0}},"difference":{"type":"number","nornType":{"kind":"decimal","scale":0}}},"required":["id","amount","remaining"]},
        "outputSchema":{"type":"object","additionalProperties":false,"properties":{"id":{"type":"string"}},"required":["id"]},
        "derive":[{"field":"difference","inputRequired":false,"expression":"input.amount - input.remaining"}],
        "preconditions":["input.remaining <= input.amount"],"writes":[{"entityKind":"erpnext.SalesOrder","change":"created"}],
        "effectProgram":{"kind":"upsert_input_object","objectIdField":"id","objectKind":"erpnext.SalesOrder","entityArtifactId":"erpnext.SalesOrder.entity","entityArtifactVersion":"0.1.0","writeMode":{"kind":"create_only"}}});
    vec![entities.remove(0), envelope, action]
}
fn bounds(artifacts: &[Value]) -> Bounds {
    Bounds {
        action: artifacts[2]["artifactId"].as_str().expect("id").into(),
        inputs: BTreeMap::from([
            ("amount".into(), ("2".into(), "12".into())),
            ("remaining".into(), ("1".into(), "12".into())),
        ]),
        identity: "one".into(),
        seed: 0,
    }
}
fn source(artifacts: &[Value]) -> DefinitionSource {
    DefinitionSource::parse(&serde_json::to_string(artifacts).expect("source"), "[]")
        .expect("source")
}
#[test]
fn original_mutant_restore_and_visible_unknown_inventory() {
    let mut artifacts = fixture();
    let original = source(&artifacts);
    let registry = BTreeSet::new();
    let authority = ApprovedContract::approve(
        &original,
        &registry,
        json!({"expr":"norn-zen/6"}),
        json!({"store":"empty"}),
        json!({"level":"bounded"}),
    )
    .expect("admitted");
    let profile = bounds(&artifacts);
    let good = authority
        .quint(&original, &registry, &profile)
        .expect("lowered");
    let modules: Vec<_> = good.goals.iter().filter_map(|g| g.as_ref().ok()).collect();
    assert_eq!(modules.len(), 3);
    assert!(good.goals.iter().any(Result::is_err));
    assert!(modules[0].contains(" - "));
    artifacts[2]["payload"]["derive"] = json!([]);
    let bad = authority
        .quint(&source(&artifacts), &registry, &profile)
        .expect("mutant");
    assert_ne!(bad.challenge.sha256(), good.challenge.sha256());
    assert_ne!(bad.goals, good.goals);
    assert_eq!(
        authority
            .quint(&original, &registry, &profile)
            .expect("restore")
            .goals,
        good.goals
    );
    let mut outside = profile;
    outside
        .inputs
        .insert("unknown".into(), ("0".into(), "1".into()));
    assert!(
        authority
            .quint(&original, &registry, &outside)
            .expect("unknown bounds")
            .goals
            .iter()
            .all(Result::is_err)
    );
}
proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]
    #[test]
    fn exact_decimal_lowering_agrees_with_native_arithmetic(a in 0..100_i64, b in 0..100_i64) {
        let fields = BTreeMap::from([("amount".into(), "a".into()), ("remaining".into(), "b".into())]);
        let lowered = super::quint::expression("input.amount - input.remaining", &fields, 2).expect("lower");
        let native = norn_expr::Expression::compile("input.amount - input.remaining").expect("native").evaluate(&norn_expr::Value::object([("input", norn_expr::Value::object([("amount", a.into()), ("remaining", b.into())]))]), norn_expr::EvaluationBudget::new(100)).expect("native eval");
        let translated = norn_expr::Expression::compile(&lowered).expect("common grammar").evaluate(&norn_expr::Value::object([("a", a.into()), ("b", b.into())]), norn_expr::EvaluationBudget::new(100)).expect("lowered eval");
        prop_assert_eq!(native.value(), translated.value());
    }
}
