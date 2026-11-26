#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::pedantic,
    missing_docs,
    reason = "integration tests for the registry/packs/data ontology pack"
)]

//! End-to-end `norn-checker` verification of the constitutional `data` ontology
//! pack (`registry/packs/data/artifacts/*.json`):
//! - Full admission over `registry/core/artifacts/` with 0 findings (`structural`).
//! - `LIFE-SOUND-1..8` structural soundness and `PROOF-LIFECYCLE` (`lifecycle_graph`)
//!   option-to-complete verification on `data.QualityCase` (`structural` +
//!   `exhaustive-finite` over pinned frozen bounds).
//! - `acyclic: true` enforcement on `data.SnapshotRecord.supersedes` (effective
//!   link flag and subtype-weakening refusal; commit-time cycle enforcement
//!   lives in `norn-kernel::commit::edges::check_acyclic_links`).
//! - `DATA-CAUSAL-1` (`LAW-LINK-1` + `LAW-INTERFACE-1`) temporal causality law
//!   lowering, second-hop refusal, installed-law replacement refusal, and
//!   `law_examples::evaluate` valuations.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;

use norn_checker::entity_scopes::ScopedView;
use norn_checker::law_examples::{Verdict, evaluate};
use norn_checker::lifecycle_soundness::codes as soundness_codes;
use norn_checker::object_types::codes as object_codes;
use norn_checker::obligations::lifecycle::finite::Bounds;
use norn_checker::obligations::{ApprovedContract, DefinitionSource};
use norn_checker::verification::codes as law_codes;
use norn_checker::{ArtifactValidation, EntityArtifactSetContext, check_artifact_set};
use norn_ir::{ArtifactEnvelope, LawIR, LifecycleOutcome};
use serde_json::{Value, json};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn load_envelope(path: &std::path::Path) -> ArtifactEnvelope {
    let text = fs::read_to_string(path).expect("artifact JSON is readable");
    let envelope: ArtifactEnvelope =
        serde_json::from_str(&text).expect("artifact envelope decodes");
    let validated = envelope.validate();
    assert!(validated.is_ok(), "{}: {validated:#?}", path.display());
    envelope
}

fn core_base_artifacts() -> Vec<ArtifactEnvelope> {
    let core = repo_root().join("registry/core/artifacts");
    let mut base: Vec<ArtifactEnvelope> = [
        "core.Agent.interface.json",
        "core.TimeInterval.interface.json",
        "core.value_types.entity.json",
        "core.time.json",
    ]
    .iter()
    .map(|name| load_envelope(&core.join(name)))
    .collect();
    base.push(load_envelope(
        &repo_root().join("registry/privacy/artifacts/privacy.markings.entity.json"),
    ));
    let extra_markings: ArtifactEnvelope = serde_json::from_value(json!({
        "irVersion": "0.1",
        "artifactId": "core.extra_markings.entity",
        "kind": "entity",
        "namespace": "core",
        "name": "Core extra markings",
        "version": "0.1.0",
        "lifecycle": "active",
        "scope": {},
        "dependencies": [],
        "provenance": { "source": "system" },
        "payload": {
            "entityVersion": "2.0",
            "markings": [
                {
                    "id": "pii.financial",
                    "label": "Financial PII",
                    "severity": "high",
                    "category": "pii.financial",
                    "mask": { "kind": "redacted" }
                },
                {
                    "id": "confidential.finance",
                    "label": "Confidential finance",
                    "severity": "high",
                    "category": "confidential.finance",
                    "mask": { "kind": "redacted" }
                },
                {
                    "id": "pii.health",
                    "label": "Health PII",
                    "severity": "high",
                    "category": "pii.health",
                    "mask": { "kind": "redacted" }
                }
            ]
        }
    }))
    .expect("extra markings envelope decodes");
    assert!(
        extra_markings.validate().is_ok(),
        "extra markings envelope validates"
    );
    base.push(extra_markings);
    base
}

fn data_pack_artifacts() -> Vec<ArtifactEnvelope> {
    let dir = repo_root().join("registry/packs/data/artifacts");
    let mut paths: Vec<PathBuf> = fs::read_dir(&dir)
        .expect("registry/packs/data/artifacts exists")
        .map(|entry| entry.expect("dir entry").path())
        .collect();
    paths.sort();
    assert!(
        !paths.is_empty(),
        "data pack must contain compiled artifacts"
    );
    paths.iter().map(|path| load_envelope(path)).collect()
}

fn law_payload(artifacts: &[ArtifactEnvelope], artifact_id: &str) -> LawIR {
    let artifact = artifacts
        .iter()
        .find(|a| a.artifact_id == artifact_id)
        .expect("expected law artifact in data pack");
    artifact
        .payload
        .to_json_value()
        .decode()
        .expect("law payload decodes")
}

