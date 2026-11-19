//! TIMER-ALARM-1 on the dev host: the loop that stands in for the hosted
//! cell's Durable Object alarm. It runs the cell's due work (reservation
//! expiries, flow timers, decision deadlines) through the same
//! [`norn_api::timers::run_due`] at the dev host's clock, and wakes again at
//! the next deadline, or within [`POLL`] to see deadlines a request created.
//! What stayed due and why goes to stderr; the event stream is stdout.
use std::io::Write as _;
use std::time::Duration;

use norn_kernel::LogicalTime;

use super::http::Serving;

/// The longest the loop sleeps: a deadline a request created is run at most
/// this late.
pub(crate) const POLL: Duration = Duration::from_secs(1);

/// Run the loop until the process exits.
pub(crate) fn drive(serving: &Serving) {
    loop {
        std::thread::sleep(tick(serving, super::now_ms()));
    }
}

/// One wake at `now_ms`: run what is due, and how long to sleep before the
/// next wake. A cell not yet serving, or a failure, waits one [`POLL`].
pub(crate) fn tick(serving: &Serving, now_ms: u64) -> Duration {
    let Ok(guard) = serving.read() else {
        return POLL;
    };
    let Some(host) = guard.as_ref() else {
        return POLL;
    };
    match norn_api::timers::run_due(host, LogicalTime::from_millis(now_ms)) {
        Ok(work) => {
            for failed in &work.failed {
                let _written = writeln!(
                    std::io::stderr().lock(),
                    "norn dev: due work not handled: {failed}"
                );
            }
            work.next.map_or(POLL, |next| {
                Duration::from_millis(next.saturating_sub(now_ms)).min(POLL)
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
