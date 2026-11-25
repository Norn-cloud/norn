//! The engine's column profiles as the object profile presents them:
//! extremes, nearest-rank quantiles, an equal-population or label histogram
//! and Tukey's far-out values.

use super::Refusal;

/// One property's profile from the engine's sorted values: a property
/// without a column (computed) counts every row as null.
pub(super) fn property_profile(
    property: &str,
    declared: Option<&norn_ir::PropertyType>,
    column: Option<&norn_cell::analytics::ColumnProfile>,
    rows: u64,
) -> Result<crate::object_profile::NornPropertyProfile, Refusal> {
    use crate::object_profile::{
        NornBin, NornHistogram, NornPropertyProfile, NornQuantile, kind_of,
    };
    use norn_cell::analytics::{PROFILE_QUANTILES, bin_ranks, nearest_rank, runtime_value};
    let kind = kind_of(declared);
    let mut out = NornPropertyProfile {
        property: property.to_owned(),
        kind: kind.to_owned(),
        count: column.map_or(0, |c| c.count),
        nulls: column.map_or(rows, |c| c.nulls),
        distinct: column.map_or(0, |c| c.distinct),
        min: None,
        max: None,
        quantiles: Vec::new(),
        histogram: None,
        outliers: None,
    };
    let Some(column) = column.filter(|c| c.count > 0 && (kind == "numeric" || kind == "temporal"))
    else {
        return Ok(out);
    };
    let n = column.count;
    let at = |rank: u64| {
        column
            .ranks
            .get(&rank)
            .map(|v| runtime_value(v).map_err(Refusal::host))
            .transpose()
    };
    out.min = at(1)?;
    out.max = at(n)?;
    for percent in PROFILE_QUANTILES {
        if let Some(value) = at(nearest_rank(percent, n))? {
            out.quantiles.push(NornQuantile { percent, value });
        }
    }
    if kind == "numeric" {
        let mut bins = Vec::new();
        for (first, last) in bin_ranks(n) {
            if let (Some(lower), Some(upper)) = (at(first)?, at(last)?) {
                bins.push(NornBin {
                    lower,
                    upper,
                    count: last.saturating_sub(first).saturating_add(1),
                });
            }
        }
        out.histogram = Some(NornHistogram {
            kind: "equal_population".into(),
            bins,
        });
        if let (Some(found), Some(q1), Some(q3)) = (
            &column.outliers,
            at(nearest_rank(25, n))?,
            at(nearest_rank(75, n))?,
        ) {
            out.outliers = Some(outliers_of(found, q1, q3)?);
        }
    } else if let Some((kind, labels)) = &column.labels {
        out.histogram = Some(NornHistogram {
            kind: kind.clone(),
            bins: labels
                .iter()
                .map(|(label, rows)| NornBin {
                    lower: norn_ir::RuntimeValue::String(label.clone()),
                    upper: norn_ir::RuntimeValue::String(label.clone()),
                    count: *rows,
                })
                .collect(),
        });
    }
    Ok(out)
}

fn outliers_of(
    found: &norn_cell::analytics::OutlierSummary,
    q1: norn_ir::RuntimeValue,
    q3: norn_ir::RuntimeValue,
) -> Result<crate::object_profile::NornOutliers, Refusal> {
    use crate::object_profile::{NornOutlier, NornOutliers};
    let number = |text: &str| {
        norn_ir::Decimal::parse(text)
            .map(norn_ir::RuntimeValue::Number)
            .map_err(Refusal::host)
    };
    Ok(NornOutliers {
        method: "tukey_far_out".into(),
        k: norn_cell::analytics::PROFILE_OUTLIER_K.to_string(),
        q1,
        q3,
        lower_fence: number(&found.lower)?,
        upper_fence: number(&found.upper)?,
        count: found.count,
        examples: found
            .examples
            .iter()
            .map(|(id, value)| {
                Ok(NornOutlier {
                    id: id.clone(),
                    value: number(value)?,
                })
            })
            .collect::<Result<_, Refusal>>()?,
    })
}
