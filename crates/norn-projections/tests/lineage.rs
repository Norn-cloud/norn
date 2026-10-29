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

//! Technical lineage (T12-VIEW-2, #2718): "why is order #1001 on hold?" is a
//! traversal over ledger commits, observation provenance, and ordinary
//! technical-view links that returns the causal chain with stable ids; and
//! one artifact set projects to distinct business and technical views.

use norn_ir::{
    FactRef, InstanceProvenance, LineageInput, MappingVersion, ObjectTypeDef, ObjectTypeName,
    ProvenancedInstance, SourceRecordRef, TechnicalType, UtcTimestamp,
};
use norn_projections::{
    LineageEdgeKind, LineageError, LineageNodeKind, explain, explain_latest, project_views,
};

const ORDER_ON_HOLD: &str =
    include_str!("../../../fixtures/lineage/order-1001-on-hold.lineage.json");
const TECHNICAL: &str =
    include_str!("../../../fixtures/ir/entity-v2/technical-view/technical-view.object-types.json");
const BUSINESS: &str =
    include_str!("../../../fixtures/ir/entity-v2/technical-view/business.object-types.json");

fn input() -> LineageInput {
    serde_json::from_str(ORDER_ON_HOLD).expect("lineage fixture decodes")
}

fn order() -> ObjectTypeName {
    ObjectTypeName("commerce.SalesOrder".to_owned())
}

const SO_TABLE: &str = "object:technical.Table/erpnext.prod.tabSales Order";
const CUSTOMER_TABLE: &str = "object:technical.Table/erpnext.prod.tabCustomer";

