//! INCIDENT-1: an internal failure is never silent. Every refusal becomes a
//! response here, and an internal one is logged with its cause (the error's
//! source chain) under an incident id that the caller gets back in the
//! problem body. The cause never reaches the caller; codes, categories and
//! messages stay as they were.
//!
//! The log carries the error kind and its chain only: refusal causes are
//! host and engine errors, which name tables, codes and limits, never request
//! bodies or credentials.

use std::sync::atomic::{AtomicU64, Ordering};

use norn_api::refusal::Refusal;

use crate::server::Response;

static NEXT: AtomicU64 = AtomicU64::new(0);

/// A fresh incident id: in the Worker, the host's clock in milliseconds
/// (natively, a random number) and a per-process sequence, so ids are unique
/// within an isolate and ordered in its log.
#[must_use]
pub fn id() -> String {
    format!(
        "inc-{:x}-{:x}",
        now_ms(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

/// Format a Unix timestamp in seconds as RFC 3339 UTC (`YYYY-MM-DDTHH:MM:SSZ`),
/// using Howard Hinnant's civil-from-days algorithm.
#[must_use]
#[allow(
    clippy::arithmetic_side_effects,
    reason = "bounded calendar decomposition over u64 Unix seconds in i128"
)]
pub fn format_utc(unix_secs: u64) -> String {
    let secs_per_day: u64 = 86_400;
    let days = unix_secs / secs_per_day;
    let rem = unix_secs % secs_per_day;
    let hour = rem / 3_600;
    let minute = (rem % 3_600) / 60;
    let second = rem % 60;
    let z = i128::from(days) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Prepare `refused` at the server boundary: log internal causes under a fresh
/// incident id (INCIDENT-1) and stamp `decidedAt` when a host clock (`now` in
/// Unix seconds) is provided.
pub fn prepare(refused: &mut Refusal, now: Option<u64>) {
    if refused.is_internal() {
        let incident = id();
        let internal = refused.extra.get_or_insert_with(Box::default);
        let cause = internal
            .cause
            .clone()
            .unwrap_or_else(|| String::from("no cause was recorded for this refusal"));
        log(&format!(
            "norn incident {incident}: {} ({}): {cause}",
            refused.code, refused.status
        ));
        if let Some(internal) = refused.extra.as_mut() {
            internal.incident_id = Some(incident);
        }
    }
    if let Some(now) = now {
        refused.set_decided_at(format_utc(now));
    }
}

/// The response for `refused`. An internal refusal is logged with its cause
/// and carries the incident id; any other refusal is served unchanged.
#[must_use]
pub fn respond(mut refused: Refusal) -> Response {
    prepare(&mut refused, None);
    Response {
        status: refused.status,
        body: serde_json::to_string(&refused).unwrap_or_else(|_| String::from("{}")),
    }
}

/// The response for `refused`, stamped with `decidedAt` from the host clock
/// (`now`, Unix seconds) and logged under an incident id when internal.
#[must_use]
pub fn respond_at(mut refused: Refusal, now: u64) -> Response {
    prepare(&mut refused, Some(now));
    Response {
        status: refused.status,
        body: serde_json::to_string(&refused).unwrap_or_else(|_| String::from("{}")),
    }
}

/// Log a panic's payload and location before the runtime aborts (installed
/// once by the Worker entry point).
pub fn install_panic_log() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            log(&format!("norn panic: {info}"));
            previous(info);
        }));
    });
}

#[cfg(target_arch = "wasm32")]
mod sink {
    use wasm_bindgen::prelude::*;

    #[wasm_bindgen]
    extern "C" {
        #[wasm_bindgen(js_namespace = console, js_name = error)]
        fn console_error(line: &str);
        #[wasm_bindgen(js_namespace = Date, js_name = now)]
        fn date_now() -> f64;
    }

    pub(super) fn log(line: &str) {
        console_error(line);
    }

    #[allow(
        clippy::as_conversions,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "Date.now() is a non-negative whole number of milliseconds"
    )]
    pub(super) fn now_ms() -> u64 {
        date_now() as u64
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod sink {
    use std::cell::RefCell;

    thread_local! {
        static RECENT: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    }

    pub(super) fn log(line: &str) {
        use std::io::Write as _;
        // A failed write to stderr has nowhere better to go.
        let _ignored = writeln!(std::io::stderr(), "{line}");
        RECENT.with(|recent| recent.borrow_mut().push(line.to_owned()));
    }

    /// A random per-id prefix (natively there is no ambient clock to read).
    pub(super) fn now_ms() -> u64 {
        getrandom::u64().unwrap_or_default()
    }

    /// The lines logged on this thread so far (native builds; for tests).
    pub(super) fn recent() -> Vec<String> {
        RECENT.with(|recent| recent.borrow().clone())
    }
}

fn log(line: &str) {
    sink::log(line);
}

fn now_ms() -> u64 {
    sink::now_ms()
}

/// The incident lines logged on this thread so far (native builds only).
#[cfg(not(target_arch = "wasm32"))]
#[must_use]
pub fn recent() -> Vec<String> {
    sink::recent()
}

#[cfg(test)]
mod tests {
    use super::format_utc;

    #[test]
    fn formats_unix_seconds_as_rfc3339_utc() {
        assert_eq!(format_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_utc(1_800_000_000), "2027-01-15T08:00:00Z");
        // Leap day: 2024-02-29T23:59:59Z
        assert_eq!(format_utc(1_709_251_199), "2024-02-29T23:59:59Z");
    }
}
