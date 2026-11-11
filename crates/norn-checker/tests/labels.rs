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
