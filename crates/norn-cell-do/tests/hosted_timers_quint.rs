//! TIMER-ALARM-1 / TIMER-RETRY-1 conformance: the hosted timers model's ITF
//! traces (verification/hosted-timers-v1, `initConform`/`stepConform`) are
//! the oracle, replayed against the real `norn_api::timers::run_due` and
//! `next_due` over a real cell, not a Rust copy of the model.
//!
//! Each model slot stands for 32 timer rows due at its deadline, so the
//! model's per-wake budget of two slots is `MAX_DUE_STEPS` = 64 rows. A
//! healthy slot's rows name no run (each is consumed by an empty commit); a
//! failing slot's rows do not decode (an ordinary timer, or a quantity
//! expiry for a `reservation` slot). One model time unit is
//! `STUCK_RETRY_MS`. Every state checks the alarm the cell asks for; every
//! pass checks which slots were handled and failed, in the order the model
//! tries them (the failures `run_due` reports and the timers its commits
//! consume, each in sequence), each failed slot's persisted retry record,
//! and the wake the pass returns.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "trace correspondence: a divergence must fail loudly"
)]

#[path = "support/quint_itf.rs"]
#[allow(dead_code, reason = "the ITF helpers serve both trace tests")]
mod quint_itf;

use std::collections::BTreeMap;
use std::path::Path;

use norn_api::host::DevHost;
use norn_api::scope::CellScope;
use norn_api::timers::{MAX_DUE_STEPS, STUCK_RETRY_MS, next_due, run_due};
use norn_cell::engine::{NativeEngine, SqlEngine, SqlValue};
use norn_cell::{CellStore, Model, StoreOptions};
use norn_kernel::LogicalTime;
use quint_itf::{int, map, traces};
use serde_json::{Value, json};

/// Rows per model slot: two slots fill one wake's budget.
const ROWS: usize = MAX_DUE_STEPS / 2;
/// Logical time of model time 0, in milliseconds.
const BASE: u64 = 10_000_000;
const NONE: i64 = -1;

fn ms(t: i64) -> u64 {
    BASE.saturating_add(
        u64::try_from(t)
            .expect("model time")
            .saturating_mul(STUCK_RETRY_MS),
    )
}

/// Everything about a slot the adapter compares.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Slot {
    live: bool,
    dl: i64,
    kind: String,
    healthy: bool,
    attempts: i64,
    retry_at: i64,
}

fn slots(state: &Value) -> BTreeMap<i64, Slot> {
    let (live, dl, kind, healthy, attempts, retry) = (
        map(state, "live"),
        map(state, "dl"),
        map(state, "kind"),
        map(state, "healthy"),
        map(state, "attempts"),
        map(state, "retryAt"),
    );
    live.keys()
        .map(|&i| {
            (
                i,
                Slot {
                    live: live[&i].as_bool().expect("live"),
                    dl: dl[&i].as_i64().expect("dl"),
                    kind: kind[&i].as_str().expect("kind").to_owned(),
                    healthy: healthy[&i].as_bool().expect("healthy"),
                    attempts: attempts[&i].as_i64().expect("attempts"),
                    retry_at: retry[&i].as_i64().expect("retryAt"),
                },
            )
        })
        .collect()
}

fn row_id(slot: i64, kind: &str, row: usize) -> String {
    if kind == "reservation" {
        format!("norn.quantity.expiry/slot{slot}-{row:02}")
    } else {
        format!("slot{slot}-{row:02}")
    }
}

fn marker() -> Model {
    let def = serde_json::from_value(
        json!({"name":"fixture.Marker","properties":[{"id":"label","type":{"kind":"text"}}]}),
    )
    .unwrap();
    Model::from_definitions(&[def], &[]).unwrap()
}

fn store(path: &Path) -> CellStore<NativeEngine> {
    CellStore::open(path, marker(), StoreOptions::default()).unwrap()
}

fn host(path: &Path) -> DevHost<NativeEngine> {
    DevHost::from_store(
        store(path),
        CellScope {
            org_id: "org_demo".into(),
            environment_id: "dev".into(),
        },
        |_| BTreeMap::new(),
    )
    .unwrap()
}

