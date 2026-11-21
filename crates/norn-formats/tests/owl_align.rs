#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic,
    missing_docs,
    reason = "integration tests"
)]
//! Alignment of the synthetic ledger ontology to the REA core and canon:
//! every class is aligned, a gap or a conflict; the checker admits both
//! packs and proves the link total.

use std::collections::BTreeSet;
use std::path::PathBuf;

use norn_checker::{
    EntityArtifactSetContext, check_artifact_set, validate_link_registry,
    validate_ontology_registry,
};
use norn_formats::SchemaImporter;
use norn_formats::contract::sha256_hex;
use norn_formats::owl::{OwlConfig, OwlGraph, OwlNamespace, OwlNormaliser, OwlReader};
use norn_formats::owl_align::{AlignmentFinding, OwlAlignment, OwlAnchor, align, term_of};
use norn_ir::{ArtifactEnvelope, DiagnosticSeverity, OntologySource, OntologySourceFile};

const GRAPH: &[u8] = include_bytes!("fixtures/synthetic/ledger.nt");
const ORIGINAL: &[u8] = include_bytes!("fixtures/synthetic/ledger.ttl");
const PTY: &str = "https://example.org/onto/Parties/";
const AGR: &str = "https://example.org/onto/Agreements/";
const LN: &str = "https://example.org/onto/Loans/";

fn config() -> OwlConfig {
    let mut admission =
        serde_json::from_str::<OntologySource>(include_str!("fixtures/synthetic/source.json"))
            .unwrap();
    admission.source_id = "source.test.synthetic-ledger".into();
    admission.license = "MIT".into();
    admission.importer = Some(norn_ir::SourceImporter {
        name: "norn.formats.owl".into(),
        version: "0.1.0".into(),
    });
    admission.files = Some(vec![OntologySourceFile {
        path: "https://example.test/synthetic-ledger/1.0/ledger.ttl".into(),
        sha256: sha256_hex(ORIGINAL),
    }]);
    let namespace = |iri: &str, prefix: &str| OwlNamespace {
        iri: iri.into(),
        prefix: prefix.into(),
    };
    OwlConfig {
        admission,
        observed_at: "2026-11-20T00:00:00Z".into(),
        graph_sha256: sha256_hex(GRAPH),
        normaliser: OwlNormaliser {
            name: "rdflib".into(),
            version: "7.6.0".into(),
        },
        namespaces: vec![
            namespace(PTY, "ex-pty"),
            namespace(AGR, "ex-agr"),
            namespace(LN, "ex-ln"),
        ],
    }
}

fn vocabulary() -> (Vec<ArtifactEnvelope>, OntologySource) {
    let config = config();
    let import = OwlReader.import(
        OwlGraph {
            path: "ledger.nt",
            bytes: GRAPH,
        },
        &config,
    );
    (import.layers[0].artifacts.clone(), config.admission)
}

fn anchor(class: &str, interface: &str, term: &str, supersedes: &[&str]) -> OwlAnchor {
    OwlAnchor {
        class: class.into(),
        interface: interface.into(),
        term: term.into(),
        justification: format!("{class} is a {interface}."),
        supersedes: supersedes.iter().map(|class| (*class).to_owned()).collect(),
    }
}

/// Loans are both contracts and claims unless the claim row supersedes the
/// commitment row.
fn table(precedence: bool) -> OwlAlignment {
    let contract = format!("{AGR}Contract");
    let supersedes: &[&str] = if precedence {
        &[contract.as_str()]
    } else {
        &[]
    };
    OwlAlignment {
        pack_id: "norn.ontology-pack.test-ledger".into(),
        link_id: "norn.link.test-ledger.canon".into(),
        canon_pack: "norn.ontology-pack.canon".into(),
        canon_version: "0.1.0".into(),
        reviewed_by: "test".into(),
        reviewed_at: "2026-11-20T00:00:00Z".into(),
        anchors: vec![
            anchor(&format!("{PTY}Party"), "core.Agent", "norn.core.agent", &[]),
            anchor(&contract, "core.Commitment", "norn.core.commitment", &[]),
            anchor(
                &format!("{LN}Loan"),
                "core.Claim",
                "norn.core.claim",
                supersedes,
            ),
            anchor(
                &format!("{LN}Collateral"),
                "core.Resource",
                "norn.core.resource.type",
                &[],
            ),
        ],
    }
}

