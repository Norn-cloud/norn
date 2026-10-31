//! The consistency engine over the real system packs and links
//! (`registry/packs`): the lowering is what the packs declare, and every
//! obligation runs to completion on every link.

use std::fs;
use std::path::PathBuf;

use norn_checker::consistency::{
    ChaseBudget, Dependency, check_pack_link, codes, lower_pack_links,
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
            serde_json::from_str(&fs::read_to_string(path).expect("readable")).expect("envelope")
        })
        .collect()
}

fn ids(dependencies: &[Dependency]) -> Vec<&str> {
    dependencies.iter().map(Dependency::id).collect()
}

#[test]
fn every_link_lowers_and_every_obligation_runs() {
    let links = lower_pack_links(&artifacts()).expect("packs lower");
    let names: Vec<&str> = links.iter().map(|l| l.link_id.as_str()).collect();
    assert_eq!(
        names,
        [
            "norn.link.erpnext.canon",
            "norn.link.odoo.canon",
            "norn.link.shopify.canon"
        ]
    );
    for link in &links {
        let findings = check_pack_link(link, ChaseBudget::default());
        for finding in &findings {
            eprintln!("{} {} {}", link.link_id, finding.code, finding.message);
        }
        eprintln!("{} not lowered: {:?}", link.link_id, link.not_lowered);
        let refused: Vec<_> = findings
            .iter()
            .filter(|f| {
                [
                    codes::MALFORMED,
                    codes::NOT_WEAKLY_ACYCLIC,
                    codes::CHASE_REFUSED,
                ]
                .contains(&f.code.as_str())
            })
            .collect();
        assert!(refused.is_empty(), "{}: {refused:?}", link.link_id);
    }
}

#[test]
fn the_lowering_is_what_the_packs_declare() {
    let links = lower_pack_links(&artifacts()).expect("packs lower");
    let erpnext = &links[0];
    let target = ids(&erpnext.target.dependencies);
    // A shape field identified with its property term, and one that is not.
    assert!(
        target.contains(&"required:norn.finance.invoice.party"),
        "{target:?}"
    );
    assert!(
        target.contains(&"required:norn.finance.invoice#total"),
        "{target:?}"
    );
    assert!(
        target.contains(&"range:norn.finance.invoice.party"),
        "{target:?}"
    );
    // The canon hierarchy is a derivation, so it is chased with the mapping.
    let mapping = ids(&erpnext.mapping.dependencies);
    assert!(
        mapping.contains(&"parent:norn.finance.invoice"),
        "{mapping:?}"
    );
    assert!(
        mapping.iter().any(|id| id.starts_with("link:")),
        "{mapping:?}"
    );
    // `derived` correspondences are reported, never lowered.
    assert!(
        erpnext.not_lowered.iter().any(|n| n.contains("derived")),
        "{:?}",
        erpnext.not_lowered
    );
    assert!(
        erpnext
            .gaps
            .contains("norn.erpnext.sales.invoice.outstanding.amount")
    );
}
