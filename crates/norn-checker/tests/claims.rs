#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    missing_docs,
    reason = "integration-test crate"
)]

//! Claim and ruling obligations over the definition catalog (norn#62), on
//! the two cases the Astra plan asks for:
//!
//! - norn#59: `commerce.SalesOrder` declares an `invoice` transition into
//!   `invoiced`, a state derived from invoice facts. The catalog rule flags
//!   it, and no ruling can silence it.
//! - a cross-system mapping conflict: an ERP and a storefront both map an
//!   order's `total`, one tax-inclusive and one net. The witness is one real
//!   order; a ratified ruling resolves it only once the storefront mapping
//!   actually changes, because the witness is rechecked against the admitted
//!   mappings.

use std::collections::BTreeSet;

use norn_checker::claims::codes;
use norn_checker::{ArtifactRef, EntityArtifactSetContext, check_artifact_set};
use norn_ir::{ArtifactEnvelope, PropertyTypeName};
use serde_json::{Value, json};

const ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");

fn read_json(path: &str) -> Value {
    serde_json::from_str(
        &std::fs::read_to_string(format!("{ROOT}/{path}"))
            .unwrap_or_else(|error| panic!("{path} should be readable: {error}")),
    )
    .unwrap_or_else(|error| panic!("{path} should parse: {error}"))
}

fn decode(value: Value) -> ArtifactEnvelope {
    serde_json::from_value(value).expect("envelope decodes")
}

fn envelope(id: &str, kind: &str, version: &str, lifecycle: &str, payload: &Value) -> Value {
    json!({
        "irVersion": "0.1", "artifactId": id, "kind": kind, "namespace": "shop",
        "name": id, "version": version, "lifecycle": lifecycle,
        "scope": { "orgId": "org_demo", "environmentId": "dev" },
        "dependencies": [], "provenance": { "source": "system" },
        "payload": payload
    })
}

/// The codes of this module the set check reports, with their artifact.
fn obligations(artifacts: Vec<Value>) -> BTreeSet<(String, String)> {
    let artifacts: Vec<ArtifactEnvelope> = artifacts.into_iter().map(decode).collect();
    let registry = BTreeSet::<PropertyTypeName>::new();
    let ours = [
        codes::CLAIM_UNKNOWN_MEMBER,
        codes::RULING_UNKNOWN_MEMBER,
        codes::RULING_UNKNOWN_CLAIM,
        codes::DERIVED_STATE_TRANSITION,
        codes::UNRULED_DIVERGENCE,
        codes::CONTRADICTION_PERSISTS,
        codes::WITNESS_UNEVALUABLE,
        codes::CATALOG_UNAVAILABLE,
    ];
    check_artifact_set(
        &artifacts,
        &EntityArtifactSetContext {
            registry: &registry,
            base: &[],
            tenant: None,
        },
    )
    .into_iter()
    .filter(|finding| ours.contains(&finding.code.as_str()))
    .map(|finding| {
        (
            finding.code.as_str().to_owned(),
            finding.artifact_id.unwrap_or_default(),
        )
    })
    .collect()
}

fn expect(pairs: &[(&str, &str)]) -> BTreeSet<(String, String)> {
    pairs
        .iter()
        .map(|(code, id)| ((*code).to_owned(), (*id).to_owned()))
        .collect()
}

fn ruling(id: &str, lifecycle: &str, claim: &str, affects: &Value) -> Value {
    envelope(
        id,
        "ruling",
        "0.1.0",
        lifecycle,
        &json!({
            "rulingVersion": "0.1",
            "id": id,
            "resolves": [{ "artifactId": claim, "version": "0.1.0" }],
            "outcome": { "kind": "choose", "definition": affects },
            "affects": [affects],
            "decidedBy": { "principal": "owner", "authority": "owner" },
            "decidedAt": "2026-10-31"
        }),
    )
}

// norn#59 ------------------------------------------------------------------

const ORDER: &str = "commerce.SalesOrder.entity";
const INVOICED: &str = "commerce.sales_order_invoiced_is_derived.claim";

fn sales_order() -> Value {
    read_json("registry/core/artifacts/commerce.SalesOrder.entity.json")
}

