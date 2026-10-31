#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::pedantic,
    missing_docs,
    reason = "registry contract tests use fixture-oriented helpers"
)]

//! The generated registry packs (`registry/packs/*/artifacts`) validate as
//! one ontology registry, and every link resolves and accounts for its
//! source pack's terms.

use std::fs;
use std::path::PathBuf;

use norn_checker::{
    ArtifactValidation, LinkCoverage, validate_link_registry, validate_ontology_registry,
};
use norn_ir::ArtifactEnvelope;

fn artifacts() -> Vec<ArtifactEnvelope> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../registry/packs");
    let mut paths: Vec<PathBuf> = fs::read_dir(&root)
        .expect("registry/packs exists")
        .flat_map(|pack| {
            fs::read_dir(pack.expect("entry").path().join("artifacts"))
                .into_iter()
                .flatten()
        })
        .map(|entry| entry.expect("artifact entry").path())
        .collect();
    paths.sort();
    paths
        .iter()
        .map(|path| {
            let text = fs::read_to_string(path).expect("artifact is readable");
            let artifact: ArtifactEnvelope =
                serde_json::from_str(&text).expect("artifact envelope");
            let validated = artifact.validate();
            assert!(validated.is_ok(), "{}: {validated:#?}", path.display());
            artifact
        })
        .collect()
}

fn coverage(link: &str) -> LinkCoverage {
    validate_link_registry(&artifacts())
        .expect("every link resolves")
        .into_iter()
        .find(|coverage| coverage.link_id == link)
        .expect("link is in the registry")
}

#[test]
fn registry_packs_form_one_ontology_registry() {
    let packs: Vec<_> = artifacts()
        .iter()
        .map(|artifact| artifact.payload.to_json_value())
        .filter(|payload| {
            payload
                .as_object()
                .and_then(|p| p.get("packKind"))
                .and_then(|k| k.as_string())
                == Some("ontology")
        })
        .collect();
    assert!(packs.len() >= 2);
    validate_ontology_registry(&packs).expect("terms are globally unique and resolve");
}

#[test]
fn erpnext_link_accounts_for_every_term() {
    let coverage = coverage("norn.link.erpnext.canon");
    let accounted = coverage.mapped.len() + coverage.residual.len() + coverage.canon_gaps.len();
    assert_eq!(accounted, coverage.active_terms);
    assert!(
        coverage
            .canon_gaps
            .contains("norn.erpnext.item.valuation.method")
    );
}
