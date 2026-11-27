//! A retained set is re-checked under its verified receipt by both rings
//! (`norn_ui_checker::check_artifact_set_with_verified_receipt`), as a cell does before it acts on
//! it (`norn-api` `admit_retained`): a set with no widget gets Ring-0's receipt byte for byte, and a
//! retained widget is judged by the UI laws, with every Ring-0 refusal kept.

use super::{
    WIDGET_ID, approve_machine, drop_transition, edit, envelope, machine_widget, support_artifacts,
};
use crate::TestMust;
use norn_checker::{
    CheckReceipt, CheckedArtifactSet, DiagnosticsDigest, EntityArtifactSetContext, ReceiptSources,
    ReceiptVersion,
};
use norn_ir::{ArtifactEnvelope, ResourceLimitProfile};
use serde_json::{Value, json};
use std::collections::BTreeSet;

fn encoded(artifacts: &[ArtifactEnvelope]) -> Vec<norn_ir::RuntimeValue> {
    norn_ir::runtime::encode(artifacts)
        .req("artifacts encode")
        .as_array()
        .req("encoded artifacts")
        .clone()
}

fn sources(artifacts: &[ArtifactEnvelope]) -> ReceiptSources {
    ReceiptSources::new(&encoded(artifacts), &[]).req("receipt sources")
}

fn stored(receipt: &CheckReceipt) -> String {
    String::from_utf8(receipt.canonical_json().req("canonical receipt")).req("UTF-8 receipt")
}

/// The verified receipt of `artifacts` as a cell retains it. A set the current laws refuse is
/// given the receipt an earlier checker issued when it admitted the set: clean, with its type
/// environment, so only the re-check can refuse it.
fn retained(artifacts: &[ArtifactEnvelope]) -> ReceiptVersion {
    let limits = ResourceLimitProfile::norn_limits_v1();
    let registry = BTreeSet::new();
    let context = EntityArtifactSetContext {
        registry: &registry,
        base: &[],
        tenant: None,
    };
    let issued =
        norn_ui_checker::check_artifact_set_with_receipt(&sources(artifacts), &context, &limits)
            .req("receipt issued");
    let mut receipt = issued.receipt;
    if !receipt.admitted {
        receipt.admitted = true;
        receipt.diagnostics = DiagnosticsDigest::of(&[]).req("empty digest");
        receipt.type_env = Some(
            norn_checker::entity_scopes::receipt_type_env(artifacts, &[], &limits)
                .ok()
                .req("type environment"),
        );
    }
    let stored = stored(&receipt);
    let hash = receipt.content_sha256().req("receipt hash");
    norn_checker::receipt::verify_admission_receipt_sources(
        &stored,
        &hash,
        &encoded(artifacts),
        &[],
        &limits,
    )
    .req("retained receipt verifies")
}

/// The re-check of `artifacts` under its retained receipt: Ring-0's, and both rings'.
fn rechecked(artifacts: &[ArtifactEnvelope]) -> (CheckedArtifactSet, CheckedArtifactSet) {
    let version = retained(artifacts);
    let limits = ResourceLimitProfile::norn_limits_v1();
    let registry = BTreeSet::new();
    let context = EntityArtifactSetContext {
        registry: &registry,
        base: &[],
        tenant: None,
    };
    let ring0 = norn_checker::check_artifact_set_with_verified_receipt(
        &sources(artifacts),
        &context,
        &limits,
        &version,
    )
    .req("Ring-0 re-check");
    let both = norn_ui_checker::check_artifact_set_with_verified_receipt(
        &sources(artifacts),
        &context,
        &limits,
        &version,
    )
    .req("Ring-1 re-check");
    (ring0, both)
}

fn set_with(widget: Option<Value>) -> Vec<ArtifactEnvelope> {
    support_artifacts()
        .into_iter()
        .chain(widget)
        .map(envelope)
        .collect()
}

fn codes(checked: &CheckedArtifactSet) -> BTreeSet<(String, Option<String>)> {
    checked
        .diagnostics
        .iter()
        .filter(|finding| finding.is_error())
        .map(|finding| {
            (
                finding.code.as_str().to_owned(),
                finding.span.as_ref().map(|span| span.pointer.clone()),
            )
        })
        .collect()
}

#[test]
fn a_retained_set_with_no_widget_gets_ring0s_receipt_byte_for_byte() {
    let (ring0, both) = rechecked(&set_with(None));
    assert!(ring0.receipt.admitted, "{:#?}", ring0.diagnostics);
    assert_eq!(
        stored(&both.receipt),
        stored(&ring0.receipt),
        "the re-check of a set with no widget is Ring-0's"
    );
    assert_eq!(
        both.receipt.content_sha256().req("hash"),
        ring0.receipt.content_sha256().req("hash")
    );
    assert_eq!(both.diagnostics, ring0.diagnostics);
}

#[test]
fn a_retained_widget_that_breaks_a_ui_law_is_refused_by_the_recheck() {
    // Admitted once, the page now leaves a pending call's timeout unhandled (UI-MACHINE-1).
    let broken = edit(approve_machine(), |machine| {
        drop_transition(machine, "on-timeout");
    });
    let (ring0, both) = rechecked(&set_with(Some(broken)));
    assert!(!both.receipt.admitted);
    assert!(
        both.diagnostics.iter().any(|finding| finding.is_error()
            && finding.artifact_id.as_deref() == Some(WIDGET_ID)
            && finding.code.as_str() == "widget.machine.pending_outcome_uncovered"),
        "{:#?}",
        both.diagnostics
    );
    // Ring-0 alone cannot judge the widget and refuses it; both rings judge it.
    assert!(!ring0.receipt.admitted);
    assert!(
        codes(&ring0)
            .iter()
            .any(|(code, _)| code == norn_checker::codes::WIDGET_RING1_REQUIRED),
        "{:#?}",
        ring0.diagnostics
    );
    assert!(
        !codes(&both)
            .iter()
            .any(|(code, _)| code == norn_checker::codes::WIDGET_RING1_REQUIRED)
    );
}

#[test]
fn a_retained_widget_keeps_every_ring0_refusal() {
    // A widget that names an artifact the set does not hold: Ring-0's dependency refusal.
    let mut widget = machine_widget(approve_machine());
    widget["dependencies"] = json!([{
        "artifactId": "commerce.missing.entity", "kind": "entity", "version": "0.1.0", "required": true
    }]);
    let (ring0, both) = rechecked(&set_with(Some(widget)));
    assert!(!both.receipt.admitted);
    let ring1 = codes(&both);
    let kept: Vec<_> = codes(&ring0)
        .into_iter()
        .filter(|(code, _)| code != norn_checker::codes::WIDGET_RING1_REQUIRED)
        .collect();
    assert!(
        kept.iter().any(
            |(code, pointer)| code == "artifact_set.resolution.missing_dependency"
                && pointer.as_deref() == Some("/dependencies/0")
        ),
        "{:#?}",
        ring0.diagnostics
    );
    for finding in &kept {
        assert!(
            ring1.contains(finding),
            "Ring-0 refusal {finding:?} dropped by the re-check: {ring1:#?}"
        );
    }
}
