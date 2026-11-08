//! The Durable Object binding: one [`NornCell`] per DO instance.
//!
//! The DO constructor passes its root `ctx.storage`; the cell keeps it for the
//! object's lifetime. The DO's input/output gates and single-threaded event
//! loop make this cell the only writer; `transactionSync` is the commit.

use norn_api::host::DevHost;
use norn_api::scope::CellScope;
use norn_cell::workers::WorkersEngine;
use wasm_bindgen::prelude::*;

use crate::principal::{DEMO_PRINCIPALS, FixturePolicy};
use crate::{seed, server};

/// One opened tenant cell.
#[wasm_bindgen]
pub struct NornCell {
    host: DevHost<WorkersEngine>,
}

/// A finite HTTP reply.
#[wasm_bindgen(getter_with_clone)]
pub struct Reply {
    /// HTTP status.
    pub status: u16,
    /// JSON body.
    pub body: String,
}

#[wasm_bindgen]
impl NornCell {
    /// Open (provisioning on first use) the cell for the DO's own scope.
    ///
    /// # Errors
    /// The scope has no published cell, or provisioning failed.
    #[wasm_bindgen(constructor)]
    pub fn new(storage: JsValue, org_id: String, environment_id: String) -> Result<Self, JsValue> {
        let scope = CellScope {
            org_id,
            environment_id,
        };
        if scope != seed::demo_scope() {
            return Err(JsValue::from_str("no cell is published for this scope"));
        }
        let store = seed::provision(WorkersEngine::new(storage), &scope)
            .map_err(|error| JsValue::from_str(&error.to_string()))?;
        let host = DevHost::from_store(store, scope, |command| command.payload.clone())
            .map_err(|refusal| JsValue::from_str(&refusal.code))?
            .with_authorizer(FixturePolicy(DEMO_PRINCIPALS));
        Ok(Self { host })
    }

    /// Serve one request; every outcome is a status and a JSON document.
    #[must_use]
    #[allow(
        clippy::needless_pass_by_value,
        reason = "wasm-bindgen passes an optional string by value"
    )]
    pub fn fetch(
        &self,
        method: &str,
        path: &str,
        query: &str,
        authorization: Option<String>,
    ) -> Reply {
        let response = server::handle(
            &self.host,
            &DEMO_PRINCIPALS,
            method,
            path,
            query,
            authorization.as_deref(),
        );
        Reply {
            status: response.status,
            body: response.body,
        }
    }
}
