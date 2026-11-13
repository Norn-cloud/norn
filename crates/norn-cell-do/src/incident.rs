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

/// A fresh incident id: the host's clock in milliseconds and a per-process
/// sequence, so ids are unique within an isolate and ordered in its log.
#[must_use]
pub fn id() -> String {
    format!(
        "inc-{:x}-{:x}",
        now_ms(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

/// The response for `refused`. An internal refusal is logged with its cause
/// and carries the incident id; any other refusal is served unchanged.
#[must_use]
pub fn respond(mut refused: Refusal) -> Response {
    if refused.is_internal() {
        let incident = id();
        let cause = refused
            .cause
            .0
            .as_deref()
            .unwrap_or("no cause was recorded for this refusal");
        log(&format!(
            "norn incident {incident}: {} ({}): {cause}",
            refused.code, refused.status
        ));
        refused.incident_id = Some(incident);
    }
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
        eprintln!("{line}");
        RECENT.with(|recent| recent.borrow_mut().push(line.to_owned()));
    }

    pub(super) fn now_ms() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| {
                u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
            })
    }

    /// The lines logged on this thread so far (native builds; for tests).
    pub fn recent() -> Vec<String> {
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
