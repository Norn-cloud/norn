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

mod support;

use std::collections::BTreeSet;
use std::path::PathBuf;

use norn_checker::{
    EntityArtifactSetContext, check_artifact_set, validate_link_registry,
    validate_ontology_registry,
};
use norn_formats::SchemaImporter;
use norn_formats::owl::OwlReader;
use norn_formats::owl_align::{AlignmentFinding, OwlAlignment, OwlAnchor, OwlHeld, align, term_of};
use norn_ir::{ArtifactEnvelope, DiagnosticSeverity, OntologySource};

const PTY: &str = "https://example.org/onto/Parties/";
const AGR: &str = "https://example.org/onto/Agreements/";
const LN: &str = "https://example.org/onto/Loans/";

fn vocabulary() -> (Vec<ArtifactEnvelope>, OntologySource) {
    let config = support::synthetic_ledger_config();
    let import = OwlReader.import(support::synthetic_ledger_graph(), &config);
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
        held: Vec::new(),
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
            | AlignmentFinding::Conflict { interface, .. }
            | AlignmentFinding::Held { interface, .. } => interface == name,
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

/// A held lineage is reported, never aligned, whatever anchors it reaches.
#[test]
fn owl_alignment_held_lineage_is_a_reported_residual() {
    let (interfaces, admission) = vocabulary();
    let mut alignment = table(true);
    alignment.held.push(OwlHeld {
        class: format!("{LN}Loan"),
        note: "only the creditor's right is a claim.".into(),
    });
    // A held class may not also be an anchor.
    let error = align(&interfaces, &admission, &alignment).unwrap_err();
    assert!(error.contains("also an anchor"), "{error}");
    alignment
        .anchors
        .retain(|row| row.class != format!("{LN}Loan"));
    let aligned = align(&interfaces, &admission, &alignment).unwrap();
    for loan in ["ex.ln.Loan", "ex.ln.SecuredLoan", "ex.ln.UnsecuredLoan"] {
        assert!(
            matches!(
                finding(&aligned.findings, loan),
                AlignmentFinding::Held { via, .. } if via == &format!("{LN}Loan")
            ),
            "{loan}"
        );
    }
    let summary = aligned.summary();
    assert_eq!(summary["held"], 3);
    let mut set = aligned.packs;
    set.push(canon());
    let coverage = validate_link_registry(&set)
        .unwrap()
        .into_iter()
        .find(|coverage| coverage.link_id == "norn.link.test-ledger.canon")
        .unwrap();
    let loan = term_of("ex.ln.Loan");
    assert!(coverage.non_conforming.contains(&loan));
    assert!(!coverage.mapped.contains_key(&loan));
    assert_eq!(
        coverage.mapped.len()
            + coverage.residual.len()
            + coverage.canon_gaps.len()
            + coverage.non_conforming.len(),
        coverage.active_terms,
        "the link is total"
    );
}

/// `class` also extends `parent` (a second lineage into the vocabulary).
fn with_parent(interfaces: &mut [ArtifactEnvelope], class: &str, parent: &str) {
    let artifact = interfaces
        .iter_mut()
        .find(|artifact| {
            serde_json::to_value(artifact).unwrap()["payload"]["interface"]["name"] == class
        })
        .unwrap();
    let mut value = serde_json::to_value(&*artifact).unwrap();
    value["payload"]["interface"]["extends"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!(parent));
    *artifact = serde_json::from_value(value).unwrap();
}

/// Holding a lineage keeps its residual but does not hide what else it
/// reaches (#1537 round 2, S3): a held descendant's anchors, and any
/// conflict between them, are reported on its finding and residual note,
/// and a held descendant may not carry an anchor of its own.
#[test]
fn owl_alignment_held_descendants_still_report_their_anchors() {
    let (mut interfaces, admission) = vocabulary();
    let mut alignment = table(false);
    alignment
        .anchors
        .retain(|row| row.class != format!("{LN}Loan"));
    alignment.held.push(OwlHeld {
        class: format!("{LN}Loan"),
        note: "only the creditor's right is a claim.".into(),
    });
    // A direct anchor below a held class is refused, not silently held.
    let mut anchored_below = alignment.clone();
    anchored_below.anchors.push(anchor(
        &format!("{LN}SecuredLoan"),
        "core.Claim",
        "norn.core.claim",
        &[],
    ));
    let error = align(&interfaces, &admission, &anchored_below).unwrap_err();
    assert!(
        error.contains("SecuredLoan") && error.contains("held"),
        "{error}"
    );
    // A secured loan is also collateral here: through that second parent it
    // reaches core.Resource beside the contract's core.Commitment.
    with_parent(&mut interfaces, "ex.ln.SecuredLoan", "ex.ln.Collateral");
    let aligned = align(&interfaces, &admission, &alignment).unwrap();
    // Read through the wire form, as the import report shows it.
    let wire = |name: &str| serde_json::to_value(finding(&aligned.findings, name)).unwrap();
    let secured = wire("ex.ln.SecuredLoan");
    assert_eq!(secured["outcome"], "held", "{secured}");
    assert_eq!(secured["via"], format!("{LN}Loan"), "{secured}");
    assert_eq!(
        secured["anchors"],
        serde_json::json!(["core.Commitment", "core.Resource"]),
        "{secured}"
    );
    let unsecured = wire("ex.ln.UnsecuredLoan");
    assert_eq!(unsecured["outcome"], "held", "{unsecured}");
    assert_eq!(
        unsecured["anchors"],
        serde_json::json!(["core.Commitment"]),
        "{unsecured}"
    );
    let summary = aligned.summary();
    assert_eq!(summary["held"], 3);
    assert_eq!(summary["heldConflicts"], 1);
    let link = serde_json::to_value(&aligned.packs[1]).unwrap();
    let note = link["payload"]["residuals"]
        .as_array()
        .unwrap()
        .iter()
        .find(|residual| residual["termId"] == term_of("ex.ln.SecuredLoan"))
        .unwrap()["note"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        note.contains("core.Commitment and core.Resource") && note.contains("disjoint"),
        "{note}"
    );
}

const CREDIT_AGREEMENT: &str =
    "https://spec.edmcouncil.org/fibo/ontology/FBC/DebtAndEquities/Debt/CreditAgreement";

/// The FIBO table claims no whole credit agreement for `core.Claim` (#1537
/// S3): only the creditor's side is a claim, so the lineage is held.
#[test]
fn owl_fibo_credit_agreement_is_held_not_a_claim() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../registry/standards/imports/edmc-fibo-2026q3.align.json");
    let table: OwlAlignment =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    assert!(
        !table
            .anchors
            .iter()
            .any(|row| row.class == CREDIT_AGREEMENT),
        "no anchor aligns the whole credit agreement"
    );
    assert!(
        table
            .anchors
            .iter()
            .all(|row| !row.supersedes.iter().any(|class| class == CREDIT_AGREEMENT)),
        "no row names it in precedence"
    );
    assert!(
        table.held.iter().any(|row| row.class == CREDIT_AGREEMENT),
        "the lineage is held as a reported residual"
    );
}