fn order_member(member: &Value) -> Value {
    json!({ "artifactId": ORDER, "version": "0.1.0", "member": member })
}

fn invoiced_is_derived() -> Value {
    envelope(
        INVOICED,
        "claim",
        "0.1.0",
        "proposed",
        &json!({
            "claimVersion": "0.1",
            "id": "commerce.sales_order_invoiced_is_derived",
            "statement": {
                "kind": "derived_state",
                "state": order_member(&json!({
                    "kind": "state", "typeName": "commerce.SalesOrder",
                    "lifecycle": "status", "state": "invoiced"
                })),
                "from": [order_member(&json!({
                    "kind": "link", "typeName": "commerce.SalesOrder", "link": "lines"
                }))]
            },
            "evidence": [{
                "source": {
                    "kind": "upstream", "system": "erpnext", "release": "v16.0.0",
                    "locator": "erpnext/selling/doctype/sales_order/sales_order.py#set_status"
                },
                "strength": "structural",
                "note": "status follows per_billed, computed from Sales Invoice items"
            }]
        }),
    )
}

#[test]
fn a_declared_transition_into_a_derived_state_is_flagged() {
    let action = read_json("fixtures/ir/commerce-sales-order-submit-invoice.action.json");
    assert_eq!(
        obligations(vec![sales_order(), action, invoiced_is_derived()]),
        expect(&[(codes::DERIVED_STATE_TRANSITION, ORDER)])
    );
}

#[test]
fn no_ruling_waives_a_derived_state_transition() {
    let ratified = ruling(
        "commerce.invoiced_is_authored.ruling",
        "approved",
        INVOICED,
        &order_member(&json!({
            "kind": "transition", "typeName": "commerce.SalesOrder",
            "lifecycle": "status", "transition": "invoice"
        })),
    );
    assert_eq!(
        obligations(vec![sales_order(), invoiced_is_derived(), ratified]),
        expect(&[(codes::DERIVED_STATE_TRANSITION, ORDER)])
    );
}

#[test]
fn the_derived_state_rule_clears_only_when_the_transition_is_gone() {
    let mut order = sales_order();
    let transitions = order["payload"]["objectType"]["lifecycles"][0]["transitions"]
        .as_array_mut()
        .expect("transitions");
    transitions.retain(|transition| transition["to"] != "invoiced");
    assert_eq!(obligations(vec![order, invoiced_is_derived()]), expect(&[]));
}

#[test]
fn a_claim_about_a_member_the_set_lacks_is_refused() {
    let mut claim = invoiced_is_derived();
    claim["payload"]["statement"]["state"]["member"]["state"] = json!("shipped");
    assert_eq!(
        obligations(vec![sales_order(), claim]),
        expect(&[(codes::CLAIM_UNKNOWN_MEMBER, INVOICED)])
    );
}

// Cross-system mapping conflict ----------------------------------------------

const ERP: &str = "shop.erp.mapping";
const STOREFRONT: &str = "shop.storefront.mapping";
const TOTALS: &str = "shop.order_total_divergence.claim";

fn model() -> Vec<Value> {
    let property = |id: &str, kind: Value| json!({ "id": id, "type": kind });
    let total = json!({ "kind": "decimal", "scale": 2 });
    vec![
        envelope(
            "shop.Priced.interface",
            "entity",
            "0.1.0",
            "draft",
            &json!({ "entityVersion": "2.0", "interface": {
                "name": "shop.Priced",
                "properties": [property("id", json!({ "kind": "text" })), property("total", total.clone())]
            }}),
        ),
        envelope(
            "shop.Order.entity",
            "entity",
            "0.1.0",
            "draft",
            &json!({ "entityVersion": "2.0", "objectType": {
                "name": "shop.Order", "implements": ["shop.Priced"],
                "properties": [property("id", json!({ "kind": "text" })), property("total", total)]
            }}),
        ),
    ]
}

