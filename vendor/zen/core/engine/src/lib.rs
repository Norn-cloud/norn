//! # ZEN Engine
//!
//! ZEN Engine is business-friendly Open-Source Business Rules Engine (BRE) which executes decision
//! models according to the GoRules JSON Decision Model (JDM) standard. It's written in Rust and
//! provided native bindings for NodeJS and Python upstream.
//!
//! # Usage
//!
//! To execute a simple decision using a Noop (default) loader you can use the code below.
//!
//! ```no_run
//! use serde_json::json;
//! use std::sync::Arc;
//! use zen_engine::model::DecisionContent;
//! use zen_engine::DecisionEngine;
//!
//! async fn evaluate(jdm_graph: &str) {
//!     let decision_content: DecisionContent = serde_json::from_str(jdm_graph).unwrap();
//!     let engine = DecisionEngine::default();
//!     let decision = engine.create_decision(Arc::new(decision_content)).unwrap();
//!
//!     let result = decision.evaluate(json!({ "input": 12 }).into()).await;
//! }
//! ```
//!
//! # Loaders
//!
//! Sub-decisions are resolved through a [`loader::DecisionLoader`]: [`loader::MemoryLoader`]
//! (a key-value store) or [`loader::NoopLoader`] (the default, which loads nothing).
//!
//! Norn (#3404): this is the vendored, stripped engine. Function (JavaScript) and custom
//! nodes, the HTTP module, policies, workspace analysis, node-level JSON Schema
//! validation, filesystem/closure/zip loaders and the language bindings were removed.

#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used)]
#![allow(clippy::module_inception)]
#![expect(
    clippy::type_complexity,
    reason = "Norn #3404: upstream layout kept so gorules/zen fixes still cherry-pick (vendor/zen)"
)]

mod config;
mod decision;
mod decision_graph;
mod engine;
pub mod error;
pub mod loader;
pub mod model;
pub mod nodes;
mod time;

pub const ENGINE_VERSION: &str = env!("CARGO_PKG_VERSION");

pub use config::ZEN_CONFIG;
pub use decision::Decision;
pub use decision_graph::{
    DecisionGraphResponse, DecisionGraphTrace, DecisionGraphValidationError, EvaluationTrace,
};
pub use engine::{
    DecisionEngine, EvaluationOptions, EvaluationSerializedOptions, EvaluationTraceKind,
};
pub use error::{
    ContentKindError, EvaluationError, SchemaCompileError, StrictErrorSite, StrictEvaluationError,
};
/// Norn `metering`: operation budget used by `Decision::evaluate_metered`.
pub use zen_expression::meter;
pub use zen_expression::Variable;