#[test]
fn lineage_why_is_order_1001_on_hold() {
    let chain = explain_latest(&input(), &order(), "1001").expect("order 1001 is explained");

    // The current version is v2: the hold.
    assert_eq!(chain.root, "fact:commerce.SalesOrder/1001@v2");

    let nodes: Vec<(&str, LineageNodeKind)> = chain
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node.kind))
        .collect();
    assert_eq!(
        nodes,
        [
            ("fact:commerce.SalesOrder/1001@v2", LineageNodeKind::Fact),
            ("commit:main/3", LineageNodeKind::Commit),
            ("decision:credit.hold_policy@2", LineageNodeKind::Decision),
            ("timer:main/credit-check-1001@2", LineageNodeKind::Trigger),
            ("commit:main/2", LineageNodeKind::Commit),
            ("fact:crm.Customer/C-77@v1", LineageNodeKind::Fact),
            ("fact:commerce.SalesOrder/1001@v1", LineageNodeKind::Fact),
            ("observation:obs-erp-so-1001", LineageNodeKind::Observation),
            ("commit:main/1", LineageNodeKind::Commit),
            (
                "source:ERPNext/Sales Order/SO-1001",
                LineageNodeKind::SourceRecord
            ),
            (
                "object:technical.Source/ERPNext",
                LineageNodeKind::Technical(TechnicalType::Source)
            ),
            (
                "object:technical.Mapping/erpnext.sales_order@3",
                LineageNodeKind::Technical(TechnicalType::Mapping)
            ),
            ("observation:obs-erp-cust-77", LineageNodeKind::Observation),
            (
                "object:technical.Owner/finance-ops",
                LineageNodeKind::Technical(TechnicalType::Owner)
            ),
            (
                "object:technical.Column/erpnext.prod.tabSales Order.grand_total",
                LineageNodeKind::Technical(TechnicalType::Column)
            ),
            (
                "object:technical.Column/erpnext.prod.tabSales Order.customer",
                LineageNodeKind::Technical(TechnicalType::Column)
            ),
            (
                "source:ERPNext/Customer/CUST-77",
                LineageNodeKind::SourceRecord
            ),
            (
                "object:technical.Mapping/erpnext.customer@2",
                LineageNodeKind::Technical(TechnicalType::Mapping)
            ),
            (SO_TABLE, LineageNodeKind::Technical(TechnicalType::Table)),
            (
                "object:technical.Column/erpnext.prod.tabCustomer.credit_limit",
                LineageNodeKind::Technical(TechnicalType::Column)
            ),
            (
                "object:technical.Dataset/erpnext.prod",
                LineageNodeKind::Technical(TechnicalType::Dataset)
            ),
            (
                CUSTOMER_TABLE,
                LineageNodeKind::Technical(TechnicalType::Table)
            ),
        ]
    );

    let edges: Vec<(&str, &str, &str)> = chain
        .edges
        .iter()
        .map(|edge| (edge.from.as_str(), edge.to.as_str(), edge.kind.name()))
        .collect();
    assert_eq!(
        edges,
        [
            (
                "fact:commerce.SalesOrder/1001@v2",
                "commit:main/3",
                "writtenBy"
            ),
            (
                "commit:main/3",
                "decision:credit.hold_policy@2",
                "decidedBy"
            ),
            (
                "commit:main/3",
                "timer:main/credit-check-1001@2",
                "consumed"
            ),
            (
                "timer:main/credit-check-1001@2",
                "commit:main/2",
                "scheduledBy"
            ),
            ("commit:main/3", "fact:crm.Customer/C-77@v1", "read"),
            ("commit:main/3", "fact:commerce.SalesOrder/1001@v1", "read"),
            ("commit:main/2", "observation:obs-erp-so-1001", "consumed"),
            ("fact:crm.Customer/C-77@v1", "commit:main/1", "writtenBy"),
            (
                "fact:commerce.SalesOrder/1001@v1",
                "commit:main/2",
                "writtenBy"
            ),
            (
                "observation:obs-erp-so-1001",
                "source:ERPNext/Sales Order/SO-1001",
                "observedFrom"
            ),
            (
                "source:ERPNext/Sales Order/SO-1001",
                "object:technical.Source/ERPNext",
                "recordOf"
            ),
            (
                "observation:obs-erp-so-1001",
                "object:technical.Mapping/erpnext.sales_order@3",
                "mappedBy"
            ),
            ("commit:main/1", "observation:obs-erp-cust-77", "consumed"),
            (
                "object:technical.Source/ERPNext",
                "object:technical.Owner/finance-ops",
                "owner"
            ),
            (
                "object:technical.Mapping/erpnext.sales_order@3",
                "object:technical.Column/erpnext.prod.tabSales Order.grand_total",
                "derivedFrom"
            ),
            (
                "object:technical.Mapping/erpnext.sales_order@3",
                "object:technical.Column/erpnext.prod.tabSales Order.customer",
                "derivedFrom"
            ),
            (
                "observation:obs-erp-cust-77",
                "source:ERPNext/Customer/CUST-77",
                "observedFrom"
            ),
            (
                "source:ERPNext/Customer/CUST-77",
                "object:technical.Source/ERPNext",
                "recordOf"
            ),
            (
                "observation:obs-erp-cust-77",
                "object:technical.Mapping/erpnext.customer@2",
                "mappedBy"
            ),
            (
                "object:technical.Column/erpnext.prod.tabSales Order.grand_total",
                SO_TABLE,
                "table"
            ),
            (
                "object:technical.Column/erpnext.prod.tabSales Order.customer",
                SO_TABLE,
                "table"
            ),
            (
                "object:technical.Mapping/erpnext.customer@2",
                "object:technical.Column/erpnext.prod.tabCustomer.credit_limit",
                "derivedFrom"
            ),
            (SO_TABLE, "object:technical.Dataset/erpnext.prod", "dataset"),
            (
                "object:technical.Column/erpnext.prod.tabCustomer.credit_limit",
                CUSTOMER_TABLE,
                "table"
            ),
            (
                "object:technical.Dataset/erpnext.prod",
                "object:technical.Source/ERPNext",
                "source"
            ),
            (
                CUSTOMER_TABLE,
                "object:technical.Dataset/erpnext.prod",
                "dataset"
            ),
        ]
    );

    // The hold itself: commit 3's ledger event is the declared transition.
    let hold = chain.node("commit:main/3").expect("the hold commit");
    assert_eq!(hold.label, "commit main/3 by credit.hold_policy@2");
    assert_eq!(hold.events.len(), 1);
    let event = serde_json::to_value(&hold.events[0]).expect("event encodes");
    assert_eq!(event["op"], "transitioned");
    assert_eq!(event["to"], "on_hold");

    // Transforms ride on the mappedBy edges.
    let mapped: Vec<Option<&str>> = chain
        .edges
        .iter()
        .filter(|edge| edge.kind == LineageEdgeKind::MappedBy)
        .map(|edge| edge.detail.as_deref())
        .collect();
    assert_eq!(
        mapped,
        [Some("sales_order.header"), Some("customer.credit")]
    );

    // The causes of the hold commit, in order.
    let causes: Vec<&str> = chain
        .causes("commit:main/3")
        .map(|edge| edge.to.as_str())
        .collect();
    assert_eq!(
        causes,
        [
            "decision:credit.hold_policy@2",
            "timer:main/credit-check-1001@2",
            "fact:crm.Customer/C-77@v1",
            "fact:commerce.SalesOrder/1001@v1",
        ]
    );

    // The pipeline that runs a mapping is not a cause of the order.
    assert!(
        chain
            .node("object:technical.Pipeline/erpnext.nightly")
            .is_none()
    );
}

#[test]
fn lineage_is_deterministic_under_input_order() {
    let forward = input();
    let mut reversed = forward.clone();
    reversed.commits.reverse();
    reversed.observations.reverse();
    reversed.objects.reverse();
    let fact = FactRef::new("commerce.SalesOrder", "1001", 2);
    assert_eq!(
        explain(&forward, &fact).expect("explained"),
        explain(&reversed, &fact).expect("explained")
    );
}

#[test]
fn lineage_explains_an_earlier_version() {
    let chain = explain(&input(), &FactRef::new("commerce.SalesOrder", "1001", 1))
        .expect("v1 is explained");
    assert_eq!(chain.root, "fact:commerce.SalesOrder/1001@v1");
    assert!(
        chain.node("commit:main/3").is_none(),
        "v1 predates the hold"
    );
    assert!(chain.node("observation:obs-erp-so-1001").is_some());
}

