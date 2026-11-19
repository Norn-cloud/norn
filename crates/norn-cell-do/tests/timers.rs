//! TIMER-ALARM-1: the hosted cell knows the time. Its deadlines run from
//! the host's clock (the Durable Object alarm, the dev host's loop) through
//! `norn_api::timers`, with no request arriving: a reservation is released
//! at its expiry, a decision past its deadline expires and moves its run, and
//! the run's retry timer fires. Every wake is at-least-once safe: delivered
//! again (a repeated alarm, a restart in between), it finds nothing due and
//! commits nothing.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "route correspondence"
)]

#[path = "../../norn-cell/tests/quantity_support/mod.rs"]
#[allow(
    dead_code,
    unreachable_pub,
    reason = "the shared quantity fixtures serve other tests too"
)]
mod quantity_support;

use std::path::Path;

use norn_api::definition_routes::DefinitionKinds;
use norn_api::host::DevHost;
use norn_api::scope::CellScope;
use norn_api::timers::{DueWork, next_due, run_due};
use norn_cell::engine::NativeEngine;
use norn_cell::{CellStore, Model, SqliteCell, StoreOptions};
use norn_cell_do::analytics::{self, DemoQuestions};
use norn_cell_do::policy::{DEMO_GRANTS, ReadPolicy};
use norn_cell_do::principal::FixturePrincipals;
use norn_cell_do::server::{self, Response};
use norn_kernel::LogicalTime;
use norn_kernel::quantity::QuantityKey;
use serde_json::{Value, json};

fn at(millis: u64) -> LogicalTime {
    LogicalTime::from_millis(millis)
}

// --- A reservation with a TTL ---

fn marker() -> Model {
    let def = serde_json::from_value(
        json!({"name":"fixture.Marker","properties":[{"id":"label","type":{"kind":"text"}}]}),
    )
    .unwrap();
    Model::from_definitions(&[def], &[]).unwrap()
}

fn dev_scope() -> CellScope {
    CellScope {
        org_id: "org_demo".into(),
        environment_id: "dev".into(),
    }
}

fn store(path: &Path) -> CellStore<NativeEngine> {
    CellStore::open(path, marker(), StoreOptions::default()).unwrap()
}

/// The cell at `path` as a host opens it.
fn host(path: &Path) -> DevHost<NativeEngine> {
    DevHost::from_store(store(path), dev_scope(), |_| {
        std::collections::BTreeMap::new()
    })
    .unwrap()
}

/// The amount held on the reserved lot, read from the stored cell.
fn held(path: &Path) -> String {
    store(path)
        .read()
        .quantity_balance(
            "main",
            "stock",
            &QuantityKey {
                account: "lot".into(),
                unit: "item".into(),
                layer: "physical".into(),
            },
        )
        .unwrap()
        .held
}

/// A cell at `path` holding 100 items on `lot`, 70 of them reserved until
/// `expires`, committed at logical time 5.
fn reserve(path: &Path, expires: u64) {
    let mut cell: SqliteCell<String, String, String, NativeEngine> =
        SqliteCell::new(store(path), dev_scope().key(), |_| String::new()).unwrap();
    cell.advance_to(at(5));
    for (key, request) in [
        ("define", quantity_support::define()),
        ("fund", quantity_support::post(1, "fund", "fund", 100)),
        ("reserve", quantity_support::reserve(2, "hold", 70, expires)),
    ] {
        let mut request = request;
        request.balance_versions = cell
            .store()
            .read()
            .quantity_balance_versions("main", &request)
            .unwrap();
        cell.commit_quantity(&request, key).unwrap();
    }
}

