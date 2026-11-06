//! HOST-READ-2 correspondences: per-row authorization before data quality, so
//! no page, count or cursor discloses rows the principal cannot read.
#![cfg(not(target_arch = "wasm32"))]
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
use query_support::{TYPE, capture_quality, fixture, fixture_writes, put};
use serde_json::json;
use std::collections::BTreeMap;

const HIDDEN: &str = "SYN-MELT-15-0";
const QUARANTINED: &str = "SYN-MELT-15-1";
/// The only rows `few` may read.
const FEW: [&str; 3] = ["SYN-MELT-00-0", "SYN-MELT-05-0", "SYN-MELT-10-0"];

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

/// `reader` reads every row; `partial` is denied Values on one row; `few`
/// reads the collection and only the rows in [`FEW`].
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
        let resource = request.access.resource.id.as_str();
        // Values without row Population, and Population without the quarantined row's Values.
        let withheld = match actor {
            "nopop" => request.capability == ReadCapability::Population && resource == HIDDEN,
            "noq" => request.capability == ReadCapability::Values && resource == QUARANTINED,
            _ => false,
        };
        if ["nopop", "noq"].contains(&actor) {
            return if withheld {
                Err(Refusal::invalid("private policy diagnostic"))
            } else {
                Ok(())
            };
        }
        if actor == "few" {
            return if resource == TYPE || FEW.contains(&resource) {
                Ok(())
            } else {
                Err(Refusal::invalid("private policy diagnostic"))
            };
        }
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
    host_with(0)
}

/// The conversions' model with the recovery-bound law, and its object type.
fn quality_model() -> (Model, norn_ir::ObjectTypeDef) {
    let (base, _, _) = fixture();
    let mut object = base.model().object_types().next().unwrap().clone();
    let mut encoded = serde_json::to_value(&object).unwrap();
    encoded["invariants"] = json!([{"id":"recovery_bound","expression":"self.recovered <= self.input","message":"Recovered mass exceeds source input"}]);
    object = serde_json::from_value(encoded).unwrap();
    (
        Model::from_definitions(std::slice::from_ref(&object), &[]).unwrap(),
        object,
    )
}

/// [`host`] with `hidden` extra conversions no fixture principal but `reader`
/// and `partial` may read; their ids sort before every fixture row.
fn host_with(hidden: usize) -> (DevHost<NativeEngine>, tempfile::TempDir) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cell.sqlite");
    let (model, object) = quality_model();
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
                writes: fixture_writes()
                    .into_iter()
                    .chain((0..hidden).map(|index| {
                        put(
                            &format!("SYN-HIDDEN-{index:05}"),
                            1_000,
                            900,
                            "2026-07-10T12:00:00Z",
                            "gold",
                        )
                    }))
                    .collect(),
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

/// Pages are the ones the principal would get if unreadable rows did not
/// exist: thousands of hidden rows change no record, count, cursor or refusal.
#[test]
fn pages_do_not_depend_on_how_many_rows_are_unreadable() {
    let (small, _small_dir) = host_with(0);
    let (large, _large_dir) = host_with(1_500);
    for limit in [1, 2, 100] {
        let without: Vec<String> = all_pages(&small, "few", limit)
            .iter()
            .map(|page| serde_json::to_string(page).unwrap())
            .collect();
        let with: Vec<String> = all_pages(&large, "few", limit)
            .iter()
            .map(|page| serde_json::to_string(page).unwrap())
            .collect();
        assert_eq!(with, without, "limit {limit}");
        assert!(
            with.iter().all(|page| !page.contains("SYN-HIDDEN")),
            "limit {limit}"
        );
        let ids: usize = all_pages(&large, "few", limit)
            .iter()
            .map(|page| page.records.len())
            .sum();
        assert_eq!(ids, FEW.len(), "limit {limit}");
    }
}

#[test]
fn values_without_row_population_never_shows_the_row() {
    let (host, _dir) = host();
    for limit in [1, 7, 100] {
        let pages = all_pages(&host, "nopop", limit);
        assert!(
            pages
                .iter()
                .all(|p| !serde_json::to_string(p).unwrap().contains(HIDDEN)),
            "limit {limit}"
        );
        let total: usize = pages.iter().map(|p| p.records.len()).sum();
        assert_eq!(
            total,
            fixture_writes().len() - 2,
            "limit {limit}: the hidden and the quarantined rows"
        );
    }
    let refused = get_object(&host, &scope(), Some("nopop"), TYPE, HIDDEN, &Mapping).unwrap_err();
    assert_eq!(refused.status, 404);
}

