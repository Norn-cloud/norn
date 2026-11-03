//! Workers adapter; the protocol and service remain native-testable Rust.
#![cfg(target_arch = "wasm32")]
use norn_api::{
    mcp_host::{Host, Request},
    scope::CellScope,
};
use wasm_bindgen::prelude::*;

/// One configured cell's ephemeral protocol sessions.
#[wasm_bindgen]
pub struct McpHost(Host);
#[wasm_bindgen]
impl McpHost {
    /// Trusted deployment binding.
    #[wasm_bindgen(constructor)]
    pub fn new(org: String, environment: String, origin: String) -> Self {
        Self(Host::new(
            CellScope {
                org_id: org,
                environment_id: environment,
            },
            origin,
        ))
    }
    /// Preserve the JSON-RPC text across the JS boundary.
    /// # Errors
    /// Invalid transport envelope or serialization failure.
    pub fn fetch(&mut self, source: &str, nonce: &str, now: u64) -> Result<String, JsValue> {
        let request: Request =
            serde_json::from_str(source).map_err(|e| JsValue::from_str(&e.to_string()))?;
        serde_json::to_string(&self.0.handle(request, nonce, now))
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }
}
