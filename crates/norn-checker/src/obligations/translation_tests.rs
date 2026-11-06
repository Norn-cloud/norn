//! Admission, goal inventory and status; no solver runs here.
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "bounded test fixtures"
)]
use super::translation::*;
use super::*;
use norn_ir::verification::translation::TranslationStatus;

fn entity(name: &str, properties: &Value, invariant: &str) -> Value {
    json!({"irVersion":"0.1","artifactId":format!("{name}.entity"),"kind":"entity",
        "namespace":"test","name":"entity","version":"0.1.0","lifecycle":"draft",
        "scope":{"orgId":"org_demo","environmentId":"dev"},"dependencies":[],
        "provenance":{"source":"authoring_sdk"},
        "payload":{"entityVersion":"2.0","objectType":{"name":name,"properties":properties,
            "invariants":[{"id":"floor","expression":invariant,"message":"refused"}]}}})
}

fn text(id: &str) -> Value {
    json!({"id":id,"type":{"kind":"text"}})
}

fn decimal(id: &str) -> Value {
    json!({"id":id,"type":{"kind":"decimal","scale":2}})
}

fn fixture() -> Vec<Value> {
    let source = entity(
        "test.Source",
        &json!([text("id"), text("payer"), decimal("net"), decimal("tax")]),
        "self.tax >= 0",
    );
    let target = entity(
        "test.Target",
        &json!([text("ref"), decimal("gross")]),
        "self.gross >= 0",
    );
    let pin = |id: &str| json!({"artifactId":id,"kind":"entity","required":true,"version":"0.1.0"});
    let law = json!({"irVersion":"0.1","artifactId":"test.Payable.law","kind":"law",
        "namespace":"test","name":"law","version":"0.1.0","lifecycle":"draft",
        "scope":{"orgId":"org_demo","environmentId":"dev"},
        "dependencies":[pin("test.Source.entity"),pin("test.Target.entity")],
        "provenance":{"source":"authoring_sdk"},
        "payload":{"lawVersion":"0.1","id":"test.payable","owner":"test.controls","severity":"error",
            "rationale":"Translation keeps the payable amount.","provenance":{"source":"authoring_sdk"},
            "scope":{"root":{"kind":"app","id":"test"},"objectTypes":["test.Source"]},
            "subject":"test.Source","condition":"self.net >= 0","modality":{"kind":"always"},
            "response":{"kind":"translation_preserves","targetType":"test.Target",
                "assignments":{"ref":"self.id","gross":"self.net + self.tax"},
                "observation":{"id":"payable","source":"self.net + self.tax","target":"self.gross"}}}});
    vec![source, target, law]
}

#[test]
fn challenge_lists_one_unknown_translation_goal() {
    let source = DefinitionSource::parse(&serde_json::to_string(&fixture()).expect("json"), "[]")
        .expect("source");
    let registry = BTreeSet::new();
    let authority = ApprovedContract::approve(
        &source,
        &registry,
        json!({"expr":"norn-zen/6"}),
        json!({"store":"empty"}),
        json!({"level":"smt"}),
    )
    .expect("admitted");
    let challenge = authority.challenge(&source, &registry).expect("challenge");
    let goals: Vec<_> = challenge.body()["obligations"]
        .as_array()
        .expect("obligations")
        .iter()
        .filter(|goal| goal["goal"]["kind"] == "translation_preserves")
        .collect();
    assert_eq!(goals.len(), 1);
    assert_eq!(goals[0]["goal"]["observation"], "payable");
    assert_eq!(goals[0]["assessment"], "unknown");
}

#[test]
fn translation_admission_requires_a_total_map_to_a_declared_target() {
    let check = |artifacts: &[Value]| {
        let typed: Vec<norn_ir::ArtifactEnvelope> = artifacts
            .iter()
            .map(|a| serde_json::from_value(a.clone()).expect("typed artifact"))
            .collect();
        crate::check_artifact_set(
            &typed,
            &crate::EntityArtifactSetContext {
                registry: &BTreeSet::new(),
                base: &[],
                tenant: None,
            },
        )
    };
    assert_eq!(check(&fixture()), vec![]);
    let mut partial = fixture();
    partial[2]["payload"]["response"]["assignments"]
        .as_object_mut()
        .expect("assignments")
        .remove("ref");
    let mut unknown = fixture();
    unknown[2]["payload"]["response"]["targetType"] = json!("test.Absent");
    let mut interface = fixture();
    let mut shape = entity("test.Shape", &json!([]), "true");
    shape["artifactId"] = json!("test.Shape.entity");
    shape["payload"] = json!({"entityVersion":"2.0","interface":{"name":"test.Shape",
        "properties":[text("ref"),decimal("gross")]}});
    interface.push(shape);
    assert_eq!(
        check(&interface),
        vec![],
        "the interface itself is admitted"
    );
    interface[2]["payload"]["response"]["targetType"] = json!("test.Shape");
    let mut eventual = fixture();
    eventual[2]["payload"]["modality"] = json!({"kind":"eventually"});
    for (artifacts, code) in [
        (partial, "law.model.unknown_binding"),
        (unknown, "law.model.unknown_binding"),
        (interface, "law.model.unknown_binding"),
        (eventual, "law.validation.unsupported_temporal"),
    ] {
        let diagnostics = check(&artifacts);
        assert!(
            diagnostics.iter().any(|d| d.code.as_str() == code),
            "{diagnostics:?}"
        );
    }
}

#[test]
fn only_a_witnessed_unsat_is_a_proof() {
    let sat = || Answer::Sat("model".into());
    let unknown = || Answer::Unknown("timeout".into());
    assert!(matches!(
        classify(Answer::Unsat, sat()),
        TranslationStatus::ProvedUnderAssumptions { .. }
    ));
    assert!(matches!(
        classify(sat(), Answer::Unsat),
        TranslationStatus::Refuted { .. }
    ));
    for (preservation, witness) in [
        (Answer::Unsat, Answer::Unsat),
        (Answer::Unsat, unknown()),
        (unknown(), sat()),
    ] {
        assert!(matches!(
            classify(preservation, witness),
            TranslationStatus::Unsupported { .. }
        ));
    }
}
