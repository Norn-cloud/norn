#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::print_stdout,
    clippy::pedantic,
    missing_docs,
    reason = "measurement harness"
)]
//! Admission at ontology scale (#1512, #1417): one measurement per process,
//! so the process's peak resident set is the measured set's.
//!
//! Ignored in CI: it reads an OWL import written by `norn-format-read
//! --emit-dir <dir> --align <table>` (the originals are fetched by pin,
//! never committed). `scripts/bench_ontology_admission.py` runs it once per
//! set, in release mode, and prints one JSON line per run:
//!
//! - `NORN_OWL_SCALE_SET=owl:<fraction>`: the vocabulary layer, cut to the
//!   first `fraction` of its interfaces by name and closed under `extends`
//!   and link targets, admitted by `check_artifact_set` as a registry layer;
//! - `owl-packs`: the ontology and link packs admitted, then the ontology
//!   registry and the link registry (totality) validated with the canon;
//! - `canon`: today's canon (`registry/packs/canon` over `registry/core`);
//! - `three-systems`: the canon with the ERPNext, Shopify and Odoo packs.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

use norn_checker::{
    EntityArtifactSetContext, check_artifact_set, validate_link_registry,
    validate_ontology_registry,
};
use norn_ir::{ArtifactEnvelope, Diagnostic, DiagnosticSeverity};
use serde_json::{Value, json};

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read_array(path: &Path) -> Vec<ArtifactEnvelope> {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display())))
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn read_dir(relative: &str) -> Vec<ArtifactEnvelope> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(repo().join(relative))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|e| e == "json"))
        .collect();
    paths.sort();
    paths
        .iter()
        .map(|path| serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap())
        .collect()
}

/// Peak resident set of this process, in KiB (Linux).
fn peak_kib() -> Option<u64> {
    std::fs::read_to_string("/proc/self/status")
        .ok()?
        .lines()
        .find_map(|line| line.strip_prefix("VmHWM:"))
        .and_then(|rest| rest.trim().trim_end_matches("kB").trim().parse().ok())
}

fn errors(diagnostics: &[Diagnostic]) -> Vec<String> {
    diagnostics
        .iter()
        .filter(|d| d.severity == DiagnosticSeverity::Error)
        .map(|d| format!("{}: {}", d.code, d.message))
        .collect()
}

fn admit(artifacts: &[ArtifactEnvelope], base: &[ArtifactEnvelope]) -> (u128, Vec<String>) {
    let registry = BTreeSet::new();
    let started = Instant::now();
    let diagnostics = check_artifact_set(
        artifacts,
        &EntityArtifactSetContext {
            registry: &registry,
            base,
            tenant: None,
        },
    );
    (started.elapsed().as_millis(), errors(&diagnostics))
}

