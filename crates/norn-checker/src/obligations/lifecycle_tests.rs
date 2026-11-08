//! Source-bound lifecycle inventory, not solver evidence.
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "bounded generated fixtures"
)]
use super::*;
use sha2::{Digest, Sha256};

fn constitution() -> DefinitionSource {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../../crates/norn-cell/tests/fixtures/constitution.json"
    ))
    .expect("constitution");
    let base: Vec<Value> = [
        include_str!("../../../../registry/core/artifacts/core.units.json"),
        include_str!("../../../../registry/core/artifacts/core.Process.interface.json"),
        include_str!("../../../../registry/core/artifacts/core.Conversion.interface.json"),
    ]
    .into_iter()
    .map(|text| serde_json::from_str(text).expect("base"))
    .collect();
    DefinitionSource::parse(
        &String::from_utf8(
            norn_canonical::canonical_json_exact_of(&fixture["withLoss"]).expect("source"),
        )
        .expect("UTF8"),
        &String::from_utf8(norn_canonical::canonical_json_exact_of(&base).expect("base"))
            .expect("UTF8"),
    )
    .expect("exact source")
}
pub(super) fn authority(source: &DefinitionSource) -> ApprovedContract {
    ApprovedContract::approve(
        source,
        &BTreeSet::new(),
        json!({"expr":"norn-zen/6"}),
        json!({"store":"empty"}),
        json!({"level":"bounded"}),
    )
    .expect("admitted")
}
#[test]
fn constitution_ten_goal_inventory_golden() {
    let source = constitution();
    let challenge = authority(&source)
        .challenge(&source, &BTreeSet::new())
        .expect("challenge");
    let goals = &challenge.body()["obligations"];
    assert_eq!(goals.as_array().expect("inventory").len(), 10);
    let bytes = norn_canonical::canonical_json_exact_of(goals).expect("exact goals");
    assert_eq!(
        format!("{:x}", Sha256::digest(&bytes)),
        "8540b972f75056de05b2183332455aa58464a5332ee76dca0ac394593bdc5b6b"
    );
    let canonical = norn_canonical::canonical_json_exact_of(challenge.body()).expect("challenge");
    let reparsed: Value = serde_json::from_slice(&canonical).expect("canonical roundtrip");
    assert_eq!(
        canonical,
        norn_canonical::canonical_json_exact_of(&reparsed).expect("same bytes")
    );
    assert_eq!(
        challenge.sha256(),
        "891aa8d83690605ec68c1fe54915d784db46db80af4e9b2f861f911a1accb8ff"
    );
}

