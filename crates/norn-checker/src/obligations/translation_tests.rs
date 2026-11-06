//! Declaration lowering correspondence; solver answers come only from the ignored test.
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::unwrap_in_result,
    clippy::disallowed_methods,
    reason = "bounded test fixtures; the ignored test runs the pinned solver"
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

fn lowered(artifacts: &[Value]) -> Result<Queries, String> {
    let source = DefinitionSource::parse(&serde_json::to_string(artifacts).expect("json"), "[]")
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
    let batch = authority.translation(&source, &registry).expect("lowered");
    assert_eq!(batch.goals.len(), 1);
    assert_eq!(batch.goals[0].0, "test.Payable.law");
    batch.goals.into_iter().next().expect("goal").1
}

fn assign(artifacts: &mut [Value], property: &str, expression: &str) {
    artifacts[2]["payload"]["response"]["assignments"][property] = json!(expression);
}

#[test]
fn generated_translation_smt_states_the_central_obligation() {
    let queries = lowered(&fixture()).expect("in fragment");
    let net = "(+ 0 0 (* 1 s1))";
    let gross = "(+ 0 0 (* 1 s1) (* 1 s3))";
    for premise in [
        "(set-logic QF_UFLIA)",
        "(declare-const s0 Id)",
        "(declare-const s1 Int)",
        "(assert (>= (+ 0 0 (* 1 s3)) (+ 0 0 )))",
        &format!("(assert (>= {net} (+ 0 0 )))"),
    ] {
        assert!(
            queries.non_vacuity.contains(premise),
            "{premise}: {queries:?}"
        );
    }
    let conclusion = queries
        .preservation
        .strip_prefix(&queries.non_vacuity)
        .expect("one added assertion");
    let expected = format!("\n(assert (not (and true (>= {gross} (+ 0 0 )) (= {gross} {gross}))))");
    assert_eq!(conclusion, expected);
}

#[test]
fn generated_translation_smt_changes_with_the_translation() {
    let original = lowered(&fixture()).expect("original");
    let mut mutant = fixture();
    assign(&mut mutant, "gross", "self.net");
    assert_ne!(original, lowered(&mutant).expect("mutant"));
    assign(&mut mutant, "gross", "self.net + self.tax");
    assert_eq!(lowered(&mutant).expect("restored"), original);
}

#[test]
fn generated_translation_smt_keeps_unsupported_goals_visible() {
    for (property, expression) in [
        ("gross", "self.net * 2"),
        ("gross", "self.id"),
        ("ref", "self.net"),
    ] {
        let mut artifacts = fixture();
        assign(&mut artifacts, property, expression);
        assert!(lowered(&artifacts).is_err(), "{expression}");
    }
    let mut scale = fixture();
    scale[1]["payload"]["objectType"]["properties"][1]["type"]["scale"] = json!(3);
    let mut kind = fixture();
    kind[0]["payload"]["objectType"]["properties"][1] =
        json!({"id":"payer","type":{"kind":"boolean"}});
    let mut boolean = fixture();
    boolean[2]["payload"]["condition"] = json!("self.net >= 0 or self.tax >= 1");
    let mut mixed = fixture();
    mixed[2]["payload"]["response"]["observation"]["source"] = json!("self.id");
    for artifacts in [scale, kind, boolean, mixed] {
        assert!(
            lowered(&artifacts).is_err(),
            "{:?}",
            artifacts[2]["payload"]
        );
    }
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

fn solve(z3: &str, query: &str) -> Answer {
    let path = std::env::temp_dir().join(format!("norn-translation-{}.smt2", std::process::id()));
    std::fs::write(&path, format!("{query}\n(check-sat)\n")).expect("query");
    let output = std::process::Command::new(z3)
        .arg(&path)
        .output()
        .expect("z3");
    std::fs::remove_file(&path).expect("cleanup");
    match String::from_utf8_lossy(&output.stdout).trim() {
        "sat" => Answer::Sat(String::new()),
        "unsat" => Answer::Unsat,
        other => Answer::Unknown(other.to_owned()),
    }
}

/// `NORN_Z3=<pinned z3> cargo test -p norn-checker generated_translation_smt -- --ignored`
#[test]
#[ignore = "needs the pinned z3 from the verification toolchain"]
fn generated_translation_smt_solves_with_z3() {
    let z3 = std::env::var("NORN_Z3").expect("NORN_Z3");
    let status = |artifacts: &[Value]| {
        let queries = lowered(artifacts).expect("in fragment");
        classify(
            solve(&z3, &queries.preservation),
            solve(&z3, &queries.non_vacuity),
        )
    };
    let proved = |artifacts: &[Value]| {
        matches!(
            status(artifacts),
            TranslationStatus::ProvedUnderAssumptions { .. }
        )
    };
    let refuted =
        |artifacts: &[Value]| matches!(status(artifacts), TranslationStatus::Refuted { .. });
    assert!(proved(&fixture()));
    let mut dropped = fixture();
    assign(&mut dropped, "gross", "self.net");
    assert!(refuted(&dropped), "the observation changes");
    let mut unadmitted = fixture();
    unadmitted[2]["payload"]
        .as_object_mut()
        .expect("law")
        .remove("condition");
    assert!(refuted(&unadmitted), "target validity needs admission");
    let mut identity = fixture();
    identity[2]["payload"]["response"]["observation"] =
        json!({"id":"party","source":"self.id","target":"self.ref"});
    assert!(proved(&identity));
    assign(&mut identity, "ref", "self.payer");
    assert!(refuted(&identity), "identities are not interchangeable");
    let mut vacuous = fixture();
    vacuous[2]["payload"]["condition"] = json!("self.net >= 1 and self.net < 1");
    assert!(matches!(
        status(&vacuous),
        TranslationStatus::Unsupported { .. }
    ));
}
