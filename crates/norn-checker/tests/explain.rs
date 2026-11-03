#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::pedantic,
    missing_docs,
    reason = "read-only explanation conformance tests"
)]
use norn_checker::{EntityArtifactSetContext, explain::explain};
use norn_ir::{
    ArtifactEnvelope,
    explain::{ExplainKind, ExplainOptions, ExplainProof},
};
use serde_json::json;
use std::collections::BTreeSet;

fn pool() -> Vec<ArtifactEnvelope> {
    let mut pool: Vec<_> =
        serde_json::from_str(include_str!("../../../fixtures/law-ir/entities.json")).unwrap();
    pool.extend(
        serde_json::from_str::<Vec<ArtifactEnvelope>>(include_str!(
            "../../../fixtures/law-ir/laws.json"
        ))
        .unwrap(),
    );
    pool
}
fn run(pool: &[ArtifactEnvelope], kind: ExplainKind, id: &str) -> norn_ir::explain::ExplainReport {
    explain(
        pool,
        &EntityArtifactSetContext {
            registry: &BTreeSet::new(),
            base: &[],
            tenant: None,
        },
        kind,
        id,
    )
    .expect("admitted explanation")
}

#[test]
fn incomplete_law_names_the_uncovered_writer_and_footprints() {
    let report = run(&pool(), ExplainKind::Law, "erpnext.no_unpaid_ship");
    assert_eq!(report.laws.len(), 1);
    let law = &report.laws[0];
    assert_eq!(law.proof, ExplainProof::Unknown);
    assert!(law.enforcement.is_none());
    assert!(law.why_unknown.iter().any(|gap| gap.writer.is_some()));
    assert!(
        law.why_unknown
            .iter()
            .all(|gap| !gap.footprint.is_empty() && !gap.fix.is_empty())
    );
    assert!(
        law.why_unknown
            .iter()
            .any(|gap| gap.footprint == "migration")
    );
    assert_eq!(
        law.source_path.as_deref(),
        Some("fixtures/law-ir/laws.source.ts")
    );
    assert_eq!(report.closure.artifacts.len(), pool().len());
    assert!(!law.obligations.is_empty());
}

#[test]
fn runtime_invariant_is_separate_from_unknown_proof() {
    let report = run(&pool(), ExplainKind::Law, "erpnext.invariant_identity");
    assert!(matches!(
        report.laws[0].enforcement,
        Some(norn_ir::definition_catalog::LawBinding::Invariant { .. })
    ));
    assert_eq!(report.laws[0].proof, ExplainProof::Unknown);
}

#[test]
fn concept_inheritance_reaches_the_authored_subject_and_source() {
    let mut pool = pool();
    let mut child = serde_json::to_value(&pool[0]).unwrap();
    child["artifactId"] = "erpnext.SpecialOrder.entity".into();
    child["payload"]["objectType"] =
        json!({"name":"erpnext.SpecialOrder", "extends":"erpnext.SalesOrder", "properties":[]});
    pool.push(serde_json::from_value(child).unwrap());
    let report = run(&pool, ExplainKind::Concept, "erpnext.SpecialOrder");
    let law = report
        .laws
        .iter()
        .find(|law| law.law.id == "erpnext.no_unpaid_ship")
        .unwrap();
    assert_eq!(
        law.inheritance_paths[0]
            .iter()
            .map(|at| at.member.id())
            .collect::<Vec<_>>(),
        ["type:erpnext.SpecialOrder", "type:erpnext.SalesOrder"]
    );
}

#[test]
fn containment_keeps_each_pinned_pack_on_the_path() {
    let mut pool = pool();
    for (id, member, kind) in [
        ("erpnext.inner", "erpnext.no_unpaid_ship.law", "law"),
        ("erpnext.outer", "erpnext.inner", "business_pack"),
    ] {
        pool.push(serde_json::from_value(json!({
            "irVersion":"0.1", "artifactId":id, "kind":"business_pack", "namespace":"erpnext", "name":id,
            "version":"0.1.0", "lifecycle":"draft", "scope":{"orgId":"org_demo", "environmentId":"dev"},
            "dependencies":[], "provenance":{"source":"system"},
            "payload":{"packId":id, "packVersion":"0.1.0", "packKind":"composition", "artifacts":[{"artifactId":member,"kind":kind,"version":"0.1.0"}]}
        })).unwrap());
    }
    let report = run(&pool, ExplainKind::Law, "erpnext.no_unpaid_ship");
    assert!(report.laws[0].containment_paths.iter().any(|path| {
        path.iter()
            .map(|key| key.artifact_id.as_str())
            .collect::<Vec<_>>()
            == [
                "erpnext.outer",
                "erpnext.inner",
                "erpnext.no_unpaid_ship.law",
            ]
    }));
}