fn chart(guard: &str) -> Value {
    json!({"irVersion":"0.1","artifactId":"flow.Item.entity","kind":"entity",
        "namespace":"flow","name":"Item","version":"0.1.0","lifecycle":"draft",
        "scope":{"orgId":"org_demo","environmentId":"dev"},"dependencies":[],
        "provenance":{"source":"authoring_sdk","sourcePath":"tests/lifecycle"},
        "payload":{"entityVersion":"2.0","objectType":{"name":"flow.Item",
            "properties":[{"id":"status","type":{"kind":"enum","values":[
                {"value":"draft"},{"value":"review"},{"value":"done"}]},"default":"draft"}],
            "lifecycles":[{"property":"status","initial":["draft"],"terminal":["done"],
                "transitions":[{"id":"submit","from":["draft"],"to":"review","guards":[guard]},
                    {"id":"finish","from":["review"],"to":"done"}]}]}}})
}
fn challenge(artifacts: &Value, base: &Value) -> Challenge {
    let source = DefinitionSource::parse(
        &serde_json::to_string(artifacts).expect("source"),
        &serde_json::to_string(base).expect("base"),
    )
    .expect("exact source");
    authority(&source)
        .challenge(&source, &BTreeSet::new())
        .expect("challenge")
}
#[test]
fn generated_lifecycle_reachability() {
    let original = challenge(&json!([chart("self.status == 'draft'")]), &json!([]));
    let goals = original.body()["obligations"].as_array().expect("goals");
    assert_eq!(goals.len(), 4);
    assert_eq!(
        goals[0]["goal"],
        json!({"kind":"lifecycle_terminal_reachability","terminal":"done"})
    );
    assert_eq!(goals[1]["goal"]["kind"], "lifecycle_no_stuck_non_terminal");
    assert_eq!(goals[2]["goal"]["transition"], "submit");
    assert_eq!(goals[3]["goal"]["transition"], "finish");
    for goal in goals {
        assert_eq!(goal["assessment"], "unknown");
        assert!(!goal["reason"].as_str().expect("reason").is_empty());
        assert_eq!(goal["statement"]["maxSteps"], 3);
        assert_eq!(goal["statement"]["maxObjects"], 1);
        assert_eq!(goal["statement"]["property"], "status");
    }
    // A structurally legal but disabled guard must not disappear from the denominator.
    let mutant = challenge(&json!([chart("false")]), &json!([]));
    assert_eq!(
        mutant.body()["obligations"]
            .as_array()
            .expect("goals")
            .len(),
        4
    );
    assert_ne!(
        goals[0]["statement"]["lifecycleDigest"],
        mutant.body()["obligations"][0]["statement"]["lifecycleDigest"]
    );
    assert_ne!(original.sha256(), mutant.sha256());
    assert_eq!(
        original.sha256(),
        challenge(&json!([chart("self.status == 'draft'")]), &json!([])).sha256()
    );
}
#[test]
fn jewelry_app_law_keeps_all_lifecycle_goals_unknown() {
    let artifacts: Value =
        serde_json::from_str(include_str!("../../tests/fixtures/jewelry_demo.json"))
            .expect("actual source golden");
    let units: Value = serde_json::from_str(include_str!(
        "../../../../registry/core/artifacts/core.units.json"
    ))
    .expect("units");
    let generated = challenge(&artifacts, &json!([units]));
    let goals = generated.body()["obligations"].as_array().expect("goals");
    let lifecycle: Vec<_> = goals
        .iter()
        .filter(|goal| goal["statement"]["rule"] == "norn.lifecycle.transition-only/1")
        .collect();
    assert_eq!(lifecycle.len(), 7); // two terminals + no stuck + four transitions
    assert!(lifecycle.iter().all(|g| g["assessment"] == "unknown"));
    for goal in &lifecycle {
        let reason = goal["reason"].as_str().expect("specific blocker");
        assert!(reason.contains("UnsupportedUntilB2"));
        assert!(
            reason.contains(
                artifacts[1]["artifactId"]
                    .as_str()
                    .expect("source law identity")
            )
        );
        assert!(reason.contains("App(\"demo.jewelry\")"));
    }
    assert_eq!(artifacts[1]["payload"]["scope"]["root"]["kind"], "app");
    let first = goals.len() - lifecycle.len();
    assert!(goals[..first].iter().all(|goal| goal.get("law").is_some()));
    assert!(goals[first..].iter().all(|goal| goal.get("law").is_none()));
}
#[test]
fn lifecycle_goal_cap_is_checked_before_append() {
    let artifact: ArtifactEnvelope = serde_json::from_value(chart("true")).expect("entity");
    let pool = [artifact];
    let catalog = crate::EffectiveCatalog::from_artifacts(&pool);
    let mut goals = vec![Value::Null; 8_192];
    assert_eq!(
        super::lifecycle::append(&catalog, &pool, &mut goals),
        Err(ChallengeError::Limit)
    );
    assert_eq!(goals.len(), 8_192);
}
proptest::proptest! {
    #[test]
    fn lifecycle_exact_guard_identity(spaces in 1_usize..16) {
        let left=challenge(&json!([chart("true")]),&json!([]));
        let right=challenge(&json!([chart(&format!("true{}", " ".repeat(spaces)))]),&json!([]));
        proptest::prop_assert_ne!(left.body()["obligations"][0]["statement"]["lifecycleDigest"].clone(),
            right.body()["obligations"][0]["statement"]["lifecycleDigest"].clone());
    }
}