/// A reservation with a TTL is released at its expiry by the host's clock
/// alone; a wake before it does nothing, and a wake delivered again, before
/// or after a restart, finds nothing due.
#[test]
fn a_reservation_is_released_at_its_expiry_without_a_request() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cell.sqlite");
    reserve(&path, 1_000);
    assert_eq!(held(&path), "70");

    let cell = host(&path);
    assert_eq!(next_due(&cell).unwrap(), Some(1_000), "the alarm's time");
    let early = run_due(&cell, at(999)).unwrap();
    assert_eq!(early.expired_reservations, 0);
    assert_eq!(early.next, Some(1_000));
    assert!(!early.committed());

    let fired = run_due(&cell, at(1_000)).unwrap();
    assert_eq!(fired.expired_reservations, 1, "{fired:?}");
    assert_eq!(fired.next, None, "nothing else is pending");
    assert!(fired.committed());
    // The same alarm delivered again does nothing.
    assert_eq!(run_due(&cell, at(1_000)).unwrap(), DueWork::default());
    drop(cell);
    assert_eq!(held(&path), "0", "the hold is released");

    // After a restart, the re-delivered alarm still finds nothing due.
    let reopened = host(&path);
    assert_eq!(next_due(&reopened).unwrap(), None);
    assert_eq!(run_due(&reopened, at(1_001)).unwrap(), DueWork::default());
    drop(reopened);
    assert_eq!(held(&path), "0");
}

/// A deadline that passed while the host was down is run at the first wake
/// after the restart, exactly once.
#[test]
fn a_deadline_missed_while_down_runs_once_after_a_restart() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cell.sqlite");
    reserve(&path, 2_000);
    let reopened = host(&path);
    assert_eq!(next_due(&reopened).unwrap(), Some(2_000));
    assert_eq!(
        run_due(&reopened, at(50_000)).unwrap().expired_reservations,
        1
    );
    assert_eq!(run_due(&reopened, at(50_000)).unwrap(), DueWork::default());
    drop(reopened);
    assert_eq!(held(&path), "0");
}

// --- A decision deadline and a flow's sleep timer ---

const SCOPE: &str = "orgId=org_demo&environmentId=analytics";
const ADMIN: Option<&str> = Some("Bearer fixture:admin@runs");
const MEMBER: Option<&str> = Some("Bearer fixture:member@runs");
const FIXTURES: &[(&str, &[&str])] = &[("admin@runs", &["admin"]), ("member@runs", &["member"])];
const ROLES: [&str; 4] = ["viewer", "member", "admin", "owner"];
/// `lowerFlow`'s output for `ask.builder.ts`: a person decides, within a day.
const ASK: &str = include_str!("../../../tools/norn-ts/fixtures/flows/ask.ir.json");
/// `lowerFlow`'s output for `retry-timer.builder.ts`: a 30 s sleep, then a
/// retried perform.
const RETRY_TIMER: &str = include_str!("../../../tools/norn-ts/fixtures/flows/retry-timer.ir.json");
/// The perform the retry-timer flow calls, as a tool this cell resolves.
const TOOL: &str = "norn.classify.whatsapp-intent";
/// The request clock, Unix seconds.
const NOW_S: u64 = 1_800_000_000;
/// The sleep the retry-timer flow declares before its perform.
const SLEEP_MS: u64 = 30_000;

struct Cell {
    host: DevHost<NativeEngine>,
    questions: DemoQuestions,
    principals: FixturePrincipals,
    kinds: DefinitionKinds,
}

/// The registered built-ins, and the test's tool as if a built-in.
fn resolve(function: &str, version: u32) -> bool {
    norn_cell::builtin_registered(function, version) || (function == TOOL && version == 1)
}

fn cell() -> Cell {
    let scope = analytics::analytics_scope();
    let opened = analytics::provision(NativeEngine::open_in_memory().unwrap(), &scope).unwrap();
    let policy =
        ReadPolicy::compile(opened.store.model().object_types(), &scope, &DEMO_GRANTS).unwrap();
    let principals = FixturePrincipals::new(FIXTURES, policy.caller());
    let host = DevHost::from_store(opened.store, scope.clone(), |command| {
        command.payload.clone()
    })
    .unwrap()
    .with_authorizer(policy);
    let questions = DemoQuestions::new(opened.plan, scope)
        .with_handle_key(&[5; 32])
        .with_metrics(opened.definitions, opened.catalog);
    Cell {
        host,
        questions,
        principals,
        kinds: DefinitionKinds::default()
            .with_roles(ROLES)
            .with_restater(norn_api::definition_routes::checker_restatement)
            .with_builtins(resolve),
    }
}