fn source(id: &str, system: &str, dataset: &str, fields: &[(&str, &str)]) -> Value {
    let fields: Vec<Value> = fields
        .iter()
        .enumerate()
        .map(|(ordinal, (name, kind))| {
            json!({
                "dataset": dataset, "name": name, "ordinal": ordinal, "nativeType": kind,
                "typeCandidate": { "type": kind, "origin": "declared" }, "required": true
            })
        })
        .collect();
    envelope(
        id,
        "source_schema",
        "0.1.0",
        "draft",
        &json!({
            "format": "norn.source-schema/1",
            "source": { "system": system, "instance": "synthetic" },
            "observedAt": "2026-10-01T00:00:00Z",
            "adapter": { "name": system, "version": "0.1.0" },
            "rawMetadata": { "uri": format!("evidence://synthetic/{system}"), "sha256": "a".repeat(64) },
            "datasets": [{ "id": dataset, "nativeName": dataset, "kind": "table" }],
            "fields": fields
        }),
    )
}

fn erp_source() -> Value {
    source(
        "shop.erp.source",
        "erp",
        "Sales Order",
        &[("name", "string"), ("grand_total", "decimal")],
    )
}

fn storefront_source() -> Value {
    source(
        "shop.storefront.source",
        "storefront",
        "orders",
        &[
            ("id", "string"),
            ("subtotal_price", "decimal"),
            ("total_price", "decimal"),
        ],
    )
}

fn mapping(
    id: &str,
    version: &str,
    source: &Value,
    dataset: &str,
    key: &str,
    total: &str,
) -> Value {
    let source_envelope = decode(source.clone());
    let sha256 = ArtifactRef::of(&source_envelope)
        .expect("source hash")
        .sha256;
    envelope(
        id,
        "mapping",
        version,
        "draft",
        &json!({
            "format": "norn.mapping/1",
            "source": { "schemaId": source_envelope.artifact_id, "version": "0.1.0", "sha256": sha256 },
            "interfaces": ["shop.Priced.interface"],
            "maps": [{
                "id": "order",
                "source": { "dataset": dataset },
                "target": { "entity": "shop.Order.entity", "interface": "shop.Priced.interface" },
                "subject": key, "sourceRecord": key,
                "properties": [
                    { "property": "id", "value": key },
                    { "property": "total", "value": total }
                ]
            }]
        }),
    )
}

fn erp_mapping() -> Value {
    mapping(
        ERP,
        "0.1.0",
        &erp_source(),
        "Sales Order",
        "input.name",
        "input.grand_total",
    )
}

/// v0.1.0 maps the net subtotal; v0.2.0, after the ruling, the gross total.
fn storefront_mapping(version: &str) -> Value {
    let total = if version == "0.1.0" {
        "input.subtotal_price"
    } else {
        "input.total_price"
    };
    mapping(
        STOREFRONT,
        version,
        &storefront_source(),
        "orders",
        "input.id",
        total,
    )
}

/// One real order: ERP `SO-1` and storefront order `1001`, 100 net + 14 tax.
fn totals_diverge() -> Value {
    envelope(
        TOTALS,
        "claim",
        "0.1.0",
        "proposed",
        &json!({
            "claimVersion": "0.1",
            "id": "shop.order_total_divergence",
            "statement": { "kind": "divergence", "witness": {
                "target": { "artifactId": "shop.Order.entity", "version": "0.1.0",
                    "member": { "kind": "property", "typeName": "shop.Order", "property": "total" } },
                "left": { "mapping": { "artifactId": ERP, "version": "0.1.0" }, "map": "order",
                    "record": { "name": "SO-1", "grand_total": 114 } },
                "right": { "mapping": { "artifactId": STOREFRONT, "version": "0.1.0" }, "map": "order",
                    "record": { "id": "1001", "subtotal_price": 100, "total_price": 114 } }
            }},
            "evidence": [{
                "source": { "kind": "upstream", "system": "storefront", "release": "2026-10",
                    "locator": "admin/api/orders.json#subtotal_price" },
                "strength": "sampled"
            }]
        }),
    )
}

fn storefront_artifact(version: &str) -> Value {
    json!({ "artifactId": STOREFRONT, "version": version, "member": { "kind": "artifact" } })
}

fn conflict_set(storefront: &str, extra: Vec<Value>) -> Vec<Value> {
    let mut artifacts = model();
    artifacts.extend([
        erp_source(),
        storefront_source(),
        erp_mapping(),
        storefront_mapping(storefront),
        totals_diverge(),
    ]);
    artifacts.extend(extra);
    artifacts
}