fn canon() -> ArtifactEnvelope {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../registry/packs/canon/artifacts/norn.ontology-pack.canon.json");
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn finding<'a>(findings: &'a [AlignmentFinding], name: &str) -> &'a AlignmentFinding {
    findings
        .iter()
        .find(|finding| match finding {
            AlignmentFinding::Aligned { interface, .. }
            | AlignmentFinding::Gap { interface }
            | AlignmentFinding::Conflict { interface, .. } => interface == name,
        })
        .unwrap_or_else(|| panic!("no finding for {name}"))
}

#[test]
fn owl_alignment_classifies_every_class_by_its_lineage() {
    let (interfaces, admission) = vocabulary();
    let aligned = align(&interfaces, &admission, &table(false)).unwrap();
    assert_eq!(aligned.findings.len(), interfaces.len());
    assert!(matches!(
        finding(&aligned.findings, "ex.pty.Corporation"),
        AlignmentFinding::Aligned { anchor, .. } if anchor == "core.Agent"
    ));
    assert!(matches!(
        finding(&aligned.findings, "ex.agr.Agreement"),
        AlignmentFinding::Gap { .. }
    ));
    for loan in ["ex.ln.Loan", "ex.ln.SecuredLoan", "ex.ln.UnsecuredLoan"] {
        assert!(
            matches!(
                finding(&aligned.findings, loan),
                AlignmentFinding::Conflict { anchors, .. }
                    if anchors == &["core.Claim".to_owned(), "core.Commitment".to_owned()]
            ),
            "{loan}"
        );
    }
    // A reviewed precedence resolves the conflict and names what it sets aside.
    let aligned = align(&interfaces, &admission, &table(true)).unwrap();
    assert!(matches!(
        finding(&aligned.findings, "ex.ln.SecuredLoan"),
        AlignmentFinding::Aligned { anchor, superseded, .. }
            if anchor == "core.Claim" && superseded == &["core.Commitment".to_owned()]
    ));
    assert!(matches!(
        finding(&aligned.findings, "ex.agr.Contract"),
        AlignmentFinding::Aligned { anchor, .. } if anchor == "core.Commitment"
    ));
    let summary = aligned.summary();
    assert_eq!(summary["classes"], interfaces.len());
    assert_eq!(summary["nonConforming"], 0);
}

#[test]
fn owl_alignment_packs_are_admitted_and_the_link_is_total() {
    let (interfaces, admission) = vocabulary();
    for precedence in [false, true] {
        let aligned = align(&interfaces, &admission, &table(precedence)).unwrap();
        let mut set = aligned.packs.clone();
        set.push(canon());
        let diagnostics = check_artifact_set(
            &aligned.packs,
            &EntityArtifactSetContext {
                registry: &BTreeSet::new(),
                base: &[],
                tenant: None,
            },
        );
        assert!(
            diagnostics
                .iter()
                .all(|d| d.severity != DiagnosticSeverity::Error),
            "{diagnostics:#?}"
        );
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
        validate_ontology_registry(&payloads).unwrap();
        let coverage = validate_link_registry(&set)
            .unwrap()
            .into_iter()
            .find(|coverage| coverage.link_id == "norn.link.test-ledger.canon")
            .unwrap();
        let accounted = coverage.mapped.len()
            + coverage.residual.len()
            + coverage.canon_gaps.len()
            + coverage.non_conforming.len();
        assert_eq!(coverage.active_terms, interfaces.len());
        assert_eq!(accounted, coverage.active_terms, "the link is total");
        let loan = term_of("ex.ln.Loan");
        assert_eq!(
            coverage.non_conforming.contains(&loan),
            !precedence,
            "{precedence}"
        );
        assert_eq!(
            coverage.mapped.contains_key(&loan),
            precedence,
            "{precedence}"
        );
    }
}

#[test]
fn owl_alignment_refuses_an_anchor_that_was_not_imported() {
    let (interfaces, admission) = vocabulary();
    let mut alignment = table(false);
    alignment.anchors.push(anchor(
        "https://example.org/onto/Elsewhere/Thing",
        "core.Agent",
        "norn.core.agent",
        &[],
    ));
    let error = align(&interfaces, &admission, &alignment).unwrap_err();
    assert!(error.contains("not an imported class"), "{error}");
    let mut alignment = table(false);
    alignment.anchors[0].supersedes = vec![format!("{LN}Facility")];
    let error = align(&interfaces, &admission, &alignment).unwrap_err();
    assert!(error.contains("which is not an anchor"), "{error}");
}
