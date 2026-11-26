//! The catalog's per-type record counts (`objectCounts` on
//! `GET /v1/definitions`) are the records list's own first-page totals: under
//! any grant of collections and rows, a type the caller may not list has no
//! count, and a listed type counts exactly the rows the caller may see, never
//! one it may not, up to the catalog's bound.
#![cfg(not(target_arch = "wasm32"))]
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    missing_docs,
    reason = "catalog count correspondence"
)]
#[path = "../../norn-cell/tests/query_support/mod.rs"]
mod query_support;

#[path = "support/hosted_read.rs"]
mod hosted_read;
use hosted_read::Mapping;
use norn_api::authz::{AccessRequest, Authorizer, ReadCapability, ReadRequest};
use norn_api::host::DevHost;
use norn_api::object_query::ObjectQueryParams;
use norn_api::object_routes::get_object_query;
use norn_api::objects::NornQueryTotal;
use norn_api::refusal::Refusal;
use norn_api::routes::object_counts;
use norn_api::scope::{CellScope, ScopeQuery};
use norn_cell::engine::NativeEngine;
// #1485 S6: fixtures write through the kernel path, never the raw door.
use norn_cell::{CellStore, CommitBatch, FactWriteRow, MAIN_WORLD};
use norn_ir::RuntimeValue as V;
use proptest::prelude::*;
use query_support::governed::GovernedCommit as _;
use query_support::{CLOCK, TYPE, object_type};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};

const OTHER: &str = "demo.Other";
const CALLER: &str = "caller";

fn physical() -> CellScope {
    CellScope {
        org_id: "org_demo".into(),
        environment_id: "dev".into(),
    }
}

/// What the caller holds: Population on some collections, and Population or
/// Values on some rows. Nothing else is granted, to anyone.
struct Grants {
    collections: BTreeSet<String>,
    population: BTreeSet<String>,
    values: BTreeSet<String>,
}

impl Authorizer for Grants {
    fn authorize(&self, _: &AccessRequest<'_>) -> Result<(), Refusal> {
        Ok(())
    }
    fn read(&self, request: &ReadRequest<'_>) -> Result<(), Refusal> {
        let resource = request.access.resource();
        let id = resource.id.as_str();
        let allowed = request.access.principal().subject_id == CALLER
            && if id == resource.object_type {
                request.capability == ReadCapability::Population && self.collections.contains(id)
            } else {
                match request.capability {
                    ReadCapability::Population => self.population.contains(id),
                    ReadCapability::Values => self.values.contains(id),
                    ReadCapability::Sources => false,
                }
            };
        if allowed {
            Ok(())
        } else {
            Err(Refusal::invalid("private policy diagnostic"))
        }
    }
}

/// The fixture type renamed `name`, every property optional.
fn typed(name: &str) -> norn_ir::ObjectTypeDef {
    let mut encoded = serde_json::to_value(object_type()).unwrap();
    encoded["name"] = json!(name);
    for property in encoded["properties"].as_array_mut().unwrap() {
        property["constraints"] = json!({"nullable": true});
    }
    serde_json::from_value(encoded).unwrap()
}

