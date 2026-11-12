//! D102: this host's transports to the tenant analytics engine. The seam,
//! the exchange and the routes are `norn_api::analytics_routes`; a host only
//! says how a call reaches the engine.

use std::sync::{Mutex, PoisonError};

use norn_api::analytics_routes::{EngineAnswer, EngineCall, Transport};
#[cfg(all(feature = "duckdb", not(target_arch = "wasm32")))]
use norn_cell::analytics::EngineRefusal;
#[cfg(all(feature = "duckdb", not(target_arch = "wasm32")))]
use serde::Serialize;

/// The Durable Object transport: a call ends the request; the Worker performs
/// it and re-enters with the answer.
#[derive(Default)]
pub struct Exchange {
    answer: Mutex<Option<EngineAnswer>>,
    pending: Mutex<Option<EngineCall>>,
}

impl Exchange {
    /// Re-enter with the answer to the previous call.
    pub fn resume(&self, answer: EngineAnswer) {
        *self.answer.lock().unwrap_or_else(PoisonError::into_inner) = Some(answer);
        *self.pending.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }

    /// The call this request ended with, if any; taken once.
    pub fn take_pending(&self) -> Option<EngineCall> {
        *self.answer.lock().unwrap_or_else(PoisonError::into_inner) = None;
        self.pending.lock().unwrap_or_else(PoisonError::into_inner).take()
    }
}

impl Transport for Exchange {
    fn resumed(&self) -> Option<EngineAnswer> {
        self.answer.lock().unwrap_or_else(PoisonError::into_inner).take()
    }
    fn call(&self, call: &EngineCall) -> Option<EngineAnswer> {
        *self.pending.lock().unwrap_or_else(PoisonError::into_inner) = Some(call.clone());
        None
    }
}

/// The engine linked into a native host: each call runs in place.
#[cfg(all(feature = "duckdb", not(target_arch = "wasm32")))]
pub struct InProcess(pub norn_cell::analytics::engine::Engine);

#[cfg(all(feature = "duckdb", not(target_arch = "wasm32")))]
impl InProcess {
    fn run<T: serde::de::DeserializeOwned, R: Serialize>(
        body: &str,
        operation: impl FnOnce(&T) -> norn_cell::analytics::engine::Outcome<R>,
    ) -> (u16, String) {
        let outcome = serde_json::from_str::<T>(body)
            .map_err(|e| EngineRefusal {
                code: "analytics.invalid".to_owned(),
                detail: e.to_string(),
            })
            .and_then(|request| operation(&request));
        match outcome {
            Ok(response) => (200, serde_json::to_string(&response).unwrap_or_default()),
            Err(refused) => (
                refusal_status(&refused.code),
                serde_json::to_string(&refused).unwrap_or_default(),
            ),
        }
    }
}

#[cfg(all(feature = "duckdb", not(target_arch = "wasm32")))]
impl Transport for InProcess {
    fn resumed(&self) -> Option<EngineAnswer> {
        None
    }
    fn call(&self, call: &EngineCall) -> Option<EngineAnswer> {
        let engine = &self.0;
        let (status, body) = match call.op.as_str() {
            "frontier" => Self::run(&call.body, |r| engine.frontier(r)),
            "sync" => Self::run(&call.body, |r| engine.sync(r)),
            "evaluate" => Self::run(&call.body, |r| engine.evaluate(r)),
            "members" => Self::run(&call.body, |r| engine.members(r)),
            "profile" => Self::run(&call.body, |r| engine.profile(r)),
            _ => (404, String::new()),
        };
        Some(EngineAnswer {
            op: call.op.clone(),
            digest: call.digest.clone(),
            status,
            body,
        })
    }
}

/// The status an engine refusal answers with (the sidecar uses the same).
#[must_use]
pub fn refusal_status(code: &str) -> u16 {
    match code {
        "analytics.frontier_moved" | "analytics.frontier_behind" | "core.compute.stale_input" => 409,
        "analytics.engine" => 500,
        code if code.starts_with("core.compute.") || code.starts_with("bulk.") => 422,
        _ => 400,
    }
}