#[test]
fn lineage_follows_stored_instance_provenance() {
    let mut input = input();
    let fact = FactRef::new("crm.CreditExposure", "C-77", 1);
    input.instances.push(ProvenancedInstance {
        fact: fact.clone(),
        provenance: vec![InstanceProvenance {
            source: SourceRecordRef::new("ERPNext", "Customer/CUST-77"),
            mapping_version: MappingVersion("erpnext.customer@2".to_owned()),
            transform: None,
            observed_at: None,
            recorded_at: UtcTimestamp("2026-09-30T08:00:02Z".to_owned()),
            source_fields: Vec::new(),
            markings: Vec::new(),
            upstream: vec![FactRef::new("crm.Customer", "C-77", 1)],
        }],
    });
    let chain = explain(&input, &fact).expect("stored provenance explains the fact");
    let causes: Vec<(&str, &str)> = chain
        .causes("fact:crm.CreditExposure/C-77@v1")
        .map(|edge| (edge.to.as_str(), edge.kind.name()))
        .collect();
    assert_eq!(
        causes,
        [
            ("source:ERPNext/Customer/CUST-77", "observedFrom"),
            ("object:technical.Mapping/erpnext.customer@2", "mappedBy"),
            ("fact:crm.Customer/C-77@v1", "derivedFrom"),
        ]
    );
    assert!(
        chain.node("commit:main/1").is_some(),
        "the upstream fact is explained in turn"
    );
}

#[test]
fn lineage_refuses_unknown_and_ambiguous_input() {
    let input = input();
    assert_eq!(
        explain(&input, &FactRef::new("commerce.SalesOrder", "1001", 9)),
        Err(LineageError::UnknownFact {
            fact: "fact:commerce.SalesOrder/1001@v9".to_owned()
        })
    );
    assert_eq!(
        explain_latest(&input, &order(), "9999"),
        Err(LineageError::UnknownObject {
            object: "object:commerce.SalesOrder/9999".to_owned()
        })
    );

    let mut duplicate_writer = input.clone();
    let mut second = duplicate_writer.commits[0].clone();
    second.seq = 4;
    duplicate_writer.commits.push(second);
    assert_eq!(
        explain(
            &duplicate_writer,
            &FactRef::new("commerce.SalesOrder", "1001", 2)
        ),
        Err(LineageError::DuplicateWriter {
            fact: "fact:commerce.SalesOrder/1001@v2".to_owned()
        })
    );

    let mut duplicate_commit = input;
    let mut again = duplicate_commit.commits[1].clone();
    again.written.clear();
    duplicate_commit.commits.push(again);
    assert_eq!(
        explain(
            &duplicate_commit,
            &FactRef::new("commerce.SalesOrder", "1001", 2)
        ),
        Err(LineageError::DuplicateCommit {
            commit: "commit:main/1".to_owned()
        })
    );
}

#[test]
fn lineage_two_views_from_one_artifact_set() {
    let mut defs: Vec<ObjectTypeDef> =
        serde_json::from_str(TECHNICAL).expect("technical view decodes");
    defs.extend(serde_json::from_str::<Vec<ObjectTypeDef>>(BUSINESS).expect("business decodes"));

    let views = project_views(&defs);
    assert_eq!(views, project_views(&defs), "the projection is pure");

    // Business view: the two business types, their properties, and only the
    // business link (customer); no technical type, no lineage link.
    let business: Vec<(&str, Vec<&str>, Vec<&str>)> = views
        .business
        .types
        .iter()
        .map(|ty| {
            (
                ty.name.as_str(),
                ty.properties.iter().map(|id| id.as_str()).collect(),
                ty.links.iter().map(|link| link.id.as_str()).collect(),
            )
        })
        .collect();
    assert_eq!(
        business,
        [
            (
                "crm.Customer",
                vec!["customer_id", "name", "credit_limit", "outstanding"],
                vec![]
            ),
            (
                "commerce.SalesOrder",
                vec!["so_number", "grand_total", "status"],
                vec!["customer"]
            ),
        ]
    );

    // Technical view: all nine assets, and four bindings (two families on
    // each business type).
    let assets: Vec<Option<TechnicalType>> = views
        .technical
        .assets
        .iter()
        .map(|asset| asset.kind)
        .collect();
    assert_eq!(assets.len(), TechnicalType::ALL.len());
    assert!(assets.iter().all(Option::is_some));
    let bindings: Vec<(&str, &str, &str)> = views
        .technical
        .bindings
        .iter()
        .map(|binding| {
            (
                binding.business_type.as_str(),
                binding.relation.relation_name(),
                binding.target.as_str(),
            )
        })
        .collect();
    assert_eq!(
        bindings,
        [
            ("crm.Customer", "backedBy", "technical.Table"),
            ("crm.Customer", "derivedFrom", "technical.Mapping"),
            ("commerce.SalesOrder", "backedBy", "technical.Table"),
            ("commerce.SalesOrder", "derivedFrom", "technical.Mapping"),
        ]
    );
}
