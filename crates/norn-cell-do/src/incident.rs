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

/// A fresh opaque 128-bit random incident id (`inc-<32 lowercase hex>`).
/// It carries no per-isolate counter or clock value, so two callers sharing
/// an isolate cannot correlate each other's error rate (#1486).
#[must_use]
pub fn id() -> String {
    format!("inc-{:032x}", random_u128())
}

/// The response for `refused`. An internal refusal is logged with its cause
/// and carries the incident id; any other refusal is served unchanged.
#[must_use]
pub fn respond(mut refused: Refusal) -> Response {
    if refused.is_internal() {
        let incident = id();
        let seq = NEXT.fetch_add(1, Ordering::Relaxed);
        let internal = refused.extra.get_or_insert_with(Box::default);
        let cause = internal
            .cause
            .clone()
            .unwrap_or_else(|| String::from("no cause was recorded for this refusal"));
        log(&format!(
            "norn incident {incident} (seq {seq}): {} ({}): {cause}",
            refused.code, refused.status
        ));
        if let Some(internal) = refused.extra.as_mut() {
            internal.incident_id = Some(incident);
        }
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
        #[wasm_bindgen(js_namespace = crypto, js_name = getRandomValues)]
        fn get_random_values(buf: &mut [u8]);
    }

    pub(super) fn log(line: &str) {
        console_error(line);
    }

    pub(super) fn random_u128() -> u128 {
        let mut bytes = [0_u8; 16];
        get_random_values(&mut bytes);
        u128::from_be_bytes(bytes)
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

    /// 128 bits of OS entropy for an opaque incident identifier.
    pub(super) fn random_u128() -> u128 {
        let mut bytes = [0_u8; 16];
        let _ignored = getrandom::fill(&mut bytes);
        u128::from_be_bytes(bytes)
    }

    /// The lines logged on this thread so far (native builds; for tests).
    pub(super) fn recent() -> Vec<String> {
        RECENT.with(|recent| recent.borrow().clone())
    }
}

fn log(line: &str) {
    sink::log(line);
}

fn random_u128() -> u128 {
    sink::random_u128()
}

/// The incident lines logged on this thread so far (native builds only).
#[cfg(not(target_arch = "wasm32"))]
#[must_use]
pub fn recent() -> Vec<String> {
    sink::recent()
}
