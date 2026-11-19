//! The host conformance suite's native reference (D122, #1130, #786).
//!
//! This test owns the request script that `apps/norn-cell-do/conformance`
//! replays on every Workers host (celld, `OpenOrange` on a Rivet Engine,
//! Wrangler): a sequence of governed definition installs, refusals,
//! retirements and reads on the analytics cell, all at one fixed clock. It
//! serves the script through the same tenant dispatch the Durable Object
//! binding (`src/wasm.rs`) runs, over `NativeEngine`, and records each
//! response's status and body and the verified ledger head after each
//! step. A hosted run passes only when every one of those is byte-identical.
//!
//! The reference itself must be reproducible: two fresh runs, and a run whose
//! cell is reopened from its file between every step, give the same record.
//!
//! With `NORN_CELL_DO_CONFORMANCE_OUT` set, the record (script included) is
//! written there for the hosted runner.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "conformance reference"
)]

use std::path::Path;

use norn_api::scope::CellScope;
use norn_cell::engine::{NativeEngine, SqlEngine};
use norn_cell::{LedgerHead, StoreOptions};
use norn_cell_do::analytics::{self, DemoQuestions};
use norn_cell_do::policy::DEMO_GRANTS;
use norn_cell_do::principal::{DEMO_FIXTURES, FIXTURE_ORG, FixturePrincipals};
use norn_cell_do::server::Request;
use norn_cell_do::tenant::Tenant;
use serde_json::{Value, json};

/// The stage lineage key every runtime is configured with for this script
/// (`NORN_LINEAGE_KEY`): a public test value, at least 32 bytes.
const LINEAGE_KEY: &str = "norn-conformance-lineage-key-v1-0123456789abcdef";
/// The request clock, Unix milliseconds: the hosted runner sends it in
/// `x-norn-conformance-now`, so every receipt records the same time.
const NOW_MS: u64 = 1_800_000_000_000;
const ORG: &str = "org_demo";
const ENVIRONMENT: &str = "analytics";
/// The fixture principal holding `admin`.
const ADMIN: &str = "Bearer fixture:admin@demo";
/// The fixture principal holding `viewer`.
const GUEST: &str = "Bearer fixture:guest@demo";

/// One scripted request.
#[derive(Clone)]
struct Step {
    method: &'static str,
    path: String,
    auth: &'static str,
    body: String,
}

fn query() -> String {
    format!("orgId={ORG}&environmentId={ENVIRONMENT}")
}

/// An admitted ad-hoc question over the analytics cell (F2b's fixture shape).
fn question() -> Value {
    let mut question: Value = serde_json::from_str(include_str!(
        "../../../fixtures/ir/metrics/sales-by-store-day.question.json"
    ))
    .unwrap();
    let fields = question.as_object_mut().unwrap();
    fields.remove("compare");
    fields.remove("filter");
    fields.insert("name".into(), json!("studio.tile.yield"));
    fields.insert("objectSet".into(), json!(analytics::TYPE));
    fields.insert("aggregate".into(), json!([{"metric": "demo.yield"}]));
    fields.insert(
        "groupBy".into(),
        json!([{"path": {"property": "occurred_at"}, "grain": "week"}]),
    );
    fields.insert(
        "window".into(),
        json!({"start": "2026-07-06T00:00:00Z", "end": "2026-10-26T00:00:00Z"}),
    );
    fields.insert(
        "freshness".into(),
        json!({"kind": "live", "maxAgeMs": 5000}),
    );
    question
}

fn dashboard(title: &str) -> Value {
    json!({
        "title": title,
        "tiles": [{
            "id": "t1",
            "title": "Yield by week",
            "question": question(),
            "view": { "kind": "chart", "chart": "line" },
            "layout": { "x": 0, "y": 0, "w": 6, "h": 4 }
        }]
    })
}

fn install(id: &str, title: &str, expected: Option<u64>) -> Step {
    let mut body = json!({ "kind": "dashboard", "id": id, "payload": dashboard(title) });
    if let Some(expected) = expected {
        body["expectedVersion"] = json!(expected);
    }
    post("/v1/definitions:install", &body)
}

fn post(path: &str, body: &Value) -> Step {
    Step {
        method: "POST",
        path: path.to_owned(),
        auth: ADMIN,
        body: body.to_string(),
    }
}

