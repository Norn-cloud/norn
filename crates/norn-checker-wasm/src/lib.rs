//! The checker ABI (T1-CHK-4, #2527): the one JSON-in/JSON-out surface the
//! TypeScript extractor, jdm-editor, the playground, and SDKs call instead of
//! linking checker internals.
//!
//! Every function takes JSON text and returns JSON text, so no `serde`,
//! `norn-ir`, or `zen-*` type crosses the boundary. The functions here are
//! plain Rust; on `wasm32` the `wasm` module exports each one to
//! JavaScript under its camel-case name (`checkArtifactSet`,
//! `lifecycleDiagrams`, `checkExpression`, `checkAgent`, `version`, and so
//! on). A native C ABI over the
//! same functions is follow-up work (see the crate README).
//!
//! Output is byte-identical natively and on `wasm32`: diagnostics are
//! [`norn_checker::diagnostics_to_json`] (`norn-checker/diagnostics@1`), the
//! receipt is the checker's Norn1 canonical receipt, and the whole document
//! is compact `serde_json` text (sorted keys). The CI job `checker-wasm`
//! asserts the equality on the fixture cases in `tests/parity/cases.json`.
//!
//! Invalid *input* (text that is not JSON, a wrong shape, an unknown expected
//! type) is an [`AbiError`] (a thrown `Error` in JavaScript). Invalid
//! *artifacts* are findings in the output, never an error.

//!
//! Without its default `checker` feature the crate is the ZEN engine alone: only
//! [`evaluate_expression_json`] (`evaluateExpression`), the same function the full package
//! exports, over `norn-expr` (D105: one evaluator). Page machines load that build
//! (`scripts/build-pkg.sh` with `NORN_WASM_ENGINE=1`); `tests/parity/engine-parity.mjs` holds the
//! two builds to the same answers.

#[cfg(feature = "checker")]
mod checker;
#[cfg(feature = "checker")]
pub use checker::*;

mod evaluate;
pub use evaluate::{EVALUATION_FORMAT, evaluate_expression_json};

#[cfg(target_arch = "wasm32")]
pub mod wasm;
