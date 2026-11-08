//! Diagnostic parity, registration boundaries, structural labels and refusal.
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "bounded integration fixtures"
)]

use std::collections::BTreeSet;

use norn_checker::{
    Diagnostic, DiagnosticRemediation, DiagnosticSeverity, EntityContext, EntityModel, Span,
    check_entity_model, effective_object_types, object_types::codes,
};
use norn_ir::{ObjectTypeDef, PropertyTypeName};

fn types(rows: &[(&str, Option<&str>)]) -> Vec<ObjectTypeDef> {
    rows.iter()
        .map(|(name, parent)| {
            let mut value = serde_json::json!({ "name": name });
            if let Some(parent) = parent {
                value["extends"] = serde_json::json!(parent);
            }
            serde_json::from_value(value).expect("typed fixture")
        })
        .collect()
}

fn findings(local: &[ObjectTypeDef], base: &[ObjectTypeDef]) -> Vec<Diagnostic> {
    let registry = BTreeSet::<PropertyTypeName>::new();
    check_entity_model(
        &EntityModel {
            object_types: local,
            ..EntityModel::EMPTY
        },
        &EntityContext {
            catalog: &[],
            registry: &registry,
            base: EntityModel {
                object_types: base,
                ..EntityModel::EMPTY
            },
            tenant: None,
        },
    )
}

fn cycle(index: usize, name: &str) -> Diagnostic {
    Diagnostic {
        code: norn_checker::DiagnosticCode::new(codes::INHERITANCE_CYCLE),
        severity: DiagnosticSeverity::Error,
        message: format!("the extends chain of {name:?} returns to it"),
        span: Some(Span::pointer(format!("/objectTypes/{index}/extends"))),
        artifact_id: None,
        remediation: Some(DiagnosticRemediation::FixDefinition),
    }
}

#[test]
fn cycles_keep_exact_diagnostic_code_span_severity_message_and_local_order() {
    let local = types(&[
        ("test.Z", Some("test.Y")),
        ("test.Y", Some("test.Z")),
        ("test.Descendant", Some("test.Y")),
        ("test.Self", Some("test.Self")),
        ("test.Clear", None),
    ]);
    let actual = findings(&local, &[]);
    assert_eq!(
        actual,
        [
            cycle(0, "test.Z"),
            cycle(1, "test.Y"),
            cycle(3, "test.Self")
        ]
    );
    let common_json = norn_checker::diagnostics_to_json(&actual);
    assert_eq!(
        common_json["diagnostics"][0]["evidence"]["kind"],
        "structural"
    );
    for diagnostic in &actual {
        let evidence = diagnostic.structural_evidence().expect("structural label");
        assert_eq!(evidence.basis, "complete_registered_catalog");
        let json = diagnostic.to_json();
        assert_eq!(json["evidence"]["kind"], "structural");
        assert_eq!(json["evidence"]["ruleId"], evidence.rule_id);
        assert!(json["evidence"].get("minimalDerivationHeight").is_none());
    }
}

#[test]
fn unknown_parent_and_acyclic_catalog_keep_existing_behavior() {
    let local = types(&[("test.A", Some("test.Missing")), ("test.B", Some("test.A"))]);
    let actual = findings(&local, &[]);
    assert_eq!(actual.len(), 1);
    assert_eq!(actual[0].code.as_str(), codes::UNKNOWN_SUPERTYPE);
    assert_eq!(actual[0].severity, DiagnosticSeverity::Error);
    assert_eq!(
        actual[0].span,
        Some(Span::pointer("/objectTypes/0/extends"))
    );
    assert!(actual[0].structural_evidence().is_none());
    assert!(actual[0].to_json().get("evidence").is_none());
    assert!(findings(&types(&[("test.A", None), ("test.B", Some("test.A"))]), &[]).is_empty());
}

