#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic,
    missing_docs,
    reason = "integration-test crate"
)]
#![deny(
    clippy::match_wildcard_for_single_variants,
    clippy::format_collect,
    clippy::format_push_string,
    clippy::inefficient_to_string,
    clippy::large_types_passed_by_value,
    clippy::needless_pass_by_value,
    clippy::large_futures
)]

//! The technical-view vocabulary (T12-VIEW-1, #2717) checks clean as ordinary
//! `EntityIR` object types, together with business types that link to it
//! through `backed_by` and `derived_from`; a lineage link to an undeclared
//! technical type is the ordinary unknown-target diagnostic.

use std::collections::BTreeSet;

use norn_checker::{Diagnostic, check_object_type_defs, object_types::codes};
use norn_ir::{ObjectTypeDef, PropertyTypeName};

const TECHNICAL: &str =
    include_str!("../../../fixtures/ir/entity-v2/technical-view/technical-view.object-types.json");
const BUSINESS: &str =
    include_str!("../../../fixtures/ir/entity-v2/technical-view/business.object-types.json");

fn decode(source: &str) -> Vec<ObjectTypeDef> {
    serde_json::from_str(source).expect("fixture decodes as object types")
}

fn codes_of(defs: &[ObjectTypeDef]) -> Vec<(String, String)> {
    let registry: BTreeSet<PropertyTypeName> = BTreeSet::new();
    check_object_type_defs(defs, &[], &registry)
        .into_iter()
        .map(|Diagnostic { code, span, .. }| {
            (
                code.as_str().to_owned(),
                span.map(|span| span.pointer).unwrap_or_default(),
            )
        })
        .collect()
}

#[test]
fn technical_view_types_check_clean_on_their_own() {
    assert_eq!(codes_of(&decode(TECHNICAL)), Vec::new());
}

#[test]
fn technical_view_with_business_types_checks_clean() {
    let mut defs = decode(TECHNICAL);
    defs.extend(decode(BUSINESS));
    assert_eq!(codes_of(&defs), Vec::new());
}

#[test]
fn technical_view_lineage_link_needs_a_declared_target() {
    // Business types without the technical vocabulary: every backed_by and
    // derived_from link names an unknown target.
    let findings = codes_of(&decode(BUSINESS));

    assert_eq!(
        findings
            .iter()
            .filter(|(code, _)| code == codes::UNKNOWN_LINK_TARGET)
            .count(),
        4,
        "two business types x two lineage links: {findings:?}"
    );
}
