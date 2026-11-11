//! D79 D step 1b(b): the Durable Object request layer under storage faults.
//!
//! A seeded simulator drives `server::handle_all` over the analytics cell,
//! on the Durable Object transaction glue run natively (`DoEngine` over the
//! `transactionSync` emulator) behind a fault-injecting engine, with a file
//! store. Each case is a generated sequence of the requests the DO serves
//! (definition installs, retirements and admissions; definition, catalog and
//! object reads), some under an armed fault (a crash or an I/O error at a
//! statement or commit point, or a failed storage commit), and simulated
//! evictions (the host is dropped and reopened from the same file).
//!
//! The oracle is differential: the same sequence on an unfaulted in-memory
//! host, with the requests whose fault fired left out. The laws:
//!
//! 1. A request whose fault fired fails closed: a non-2xx response whose
//!    body is a typed refusal (it has a `code`).
//! 2. Every other request gets byte-for-byte the oracle's response, so a
//!    fault leaves no partial state and the request after a fault or an
//!    eviction sees exactly the oracle's state.
//! 3. After every fired fault and eviction, and at the end, a state probe
//!    (every definition version, the catalog) equals the oracle's.
//! 4. The same case run twice gives byte-identical responses.
//!
//! A host crash kills the engine, so the simulated DO is evicted (reopened)
//! right after a fired crash, as the platform restarts it.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "fault simulator"
)]

use std::path::Path;

use norn_api::definition_routes::DefinitionKinds;
use norn_api::host::DevHost;
use norn_cell::engine::{
    DoEngine, DoStorage, DoStorageEmulator, EngineFault, EngineFaultKind, FaultEngine, FaultSwitch,
    NativeEngine, SqlEngine, SqlValue, Thrown, Transact,
};
use norn_cell::{CellError, StoreOptions};
use norn_cell_do::analytics::{self, DemoQuestions};
use norn_cell_do::policy::{DEMO_GRANTS, ReadPolicy};
use norn_cell_do::principal::{DEMO_FIXTURES, FixturePrincipals};
use norn_cell_do::server::{self, Response};
use serde_json::{Value, json};

const SCOPE: &str = "orgId=org_demo&environmentId=analytics";
/// The fixture principal holding `admin` on the analytics cell.
const ADMIN: Option<&str> = Some("Bearer fixture:viewer@demo");
/// The host secret the cell's lineage handle key derives from.
const HANDLE_KEY: [u8; 32] = [5; 32];
/// The request clock: every receipt records the same time, so responses are
/// reproducible.
const NOW: u64 = 1_800_000_000;
/// Cases per run unless `PROPTEST_CASES` is set (a soak raises it).
const CASES: u64 = 16;
/// The fixed seed of the case set.
const SEED: u64 = 0x6e6f_726e_2d64_6f72; // "norn-dor"
/// Every this many cases, the system run is repeated to check determinism.
const REPLAY_EVERY: u64 = 4;
/// Definition ids the generator uses: few, so writes conflict and replace.
const IDS: u64 = 3;

// ------------------------------------------------------------------ cells

/// One opened analytics cell and the services its requests are served with.
struct Cell<E: SqlEngine> {
    host: DevHost<E>,
    questions: DemoQuestions,
    principals: FixturePrincipals,
    kinds: DefinitionKinds,
}

fn open<E: SqlEngine>(engine: E) -> Cell<E> {
    let scope = analytics::analytics_scope();
    let opened = analytics::provision(engine, &scope).expect("analytics cell provisions");
    let policy =
        ReadPolicy::compile(opened.store.model().object_types(), &scope, &DEMO_GRANTS).unwrap();
    let principals = FixturePrincipals::new(DEMO_FIXTURES, policy.caller());
    let host = DevHost::from_store(opened.store, scope.clone(), |command| {
        command.payload.clone()
    })
    .unwrap()
    .with_authorizer(policy);
    let questions = DemoQuestions::new(opened.plan, scope)
        .with_handle_key(&HANDLE_KEY)
        .with_metrics(opened.definitions, opened.catalog);
    Cell {
        host,
        questions,
        principals,
        kinds: DefinitionKinds::default(),
    }
}

/// A Durable Object storage over a native connection, failing its commits
/// on the switch's armed commit faults.
trait Storage: DoStorage + Sized + 'static {
    fn over(native: NativeEngine, faults: &FaultSwitch) -> Self;
}

impl Storage for DoStorageEmulator {
    fn over(native: NativeEngine, faults: &FaultSwitch) -> Self {
        Self::new(native).with_faults(faults.clone())
    }
}

/// The system under test: the DO glue over storage `S`, behind the faults.
type System<S> = FaultEngine<DoEngine<S>>;

