//! LABEL-LOCALE-1: label catalogs are admitted only when well-formed, and only
//! for definitions the set or the installed base declares; a downstream
//! catalog overlays upstream labels, two candidate catalogs never compete.
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    missing_docs,
    reason = "integration scenarios"
)]
use norn_checker::{EntityArtifactSetContext, check_artifact, check_artifact_set};
use norn_ir::ArtifactEnvelope;
use proptest::prelude::*;
use serde_json::{Value, json};
use std::collections::BTreeSet;

fn envelope(id: &str, kind: &str, payload: &Value) -> ArtifactEnvelope {
    let (namespace, name) = id.split_once('.').expect("qualified id");
    serde_json::from_value(json!({
        "irVersion": "0.1", "artifactId": id, "kind": kind, "namespace": namespace,
        "name": name, "version": "1.0.0", "lifecycle": "draft", "scope": {},
        "dependencies": [], "provenance": { "source": "system" }, "payload": payload,
    }))
    .expect("envelope")
}

/// `sales.Invoice` with an inline `status` enum and a `number`.
fn invoice() -> ArtifactEnvelope {
    envelope(
        "sales.Invoice.entity",
        "entity",
        &json!({
            "entityVersion": "2.0",
            "objectType": {
                "name": "sales.Invoice",
                "properties": [
                    { "id": "number", "type": { "kind": "text" } },
                    { "id": "status", "type": { "kind": "enum", "values": [
                        { "value": "draft" }, { "value": "posted" }
                    ] } }
                ]
            }
        }),
    )
}

fn labels(id: &str, entries: &Value) -> ArtifactEnvelope {
    envelope(
        id,
        "labels",
        &json!({ "labelsVersion": "0.1", "entries": entries }),
    )
}

fn codes(findings: &[norn_checker::Diagnostic]) -> BTreeSet<String> {
    findings
        .iter()
        .filter(|finding| finding.is_error())
        .map(|finding| finding.code.as_str().to_owned())
        .collect()
}

fn set_codes(candidates: &[ArtifactEnvelope], base: &[ArtifactEnvelope]) -> BTreeSet<String> {
    codes(&check_artifact_set(
        candidates,
        &EntityArtifactSetContext {
            registry: &BTreeSet::new(),
            base,
            tenant: None,
        },
    ))
}

fn invoice_labels() -> Value {
    json!([
        { "target": { "kind": "objectType", "objectType": "sales.Invoice" },
          "label": { "en": "Invoice", "ar": "فاتورة" },
          "description": { "ar": "فاتورة بيع للعميل" },
          "review": { "ar": "needs_review" } },
        { "target": { "kind": "property", "objectType": "sales.Invoice", "property": "number" },
          "label": { "en": "Number", "ar": "الرقم" } },
        { "target": { "kind": "enumValue", "objectType": "sales.Invoice", "property": "status", "value": "posted" },
          "label": { "en": "Posted", "ar-EG": "مُرحّلة" } }
    ])
}

#[test]
fn a_complete_catalog_is_admitted_with_its_definitions() {
    let catalog = labels("sales.labels", &invoice_labels());
    assert!(codes(&check_artifact(&catalog)).is_empty());
    assert_eq!(set_codes(&[invoice(), catalog], &[]), BTreeSet::new());
}

#[test]
fn a_downstream_catalog_overlays_installed_labels() {
    let upstream = labels("sales.labels", &invoice_labels());
    let overlay = labels(
        "tenant.labels",
        &json!([{ "target": { "kind": "objectType", "objectType": "sales.Invoice" },
                 "label": { "ar": "فاتورة ضريبية" } }]),
    );
    assert_eq!(
        set_codes(&[overlay], &[invoice(), upstream]),
        BTreeSet::new()
    );
}

