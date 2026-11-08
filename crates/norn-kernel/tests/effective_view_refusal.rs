#![allow(
    clippy::expect_used,
    clippy::panic,
    missing_docs,
    reason = "integration-test assertions"
)]

//! A catalog whose effective view is refused never materializes values or
//! resolves types from an empty view.

use norn_ir::{ObjectTypeDef, ObjectTypeName};
use norn_kernel::{EffectiveValuesError, TypeCatalog};

fn types(count: usize) -> Vec<ObjectTypeDef> {
    (0..count)
        .map(|index| {
            serde_json::from_value(serde_json::json!({ "name": format!("budget.T{index:04}") }))
                .expect("object type")
        })
        .collect()
}

#[test]
fn a_refused_catalog_errors_instead_of_passing_values_through() {
    let objects = types(4_097);
    let catalog = TypeCatalog::new(&objects);
    let refusal = catalog
        .effective_refusal()
        .expect("4,097 types exceed the classification budget");
    assert_eq!(
        (refusal.dimension, refusal.limit),
        ("registered types", 4_096)
    );
    let error = catalog
        .effective_object_values(
            &ObjectTypeName("budget.T0000".to_owned()),
            &norn_ir::RuntimeObject::new(),
        )
        .expect_err("no effective view means no materialized values");
    assert!(matches!(error, EffectiveValuesError::ViewRefused(_)));
    assert!(
        catalog
            .effective_link_values(
                &ObjectTypeName("budget.T0000".to_owned()),
                "any",
                &norn_ir::RuntimeObject::new()
            )
            .is_err()
    );
}

#[test]
fn a_catalog_within_budget_has_an_effective_view() {
    let objects = types(4_096);
    let catalog = TypeCatalog::new(&objects);
    assert!(catalog.effective_refusal().is_none());
    assert!(
        catalog
            .effective_object_values(
                &ObjectTypeName("budget.T0000".to_owned()),
                &norn_ir::RuntimeObject::new()
            )
            .is_ok()
    );
}