fn open_system<S: Storage>(path: &Path, faults: &FaultSwitch) -> Cell<System<S>> {
    let native = NativeEngine::open(path, StoreOptions::default()).expect("opens the file");
    open(FaultEngine::new(
        DoEngine::new(S::over(native, faults)),
        faults.clone(),
    ))
}

fn oracle() -> Cell<DoEngine> {
    open(DoEngine::new(DoStorageEmulator::new(
        NativeEngine::open_in_memory().expect("opens memory"),
    )))
}

// ------------------------------------------------------------------ requests

/// One request the DO serves, as the generator draws it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Req {
    /// Install a dashboard (`title` 3 is inadmissible) at `expected`.
    Install {
        id: u64,
        title: u64,
        expected: Option<u64>,
    },
    /// Retire a definition at `expected`.
    Retire { id: u64, expected: u64 },
    /// Admit a dashboard: validated, never written.
    Admit { id: u64, title: u64 },
    /// Read a definition, current or at `version`.
    Get { id: u64, version: Option<u64> },
    /// List the dashboards, with the retired ones or not.
    List { retired: bool },
    /// The definitions catalog.
    Catalog,
    /// A page of the cell's objects.
    Objects,
}

/// One step of a case.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    /// Serve a request.
    Serve(Req),
    /// Serve a request with a storage fault armed for it.
    Faulted(EngineFault, Req),
    /// The DO is evicted: the host is dropped and reopened from its file.
    Evict,
}