/// Insert a slot's rows straight into the timer table (another connection).
fn seed(path: &Path, slot: i64, s: &Slot) {
    let engine = NativeEngine::open(path, StoreOptions::default()).unwrap();
    for row in 0..ROWS {
        let id = row_id(slot, &s.kind, row);
        let due = ms(s.dl);
        let timer = if s.healthy {
            json!({"id": id, "due": due, "payload": {"kind": "conformance"}})
        } else {
            json!({"bad": true})
        };
        engine
            .execute(
                &format!(
                    r#"INSERT INTO "_norn_timer" ("timer_id","due","timer","commit_seq") VALUES ('{id}',{due},'{timer}',1)"#
                ),
                &[],
            )
            .unwrap();
    }
}

/// How many of a slot's rows are still pending.
fn pending(path: &Path, slot: i64, kind: &str) -> usize {
    // Read the table itself: the store's typed reader refuses the corrupt
    // rows a failing slot is made of.
    let engine = NativeEngine::open(path, StoreOptions::default()).unwrap();
    engine
        .query(r#"SELECT "timer_id" FROM "_norn_timer""#, &[])
        .unwrap()
        .iter()
        .filter(|row| {
            matches!(row.first(), Some(SqlValue::Text(id))
                if (0..ROWS).any(|n| *id == row_id(slot, kind, n)))
        })
        .count()
}

/// The retry records (TIMER-RETRY-1), item -> (attempts, eligibility),
/// read from the table as the cell left it.
fn retries(path: &Path) -> BTreeMap<String, (i64, u64)> {
    let engine = NativeEngine::open(path, StoreOptions::default()).unwrap();
    engine
        .query(
            r#"SELECT "item","attempts","eligible_at" FROM "_norn_due_retry""#,
            &[],
        )
        .unwrap()
        .into_iter()
        .map(|row| match row.as_slice() {
            [
                SqlValue::Text(item),
                SqlValue::Integer(attempts),
                SqlValue::Integer(at),
            ] => (
                item.clone(),
                (*attempts, u64::try_from(*at).expect("eligibility")),
            ),
            other => panic!("retry row {other:?}"),
        })
        .collect()
}

/// The timers consumed by the commits after `after`, in commit order.
fn consumed_since(path: &Path, after: i64) -> (Vec<String>, i64) {
    let engine = NativeEngine::open(path, StoreOptions::default()).unwrap();
    let rows = engine
        .query(
            r#"SELECT "commit_seq", json_extract("receipt", '$.consumed.timerId') FROM "_norn_receipt" WHERE "commit_seq" > ?1 ORDER BY "commit_seq""#,
            &[SqlValue::Integer(after)],
        )
        .unwrap();
    let mut last = after;
    let mut ids = Vec::new();
    for row in rows {
        match row.as_slice() {
            [SqlValue::Integer(seq), consumed] => {
                last = *seq;
                if let SqlValue::Text(id) = consumed {
                    ids.push(id.clone());
                }
            }
            other => panic!("receipt row {other:?}"),
        }
    }
    (ids, last)
}

/// The row a `DueWork::failed` entry names.
fn failed_row(why: &str) -> String {
    let rest = why
        .strip_prefix("quantity expiry ")
        .or_else(|| why.strip_prefix("timer "))
        .unwrap_or_else(|| panic!("unexpected failure {why}"));
    rest.split_once(": ").map_or(rest, |(id, _)| id).to_owned()
}

/// The model's order of trying items (R2): original deadline, kind, slot.
fn kind_rank(kind: &str) -> u8 {
    match kind {
        "reservation" => 0,
        "timer" => 1,
        _ => 2,
    }
}

/// R2: the failures `run_due` reports and the timers its commits consumed
/// since commit `seen` are the model's tried slots in its order (original
/// deadline, kind, slot), row by row; returns the last commit read.
fn check_order(
    path: &Path,
    old: &BTreeMap<i64, Slot>,
    new: &BTreeMap<i64, Slot>,
    failed: &[String],
    seen: i64,
    at: &str,
) -> i64 {
    let mut tried: Vec<(&i64, &Slot)> = old
        .iter()
        .filter(|(i, s)| (s.live && !new[i].live) || new[i].attempts > s.attempts)
        .collect();
    tried.sort_by_key(|(i, s)| (s.dl, kind_rank(&s.kind), **i));
    let rows = |handled: bool| -> Vec<String> {
        tried
            .iter()
            .filter(|(i, _)| new[i].live != handled)
            .flat_map(|(i, s)| (0..ROWS).map(|row| row_id(**i, &s.kind, row)))
            .collect()
    };
    let (consumed, last) = consumed_since(path, seen);
    assert_eq!(
        failed.iter().map(|why| failed_row(why)).collect::<Vec<_>>(),
        rows(false),
        "{at}: the failures, in the order tried"
    );
    assert_eq!(
        consumed,
        rows(true),
        "{at}: the timers consumed, in the order tried"
    );
    last
}

/// The model's wake as the alarm time (`None` = no alarm).
fn wake_ms(state: &Value) -> Option<u64> {
    let wake = int(state, "wake");
    (wake != NONE).then(|| ms(wake))
}

#[test]
fn hosted_timers_quint_traces() {
    let mut passes = 0_usize;
    let mut failures = 0_usize;
    for (file, states) in traces("hosted-timers") {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("cell.sqlite");
        let cell = host(&path);
        let first = slots(&states[0]);
        for (&slot, s) in &first {
            if s.live {
                seed(&path, slot, s);
            }
        }
        let name = file.display();
        let mut seen = 0_i64;
        for (step, pair) in states.windows(2).enumerate() {
            let (before, after) = (&pair[0], &pair[1]);
            let now = int(before, "now");
            // The alarm the cell asks for, as wake.mjs sets it (never before now).
            let asked = next_due(&cell).unwrap().map(|at| at.max(ms(now)));
            assert_eq!(
                asked,
                wake_ms(before),
                "{name} step {step}: the cell's next deadline"
            );
            let (old, new) = (slots(before), slots(after));
            if old == new {
                // Time passes with nothing due.
                continue;
            }
            let at = int(after, "now");
            let work = run_due(&cell, LogicalTime::from_millis(ms(at))).unwrap();
            passes += 1;
            seen = check_order(
                &path,
                &old,
                &new,
                &work.failed,
                seen,
                &format!("{name} step {step}"),
            );
            let handled = old.iter().filter(|(i, s)| s.live && !new[i].live).count();
            let failed = old
                .iter()
                .filter(|(i, s)| new[i].attempts > s.attempts)
                .count();
            failures += failed;
            assert_eq!(
                usize::try_from(work.timers + work.expired_reservations).unwrap(),
                handled * ROWS,
                "{name} step {step}: handled rows {work:?}"
            );
            assert_eq!(
                work.failed.len(),
                failed * ROWS,
                "{name} step {step}: failed rows {work:?}"
            );
            assert_eq!(
                work.next.map(|next| next.max(ms(at))),
                wake_ms(after),
                "{name} step {step}: the wake the pass returns"
            );
            let retries = retries(&path);
            for (&slot, s) in &new {
                assert_eq!(
                    pending(&path, slot, &s.kind),
                    if s.live { ROWS } else { 0 },
                    "{name} step {step}: slot {slot} rows"
                );
                for row in 0..ROWS {
                    let key = format!("timer/{}", row_id(slot, &s.kind, row));
                    match retries.get(&key) {
                        Some(retry) => {
                            assert!(s.live && s.attempts > 0, "{name} step {step}: stray {key}");
                            assert_eq!(retry.0, s.attempts, "{name} {key} attempts");
                            assert_eq!(retry.1, ms(s.retry_at), "{name} {key} eligibility");
                        }
                        None => assert!(
                            !s.live || s.attempts == 0,
                            "{name} step {step}: {key} has no retry record"
                        ),
                    }
                }
            }
        }
    }
    // The committed traces exercise passes and retries, not only idle time.
    assert!(passes >= 20, "only {passes} passes replayed");
    assert!(failures >= 5, "only {failures} failed slots replayed");
}