#[test]
fn two_candidate_catalogs_never_label_one_target_in_one_locale() {
    let one = labels(
        "sales.labels",
        &json!([{ "target": { "kind": "objectType", "objectType": "sales.Invoice" }, "label": { "ar": "فاتورة" } }]),
    );
    let other = labels(
        "tenant.labels",
        &json!([{ "target": { "kind": "objectType", "objectType": "sales.Invoice" }, "label": { "AR": "فاتورة ضريبية", "en": "Tax invoice" } }]),
    );
    assert_eq!(
        set_codes(&[invoice(), one.clone(), other], &[]),
        BTreeSet::from(["labels.admission.conflicting_label".to_owned()])
    );
    let disjoint = labels(
        "tenant.labels",
        &json!([{ "target": { "kind": "objectType", "objectType": "sales.Invoice" }, "label": { "en": "Tax invoice" } }]),
    );
    assert_eq!(set_codes(&[invoice(), one, disjoint], &[]), BTreeSet::new());
}

#[test]
fn every_target_resolves_to_a_declared_definition() {
    for target in [
        json!({ "kind": "objectType", "objectType": "sales.Receipt" }),
        json!({ "kind": "property", "objectType": "sales.Invoice", "property": "total" }),
        json!({ "kind": "enumValue", "objectType": "sales.Invoice", "property": "status", "value": "void" }),
        json!({ "kind": "enumValue", "objectType": "sales.Invoice", "property": "number", "value": "draft" }),
        json!({ "kind": "action", "actionId": "sales.post" }),
        json!({ "kind": "rule", "ruleId": "sales.tax" }),
        json!({ "kind": "law", "lawId": "sales.balanced" }),
        json!({ "kind": "page", "widgetId": "sales", "pageId": "home" }),
    ] {
        let catalog = labels(
            "sales.labels",
            &json!([{ "target": target, "label": { "en": "X" } }]),
        );
        assert!(codes(&check_artifact(&catalog)).is_empty(), "{target}");
        assert_eq!(
            set_codes(&[invoice(), catalog], &[]),
            BTreeSet::from(["labels.admission.unknown_target".to_owned()]),
            "{target}"
        );
    }
}

#[test]
fn malformed_catalogs_refuse_with_stable_codes() {
    let target = json!({ "kind": "objectType", "objectType": "sales.Invoice" });
    for (entries, code) in [
        (
            json!([{ "target": target, "label": { "en_US": "Invoice" } }]),
            "labels.validation.invalid_locale",
        ),
        (
            json!([{ "target": target, "label": { "ar": "فاتورة", "AR": "فاتورة" } }]),
            "labels.validation.duplicate_locale",
        ),
        (
            json!([{ "target": target, "label": {} }]),
            "labels.validation.blank_text",
        ),
        (
            json!([{ "target": target, "label": { "ar": "  " } }]),
            "labels.validation.blank_text",
        ),
        (
            json!([{ "target": target, "label": { "en": "Invoice" }, "description": { "ar": "فاتورة" } }]),
            "labels.validation.unlabelled_locale",
        ),
        (
            json!([{ "target": target, "label": { "en": "Invoice" }, "review": { "ar": "needs_review" } }]),
            "labels.validation.unlabelled_locale",
        ),
        (
            json!([{ "target": target, "label": { "en": "A" } }, { "target": target, "label": { "ar": "ب" } }]),
            "labels.validation.duplicate_target",
        ),
        (
            json!([{ "target": { "kind": "type", "name": "sales.Invoice" }, "label": { "en": "Invoice" } }]),
            "labels.validation.invalid_payload",
        ),
        (
            json!([{ "target": target, "label": { "en": "Invoice" }, "note": "x" }]),
            "labels.validation.invalid_payload",
        ),
    ] {
        let catalog = labels("sales.labels", &entries);
        assert_eq!(
            codes(&check_artifact(&catalog)),
            BTreeSet::from([code.to_owned()]),
            "{entries}"
        );
    }
}