fn call(cell: &Cell, method: &str, path: &str, auth: Option<&str>, body: &str) -> (u16, Value) {
    let request = server::Request {
        method,
        path,
        query: SCOPE,
        authorization: auth,
        now: NOW_S,
        body,
    };
    let Response { status, body } = server::handle_all(
        &cell.host,
        &cell.principals,
        Some(&cell.questions),
        &cell.kinds,
        &request,
    );
    (status, serde_json::from_str(&body).unwrap())
}

/// Install the lowered flow `flow` as this cell's admin, and start `run-1`.
fn started(cell: &Cell, lowered: &str, flow: &str) {
    let artifacts: Vec<Value> = serde_json::from_str::<Vec<Value>>(lowered)
        .unwrap()
        .into_iter()
        .map(|mut artifact| {
            artifact["scope"] = json!({ "orgId": "org_demo", "environmentId": "analytics" });
            artifact
        })
        .collect();
    let mut body = json!({ "kind": "flow", "id": flow, "payload": {
        "flow": { "id": flow, "version": "1.0.0", "title": "A timed flow" },
        "artifacts": artifacts } });
    let (_, admitted) = call(
        cell,
        "POST",
        "/v1/definitions:admit",
        ADMIN,
        &body.to_string(),
    );
    body["signoff"] = json!({ "restatementSha256": admitted["restatementSha256"] });
    let (status, installed) = call(
        cell,
        "POST",
        "/v1/definitions:install",
        ADMIN,
        &body.to_string(),
    );
    assert_eq!(status, 200, "{installed}");
    let (status, run) = call(
        cell,
        "POST",
        "/v1/runs",
        MEMBER,
        &json!({ "flow": flow, "runId": "run-1" }).to_string(),
    );
    assert_eq!(status, 201, "{run}");
}

fn state(cell: &Cell) -> Value {
    let (status, run) = call(cell, "GET", "/v1/runs/run-1", ADMIN, "");
    assert_eq!(status, 200, "{run}");
    run["state"].clone()
}

/// A decision nobody answers expires at its deadline with no request: the
/// run receives the step's `Expired` failure and ends `failed_expired`. The
/// alarm delivered again does nothing.
#[test]
fn a_decision_deadline_fires_from_the_clock() {
    let cell = cell();
    started(&cell, ASK, "ops.approve_refund");
    let deadline = next_due(&cell.host)
        .unwrap()
        .expect("the decision's deadline");
    assert_eq!(deadline, NOW_S * 1000 + 86_400_000);
    let early = run_due(&cell.host, at(deadline - 1)).unwrap();
    assert!(!early.committed(), "{early:?}");
    assert_eq!(early.next, Some(deadline));

    let expired = run_due(&cell.host, at(deadline)).unwrap();
    assert_eq!(expired.decisions, 1, "{expired:?}");
    assert_eq!(expired.next, None, "{expired:?}");
    assert_eq!(state(&cell), json!("failed_expired"));
    assert_eq!(
        run_due(&cell.host, at(deadline)).unwrap(),
        DueWork::default()
    );
    assert_eq!(next_due(&cell.host).unwrap(), None);
}

/// A flow's sleep timer fires at its due time with no request, and the run
/// moves on to its perform; delivered again, it does nothing.
#[test]
fn a_flow_sleep_timer_fires_from_the_clock() {
    let cell = cell();
    let lowered = RETRY_TIMER
        .replace("billing.charge_card", TOOL)
        .replace("functionVersion: '1.0.0'", "functionVersion: '1'");
    started(&cell, &lowered, "billing.retry_timer");
    assert_eq!(state(&cell), json!("sleeping_settle"));
    let due = next_due(&cell.host).unwrap().expect("the sleep's timer");
    assert_eq!(due, NOW_S * 1000 + SLEEP_MS);
    assert!(!run_due(&cell.host, at(due - 1)).unwrap().committed());
    assert_eq!(state(&cell), json!("sleeping_settle"));

    let fired = run_due(&cell.host, at(due)).unwrap();
    assert_eq!(fired.timers, 1, "{fired:?}");
    assert_eq!(state(&cell), json!("awaiting_retry_charge_attempt_1"));
    let again = run_due(&cell.host, at(due)).unwrap();
    assert_eq!((again.timers, again.committed()), (0, false), "{again:?}");
    assert_eq!(state(&cell), json!("awaiting_retry_charge_attempt_1"));
}
