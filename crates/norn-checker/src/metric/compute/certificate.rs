//! COMPUTE-DETERMINISTIC-1: DBSP algebra, conditional on faithful operator lowering.
//! This checks equations, not execution, authorization, or arbitrary fold laws.
use super::QuestionPlan;

/// Version of the interpreted algebraic rules.
pub const SEMANTICS: &str = "norn.ivm-algebra/1";
/// Runtime correspondence cannot be inferred from the bilinear equation.
pub const JOIN_RUNTIME: Correspondence =
    Correspondence::Unknown("QuestionPlan does not admit joins");

/// Runtime evidence is separate from equation checking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Correspondence {
    /// Generated native harness comparisons, not universal proof.
    Sampled,
    /// Missing implementation correspondence with its explicit reason.
    Unknown(&'static str),
}

/// The extracted, proved per-node decision (#718 U5).
pub mod decision;

pub use decision::{Homomorphism, Operator};

/// Evidence about equations only. No variant grants a runtime proof.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Algebraic {
    /// Every node matches a closed algebraic rule (proved-algebraic).
    ProvedAlgebraic,
    /// A bilinear expansion has an incorrect symbolic coefficient.
    Refuted {
        /// Operator index.
        operator: usize,
        /// Term index in the declared coefficient order.
        term: usize,
    },
    /// Unsupported laws, malformed dependencies, or resource limit.
    Unknown(&'static str),
}

/// Check a whole acyclic composition, retaining one result per generated node.
/// Signed changes include deletion and replacement; monoids alone do not suffice.
#[must_use]
pub fn check(operators: &[Operator], signed_changes: bool) -> Vec<Algebraic> {
    decision::check(operators, signed_changes)
        .into_iter()
        .enumerate()
        .map(|(operator, verdict)| match verdict {
            decision::Verdict::Proved => Algebraic::ProvedAlgebraic,
            decision::Verdict::Refuted(term) => Algebraic::Refuted { operator, term },
            decision::Verdict::OverBudget => Algebraic::Unknown("operator budget exceeded"),
            decision::Verdict::UndeclaredFold => Algebraic::Unknown("undeclared fold homomorphism"),
            decision::Verdict::MonoidUnderDeletions => {
                Algebraic::Unknown("monoid law does not support deletions")
            }
            decision::Verdict::Unsupported => Algebraic::Unknown("unsupported operator"),
            decision::Verdict::InvalidDependency => {
                Algebraic::Unknown("uncertified or invalid dependency")
            }
        })
        .collect()
}

/// Plan-bound certificate of maintained sufficient statistics, not ratio finalization.
/// The borrowed plan prevents substituting another admitted declaration in this result.
pub struct QuestionCertificate<'a> {
    /// Exact admitted source of the reconstructed operators.
    pub plan: &'a QuestionPlan,
    /// One result per operator; this never suppresses unknown obligations.
    pub algebraic: Vec<Algebraic>,
    /// Rounded ratios are recomputed from maintained totals, not homomorphisms.
    pub finalizer: &'static str,
    /// Evidence ceiling for the unchanged native correspondence harness.
    pub runtime: Correspondence,
}

/// Reconstruct only the operators admitted today; never accept a caller's plan.
#[must_use]
pub fn question(plan: &QuestionPlan) -> QuestionCertificate<'_> {
    let mut operators = vec![Operator::Input];
    for _ in plan.sums() {
        let selected = operators.len();
        operators.push(Operator::Homomorphism {
            input: 0,
            law: Some(Homomorphism::Select),
        });
        let projected = operators.len();
        operators.push(Operator::Homomorphism {
            input: selected,
            law: Some(Homomorphism::Project),
        });
        operators.push(Operator::Homomorphism {
            input: projected,
            law: Some(Homomorphism::GroupSum),
        });
    }
    QuestionCertificate {
        plan,
        algebraic: check(&operators, true),
        finalizer: "shared finalizer correspondence is sampled, including rounded ratios",
        runtime: Correspondence::Sampled,
    }
}

#[cfg(test)]
#[allow(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "exhaustive fixed join coefficient checks"
)]
mod tests {
    use super::*;

    #[test]
    fn each_omitted_join_term_is_refuted_at_its_index() {
        for term in 0..3 {
            let mut coefficients = [1; 3];
            coefficients[term] = 0;
            assert_eq!(
                check(
                    &[
                        Operator::Input,
                        Operator::Join {
                            left: 0,
                            right: 0,
                            coefficients
                        }
                    ],
                    true
                )[1],
                Algebraic::Refuted { operator: 1, term }
            );
        }
        assert!(
            check(
                &[
                    Operator::Input,
                    Operator::Join {
                        left: 0,
                        right: 0,
                        coefficients: [1; 3]
                    }
                ],
                true
            )
            .iter()
            .all(|r| *r == Algebraic::ProvedAlgebraic)
        );
    }

    #[test]
    fn every_join_coefficient_matches_independent_basis() {
        let samples = [(0i32, 1i32, 1i32, 0i32), (1, 0, 0, 1), (0, 0, 1, 1)];
        let mut proved = Vec::new();
        for first in -8i8..=8 {
            for second in -8i8..=8 {
                for cross in -8i8..=8 {
                    let coefficients = [first, second, cross];
                    let [first, second, cross] = coefficients.map(i32::from);
                    let correct =
                        samples
                            .into_iter()
                            .all(|(left, right, left_delta, right_delta)| {
                                first * left_delta * right
                                    + second * left * right_delta
                                    + cross * left_delta * right_delta
                                    == (left + left_delta) * (right + right_delta) - left * right
                            });
                    let result = check(
                        &[
                            Operator::Input,
                            Operator::Join {
                                left: 0,
                                right: 0,
                                coefficients,
                            },
                        ],
                        true,
                    );
                    let certified = result.get(1) == Some(&Algebraic::ProvedAlgebraic);
                    assert_eq!(certified, correct, "coefficients {coefficients:?}");
                    if certified {
                        proved.push(coefficients);
                    }
                }
            }
        }
        assert_eq!(proved, [[1; 3]]);
    }

    #[test]
    fn undeclared_folds_and_invalid_compositions_stay_unknown() {
        assert_eq!(
            JOIN_RUNTIME,
            Correspondence::Unknown("QuestionPlan does not admit joins")
        );
        for law in [None, Some(Homomorphism::MonoidSum)] {
            assert!(matches!(
                check(
                    &[Operator::Input, Operator::Homomorphism { input: 0, law }],
                    true
                )[1],
                Algebraic::Unknown(_)
            ));
        }
        assert_eq!(
            check(
                &[
                    Operator::Input,
                    Operator::Homomorphism {
                        input: 0,
                        law: Some(Homomorphism::MonoidSum)
                    }
                ],
                false
            )[1],
            Algebraic::ProvedAlgebraic
        );
        assert!(matches!(
            check(
                &[Operator::Homomorphism {
                    input: 0,
                    law: Some(Homomorphism::GroupSum)
                }],
                true
            )[0],
            Algebraic::Unknown(_)
        ));
        assert_eq!(
            check(
                &[
                    Operator::Unknown,
                    Operator::Join {
                        left: 0,
                        right: 0,
                        coefficients: [1; 3]
                    }
                ],
                true
            )[1],
            Algebraic::Unknown("uncertified or invalid dependency")
        );
    }
}