fn definition_id(id: u64) -> String {
    format!("studio.dashboard.d{id}")
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

fn dashboard(title: u64) -> Value {
    // An empty title is refused by the dashboard checker.
    let title = if title == 3 {
        String::new()
    } else {
        format!("Board {title}")
    };
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

/// The request's method, path, query and body.
fn render(req: Req) -> (&'static str, String, String, String) {
    let scope = SCOPE.to_owned();
    match req {
        Req::Install {
            id,
            title,
            expected,
        } => {
            let mut body = json!({ "kind": "dashboard", "id": definition_id(id), "payload": dashboard(title) });
            if let Some(expected) = expected {
                body["expectedVersion"] = json!(expected);
            }
            (
                "POST",
                "/v1/definitions:install".to_owned(),
                scope,
                body.to_string(),
            )
        }
        Req::Retire { id, expected } => (
            "POST",
            format!("/v1/definitions/{}:retire", definition_id(id)),
            scope,
            json!({ "expectedVersion": expected }).to_string(),
        ),
        Req::Admit { id, title } => (
            "POST",
            "/v1/definitions:admit".to_owned(),
            scope,
            json!({ "kind": "dashboard", "id": definition_id(id), "payload": dashboard(title) })
                .to_string(),
        ),
        Req::Get { id, version } => (
            "GET",
            format!("/v1/definitions/{}", definition_id(id)),
            version.map_or(scope, |version| format!("{SCOPE}&version={version}")),
            String::new(),
        ),
        Req::List { retired } => (
            "GET",
            "/v1/definitions:list".to_owned(),
            if retired {
                format!("{SCOPE}&kind=dashboard&include=retired")
            } else {
                format!("{SCOPE}&kind=dashboard")
            },
            String::new(),
        ),
        Req::Catalog => ("GET", "/v1/definitions".to_owned(), scope, String::new()),
        Req::Objects => (
            "GET",
            format!("/v1/objects/{}", analytics::TYPE),
            format!("{SCOPE}&limit=2"),
            String::new(),
        ),
    }
}

fn call<E: SqlEngine>(cell: &Cell<E>, req: Req) -> Response {
    let (method, path, query, body) = render(req);
    let request = server::Request {
        method,
        path: &path,
        query: &query,
        authorization: ADMIN,
        now: NOW,
        body: &body,
    };
    server::handle_all(
        &cell.host,
        &cell.principals,
        Some(&cell.questions),
        &cell.kinds,
        &request,
    )
}

/// The cell's observable definitions state: every version of every id, the
/// list with the retired ones, and the catalog.
fn probe<E: SqlEngine>(cell: &Cell<E>) -> Vec<Response> {
    let mut reads = vec![Req::List { retired: true }, Req::Catalog];
    for id in 0..IDS {
        reads.push(Req::Get { id, version: None });
        for version in 1..=4 {
            reads.push(Req::Get {
                id,
                version: Some(version),
            });
        }
    }
    reads.into_iter().map(|req| call(cell, req)).collect()
}

// ------------------------------------------------------------------ generator

/// `SplitMix64`: a tiny deterministic generator, so a case is its seed.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// Uniform in `0..n` (`n > 0`).
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    /// True with probability `percent`/100.
    fn chance(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }
}

fn write_request(rng: &mut Rng) -> Req {
    let id = rng.below(IDS);
    match rng.below(10) {
        0..=5 => Req::Install {
            id,
            title: rng.below(4),
            expected: if rng.chance(50) {
                None
            } else {
                Some(1 + rng.below(3))
            },
        },
        6..=8 => Req::Retire {
            id,
            expected: 1 + rng.below(3),
        },
        _ => Req::Admit {
            id,
            title: rng.below(4),
        },
    }
}

fn request(rng: &mut Rng) -> Req {
    let id = rng.below(IDS);
    match rng.below(10) {
        0..=4 => write_request(rng),
        5..=6 => Req::Get {
            id,
            version: rng.chance(50).then(|| 1 + rng.below(3)),
        },
        7 => Req::List {
            retired: rng.chance(50),
        },
        8 => Req::Catalog,
        _ => Req::Objects,
    }
}

fn fault(rng: &mut Rng) -> EngineFault {
    let kind = match rng.below(3) {
        0 => EngineFaultKind::Crash,
        1 => EngineFaultKind::Io,
        _ => EngineFaultKind::Commit,
    };
    EngineFault {
        kind,
        // Mostly the request's first write transaction; sometimes a later
        // one, which most requests never run.
        transaction: if rng.chance(80) { 0 } else { 1 },
        // Mostly an early statement; sometimes past the last (commit point).
        at_statement: if rng.chance(75) {
            u32::try_from(rng.below(16)).unwrap()
        } else {
            u32::try_from(rng.below(256)).unwrap()
        },
    }
}

/// Case `case` of the fixed seed set: 1 to 12 steps.
fn generate(case: u64) -> Vec<Step> {
    let mut rng = Rng(SEED ^ case.wrapping_mul(0x2545_f491_4f6c_dd1d));
    let steps = 1 + rng.below(12);
    (0..steps)
        .map(|_| match rng.below(100) {
            0..=11 => Step::Evict,
            12..=46 => {
                let fault = fault(&mut rng);
                // Faults are aimed mostly at writes; a read runs no write
                // transaction, so its fault does not fire.
                let req = if rng.chance(80) {
                    write_request(&mut rng)
                } else {
                    request(&mut rng)
                };
                Step::Faulted(fault, req)
            }
            _ => Step::Serve(request(&mut rng)),
        })
        .collect()
}

// ------------------------------------------------------------------ laws

/// What one run of a case exercised.
#[derive(Debug, Default)]
struct Stats {
    crash: u64,
    io: u64,
    commit: u64,
    unfired: u64,
    evictions: u64,
    server_errors: u64,
}

fn fail_closed(response: &Response) -> Result<(), String> {
    let typed = serde_json::from_str::<Value>(&response.body)
        .ok()
        .is_some_and(|body| body["code"].is_string());
    if (200..300).contains(&response.status) || !typed {
        return Err(format!(
            "law violated: a faulted request did not fail closed: {} {}",
            response.status, response.body
        ));
    }
    Ok(())
}

fn same_state<E: SqlEngine, O: SqlEngine>(
    system: &Cell<E>,
    oracle: &Cell<O>,
    when: &str,
) -> Result<(), String> {
    let (system, oracle) = (probe(system), probe(oracle));
    if system != oracle {
        return Err(format!(
            "law violated: the state {when} differs from the oracle's\nsystem: {system:?}\noracle: {oracle:?}"
        ));
    }
    Ok(())
}

/// Runs one case on storage `S` against the oracle and returns the system's
/// responses (a fired fault's included).
fn simulate<S: Storage>(steps: &[Step], stats: &mut Stats) -> Result<Vec<Response>, String> {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("cell.sqlite");
    let faults = FaultSwitch::default();
    let oracle = oracle();
    let mut system = open_system::<S>(&path, &faults);
    let mut transcript = Vec::new();
    for (ix, step) in steps.iter().enumerate() {
        match *step {
            Step::Serve(req) => {
                let (got, want) = (call(&system, req), call(&oracle, req));
                if got != want {
                    return Err(format!(
                        "law violated: step {ix} {req:?} differs from the oracle\nsystem: {got:?}\noracle: {want:?}"
                    ));
                }
                transcript.push(got);
            }
            Step::Faulted(fault, req) => {
                faults.arm(fault);
                let got = call(&system, req);
                if faults.disarm() {
                    fail_closed(&got).map_err(|error| format!("step {ix} {req:?}: {error}"))?;
                    if got.status >= 500 {
                        stats.server_errors += 1;
                    }
                    match fault.kind {
                        EngineFaultKind::Crash => {
                            stats.crash += 1;
                            // The platform restarts a crashed DO.
                            drop(system);
                            system = open_system::<S>(&path, &faults);
                            stats.evictions += 1;
                        }
                        EngineFaultKind::Io => stats.io += 1,
                        EngineFaultKind::Commit => stats.commit += 1,
                    }
                    same_state(&system, &oracle, &format!("after the fault at step {ix}"))?;
                } else {
                    stats.unfired += 1;
                    let want = call(&oracle, req);
                    if got != want {
                        return Err(format!(
                            "law violated: step {ix} {req:?} (fault unfired) differs from the oracle\nsystem: {got:?}\noracle: {want:?}"
                        ));
                    }
                }
                transcript.push(got);
            }
            Step::Evict => {
                drop(system);
                system = open_system::<S>(&path, &faults);
                stats.evictions += 1;
                same_state(
                    &system,
                    &oracle,
                    &format!("after the eviction at step {ix}"),
                )?;
            }
        }
    }
    same_state(&system, &oracle, "at the end")?;
    transcript.extend(probe(&system));
    Ok(transcript)
}

fn cases() -> u64 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|cases| cases.parse().ok())
        .unwrap_or(CASES)
}

