//! Closed-world finite predicate oracle; diagnostic analysis, never proof authority.
use super::diagnosis::{Oracle, Verdict};
use crate::{
    finite::{domain, expression},
    table_analysis::{MAX_CONTEXTS, MAX_DOMAIN_BYTES, MAX_EVALUATIONS, Unknown},
};
use norn_expr::{Value, schema::Schema};
use std::collections::BTreeSet;

/// Versioned fragment, independent of any future SMT backend.
pub const PROFILE: &str = "norn.finite-conjunction/1";

/// Complete finite truth table relative to a declared closed-world schema.
/// The fixed universe is an explicit assumption, not an inferred closed roster.
pub struct FiniteOracle {
    models: Vec<BTreeSet<usize>>,
    declarations: usize,
    /// Enumerated assignments, bounded by `MAX_CONTEXTS` (256).
    pub assignments: usize,
}

impl FiniteOracle {
    /// Enumerate admitted Boolean predicates with the same domain/evaluator as
    /// decision-table analysis. Limits: 256 contexts, 65536 encoded domain bytes,
    /// 8192 predicate evaluations and 1000 fuel per expression. Disclose these
    /// alongside the schema and closed-world assumption in every diagnosis.
    /// # Errors
    /// Unsupported domains, non-Boolean results or exhaustion return unknown.
    pub fn new(schema: &Schema, predicates: &[&str]) -> Result<Self, Unknown> {
        let inputs = domain(schema)?;
        if inputs.values.len().saturating_mul(predicates.len()) > MAX_EVALUATIONS {
            return Err(Unknown::Limit);
        }
        let mut models = Vec::new();
        for input in inputs.values {
            let runtime: norn_ir::RuntimeValue =
                serde_json::from_value(input).map_err(|e| Unknown::Expression(e.to_string()))?;
            let context =
                Value::try_from(&runtime).map_err(|e| Unknown::Expression(e.to_string()))?;
            let mut holds = BTreeSet::new();
            for (index, predicate) in predicates.iter().enumerate() {
                match expression(predicate, &context)? {
                    Value::Bool(true) => {
                        holds.insert(index);
                    }
                    Value::Bool(false) => {}
                    _ => return Err(Unknown::Expression("predicate is not boolean".into())),
                }
            }
            models.push(holds);
        }
        let assignments = models.len();
        Ok(Self {
            models,
            declarations: predicates.len(),
            assignments,
        })
    }

    /// Explicit enumeration and evaluator ceilings for diagnostic pins/messages.
    #[must_use]
    pub const fn bounds() -> [usize; 4] {
        [MAX_CONTEXTS, MAX_DOMAIN_BYTES, MAX_EVALUATIONS, 1000]
    }
}

impl Oracle for FiniteOracle {
    type Unknown = Unknown;
    fn check(&mut self, active: &BTreeSet<usize>) -> Verdict<Unknown> {
        if active.iter().any(|i| *i >= self.declarations) {
            return Verdict::Unknown(Unknown::Limit);
        }
        if self.models.iter().any(|model| active.is_subset(model)) {
            Verdict::Sat
        } else {
            Verdict::Unsat
        }
    }
}