#[test]
fn an_unreadable_quarantined_row_is_not_counted() {
    let (host, _dir) = host();
    let pages = all_pages(&host, "noq", 100);
    assert_eq!(pages.iter().map(|p| p.quarantine_count).sum::<u64>(), 0);
    assert!(
        pages
            .iter()
            .all(|p| !serde_json::to_string(p).unwrap().contains(QUARANTINED))
    );
}

/// A mapping that commits through a second connection the first time it maps a row.
struct Committing {
    path: std::path::PathBuf,
    model: Model,
    fired: std::sync::atomic::AtomicBool,
}
impl ObjectAccess for Committing {
    fn access(
        &self,
        scope: &CellScope,
        actor: &str,
        capability: ReadCapability,
        object_type: &str,
        row: Option<&ObjectRecord>,
    ) -> Result<PdpRequest, Refusal> {
        if row.is_some() && !self.fired.swap(true, std::sync::atomic::Ordering::SeqCst) {
            let mut store = CellStore::open(
                &self.path,
                self.model.clone(),
                norn_cell::StoreOptions::default(),
            )
            .map_err(Refusal::host)?;
            store
                .commit(&CommitBatch {
                    world: MAIN_WORLD.into(),
                    scope: physical().key(),
                    logical_time: query_support::CLOCK + 10_000_000,
                    writes: vec![put("SYN-LATE", 1_000, 900, "2026-07-10T12:00:00Z", "gold")],
                    ..CommitBatch::default()
                })
                .map_err(Refusal::host)?;
        }
        Mapping.access(scope, actor, capability, object_type, row)
    }
}

/// The scan releases the cell between batches; a commit in between refuses
/// the page as retryable rather than mixing two snapshots.
#[test]
fn a_commit_during_the_scan_refuses_the_page_as_retryable() {
    let (host, dir) = host();
    let committing = Committing {
        path: dir.path().join("cell.sqlite"),
        model: quality_model().0,
        fired: std::sync::atomic::AtomicBool::new(false),
    };
    let refused = get_object_page(
        &host,
        &scope(),
        Some("reader"),
        TYPE,
        None,
        100,
        &committing,
    )
    .unwrap_err();
    assert_eq!(
        (refused.status, refused.code.as_str(), refused.retryable),
        (409, "object.snapshot_moved", true)
    );
    let retried =
        get_object_page(&host, &scope(), Some("reader"), TYPE, None, 100, &Mapping).unwrap();
    assert!(retried.records.iter().any(|r| r.id == "SYN-LATE"));
}

/// Commits through a second connection inside the `nth` locked read section of
/// a page, after its opening head check (the fault-injection seam).
struct CommitsInside {
    path: std::path::PathBuf,
    model: Model,
    nth: usize,
    sections: std::sync::atomic::AtomicUsize,
}
impl ObjectAccess for CommitsInside {
    fn access(
        &self,
        scope: &CellScope,
        actor: &str,
        capability: ReadCapability,
        object_type: &str,
        row: Option<&ObjectRecord>,
    ) -> Result<PdpRequest, Refusal> {
        Mapping.access(scope, actor, capability, object_type, row)
    }
    fn within_snapshot(&self) {
        let section = self
            .sections
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            .saturating_add(1);
        if section == self.nth {
            let mut store = CellStore::open(
                &self.path,
                self.model.clone(),
                norn_cell::StoreOptions::default(),
            )
            .expect("second connection");
            store
                .commit(&CommitBatch {
                    world: MAIN_WORLD.into(),
                    scope: physical().key(),
                    logical_time: query_support::CLOCK + 20_000_000,
                    writes: vec![put(
                        "SYN-INSIDE",
                        1_000,
                        900,
                        "2026-07-10T12:00:00Z",
                        "gold",
                    )],
                    ..CommitBatch::default()
                })
                .expect("commit inside the section");
        }
    }
}

/// A commit after a section's opening head check and before its reads (the
/// batch read, or the quarantine reads) refuses the page: the closing check
/// sees the moved head, so quarantine state from one snapshot never filters
/// rows read at another.
#[test]
fn a_commit_inside_a_read_section_refuses_the_page() {
    for (nth, section) in [(1, "batch read"), (2, "quarantine reads")] {
        let (host, dir) = host();
        let commits = CommitsInside {
            path: dir.path().join("cell.sqlite"),
            model: quality_model().0,
            nth,
            sections: std::sync::atomic::AtomicUsize::new(0),
        };
        let refused = get_object_page(&host, &scope(), Some("reader"), TYPE, None, 100, &commits)
            .unwrap_err();
        assert_eq!(
            (refused.status, refused.code.as_str(), refused.retryable),
            (409, "object.snapshot_moved", true),
            "{section}"
        );
    }
}
