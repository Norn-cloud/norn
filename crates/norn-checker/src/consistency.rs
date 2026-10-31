//! Consistency engine (norn#63 lane D, norn-platform#3662).
//!
//! Definitions and mappings lower to a [`Theory`] of closed-world
//! dependencies; the bounded [`chase`] finds contradictions as concrete
//! conflicts. The checks are separate obligations (norn#62), each with its
//! own diagnostic:
//!
//! - **non-vacuity** ([`check_non_vacuity`]): each relation of a theory can
//!   hold a tuple; otherwise the failing equation, a minimal rule set and the
//!   frozen tuple that triggers them;
//! - **schema compatibility** ([`check_schema_compatibility`]): every source
//!   relation can be exchanged, and which target constraints the source does
//!   not guarantee, each with a witness source instance;
//! - **instance validation** ([`check_instance`]): sample or real data
//!   against both systems' constraints;
//! - **mapping totality** ([`check_mapping_totality`]): source relations and
//!   columns that never reach the target.
//!
//! Behavioural equivalence is a separate obligation, not checked here.

mod chase;
mod exchange;
mod theory;

pub use chase::{
    ChaseBudget, ChaseError, ChaseOutcome, Conflict, Fact, Value, chase, first_violation,
};
pub use exchange::{Exchange, check_instance, check_mapping_totality, check_schema_compatibility};
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
    /// No tuple of a source relation can be exchanged into the target.
    pub const INCOMPATIBLE: &str = "consistency.schema.incompatible";
    /// The source and mapping permit data that breaks a target constraint.
    pub const UNPRESERVED: &str = "consistency.schema.unpreserved_constraint";
    /// Sample or real data breaks a source or target constraint.
    pub const INSTANCE_VIOLATION: &str = "consistency.instance.violation";
    /// A source relation or column never reaches the target.
    pub const NON_TOTAL: &str = "consistency.mapping.non_total";
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
                let mut refused = refusal(theory, &error);
                refused.message = format!(
                    "{relation}: {}; relations after it were not checked",
                    refused.message
                );
                findings.push(refused);
                return findings;
            }
        }
    }
    findings
}

fn vacuous(theory: &Theory, relation: &str, conflict: &Conflict) -> Diagnostic {
    finding(
        codes::VACUOUS,
        DiagnosticSeverity::Error,
        format!(
            "{relation} can never hold a tuple: {}",
            conflict_message(conflict)
        ),
        origin_of(theory, &conflict.egd),
    )
}

/// `from <witness> the rules [<rules>] force <left> = <right> (EGD <id> on <facts>)`.
fn conflict_message(conflict: &Conflict) -> String {
    let render = |facts: &[Fact]| {
        facts
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    };
    format!(
        "from {} the rules [{}] force {} = {} (EGD {} on {})",
        render(&conflict.witness),
        conflict.rules.join(", "),
        conflict.left,
        conflict.right,
        conflict.egd,
        render(&conflict.matched),
    )
}

fn refusal(theory: &Theory, error: &ChaseError) -> Diagnostic {
    match error {
        ChaseError::Theory(error) => finding(
            codes::MALFORMED,
            DiagnosticSeverity::Error,
            format!("the lowered theory is malformed: {error:?}"),
            None,
        ),
        ChaseError::NotWeaklyAcyclic { rules } => finding(
            codes::NOT_WEAKLY_ACYCLIC,
            DiagnosticSeverity::Error,
            format!(
                "TGDs [{}] create existential values on a cycle, so the chase may not terminate",
                rules.join(", ")
            ),
            rules.first().and_then(|rule| origin_of(theory, rule)),
        ),
        ChaseError::BudgetExceeded { resource, limit } => finding(
            codes::CHASE_REFUSED,
            DiagnosticSeverity::Error,
            format!("the chase exceeded its {resource} budget of {limit}; consistency is unknown"),
            None,
        ),
        ChaseError::Internal(detail) => finding(
            codes::CHASE_REFUSED,
            DiagnosticSeverity::Error,
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

fn finding(
    code: &str,
    severity: DiagnosticSeverity,
    message: String,
    origin: Option<&RuleOrigin>,
) -> Diagnostic {
    Diagnostic {
        code: DiagnosticCode::new(code),
        severity,
        message,
        span: origin.map(|origin| Span::pointer(origin.pointer.clone())),
        artifact_id: origin.and_then(|origin| origin.artifact_id.clone()),
        remediation: Some(DiagnosticRemediation::FixDefinition),
    }
}
