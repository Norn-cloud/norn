//! CONS-LINEAR-1: exact coefficient checking, conditional on a trusted lowering.
//!
//! This module accepts a mathematical transition system, not declarations. Its
//! success is not application evidence: the source-bound adapter must establish
//! complete writer coverage, the declared functional and faithful affine deltas.
use std::collections::{BTreeMap, BTreeSet};

use norn_ir::UnitRef;

/// Untrusted exact equality-feasibility discovery; candidates require checking.
pub mod discovery;

pub use crate::exact_rational::Rational;

/// One coordinate of the declared conserved functional.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Coordinate {
    /// Stable source coordinate identity.
    pub id: String,
    /// Exact scope bindings, including book/currency/valuation basis when present.
    pub scope: BTreeMap<String, String>,
    /// Exact unit identity. No implicit conversion or cross-unit cancellation.
    pub unit: UnitRef,
    /// Weight required by the declared law, independently of discovery.
    pub weight: Rational,
}

/// Each row is one coordinate; columns are independent symbolic parameters,
/// followed by a constant column. Every possible parameter value is checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delta {
    /// Faithfully extracted affine state change.
    Affine(Vec<Vec<Rational>>),
    /// Untranslated writer, including nonlinear or opaque semantics.
    Unknown(String),
}

/// A writer in the complete source-bound closure supplied by the adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Writer {
    /// Stable declaration identity.
    pub id: String,
    /// Proposed delta before target-law filtering.
    pub delta: Delta,
}

/// A refusal explains exactly which symbolic coefficient did not balance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// Invalid shape, duplicate identity, zero functional or altered weights.
    Malformed,
    /// Unsupported semantics or arithmetic/resource limit; no proof claim.
    Unknown(String),
    /// Nonzero coefficient in a single scope/unit partition. This is an
    /// algebraic witness, not a claim of guard-feasible runtime reachability.
    Unbalanced {
        /// Writer declaration identity.
        writer: String,
        /// Representative coordinate identifying the scope and unit partition.
        coordinate: String,
        /// Parameter column (last column is the affine constant).
        column: usize,
        /// Exact nonzero weighted coefficient.
        residual: Rational,
    },
}

fn validate(
    coordinates: &[Coordinate],
    writers: &[Writer],
    certificate: &[Rational],
) -> Result<(), Failure> {
    let malformed = || Failure::Malformed;
    if coordinates.is_empty()
        || coordinates.len() > 256
        || writers.len() > 256
        || certificate.len() != coordinates.len()
        || !coordinates.iter().any(|c| c.weight.numerator != 0)
    {
        return Err(malformed());
    }
    let mut ids = BTreeSet::new();
    for (coordinate, weight) in coordinates.iter().zip(certificate) {
        if coordinate.id.is_empty()
            || coordinate.unit.code.is_empty()
            || coordinate.scope.is_empty()
            || coordinate
                .scope
                .iter()
                .any(|(k, v)| k.is_empty() || v.is_empty())
            || !ids.insert(&coordinate.id)
            || coordinate.weight != *weight
        {
            return Err(malformed());
        }
    }
    let mut writer_ids = BTreeSet::new();
    if writers
        .iter()
        .any(|w| w.id.is_empty() || !writer_ids.insert(&w.id))
    {
        return Err(malformed());
    }
    Ok(())
}

