//! OBS-LINK-DISPUTE-1 (Sol, #1544 round 2, S2): a held link is disclosed as
//! the link itself would be. A `core.boundary.link_held` finding names both
//! endpoints, so a caller who may review the disputed source but may not see
//! the target (no Population on it) gets neither the finding nor a count that
//! includes it, on every read that serves findings: the object's findings,
//! the quarantined list's open-finding counts and the inbox's held-back groups.
//! Each of them confirms its snapshot after that policy (round 3, S2).
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
    AccessRequest as PdpRequest, ActorKind, Binding, ResourceInput, ScopeKey, UnboundProfile,
    VerifiedPrincipal,
};
use norn_kernel::{ObservationEvent, ObservationId, Observed};
use serde_json::{Value, json};
use std::collections::BTreeMap;

const HOLDING: &str = "canon.Holding";
const SOUTH: &str = "demo/holding/owner/south/gold";
const OWNER: &str = "demo/party/owner";

/// `full` reads everything; `limited` everything but the owner party's row;
/// `reviewer` everything but Sources.
struct Policy;
impl Authorizer for Policy {
    fn authorize(&self, _: &AccessRequest<'_>) -> Result<(), Refusal> {
        Ok(())
    }
    fn read(&self, request: &ReadRequest<'_>) -> Result<(), Refusal> {
        let allowed = match request.access.principal().subject_id.as_str() {
            "full" => true,
            "limited" => request.access.resource().id != OWNER,
            "reviewer" => request.capability != ReadCapability::Sources,
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
        Ok(PdpRequest::new(
            &VerifiedPrincipal::new(actor, ActorKind::User, None, vec![], BTreeMap::new()),
            Some(key.clone()),
            format!("fixture.{capability:?}"),
            "norn.read",
            ResourceInput {
                object_type: row
                    .map_or(object_type, |row| row.object_type.as_str())
                    .into(),
                id: row.map_or(object_type, |row| row.id.as_str()).into(),
                scope: key,
                attributes: BTreeMap::new(),
            },
            BTreeMap::new(),
            Binding::Unbound(UnboundProfile::HostedRead),
        ))
    }
}

fn scope() -> ScopeQuery {
    ScopeQuery {
        org_id: Some("org_demo".into()),
        environment_id: Some("dev".into()),
    }
}

/// An erpnext snapshot row as its connector delivers it, as event `event`.
fn raw(row: &Value, event: &str) -> (Observed<RuntimeValue>, MappingInput) {
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
    (raw, input)
}

/// Deliver `row` as event `event`, one by one.
fn deliver(
    cell: &mut SqliteCell<ObservationEvent, RuntimeObject, RuntimeObject>,
    row: &Value,
    event: &str,
) {
    let (raw, input) = raw(row, event);
    let (id, _) = cell.inject_mapping_bundle(&raw, &input).expect("capture");
    cell.process_mapping_outcome(&id).expect("process");
}

/// The erpnext fixture's snapshot rows.
fn rows() -> Vec<Value> {
    serde_json::from_str::<Value>(
        &std::fs::read_to_string(mapped_cell::root().join("registry/packs/erpnext/synthetic.json"))
            .expect("fixture"),
    )
    .expect("json")["snapshot"]
        .as_array()
        .expect("rows")
        .clone()
}

/// The south holding lands while its owner is absent; a conflicting re-send
/// of its event disputes it; then the owner lands, so the freeze holds
/// `south -owner-> owner` and the south holding carries the held finding.
fn host() -> (DevHost<NativeEngine>, tempfile::TempDir) {
    let directory = tempfile::tempdir().expect("directory");
    let store = mapped_cell::store(&directory.path().join("cell.sqlite"), |_| {});
    let mut cell = SqliteCell::new(store, "org_demo/dev", |_| BTreeMap::new()).expect("cell");
    let rows = rows();
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
            (
                false,
                norn_kernel::LogicalTime::from_millis(1_793_702_400_000),
            ),
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
    assert_eq!(
        (full.3, limited.3),
        (1, 0),
        "the inbox groups it only for full"
    );
}

/// [`Mapping`], which commits a change to the owner party, through the host,
/// the first time the owner's row policy runs: a commit racing the endpoint
/// policy, which runs outside the cell lock.
struct Racing<'h> {
    host: &'h DevHost<NativeEngine>,
    change: std::sync::Mutex<Option<(Observed<RuntimeValue>, MappingInput)>>,
}
impl ObjectAccess for Racing<'_> {
    fn access(
        &self,
        scope: &CellScope,
        actor: &str,
        capability: ReadCapability,
        object_type: &str,
        row: Option<&ObjectRecord>,
    ) -> Result<PdpRequest, Refusal> {
        if capability == ReadCapability::Population && row.is_some_and(|row| row.id == OWNER) {
            let change = self.change.lock().ok().and_then(|mut change| change.take());
            if let Some(change) = change {
                let landed = self.host.ingest(&[change])?;
                if landed.first().map(|row| &row.outcome)
                    != Some(&norn_cell::MappingOutcome::Landed)
                {
                    return Err(Refusal::invalid("the racing commit did not land"));
                }
            }
        }
        Mapping.access(scope, actor, capability, object_type, row)
    }
}

