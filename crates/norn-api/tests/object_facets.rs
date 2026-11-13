//! HOST-READ-7 correspondences: a facet counts exactly the rows a grid page
//! with the same filter lists; a row the caller cannot see, or whose values
//! are withheld, is never counted; only text, choice and yes/no properties
//! are counted, and anything else is refused at its pointer.
#![cfg(not(target_arch = "wasm32"))]
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    missing_docs,
    reason = "object facet correspondence"
)]

use norn_api::authz::{AccessRequest, Authorizer, ReadCapability, ReadRequest};
use norn_api::host::DevHost;
use norn_api::object_facets::{NornObjectFacets, ObjectFacetParams, get_object_facets};
use norn_api::object_routes::ObjectAccess;
use norn_api::refusal::Refusal;
use norn_api::scope::{CellScope, ScopeQuery};
use norn_cell::engine::NativeEngine;
use norn_cell::{CellStore, CommitBatch, MAIN_WORLD, Model, ObjectRecord};
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
        r#"{"name":"test.Node","properties":[
            {"id":"status","type":{"kind":"enum","values":[{"value":"open"},{"value":"closed"}]}},
            {"id":"region","type":{"kind":"text"},"constraints":{"nullable":true}},
            {"id":"amount","type":{"kind":"decimal","scale":2}}]}"#,
    )
    .unwrap();
    Model::from_definitions(&[node], &[]).unwrap()
}

fn create(key: &str, status: &str, region: Option<&str>) -> FactChange {
    let region = region.map_or_else(|| "null".to_owned(), |region| format!(r#""{region}""#));
    serde_json::from_str(&format!(
        r#"{{"op":"create","objectType":"test.Node","key":"{key}","values":{{"status":"{status}","region":{region},"amount":1.5}}}}"#
    ))
    .unwrap()
}

/// Five rows: three open (one in no region, one hidden from `few`), two closed.
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
                facts: vec![
                    create("a", "open", Some("north")),
                    create("b", "open", Some("south")),
                    create("c", "closed", Some("north")),
                    create("d", "closed", None),
                    create(HIDDEN, "open", Some("north")),
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

fn facets(
    host: &DevHost<NativeEngine>,
    actor: &str,
    properties: Option<&str>,
    filter: Option<&str>,
) -> Result<NornObjectFacets, Refusal> {
    let params = ObjectFacetParams {
        properties: properties.map(str::to_owned),
        filter: filter.map(str::to_owned),
    };
    get_object_facets(host, &scope(), Some(actor), TYPE, &params, &Mapping)
}

/// Each facet as `(value, count)` pairs, nulls and the tail.
fn read(found: &NornObjectFacets, property: &str) -> (Vec<(String, u64)>, u64, u64) {
    let facet = found
        .facets
        .iter()
        .find(|facet| facet.property == property)
        .unwrap();
    let values = facet
        .values
        .iter()
        .map(|value| {
            let text = value
                .value
                .as_str()
                .map_or_else(|| format!("{:?}", value.value), str::to_owned);
            (text, value.count)
        })
        .collect();
    (values, facet.nulls, facet.other_rows)
}

#[test]
fn every_categorical_property_is_counted_exactly_most_rows_first() {
    let (host, _dir) = host();
    let found = facets(&host, "full", None, None).unwrap();
    assert_eq!(found.rows, 5);
    assert!(!found.approximate);
    assert_eq!(
        found
            .facets
            .iter()
            .map(|facet| facet.property.as_str())
            .collect::<Vec<_>>(),
        ["region", "status"]
    );
    assert_eq!(
        read(&found, "status"),
        (vec![("open".into(), 3), ("closed".into(), 2)], 0, 0)
    );
    assert_eq!(
        read(&found, "region"),
        (vec![("north".into(), 3), ("south".into(), 1)], 1, 0)
    );
}

#[test]
fn a_filter_narrows_the_counts_to_the_rows_a_page_lists() {
    let (host, _dir) = host();
    let found = facets(
        &host,
        "full",
        Some("region"),
        Some(r#"{"all":[{"property":"status","op":"eq","value":"open"}]}"#),
    )
    .unwrap();
    assert_eq!(found.rows, 3);
    assert_eq!(
        read(&found, "region"),
        (vec![("north".into(), 2), ("south".into(), 1)], 0, 0)
    );
}

#[test]
fn a_row_the_caller_cannot_see_or_whose_values_are_withheld_is_never_counted() {
    let (host, _dir) = host();
    let few = facets(&host, "few", Some("status"), None).unwrap();
    assert_eq!(few.rows, 4);
    assert_eq!(
        read(&few, "status").0,
        vec![("closed".into(), 2), ("open".into(), 2)]
    );
    let pop = facets(&host, "pop", Some("status"), None).unwrap();
    assert_eq!(pop.rows, 0);
    assert_eq!(pop.withheld_rows, 5);
    assert!(read(&pop, "status").0.is_empty());
}

#[test]
fn a_property_that_is_not_text_a_choice_or_yes_no_is_refused_at_its_pointer() {
    let (host, _dir) = host();
    let refusal = facets(&host, "full", Some("status,amount,missing"), None).unwrap_err();
    let document = serde_json::to_value(&refusal).unwrap();
    let wire = document.to_string();
    assert!(wire.contains("amount is not a text"), "{wire}");
    assert!(wire.contains("missing is not a text"), "{wire}");
    assert!(wire.contains("/properties"), "{wire}");
    assert!(facets(&host, "full", None, Some("{not json")).is_err());
}