fn get(path: &str, auth: &'static str) -> Step {
    Step {
        method: "GET",
        path: path.to_owned(),
        auth,
        body: String::new(),
    }
}

/// The script: commits, a refused commit, a version conflict, an admission
/// that writes nothing, a retirement and reads of what was written.
fn script() -> Vec<Step> {
    let a = "studio.dashboard.conformance-a";
    let b = "studio.dashboard.conformance-b";
    vec![
        get("/v1/definitions", ADMIN),
        install(a, "Board A", None),
        install(b, "Board B", None),
        install(a, "Board A, revised", Some(1)),
        // A stale expected version is refused: nothing commits.
        install(b, "Board B, stale", Some(7)),
        // An empty title is refused by the dashboard checker.
        install("studio.dashboard.conformance-c", "", None),
        post(
            "/v1/definitions:admit",
            &json!({ "kind": "dashboard", "id": "studio.dashboard.conformance-d", "payload": dashboard("Board D") }),
        ),
        post(
            &format!("/v1/definitions/{b}:retire"),
            &json!({ "expectedVersion": 1 }),
        ),
        install(b, "Board B, again", None),
        get(&format!("/v1/definitions/{a}"), GUEST),
        get("/v1/definitions:list", ADMIN),
        get("/v1/definitions", GUEST),
    ]
}

/// The analytics cell as the Durable Object binding opens it.
struct Cell<E: SqlEngine> {
    tenant: Tenant<E>,
    principals: FixturePrincipals,
}

fn open<E: SqlEngine>(engine: E) -> Cell<E> {
    let scope = CellScope {
        org_id: ORG.to_owned(),
        environment_id: ENVIRONMENT.to_owned(),
    };
    assert_eq!(scope, analytics::analytics_scope());
    let analytics::Analytics {
        store,
        plan,
        definitions,
        catalog,
    } = analytics::provision(engine, &scope).unwrap();
    let questions = DemoQuestions::new(plan, scope.clone())
        .try_with_handle_key(Some(LINEAGE_KEY.as_bytes()))
        .unwrap()
        .with_metrics(definitions, catalog);
    let tenant = Tenant::seeded(scope.clone(), &DEMO_GRANTS, store, Some(questions)).unwrap();
    let principals =
        FixturePrincipals::new(DEMO_FIXTURES, tenant.caller()).of_org(FIXTURE_ORG, &scope);
    Cell { tenant, principals }
}

impl<E: SqlEngine> Cell<E> {
    fn serve(&mut self, step: &Step) -> (u16, String) {
        let query = query();
        let request = Request {
            method: step.method,
            path: &step.path,
            query: &query,
            authorization: Some(step.auth),
            now: NOW_MS / 1000,
            body: &step.body,
        };
        let response = self
            .tenant
            .handle_analytics(&self.principals, None, &request)
            .unwrap_or_else(|| self.tenant.handle(&self.principals, &request));
        (response.status, response.body)
    }

    fn head(&self) -> LedgerHead {
        self.tenant.serving_host().unwrap().verified_head().unwrap()
    }
}

/// The record of one run of `steps` from a freshly provisioned cell: the
/// head after provisioning, then per step the status, body and head.
fn record(
    steps: &[Step],
    mut serve: impl FnMut(&Step) -> ((u16, String), LedgerHead),
    opened: &LedgerHead,
) -> Value {
    let steps: Vec<Value> = steps
        .iter()
        .map(|step| {
            let ((status, body), head) = serve(step);
            json!({ "status": status, "body": body, "head": head })
        })
        .collect();
    json!({ "opened": opened, "steps": steps })
}

fn in_memory_run(steps: &[Step]) -> Value {
    let mut cell = open(NativeEngine::open_in_memory().unwrap());
    let opened = cell.head();
    record(
        steps,
        |step| {
            let reply = cell.serve(step);
            (reply, cell.head())
        },
        &opened,
    )
}

/// The same steps with the cell dropped and reopened from its file before
/// every step: a restart between requests changes nothing.
fn reopened_run(steps: &[Step], path: &Path) -> Value {
    let reopen = || open(NativeEngine::open(path, StoreOptions::default()).unwrap());
    let opened = reopen().head();
    record(
        steps,
        |step| {
            let mut cell = reopen();
            let reply = cell.serve(step);
            (reply, cell.head())
        },
        &opened,
    )
}

