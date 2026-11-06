//! HOST-READ-2 correspondences: per-row authorization before data quality, so
#![cfg(not(target_arch = "wasm32"))]
//! no page, count or cursor discloses rows the principal cannot read.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    missing_docs,
    reason = "object read correspondence"
)]
#[path = "../../norn-cell/tests/query_support/mod.rs"]
mod query_support;

use norn_api::authz::{AccessRequest, Authorizer, ReadCapability, ReadRequest};
use norn_api::host::DevHost;
use norn_api::object_routes::{ObjectAccess, get_object, get_object_page};
use norn_api::refusal::Refusal;
use norn_api::scope::{CellScope, ScopeQuery};
use norn_cell::engine::NativeEngine;
use norn_cell::{CellStore, CommitBatch, MAIN_WORLD, Model, ObjectRecord};
use norn_kernel::authz::{
    AccessRequest as PdpRequest, ActorKind, Delegation, PrincipalInput, ResourceInput, ScopeKey,
};
use query_support::{TYPE, capture_quality, fixture, fixture_writes};
use serde_json::json;
use std::collections::BTreeMap;

const HIDDEN: &str = "SYN-MELT-15-0";
const QUARANTINED: &str = "SYN-MELT-15-1";

fn physical() -> CellScope {
    CellScope {
        org_id: "org_demo".into(),
        environment_id: "dev".into(),
    }
}
fn scope() -> ScopeQuery {
    ScopeQuery {
        org_id: Some("org_demo".into()),
        environment_id: Some("dev".into()),
    }
}

/// `reader` reads every row; `partial` is denied Values on one row.
struct Policy;
impl Authorizer for Policy {
    fn authorize(&self, _: &AccessRequest<'_>) -> Result<(), Refusal> {
        Ok(())
    }
    fn read(&self, request: &ReadRequest<'_>) -> Result<(), Refusal> {
        let actor = request.access.principal.subject_id.as_str();
        let hidden = actor == "partial"
            && request.capability == ReadCapability::Values
            && request.access.resource.id == HIDDEN;
        if ["reader", "partial"].contains(&actor) && !hidden {
            Ok(())
        } else {
            Err(Refusal::invalid("private policy diagnostic"))
        }
    }
}

struct Mapping;
impl ObjectAccess for Mapping {
    fn access(
        &self,
        scope: &CellScope,
        actor: &str,
        capability: ReadCapability,
        object_type: &str,
        row: Option<&ObjectRecord>,
    ) -> Result<PdpRequest, Refusal> {
        let key = ScopeKey {
            org: scope.org_id.clone(),
            environment: Some(scope.environment_id.clone()),
            tenant: None,
            workspace: None,
            business_unit: None,
        };
        Ok(PdpRequest {
            principal: PrincipalInput {
                subject_id: actor.into(),
                actor_kind: ActorKind::User,
                agent_session_id: None,
                roles: vec![],
                attributes: BTreeMap::new(),
            },
            scope: Some(key.clone()),
            capability: format!("fixture.{capability:?}"),
            action_id: "norn.read".into(),
            resource: ResourceInput {
                object_type: row
                    .map_or(object_type, |row| row.object_type.as_str())
                    .into(),
                id: row.map_or(object_type, |row| row.id.as_str()).into(),
                scope: key,
                attributes: BTreeMap::new(),
            },
            context: BTreeMap::new(),
            binding: None,
            delegation: Delegation::Direct,
        })
    }
}

/// The scrap-yield conversions with a recovery-bound law; one record arrives
/// from a source that breaks it and is quarantined by a real finding.
fn host() -> (DevHost<NativeEngine>, tempfile::TempDir) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cell.sqlite");
    let (base, _, _) = fixture();
    let mut object = base.model().object_types().next().unwrap().clone();
    let mut encoded = serde_json::to_value(&object).unwrap();
    encoded["invariants"] = json!([{"id":"recovery_bound","expression":"self.recovered <= self.input","message":"Recovered mass exceeds source input"}]);
    object = serde_json::from_value(encoded).unwrap();
    let model = Model::from_definitions(std::slice::from_ref(&object), &[]).unwrap();
    let artifact = json!({"irVersion":"0.1","artifactId":"demo.Conversion.entity","kind":"entity","namespace":"demo","name":"Conversion","version":"0.1.0","lifecycle":"draft","scope":{"orgId":"org_demo","environmentId":"dev"},"dependencies":[],"provenance":{"source":"authoring_sdk"},"payload":{"entityVersion":"2.0","objectType":object}});
    let key = physical().key();
    {
        let mut store =
            CellStore::open(&path, model.clone(), norn_cell::StoreOptions::default()).unwrap();
        store
            .install_definitions(
                model.clone(),
                norn_cell::InstalledDefinitionInput {
                    canonical_set: serde_json::to_string(&vec![artifact]).unwrap(),
                    registry: vec![],
                    receipt_sha256: "synthetic-quality-v1".into(),
                    admission_receipt: None,
                    readmission: None,
                    diff: json!({}),
                    installed_at: query_support::CLOCK,
                    layout: None,
                },
                Some(0),
                &key,
            )
            .unwrap();
        store
            .commit(&CommitBatch {
                world: MAIN_WORLD.into(),
                scope: key.clone(),
                logical_time: query_support::CLOCK,
                writes: fixture_writes(),
                ..CommitBatch::default()
            })
            .unwrap();
        let mut cell = norn_cell::SqliteCell::new(store, &key, |_| BTreeMap::new()).unwrap();
        capture_quality(&mut cell, 80_000, "bad-source");
    }
    let store = CellStore::open(&path, model, norn_cell::StoreOptions::default()).unwrap();
    let host = DevHost::from_store(store, physical(), |command| command.payload.clone())
        .unwrap()
        .with_authorizer(Policy);
    (host, directory)
}

