//! Declaration lowering correspondence, not solver evidence.
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "bounded test fixtures"
)]
use super::quint::*;
use super::*;
use proptest::prelude::*;

pub(super) fn fixture() -> Vec<Value> {
    let mut entities: Vec<Value> =
        serde_json::from_str(include_str!("../../../../fixtures/law-ir/entities.json"))
            .expect("generated entities");
    let laws: Vec<Value> =
        serde_json::from_str(include_str!("../../../../fixtures/law-ir/laws.json"))
            .expect("generated laws");
    let mut object: norn_ir::EntityRootV2 =
        serde_json::from_value(entities[0]["payload"].clone()).expect("entity");
    object.object_type.lifecycles.clear();
    object.object_type.properties = ["id", "amount", "remaining", "difference"]
        .into_iter()
        .map(|id| norn_ir::Property {
            protected: false,
            id: norn_ir::PropertyId(id.into()),
            description: None,
            property_type: if id == "id" {
                norn_ir::PropertyType::Text { format: None }
            } else {
                norn_ir::PropertyType::Decimal {
                    scale: norn_ir::WholeNumber(0),
                    additivity: norn_ir::PropertyAdditivity::default(),
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
    let mut envelope = laws.first().cloned().expect("generated law envelope");
    let mut law: norn_ir::LawIR = serde_json::from_value(envelope["payload"].clone()).expect("law");
    law.modality = norn_ir::LawModality::Always {};
    law.scope.root = norn_ir::LawRoot::Entity("erpnext.SalesOrder".into());
    law.response = norn_ir::LawResponse::Predicate {
        expression: norn_ir::ZenExpression(
            "self.amount == self.remaining + self.difference".into(),
        ),
    };
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
pub(super) fn bounds(artifacts: &[Value]) -> Bounds {
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
pub(super) fn source(artifacts: &[Value]) -> DefinitionSource {
    DefinitionSource::parse(&serde_json::to_string(artifacts).expect("source"), "[]")
        .expect("source")
}
#[test]
fn original_mutant_restore_and_visible_unknown_inventory() {
    let mut artifacts = fixture();
    let typed: Vec<norn_ir::ArtifactEnvelope> = artifacts
        .iter()
        .map(|a| serde_json::from_value(a.clone()).expect("typed artifact"))
        .collect();
    let diagnostics = crate::check_artifact_set(
        &typed,
        &crate::EntityArtifactSetContext {
            registry: &BTreeSet::new(),
            base: &[],
            tenant: None,
        },
    );
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
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
    assert_eq!(modules.len(), 3, "{:?}", good.goals);
    assert_eq!(good.goals.len(), 10);
    assert_eq!(good.goals.iter().filter(|goal| goal.is_err()).count(), 7);
    assert!(modules[0].contains(" - "));
    artifacts[2]["payload"]["derive"] = json!([]);
    let bad = authority
        .quint(&source(&artifacts), &registry, &profile)
        .expect("mutant");
    assert_ne!(bad.challenge.sha256(), good.challenge.sha256());
    assert_ne!(bad.goals, good.goals);
    if let Some(directory) = std::env::var_os("NORN_GENERATED_QUINT_DIR") {
        let directory = std::path::PathBuf::from(directory);
        std::fs::create_dir_all(&directory).expect("generated model directory");
        for (name, batch) in [("original.qnt", &good), ("mutant.qnt", &bad)] {
            std::fs::write(
                directory.join(name),
                batch
                    .goals
                    .iter()
                    .find_map(|goal| goal.as_ref().ok())
                    .expect("supported model"),
            )
            .expect("generated model");
        }
    }
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
        let expr = "input.amount - input.remaining + 0.250";
        let lowered = super::finite::expression(expr, &fields, 2).expect("lower");
        let native = norn_expr::Expression::compile(expr).expect("native").evaluate(&norn_expr::Value::object([("input", norn_expr::Value::object([("amount", norn_expr::Value::Number(format!("0.{a:02}").parse().expect("decimal"))), ("remaining", norn_expr::Value::Number(format!("0.{b:02}").parse().expect("decimal")))]))]), norn_expr::EvaluationBudget::new(100)).expect("native eval");
        let translated = norn_expr::Expression::compile(&lowered).expect("common grammar").evaluate(&norn_expr::Value::object([("a", a.into()), ("b", b.into())]), norn_expr::EvaluationBudget::new(100)).expect("lowered eval");
        let scaled_native = norn_expr::Expression::compile("value * 100").expect("scale oracle").evaluate(&norn_expr::Value::object([("value", native.value().clone())]), norn_expr::EvaluationBudget::new(100)).expect("oracle");
        prop_assert_eq!(scaled_native.value(), translated.value());
    }
}

#[test]
fn unsupported_syntax_and_inexact_literals_are_visible_refusals() {
    let fields = BTreeMap::from([("amount".into(), "a".into())]);
    for source in [
        "input.amount * 2",
        "input.amount / 2",
        "input.amount + 0.001",
        "actor.id",
        "sum(input)",
        "input.amounts",
        "input.amount[0]",
    ] {
        assert!(
            super::finite::expression(source, &fields, 2).is_err(),
            "{source}"
        );
    }
}

#[test]
fn finite_relation_is_backend_neutral() {
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
    let neutral = authority
        .finite(&original, &registry, &profile)
        .expect("finite relation");
    let quint = authority
        .quint(&original, &registry, &profile)
        .expect("projection");
    assert_eq!(neutral.challenge.sha256(), quint.challenge.sha256());
    assert_eq!(neutral.bounds, quint.bounds);
    assert_eq!(neutral.goals.len(), quint.goals.len());
    for (relation, module) in neutral.goals.iter().zip(&quint.goals) {
        assert_eq!(relation.is_ok(), module.is_ok());
        if let (Err(left), Err(right)) = (relation, module) {
            assert_eq!(left, right);
        }
    }
    // Check-time golden hashes from the pre-refactor 4ee0b129 renderer on Boat.
    let module = quint
        .goals
        .iter()
        .find_map(|goal| goal.as_ref().ok())
        .expect("original module");
    assert_eq!(
        format!("{:x}", Sha256::digest(module.as_bytes())),
        "4ff478be0ab3a6f1cae1f144f0c748061d16c96c8a9c37cf287fcbff285e23df"
    );
    let relation = neutral
        .goals
        .iter()
        .find_map(|goal| goal.as_ref().ok())
        .expect("eligible");
    assert_eq!(relation.writer_artifact, profile.action);
    assert_eq!(relation.identity, profile.identity);
    assert_eq!(
        relation
            .state
            .iter()
            .map(|field| field.property.as_str())
            .collect::<Vec<_>>(),
        ["amount", "difference", "remaining"]
    );
    assert_eq!(
        relation
            .inputs
            .iter()
            .map(|input| (input.property.as_str(), input.lower, input.upper))
            .collect::<Vec<_>>(),
        [("amount", 2, 12), ("remaining", 1, 12)]
    );
    let difference = relation
        .state
        .iter()
        .find(|field| field.property.as_str() == "difference")
        .expect("declaration");
    let update = |model: &norn_ir::verification::finite::CreateRelation| {
        model
            .updates
            .iter()
            .find(|(symbol, _)| symbol == &difference.symbol)
            .expect("write")
            .1
            .as_str()
            .to_owned()
    };
    assert!(update(relation).contains(" - "));
    artifacts[2]["payload"]["derive"] = json!([]);
    let module = authority
        .quint(&source(&artifacts), &registry, &profile)
        .expect("mutant projection")
        .goals
        .into_iter()
        .find_map(Result::ok)
        .expect("mutant module");
    assert_eq!(
        format!("{:x}", Sha256::digest(module.as_bytes())),
        "c9bf44daf2c25703b36a77da1c28029c75d705e3bacc3c15797ce2b5cd4ad0d3"
    );
    let mutant = authority
        .finite(&source(&artifacts), &registry, &profile)
        .expect("mutant");
    let mutant = mutant
        .goals
        .iter()
        .find_map(|goal| goal.as_ref().ok())
        .expect("mutant relation");
    assert_eq!(update(mutant), "0");
    assert_eq!(relation.inputs, mutant.inputs);
    assert_eq!(relation.state, mutant.state);
}

fn evaluate_law(text: &str, context: &norn_expr::Value) -> norn_expr::Value {
    norn_expr::Expression::compile(text)
        .expect("native expression")
        .evaluate(context, norn_expr::EvaluationBudget::new(1000))
        .expect("native evaluation")
        .value()
        .clone()
}

fn law_failures(
    artifacts: &[Value],
    batch: &super::finite::Batch,
    mutated: bool,
) -> BTreeSet<String> {
    let mut failures = BTreeSet::new();
    for (index, relation) in batch.goals.iter().enumerate() {
        let Ok(relation) = relation else { continue };
        let law_index = batch.challenge.body()["obligations"][index]["law"]
            .as_u64()
            .expect("law index");
        assert_eq!(
            batch.challenge.body()["lawStatements"][usize::try_from(law_index).expect("index")]["artifactId"],
            relation.law_artifact
        );
        let law = artifacts
            .iter()
            .find(|a| a["artifactId"] == relation.law_artifact)
            .expect("governing law");
        for amount in 2_i64..=12 {
            for remaining in 1_i64..=amount {
                let input = norn_expr::Value::object([(
                    "input",
                    norn_expr::Value::object([
                        ("amount", amount.into()),
                        ("remaining", remaining.into()),
                    ]),
                )]);
                let difference = if mutated {
                    0.into()
                } else {
                    evaluate_law(
                        artifacts[2]["payload"]["derive"][0]["expression"]
                            .as_str()
                            .expect("derivation"),
                        &input,
                    )
                };
                let native = norn_expr::Value::object([(
                    "self",
                    norn_expr::Value::object([
                        ("amount", amount.into()),
                        ("remaining", remaining.into()),
                        ("difference", difference),
                    ]),
                )]);
                let expected = evaluate_law(
                    law["payload"]["response"]["expression"]
                        .as_str()
                        .expect("predicate"),
                    &native,
                );
                let choices = norn_expr::Value::object(relation.inputs.iter().map(|field| {
                    (
                        field.symbol.clone(),
                        if field.property.as_str() == "amount" {
                            amount.into()
                        } else {
                            remaining.into()
                        },
                    )
                }));
                let post = norn_expr::Value::object(relation.updates.iter().map(
                    |(symbol, expression)| {
                        (symbol.clone(), evaluate_law(expression.as_str(), &choices))
                    },
                ));
                let actual = evaluate_law(relation.response.as_str(), &post);
                assert_eq!(
                    actual, expected,
                    "{} at {amount}/{remaining}",
                    relation.law_artifact
                );
                if actual == norn_expr::Value::Bool(false) {
                    failures.insert(relation.law_artifact.clone());
                }
            }
        }
    }
    failures
}

fn two_laws() -> Vec<Value> {
    let mut artifacts = fixture();
    let mut second = artifacts[1].clone();
    second["artifactId"] = json!("erpnext_nonnegative_difference_law");
    second["payload"]["id"] = json!("erpnext.nonnegative_difference");
    second["payload"]["response"]["expression"] = json!("self.difference >= 0");
    artifacts.push(second);
    artifacts
}

#[test]
fn multiple_governing_laws_keep_distinct_goals_and_mutant_semantics() {
    let artifacts = two_laws();
    let registry = BTreeSet::new();
    let original = source(&artifacts);
    let authority = ApprovedContract::approve(
        &original,
        &registry,
        json!({"expr":"norn-zen/6"}),
        json!({"store":"empty"}),
        json!({"level":"bounded"}),
    )
    .expect("both laws admitted");
    let profile = bounds(&artifacts);
    let good = authority
        .finite(&original, &registry, &profile)
        .expect("original");
    assert_eq!(good.goals.len(), 20);
    assert_eq!(good.goals.iter().filter(|goal| goal.is_ok()).count(), 6);
    let rendered = authority
        .quint(&original, &registry, &profile)
        .expect("Quint projection");
    assert_eq!(rendered.goals.len(), good.goals.len());
    for (relation, model) in good.goals.iter().zip(&rendered.goals) {
        assert_eq!(relation.is_ok(), model.is_ok());
        if let (Ok(relation), Ok(model)) = (relation, model) {
            assert!(model.contains(relation.response.as_str()));
        }
    }
    let mut changed = artifacts.clone();
    changed[2]["payload"]["derive"] = json!([]);
    let mutant = authority
        .finite(&source(&changed), &registry, &profile)
        .expect("mutant");
    assert_ne!(good.challenge.sha256(), mutant.challenge.sha256());
    let restored = authority
        .finite(&original, &registry, &profile)
        .expect("restored");
    assert_eq!(good.challenge.sha256(), restored.challenge.sha256());
    assert_eq!(good.goals, restored.goals);
    for (batch, mutated) in [(&good, false), (&mutant, true)] {
        let failures = law_failures(&artifacts, batch, mutated);
        assert_eq!(
            failures,
            if mutated {
                BTreeSet::from([artifacts[1]["artifactId"].as_str().expect("law").to_owned()])
            } else {
                BTreeSet::new()
            }
        );
    }
}

#[test]
fn an_unsupported_governing_law_does_not_suppress_supported_laws() {
    let mut artifacts = two_laws();
    artifacts[3]["payload"]["response"]["expression"] = json!("self.amount * 1 >= 0");
    let registry = BTreeSet::new();
    let source = source(&artifacts);
    let authority = ApprovedContract::approve(
        &source,
        &registry,
        json!({"expr":"norn-zen/6"}),
        json!({"store":"empty"}),
        json!({"level":"bounded"}),
    )
    .expect("admitted native multiplication");
    let batch = authority
        .finite(&source, &registry, &bounds(&artifacts))
        .expect("complete inventory");
    assert_eq!(batch.goals.len(), 20);
    assert_eq!(batch.goals.iter().filter(|goal| goal.is_ok()).count(), 3);
    for (index, goal) in batch.goals.iter().enumerate() {
        let law = batch.challenge.body()["obligations"][index]["law"]
            .as_u64()
            .expect("law");
        let identity = &batch.challenge.body()["lawStatements"]
            [usize::try_from(law).expect("index")]["artifactId"];
        if identity == "erpnext_nonnegative_difference_law" {
            assert_eq!(
                goal.as_ref().expect_err("opaque law remains Unknown"),
                "unsupported expression syntax"
            );
        }
    }
}