/// Runs the fixed case set on storage `S`; the first violation names its
/// seed, case and steps.
fn run<S: Storage>() -> Result<Stats, String> {
    let mut stats = Stats::default();
    for case in 0..cases() {
        let steps = generate(case);
        let report =
            |error: String| format!("{error}\nseed: {SEED:#x}, case: {case}\nsteps: {steps:#?}");
        let first = simulate::<S>(&steps, &mut stats).map_err(report)?;
        if case % REPLAY_EVERY == 0 {
            let second = simulate::<S>(&steps, &mut Stats::default()).map_err(report)?;
            if first != second {
                return Err(report(
                    "law violated: the same case gave different responses".to_owned(),
                ));
            }
        }
    }
    Ok(stats)
}

#[test]
fn do_requests_fail_closed_and_match_the_oracle_under_faults() {
    let stats = run::<DoStorageEmulator>().unwrap_or_else(|failure| panic!("{failure}"));
    assert!(
        stats.crash > 0 && stats.io > 0 && stats.commit > 0,
        "every fault kind must fire: {stats:?}"
    );
    assert!(
        stats.unfired > 0 && stats.evictions > 0,
        "unfired faults and evictions must occur: {stats:?}"
    );
}

// ------------------------------------------------------------------ mutant

/// Mutant storage: it ignores the rollback marker, commits what the request
/// wrote before its fault, and only then rethrows: a write survives a fault.
#[derive(Debug)]
struct CommitsOnRollback(DoStorageEmulator);

impl Storage for CommitsOnRollback {
    fn over(native: NativeEngine, faults: &FaultSwitch) -> Self {
        Self(DoStorageEmulator::over(native, faults))
    }
}

impl Transact for CommitsOnRollback {
    type Thrown = Thrown;

    fn transaction_sync<F: FnMut() -> Result<(), Thrown>>(
        &self,
        callback: &mut F,
    ) -> Result<(), Thrown> {
        let mut swallowed = None;
        // The bug: a throw does not stop the commit.
        let mut commit_anyway = || {
            callback().or_else(|thrown| {
                swallowed = Some(thrown);
                Ok(())
            })
        };
        let result = self.0.transaction_sync(&mut commit_anyway);
        swallowed.map_or(result, Err)
    }

    fn rollback_marker(&self) -> Thrown {
        self.0.rollback_marker()
    }

    fn is_rollback_marker(&self, thrown: &Thrown) -> bool {
        self.0.is_rollback_marker(thrown)
    }

    fn callback_reused(&self) -> Thrown {
        self.0.callback_reused()
    }

    fn engine_error(&self, thrown: &Thrown) -> CellError {
        self.0.engine_error(thrown)
    }
}

impl DoStorage for CommitsOnRollback {
    fn exec(&self, sql: &str, params: &[SqlValue]) -> Result<Vec<Vec<SqlValue>>, CellError> {
        self.0.exec(sql, params)
    }

    fn exec_batch(&self, sql: &str) -> Result<(), CellError> {
        self.0.exec_batch(sql)
    }
}

/// The request-layer laws can fail: a storage that lets a faulted request's
/// writes survive is caught by the oracle.
#[test]
fn do_request_mutant_whose_writes_survive_a_fault_is_caught() {
    let failure = run::<CommitsOnRollback>()
        .expect_err("a storage whose writes survive a fault must fail the request laws");
    assert!(failure.contains("law violated"), "{failure}");
}