fn all_pages(
    host: &DevHost<NativeEngine>,
    actor: &str,
    limit: usize,
) -> Vec<norn_api::objects::NornObjectPage> {
    let mut pages = Vec::new();
    let mut after: Option<String> = None;
    loop {
        let page = get_object_page(
            host,
            &scope(),
            Some(actor),
            TYPE,
            after.as_deref(),
            limit,
            &Mapping,
        )
        .unwrap();
        after.clone_from(&page.scanned_through);
        pages.push(page);
        if after.is_none() {
            return pages;
        }
    }
}

#[test]
fn quarantine_is_counted_only_among_visible_rows_and_never_listed() {
    let (host, _dir) = host();
    let pages = all_pages(&host, "reader", 7);
    let ids: Vec<_> = pages
        .iter()
        .flat_map(|p| p.records.iter().map(|r| r.id.clone()))
        .collect();
    assert!(!ids.contains(&QUARANTINED.to_owned()));
    assert!(ids.contains(&HIDDEN.to_owned()));
    assert_eq!(pages.iter().map(|p| p.quarantine_count).sum::<u64>(), 1);
    assert_eq!(fixture_writes().len(), ids.len() + 1);
}

#[test]
fn an_unreadable_row_is_absent_from_records_counts_and_cursors() {
    let (host, _dir) = host();
    for limit in [1, 3, 7, 100] {
        let pages = all_pages(&host, "partial", limit);
        for page in &pages {
            assert!(page.records.iter().all(|r| r.id != HIDDEN), "limit {limit}");
            assert_ne!(
                page.scanned_through.as_deref(),
                Some(HIDDEN),
                "limit {limit}"
            );
            let wire = serde_json::to_string(page).unwrap();
            assert!(!wire.contains(HIDDEN), "limit {limit}: {wire}");
        }
        let total: usize = pages.iter().map(|p| p.records.len()).sum();
        assert_eq!(total, fixture_writes().len() - 2, "limit {limit}");
    }
}

#[test]
fn absent_invisible_and_quarantined_objects_share_one_refusal() {
    let (host, _dir) = host();
    let get = |actor, id| get_object(&host, &scope(), Some(actor), TYPE, id, &Mapping);
    assert_eq!(get("reader", HIDDEN).unwrap().id, HIDDEN);
    let absent = serde_json::to_string(&get("reader", "SYN-NOT-THERE").unwrap_err()).unwrap();
    for (actor, id) in [("partial", HIDDEN), ("reader", QUARANTINED)] {
        let refused = get(actor, id).unwrap_err();
        assert_eq!(refused.status, 404);
        assert_eq!(
            serde_json::to_string(&refused).unwrap(),
            absent,
            "{actor} {id}"
        );
    }
    let undeclared = get_object(
        &host,
        &scope(),
        Some("reader"),
        "demo.Other",
        HIDDEN,
        &Mapping,
    )
    .unwrap_err();
    assert_eq!(serde_json::to_string(&undeclared).unwrap(), absent);
}

#[test]
fn denied_and_foreign_scope_reads_share_one_opaque_denial() {
    let (host, _dir) = host();
    let anonymous = serde_json::to_string(
        &get_object_page(&host, &scope(), None, TYPE, None, 10, &Mapping).unwrap_err(),
    )
    .unwrap();
    let foreign = ScopeQuery {
        org_id: Some("org_other".into()),
        environment_id: Some("dev".into()),
    };
    for (query, actor) in [(scope(), "stranger"), (foreign, "reader")] {
        let page =
            get_object_page(&host, &query, Some(actor), TYPE, None, 10, &Mapping).unwrap_err();
        assert_eq!(serde_json::to_string(&page).unwrap(), anonymous);
        let one = get_object(&host, &query, Some(actor), TYPE, HIDDEN, &Mapping).unwrap_err();
        assert_eq!(serde_json::to_string(&one).unwrap(), anonymous);
    }
    assert!(!anonymous.contains("org_demo") && !anonymous.contains("private policy"));
}

#[test]
fn page_bounds_refuse_before_reading() {
    let (host, _dir) = host();
    for limit in [0, norn_api::object_routes::MAX_PAGE + 1] {
        let refused = get_object_page(&host, &scope(), Some("reader"), TYPE, None, limit, &Mapping)
            .unwrap_err();
        assert_eq!(refused.status, 400, "{limit}");
    }
}
