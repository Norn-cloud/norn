//! The importer conformance harness, shared by every importer's tests.
//!
//! [`check_conformance`] runs an importer twice on one input and fails on:
//! non-identical canonical output (no clock, no randomness, no map-order
//! leaks); output that is not in canonical form; any `error` diagnostic;
//! any loss-report finding (unexplained elements, unregistered codes,
//! dangling targets, `SourceSchemaIR` elements without an entry, drops
//! outside the allowed [`DropReason`] set); and any checker finding on any
//! layer (each layer checked with the earlier layers as its base) or on the
//! `SourceSchemaIR` artifact.

#![allow(dead_code, reason = "each test crate uses part of the harness")]

use std::collections::BTreeSet;
use std::path::Path;

use norn_checker::{EntityArtifactSetContext, TenantLayer, check_artifact_set};
use norn_formats::contract::{DropReason, LossReport, sorted_keys};
use norn_formats::{Disposition, SchemaImport, SchemaImporter};
use norn_ir::{ArtifactEnvelope, DiagnosticSeverity};

pub(crate) const UPDATE_ENV: &str = "NORN_FORMATS_UPDATE_GOLDENS";

/// Compare `actual` with the golden at `relative` (or rewrite it under
/// [`UPDATE_ENV`]).
pub(crate) fn assert_golden(relative: &str, actual: &str) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(relative);
    if std::env::var_os(UPDATE_ENV).is_some() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("golden dir");
        }
        std::fs::write(&path, actual).expect("write golden");
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_default();
    if expected != actual {
        panic!(
            "golden {relative} is stale; rerun with {UPDATE_ENV}=1 and review the diff\n\
             ----- BEGIN ACTUAL {relative} -----\n{actual}----- END ACTUAL {relative} -----"
        );
    }
}

/// The loss report as JSON Lines: one compact, key-sorted entry per line
/// (diff-friendly goldens).
pub(crate) fn loss_jsonl(report: &LossReport) -> String {
    let mut out = String::new();
    for entry in &report.entries {
        let value = sorted_keys(serde_json::to_value(entry).expect("entry serializes"));
        out.push_str(&serde_json::to_string(&value).expect("entry serializes"));
        out.push('\n');
    }
    out
}

/// The canonical output without its loss report (which has its own golden).
pub(crate) fn output_without_loss(import: &SchemaImport) -> String {
    let mut value: serde_json::Value =
        serde_json::from_str(&import.canonical_json().expect("canonical JSON")).expect("JSON");
    if let Some(map) = value.as_object_mut() {
        map.remove("lossReport");
    }
    // Removing a key may swap the last entry into its slot with preserve_order.
    let mut text = serde_json::to_string_pretty(&sorted_keys(value)).expect("JSON");
    text.push('\n');
    text
}

/// Run the conformance checks; returns the first run's output.
pub(crate) fn check_conformance<I: SchemaImporter>(
    importer: &I,
    source: I::Source<'_>,
    config: &I::Config,
    schema_prefix: &str,
) -> SchemaImport {
    let first = importer.import(source, config);
    let second = importer.import(source, config);

    // 1. Determinism: byte-identical canonical output.
    let first_json = first.canonical_json().expect("canonical JSON");
    let second_json = second.canonical_json().expect("canonical JSON");
    assert!(first_json == second_json, "two runs differ");
    assert_eq!(first, second, "two runs differ structurally");

    // 2. Canonical form: re-canonicalizing is a fixed point; artifact ids
    //    are unique; loss entries strictly ordered (also in validate()).
    let reparsed: serde_json::Value = serde_json::from_str(&first_json).expect("JSON");
    let mut again = serde_json::to_string_pretty(&sorted_keys(reparsed)).expect("JSON");
    again.push('\n');
    assert!(again == first_json, "canonical JSON is not a fixed point");
    let mut ids = BTreeSet::new();
    for artifact in first.layers.iter().flat_map(|layer| &layer.artifacts) {
        assert!(
            ids.insert(artifact.artifact_id.clone()),
            "duplicate artifact id {}",
            artifact.artifact_id
        );
    }

    // 3. No error diagnostics.
    let errors: Vec<_> = first
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity == DiagnosticSeverity::Error)
        .collect();
    assert!(errors.is_empty(), "importer errors: {errors:#?}");

    // 4. Loss report: zero findings, zero unexplained, closed drop reasons,
    //    and it round-trips through its typed form.
    let inventory = importer.inventory(source, config);
    let mut findings = first.loss_report.validate_inventory(&inventory);
    findings.extend(
        first
            .loss_report
            .validate(&first.layers, first.source_schema.as_ref()),
    );
    assert!(
        findings.is_empty(),
        "loss report findings:\n{}",
        findings.join("\n")
    );
    let summary = first.loss_report.summary();
    assert_eq!(summary.unexplained, 0);
    assert!(summary.total > 0, "empty inventory");
    for entry in &first.loss_report.entries {
        if let Disposition::Dropped { reason, .. } = &entry.disposition {
            assert!(DropReason::ALL.contains(reason), "{entry:?}");
        }
    }
    let wire = serde_json::to_string(&first.loss_report).expect("loss report serializes");
    let decoded: LossReport = serde_json::from_str(&wire).expect("loss report decodes");
    assert_eq!(decoded, first.loss_report);
    let targeted: BTreeSet<&str> = first
        .loss_report
        .entries
        .iter()
        .filter_map(|entry| match &entry.disposition {
            Disposition::Mapped { target, .. } | Disposition::Extension { target, .. } => {
                Some(target.artifact_id.as_str())
            }
            Disposition::Deferred { .. }
            | Disposition::Rejected { .. }
            | Disposition::Dropped { .. }
            | Disposition::Unexplained => None,
        })
        .collect();
    for id in &ids {
        assert!(
            targeted.contains(id.as_str()),
            "{id} is the target of no source element"
        );
    }

    // 5. Pins: every input pinned; tables pinned when evaluated.
    assert!(!first.provenance.inputs.is_empty());
    if first.provenance.evaluator.is_some() {
        assert!(
            !first.provenance.tables.is_empty(),
            "evaluator without table pins"
        );
    }

    // 6. Artifact-set checker: zero findings per layer.
    let schema = first
        .source_schema_envelope(schema_prefix)
        .expect("source schema envelope")
        .expect("source schema");
    let registry = BTreeSet::new();
    let mut base: Vec<ArtifactEnvelope> = Vec::new();
    for (index, layer) in first.layers.iter().enumerate() {
        let namespace = layer.tenant.as_ref().map(|tenant| tenant.namespace.clone());
        let tenant = layer
            .tenant
            .as_ref()
            .zip(namespace.as_ref())
            .map(|(tenant, namespace)| TenantLayer {
                tenant_id: &tenant.tenant_id,
                namespace,
            });
        let context = EntityArtifactSetContext {
            registry: &registry,
            base: &base,
            tenant,
        };
        let mut artifacts = layer.artifacts.clone();
        if index == 0 {
            artifacts.push(schema.clone());
        }
        let diagnostics = check_artifact_set(&artifacts, &context);
        assert!(
            diagnostics.is_empty(),
            "layer {} checker findings: {diagnostics:#?}",
            layer.name,
        );
        base.extend(layer.artifacts.iter().cloned());
    }
    first
}