/// A cell holding `rows` (type, id) under `grants`.
fn host(rows: &[(&str, String)], grants: Grants) -> DevHost<NativeEngine> {
    let types = [typed(TYPE), typed(OTHER)];
    let model = norn_cell::Model::from_definitions(&types, &[]).unwrap();
    let artifacts: Vec<serde_json::Value> = types
        .iter()
        .map(|object| {
            let name = object.name.as_str();
            json!({"irVersion":"0.1","artifactId":format!("{name}.entity"),"kind":"entity","namespace":"demo","name":name.trim_start_matches("demo."),"version":"0.1.0","lifecycle":"draft","scope":{"orgId":"org_demo","environmentId":"dev"},"dependencies":[],"provenance":{"source":"authoring_sdk"},"payload":{"entityVersion":"2.0","objectType":object}})
        })
        .collect();
    let key = physical().key();
    let mut store = CellStore::provision_definitions(
        NativeEngine::open_in_memory().unwrap(),
        model,
        &norn_cell::InstalledDefinitionInput {
            canonical_set: serde_json::to_string(&artifacts).unwrap(),
            registry: vec![],
            receipt_sha256: "synthetic-catalog-counts".into(),
            admission_receipt: None,
            readmission: None,
            diff: json!({}),
            installed_at: CLOCK,
            layout: None,
        },
        &key,
    )
    .unwrap();
    if !rows.is_empty() {
        store
            .commit(&CommitBatch {
                world: MAIN_WORLD.into(),
                scope: key,
                logical_time: CLOCK,
                writes: rows
                    .iter()
                    .map(|(object_type, id)| FactWriteRow::Put {
                        object_type: (*object_type).into(),
                        key: id.clone(),
                        values: BTreeMap::from([(
                            "occurred_at".to_owned(),
                            V::from("2026-07-10T12:00:00Z"),
                        )]),
                    })
                    .collect(),
                ..CommitBatch::default()
            })
            .unwrap();
    }
    DevHost::from_store(store, physical(), |command| command.payload.clone())
        .unwrap()
        .with_authorizer(grants)
}

/// One row of a type: whether the caller holds Population and Values on it.
fn rows() -> impl Strategy<Value = Vec<(bool, bool)>> {
    proptest::collection::vec((any::<bool>(), any::<bool>()), 0..9)
}

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(96))]
    #[test]
    fn a_count_is_the_lists_total_and_an_unlistable_type_has_none(
        a in rows(),
        b in rows(),
        listable in (any::<bool>(), any::<bool>()),
        bound in 1_usize..10,
        anonymous in any::<bool>(),
        foreign in any::<bool>(),
    ) {
        let mut grants = Grants {
            collections: BTreeSet::new(),
            population: BTreeSet::new(),
            values: BTreeSet::new(),
        };
        let mut stored = Vec::new();
        let mut visible = BTreeMap::from([(TYPE, 0_u64), (OTHER, 0_u64)]);
        for (object_type, prefix, held, list) in [(TYPE, "A", &a, listable.0), (OTHER, "B", &b, listable.1)] {
            if list {
                grants.collections.insert(object_type.to_owned());
            }
            for (index, (population, values)) in held.iter().enumerate() {
                let id = format!("{prefix}-{index:02}");
                if *population {
                    grants.population.insert(id.clone());
                    *visible.get_mut(object_type).unwrap() += 1;
                }
                if *values {
                    grants.values.insert(id.clone());
                }
                stored.push((object_type, id));
            }
        }
        let cell = host(&stored, grants);
        let scope = ScopeQuery {
            org_id: Some(if foreign { "org_foreign" } else { "org_demo" }.into()),
            environment_id: Some("dev".into()),
        };
        let actor = (!anonymous).then_some(CALLER);
        let counts = object_counts(&cell, &scope, actor, &Mapping, bound);
        let limit = u64::try_from(bound).unwrap();
        for (object_type, list) in [(TYPE, listable.0), (OTHER, listable.1)] {
            let expected = (list && !anonymous && !foreign).then(|| {
                let seen = visible[object_type];
                NornQueryTotal { count: seen.min(limit), approximate: seen > limit }
            });
            prop_assert_eq!(counts.get(object_type).copied(), expected, "{}", object_type);
            // The list itself agrees: it refuses where the catalog has no
            // count, and reports the same total where the count is exact.
            let listed = get_object_query(&cell, &scope, actor, object_type, &ObjectQueryParams::default(), &Mapping);
            match expected {
                None => prop_assert!(listed.is_err()),
                Some(total) if !total.approximate => prop_assert_eq!(listed.unwrap().total, Some(total)),
                Some(_) => prop_assert_eq!(listed.unwrap().total.map(|total| total.count), Some(visible[object_type])),
            }
        }
        prop_assert!(counts.keys().all(|name| name == TYPE || name == OTHER));
    }
}