/// Interface name → (artifact, the interface names it references).
fn references(vocabulary: &[ArtifactEnvelope]) -> BTreeMap<String, (ArtifactEnvelope, Vec<String>)> {
    vocabulary
        .iter()
        .map(|artifact| {
            let value = serde_json::to_value(artifact).unwrap();
            let interface = &value["payload"]["interface"];
            let mut names: Vec<String> = interface["extends"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect();
            names.extend(
                interface["links"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|link| link["target"].as_str().map(str::to_owned)),
            );
            (
                interface["name"].as_str().unwrap().to_owned(),
                (artifact.clone(), names),
            )
        })
        .collect()
}

fn members(artifacts: &[ArtifactEnvelope], field: &str) -> usize {
    artifacts
        .iter()
        .map(|artifact| {
            serde_json::to_value(artifact).unwrap()["payload"]["interface"][field]
                .as_array()
                .map_or(0, Vec::len)
        })
        .sum()
}

/// The first `fraction` of the interfaces by name, closed under references.
fn cut(vocabulary: &[ArtifactEnvelope], fraction: f64) -> Vec<ArtifactEnvelope> {
    let by_name = references(vocabulary);
    let take = ((by_name.len() as f64) * fraction).ceil() as usize;
    let mut chosen = BTreeSet::new();
    let mut stack: Vec<&str> = by_name.keys().take(take).map(String::as_str).collect();
    while let Some(name) = stack.pop() {
        if let Some((_, refs)) = by_name.get(name)
            && chosen.insert(name)
        {
            stack.extend(refs.iter().map(String::as_str));
        }
    }
    chosen.iter().map(|name| by_name[*name].0.clone()).collect()
}

fn emit_dir() -> PathBuf {
    PathBuf::from(std::env::var("NORN_OWL_EMIT_DIR").expect("NORN_OWL_EMIT_DIR: an owl --emit-dir"))
}

#[test]
#[ignore = "reads an import fetched by pin; run by scripts/bench_ontology_admission.py"]
fn owl_scale_admission() {
    let set = std::env::var("NORN_OWL_SCALE_SET").unwrap_or_else(|_| "owl:1".into());
    let started = Instant::now();
    let mut record = json!({ "set": set });
    match set.split_once(':').map_or((set.as_str(), ""), |(a, b)| (a, b)) {
        ("owl", fraction) => {
            let vocabulary = read_array(&emit_dir().join("artifacts.json"));
            let fraction: f64 = fraction.parse().expect("owl:<fraction>");
            let subset = cut(&vocabulary, fraction);
            let (millis, errors) = admit(&subset, &[]);
            record["artifacts"] = json!(subset.len());
            record["extends"] = json!(members(&subset, "extends"));
            record["links"] = json!(members(&subset, "links"));
            record["properties"] = json!(members(&subset, "properties"));
            record["admitMillis"] = json!(millis);
            record["errors"] = json!(errors.len());
            record["firstErrors"] = json!(errors.iter().take(5).collect::<Vec<_>>());
        }
        ("owl-packs", _) => {
            let packs = read_array(&emit_dir().join("packs.json"));
            let canon = read_dir("registry/packs/canon/artifacts");
            let (admit_millis, admit_errors) = admit(&packs, &[]);
            let mut set = packs.clone();
            set.extend(canon.iter().filter(|a| a.artifact_id == "norn.ontology-pack.canon").cloned());
            let payloads: Vec<_> = set
                .iter()
                .map(|artifact| artifact.payload.to_json_value())
                .filter(|payload| {
                    payload
                        .as_object()
                        .and_then(|object| object.get("packKind"))
                        .and_then(|kind| kind.as_string())
                        == Some("ontology")
                })
                .collect();
            let started = Instant::now();
            let registry = validate_ontology_registry(&payloads);
            let registry_millis = started.elapsed().as_millis();
            let started = Instant::now();
            let links = validate_link_registry(&set);
            let link_millis = started.elapsed().as_millis();
            record["admitMillis"] = json!(admit_millis);
            record["errors"] = json!(admit_errors.len());
            record["firstErrors"] = json!(admit_errors.iter().take(5).collect::<Vec<_>>());
            record["ontologyRegistryMillis"] = json!(registry_millis);
            record["ontologyRegistry"] = match registry {
                Ok(()) => json!("valid"),
                Err(diagnostics) => json!(errors(&diagnostics).into_iter().take(5).collect::<Vec<_>>()),
            };
            record["linkRegistryMillis"] = json!(link_millis);
            record["linkRegistry"] = match links {
                Ok(coverages) => Value::Array(
                    coverages
                        .iter()
                        .map(|c| {
                            json!({
                                "link": c.link_id,
                                "activeTerms": c.active_terms,
                                "mapped": c.mapped.len(),
                                "residual": c.residual.len(),
                                "canonGaps": c.canon_gaps.len(),
                                "nonConforming": c.non_conforming.len(),
                                "total": c.mapped.len() + c.residual.len() + c.canon_gaps.len() + c.non_conforming.len() == c.active_terms,
                            })
                        })
                        .collect(),
                ),
                Err(diagnostics) => json!(errors(&diagnostics).into_iter().take(5).collect::<Vec<_>>()),
            };
        }
        (baseline @ ("canon" | "three-systems"), _) => {
            let mut dirs = vec!["registry/core/artifacts", "registry/packs/canon/artifacts"];
            if baseline == "three-systems" {
                dirs.extend([
                    "registry/packs/erpnext/artifacts",
                    "registry/packs/shopify/artifacts",
                    "registry/packs/odoo/artifacts",
                ]);
            }
            let all: Vec<ArtifactEnvelope> = dirs.iter().flat_map(|dir| read_dir(dir)).collect();
            let (base, candidate): (Vec<_>, Vec<_>) = all
                .into_iter()
                .partition(|a| a.artifact_id.starts_with("core.") || a.artifact_id.starts_with("std."));
            let (millis, errors) = admit(&candidate, &base);
            record["artifacts"] = json!(candidate.len());
            record["base"] = json!(base.len());
            record["admitMillis"] = json!(millis);
            record["errors"] = json!(errors.len());
            record["firstErrors"] = json!(errors.iter().take(5).collect::<Vec<_>>());
        }
        (other, _) => panic!("unknown NORN_OWL_SCALE_SET {other}"),
    }
    record["totalMillis"] = json!(started.elapsed().as_millis());
    record["peakKiB"] = json!(peak_kib());
    println!("NORN_OWL_SCALE {record}");
}