proptest! {
    /// Any embedding, override or isolate control in a label or description
    /// refuses: stored text never reorders the ids rendered around it.
    #[test]
    fn bidi_controls_in_label_text_refuse(
        prefix in "[a-z\u{0621}-\u{064A}]{0,6}",
        control in prop::sample::select(vec!['\u{202a}', '\u{202b}', '\u{202c}', '\u{202d}', '\u{202e}', '\u{2066}', '\u{2067}', '\u{2068}', '\u{2069}']),
        suffix in "[a-z0-9\u{0621}-\u{064A}]{1,6}",
        in_description in any::<bool>(),
    ) {
        let text = format!("{prefix}{control}{suffix}");
        let mut entry = json!({ "target": { "kind": "objectType", "objectType": "sales.Invoice" }, "label": { "ar": "فاتورة" } });
        if in_description {
            entry["description"] = json!({ "ar": text });
        } else {
            entry["label"] = json!({ "ar": text });
        }
        let catalog = labels("sales.labels", &json!([entry]));
        prop_assert_eq!(codes(&check_artifact(&catalog)), BTreeSet::from(["labels.validation.bidi_control".to_owned()]));
    }

    /// Plain Arabic and Latin text with digits, including LRM/RLM/ALM marks
    /// (legitimate in Arabic text), is admitted in any well-formed locale.
    #[test]
    fn plain_text_in_well_formed_locales_is_admitted(
        tag in prop::sample::select(vec!["en", "ar", "ar-EG", "AR-eg", "fr-CA", "zh-Hant-TW", "x-pseudo"]),
        text in "[A-Za-z\u{0621}-\u{064A}0-9][A-Za-z\u{0621}-\u{064A}\u{0660}-\u{0669}0-9 \u{200e}\u{200f}\u{061c}]{0,20}",
    ) {
        let catalog = labels("sales.labels", &json!([{ "target": { "kind": "property", "objectType": "sales.Invoice", "property": "number" }, "label": Value::Object([(tag.to_owned(), Value::String(text))].into_iter().collect()) }]));
        prop_assert_eq!(codes(&check_artifact(&catalog)), BTreeSet::new());
        prop_assert_eq!(set_codes(&[invoice(), catalog], &[]), BTreeSet::new());
    }
}

fn invoice_targets() -> Vec<Value> {
    vec![
        json!({ "kind": "objectType", "objectType": "sales.Invoice" }),
        json!({ "kind": "property", "objectType": "sales.Invoice", "property": "number" }),
        json!({ "kind": "property", "objectType": "sales.Invoice", "property": "status" }),
        json!({ "kind": "enumValue", "objectType": "sales.Invoice", "property": "status", "value": "draft" }),
        json!({ "kind": "enumValue", "objectType": "sales.Invoice", "property": "status", "value": "posted" }),
    ]
}

fn target_key(target: &Value) -> String {
    serde_json::from_value::<norn_ir::LabelTarget>(target.clone())
        .expect("label target")
        .key()
}

fn missing_label_findings(
    candidates: &[ArtifactEnvelope],
    base: &[ArtifactEnvelope],
) -> Vec<norn_checker::Diagnostic> {
    check_artifact_set(
        candidates,
        &EntityArtifactSetContext {
            registry: &BTreeSet::new(),
            base,
            tenant: None,
        },
    )
    .into_iter()
    .filter(|finding| finding.code.as_str() == "labels.lint.missing_label")
    .collect()
}

