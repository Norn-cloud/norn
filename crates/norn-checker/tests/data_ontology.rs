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
use std::path::{Path, PathBuf};

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

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn load_envelope(path: &Path) -> TestResult<ArtifactEnvelope> {
    let text = fs::read_to_string(path)?;
    let envelope: ArtifactEnvelope = serde_json::from_str(&text)?;
    let validated = envelope.validate();
    assert!(validated.is_ok(), "{}: {validated:#?}", path.display());
    Ok(envelope)
}

fn core_base_artifacts() -> TestResult<Vec<ArtifactEnvelope>> {
    let core = repo_root().join("registry/core/artifacts");
    let mut base = Vec::new();
    for name in [
        "core.Agent.interface.json",
        "core.TimeInterval.interface.json",
        "core.value_types.entity.json",
        "core.time.json",
    ] {
        base.push(load_envelope(&core.join(name))?);
    }
    base.push(load_envelope(
        &repo_root().join("registry/privacy/artifacts/privacy.markings.entity.json"),
    )?);
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
    }))?;
    assert!(
        extra_markings.validate().is_ok(),
        "extra markings envelope validates"
    );
    base.push(extra_markings);
    Ok(base)
}

fn data_pack_artifacts() -> TestResult<Vec<ArtifactEnvelope>> {
    let dir = repo_root().join("registry/packs/data/artifacts");
    let mut paths: Vec<PathBuf> = fs::read_dir(&dir)?
        .map(|entry| entry.map(|item| item.path()))
        .collect::<Result<_, _>>()?;
    paths.sort();
    assert!(
        !paths.is_empty(),
        "data pack must contain compiled artifacts"
    );
    paths.iter().map(|path| load_envelope(path)).collect()
}

fn law_payload(artifacts: &[ArtifactEnvelope], artifact_id: &str) -> TestResult<LawIR> {
    let artifact = artifacts
        .iter()
        .find(|a| a.artifact_id == artifact_id)
        .ok_or("expected law artifact in data pack")?;
    Ok(artifact.payload.to_json_value().decode()?)
}

#[test]
fn data_pack_admits_cleanly_over_core_base() -> TestResult {
    let base = core_base_artifacts()?;
    let data = data_pack_artifacts()?;
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
    Ok(())
}

#[test]
fn quality_case_satisfies_life_sound_and_proof_lifecycle_graph() -> TestResult {
    let base = core_base_artifacts()?;
    let data = data_pack_artifacts()?;

    let mut combined = base.clone();
    combined.extend(data.clone());
    let catalog = ScopedView::uniform(&combined, &BTreeSet::new()).into_catalog();
    let quality_case = catalog
        .object_type("data.QualityCase.entity")
        .ok_or("QualityCase defined")?
        .map_err(|err| format!("QualityCase available: {err:?}"))?;
    let status = quality_case
        .lifecycle("status")
        .ok_or("QualityCase declares status lifecycle")?;
    assert_eq!(status.lifecycle.initial, ["open"]);
    assert_eq!(status.lifecycle.terminal, ["resolved"]);
    assert_eq!(status.lifecycle.immutable, ["resolved"]);
    assert_eq!(status.lifecycle.endings.len(), 1);
    let ending = status.lifecycle.endings.first().ok_or("missing ending")?;
    assert_eq!(ending.state, "resolved");
    assert_eq!(ending.outcome, LifecycleOutcome::Completed);

    // PROOF-LIFECYCLE (lifecycle_graph) + LIFE-SOUND-4 over pinned frozen bounds.
    let source_json = String::from_utf8(norn_canonical::canonical_json_exact_of(&data)?)?;
    let base_json = String::from_utf8(norn_canonical::canonical_json_exact_of(&base)?)?;
    let source = DefinitionSource::parse(&source_json, &base_json)
        .map_err(|err| format!("definition source: {err:?}"))?;
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
    .map_err(|err| format!("admitted data pack approves: {err:?}"))?;

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
        .map_err(|err| format!("lifecycle_graph: {err:?}"))?;
    let graph = batch
        .graph
        .ok_or("QualityCase builds a complete bounded graph")?;
    assert_eq!(graph.inputs.nodes.len(), 4);
    assert_eq!(graph.initial.len(), 1);
    assert_eq!(graph.edges.len(), 6);
    assert!(graph.blocked.is_empty());
    let soundness = graph
        .soundness()
        .map_err(|err| format!("soundness analysis: {err:?}"))?;
    assert_eq!(
        soundness,
        [],
        "every reachable QualityCase row must have option to complete"
    );
    Ok(())
}

