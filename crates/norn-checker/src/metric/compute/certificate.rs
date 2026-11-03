//! COMPUTE-DETERMINISTIC-1: DBSP algebra, conditional on faithful operator lowering.
//! This checks equations, not execution, authorization, or arbitrary fold laws.
use super::QuestionPlan;

/// Version of the interpreted algebraic rules.
pub const SEMANTICS: &str = "norn.ivm-algebra/1";
/// Runtime correspondence cannot be inferred from the bilinear equation.
pub const JOIN_RUNTIME: &str = "QuestionPlan does not admit joins";

/// Closed homomorphisms; these are mathematical operations, not user assertions.
#[derive(Debug, Clone, Copy)]
pub enum Homomorphism {
    /// Fixed predicate on rows, extended by signed multiplicity.
    Select,
    /// Fixed row mapping, summing multiplicities on collisions.
    Project,
    /// Exact keyed sum, without rounding.
    GroupSum,
    /// Natural multiplicity sum; valid only for insert-only changes.
    MonoidSum,
}

/// One dependency-ordered operator. References must point strictly backwards.
#[derive(Debug, Clone)]
pub enum Operator {
    /// Signed multiset input (Z-set).
    Input,
    /// A declared closed homomorphism, including folds with these exact laws.
    Homomorphism {
        /// Prior operator index.
        input: usize,
        /// Absent declarations never inherit a law from sampled behavior.
        law: Option<Homomorphism>,
    },
    /// Bilinear equijoin, with coefficients for J(dL,R), J(L,dR), J(dL,dR).
    Join {
        /// Prior left operator.
        left: usize,
        /// Prior right operator.
        right: usize,
        /// Old snapshots on both sides; the cross term is mandatory.
        coefficients: [i8; 3],
    },
    /// Semantics outside the supported fragment.
    Unknown,
}

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
    if operators.len() > 256 {
        return vec![Algebraic::Unknown("operator budget exceeded"); operators.len()];
    }
    let mut results = Vec::with_capacity(operators.len());
    for (index, operator) in operators.iter().enumerate() {
        let dependency =
            |id: usize| id < index && results.get(id) == Some(&Algebraic::ProvedAlgebraic);
        let result = match operator {
            Operator::Input => Algebraic::ProvedAlgebraic,
            Operator::Homomorphism { input, law } if dependency(*input) => match law {
                None => Algebraic::Unknown("undeclared fold homomorphism"),
                Some(Homomorphism::MonoidSum) if signed_changes => {
                    Algebraic::Unknown("monoid law does not support deletions")
                }
                Some(_) => Algebraic::ProvedAlgebraic,
            },
            Operator::Join {
                left,
                right,
                coefficients,
            } if dependency(*left) && dependency(*right) => coefficients
                .iter()
                .position(|coefficient| *coefficient != 1)
                .map_or(Algebraic::ProvedAlgebraic, |term| Algebraic::Refuted {
                    operator: index,
                    term,
                }),
            Operator::Unknown => Algebraic::Unknown("unsupported operator"),
            _ => Algebraic::Unknown("uncertified or invalid dependency"),
        };
        results.push(result);
    }
    results
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
    pub runtime: &'static str,
}

/// Reconstruct only the operators admitted today; never accept a caller's plan.
#[must_use]
pub fn question(plan: &QuestionPlan) -> QuestionCertificate<'_> {
    let mut operators = vec![Operator::Input];
    for _ in plan.sums() {
        operators.push(Operator::Homomorphism {
            input: 0,
            law: Some(Homomorphism::Select),
        });
        operators.push(Operator::Homomorphism {
            input: operators.len() - 1,
            law: Some(Homomorphism::Project),
        });
        operators.push(Operator::Homomorphism {
            input: operators.len() - 1,
            law: Some(Homomorphism::GroupSum),
        });
    }
    QuestionCertificate {
        plan,
        algebraic: check(&operators, true),
        finalizer: "shared finalizer correspondence is sampled, including rounded ratios and zero divisors",
        runtime: "sampled runtime correspondence",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exhaustive_signed_relations_detect_each_omitted_join_term() {
        // Two-key relations with signed multiplicities; +/-2 includes duplicates.
        let relations: Vec<[i32; 2]> = (-2..=2)
            .flat_map(|a| (-2..=2).map(move |b| [a, b]))
            .collect();
        let join = |a: [i32; 2], b: [i32; 2]| [a[0] * b[0], a[1] * b[1]];
        let add = |a: [i32; 2], b: [i32; 2]| [a[0] + b[0], a[1] + b[1]];
        let mut witnessed = [false; 3];
        for &l in &relations {
            for &r in &relations {
                for &dl in &relations {
                    for &dr in &relations {
                        let old = join(l, r);
                        let new = join(add(l, dl), add(r, dr));
                        let delta = [new[0] - old[0], new[1] - old[1]];
                        let terms = [join(dl, r), join(l, dr), join(dl, dr)];
                        assert_eq!(add(add(terms[0], terms[1]), terms[2]), delta);
                        for omitted in 0..3 {
                            let mut mutant = terms;
                            mutant[omitted] = [0, 0];
                            witnessed[omitted] |=
                                add(add(mutant[0], mutant[1]), mutant[2]) != delta;
                        }
                    }
                }
            }
        }
        assert_eq!(witnessed, [true; 3]);
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
    fn undeclared_folds_and_invalid_compositions_stay_unknown() {
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
