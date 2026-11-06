//! HOST-READ-4 correspondences: an object's links are listed only when the
//! caller may see both endpoints, and a link to a hidden row is neither listed
//! nor counted; lineage needs Values and Sources, and refuses opaquely.
#![cfg(not(target_arch = "wasm32"))]
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    missing_docs,
    reason = "object link correspondence"
)]

use norn_api::authz::{AccessRequest, Authorizer, ReadCapability, ReadRequest};
use norn_api::host::DevHost;
use norn_api::object_links::{ObjectLinkParams, get_object_lineage, get_object_links};
use norn_api::object_routes::ObjectAccess;
use norn_api::objects::NornObjectLinkPage;
use norn_api::refusal::Refusal;
use norn_api::scope::{CellScope, ScopeQuery};
use norn_cell::engine::NativeEngine;
use norn_cell::{CellStore, CommitBatch, LinkChange, MAIN_WORLD, Model, ObjectRecord};
use norn_ir::FactChange;
use norn_kernel::authz::{
    AccessRequest as PdpRequest, ActorKind, Delegation, PrincipalInput, ResourceInput, ScopeKey,
};
use std::collections::BTreeMap;

const TYPE: &str = "test.Node";
/// The row `few` may not see at all.
const HIDDEN: &str = "h";

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

