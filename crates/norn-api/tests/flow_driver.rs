//! FLOW-DRIVER-1: a started run advances through its perform results, one
//! committed result turn per result, to its terminal state.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "integration assertions"
)]

use std::collections::BTreeMap;

use http::StatusCode;
use norn_api::authz::{AccessRequest, Authorizer, Operation, ReadCapability, ReadRequest};
use norn_api::flow_driver::{deliver_result, drive_performs};
use norn_api::host::DevHost;
use norn_api::refusal::Refusal;
use norn_api::run_routes::{StartRunRequest, get_run, start_run};
use norn_api::scope::{CellScope, ScopeQuery};
use norn_cell::{CellStore, InstalledDefinitionInput, Model};
use norn_ir::{EntityRoot, RuntimeValue};
use norn_kernel::authz::{ActorKind, Delegation, PrincipalInput, ResourceInput, ScopeKey};

const FLOW: &str = "billing.route";
const RUN_TYPE: &str = "billing.RouteRun";

fn artifacts() -> Vec<serde_json::Value> {
    serde_json::from_str(include_str!(
        "../../../tools/norn-ts/fixtures/flows/route.ir.json"
    ))
    .unwrap()
}

fn scope() -> CellScope {
    CellScope {
        org_id: "org_demo".into(),
        environment_id: "dev".into(),
    }
}

fn query() -> ScopeQuery {
    ScopeQuery {
        org_id: Some("org_demo".into()),
        environment_id: Some("dev".into()),
    }
}

/// Starts runs for `user:ada`; reads run values for `user:ada` only.
struct Policy;

impl Authorizer for Policy {
    fn authorize(&self, request: &AccessRequest<'_>) -> Result<(), Refusal> {
        match (&request.operation, request.actor) {
            (Operation::StartRun(_), Some("user:ada")) => Ok(()),
            _ => Err(Refusal::new(
                StatusCode::FORBIDDEN,
                "authz.denied",
                "denied",
                "denied",
            )),
        }
    }

    fn read(&self, request: &ReadRequest<'_>) -> Result<(), Refusal> {
        if request.capability == ReadCapability::Values
            && request.access.principal.subject_id == "user:ada"
            && request.access.resource.object_type == RUN_TYPE
        {
            Ok(())
        } else {
            Err(Refusal::new(
                StatusCode::FORBIDDEN,
                "authz.denied",
                "denied",
                "denied",
            ))
        }
    }
}

fn access(
    subject: &'static str,
) -> impl Fn(ReadCapability, &str) -> norn_kernel::authz::AccessRequest {
    move |capability, object_type| {
        let key = ScopeKey {
            org: "org_demo".into(),
            environment: Some("dev".into()),
            tenant: None,
            workspace: None,
            business_unit: None,
        };
        norn_kernel::authz::AccessRequest {
            principal: PrincipalInput {
                subject_id: subject.into(),
                actor_kind: ActorKind::User,
                agent_session_id: None,
                roles: vec![],
                attributes: BTreeMap::new(),
            },
            scope: Some(key.clone()),
            capability: match capability {
                ReadCapability::Population => "population",
                ReadCapability::Values => "values",
                ReadCapability::Sources => "sources",
            }
            .into(),
            action_id: "norn.read".into(),
            resource: ResourceInput {
                object_type: object_type.into(),
                id: object_type.into(),
                scope: key,
                attributes: BTreeMap::new(),
            },
            context: BTreeMap::new(),
            binding: None,
            delegation: Delegation::Direct,
        }
    }
}