/// OBS-LINK-DISPUTE-1 (Sol, #1544 round 3, S2): the held link's endpoint
/// policy runs outside the cell lock, so a commit can land while it runs;
/// every read that serves findings then refuses (`object.snapshot_moved`)
/// rather than disclose by the row it read before, as the link route does.
/// The object's findings and the quarantined list take no later lock when no
/// evidence row links to the object; the inbox reviewer lacks Sources, so no
/// inbox record reads evidence under the lock after the policy either: on
/// every surface the check is the disclosure's own.
#[test]
fn obs_link_dispute_held_link_disclosure_is_confirmed_at_its_snapshot() {
    let (host, _directory) = host();
    let mut owner = rows()[0].clone();
    let mut racing = |event: &str| {
        owner["record"]["label"] = json!(format!("Owner, event {event}"));
        Racing {
            host: &host,
            change: std::sync::Mutex::new(Some(raw(&owner, event))),
        }
    };
    // Each read's refusal code, or `served` when it answered.
    let mut outcomes = Vec::new();
    let mut record = |read: &str, answer: Result<(), Refusal>, access: &Racing<'_>| {
        assert!(
            access.change.lock().expect("change").is_none(),
            "{read}: the race ran"
        );
        outcomes.push((
            read.to_owned(),
            answer.map_or_else(
                |refused| format!("{} {} {}", refused.status, refused.code, refused.retryable),
                |()| "served".to_owned(),
            ),
        ));
    };
    let access = racing("2");
    let answer = get_object_findings(&host, &scope(), Some("full"), HOLDING, SOUTH, &access);
    record("findings", answer.map(|_| ()), &access);
    let access = racing("3");
    let answer = get_quarantined_objects(&host, &scope(), Some("full"), HOLDING, &access);
    record("quarantined", answer.map(|_| ()), &access);
    let access = racing("4");
    let answer = get_inbox(
        &host,
        &InboxQuery {
            org_id: Some("org_demo".into()),
            environment_id: Some("dev".into()),
        },
        Some("reviewer"),
        &access,
        (
            false,
            norn_kernel::LogicalTime::from_millis(1_793_702_400_000),
        ),
    );
    record("inbox", answer.map(|_| ()), &access);
    let moved = "409 object.snapshot_moved true".to_owned();
    assert_eq!(
        outcomes,
        ["findings", "quarantined", "inbox"].map(|read| (read.to_owned(), moved.clone())),
        "every read that serves the held link confirms its snapshot"
    );
    // Without a race, the held link is served.
    let findings = get_object_findings(&host, &scope(), Some("full"), HOLDING, SOUTH, &Mapping)
        .expect("findings");
    assert_eq!(findings.findings.iter().filter(|f| is_held(f)).count(), 1);
}

/// OBS-LINK-DISPUTE-1 (round 4): the host delivers a page the cell refuses
/// as spanning a dispute one record per page, each captured and committed
/// before the next, as one by one: north lands before the conflicting
/// re-send of south is captured.
#[test]
fn obs_link_dispute_a_host_delivers_a_page_spanning_a_dispute_record_by_record() {
    let directory = tempfile::tempdir().expect("directory");
    let store = mapped_cell::store(&directory.path().join("cell.sqlite"), |_| {});
    let mut cell = SqliteCell::new(store, "org_demo/dev", |_| BTreeMap::new()).expect("cell");
    let rows = rows();
    let (north, south) = (&rows[4], &rows[5]);
    deliver(&mut cell, south, "1");
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
    let mut resent = south.clone();
    resent["record"]["quantity"] = json!(201);
    let report = host
        .ingest_within(&[raw(north, "1"), raw(&resent, "1")], None)
        .expect("ingest");
    assert_eq!(report.pages, 2, "one page per record");
    assert_eq!(report.outcomes.len(), 2);
    assert_eq!(
        report.outcomes[0].outcome,
        norn_cell::MappingOutcome::Landed,
        "north"
    );
    assert!(
        report.outcomes[1]
            .observation_id
            .starts_with("mapping-conflict:"),
        "{:?}",
        report.outcomes[1]
    );
}