/// `full` reads everything; `few` everything but [`HIDDEN`]; `pop` every
/// row's existence and no value; `nosrc` existence and values, no sources.
struct Policy;
impl Authorizer for Policy {
    fn authorize(&self, _: &AccessRequest<'_>) -> Result<(), Refusal> {
        Ok(())
    }
    fn read(&self, request: &ReadRequest<'_>) -> Result<(), Refusal> {
        let resource = request.access.resource.id.as_str();
        let allowed = match request.access.principal.subject_id.as_str() {
            "full" => true,
            "few" => resource != HIDDEN,
            "pop" => request.capability == ReadCapability::Population,
            "nosrc" => request.capability != ReadCapability::Sources,
            _ => false,
        };
        if allowed {
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

fn model() -> Model {
    let node = serde_json::from_str(
        r#"{"name":"test.Node","properties":[{"id":"amount","type":{"kind":"decimal","scale":6}}],
            "links":[{"id":"related","target":"test.Node","cardinality":{"to":"many","from":"many"}}]}"#,
    )
    .unwrap();
    Model::from_definitions(&[node], &[]).unwrap()
}

fn create(key: &str) -> FactChange {
    serde_json::from_str(&format!(
        r#"{{"op":"create","objectType":"test.Node","key":"{key}","values":{{"amount":1.5}}}}"#
    ))
    .unwrap()
}

fn link(source: &str, target: &str) -> LinkChange {
    LinkChange::Add {
        object_type: TYPE.to_owned(),
        link: "related".to_owned(),
        source: source.to_owned(),
        target: target.to_owned(),
        values: BTreeMap::default(),
        target_type: None,
    }
}

/// Nodes `a`, `b`, `c`, `d` and [`HIDDEN`]; `a` links to every other node,
/// and `b`, `h` and `d` link back to it.
fn host() -> (DevHost<NativeEngine>, tempfile::TempDir) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cell.sqlite");
    {
        let mut store =
            CellStore::open(&path, model(), norn_cell::StoreOptions::default()).unwrap();
        store
            .commit(&CommitBatch {
                world: MAIN_WORLD.to_owned(),
                scope: physical().key(),
                logical_time: 1,
                facts: ["a", "b", "c", "d", HIDDEN].into_iter().map(create).collect(),
                links: vec![
                    link("a", "b"),
                    link("a", "c"),
                    link("a", "d"),
                    link("a", HIDDEN),
                    link("b", "a"),
                    link(HIDDEN, "a"),
                    link("d", "a"),
                ],
                ..CommitBatch::default()
            })
            .unwrap();
    }
    let store = CellStore::open(&path, model(), norn_cell::StoreOptions::default()).unwrap();
    let host = DevHost::from_store(store, physical(), |command| command.payload.clone())
        .unwrap()
        .with_authorizer(Policy);
    (host, directory)
}

fn params(direction: Option<&str>, limit: Option<&str>) -> ObjectLinkParams {
    ObjectLinkParams {
        direction: direction.map(str::to_owned),
        link: None,
        limit: limit.map(str::to_owned),
        after: None,
    }
}

fn links(
    host: &DevHost<NativeEngine>,
    actor: &str,
    id: &str,
    params: &ObjectLinkParams,
) -> Result<NornObjectLinkPage, Refusal> {
    get_object_links(host, &scope(), Some(actor), TYPE, id, params, &Mapping)
}

/// Every page, following `next`.
fn pages(host: &DevHost<NativeEngine>, actor: &str, params: &ObjectLinkParams) -> Vec<NornObjectLinkPage> {
    let mut all = Vec::new();
    let mut params = params.clone();
    loop {
        let page = links(host, actor, "a", &params).unwrap();
        params.after.clone_from(&page.next);
        all.push(page);
        if params.after.is_none() {
            return all;
        }
    }
}

fn edges(pages: &[NornObjectLinkPage]) -> Vec<(String, String)> {
    pages
        .iter()
        .flat_map(|page| page.items.iter().map(|edge| (edge.source.clone(), edge.target.clone())))
        .collect()
}

/// A link to a row the caller cannot see is neither listed nor counted: the
/// caller's pages are exactly the full pages without it, at every page size.
#[test]
fn a_link_to_a_hidden_row_is_neither_listed_nor_counted() {
    let (host, _dir) = host();
    let everything = edges(&pages(&host, "full", &params(None, None)));
    assert_eq!(everything.len(), 7);
    let visible: Vec<_> = everything
        .iter()
        .filter(|(source, target)| source != HIDDEN && target != HIDDEN)
        .cloned()
        .collect();
    for limit in ["1", "2", "5", "50"] {
        let few = pages(&host, "few", &params(None, Some(limit)));
        assert_eq!(edges(&few), visible, "limit {limit}");
        let wire = serde_json::to_string(&few).unwrap();
        assert!(!wire.contains("\"h\""), "limit {limit}: {wire}");
        // A last page names no next page: none of the hidden links remain.
        assert!(few.last().unwrap().next.is_none(), "limit {limit}");
        assert!(few.iter().all(|page| page.items.len() <= limit.parse().unwrap()));
    }
    let out = edges(&pages(&host, "few", &params(Some("out"), None)));
    assert_eq!(out, [("a", "b"), ("a", "c"), ("a", "d")].map(|(s, t)| (s.to_owned(), t.to_owned())));
}

/// Links are existence-level: a caller holding Population alone sees them.
#[test]
fn a_population_only_caller_sees_links() {
    let (host, _dir) = host();
    assert_eq!(
        edges(&pages(&host, "pop", &params(None, None))),
        edges(&pages(&host, "full", &params(None, None)))
    );
}

/// A hidden or absent requested object refuses the same way.
#[test]
fn a_hidden_object_refuses_like_an_absent_one() {
    let (host, _dir) = host();
    let hidden = links(&host, "few", HIDDEN, &params(None, None)).unwrap_err();
    let absent = links(&host, "few", "zz", &params(None, None)).unwrap_err();
    assert_eq!(hidden.status, 404);
    assert_eq!(
        serde_json::to_string(&hidden).unwrap(),
        serde_json::to_string(&absent).unwrap()
    );
}

/// Every violation is reported at once, each at its pointer.
#[test]
fn an_invalid_links_query_lists_every_violation() {
    let (host, _dir) = host();
    let refused = links(
        &host,
        "full",
        "a",
        &ObjectLinkParams {
            direction: Some("sideways".into()),
            link: Some("ghost".into()),
            limit: Some("0".into()),
            after: None,
        },
    )
    .unwrap_err();
    assert_eq!(refused.status, 400);
    let found: Vec<(String, String)> = refused
        .diagnostics
        .iter()
        .map(|d| (d.code.clone(), d.span.as_ref().unwrap().pointer.clone()))
        .collect();
    for (code, pointer) in [
        ("query.value_invalid", "/direction"),
        ("query.property_unknown", "/link"),
        ("query.limit_invalid", "/limit"),
    ] {
        assert!(found.contains(&(code.to_owned(), pointer.to_owned())), "{found:?}");
    }
}

/// A cursor continues only its own object, query and caller at its own
/// snapshot: anything else is 400 at `/after`, and a commit since is 409.
#[test]
fn a_links_cursor_binds_its_object_query_caller_and_snapshot() {
    let (host, dir) = host();
    let first = links(&host, "full", "a", &params(None, Some("1"))).unwrap();
    let next = first.next.unwrap();
    let with = |base: ObjectLinkParams| ObjectLinkParams {
        after: Some(next.clone()),
        ..base
    };
    let invalid = |refused: Refusal| {
        assert_eq!(refused.status, 400);
        assert_eq!(refused.diagnostics[0].code, "query.cursor_invalid");
        assert_eq!(refused.diagnostics[0].span.as_ref().unwrap().pointer, "/after");
    };
    invalid(links(&host, "few", "a", &with(params(None, Some("1")))).unwrap_err());
    invalid(links(&host, "full", "a", &with(params(Some("out"), Some("1")))).unwrap_err());
    invalid(links(&host, "full", "b", &with(params(None, Some("1")))).unwrap_err());
    let tampered = ObjectLinkParams {
        after: Some(format!("{next}x")),
        ..params(None, Some("1"))
    };
    invalid(links(&host, "full", "a", &tampered).unwrap_err());
    assert!(links(&host, "full", "a", &with(params(None, Some("3")))).is_ok());
    let mut store = CellStore::open(
        &dir.path().join("cell.sqlite"),
        model(),
        norn_cell::StoreOptions::default(),
    )
    .unwrap();
    store
        .commit(&CommitBatch {
            world: MAIN_WORLD.to_owned(),
            scope: physical().key(),
            logical_time: 2,
            facts: vec![create("e")],
            ..CommitBatch::default()
        })
        .unwrap();
    drop(store);
    let moved = links(&host, "full", "a", &with(params(None, Some("1")))).unwrap_err();
    assert_eq!(
        (moved.status, moved.code.as_str(), moved.retryable),
        (409, "object.snapshot_moved", true)
    );
}

/// Lineage needs Values and Sources; without either, and for an object with
/// no retained source bundle, it refuses with the one opaque 404.
#[test]
fn lineage_refuses_opaquely_without_values_sources_or_a_bundle() {
    let (host, _dir) = host();
    let lineage = |actor, id| {
        get_object_lineage(&host, &scope(), Some(actor), TYPE, id, &Mapping).unwrap_err()
    };
    let absent = serde_json::to_string(&lineage("full", "zz")).unwrap();
    for (actor, id) in [("full", "a"), ("pop", "a"), ("nosrc", "a"), ("few", HIDDEN)] {
        let refused = lineage(actor, id);
        assert_eq!(refused.status, 404, "{actor} {id}");
        assert_eq!(serde_json::to_string(&refused).unwrap(), absent, "{actor} {id}");
    }
}
