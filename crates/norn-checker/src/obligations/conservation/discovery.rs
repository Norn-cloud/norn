//! CONS-LINEAR-DISCOVERY-1: exact feasibility for homogeneous balance equations plus weight anchors.
//! This equality-only LP has unrestricted rational variables and zero objective.
//! Discovery is untrusted: neither a candidate nor failure is proof evidence.
use super::{Coordinate, Delta, Failure, Rational, Writer, validate};
use std::collections::BTreeMap;

fn limit() -> Failure {
    Failure::Unknown("discovery rational/work limit".into())
}
const ZERO: Rational = Rational {
    numerator: 0,
    denominator: 1,
};
const ONE: Rational = Rational {
    numerator: 1,
    denominator: 1,
};

/// Find a candidate satisfying each scope/unit balance equation and supplied
/// anchors. At least one nonzero anchor is required to exclude the zero vector.
/// Unanchored free variables deterministically become zero. A candidate must
/// still pass the independent checker against the declared functional.
/// # Errors
/// Malformed system/anchors, unsupported writer, exact arithmetic/work limit,
/// or no feasible candidate. Infeasibility is a discovery result, not a refutation
/// of the law or a reachable counterexample.
pub fn discover(
    coordinates: &[Coordinate],
    writers: &[Writer],
    anchors: &[Option<Rational>],
) -> Result<Vec<Rational>, Failure> {
    let declared: Vec<_> = coordinates.iter().map(|c| c.weight).collect();
    validate(coordinates, writers, &declared)?;
    let n = coordinates.len();
    if anchors.len() != n || !anchors.iter().flatten().any(|q| q.numerator != 0) {
        return Err(Failure::Malformed);
    }
    let width = n.checked_add(1).ok_or_else(limit)?;
    let mut partitions = BTreeMap::<_, Vec<usize>>::new();
    for (i, coordinate) in coordinates.iter().enumerate() {
        partitions
            .entry((&coordinate.scope, &coordinate.unit))
            .or_default()
            .push(i);
    }
    let mut equations = Vec::new();
    let mut coefficients = 0_usize;
    for writer in writers {
        let rows = match &writer.delta {
            Delta::Unknown(reason) => {
                return Err(Failure::Unknown(format!("{}: {reason}", writer.id)));
            }
            Delta::Affine(rows) => rows,
        };
        let columns = rows.first().map_or(0, Vec::len);
        if rows.len() != n || columns == 0 || rows.iter().any(|row| row.len() != columns) {
            return Err(Failure::Malformed);
        }
        coefficients = coefficients.saturating_add(n.saturating_mul(columns));
        if coefficients > 65_536 {
            return Err(limit());
        }
        for indices in partitions.values() {
            for column in 0..columns {
                let mut row = vec![ZERO; width];
                for i in indices {
                    let coefficient = rows
                        .get(*i)
                        .and_then(|r| r.get(column))
                        .ok_or(Failure::Malformed)?;
                    *row.get_mut(*i).ok_or(Failure::Malformed)? = *coefficient;
                }
                if row.iter().any(|q| q.numerator != 0) {
                    push(&mut equations, row, width)?;
                }
            }
        }
    }
    for (i, anchor) in anchors.iter().enumerate() {
        if let Some(anchor) = anchor {
            let mut row = vec![ZERO; width];
            *row.get_mut(i).ok_or(Failure::Malformed)? = ONE;
            *row.last_mut().ok_or(Failure::Malformed)? = *anchor;
            push(&mut equations, row, width)?;
        }
    }
    eliminate(equations, n)
}

fn push(
    equations: &mut Vec<Vec<Rational>>,
    row: Vec<Rational>,
    width: usize,
) -> Result<(), Failure> {
    if equations.len().saturating_add(1).saturating_mul(width) > 65_536 {
        return Err(limit());
    }
    equations.push(row);
    Ok(())
}