#[test]
fn data_pack_admits_cleanly_over_core_base() {
    let base = core_base_artifacts();
    let data = data_pack_artifacts();
    let findings = check_artifact_set(
        &data,
        &EntityArtifactSetContext {
            registry: &BTreeSet::new(),
            base: &base,
            tenant: None,
        },
    );
    assert_eq!(
        findings,
        [],
        "registry/packs/data must admit with zero diagnostics: {findings:#?}"
    );
}

#[test]
fn quality_case_satisfies_life_sound_and_proof_lifecycle_graph() {
    let base = core_base_artifacts();
    let data = data_pack_artifacts();

    // Verify effective lifecycle shape on data.QualityCase.
    let mut combined = base.clone();
    combined.extend(data.clone());
    let catalog = ScopedView::uniform(&combined, &BTreeSet::new()).into_catalog();
    let quality_case = catalog
        .object_type("data.QualityCase.entity")
        .expect("QualityCase defined")
        .expect("QualityCase available");
    let status = quality_case
        .lifecycle("status")
        .expect("QualityCase declares status lifecycle");
    assert_eq!(status.lifecycle.initial, ["open"]);
    assert_eq!(status.lifecycle.terminal, ["resolved"]);
    assert_eq!(status.lifecycle.immutable, ["resolved"]);
    assert_eq!(status.lifecycle.endings.len(), 1);
    assert_eq!(status.lifecycle.endings[0].state, "resolved");
    assert_eq!(
        status.lifecycle.endings[0].outcome,
        LifecycleOutcome::Completed
    );

    // PROOF-LIFECYCLE (lifecycle_graph) + LIFE-SOUND-4 over pinned frozen bounds.
    let source_json = String::from_utf8(
        norn_canonical::canonical_json_exact_of(&data).expect("canonical data artifacts"),
    )
    .expect("UTF-8");
    let base_json = String::from_utf8(
        norn_canonical::canonical_json_exact_of(&base).expect("canonical base artifacts"),
    )
    .expect("UTF-8");
    let source = DefinitionSource::parse(&source_json, &base_json).expect("definition source");
    let authority = ApprovedContract::approve(
        &source,
        &BTreeSet::new(),
        json!({
            "expression": "norn-zen/7",
            "numeric": "exact-decimal",
            "time": "logical-millis",
            "missing": "distinct-null",
            "refusal": "atomic"
        }),
        json!({"axioms": [], "host": "single-cell"}),
        json!({"required": "checked-proof", "checker": "not-yet-qualified"}),
    )
    .expect("admitted data pack approves");

    let bounds = Bounds {
        object_type: "data.QualityCase".into(),
        property: "status".into(),
        seed: 0,
        frozen: BTreeMap::from([
            ("finding_id".into(), vec!["\"qf-1\"".into()]),
            ("dimension".into(), vec!["\"completeness\"".into()]),
            ("severity".into(), vec!["\"error\"".into()]),
            (
                "offending_path".into(),
                vec!["\"/records/0/amount\"".into()],
            ),
        ]),
    };

    let batch = authority
        .lifecycle_graph(&source, &BTreeSet::new(), &bounds)
        .expect("lifecycle_graph succeeds");
    let graph = batch
        .graph
        .expect("QualityCase builds a complete bounded graph");
    assert_eq!(graph.inputs.nodes.len(), 4);
    assert_eq!(graph.initial.len(), 1);
    assert_eq!(graph.edges.len(), 6);
    assert!(graph.blocked.is_empty());
    assert_eq!(
        graph.soundness().expect("soundness analysis completes"),
        [],
        "every reachable QualityCase row must have option to complete"
    );

    // Planted LIFE-SOUND trap: break resolution paths out of acknowledged/quarantined
    // into a non-terminating loop and confirm the checker flags `lifecycle.soundness.trap`.
    let mut trapped: Vec<Value> = data
        .iter()
        .map(|a| serde_json::to_value(a).expect("envelope to value"))
        .collect();
    let qc = trapped
        .iter_mut()
        .find(|v| v["artifactId"] == "data.QualityCase.entity")
        .expect("QualityCase artifact");
    qc["payload"]["objectType"]["lifecycles"][0]["transitions"] = json!([
        { "id": "acknowledge", "from": ["open", "quarantined"], "to": "acknowledged" },
        { "id": "quarantine", "from": ["open", "acknowledged"], "to": "quarantined" },
        { "id": "resolve", "from": ["open"], "to": "resolved" }
    ]);
    let trapped_envelopes: Vec<ArtifactEnvelope> = trapped
        .into_iter()
        .map(|v| serde_json::from_value(v).expect("trapped envelope"))
        .collect();
    let trap_findings = check_artifact_set(
        &trapped_envelopes,
        &EntityArtifactSetContext {
            registry: &BTreeSet::new(),
            base: &base,
            tenant: None,
        },
    );
    assert!(
        trap_findings
            .iter()
            .any(|d| d.code.as_str() == soundness_codes::TRAP),
        "planted non-terminating cycle must trigger lifecycle.soundness.trap: {trap_findings:#?}"
    );
}

