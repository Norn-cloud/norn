//! RUN-ROUTES-1: start, read and list runs of an installed flow through the
//! shared host services, under explicit scope, the host authorizer and the
//! ordinary read gate.
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
use norn_api::host::DevHost;
use norn_api::refusal::Refusal;
use norn_api::run_routes::{RunsQuery, StartRunRequest, get_run, list_runs, start_run};
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
    DevHost::from_store(store, scope(), |command| command.payload.clone())
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
fn a_started_run_commits_its_first_turn_with_its_ledger_row() {
    let host = host();
    let (status, run) = start(&host, "run-1").unwrap();
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(run.id, "run-1");
    assert_eq!(run.flow.id, FLOW);
    assert_eq!(run.flow.version, "1.0.0");
    assert_eq!(run.object_type, RUN_TYPE);
    assert_eq!(run.generation, 1);
    assert_eq!(
        run.state.as_deref(),
        Some("awaiting_perform_load"),
        "the start turn creates the run and waits on its first perform"
    );
    // An exact retry returns the same run and commits nothing new.
    let (status, again) = start(&host, "run-1").unwrap();
    assert_eq!(status, StatusCode::OK);
    assert_eq!(again, run);
    assert_eq!(
        get_run(&host, &query(), "run-1", &access("user:ada")).unwrap(),
        run
    );
}

#[test]
fn starts_refuse_with_typed_codes_and_commit_nothing() {
    let host = host();
    let refuse = |actor: Option<&str>, scope: ScopeQuery, flow: &str, run: &str| {
        start_run(
            &host,
            &scope,
            actor,
            &StartRunRequest {
                flow: flow.into(),
                run_id: run.into(),
            },
            &access("user:ada"),
        )
        .unwrap_err()
    };
    assert_eq!(
        refuse(Some("user:bob"), query(), FLOW, "r").code,
        "authz.denied"
    );
    assert_eq!(refuse(None, query(), FLOW, "r").code, "authz.denied");
    assert_eq!(
        refuse(Some("user:ada"), ScopeQuery::default(), FLOW, "r").code,
        "scope.missing"
    );
    assert_eq!(
        refuse(Some("user:ada"), query(), "billing.nope", "r").code,
        "run.flow_not_found"
    );
    assert_eq!(
        refuse(Some("user:ada"), query(), FLOW, "bad id/").code,
        "request.invalid"
    );
    assert_eq!(
        refuse(Some("user:ada"), query(), FLOW, "").code,
        "request.invalid"
    );
    start(&host, "taken").unwrap();
    assert_eq!(
        refuse(Some("user:ada"), query(), "billing.other", "taken").code,
        "run.exists"
    );
    let page = list_runs(
        &host,
        &RunsQuery {
            org_id: Some("org_demo".into()),
            environment_id: Some("dev".into()),
            limit: Some(10),
            ..RunsQuery::default()
        },
        &access("user:ada"),
    )
    .unwrap();
    assert_eq!(page.runs.len(), 1, "refused starts left no run");
}

#[test]
fn reads_pass_the_values_gate_and_never_reveal_invisible_runs() {
    let host = host();
    start(&host, "run-1").unwrap();
    for (id, subject) in [("run-1", "user:bob"), ("missing", "user:ada")] {
        assert_eq!(
            get_run(&host, &query(), id, &access(subject))
                .unwrap_err()
                .code,
            "run.not_found",
            "{id} as {subject}"
        );
    }
    let hidden = list_runs(
        &host,
        &RunsQuery {
            org_id: Some("org_demo".into()),
            environment_id: Some("dev".into()),
            limit: Some(10),
            ..RunsQuery::default()
        },
        &access("user:bob"),
    )
    .unwrap();
    assert!(hidden.runs.is_empty());
    // A full page of invisible runs never names one in the cursor.
    for run in ["run-2", "run-3"] {
        start(&host, run).unwrap();
    }
    let page = list_runs(
        &host,
        &RunsQuery {
            org_id: Some("org_demo".into()),
            environment_id: Some("dev".into()),
            limit: Some(1),
            ..RunsQuery::default()
        },
        &access("user:bob"),
    )
    .unwrap();
    assert!(page.runs.is_empty());
    assert_eq!(
        page.next_cursor, None,
        "the cursor reveals no invisible run"
    );
    // A reader's cursor names the last run it was shown.
    let page = list_runs(
        &host,
        &RunsQuery {
            org_id: Some("org_demo".into()),
            environment_id: Some("dev".into()),
            limit: Some(2),
            ..RunsQuery::default()
        },
        &access("user:ada"),
    )
    .unwrap();
    assert_eq!(page.next_cursor.as_deref(), Some("run-2"));
}

#[test]
fn runs_page_by_cursor_and_filter_by_flow_and_state() {
    let host = host();
    for run in ["a", "b", "c"] {
        start(&host, run).unwrap();
    }
    let page = |cursor: Option<String>, state: Option<&str>, flow: Option<&str>| {
        list_runs(
            &host,
            &RunsQuery {
                org_id: Some("org_demo".into()),
                environment_id: Some("dev".into()),
                flow: flow.map(str::to_owned),
                state: state.map(str::to_owned),
                cursor,
                limit: Some(2),
            },
            &access("user:ada"),
        )
        .unwrap()
    };
    let first = page(None, None, Some(FLOW));
    assert_eq!(
        first
            .runs
            .iter()
            .map(|run| run.id.as_str())
            .collect::<Vec<_>>(),
        ["a", "b"]
    );
    let second = page(first.next_cursor, None, Some(FLOW));
    assert_eq!(
        second
            .runs
            .iter()
            .map(|run| run.id.as_str())
            .collect::<Vec<_>>(),
        ["c"]
    );
    assert_eq!(second.next_cursor, None);
    assert!(page(None, Some("completed"), None).runs.is_empty());
    assert!(page(None, None, Some("billing.other")).runs.is_empty());
    assert_eq!(
        list_runs(
            &host,
            &RunsQuery {
                org_id: Some("org_demo".into()),
                environment_id: Some("dev".into()),
                limit: Some(0),
                ..RunsQuery::default()
            },
            &access("user:ada"),
        )
        .unwrap_err()
        .code,
        "request.invalid"
    );
}
