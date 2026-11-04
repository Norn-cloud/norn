//! Source-bound lifecycle inventory, not solver evidence.
#![allow(clippy::expect_used, clippy::indexing_slicing, reason = "bounded generated fixtures")]
use super::*;
use sha2::{Digest, Sha256};

fn constitution() -> DefinitionSource {
    let fixture: Value = serde_json::from_str(include_str!("../../../../crates/norn-cell/tests/fixtures/constitution.json")).expect("constitution");
    let base: Vec<Value> = [
        include_str!("../../../../registry/core/artifacts/core.units.json"),
        include_str!("../../../../registry/core/artifacts/core.Process.interface.json"),
        include_str!("../../../../registry/core/artifacts/core.Conversion.interface.json"),
    ].into_iter().map(|text| serde_json::from_str(text).expect("base")).collect();
    DefinitionSource::parse(&serde_json::to_string(&fixture["withLoss"]).expect("source"), &serde_json::to_string(&base).expect("base")).expect("exact source")
}
fn authority(source: &DefinitionSource) -> ApprovedContract {
    ApprovedContract::approve(source, &BTreeSet::new(), json!({"expr":"norn-zen/6"}), json!({"store":"empty"}), json!({"level":"bounded"})).expect("admitted")
}
#[test]
fn constitution_ten_goal_inventory_golden() {
    let source = constitution();
    let challenge = authority(&source).challenge(&source, &BTreeSet::new()).expect("challenge");
    let goals = &challenge.body()["obligations"];
    assert_eq!(goals.as_array().expect("inventory").len(), 10);
    let bytes = norn_canonical::serialize_json_verbatim(goals).expect("exact goals");
    eprintln!("LIFECYCLE_CONST_INVENTORY_SHA {:x}", Sha256::digest(&bytes));
    eprintln!("LIFECYCLE_CONST_CHALLENGE_SHA {}", challenge.sha256());
}
