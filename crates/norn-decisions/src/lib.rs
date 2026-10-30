//! `norn-decisions`: the `zen-engine` JDM evaluator behind a Norn-owned,
//! synchronous `decide(slice, event) -> { facts, commands, timers }` API
//! (T3-DEC-2, #2670; ruling 2026-09-25 §3).
//!
//! Ring 0 — pure, portable semantic core (see `AGENTS.md`). This crate is the
//! only Norn crate that depends on `zen-engine`, the vendored engine
//! (`vendor/zen`, imported from `Norn-cloud/zen` at [`ENGINE_TAG`]) whose only
//! behaviour is the Norn profile: no JavaScript, HTTP, JSON Schema, or OS
//! randomness, the `regex-lite` backend, strict errors, ordered maps and the
//! operation meter. No `zen-*` type crosses this boundary; the kernel
//! consumes only [`AdmittedDecision`], [`ValidatedSlice`], [`TypedEvent`],
//! `norn_expr::EvaluationBudget`, [`Evaluated`] (the canonical
//! `norn_ir::DecisionOutput` plus the units used), and [`DecisionError`].
//!
//! - [`AdmittedDecision::admit`] loads a `RuleIR` v2 `decide` graph and its
//!   exact sub-decision closure, re-checking the Norn profile (fail closed).
//! - [`AdmittedDecision::validate_slice`] / [`AdmittedDecision::type_event`]
//!   check the inputs against the decide input schema.
//! - [`evaluate`] runs the graph once, synchronously, under an operation
//!   budget, and validates the output against the declared output schema
//!   (fact-change, command, and timer item records, including the string
//!   enums that declare write sets and lifecycle transitions).
//!
//! Inside the evaluator, values are `norn_expr::Value`: numbers are exact
//! decimals and never pass through `f64` or serde ([`value_from_canonical`]
//! and [`value_to_canonical`] bridge the canonical JSON wire;
//! [`value_from_runtime`] and [`value_to_runtime`] bridge the exact runtime
//! values). The canonical `norn_ir::DecisionOutput` carries exact runtime
//! values (#2770): an output decimal is never narrowed or rounded. Numeric
//! output defaults are taken from the rule's source text
//! ([`AdmittedDecision::admit_with_defaults`]).
//! Evaluation reads no clock for its result, does no IO, and builds for
//! `wasm32-unknown-unknown`.
//!
//! Still to come: per-node expression type checking against the declared
//! slice at admission (T3-DEC-3 #2671, T1-EXPR-5 #2523), which is also what
//! rules out ambient-time builtins; the T1-EXPR-6 profile corpus (#2766); the
//! DMN TCK subset (T6-DMN-1 #2688). The kernel commits the output
//! (`norn_kernel::commit_decision`, #2535).
//!
//! ```
//! # use std::collections::BTreeMap;
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! use norn_decisions::{AdmittedDecision, DecisionBinding, evaluate};
//! use norn_expr::{EvaluationBudget, Value};
//!
//! # let fixture: serde_json::Value =
//! #     serde_json::from_str(include_str!("../tests/zen_corpus/expression_node.json"))?;
//! # let rule: norn_ir::RuleRoot = serde_json::from_value(fixture["rule"].clone())?;
//! let binding = DecisionBinding {
//!     profile_id: "norn-zen/6".to_owned(),
//!     environment_hash: "sha256:…".to_owned(),
//! };
//! let decision = AdmittedDecision::admit(&rule, &BTreeMap::new(), binding)?;
//! let slice = decision.validate_slice(Value::object([("amount", Value::from(7))]))?;
//! let event = decision.type_event(Value::object([("kind", Value::from("submitted"))]))?;
//! let output = evaluate(&decision, &slice, &event, EvaluationBudget::default())?;
//! assert_eq!(output.output.commands.len(), 1);
//! # Ok(())
//! # }
//! ```

#![forbid(unsafe_code)]

mod admit;
mod convert;
mod error;
mod evaluate;
mod schema;

pub use admit::{
    AdmittedDecision, DecisionBinding, ENGINE_TAG, MAX_SUB_DECISION_CLOSURE, MAX_SUB_DECISION_DEPTH,
};
pub use convert::{value_from_canonical, value_from_runtime, value_to_canonical, value_to_runtime};
pub use error::{AdmissionError, DecisionError, StrictSite};
pub use evaluate::{
    Evaluated, MatchedRow, TypedEvent, ValidatedSlice, evaluate, evaluate_explained,
    evaluate_explained_with_fact_check, lower_fact_record,
};