/// CONS-LINEAR-1: verify all coefficients of wᵀΔ independently per scope/unit.
/// A certificate may not replace the declared functional with another nullspace
/// vector. This checks all parameter valuations without quantity enumeration.
/// Empty writer sets prove only the mathematical no-step system.
/// # Errors
/// Malformed certificate/system, unsupported writer, resource/number limit, or
/// a nonzero symbolic coefficient. No failure grants application authority.
pub fn check(
    coordinates: &[Coordinate],
    writers: &[Writer],
    certificate: &[Rational],
) -> Result<(), Failure> {
    validate(coordinates, writers, certificate)?;
    let mut work = 0_usize;
    for writer in writers {
        let rows = match &writer.delta {
            Delta::Affine(rows) => rows,
            Delta::Unknown(reason) => {
                return Err(Failure::Unknown(format!("{}: {reason}", writer.id)));
            }
        };
        let columns = rows.first().map_or(0, Vec::len);
        if rows.len() != coordinates.len()
            || columns == 0
            || rows.iter().any(|r| r.len() != columns)
        {
            return Err(Failure::Malformed);
        }
        work = work.saturating_add(rows.len().saturating_mul(columns));
        if work > 65_536 {
            return Err(Failure::Unknown("coefficient budget exceeded".into()));
        }
        let mut partitions = BTreeMap::new();
        for (coordinate, row) in coordinates.iter().zip(rows) {
            let (_, totals) = partitions
                .entry((&coordinate.scope, &coordinate.unit))
                .or_insert_with(|| {
                    (
                        &coordinate.id,
                        vec![
                            Rational {
                                numerator: 0,
                                denominator: 1
                            };
                            columns
                        ],
                    )
                });
            for (total, coefficient) in totals.iter_mut().zip(row) {
                *total = coefficient
                    .mul(coordinate.weight)
                    .and_then(|v| total.add(v))
                    .ok_or_else(|| {
                        Failure::Unknown(format!("{}: rational representation limit", writer.id))
                    })?;
            }
        }
        for (_, (coordinate, totals)) in partitions {
            for (column, residual) in totals.into_iter().enumerate() {
                if residual.numerator != 0 {
                    return Err(Failure::Unbalanced {
                        writer: writer.id.clone(),
                        coordinate: coordinate.clone(),
                        column,
                        residual,
                    });
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "small exact certificate fixtures"
)]
mod tests {
    use super::*;
    use norn_ir::UnitSystem;
    use proptest::prelude::*;

    fn q(n: i64) -> Rational {
        Rational::new(n, 1).unwrap()
    }
    fn system() -> (Vec<Coordinate>, Vec<Writer>) {
        let coordinates = ["input", "output", "loss"]
            .into_iter()
            .map(|id| Coordinate {
                id: id.into(),
                scope: BTreeMap::from([("book".into(), "primary".into())]),
                unit: UnitRef {
                    system: UnitSystem::Ucum,
                    code: "g".into(),
                },
                weight: q(1),
            })
            .collect();
        let writer = Writer {
            id: "convert".into(),
            delta: Delta::Affine(vec![
                vec![q(-1), q(0), q(0)],
                vec![q(0), q(1), q(0)],
                vec![q(1), q(-1), q(0)],
            ]),
        };
        (coordinates, vec![writer])
    }

    #[test]
    fn cons_linear_valid_and_omitted_loss() {
        let (c, mut w) = system();
        assert_eq!(check(&c, &w, &[q(1); 3]), Ok(()));
        let Delta::Affine(rows) = &mut w[0].delta else {
            unreachable!()
        };
        rows[2] = vec![q(0); 3];
        assert!(
            matches!(check(&c, &w, &[q(1); 3]), Err(Failure::Unbalanced { column: 0, residual, .. }) if residual == q(-1))
        );
    }

    #[test]
    fn cons_linear_units_books_and_functional_cannot_be_substituted() {
        let (mut c, w) = system();
        c[2].unit.code = "mg".into();
        assert!(matches!(
            check(&c, &w, &[q(1); 3]),
            Err(Failure::Unbalanced { .. })
        ));
        c[2].unit.code = "g".into();
        c[2].scope.insert("book".into(), "other".into());
        assert!(matches!(
            check(&c, &w, &[q(1); 3]),
            Err(Failure::Unbalanced { .. })
        ));
        assert_eq!(check(&c, &w, &[q(0); 3]), Err(Failure::Malformed));
        assert_eq!(check(&c, &w, &[q(1); 2]), Err(Failure::Malformed));
    }

    #[test]
    fn cons_linear_unknown_and_limits() {
        let (c, mut w) = system();
        w[0].delta = Delta::Unknown("nonlinear multiplication".into());
        assert!(
            matches!(check(&c, &w, &[q(1); 3]), Err(Failure::Unknown(reason)) if reason.contains("nonlinear"))
        );
        assert!(Rational::new(1, 0).is_none());
        assert_eq!(Rational::new(2, -4), Rational::new(-1, 2));
        assert_eq!(Rational::new(i64::MIN, i64::MIN), Some(q(1)));
        assert_eq!(q(i64::MAX).add(q(1)), None);
        assert_eq!(q(i64::MAX).mul(q(i64::MAX)), None);
        w[0].delta = Delta::Affine(vec![vec![q(0)]; 2]);
        assert_eq!(check(&c, &w, &[q(1); 3]), Err(Failure::Malformed));
    }

    proptest! {
        #[test]
        fn cons_linear_fractional_balance(n in -10000_i64..10000, d in 1_i64..10000, amount in -10000_i64..10000) {
            let r = Rational::new(n, d).unwrap();
            let neg = Rational::new(n.checked_neg().unwrap(), d).unwrap();
            prop_assert_eq!(r.add(neg), Some(q(0)));
            prop_assert_eq!(r.mul(q(amount)).unwrap().add(neg.mul(q(amount)).unwrap()), Some(q(0)));
            let (mut c, mut w) = system();
            for coordinate in &mut c { coordinate.weight = Rational::new(1, d).unwrap(); }
            prop_assert_eq!(check(&c, &w, &[Rational::new(1, d).unwrap(); 3]), Ok(()));
            let Delta::Affine(rows) = &mut w[0].delta else { unreachable!() };
            rows[2][2] = q(amount);
            prop_assert_eq!(check(&c, &w, &[Rational::new(1, d).unwrap(); 3]).is_ok(), amount == 0);
        }
    }
}