/// The outside world of the route fixture: an open, overdue invoice, and a
/// delivered reminder.
fn billing(command: &norn_kernel::Command<norn_ir::RuntimeObject>) -> norn_ir::RuntimeObject {
    let text = |value: &str| RuntimeValue::String(value.to_owned());
    match command
        .payload
        .get("function")
        .and_then(RuntimeValue::as_str)
    {
        Some("billing.load_invoice") => BTreeMap::from([
            ("id".to_owned(), text("inv-1")),
            ("status".to_owned(), text("open")),
            ("overdue".to_owned(), RuntimeValue::Bool(true)),
            (
                "amountMinor".to_owned(),
                RuntimeValue::from_json(serde_json::json!(4200)).unwrap(),
            ),
        ]),
        _ => BTreeMap::from([("delivered".to_owned(), RuntimeValue::Bool(true))]),
    }
}

fn host() -> DevHost<norn_cell::engine::NativeEngine> {
    let artifacts = artifacts();
    let objects = artifacts
        .iter()
        .filter(|artifact| artifact["kind"] == "entity")
        .map(|artifact| {
            let root = serde_json::from_value::<EntityRoot>(artifact["payload"].clone()).unwrap();
            let EntityRoot::V2(root) = root else {
                panic!("the flow's Run type is an entity root")
            };
            root.object_type
        })
        .collect::<Vec<_>>();
    let model = Model::from_definitions(&objects, &[]).unwrap();
    let set = RuntimeValue::from_json(serde_json::Value::Array(artifacts)).unwrap();
    let input = InstalledDefinitionInput {
        canonical_set: String::from_utf8(set.canonical_json().unwrap()).unwrap(),
        registry: vec![],
        // The route trusts the installed set; admission is the install route's.
        receipt_sha256: format!("sha256:{}", "0".repeat(64)),
        admission_receipt: None,
        readmission: None,
        diff: serde_json::json!({}),
        installed_at: 1,
        layout: None,
    };
    let engine = norn_cell::engine::NativeEngine::open_in_memory().unwrap();
    let store = CellStore::provision_definitions(engine, model, &input, &scope().key()).unwrap();
    DevHost::from_store(store, scope(), billing)
        .unwrap()
        .with_authorizer(Policy)
}

fn start(
    host: &DevHost<norn_cell::engine::NativeEngine>,
    run: &str,
) -> Result<(StatusCode, norn_api::run_routes::RunDocument), Refusal> {
    start_run(
        host,
        &query(),
        Some("user:ada"),
        &StartRunRequest {
            flow: FLOW.into(),
            run_id: run.into(),
        },
        &access("user:ada"),
    )
}

#[test]
fn a_run_advances_once_per_result_to_its_terminal_state() {
    let host = host();
    let (_, run) = start(&host, "run-1").unwrap();
    assert_eq!(run.state.as_deref(), Some("awaiting_perform_load"));
    let advanced = drive_performs(&host).unwrap();
    let states: Vec<_> = advanced
        .iter()
        .map(|run| run.state.clone().unwrap())
        .collect();
    // load: the invoice is open and overdue, so the flow reminds, then completes.
    assert_eq!(states, ["awaiting_perform_remind", "completed"]);
    let done = get_run(&host, &query(), "run-1", &access("user:ada")).unwrap();
    assert_eq!(done.state.as_deref(), Some("completed"));
    assert!(done.version > run.version);
    // Nothing is left to perform, and no result can be delivered twice.
    assert!(drive_performs(&host).unwrap().is_empty());
    let refused = deliver_result(
        &host,
        &norn_kernel::CommandId("no-such-command".into()),
        norn_kernel::Generation(1),
    )
    .unwrap_err();
    assert_eq!(refused.code, "run.result_not_pending");
}

#[test]
fn runs_advance_independently() {
    let host = host();
    for run in ["a", "b"] {
        start(&host, run).unwrap();
    }
    let advanced = drive_performs(&host).unwrap();
    assert_eq!(advanced.len(), 4, "two results per run");
    for run in ["a", "b"] {
        assert_eq!(
            get_run(&host, &query(), run, &access("user:ada"))
                .unwrap()
                .state
                .as_deref(),
            Some("completed")
        );
    }
}