#[test]
fn snapshot_record_supersedes_is_acyclic_and_rejects_subtype_weakening() {
    let base = core_base_artifacts();
    let data = data_pack_artifacts();

    let mut combined = base.clone();
    combined.extend(data.clone());
    let catalog = ScopedView::uniform(&combined, &BTreeSet::new()).into_catalog();
    let snap = catalog
        .object_type("data.SnapshotRecord.entity")
        .expect("SnapshotRecord defined")
        .expect("SnapshotRecord available");
    assert!(snap.has_links, "SnapshotRecord must carry declared links");
    let supersedes = catalog
        .links_for_object_type("data.SnapshotRecord")
        .expect("SnapshotRecord links available")
        .iter()
        .find(|link| link.id.as_str() == "supersedes")
        .expect("supersedes link present");
    assert!(
        supersedes.acyclic,
        "data.SnapshotRecord.supersedes must declare acyclic: true"
    );

    // Attempting to weaken `acyclic: true` to `acyclic: false` in a subtype must refuse.
    let mut with_weaker_sub = data;
    let weaker_subtype: ArtifactEnvelope = serde_json::from_value(json!({
        "irVersion": "0.1",
        "artifactId": "data.CyclicSnapshotRecord.entity",
        "kind": "entity",
        "namespace": "data",
        "name": "Cyclic snapshot record",
        "version": "0.1.0",
        "lifecycle": "active",
        "scope": {},
        "dependencies": [
            { "artifactId": "data.SnapshotRecord.entity", "kind": "entity", "version": "0.1.0", "required": true }
        ],
        "provenance": { "source": "authoring_sdk", "sourcePath": "tests/data_ontology.rs" },
        "payload": {
            "entityVersion": "2.0",
            "objectType": {
                "name": "data.CyclicSnapshotRecord",
                "extends": "data.SnapshotRecord",
                "role": "thing",
                "properties": [],
                "links": [
                    {
                        "id": "supersedes",
                        "target": "data.SnapshotRecord",
                        "cardinality": { "to": "one", "from": "many" },
                        "acyclic": false
                    }
                ]
            }
        }
    }))
    .expect("subtype envelope decodes");
    with_weaker_sub.push(weaker_subtype);
    let findings = check_artifact_set(
        &with_weaker_sub,
        &EntityArtifactSetContext {
            registry: &BTreeSet::new(),
            base: &base,
            tenant: None,
        },
    );
    assert!(
        findings
            .iter()
            .any(|d| d.code.as_str() == object_codes::LINK_OVERRIDE_CHANGES_ACYCLIC),
        "weakening acyclic: true on supersedes must refuse: {findings:#?}"
    );
}

