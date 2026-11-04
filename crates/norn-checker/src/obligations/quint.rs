//! PROOF-QUINT-1: syntax emission from the shared finite relation.
/// Native trusted prepare/bind controller; solver processes remain outside the checker.
#[cfg(not(target_arch = "wasm32"))]
pub mod driver;
pub use super::finite::Bounds;
use super::{ApprovedContract, Challenge, ChallengeError, DefinitionSource};
use norn_ir::verification::finite::CreateRelation;
use serde_json::Value;
use std::collections::BTreeSet;

/// Complete generated inventory, including unsupported goals.
#[derive(Debug)]
pub struct Batch {
    /// Fresh checker-owned challenge.
    pub challenge: Challenge,
    /// Exact bounds and declaration interpretation for evidence binding.
    pub bounds: Value,
    /// One result per generated obligation, in challenge order.
    pub goals: Vec<Result<String, String>>,
}
impl ApprovedContract {
    /// PROOF-QUINT-1: emit only the freshly reconstructed finite relation.
    /// # Errors
    /// Returns the ordinary admission/challenge error, never a solver verdict.
    pub fn quint(
        &self,
        source: &DefinitionSource,
        registry: &BTreeSet<norn_ir::PropertyTypeName>,
        bounds: &Bounds,
    ) -> Result<Batch, ChallengeError> {
        let batch = self.finite(source, registry, bounds)?;
        Ok(Batch {
            challenge: batch.challenge,
            bounds: batch.bounds,
            goals: batch
                .goals
                .into_iter()
                .map(|goal| goal.map(|model| emit(&model)))
                .collect(),
        })
    }
}
fn emit(model: &CreateRelation) -> String {
    let variables: Vec<_> = model.state.iter().map(|field| &field.symbol).collect();
    let choices: Vec<_> = model
        .inputs
        .iter()
        .map(|input| {
            format!(
                "nondet {} = ({}).to({}).oneOf()",
                input.symbol, input.lower, input.upper
            )
        })
        .collect();
    let guards: Vec<_> = model
        .guards
        .iter()
        .map(norn_ir::ZenExpression::as_str)
        .collect();
    let condition = model.condition.as_str();
    let response = model.response.as_str();
    let initial = variables
        .iter()
        .map(|v| format!("{v}' = 0"))
        .collect::<Vec<_>>()
        .join(", ");
    let updates = model
        .updates
        .iter()
        .map(|(variable, expression)| format!("{variable}' = {}", expression.as_str()))
        .collect::<Vec<_>>()
        .join(", ");
    let frame = variables
        .iter()
        .map(|v| format!("{v}' = {v}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "module generated {{\n{}\nvar present: bool\naction init = all {{ present' = false, {initial} }}\naction write = {{\n{}\nall {{ not(present), {}, present' = true, {updates} }}\n}}\naction idle = all {{ present' = present, {frame} }}\naction step = any {{ write, idle }}\nval law = not(present) or not({condition}) or ({response})\nval unreachable = not(present and ({condition}) and ({response}))\n}}\n",
        variables
            .iter()
            .map(|v| format!("var {v}: int"))
            .collect::<Vec<_>>()
            .join("\n"),
        choices.join("\n"),
        if guards.is_empty() {
            "true".into()
        } else {
            format!("({})", guards.join(") and ("))
        }
    )
}
