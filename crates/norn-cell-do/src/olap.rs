//! D102: this host's transports to the tenant analytics engine. The seam,
//! the exchange and the routes are `norn_api::analytics_routes`; a host only
//! says how a call reaches the engine.

use std::sync::{Mutex, PoisonError};

use norn_api::analytics_routes::{EngineAnswer, EngineCall, Transport};
use norn_api::olap_credential::{Issuer, body_digest};
#[cfg(all(feature = "duckdb", not(target_arch = "wasm32")))]
use norn_cell::analytics::{
    AnalyticalEngine, EngineOutcome, EngineRefusal, STALE_INPUT_CODE, dispatch_evaluate,
    dispatch_members,
};
use norn_cell::analytics::{EngineCapabilities, TenantId};
#[cfg(all(feature = "duckdb", not(target_arch = "wasm32")))]
use serde::Serialize;

/// The Durable Object transport: a call ends the request; the Worker performs
/// it and re-enters with the answer. OLAP-CRED-1: each call it hands the
/// Worker carries a fresh credential for this cell's own tenant, the only
/// bearer the engine accepts.
pub struct Exchange {
    issuer: Issuer,
    tenant: TenantId,
    capabilities: EngineCapabilities,
    answer: Mutex<Option<EngineAnswer>>,
    pending: Mutex<Option<EngineCall>>,
}

impl Exchange {
    /// The link of the cell whose engine database is `tenant`, minting with `issuer`,
    /// targeting a sidecar qualified for `capabilities`.
    #[must_use]
    pub const fn new(issuer: Issuer, tenant: TenantId, capabilities: EngineCapabilities) -> Self {
        Self {
            issuer,
            tenant,
            capabilities,
            answer: Mutex::new(None),
            pending: Mutex::new(None),
        }
    }

    /// Re-enter with the answer to the previous call.
    pub fn resume(&self, answer: EngineAnswer) {
        *self.answer.lock().unwrap_or_else(PoisonError::into_inner) = Some(answer);
        *self.pending.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }

    /// The call this request ended with, if any, with this cell's credential
    /// minted at `now_ms` (Unix milliseconds); taken once. OLAP-03: the
    /// credential is bound to the call's operation and to the digest of the
    /// exact body the host sends, computed here from that body (never taken
    /// from the call's own `digest`, which names a load without its URL).
    pub fn take_pending(&self, now_ms: u64) -> Option<EngineCall> {
        *self.answer.lock().unwrap_or_else(PoisonError::into_inner) = None;
        let mut call = self
            .pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()?;
        call.credential = Some(self.issuer.mint(
            &self.tenant,
            &call.op,
            &body_digest(call.body.as_bytes()),
            now_ms,
        ));
        Some(call)
    }
}

impl Transport for Exchange {
    fn capabilities(&self) -> EngineCapabilities {
        self.capabilities
    }
    fn resumed(&self) -> Option<EngineAnswer> {
        self.answer
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }
    fn call(&self, call: &EngineCall) -> Option<EngineAnswer> {
        *self.pending.lock().unwrap_or_else(PoisonError::into_inner) = Some(call.clone());
        None
    }
}

/// The engine linked into a native host: each call runs in place through
/// [`AnalyticalEngine`].
#[cfg(all(feature = "duckdb", not(target_arch = "wasm32")))]
pub struct InProcess<A: AnalyticalEngine = norn_cell::analytics::engine::Engine>(pub A);

#[cfg(all(feature = "duckdb", not(target_arch = "wasm32")))]
impl<A: AnalyticalEngine> InProcess<A> {
    fn run<T: serde::de::DeserializeOwned, R: Serialize>(
        body: &str,
        operation: impl FnOnce(&T) -> EngineOutcome<R>,
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
impl<A: AnalyticalEngine> Transport for InProcess<A> {
    fn capabilities(&self) -> EngineCapabilities {
        self.0.capabilities()
    }
    fn resumed(&self) -> Option<EngineAnswer> {
        None
    }
    fn call(&self, call: &EngineCall) -> Option<EngineAnswer> {
        let engine: &dyn AnalyticalEngine = &self.0;
        let (status, body) = match call.op.as_str() {
            "frontier" => Self::run(&call.body, |r| engine.mirror_frontier(r)),
            "sync" => Self::run(&call.body, |r| engine.mirror_sync(r)),
            "evaluate" => Self::run(&call.body, |r| {
                let mut resp = dispatch_evaluate(engine, r)?;
                resp.provenance = None;
                Ok(resp)
            }),
            "v2/evaluate" => Self::run(&call.body, |r| {
                let resp = dispatch_evaluate(engine, r)?;
                if resp.provenance.is_none() {
                    return Err(EngineRefusal {
                        code: STALE_INPUT_CODE.to_owned(),
                        detail: "v2 evaluate response omitted provenance".to_owned(),
                    });
                }
                Ok(resp)
            }),
            "members" => Self::run(&call.body, |r| dispatch_members(engine, r)),
            "profile" => Self::run(&call.body, |r| {
                let mut resp = engine.profile_objects(r)?;
                resp.provenance = None;
                Ok(resp)
            }),
            "v2/profile" => Self::run(&call.body, |r| {
                let resp = engine.profile_objects(r)?;
                if resp.provenance.is_none() {
                    return Err(EngineRefusal {
                        code: STALE_INPUT_CODE.to_owned(),
                        detail: "v2 profile response omitted provenance".to_owned(),
                    });
                }
                Ok(resp)
            }),
            "page" => Self::run(&call.body, |r| engine.query_page(r)),
            "lineage" => Self::run(&call.body, |r| engine.query_lineage(r)),
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
        "analytics.frontier_moved" | "analytics.frontier_behind" | "core.compute.stale_input" => {
            409
        }
        "analytics.engine" => 500,
        code if code.starts_with("core.compute.") || code.starts_with("bulk.") => 422,
        _ => 400,
    }
}

/// The cell's completed uploads, for loads (#921).
impl norn_api::analytics_routes::Uploads for crate::connect::Connect {
    fn record(&self, id: &str) -> Option<norn_api::analytics_routes::UploadRecord> {
        self.upload_record(id)
            .map(|record| norn_api::analytics_routes::UploadRecord {
                uri: record.uri,
                sha256: record.sha256,
                content_type: record.content_type,
                file_name: record.file_name,
            })
    }
    fn issued(&self, id: &str, scope_key: &str, uri: &str) -> bool {
        crate::connect::Connect::issued_upload(self, id, scope_key, uri)
    }
    fn presigned_get(&self, uri: &str, seconds: u64, now_ms: u64) -> Option<String> {
        self.signed_get(uri, u32::try_from(seconds).ok()?, now_ms)
    }
    fn presigned_put(&self, key: &str, seconds: u64, now_ms: u64) -> Option<(String, String)> {
        self.signed_put(key, u32::try_from(seconds).ok()?, now_ms)
    }
}