#[test]
fn data_causal_1_laws_lower_evaluate_and_refuse_second_hop_and_weakening() {
    let base = core_base_artifacts();
    let data = data_pack_artifacts();

    let input_before_end =
        law_payload(&data, "data.Derivation.input_known_before_activity_end.law");
    let output_after_start = law_payload(
        &data,
        "data.Derivation.output_known_after_activity_start.law",
    );
    let monotonic_known = law_payload(&data, "data.Derivation.monotonic_knowledge_time.law");
    let distinct = law_payload(&data, "data.Derivation.distinct_snapshots.law");

    let valid = json!({
        "derivation_id": "drv-ok",
        "input_snapshot": { "snapshot_id": "snap-1", "known_at": "2026-11-01T10:00:00Z" },
        "output_snapshot": { "snapshot_id": "snap-2", "known_at": "2026-11-01T10:15:00Z" },
        "activity": { "starts_at": "2026-11-01T10:05:00Z", "ends_at": "2026-11-01T10:15:00Z" }
    });
    assert_eq!(evaluate(&input_before_end, &valid), Ok(Verdict::Holds));
    assert_eq!(evaluate(&output_after_start, &valid), Ok(Verdict::Holds));
    assert_eq!(evaluate(&monotonic_known, &valid), Ok(Verdict::Holds));
    assert_eq!(evaluate(&distinct, &valid), Ok(Verdict::Holds));

    // Future-knowledge leakage (input known after activity ends) is violated.
    let lookahead = json!({
        "derivation_id": "drv-leak",
        "input_snapshot": { "snapshot_id": "snap-future", "known_at": "2026-11-01T11:00:00Z" },
        "output_snapshot": { "snapshot_id": "snap-2", "known_at": "2026-11-01T10:15:00Z" },
        "activity": { "starts_at": "2026-11-01T10:05:00Z", "ends_at": "2026-11-01T10:15:00Z" }
    });
    assert_eq!(
        evaluate(&input_before_end, &lookahead),
        Ok(Verdict::Violated)
    );
    assert_eq!(
        evaluate(&monotonic_known, &lookahead),
        Ok(Verdict::Violated)
    );

    // Output known before activity starts is violated.
    let premature = json!({
        "derivation_id": "drv-premature",
        "input_snapshot": { "snapshot_id": "snap-1", "known_at": "2026-11-01T09:00:00Z" },
        "output_snapshot": { "snapshot_id": "snap-early", "known_at": "2026-11-01T09:50:00Z" },
        "activity": { "starts_at": "2026-11-01T10:05:00Z", "ends_at": "2026-11-01T10:15:00Z" }
    });
    assert_eq!(
        evaluate(&output_after_start, &premature),
        Ok(Verdict::Violated)
    );

    // Reflexive derivation (input_snapshot == output_snapshot) is violated.
    let reflexive = json!({
        "derivation_id": "drv-self",
        "input_snapshot": { "snapshot_id": "snap-same", "known_at": "2026-11-01T10:00:00Z" },
        "output_snapshot": { "snapshot_id": "snap-same", "known_at": "2026-11-01T10:00:00Z" },
        "activity": { "starts_at": "2026-11-01T10:05:00Z", "ends_at": "2026-11-01T10:15:00Z" }
    });
    assert_eq!(evaluate(&distinct, &reflexive), Ok(Verdict::Violated));

    // Planted LAW-LINK-1 second-hop read must refuse with law.model.unknown_binding.
    let mut second_hop: Vec<Value> = data
        .iter()
        .map(|a| serde_json::to_value(a).expect("envelope to value"))
        .collect();
    let law_json = second_hop
        .iter_mut()
        .find(|v| v["artifactId"] == "data.Derivation.distinct_snapshots.law")
        .expect("distinct_snapshots law");
    law_json["payload"]["response"]["expression"] =
        json!("self.input_snapshot.dataset.schema_uri != ''");
    let second_hop_envelopes: Vec<ArtifactEnvelope> = second_hop
        .into_iter()
        .map(|v| serde_json::from_value(v).expect("second-hop envelope"))
        .collect();
    let hop_findings = check_artifact_set(
        &second_hop_envelopes,
        &EntityArtifactSetContext {
            registry: &BTreeSet::new(),
            base: &base,
            tenant: None,
        },
    );
    assert!(
        hop_findings.iter().any(|d| {
            d.code.as_str() == law_codes::UNKNOWN_BINDING && d.message.contains("is a second hop")
        }),
        "LAW-LINK-1 must refuse second-hop link reads: {hop_findings:#?}"
    );

    // Replacing an installed DATA-CAUSAL-1 law identity on concrete DerivationLink
    // must refuse with `law.model.weakens_inherited`.
    let mut installed = base;
    installed.extend(data.clone());
    let mut replacement = serde_json::to_value(
        data.iter()
            .find(|a| a.artifact_id == "data.Derivation.input_known_before_activity_end.law")
            .expect("law exists"),
    )
    .expect("law json");
    replacement["artifactId"] = json!("data.DerivationLink.input_known_before_activity_end.law");
    replacement["dependencies"] = json!([
        { "artifactId": "data.DerivationLink.entity", "kind": "entity", "version": "0.1.0", "required": true }
    ]);
    replacement["payload"]["subject"] = json!("data.DerivationLink");
    replacement["payload"]["scope"] = json!({
        "root": { "kind": "entity", "id": "data.DerivationLink" },
        "objectTypes": ["data.DerivationLink"]
    });
    replacement["payload"]["condition"] = json!("self.derivation_id != 'exempt'");
    let replacement_env: ArtifactEnvelope =
        serde_json::from_value(replacement).expect("replacement law");
    let replace_findings = check_artifact_set(
        &[replacement_env],
        &EntityArtifactSetContext {
            registry: &BTreeSet::new(),
            base: &installed,
            tenant: None,
        },
    );
    assert!(
        replace_findings
            .iter()
            .any(|d| d.code.as_str() == law_codes::WEAKENS_INHERITED),
        "replacing an installed DATA-CAUSAL-1 law identity must refuse: {replace_findings:#?}"
    );
}