#[test]
fn a_required_locale_lists_every_unlabelled_definition_as_a_warning() {
    let catalog = envelope(
        "tenant.labels",
        "labels",
        &json!({ "labelsVersion": "0.1", "requiredLocales": ["ar"], "entries": [
            { "target": invoice_targets()[0], "label": { "ar": "فاتورة" } }
        ] }),
    );
    let findings = missing_label_findings(&[invoice(), catalog], &[]);
    assert_eq!(findings.len(), 1);
    assert!(!findings[0].is_error());
    assert_eq!(findings[0].artifact_id.as_deref(), Some("tenant.labels"));
    assert!(
        findings[0]
            .message
            .starts_with("4 definitions have no ar label: ")
    );
    let filled = envelope(
        "tenant.more_labels",
        "labels",
        &json!({ "labelsVersion": "0.1", "entries": invoice_targets()[1..].iter()
            .map(|target| json!({ "target": target, "label": { "AR": "تسمية" } })).collect::<Vec<_>>() }),
    );
    let catalog = envelope(
        "tenant.labels",
        "labels",
        &json!({ "labelsVersion": "0.1", "requiredLocales": ["ar"], "entries": [
            { "target": invoice_targets()[0], "label": { "ar": "فاتورة" } }
        ] }),
    );
    assert!(missing_label_findings(&[filled, catalog], &[invoice()]).is_empty());
}

#[test]
fn an_installed_requirement_still_lists_new_definitions() {
    let requirement = envelope(
        "tenant.labels",
        "labels",
        &json!({ "labelsVersion": "0.1", "requiredLocales": ["ar"], "entries": [] }),
    );
    let findings = missing_label_findings(&[invoice()], &[requirement]);
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].artifact_id, None);
    assert!(
        findings[0]
            .message
            .starts_with("5 definitions have no ar label: ")
    );
}

#[test]
fn a_malformed_required_locale_refuses() {
    let catalog = labels_with_required(&["ar", "en_US"]);
    assert_eq!(
        codes(&check_artifact(&catalog)),
        BTreeSet::from(["labels.validation.invalid_locale".to_owned()])
    );
}

fn labels_with_required(locales: &[&str]) -> ArtifactEnvelope {
    envelope(
        "tenant.labels",
        "labels",
        &json!({ "labelsVersion": "0.1", "requiredLocales": locales, "entries": [] }),
    )
}

proptest! {
    /// LABEL-COVERAGE-1: the warning lists exactly the declared definitions
    /// with no label in the required locale in either layer.
    #[test]
    fn the_missing_list_is_exactly_the_unlabelled_complement(
        upstream in prop::collection::vec(any::<bool>(), 5),
        overlay in prop::collection::vec(any::<bool>(), 5),
        overlay_tag in prop::sample::select(vec!["ar", "AR", "ar-EG"]),
    ) {
        let entries = |chosen: &[bool], tag: &str| -> Vec<Value> {
            invoice_targets().into_iter().zip(chosen).filter(|(_, keep)| **keep)
                .map(|(target, _)| json!({
                    "target": target,
                    "label": Value::Object([(tag.to_owned(), json!("تسمية"))].into_iter().collect()),
                })).collect()
        };
        let installed = envelope("core.labels", "labels",
            &json!({ "labelsVersion": "0.1", "entries": entries(&upstream, "ar") }));
        let candidate = envelope("tenant.labels", "labels",
            &json!({ "labelsVersion": "0.1", "requiredLocales": ["ar"], "entries": entries(&overlay, overlay_tag) }));
        // A regional tag never satisfies its base language: no fallback.
        let covers = overlay_tag.eq_ignore_ascii_case("ar");
        let findings = missing_label_findings(&[candidate], &[invoice(), installed]);
        let expected: Vec<String> = invoice_targets().iter().zip(upstream.iter().zip(&overlay))
            .filter(|(_, (up, over))| !(**up || (covers && **over))).map(|(target, _)| target_key(target)).collect();
        let mut expected_sorted = expected.clone();
        expected_sorted.sort();
        if expected.is_empty() {
            prop_assert!(findings.is_empty());
        } else {
            prop_assert_eq!(findings.len(), 1);
            prop_assert_eq!(
                &findings[0].message,
                &format!("{} definitions have no ar label: {}", expected.len(), expected_sorted.join(", "))
            );
        }
    }
}