#[test]
fn owl_align_rejects_supersedes_cycles_and_preserves_narrowed_to_one_shape() {
    let make_iface = |name: &str, iri: &str, extends: &[&str], to: Option<&str>| {
        let links = to.map_or_else(Vec::new, |card| {
            vec![serde_json::json!({
                "id": "owner",
                "target": "ex.Party",
                "cardinality": { "to": card, "required": false }
            })]
        });
        let (ns, local) = name.rsplit_once('.').unwrap();
        serde_json::from_value(serde_json::json!({
            "irVersion": "0.1",
            "artifactId": format!("{name}.interface"),
            "kind": "entity",
            "namespace": ns,
            "name": local,
            "version": "0.1.0",
            "lifecycle": "draft",
            "scope": {},
            "dependencies": [],
            "provenance": { "source": "import", "sourcePath": iri },
            "payload": {
                "entityVersion": "2.0",
                "interface": {
                    "name": name,
                    "extends": extends,
                    "properties": [],
                    "links": links
                }
            }
        }))
        .unwrap()
    };
    let interfaces = vec![
        make_iface("ex.Party", "https://example.org/Party", &[], None),
        make_iface("ex.P1", "https://example.org/P1", &[], Some("many")),
        make_iface("ex.P2", "https://example.org/P2", &[], Some("one")),
        make_iface(
            "ex.Child",
            "https://example.org/Child",
            &["ex.P1", "ex.P2"],
            None,
        ),
    ];
    let admission = support::synthetic_ledger_config().admission;
    let mut cyclic = table(false);
    cyclic.anchors = vec![
        anchor(
            "https://example.org/P1",
            "core.Agent",
            "norn.core.agent",
            &["https://example.org/P2"],
        ),
        anchor(
            "https://example.org/P2",
            "core.Claim",
            "norn.core.claim",
            &["https://example.org/P1"],
        ),
    ];
    let err = align(&interfaces, &admission, &cyclic).unwrap_err();
    assert!(err.contains("cycle"), "{err}");

    let mut valid = table(false);
    valid.anchors = vec![anchor(
        "https://example.org/P1",
        "core.Agent",
        "norn.core.agent",
        &[],
    )];
    let aligned = align(&interfaces, &admission, &valid).unwrap();
    let pack = serde_json::to_value(&aligned.packs[0]).unwrap();
    let child_term = term_of("ex.Child");
    let child_shape = pack["payload"]["shapes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|shape| shape["termId"] == child_term)
        .expect("child shape");
    assert!(
        child_shape["fields"]
            .as_array()
            .unwrap()
            .iter()
            .any(|field| field["fieldId"] == "owner"),
        "narrowed to_one member from P2 must not be shadowed by P1's to_many: {child_shape}"
    );
}