/// The kill phase: one install per kill point, each on the state the last
/// left, then one acknowledged install killed after its reply.
fn kill_steps() -> Vec<Step> {
    (0..12)
        .map(|index| {
            install(
                &format!("studio.dashboard.kill-{index:02}"),
                &format!("Kill {index}"),
                None,
            )
        })
        .collect()
}

/// The stalled-runner phase: `x` is held mid-commit on one runner while `y`
/// is sent through another.
fn zombie_steps() -> (Step, Step) {
    (
        install("studio.dashboard.zombie-x", "Held", None),
        install("studio.dashboard.zombie-y", "Moved", None),
    )
}

fn final_head(run: &Value) -> Value {
    run["steps"].as_array().unwrap().last().unwrap()["head"].clone()
}

fn wire(steps: &[Step]) -> Vec<Value> {
    let query = query();
    steps
        .iter()
        .map(|step| {
            json!({
                "method": step.method,
                "path": step.path,
                "query": query,
                "authorization": step.auth,
                "body": step.body,
            })
        })
        .collect()
}

#[test]
fn the_reference_record_is_reproducible_and_survives_restarts() {
    let main = script();
    let first = in_memory_run(&main);
    assert_eq!(first, in_memory_run(&main), "two fresh runs differ");
    let directory = tempfile::tempdir().unwrap();
    let restarted = reopened_run(&main, &directory.path().join("cell.sqlite"));
    assert_eq!(
        first, restarted,
        "a restart between steps changed the record"
    );

    // The script exercises what it claims: commits, refusals that commit
    // nothing, and reads.
    let steps = first["steps"].as_array().unwrap();
    let commits = |index: usize| {
        let before = if index == 0 {
            &first["opened"]
        } else {
            &steps[index - 1]["head"]
        };
        steps[index]["head"]["commits"] != before["commits"]
    };
    for (index, step) in steps.iter().enumerate() {
        let status = step["status"].as_u64().unwrap();
        assert!(
            status < 400 || !commits(index),
            "step {index} was refused ({status}) but committed"
        );
    }
    assert!(
        (0..steps.len()).filter(|&index| commits(index)).count() >= 4,
        "the script commits too little: {first:#}"
    );
    assert!(
        steps
            .iter()
            .any(|step| step["status"].as_u64().unwrap() >= 400)
    );

    // Every kill-phase install commits exactly once.
    let kills = kill_steps();
    let kill = in_memory_run(&kills);
    let mut previous = kill["opened"]["commits"].as_u64().unwrap();
    for step in kill["steps"].as_array().unwrap() {
        assert_eq!(step["status"], 200, "{step}");
        let now = step["head"]["commits"].as_u64().unwrap();
        assert_eq!(now, previous + 1);
        previous = now;
    }

    // The stalled-runner outcomes: serialized (x then y), fenced (y alone)
    // and split brain (y then x).
    let (x, y) = zombie_steps();
    let [xy, alone, yx] = [
        vec![x.clone(), y.clone()],
        vec![y.clone()],
        vec![y.clone(), x.clone()],
    ]
    .map(|steps| final_head(&in_memory_run(&steps)));
    assert_ne!(xy, yx, "the two orders must be told apart");

    if let Some(out) = std::env::var_os("NORN_CELL_DO_CONFORMANCE_OUT") {
        let concurrent: Vec<Step> = (0..8)
            .map(|index| {
                install(
                    &format!("studio.dashboard.concurrent-{index}"),
                    "Concurrent",
                    None,
                )
            })
            .collect();
        let document = json!({
            "format": "norn/host-conformance-reference@1",
            "nowMs": NOW_MS,
            "lineageKey": LINEAGE_KEY,
            "script": wire(&main),
            "native": first,
            "kill": { "script": wire(&kills), "native": kill },
            "zombie": { "x": wire(&[x])[0], "y": wire(&[y])[0], "heads": { "xy": xy, "y": alone, "yx": yx } },
            "concurrent": wire(&concurrent),
            "live": wire(&[install("studio.dashboard.live", "Live", None)])[0],
            "reads": wire(&[get("/v1/definitions", ADMIN), get("/v1/definitions:list", ADMIN)]),
            "bench": wire(&[install("studio.dashboard.bench", "Bench", None)])[0],
        });
        std::fs::write(out, serde_json::to_string_pretty(&document).unwrap()).unwrap();
    }
}
