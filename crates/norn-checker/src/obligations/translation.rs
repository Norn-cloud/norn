//! TRANSLATION-PRESERVES-1: `Valid_S and Admit => Valid_T(f(x)) and O_T(f(x)) = O_S(x)`.
//!
//! A goal's status comes from two solver answers: one for a counterexample and
//! one for an admitted valid source. No backend lowers these goals yet.
use norn_ir::verification::translation::TranslationStatus;

/// What every proof here assumes about the runtime it models.
pub const ASSUMPTIONS: [&str; 3] = [
    "decimal values are exact and unbounded: no overflow or rounding",
    "text is an identity compared only by equality",
    "stored records satisfy their integrity invariants",
];

/// One solver answer; model text is opaque here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    /// Satisfiable, with the solver's model.
    Sat(String),
    /// Exactly `unsat`.
    Unsat,
    /// No usable answer.
    Unknown(String),
}

/// A proof needs both answers; a refutation needs only its own model.
#[must_use]
pub fn classify(preservation: Answer, non_vacuity: Answer) -> TranslationStatus {
    match (preservation, non_vacuity) {
        (Answer::Sat(counterexample), _) => TranslationStatus::Refuted { counterexample },
        (Answer::Unsat, Answer::Sat(witness)) => TranslationStatus::ProvedUnderAssumptions {
            assumptions: ASSUMPTIONS.map(str::to_owned).to_vec(),
            witness,
        },
        (Answer::Unsat, Answer::Unsat) => TranslationStatus::Unsupported {
            reason: "vacuous: no admitted valid source exists".into(),
        },
        (Answer::Unknown(reason), _) | (Answer::Unsat, Answer::Unknown(reason)) => {
            TranslationStatus::Unsupported { reason }
        }
    }
}
