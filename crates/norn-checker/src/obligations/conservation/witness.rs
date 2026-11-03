//! CONS-LINEAR-WITNESS-1: exact algebraic countervaluations, not reachable traces.
use super::{Coordinate, Delta, Failure, Rational, Writer, certificate::number, check};
use serde_json::{Value, json};

fn limit() -> Failure {
    Failure::Unknown("witness rational representation limit".into())
}

/// Recheck the declared functional and render a concrete rational valuation
/// whose change is nonzero in one exact scope/unit partition. Constant failure
/// uses all-zero parameters; otherwise one basis parameter is one. No quantity
/// domain is enumerated. Guards and runtime representability remain unchecked.
/// # Errors
/// Malformed/unsupported system or exact arithmetic limit; never fabricated data.
pub fn render(coordinates: &[Coordinate], writers: &[Writer]) -> Result<Option<Value>, Failure> {
    let failure = match check(
        coordinates,
        writers,
        &coordinates.iter().map(|c| c.weight).collect::<Vec<_>>(),
    ) {
        Ok(()) => return Ok(None),
        Err(failure @ Failure::Unbalanced { .. }) => failure,
        Err(failure) => return Err(failure),
    };
    let Failure::Unbalanced {
        writer,
        coordinate,
        column,
        ..
    } = failure
    else {
        return Err(Failure::Malformed);
    };
    let partition = coordinates
        .iter()
        .find(|c| c.id == coordinate)
        .ok_or(Failure::Malformed)?;
    let writer = writers
        .iter()
        .find(|w| w.id == writer)
        .ok_or(Failure::Malformed)?;
    let Delta::Affine(rows) = &writer.delta else {
        return Err(Failure::Malformed);
    };
    let count = rows
        .first()
        .and_then(|r| r.len().checked_sub(1))
        .ok_or(Failure::Malformed)?;
    let zero = Rational::new(0, 1).ok_or(Failure::Malformed)?;
    let mut constant = zero;
    for (c, row) in coordinates.iter().zip(rows) {
        if c.scope == partition.scope && c.unit == partition.unit {
            constant = row
                .last()
                .ok_or(Failure::Malformed)?
                .mul(c.weight)
                .and_then(|q| constant.add(q))
                .ok_or_else(limit)?;
        }
    }
    let mut parameters = vec![zero; count];
    if constant.numerator == 0 {
        *parameters.get_mut(column).ok_or(Failure::Malformed)? =
            Rational::new(1, 1).ok_or(Failure::Malformed)?;
    }
    let mut change = zero;
    let mut deltas = Vec::new();
    for (c, row) in coordinates.iter().zip(rows) {
        let mut delta = *row.last().ok_or(Failure::Malformed)?;
        for (coefficient, value) in row.iter().zip(&parameters) {
            delta = coefficient
                .mul(*value)
                .and_then(|q| delta.add(q))
                .ok_or_else(limit)?;
        }
        if c.scope == partition.scope && c.unit == partition.unit {
            change = delta
                .mul(c.weight)
                .and_then(|q| change.add(q))
                .ok_or_else(limit)?;
        }
        deltas.push(json!({"coordinate":c.id,"scope":c.scope,"unit":c.unit,"delta":number(delta)}));
    }
    if change.numerator == 0 {
        return Err(Failure::Malformed);
    }
    Ok(Some(
        json!({"kind":"algebraic_countervaluation","writer":writer.id,
        "scope":partition.scope,"unit":partition.unit,"parameters":parameters.into_iter().map(number).collect::<Vec<_>>(),
        "deltas":deltas,"weightedChange":number(change),
        "guardFeasibility":"unknown","runtimeRepresentability":"unknown"}),
    ))
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "symbolic witness fixtures"
)]
mod tests {
    use super::*;

    #[test]
    fn cons_witness_scoped_unit_and_constant_countervaluations() {
        let q = |n| Rational::new(n, 1).unwrap();
        let (mut c, mut w) = super::super::tests::system();
        assert_eq!(render(&c, &w), Ok(None));
        c[1].scope.insert("book".into(), "two".into());
        let witness = render(&c, &w).unwrap().unwrap();
        assert_eq!(witness["weightedChange"], json!(["-1", "1"]));
        assert_eq!(witness["parameters"], json!([["0", "1"], ["1", "1"]]));
        assert_eq!(witness["guardFeasibility"], "unknown");
        c[1].scope = c[0].scope.clone();
        c[1].unit.code = "mg".into();
        assert!(render(&c, &w).unwrap().is_some());
        let Delta::Affine(rows) = &mut w[0].delta else {
            unreachable!()
        };
        *rows[0].last_mut().unwrap() = q(1);
        let witness = render(&c, &w).unwrap().unwrap();
        assert_eq!(witness["parameters"], json!([["0", "1"], ["0", "1"]]));
        assert_eq!(witness["weightedChange"], json!(["1", "1"]));
    }
}
