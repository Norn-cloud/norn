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

//! The technical-view vocabulary (T12-VIEW-1, #2717): the fixtures declare
//! every technical-view type as an ordinary `EntityIR` object type, business
//! types link to them through both lineage relation families, and the
//! fixtures round-trip in canonical form.

use std::collections::BTreeSet;

use norn_ir::{
    JsonValue, LineageRelation, ObjectTypeDef, ObjectTypeName, TECHNICAL_NAMESPACE, TechnicalType,
    is_technical_type,
};

const TECHNICAL: &str =
    include_str!("../../../fixtures/ir/entity-v2/technical-view/technical-view.object-types.json");
const BUSINESS: &str =
    include_str!("../../../fixtures/ir/entity-v2/technical-view/business.object-types.json");

fn decode(source: &str) -> Vec<ObjectTypeDef> {
    serde_json::from_str(source).expect("fixture decodes as object types")
}

fn canonical(source: &str) -> String {
    let value: JsonValue = serde_json::from_str(source).expect("fixture is JSON");
    serde_json::to_string(&value).expect("JSON encodes")
}

fn find<'a>(defs: &'a [ObjectTypeDef], name: &str) -> &'a ObjectTypeDef {
    defs.iter()
        .find(|def| def.name.as_str() == name)
        .unwrap_or_else(|| panic!("{name} is declared"))
}

#[test]
fn technical_view_fixture_declares_every_type_once() {
    let defs = decode(TECHNICAL);
    let declared: Vec<&str> = defs.iter().map(|def| def.name.as_str()).collect();
    let expected: BTreeSet<&str> = TechnicalType::ALL
        .into_iter()
        .map(TechnicalType::type_name)
        .collect();
    assert_eq!(declared.len(), expected.len(), "no type is declared twice");
    assert_eq!(declared.into_iter().collect::<BTreeSet<_>>(), expected);
    for def in &defs {
        assert!(is_technical_type(&def.name), "{} is technical", def.name.0);
        assert_eq!(def.name.domain(), Some(TECHNICAL_NAMESPACE));
        assert_eq!(
            TechnicalType::of(&def.name).map(TechnicalType::type_name),
            Some(def.name.as_str())
        );
        assert!(!def.identity.is_empty(), "{} has a key", def.name.0);
    }
}

#[test]
fn technical_view_business_types_use_both_relation_families() {
    let technical = decode(TECHNICAL);
    let business = decode(BUSINESS);
    assert!(!business.is_empty());
    for def in &business {
        assert!(
            !is_technical_type(&def.name),
            "{} is a business type",
            def.name.0
        );
        assert_eq!(TechnicalType::of(&def.name), None);
        for relation in LineageRelation::ALL {
            let link = def
                .links
                .iter()
                .find(|link| LineageRelation::of(&link.id) == Some(relation))
                .unwrap_or_else(|| panic!("{} declares {}", def.name.0, relation.relation_name()));
            assert!(
                is_technical_type(&link.target),
                "{}.{} targets the technical view",
                def.name.0,
                link.id.as_str()
            );
        }
    }

    let backed_by = |def: &ObjectTypeDef| -> Vec<ObjectTypeName> {
        def.links
            .iter()
            .filter(|link| LineageRelation::of(&link.id) == Some(LineageRelation::BackedBy))
            .map(|link| link.target.clone())
            .collect()
    };
    let order = find(&business, "commerce.SalesOrder");
    assert_eq!(
        backed_by(order),
        [ObjectTypeName(TechnicalType::Table.type_name().to_owned())]
    );

    // derivedFrom also chains inside the technical view: a mapping version is
    // derived from the source columns it reads.
    let mapping = find(&technical, TechnicalType::Mapping.type_name());
    let derived: Vec<&str> = mapping
        .links
        .iter()
        .filter(|link| LineageRelation::of(&link.id) == Some(LineageRelation::DerivedFrom))
        .map(|link| link.target.as_str())
        .collect();
    assert_eq!(derived, [TechnicalType::Column.type_name()]);
}

#[test]
fn technical_view_chain_reaches_source() {
    // Column → Table → Dataset → Source, each a required single link.
    let technical = decode(TECHNICAL);
    for (from, link, to) in [
        (TechnicalType::Column, "table", TechnicalType::Table),
        (TechnicalType::Table, "dataset", TechnicalType::Dataset),
        (TechnicalType::Dataset, "source", TechnicalType::Source),
        (TechnicalType::QualityCheck, "checks", TechnicalType::Column),
        (TechnicalType::Pipeline, "runs", TechnicalType::Mapping),
        (TechnicalType::Source, "owner", TechnicalType::Owner),
        (TechnicalType::Source, "domain", TechnicalType::Domain),
    ] {
        let def = find(&technical, from.type_name());
        let found = def
            .links
            .iter()
            .find(|candidate| candidate.id.as_str() == link)
            .unwrap_or_else(|| panic!("{}.{link} is declared", from.type_name()));
        assert_eq!(found.target.as_str(), to.type_name());
    }
}

#[test]
fn technical_view_fixtures_round_trip() {
    for source in [TECHNICAL, BUSINESS] {
        let defs = decode(source);
        assert_eq!(
            canonical(&serde_json::to_string(&defs).expect("defs encode")),
            canonical(source),
            "fixtures are in canonical form"
        );
    }
}

#[test]
fn technical_view_relation_names() {
    assert_eq!(LineageRelation::BackedBy.link_id(), "backed_by");
    assert_eq!(LineageRelation::BackedBy.relation_name(), "backedBy");
    assert_eq!(LineageRelation::DerivedFrom.link_id(), "derived_from");
    assert_eq!(LineageRelation::DerivedFrom.relation_name(), "derivedFrom");
    assert_eq!(
        LineageRelation::of(&norn_ir::LinkId("customer".to_owned())),
        None
    );
}