#[test]
fn quality_case_planted_trap_triggers_soundness_refusal() -> TestResult {
    let base = core_base_artifacts()?;
    let data = data_pack_artifacts()?;

    // Planted LIFE-SOUND trap: break resolution paths out of acknowledged/quarantined
    // into a non-terminating loop and confirm the checker flags `lifecycle.soundness.trap`.
    let mut trapped: Vec<Value> = data
        .iter()
        .map(serde_json::to_value)
        .collect::<Result<_, _>>()?;
    let qc = trapped
        .iter_mut()
        .find(|v| v.get("artifactId").and_then(Value::as_str) == Some("data.QualityCase.entity"))
        .ok_or("QualityCase artifact")?;
    let lifecycle = qc
        .get_mut("payload")
        .and_then(|v| v.get_mut("objectType"))
        .and_then(|v| v.get_mut("lifecycles"))
        .and_then(Value::as_array_mut)
        .and_then(|arr| arr.first_mut())
        .and_then(Value::as_object_mut)
        .ok_or("QualityCase status lifecycle object")?;
    lifecycle.insert(
        "transitions".to_owned(),
        json!([
            { "id": "acknowledge", "from": ["open", "quarantined"], "to": "acknowledged" },
            { "id": "quarantine", "from": ["open", "acknowledged"], "to": "quarantined" },
            { "id": "resolve", "from": ["open"], "to": "resolved" }
        ]),
    );
    let trapped_envelopes: Vec<ArtifactEnvelope> = trapped
        .into_iter()
        .map(serde_json::from_value)
        .collect::<Result<_, _>>()?;
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
    Ok(())
}

#[test]
fn snapshot_record_supersedes_is_acyclic_and_rejects_subtype_weakening() -> TestResult {
    let base = core_base_artifacts()?;
    let data = data_pack_artifacts()?;

    let mut combined = base.clone();
    combined.extend(data.clone());
    let catalog = ScopedView::uniform(&combined, &BTreeSet::new()).into_catalog();
    let snap = catalog
        .object_type("data.SnapshotRecord.entity")
        .ok_or("SnapshotRecord defined")?
        .map_err(|err| format!("SnapshotRecord available: {err:?}"))?;
    assert!(snap.has_links, "SnapshotRecord must carry declared links");
    let supersedes = catalog
        .links_for_object_type("data.SnapshotRecord")
        .ok_or("SnapshotRecord links available")?
        .iter()
        .find(|link| link.id.as_str() == "supersedes")
        .ok_or("supersedes link present")?;
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
    }))?;
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
    Ok(())
}

#[test]
fn data_causal_1_laws_evaluate_valuations() -> TestResult {
    let data = data_pack_artifacts()?;

    let input_before_end =
        law_payload(&data, "data.Derivation.input_known_before_activity_end.law")?;
    let output_after_start = law_payload(
        &data,
        "data.Derivation.output_known_after_activity_start.law",
    )?;
    let monotonic_known = law_payload(&data, "data.Derivation.monotonic_knowledge_time.law")?;
    let distinct = law_payload(&data, "data.Derivation.distinct_snapshots.law")?;

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
    Ok(())
}

#[test]
fn data_causal_1_laws_refuse_second_hop_and_weakening() -> TestResult {
    let base = core_base_artifacts()?;
    let data = data_pack_artifacts()?;

    // Planted LAW-LINK-1 second-hop read must refuse with law.model.unknown_binding.
    let mut second_hop: Vec<Value> = data
        .iter()
        .map(serde_json::to_value)
        .collect::<Result<_, _>>()?;
    let law_json = second_hop
        .iter_mut()
        .find(|v| {
            v.get("artifactId").and_then(Value::as_str)
                == Some("data.Derivation.distinct_snapshots.law")
        })
        .ok_or("distinct_snapshots law")?;
    let response = law_json
        .get_mut("payload")
        .and_then(|v| v.get_mut("response"))
        .and_then(Value::as_object_mut)
        .ok_or("law response object")?;
    response.insert(
        "expression".to_owned(),
        json!("self.input_snapshot.dataset.schema_uri != ''"),
    );
    let second_hop_envelopes: Vec<ArtifactEnvelope> = second_hop
        .into_iter()
        .map(serde_json::from_value)
        .collect::<Result<_, _>>()?;
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
            .ok_or("law exists")?,
    )?;
    let root_obj = replacement.as_object_mut().ok_or("replacement object")?;
    root_obj.insert(
        "artifactId".to_owned(),
        json!("data.DerivationLink.input_known_before_activity_end.law"),
    );
    root_obj.insert(
        "dependencies".to_owned(),
        json!([
            { "artifactId": "data.DerivationLink.entity", "kind": "entity", "version": "0.1.0", "required": true }
        ]),
    );
    let payload_obj = root_obj
        .get_mut("payload")
        .and_then(Value::as_object_mut)
        .ok_or("replacement payload")?;
    payload_obj.insert("subject".to_owned(), json!("data.DerivationLink"));
    payload_obj.insert(
        "scope".to_owned(),
        json!({
            "root": { "kind": "entity", "id": "data.DerivationLink" },
            "objectTypes": ["data.DerivationLink"]
        }),
    );
    payload_obj.insert(
        "condition".to_owned(),
        json!("self.derivation_id != 'exempt'"),
    );
    let replacement_env: ArtifactEnvelope = serde_json::from_value(replacement)?;
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
    Ok(())
}