fn eliminate(mut equations: Vec<Vec<Rational>>, n: usize) -> Result<Vec<Rational>, Failure> {
    let mut pivots = Vec::new();
    let mut work = 0_usize;
    for column in 0..n {
        let rank = pivots.len();
        let Some(pivot) = equations
            .iter()
            .enumerate()
            .skip(rank)
            .find(|(_, row)| row.get(column).is_some_and(|q| q.numerator != 0))
            .map(|(i, _)| i)
        else {
            continue;
        };
        equations.swap(rank, pivot);
        let row = equations.get_mut(rank).ok_or(Failure::Malformed)?;
        let divisor = *row.get(column).ok_or(Failure::Malformed)?;
        for coefficient in row.iter_mut().skip(column) {
            *coefficient = coefficient.checked_div(divisor).ok_or_else(limit)?;
        }
        let normalized = row.clone();
        for (index, row) in equations.iter_mut().enumerate() {
            if index == rank {
                continue;
            }
            let factor = *row.get(column).ok_or(Failure::Malformed)?;
            if factor.numerator == 0 {
                continue;
            }
            for (coefficient, pivot) in row.iter_mut().zip(&normalized).skip(column) {
                work = work.saturating_add(1);
                if work > 1_000_000 {
                    return Err(limit());
                }
                *coefficient = pivot
                    .mul(factor)
                    .and_then(Rational::checked_neg)
                    .and_then(|v| coefficient.add(v))
                    .ok_or_else(limit)?;
            }
        }
        pivots.push(column);
    }
    if equations.iter().any(|row| {
        row.iter().take(n).all(|q| q.numerator == 0) && row.last().is_some_and(|q| q.numerator != 0)
    }) {
        return Err(Failure::Unknown(
            "no feasible weights for these anchors".into(),
        ));
    }
    let mut result = vec![ZERO; n];
    for (rank, column) in pivots.iter().enumerate() {
        *result.get_mut(*column).ok_or(Failure::Malformed)? = *equations
            .get(rank)
            .and_then(|r| r.last())
            .ok_or(Failure::Malformed)?;
    }
    Ok(result)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "small exact discovery fixtures"
)]
mod tests {
    use super::super::check;
    use super::*;
    use norn_ir::{UnitRef, UnitSystem};
    use proptest::prelude::*;

    fn q(n: i64, d: i64) -> Rational {
        Rational::new(n, d).unwrap()
    }
    fn system(n: i64, d: i64) -> (Vec<Coordinate>, Vec<Writer>) {
        let coordinates = [q(d, n), ONE]
            .into_iter()
            .enumerate()
            .map(|(i, weight)| Coordinate {
                id: i.to_string(),
                scope: BTreeMap::from([("book".into(), "primary".into())]),
                unit: UnitRef {
                    system: UnitSystem::Ucum,
                    code: "g".into(),
                },
                weight,
            })
            .collect();
        (
            coordinates,
            vec![Writer {
                id: "exchange".into(),
                delta: Delta::Affine(vec![
                    vec![q(n.checked_neg().unwrap(), 1), ZERO],
                    vec![q(d, 1), ZERO],
                ]),
            }],
        )
    }

    #[test]
    fn cons_discovery_fractional_candidate_requires_separate_check() {
        let (mut c, w) = system(2, 1);
        let candidate = discover(&c, &w, &[None, Some(ONE)]).unwrap();
        assert_eq!(candidate, [q(1, 2), ONE]);
        assert_eq!(check(&c, &w, &candidate), Ok(()));
        c[0].weight = ONE;
        assert_eq!(check(&c, &w, &candidate), Err(Failure::Malformed));
    }

    #[test]
    fn cons_discovery_anchor_conflict_units_books_and_opaque_are_not_proofs() {
        let (mut c, mut w) = system(2, 1);
        assert!(matches!(
            discover(&c, &w, &[Some(ONE), Some(ONE)]),
            Err(Failure::Unknown(_))
        ));
        assert_eq!(
            discover(&c, &w, &[None, Some(ZERO)]),
            Err(Failure::Malformed)
        );
        c[1].unit.code = "mg".into();
        assert!(matches!(
            discover(&c, &w, &[None, Some(ONE)]),
            Err(Failure::Unknown(_))
        ));
        c[1].unit.code = "g".into();
        c[1].scope.insert("book".into(), "other".into());
        assert!(matches!(
            discover(&c, &w, &[None, Some(ONE)]),
            Err(Failure::Unknown(_))
        ));
        w[0].delta = Delta::Unknown("opaque performer".into());
        assert!(
            matches!(discover(&c, &w, &[None, Some(ONE)]), Err(Failure::Unknown(reason)) if reason.contains("opaque"))
        );
    }

    #[test]
    fn cons_discovery_free_coordinates_and_duplicate_identity() {
        let (mut c, mut w) = system(1, 1);
        let mut free = c[0].clone();
        free.id = "free".into();
        free.weight = ZERO;
        c.push(free);
        let Delta::Affine(rows) = &mut w[0].delta else {
            unreachable!()
        };
        rows.push(vec![ZERO; 2]);
        let candidate = discover(&c, &w, &[Some(ONE), None, None]).unwrap();
        assert_eq!(candidate, [ONE, ONE, ZERO]);
        assert_eq!(check(&c, &w, &candidate), Ok(()));
        w.push(w[0].clone());
        assert_eq!(
            discover(&c, &w, &[Some(ONE), None, None]),
            Err(Failure::Malformed)
        );
        assert_eq!(check(&c, &w, &candidate), Err(Failure::Malformed));
    }

    proptest! {
        #[test]
        fn cons_discovered_scaled_exchange_satisfies_independent_checker(n in 1_i64..1000, d in 1_i64..1000) {
            let (c, w) = system(n, d);
            let candidate = discover(&c, &w, &[None, Some(ONE)]).unwrap();
            prop_assert_eq!(&candidate, &[q(d, n), ONE]);
            prop_assert_eq!(check(&c, &w, &candidate), Ok(()));
        }
    }
}