#[test]
fn mixed_layers_report_only_local_members_and_snapshots_never_share_state() {
    let base = types(&[("test.Base", Some("test.Local"))]);
    let local = types(&[
        ("test.Local", Some("test.Base")),
        ("test.Child", Some("test.Local")),
    ]);
    assert_eq!(findings(&local, &base), [cycle(0, "test.Local")]);
    // Equal names in a distinct selected catalog do not inherit the prior cycle.
    let other_base = types(&[("test.Base", None)]);
    assert!(findings(&local, &other_base).is_empty());
    assert_eq!(findings(&local, &base), [cycle(0, "test.Local")]);
    // Unrelated base-only cycles are not re-reported as local diagnostics.
    assert!(
        findings(
            &types(&[("test.Clear", None)]),
            &types(&[("test.A", Some("test.A"))])
        )
        .is_empty()
    );
}

#[test]
fn budget_refusal_stops_before_cyclic_ancestry_and_effective_tables() {
    let mut local = types(&[("test.A", Some("test.B")), ("test.B", Some("test.A"))]);
    local.extend(
        (0..4_095).map(|index| types(&[(&format!("test.T{index:04}"), Some("test.A"))]).remove(0)),
    );
    let actual = findings(&local, &[]);
    assert_eq!(actual.len(), 1);
    assert_eq!(
        actual[0].code.as_str(),
        "object_type.validation.inheritance_budget_exhausted"
    );
    assert_eq!(actual[0].severity, DiagnosticSeverity::Error);
    assert_eq!(actual[0].span, Some(Span::pointer("/objectTypes")));
    assert!(
        actual[0]
            .message
            .contains("catalog validation stopped before ancestry resolution")
    );
    let registry = BTreeSet::new();
    assert!(
        effective_object_types(
            &EntityModel {
                object_types: &local,
                ..EntityModel::EMPTY
            },
            &EntityContext {
                catalog: &[],
                registry: &registry,
                base: EntityModel::EMPTY,
                tenant: None
            },
        )
        .is_err_and(|refusal| refusal.dimension == "registered types" && refusal.limit == 4_096)
    );
}

#[test]
fn more_than_the_type_budget_refuses_the_effective_view_instead_of_returning_an_empty_one() {
    let local = types(
        &(0..4_097)
            .map(|index| (format!("test.T{index:04}"), None::<String>))
            .collect::<Vec<_>>()
            .iter()
            .map(|(name, parent)| (name.as_str(), parent.as_deref()))
            .collect::<Vec<_>>(),
    );
    let registry = BTreeSet::new();
    let model = EntityModel {
        object_types: &local,
        ..EntityModel::EMPTY
    };
    let context = EntityContext {
        catalog: &[],
        registry: &registry,
        base: EntityModel::EMPTY,
        tenant: None,
    };
    let refusal = effective_object_types(&model, &context)
        .expect_err("4,097 registered types exceed the classification budget");
    assert_eq!(
        (refusal.dimension, refusal.limit),
        ("registered types", 4_096)
    );
    assert_eq!(
        refusal.diagnostic().code.as_str(),
        "object_type.validation.inheritance_budget_exhausted"
    );
    assert!(refusal.to_string().contains("budget"));
    // The model check refuses the same input, so the two never disagree.
    assert!(
        check_entity_model(&model, &context)
            .iter()
            .any(|finding| finding.code.as_str()
                == "object_type.validation.inheritance_budget_exhausted")
    );
}

#[test]
fn local_filtering_cannot_turn_a_stopped_pass_into_success() {
    let local = types(&[("test.Local", Some(&"test.Parent".repeat(120_000)))]);
    let actual = norn_checker::check_object_type_local(&local[0]);
    assert_eq!(actual.len(), 1);
    assert_eq!(
        actual[0].code.as_str(),
        "object_type.validation.inheritance_budget_exhausted"
    );
    assert_eq!(actual[0].span, Some(Span::pointer("")));
}
