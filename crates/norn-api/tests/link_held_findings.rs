//! OBS-LINK-DISPUTE-1 (Sol, #1544 round 2, S2): a held link is disclosed as
//! the link itself would be. A `core.boundary.link_held` finding names both
//! endpoints, so a caller who may review the disputed source but may not see
//! the target (no Population on it) gets neither the finding nor a count that
//! includes it, on every read that serves findings: the object's findings,
//! the quarantined list's open-finding counts and the inbox's held-back groups.
#![cfg(not(target_arch = "wasm32"))]
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    missing_docs,
    reason = "held-link disclosure correspondence"
)]

#[path = "../../norn-cell/tests/support/mapped_cell.rs"]
mod mapped_cell;

use norn_api::authz::{AccessRequest, Authorizer, ReadCapability, ReadRequest};
use norn_api::host::DevHost;
use norn_api::inbox::{InboxQuery, NornInboxItem, get_inbox};
use norn_api::object_review::{get_object_findings, get_quarantined_objects};
use norn_api::object_routes::ObjectAccess;
use norn_api::refusal::Refusal;
use norn_api::scope::{CellScope, ScopeQuery};
use norn_cell::engine::NativeEngine;
use norn_cell::mapping_bundle::MappingInput;
use norn_cell::{LINK_HELD_RULE, ObjectRecord, SqliteCell};
use norn_ir::{DataQualityRuleRef, RuntimeObject, RuntimeValue};
use norn_kernel::authz::{
    AccessRequest as PdpRequest, ActorKind, Delegation, PrincipalInput, ResourceInput, ScopeKey,
};
use norn_kernel::{ObservationEvent, ObservationId, Observed};
use serde_json::{Value, json};
use std::collections::BTreeMap;

const HOLDING: &str = "canon.Holding";
const SOUTH: &str = "demo/holding/owner/south/gold";
const OWNER: &str = "demo/party/owner";

