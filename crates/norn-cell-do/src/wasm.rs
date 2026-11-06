//! The Durable Object binding: one [`NornCell`] per DO instance.
//!
//! The DO constructor passes its root `ctx.storage`; the cell keeps it for the
//! object's lifetime. The DO's input/output gates and single-threaded event
//! loop make this cell the only writer; `transactionSync` is the commit.

use norn_api::host::DevHost;
use norn_api::question_routes::QuestionRuntime;
use norn_api::scope::CellScope;
use norn_cell::workers::WorkersEngine;
use wasm_bindgen::prelude::*;

use crate::analytics::{self, DemoQuestions};
use crate::policy::{DEMO_GRANTS, ReadPolicy};
use crate::principal::PrincipalResolver;
use crate::{seed, server};

/// One opened tenant cell.
#[wasm_bindgen]
pub struct NornCell {
    host: DevHost<WorkersEngine>,
    /// The cell's admitted questions, when it publishes any.
    questions: Option<DemoQuestions>,
    principals: Box<dyn PrincipalResolver>,
}

fn rejected(error: impl std::fmt::Display) -> JsValue {
    JsValue::from_str(&error.to_string())
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
        let engine = WorkersEngine::new(storage);
        let (store, questions) = if scope == seed::demo_scope() {
            (seed::provision(engine, &scope).map_err(rejected)?, None)
        } else if scope == analytics::analytics_scope() {
            let opened = analytics::provision(engine, &scope).map_err(rejected)?;
            let questions = DemoQuestions::new(opened.plan, scope.clone());
            (opened.store, Some(questions))
        } else {
            return Err(JsValue::from_str("no cell is published for this scope"));
        };
        // Data reads are decided by the kernel PDP over this cell's own model.
        let policy = ReadPolicy::compile(store.model().object_types(), &scope, &DEMO_GRANTS)
            .map_err(rejected)?;
        // Fixture principals exist only in local smoke builds; a deployable
        // build verifies nobody until host tokens are configured (#817).
        #[cfg(feature = "fixtures")]
        let principals: Box<dyn PrincipalResolver> =
            Box::new(crate::principal::FixturePrincipals::new(
                crate::principal::DEMO_FIXTURES,
                policy.caller(),
            ));
        #[cfg(not(feature = "fixtures"))]
        let principals: Box<dyn PrincipalResolver> =
            Box::new(crate::principal::NoPrincipals(policy.caller()));
        let host = DevHost::from_store(store, scope, |command| command.payload.clone())
            .map_err(|refusal| JsValue::from_str(&refusal.code))?
            .with_authorizer(policy);
        Ok(Self {
            host,
            questions,
            principals,
        })
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
        let questions = self
            .questions
            .as_ref()
            .map(|questions| -> &dyn QuestionRuntime<WorkersEngine> { questions });
        let request = server::Request {
            method,
            path,
            query,
            authorization: authorization.as_deref(),
            now: 0,
        };
        let response =
            server::handle_with(&self.host, self.principals.as_ref(), questions, &request);
        Reply {
            status: response.status,
            body: response.body,
        }
    }
}
