#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::pedantic,
    missing_docs,
    reason = "integration-test crate"
)]
#![deny(
    clippy::match_wildcard_for_single_variants,
    clippy::format_collect,
    clippy::format_push_string,
    clippy::inefficient_to_string,
    clippy::large_types_passed_by_value,
    clippy::needless_pass_by_value,
    clippy::large_futures
)]

//! Lineage queries cannot mix main and fork fact-version namespaces.

use norn_ir::{FactRef, LineageInput, ObjectTypeName};
use norn_projections::{LineageError, explain, explain_latest};

const FIXTURE: &str = include_str!("../../../fixtures/lineage/order-1001-on-hold.lineage.json");

fn input() -> LineageInput {
    serde_json::from_str(FIXTURE).expect("scoped lineage fixture decodes")
}

#[test]
fn lineage_rejects_foreign_world_collisions_and_newer_versions() {
    let fact = FactRef::new("commerce.SalesOrder", "1001", 2);
    for version in [2, 99] {
        let mut input = input();
        let mut fork = input.commits[0].clone();
        fork.world = "scenario".to_owned();
        fork.written[0].version = version;
        let expected = LineageError::WorldMismatch {
            expected: "main".to_owned(),
            actual: "scenario".to_owned(),
            commit: fork.stable_id(),
        };
        input.commits.push(fork);

        // Refuse both a same-version writer collision and a higher fork
        // version before either can influence a main-world answer.
        for _ in 0..2 {
            assert_eq!(explain(&input, &fact), Err(expected.clone()));
            assert_eq!(
                explain_latest(&input, &fact.object_type, &fact.key),
                Err(expected.clone())
            );
            input.commits.reverse();
        }
    }
}

#[test]
fn lineage_explicit_fork_queries_stay_in_the_selected_world() {
    let mut input = input();
    input.world = "scenario".to_owned();
    for commit in &mut input.commits {
        commit.world.clone_from(&input.world);
    }
    let chain = explain_latest(
        &input,
        &ObjectTypeName("commerce.SalesOrder".to_owned()),
        "1001",
    )
    .expect("an explicitly scoped fork is explainable");
    assert_eq!(chain.root, "fact:commerce.SalesOrder/1001@v2");
    assert!(chain.node("commit:scenario/3").is_some());
    assert!(chain.node("commit:scenario/2").is_some());
    assert!(chain.node("commit:main/3").is_none());
}

#[test]
fn lineage_requires_an_explicit_world_even_without_commits() {
    let mut input = input();
    input.world.clear();
    input.commits.clear();
    let fact = FactRef::new("commerce.SalesOrder", "1001", 2);
    assert_eq!(explain(&input, &fact), Err(LineageError::MissingWorld));
    assert_eq!(
        explain_latest(&input, &fact.object_type, &fact.key),
        Err(LineageError::MissingWorld)
    );

    let mut wire: serde_json::Value = serde_json::from_str(FIXTURE).expect("fixture is JSON");
    let object = wire.as_object_mut().expect("fixture is an object");
    object.remove("world");
    assert!(serde_json::from_value::<LineageInput>(wire).is_err());
}
