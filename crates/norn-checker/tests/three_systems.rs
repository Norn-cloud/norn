#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    missing_docs,
    reason = "synthetic three-system conformance"
)]

//! MVP-MAP-THREE / MVP-MAP-GAP: admitted mappings preserve explicit identity,
//! REA roles and source evidence; coverage gaps never invent valuation authority.
use norn_checker::{
    ArtifactRef, EntityArtifactSetContext, check_artifact_set,
    lint::coverage::{MemberRef, MemberStatus},
    mapping::{CompiledMapping, SourceDocument, compile_mapping_artifact},
};
use norn_ir::{ArtifactEnvelope, mapping::MappedRecord, provenance::UtcTimestamp};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use std::{collections::BTreeSet, fs, path::PathBuf};

const SYSTEMS: [&str; 3] = ["erpnext", "shopify", "odoo"];
fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}
fn artifacts() -> Vec<ArtifactEnvelope> {
    [
        "core",
        "packs/canon",
        "packs/erpnext",
        "packs/shopify",
        "packs/odoo",
    ]
    .into_iter()
    .flat_map(|dir| {
        fs::read_dir(root().join(format!("registry/{dir}/artifacts"))).expect("directory")
    })
    .map(|entry| {
        serde_json::from_str(&fs::read_to_string(entry.expect("entry").path()).expect("artifact"))
            .expect("envelope")
    })
    .filter(|a: &ArtifactEnvelope| {
        ![
            "commerce.SalesOrder.entity",
            "core.Order.interface",
            "core.Entitlement.entity",
            "identity.BusinessUnit.entity",
            "location.Region.entity",
            "location.Space.entity",
            "party.Brand.entity",
        ]
        .contains(&a.artifact_id.as_str())
    })
    .collect()
}
fn compiled(system: &str) -> CompiledMapping {
    let set = artifacts();
    let mapping = set
        .iter()
        .find(|a| a.artifact_id == format!("{system}.three.mapping"))
        .expect("mapping");
    let source = set
        .iter()
        .find(|a| a.artifact_id == format!("{system}.three.source"))
        .expect("source");
    let source_wire = serde_json::to_value(source).expect("source wire");
    let bytes = fs::read(root().join(format!("registry/packs/{system}/synthetic.json")))
        .expect("fixture bytes");
    assert_eq!(
        source_wire["payload"]["rawMetadata"]["sha256"],
        format!("{:x}", Sha256::digest(bytes))
    );
    let wire = serde_json::to_value(mapping).expect("wire");
    assert_eq!(
        wire["payload"]["source"]["sha256"],
        ArtifactRef::of(source).expect("source pin").sha256
    );
    compile_mapping_artifact(mapping, &set, &[]).unwrap_or_else(|d| panic!("{system}: {d:#?}"))
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Fixture {
    profile: String,
    capability: String,
    snapshot_watermark: u64,
    production_cdc: bool,
    snapshot: Vec<Row>,
    incremental: Vec<Change>,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Row {
    dataset: String,
    record: Value,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Change {
    sequence: u64,
    operation: String,
    dataset: String,
    record: Value,
}
fn fixture(system: &str) -> Fixture {
    serde_json::from_str(
        &fs::read_to_string(root().join(format!("registry/packs/{system}/synthetic.json")))
            .expect("fixture"),
    )
    .expect("synthetic contract")
}
fn documents(rows: &[Row]) -> Vec<SourceDocument> {
    rows.iter()
        .map(|r| {
            SourceDocument::from_json(&r.dataset, &r.record.to_string(), None).expect("evidence")
        })
        .collect()
}
fn apply(mapping: &CompiledMapping, rows: &[Row]) -> Vec<MappedRecord> {
    mapping
        .apply(
            &documents(rows),
            &UtcTimestamp("2026-11-02T12:00:00Z".into()),
        )
        .unwrap_or_else(|d| panic!("mapping: {d:#?}"))
}
fn all_rows(f: &Fixture) -> Vec<Row> {
    f.snapshot
        .iter()
        .cloned()
        .chain(f.incremental.iter().map(|c| Row {
            dataset: c.dataset.clone(),
            record: c.record.clone(),
        }))
        .collect()
}

#[test]
fn mvp_map_three_artifact_set_is_admitted() {
    let set = artifacts();
    let (base, candidate): (Vec<_>, Vec<_>) = set
        .into_iter()
        .partition(|a| a.artifact_id.starts_with("core.") || a.artifact_id.starts_with("std."));
    let diagnostics = check_artifact_set(
        &candidate,
        &EntityArtifactSetContext {
            registry: &BTreeSet::new(),
            base: &base,
            tenant: None,
        },
    );
    assert!(
        diagnostics
            .iter()
            .all(|d| d.severity != norn_ir::DiagnosticSeverity::Error),
        "{diagnostics:#?}"
    );
}
#[test]
fn mvp_map_three_aliases_roles_and_one_economic_identity() {
    let mut deliveries = BTreeSet::new();
    let mut aliases = BTreeSet::new();
    for system in SYSTEMS {
        let mapping = compiled(system);
        let rows = all_rows(&fixture(system));
        let mapped = apply(&mapping, &rows);
        let delivery = mapped
            .iter()
            .find(|r| r.map == "delivery")
            .expect("delivery");
        deliveries.insert((delivery.object_type.clone(), delivery.key.clone()));
        aliases.insert(format!("{:?}", delivery.provenance.source));
        assert_eq!(delivery.links.len(), 4);
        let values = serde_json::to_value(&delivery.invocations[0].input).expect("delivery values");
        assert_eq!(values["quantity"], json!(20));
        assert_eq!(values["unit"], json!("g"));
        assert_eq!(values["action"], json!("transfer_ownership"));
        assert_eq!(values["currency"], Value::Null);
        for (role, key) in [
            ("provider", "demo/party/owner"),
            ("receiver", "demo/party/customer"),
            ("resource", "demo/holding/owner/north/gold"),
            ("fulfils", "demo/order/1/line/1"),
        ] {
            let link = delivery
                .links
                .iter()
                .find(|l| l.link.as_str() == role)
                .expect("role");
            assert_eq!(link.target_key, key);
            assert_eq!(link.key, delivery.key);
        }
        assert!(!delivery.property_provenance.is_empty());
        let holding = mapped.iter().find(|r| r.map == "holding").expect("holding");
        for (role, key) in [
            ("owner", "demo/party/owner"),
            ("custodian", "demo/party/custodian"),
        ] {
            assert_eq!(
                holding
                    .links
                    .iter()
                    .find(|l| l.link.as_str() == role)
                    .expect("resource role")
                    .target_key,
                key
            );
        }
        let values = serde_json::to_value(&holding.invocations[0].input).expect("holding values");
        assert_eq!(values["quantity"], json!(200));
        assert_eq!(
            values["quantity_basis"],
            json!(if system == "shopify" {
                "available"
            } else {
                "on_hand"
            })
        );
        assert_eq!(mapped.iter().filter(|r| r.map == "party_role").count(), 3);
        assert!(
            mapped
                .iter()
                .any(|r| r.object_type.as_str() == "canon.OrderLine")
        );
        assert!(
            mapped
                .iter()
                .any(|r| r.object_type.as_str() == "canon.SettlementObservation")
        );
    }
    assert_eq!(
        deliveries.len(),
        1,
        "three source reports describe one event, not three contributions"
    );
    assert_eq!(aliases.len(), 3, "keep all source aliases/receipts");
}
#[test]
fn mvp_item_holding_catalog_and_owner_location_partitions() {
    for system in SYSTEMS {
        let mapped = apply(&compiled(system), &fixture(system).snapshot);
        let items: Vec<_> = mapped.iter().filter(|r| r.map == "item").collect();
        let holdings: Vec<_> = mapped.iter().filter(|r| r.map == "holding").collect();
        assert_eq!(items.len(), 1);
        assert_eq!(holdings.len(), 2);
        assert_eq!(items[0].object_type.as_str(), "canon.Item");
        assert_eq!(holdings[0].object_type.as_str(), "canon.Holding");
        assert_ne!(holdings[0].key, holdings[1].key);
        assert!(items[0].links.is_empty());
    }
}
#[test]
fn mvp_map_gap_shopify_price_is_never_stock_cost_or_gl() {
    let mapping = compiled("shopify");
    let mut rows = fixture("shopify").snapshot;
    let mapped = apply(&mapping, &rows);
    for row in &mut rows {
        if row.dataset == "ProductVariant" {
            row.record["price"] = json!(999_999);
        }
    }
    assert_eq!(
        mapped,
        apply(&mapping, &rows),
        "price has no canonical numeric contribution"
    );
    assert!(
        mapped
            .iter()
            .all(|r| r.object_type.as_str() != "canon.Posting")
    );
    let coverage = mapping
        .coverage()
        .iter()
        .find(|c| c.target.as_str() == "canon.Holding")
        .expect("coverage");
    for field in ["stock_cost", "cost_currency"] {
        let member = coverage
            .member(&MemberRef::Property(norn_ir::PropertyId(field.to_owned())))
            .expect("explicit gap");
        assert!(matches!(member.status, MemberStatus::Gap { .. }));
    }
    let holding = mapped.iter().find(|r| r.map == "holding").expect("holding");
    assert!(!holding.invocations[0].input.contains_key("stock_cost"));
    assert!(!holding.invocations[0].input.contains_key("cost_currency"));
}
#[test]
fn mvp_map_three_ambiguous_identity_refuses_without_sku_guessing() {
    for (system, field) in [
        ("erpnext", "custom_norn_ref"),
        ("shopify", "metafield_norn_ref"),
        ("odoo", "x_norn_ref"),
    ] {
        let mapping = compiled(system);
        let f = fixture(system);
        let item = f
            .snapshot
            .iter()
            .find(|r| r.record.get("sku").is_some())
            .expect("item");
        for invalid in [
            Value::Null,
            json!(["demo/item/a", "demo/item/b"]),
            json!(""),
        ] {
            let mut row = item.clone();
            row.record[field] = invalid;
            let findings = mapping
                .apply(
                    &documents(&[row]),
                    &UtcTimestamp("2026-11-02T12:00:00Z".into()),
                )
                .expect_err("mapping refusal");
            assert!(findings.iter().any(|d| {
                [
                    "mapping.runtime.invalid_record",
                    "mapping.runtime.invalid_key",
                ]
                .contains(&d.code.as_str())
            }));
        }
        let mut second = item.clone();
        second.record[field] = json!("demo/item/other");
        second.record[if system == "erpnext" { "name" } else { "id" }] =
            json!("SYN-DISTINCT-SOURCE");
        let mapped = apply(&mapping, &[item.clone(), second]);
        assert_ne!(
            mapped[0].key, mapped[1].key,
            "equal SKUs never merge explicit identities"
        );
    }
}
#[test]
fn mvp_map_three_odoo_snapshot_incremental_and_replay_are_synthetic() {
    for system in SYSTEMS {
        let f = fixture(system);
        let mapping = compiled(system);
        assert_eq!(f.profile, format!("synthetic-{system}-three/1"));
        assert_eq!(f.capability, "synthetic_snapshot_incremental");
        assert!(!f.production_cdc);
        let mut watermark = f.snapshot_watermark;
        for change in &f.incremental {
            assert_eq!(change.operation, "upsert");
            assert!(change.sequence == watermark || change.sequence == watermark + 1);
            watermark = change.sequence;
        }
        let incremental: Vec<_> = f
            .incremental
            .iter()
            .map(|c| Row {
                dataset: c.dataset.clone(),
                record: c.record.clone(),
            })
            .collect();
        let first = apply(&mapping, &incremental);
        let updated = first
            .iter()
            .find(|r| r.map == "holding")
            .expect("balance increment");
        let values = serde_json::to_value(&updated.invocations[0].input).expect("updated holding");
        assert_eq!(updated.key, "demo/holding/owner/north/gold");
        assert_eq!(values["quantity"], json!(180));
        if system != "shopify" {
            assert_eq!(values["stock_cost"], json!(8100));
            assert_eq!(values["cost_currency"], json!("USD"));
        }

        assert_eq!(first, apply(&mapping, &incremental), "deterministic replay");
        let keys: BTreeSet<_> = first.iter().map(|r| (&r.object_type, &r.key)).collect();
        assert_eq!(
            keys.len() + 1,
            first.len(),
            "only deliberate delivery redelivery shares identity"
        );
        if system != "shopify" {
            let postings: Vec<_> = first.iter().filter(|r| r.map == "posting").collect();
            assert_eq!(postings.len(), 2);
            for (row, account, amount) in
                [(postings[0], "cogs", 900), (postings[1], "inventory", -900)]
            {
                let input =
                    serde_json::to_value(&row.invocations[0].input).expect("posting values");
                assert_eq!(input["amount"], json!(amount));
                assert_eq!(input["currency"], json!("USD"));
                assert_eq!(input["book"], json!("demo/stock-usd"));
                assert_eq!(input["account_role"], json!(account));
                assert_eq!(row.links[0].target_key, "demo/delivery/1/line/1");
            }
        }
    }
}

#[test]
fn mvp_map_three_unresolved_units_and_draft_postings_refuse() {
    for system in SYSTEMS {
        let mapping = compiled(system);
        let f = fixture(system);
        let mut row = f
            .snapshot
            .iter()
            .find(|r| r.record.get("quantity").is_some())
            .expect("holding")
            .clone();
        row.record["unit"] = json!("kg");
        assert!(
            mapping
                .apply(
                    &documents(&[row]),
                    &UtcTimestamp("2026-11-02T12:00:00Z".into())
                )
                .is_err()
        );
        let mut row = Row {
            dataset: f.incremental[0].dataset.clone(),
            record: f.incremental[0].record.clone(),
        };
        row.record["posting_state"] = json!("draft");
        assert!(
            mapping
                .apply(
                    &documents(&[row]),
                    &UtcTimestamp("2026-11-02T12:00:00Z".into())
                )
                .is_err()
        );
    }
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(32))]
    #[test]
    fn mvp_map_three_generated_aliases_never_guess_catalog_identity(a in "[A-Z]{1,10}", b in "[A-Z]{1,10}") {
        proptest::prop_assume!(a != b);
        let mut canonical = BTreeSet::new();
        for (system,field,native) in [("erpnext","custom_norn_ref","name"),("shopify","metafield_norn_ref","id"),("odoo","x_norn_ref","id")] {
            let mapping=compiled(system); let f=fixture(system);
            let item=f.snapshot.iter().find(|r|r.record.get("sku").is_some()).expect("item");
            let mut first=item.clone(); first.record[field]=json!(format!("demo/item/{a}"));
            let mut second=item.clone(); second.record[field]=json!(format!("demo/item/{b}")); second.record[native]=json!("SYN-DISTINCT-SOURCE");
            let mapped=apply(&mapping,&[first.clone(),second.clone()]);
            proptest::prop_assert_ne!(&mapped[0].key,&mapped[1].key);
            canonical.insert((mapped[0].object_type.clone(),mapped[0].key.clone()));
            let reversed=apply(&mapping,&[second,first]);
            proptest::prop_assert_eq!(&mapped[0],&reversed[1]);
            proptest::prop_assert_eq!(&mapped[1],&reversed[0]);
        }
        proptest::prop_assert_eq!(canonical.len(),1);
    }
}

/// OBS-LINK-DECLARED: compiled outputs preserve every admitted source role.
#[test]
fn obs_link_declared_outputs_retain_canonical_endpoints_and_rule_provenance() {
    for system in SYSTEMS {
        let mapped = apply(&compiled(system), &all_rows(&fixture(system)));
        let mut roles = BTreeSet::new();
        for record in mapped {
            let claims = record.link_claims(&format!("synthetic-bundle:{system}"), "synthetic");
            assert_eq!(
                claims.len(),
                record.links.len() + record.cleared_links.len()
            );
            for patch in &record.links {
                let claim = claims
                    .iter()
                    .find(|claim| claim.role == patch.link)
                    .expect("declared claim");
                assert_eq!(claim.source_type, patch.object_type);
                assert_eq!(claim.source_key, patch.key);
                let target = claim.target.as_ref().expect("explicit canonical endpoint");
                assert_eq!(target.object_type, patch.target_type);
                assert_eq!(target.key, patch.target_key);
                assert_eq!(claim.provenance, patch.provenance);
                assert!(target.key.starts_with("demo/"));
                assert_eq!(
                    claim.provenance.transform.as_deref(),
                    Some(format!("{}.{}", record.map, patch.link.as_str()).as_str())
                );
                roles.insert((record.map.clone(), patch.link.0.clone()));
            }
        }
        for (map, role) in [
            ("party_role", "agent"),
            ("holding", "item"),
            ("holding", "owner"),
            ("holding", "custodian"),
            ("order", "provider"),
            ("order", "receiver"),
            ("order", "resource"),
            ("delivery", "provider"),
            ("delivery", "receiver"),
            ("delivery", "resource"),
            ("delivery", "fulfils"),
            ("settlement", "payer"),
            ("settlement", "payee"),
        ] {
            assert!(
                roles.contains(&(map.into(), role.into())),
                "{system}: {map}.{role}"
            );
        }
        assert_eq!(
            roles.contains(&("posting".into(), "event".into())),
            system != "shopify"
        );
    }
}

/// METRIC-ADDITIVITY-REF-1 over canon: the flows derive exactly one sum each over
/// their own time property, and the stock (`Holding`) derives only its as-of
/// snapshots over `observed_at` (METRIC-SNAPSHOT-ASOF-1). A posting's amount takes
/// its event's time (METRIC-GRAIN-LINK-1); `SettlementObservation` derives nothing.
/// Each quantity carries its unit partition (`unit`, `currency`).
#[test]
fn three_systems_derive_exactly_the_canon_flow_sums() {
    let space = norn_checker::metric::derive::derive_metric_space(
        &artifacts(),
        &BTreeSet::new(),
        &norn_ir::LawRoot::Entity("canon.Delivery".to_owned()),
    )
    .unwrap_or_else(|d| panic!("{d:#?}"));
    let derived: BTreeSet<(String, String, String, bool)> = space
        .candidates()
        .iter()
        .map(|candidate| {
            let params = &candidate.definition.type_params;
            let model = params
                .metric_aggregation_params
                .as_ref()
                .expect("a derived sum")
                .semantic_model
                .as_str()
                .to_owned();
            let time = candidate
                .definition
                .norn
                .time_dimension
                .as_ref()
                .expect("a time grain")
                .property
                .as_str()
                .to_owned();
            let snapshot = matches!(
                candidate.definition.norn.additivity,
                norn_ir::metric::MetricAdditivity::SemiAdditive { .. }
            );
            (
                model,
                params.expr.clone().expect("a measure"),
                time,
                snapshot,
            )
        })
        .collect();
    let expected = [
        ("canon.Delivery", "quantity", "occurred_at", false),
        ("canon.OrderLine", "quantity", "due_at", false),
        ("canon.Holding", "quantity", "observed_at", true),
        ("canon.Holding", "stock_cost", "observed_at", true),
        // METRIC-GRAIN-LINK-1: a posting has no time of its own; it takes its event's.
        ("canon.Posting", "amount", "occurred_at", false),
        // A settlement is summed per currency by its optional event's time; one
        // without an event is counted as unlinked, never bucketed.
        (
            "canon.SettlementObservation",
            "amount",
            "occurred_at",
            false,
        ),
    ]
    .map(|(model, measure, time, snapshot)| {
        (
            model.to_owned(),
            measure.to_owned(),
            time.to_owned(),
            snapshot,
        )
    });
    assert_eq!(derived, BTreeSet::from(expected));
    // METRIC-UNIT-PARTITION-1: each sum names where its unit is stored.
    for candidate in space.candidates() {
        let partition: Vec<_> = candidate
            .unit_partition
            .iter()
            .map(|path| path.property.as_str())
            .collect();
        let expected: &[&str] = match candidate.definition.type_params.expr.as_deref() {
            Some("quantity") => &["unit", "currency"],
            Some("amount") => &["currency"],
            // A stock's cost is held per its currency source.
            Some("stock_cost") => &["cost_currency"],
            _ => &[],
        };
        assert_eq!(partition, expected, "{}", candidate.definition.name);
    }
}