/// `full` reads everything; `limited` everything but the owner party's row.
struct Policy;
impl Authorizer for Policy {
    fn authorize(&self, _: &AccessRequest<'_>) -> Result<(), Refusal> {
        Ok(())
    }
    fn read(&self, request: &ReadRequest<'_>) -> Result<(), Refusal> {
        let allowed = match request.access.principal.subject_id.as_str() {
            "full" => true,
            "limited" => request.access.resource.id != OWNER,
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

fn scope() -> ScopeQuery {
    ScopeQuery {
        org_id: Some("org_demo".into()),
        environment_id: Some("dev".into()),
    }
}

/// An erpnext snapshot row as its connector delivers it, as event `event`.
fn deliver(
    cell: &mut SqliteCell<ObservationEvent, RuntimeObject, RuntimeObject>,
    row: &Value,
    event: &str,
) {
    let record = &row["record"];
    let alias = record["name"].as_str().expect("source alias").to_owned();
    let payload = RuntimeValue::from_json(record.clone()).expect("payload");
    let digest = norn_ir::canonical::HashProfile::Norn1
        .hash_json(serde_json::to_string(&payload).expect("json"))
        .expect("digest");
    let source = "synthetic-erpnext".to_owned();
    let raw = Observed {
        id: ObservationId(format!("delivery-{event}-{alias}-{digest}")),
        provenance: norn_kernel::Provenance {
            source: source.clone(),
            source_id: alias.clone(),
            mapping_version: "synthetic/1".into(),
            observed_at: norn_kernel::LogicalTime::from_millis(1_793_702_400_000),
        },
        input: norn_ir::runtime_object! {
            "contract"=>"norn.raw_observation/v1", "source"=>source, "source_id"=>alias,
            "event_id"=>event, "source_version"=>event, "pipeline_version"=>"synthetic/1", "kind"=>"upsert", "extracted_at"=>1_793_702_400_000u64, "payload"=>payload, "payload_digest"=>digest,
        },
    };
    let input = MappingInput {
        mapping: "erpnext.three.mapping".into(),
        dataset: row["dataset"].as_str().expect("dataset").into(),
        connector_instance: "demo".into(),
        partition: "snapshot".into(),
        recorded_at: norn_ir::provenance::UtcTimestamp("2026-11-03T00:00:00Z".into()),
    };
    let (id, _) = cell.inject_mapping_bundle(&raw, &input).expect("capture");
    cell.process_mapping_outcome(&id).expect("process");
}

/// The south holding lands while its owner is absent; a conflicting re-send
/// of its event disputes it; then the owner lands, so the freeze holds
/// `south -owner-> owner` and the south holding carries the held finding.
fn host() -> (DevHost<NativeEngine>, tempfile::TempDir) {
    let directory = tempfile::tempdir().expect("directory");
    let store = mapped_cell::store(&directory.path().join("cell.sqlite"), |_| {});
    let mut cell = SqliteCell::new(store, "org_demo/dev", |_| BTreeMap::new()).expect("cell");
    let rows: Vec<Value> = serde_json::from_str::<Value>(
        &std::fs::read_to_string(mapped_cell::root().join("registry/packs/erpnext/synthetic.json"))
            .expect("fixture"),
    )
    .expect("json")["snapshot"]
        .as_array()
        .expect("rows")
        .clone();
    let (owner, south) = (&rows[0], &rows[5]);
    deliver(&mut cell, south, "1");
    let mut resent = south.clone();
    resent["record"]["quantity"] = json!(201);
    deliver(&mut cell, &resent, "1");
    deliver(&mut cell, owner, "1");
    let held = cell
        .store()
        .read()
        .observation_findings(norn_cell::MAIN_WORLD, HOLDING, SOUTH)
        .expect("findings")
        .into_iter()
        .filter(is_held)
        .count();
    assert_eq!(held, 1, "the fixture holds south -owner-> owner");
    let host = DevHost::from_store(
        cell.into_store(),
        CellScope {
            org_id: "org_demo".into(),
            environment_id: "dev".into(),
        },
        |command| command.payload.clone(),
    )
    .expect("host")
    .with_authorizer(Policy);
    (host, directory)
}

fn is_held(finding: &norn_ir::DataQualityFinding) -> bool {
    matches!(&finding.rule_ref, DataQualityRuleRef::Builtin { id, .. } if id == LINK_HELD_RULE)
}

#[test]
fn obs_link_dispute_held_link_is_disclosed_only_with_both_endpoints() {
    let (host, _directory) = host();
    let mut counts = BTreeMap::new();
    for actor in ["full", "limited"] {
        let findings = get_object_findings(&host, &scope(), Some(actor), HOLDING, SOUTH, &Mapping)
            .expect("findings");
        assert!(findings.quarantined, "{actor}: the disputed holding");
        let held: Vec<_> = findings.findings.iter().filter(|f| is_held(f)).collect();
        let others = findings.findings.len() - held.len();
        assert!(others > 0, "{actor}: the dispute's own finding is served");
        let quarantined = get_quarantined_objects(&host, &scope(), Some(actor), HOLDING, &Mapping)
            .expect("quarantined");
        let open = quarantined
            .items
            .iter()
            .find(|item| item.id == SOUTH)
            .expect("south is listed")
            .open_findings;
        let inbox = get_inbox(
            &host,
            &InboxQuery {
                org_id: Some("org_demo".into()),
                environment_id: Some("dev".into()),
            },
            Some(actor),
            &Mapping,
            (false, norn_kernel::LogicalTime::from_millis(1_793_702_400_000)),
        )
        .expect("inbox");
        let groups = inbox
            .items
            .iter()
            .filter(|item| {
                matches!(item, NornInboxItem::HeldBack(group) if matches!(&group.rule_ref, DataQualityRuleRef::Builtin { id, .. } if id == LINK_HELD_RULE))
            })
            .count();
        counts.insert(actor, (held.len(), others, open, groups));
    }
    let (full, limited) = (counts["full"], counts["limited"]);
    assert_eq!(full.0, 1, "full sees the held link: {counts:?}");
    assert_eq!(limited.0, 0, "limited never sees the owner: {counts:?}");
    assert_eq!(full.1, limited.1, "every other finding is served alike");
    assert_eq!(full.2, limited.2 + 1, "the count includes it only for full");
    assert_eq!((full.3, limited.3), (1, 0), "the inbox groups it only for full");
}