const RULING: &str = "shop.order_total_is_gross.ruling";

#[test]
fn a_witnessed_divergence_needs_a_ruling_and_contradicts() {
    assert_eq!(
        obligations(conflict_set("0.1.0", Vec::new())),
        expect(&[
            (codes::UNRULED_DIVERGENCE, TOTALS),
            (codes::CONTRADICTION_PERSISTS, TOTALS)
        ])
    );
}

#[test]
fn a_ratified_ruling_never_waives_the_contradiction() {
    // The ruling is ratified and cites the claim, but the storefront mapping
    // still maps the net subtotal: the recheck still finds 114 against 100.
    let ruling = ruling(RULING, "approved", TOTALS, &storefront_artifact("0.1.0"));
    assert_eq!(
        obligations(conflict_set("0.1.0", vec![ruling])),
        expect(&[(codes::CONTRADICTION_PERSISTS, TOTALS)])
    );
}

#[test]
fn the_ruling_resolves_once_the_mapping_changes() {
    let ruling = ruling(RULING, "approved", TOTALS, &storefront_artifact("0.2.0"));
    assert_eq!(
        obligations(conflict_set("0.2.0", vec![ruling])),
        expect(&[])
    );
}

#[test]
fn a_fixed_mapping_still_needs_a_ratified_ruling() {
    let proposed = ruling(RULING, "proposed", TOTALS, &storefront_artifact("0.2.0"));
    assert_eq!(
        obligations(conflict_set("0.2.0", vec![proposed])),
        expect(&[(codes::UNRULED_DIVERGENCE, TOTALS)])
    );
}

#[test]
fn a_ruling_pinned_to_a_replaced_version_does_not_count() {
    let stale = ruling(RULING, "approved", TOTALS, &storefront_artifact("0.1.0"));
    assert_eq!(
        obligations(conflict_set("0.2.0", vec![stale])),
        expect(&[
            (codes::RULING_UNKNOWN_MEMBER, RULING),
            (codes::UNRULED_DIVERGENCE, TOTALS)
        ])
    );
}

#[test]
fn a_witness_through_a_missing_mapping_fails_closed() {
    let artifacts: Vec<Value> = conflict_set("0.1.0", Vec::new())
        .into_iter()
        .filter(|artifact| artifact["artifactId"] != STOREFRONT)
        .collect();
    assert_eq!(
        obligations(artifacts),
        expect(&[
            (codes::UNRULED_DIVERGENCE, TOTALS),
            (codes::WITNESS_UNEVALUABLE, TOTALS)
        ])
    );
}

#[test]
fn a_witness_mapping_without_the_target_value_fails_closed() {
    // Both mappings stop mapping `total`: no value is not agreement.
    let mut artifacts = conflict_set("0.1.0", Vec::new());
    for artifact in &mut artifacts {
        if artifact["kind"] == "mapping" {
            artifact["payload"]["maps"][0]["properties"]
                .as_array_mut()
                .expect("properties")
                .retain(|rule| rule["property"] != "total");
            artifact["payload"]["maps"][0]["gaps"] =
                json!([{ "member": "total", "reason": "deferred" }]);
        }
    }
    assert_eq!(
        obligations(artifacts),
        expect(&[
            (codes::UNRULED_DIVERGENCE, TOTALS),
            (codes::WITNESS_UNEVALUABLE, TOTALS)
        ])
    );
}

#[test]
fn a_witness_mapping_to_another_type_fails_closed() {
    // The storefront map is redirected to another type with a `total`.
    let mut artifacts = conflict_set("0.1.0", Vec::new());
    let mut other = model()[1].clone();
    other["artifactId"] = json!("shop.Quote.entity");
    other["payload"]["objectType"]["name"] = json!("shop.Quote");
    artifacts.push(other);
    for artifact in &mut artifacts {
        if artifact["artifactId"] == STOREFRONT {
            artifact["payload"]["maps"][0]["target"]["entity"] = json!("shop.Quote.entity");
        }
    }
    assert_eq!(
        obligations(artifacts),
        expect(&[
            (codes::UNRULED_DIVERGENCE, TOTALS),
            (codes::WITNESS_UNEVALUABLE, TOTALS)
        ])
    );
}
