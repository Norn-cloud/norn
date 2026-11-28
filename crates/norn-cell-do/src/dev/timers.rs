//! TIMER-ALARM-1 / QUESTION-STEP-1 on the dev host: the loop that stands in
//! for the hosted cell's Durable Object alarm. It runs the cell's due work
//! (reservation expiries, flow timers, decision deadlines, and parked
//! `norn.question@1` steps) through [`crate::runs::run_due_with_runtime`] at
//! the dev host's clock, and wakes again at the next deadline, or within
//! [`POLL`] to see deadlines a request created. What stayed due and why goes
//! to stderr; the event stream is stdout.
use std::io::Write as _;
use std::time::Duration;

use norn_api::question_routes::AdHocQuestions;
use norn_kernel::LogicalTime;

use super::http::Serving;
use super::policy::{DevMetrics, DevPrincipals};
use crate::principal::PrincipalResolver as _;

/// The longest the loop sleeps: a deadline a request created is run at most
/// this late.
pub(crate) const POLL: Duration = Duration::from_secs(1);
/// Minimum sleep when a next deadline is already at or behind `now_ms`, so
/// `drive` never busy-spins at 100% CPU.
const MIN_SLEEP: Duration = Duration::from_millis(10);

/// Run the loop until the process exits.
pub(crate) fn drive(serving: &Serving, principals: &DevPrincipals, questions: &DevMetrics) {
    loop {
        std::thread::sleep(tick_with(
            serving,
            Some((principals, questions)),
            super::now_ms(),
        ));
    }
}

/// One wake at `now_ms` without a question runtime (legacy test helper).
#[cfg(test)]
pub(crate) fn tick(serving: &Serving, now_ms: u64) -> Duration {
    tick_with(serving, None, now_ms)
}

/// One wake at `now_ms`: run what is due (dispatching parked `norn.question@1`
/// steps under each run's recorded starter when `wake` supplies the dev host's
/// principals and derived questions runtime), and return how long to sleep
/// before the next wake. A cell not yet serving, or a failure, waits one
/// [`POLL`].
pub(crate) fn tick_with(
    serving: &Serving,
    wake: Option<(&DevPrincipals, &DevMetrics)>,
    now_ms: u64,
) -> Duration {
    let Ok(guard) = serving.read() else {
        return POLL;
    };
    let Some(host) = guard.as_ref() else {
        return POLL;
    };
    let result = match wake {
        Some((principals, questions)) => {
            let derived = questions.questions_at(host, now_ms).ok();
            let runtime = derived
                .as_ref()
                .map(|rt| -> &dyn AdHocQuestions<norn_cell::engine::NativeEngine> { rt });
            if let Some(slot) = principals.caller() {
                crate::runs::run_due_with_runtime(
                    host,
                    slot,
                    crate::policy::DEMO_GRANTS.version,
                    runtime,
                    now_ms,
                )
            } else {
                norn_api::timers::run_due(host, LogicalTime::from_millis(now_ms))
            }
        }
        None => norn_api::timers::run_due(host, LogicalTime::from_millis(now_ms)),
    };
    match result {
        Ok(work) => {
            for failed in &work.failed {
                let _written = writeln!(
                    std::io::stderr().lock(),
                    "norn dev: due work not handled: {failed}"
                );
            }
            work.next.map_or(POLL, |next| {
                let wait = next.saturating_sub(now_ms);
                if wait == 0 {
                    if work.failed.is_empty() {
                        MIN_SLEEP
                    } else {
                        POLL
                    }
                } else {
                    Duration::from_millis(wait).min(POLL)
                }
            })
        }
        Err(refusal) => {
            let _written = writeln!(
                std::io::stderr().lock(),
                "norn dev: due work failed: {}",
                refusal.code
            );
            POLL
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test fixtures")]
mod tests {
    use std::sync::{Arc, RwLock};

    use norn_api::host::DevHost;
    use norn_api::scope::CellScope;
    use norn_cell::engine::{NativeEngine, SqlEngine};
    use norn_cell::{CellStore, Model, StoreOptions};

    use super::{POLL, tick};

    fn model() -> Model {
        let def = serde_json::from_value(serde_json::json!(
            {"name":"fixture.Marker","properties":[{"id":"label","type":{"kind":"text"}}]}
        ))
        .unwrap();
        Model::from_definitions(&[def], &[]).unwrap()
    }

    /// TIMER-RETRY-1: the dev host's loop wakes every second, yet a due item
    /// that cannot be handled is tried once per backoff, as on the hosted
    /// cell: the retry eligibility is the cell's, not the loop's.
    #[test]
    fn the_dev_loop_respects_a_failed_items_backoff() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("cell.sqlite");
        let store = CellStore::open(&path, model(), StoreOptions::default()).unwrap();
        // A timer no host can deliver (its record does not decode), added
        // by another connection to the file.
        NativeEngine::open(&path, StoreOptions::default())
            .unwrap()
            .execute(
                r#"INSERT INTO "_norn_timer" ("timer_id","due","timer","commit_seq") VALUES ('stuck',1000,'{"bad":true}',1)"#,
                &[],
            )
            .unwrap();
        let scope = CellScope {
            org_id: "org_demo".into(),
            environment_id: "dev".into(),
        };
        let host =
            DevHost::from_store(store, scope, |_| std::collections::BTreeMap::new()).unwrap();
        let serving = Arc::new(RwLock::new(Some(host)));
        let attempts = || {
            let reader = NativeEngine::open(&path, StoreOptions::default()).unwrap();
            reader
                .query(
                    r#"SELECT "attempts","eligible_at" FROM "_norn_due_retry" WHERE "item"='timer/stuck'"#,
                    &[],
                )
                .unwrap()
                .into_iter()
                .next()
                .map(|row| {
                    let row = norn_cell::engine::SqlRow(row);
                    (row.get::<i64>(0).unwrap(), row.get::<i64>(1).unwrap())
                })
        };
        assert_eq!(tick(&serving, 2_000), POLL);
        assert_eq!(attempts(), Some((1, 62_000)));
        // The loop's next wakes, a second apart, try nothing until 62 s.
        for now in [3_000, 4_000, 61_999] {
            tick(&serving, now);
            assert_eq!(attempts(), Some((1, 62_000)), "at {now}");
        }
        tick(&serving, 62_000);
        assert_eq!(attempts(), Some((2, 182_000)));
    }
}
