//! Consistency engine (norn#63 lane D, norn-platform#3662).
//!
//! Definitions and mappings lower to a [`Theory`] of closed-world
//! dependencies; the bounded [`chase`] finds contradictions as concrete
//! conflicts. The checks are separate obligations (norn#62): this module
//! starts with **non-vacuity**, a witness that each relation of the theory
//! can hold a tuple. A relation that cannot is reported with the failing
//! equation, a minimal set of conflicting rules and the frozen tuple that
//! triggers them.

mod chase;
mod divergence;
mod theory;

pub use chase::{ChaseBudget, ChaseError, ChaseOutcome, Conflict, Fact, Value, chase};
pub use divergence::{CanonKey, Divergence, MappedObservation, WitnessIdentity, find_divergences};
pub use theory::{Atom, Dependency, Egd, RuleOrigin, Term, Tgd, Theory, TheoryError};

use crate::{Diagnostic, DiagnosticCode, DiagnosticRemediation, DiagnosticSeverity, Span};

/// Stable consistency diagnostic codes.
pub mod codes {
    /// The lowered theory is malformed (duplicate rule, arity, unbound variable).
    pub const MALFORMED: &str = "consistency.theory.malformed";
    /// TGDs create existential values on a cycle; the chase may not terminate.
    pub const NOT_WEAKLY_ACYCLIC: &str = "consistency.theory.not_weakly_acyclic";
    /// A relation can never hold a tuple under the theory.
    pub const VACUOUS: &str = "consistency.theory.vacuous";
    /// The chase exhausted its budget; nothing is claimed.
    pub const CHASE_REFUSED: &str = "consistency.chase.refused";
}

/// The non-vacuity witness for `relation`: chase one tuple of fresh labelled
/// nulls. A [`ChaseOutcome::Model`] is a finite instance in which `relation`
/// is non-empty and every dependency holds.
///
/// # Errors
/// See [`chase`], or [`TheoryError::UnknownRelation`].
pub fn non_vacuity_witness(
    theory: &Theory,
    relation: &str,
    budget: ChaseBudget,
) -> Result<ChaseOutcome, ChaseError> {
    let signature = theory.signature().map_err(ChaseError::Theory)?;
    let Some(&arity) = signature.get(relation) else {
        return Err(ChaseError::Theory(TheoryError::UnknownRelation(
            relation.to_owned(),
        )));
    };
    let frozen = Fact {
        relation: relation.to_owned(),
        values: (0..arity).map(Value::Null).collect(),
    };
    chase(theory, &[frozen], budget)
}

/// Report every relation of `theory` that has no instance.
#[must_use]
pub fn check_non_vacuity(theory: &Theory, budget: ChaseBudget) -> Vec<Diagnostic> {
    let signature = match theory.signature() {
        Ok(signature) => signature,
        Err(error) => return vec![refusal(theory, &ChaseError::Theory(error))],
    };
    let mut findings = Vec::new();
    for relation in signature.keys() {
        match non_vacuity_witness(theory, relation, budget) {
            Ok(ChaseOutcome::Model(_)) => {}
            Ok(ChaseOutcome::Conflict(conflict)) => {
                findings.push(vacuous(theory, relation, &conflict));
            }
            // A refusal stops the run but keeps the contradictions already found.
            Err(error) => {
                findings.push(refusal(theory, &error));
                return findings;
            }
        }
    }
    findings
}

fn vacuous(theory: &Theory, relation: &str, conflict: &Conflict) -> Diagnostic {
    let witness: Vec<String> = conflict.witness.iter().map(ToString::to_string).collect();
    let matched: Vec<String> = conflict.matched.iter().map(ToString::to_string).collect();
    finding(
        codes::VACUOUS,
        format!(
            "{relation} can never hold a tuple: from {} the rules [{}] force {} = {} \
             (EGD {} on {})",
            witness.join(", "),
            conflict.rules.join(", "),
            conflict.left,
            conflict.right,
            conflict.egd,
            matched.join(", "),
        ),
        origin_of(theory, &conflict.egd),
    )
}

fn refusal(theory: &Theory, error: &ChaseError) -> Diagnostic {
    match error {
        ChaseError::Theory(error) => finding(
            codes::MALFORMED,
            format!("the lowered theory is malformed: {error:?}"),
            None,
        ),
        ChaseError::NotWeaklyAcyclic { rules } => finding(
            codes::NOT_WEAKLY_ACYCLIC,
            format!(
                "TGDs [{}] create existential values on a cycle, so the chase may not terminate",
                rules.join(", ")
            ),
            rules.first().and_then(|rule| origin_of(theory, rule)),
        ),
        ChaseError::BudgetExceeded { resource, limit } => finding(
            codes::CHASE_REFUSED,
            format!("the chase exceeded its {resource} budget of {limit}; consistency is unknown"),
            None,
        ),
        ChaseError::Internal(detail) => finding(
            codes::CHASE_REFUSED,
            format!("the chase was refused: {detail}; consistency is unknown"),
            None,
        ),
    }
}

fn origin_of<'a>(theory: &'a Theory, rule: &str) -> Option<&'a RuleOrigin> {
    theory
        .dependencies
        .iter()
        .find(|dependency| dependency.id() == rule)
        .map(Dependency::origin)
}

fn finding(code: &str, message: String, origin: Option<&RuleOrigin>) -> Diagnostic {
    Diagnostic {
        code: DiagnosticCode::new(code),
        severity: DiagnosticSeverity::Error,
        message,
        span: origin.map(|origin| Span::pointer(origin.pointer.clone())),
        artifact_id: origin.and_then(|origin| origin.artifact_id.clone()),
        remediation: Some(DiagnosticRemediation::FixDefinition),
    }
}