#[test]
fn unknown_selector_is_an_existing_diagnostic_with_guidance() {
    let errors = explain(
        &pool(),
        &EntityArtifactSetContext {
            registry: &BTreeSet::new(),
            base: &[],
            tenant: None,
        },
        ExplainKind::Law,
        "not-present",
    )
    .unwrap_err();
    assert_eq!(
        errors[0].code.as_str(),
        norn_checker::verification::codes::UNKNOWN_BINDING
    );
    assert!(errors[0].to_json_v2()["fix"].is_string());
}

#[test]
fn action_direct_effects_are_the_same_catalog_rows() {
    let paths = [
        "../../fixtures/ir/entity-v2/artifact-set/commerce-sales-order.entity.json",
        "../../fixtures/ir/commerce-sales-order-submit-invoice.action.json",
    ];
    let pool: Vec<_> = paths
        .iter()
        .map(|path| {
            let text = std::fs::read_to_string(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(path),
            )
            .unwrap();
            serde_json::from_str(&text).unwrap()
        })
        .collect();
    let report = run(
        &pool,
        ExplainKind::Action,
        "commerce.sales_order.submit_invoice.action",
    );
    assert!(!report.direct_effects.is_empty());
    assert!(
        report
            .direct_effects
            .iter()
            .all(|effect| effect.writer == report.target)
    );
}

fn requirement(law: &norn_ir::LawIR, french: bool) -> norn_ir::deontic::StructuredRequirement {
    use norn_ir::deontic::{DeonticModality, DeonticRule, RequirementTiming, RuleSourceSpan};
    norn_ir::deontic::StructuredRequirement {
        norm: DeonticRule {
            law: law.clone(),
            modality: DeonticModality::Prohibition,
            authority_id: "demo.review".into(),
            authority_version: "1".into(),
            source: RuleSourceSpan {
                artifact_id: "demo.review".into(),
                pointer: "/laws/0".into(),
                bytes: None,
            },
            exceptions: Vec::new(),
            defeated_by: Vec::new(),
        },
        timing: RequirementTiming::SameCommit,
        wording: serde_json::from_str(if french {
            include_str!("../../../fixtures/wording/french.json")
        } else {
            include_str!("../../../fixtures/wording/locales.json")
        })
        .unwrap(),
    }
}

#[test]
fn wording_uses_arabic_and_data_only_french_maps_and_refuses_missing_locale() {
    for (locale, french, direction) in [("ar", false, "rtl"), ("fr", true, "ltr")] {
        let mut report = run(&pool(), ExplainKind::Law, "erpnext.no_unpaid_ship");
        let options = ExplainOptions {
            locale: Some(locale.into()),
            requirements: vec![requirement(&report.laws[0].law, french)],
        };
        norn_checker::explain::render_wording(&mut report, &options).unwrap();
        let wording = report.laws[0].wording.as_ref().unwrap();
        assert_eq!(wording.direction, direction);
        assert!(!wording.slots.is_empty());
        assert_eq!(wording.sha256.len(), 64);
        let invalid = ExplainOptions {
            locale: Some("zz".into()),
            ..options
        };
        let errors = norn_checker::explain::render_wording(&mut report, &invalid).unwrap_err();
        assert_eq!(errors[0].code.as_str(), "law.wording.unknown_locale");
        let missing = ExplainOptions { locale: None, ..invalid };
        let errors = norn_checker::explain::render_wording(&mut report, &missing).unwrap_err();
        assert_eq!(errors[0].code.as_str(), "law.wording.unknown_locale");
    }
}

#[test]
fn wording_with_the_same_law_id_but_a_different_pin_is_refused() {
    let mut report = run(&pool(), ExplainKind::Law, "erpnext.no_unpaid_ship");
    let mut stale = requirement(&report.laws[0].law, false);
    stale.norm.law.rationale.push_str(" stale revision");
    let error = norn_checker::explain::render_wording(
        &mut report,
        &ExplainOptions {
            locale: Some("en".into()),
            requirements: vec![stale],
        },
    )
    .unwrap_err();
    assert_eq!(
        error[0].code.as_str(),
        norn_checker::verification::codes::UNKNOWN_BINDING
    );
    assert!(report.laws[0].wording.is_none());
}

proptest::proptest! {
    #[test]
    fn explanation_is_invariant_under_input_permutation(rotation in 0usize..80) {
        let mut artifacts = pool();
        let expected = serde_json::to_value(run(&artifacts, ExplainKind::Law, "erpnext.no_unpaid_ship")).unwrap();
        let count = artifacts.len();
        artifacts.rotate_left(rotation.checked_rem(count).unwrap_or_default());
        if rotation.is_multiple_of(2) { artifacts.reverse(); }
        proptest::prop_assert_eq!(serde_json::to_value(run(&artifacts, ExplainKind::Law, "erpnext.no_unpaid_ship")).unwrap(), expected);
    }
}
