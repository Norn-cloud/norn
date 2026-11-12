//! HOST-READ-5: `GET /v1/objects/{type}:profile`, a column profile of the
//! caller's authorized snapshot of a concept's objects.
//!
//! The profile reads the same snapshot a grid query reads, through the same
//! gates (D67): the collection gate first, then each row on its own. Only rows
//! whose values the caller may see are profiled; a withheld row is counted as
//! withheld and none of its values is counted anywhere. Quarantined rows are
//! left out, as on a page.
//!
//! The engine is the native exact one: decimals are compared exactly,
//! quantiles are taken by rank (nearest rank, never interpolated), and no
//! number passes through a float. The work is bounded by the same visible-row
//! scan as a grid query; past it the profile covers the rows read and says
//! `approximate`.

use std::collections::{BTreeMap, BTreeSet};

use norn_cell::engine::SqlEngine;
use norn_ir::{Decimal, PropertyType, RuntimeValue};
use serde::Serialize;
use utoipa::ToSchema;
use utoipa::openapi::schema::{Object, Schema, SchemaType};

use crate::{
    host::DevHost,
    object_routes::{MAX_QUERY_SCAN, ObjectAccess, collection, not_found, scan_listed},
    refusal::Refusal,
    scope::ScopeQuery,
};

/// Equal-population bins of a numeric histogram.
const NUMERIC_BINS: usize = 10;
/// A temporal histogram counts per day up to this many days, else per month.
const MAX_DAY_BINS: usize = 62;
/// Tukey's far-out multiplier: a value further than this many interquartile
/// ranges outside the quartiles is an outlier.
const OUTLIER_K: u8 = 3;
/// Most outlier rows one property profile names.
const MAX_OUTLIER_EXAMPLES: usize = 20;
/// The quantiles reported, in percent.
const QUANTILES: [u32; 5] = [0, 25, 50, 75, 100];

/// The profile of a concept's objects as the caller may see them.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NornObjectProfile {
    /// The concept profiled; rows may be of its subtypes.
    pub object_type: String,
    /// Rows whose values were profiled.
    pub rows: u64,
    /// Rows the caller may know exist but whose values are withheld; none of
    /// their values is counted.
    pub withheld_rows: u64,
    /// Rows held back by source disputes or open quarantining findings.
    pub quarantined_rows: u64,
    /// True when the scan bound was reached: the profile covers only the rows
    /// read, and every count is a lower bound.
    pub approximate: bool,
    /// The visible-row scan bound.
    pub scan_limit: u64,
    /// The engine that computed the profile: `cell`, the native exact engine.
    pub engine: String,
    /// One profile per declared property, in declared order.
    pub properties: Vec<NornPropertyProfile>,
}

/// The profile of one property over the profiled rows.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NornPropertyProfile {
    /// The property id.
    pub property: String,
    /// `numeric`, `temporal`, `boolean`, `text` or `other`.
    pub kind: String,
    /// Rows with a value.
    pub count: u64,
    /// Rows without one.
    pub nulls: u64,
    /// Distinct values.
    pub distinct: u64,
    /// The smallest value (numeric and temporal properties).
    #[schema(schema_with = optional_value_schema, required = true)]
    pub min: Option<RuntimeValue>,
    /// The largest value (numeric and temporal properties).
    #[schema(schema_with = optional_value_schema, required = true)]
    pub max: Option<RuntimeValue>,
    /// Quantiles by nearest rank (numeric and temporal properties).
    pub quantiles: Vec<NornQuantile>,
    /// The distribution (numeric and temporal properties).
    #[schema(required = true, nullable = true)]
    pub histogram: Option<NornHistogram>,
    /// Values far outside the middle half (numeric properties; null for
    /// every other kind, and when the quartiles cannot be subtracted exactly).
    #[schema(required = true, nullable = true)]
    pub outliers: Option<NornOutliers>,
}

/// Tukey's far-out values of one numeric property over the profiled rows:
/// those below `q1 − k·(q3 − q1)` or above `q3 + k·(q3 − q1)`. The quartiles
/// are the profile's own nearest-rank quantiles (25 and 75), so the fences
/// agree with the quantiles shown, and every number is exact (no float).
/// Like the rest of the profile, only rows whose values the caller may read
/// are counted or named; past the scan bound (`approximate`) the outliers
/// cover only the rows read.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NornOutliers {
    /// `tukey_far_out`.
    pub method: String,
    /// The multiplier `k`, as an exact decimal (`"3"`).
    pub k: String,
    /// The 25th percentile by nearest rank.
    #[schema(schema_with = value_schema)]
    pub q1: RuntimeValue,
    /// The 75th percentile by nearest rank.
    #[schema(schema_with = value_schema)]
    pub q3: RuntimeValue,
    /// `q1 − k·(q3 − q1)`; a value below it is an outlier.
    #[schema(schema_with = value_schema)]
    pub lower_fence: RuntimeValue,
    /// `q3 + k·(q3 − q1)`; a value above it is an outlier.
    #[schema(schema_with = value_schema)]
    pub upper_fence: RuntimeValue,
    /// Rows outside a fence.
    pub count: u64,
    /// At most 20 of them, furthest from their fence first, then by id.
    pub examples: Vec<NornOutlier>,
}

/// One row whose value is outside a fence.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NornOutlier {
    /// The row's id.
    pub id: String,
    /// Its value.
    #[schema(schema_with = value_schema)]
    pub value: RuntimeValue,
}

/// One quantile.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NornQuantile {
    /// The percentile, 0 to 100.
    pub percent: u32,
    /// The value at that nearest rank.
    #[schema(schema_with = value_schema)]
    pub value: RuntimeValue,
}

/// A histogram.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NornHistogram {
    /// `equal_population` (numeric: bins of equal row count, bounded by the
    /// values at their edges, so no bin edge is computed), `day` or `month`
    /// (temporal: calendar bins in UTC).
    pub kind: String,
    /// Bins in ascending order.
    pub bins: Vec<NornBin>,
}

/// One histogram bin.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NornBin {
    /// The smallest value in the bin, or the calendar label.
    #[schema(schema_with = value_schema)]
    pub lower: RuntimeValue,
    /// The largest value in the bin, or the calendar label.
    #[schema(schema_with = value_schema)]
    pub upper: RuntimeValue,
    /// Rows in the bin.
    pub count: u64,
}

const VALUE: &str = "A property value: an exact decimal as a raw JSON number token (read its \
     text, not a binary float), or a UTC RFC 3339 timestamp or ISO 8601 date string.";

fn value_schema() -> Schema {
    let mut schema = Object::with_type(SchemaType::AnyValue);
    schema.description = Some(VALUE.to_owned());
    Schema::Object(schema)
}

fn optional_value_schema() -> Schema {
    let mut schema = Object::with_type(SchemaType::AnyValue);
    schema.description = Some(format!(
        "{VALUE} Null when the property has no value to report."
    ));
    Schema::Object(schema)
}

fn count(n: usize) -> u64 {
    u64::try_from(n).unwrap_or(u64::MAX)
}

/// The value at nearest rank `percent` of `sorted` (non-empty).
fn at_rank<T: Clone>(sorted: &[T], percent: u32) -> Option<T> {
    let n = sorted.len();
    let percent = usize::try_from(percent).ok()?;
    let rank = percent
        .checked_mul(n)?
        .checked_add(99)?
        .checked_div(100)?
        .max(1);
    sorted.get(rank.checked_sub(1)?).cloned()
}

fn numeric_histogram(sorted: &[RuntimeValue]) -> NornHistogram {
    let n = sorted.len();
    let mut bins = Vec::new();
    for bin in 0..NUMERIC_BINS {
        let start = bin.saturating_mul(n).checked_div(NUMERIC_BINS).unwrap_or(0);
        let end = bin
            .saturating_add(1)
            .saturating_mul(n)
            .checked_div(NUMERIC_BINS)
            .unwrap_or(0);
        let Some(slice) = sorted.get(start..end) else {
            continue;
        };
        if let (Some(lower), Some(upper)) = (slice.first(), slice.last()) {
            bins.push(NornBin {
                lower: lower.clone(),
                upper: upper.clone(),
                count: count(slice.len()),
            });
        }
    }
    NornHistogram {
        kind: "equal_population".into(),
        bins,
    }
}

/// Calendar bins of RFC 3339 values: per day, or per month past
/// [`MAX_DAY_BINS`] days.
fn temporal_histogram(sorted: &[String]) -> NornHistogram {
    let label = |text: &str, width: usize| text.get(..width).unwrap_or(text).to_owned();
    let days: BTreeSet<String> = sorted.iter().map(|text| label(text, 10)).collect();
    let (kind, width) = if days.len() <= MAX_DAY_BINS {
        ("day", 10)
    } else {
        ("month", 7)
    };
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for text in sorted {
        let slot = counts.entry(label(text, width)).or_default();
        *slot = slot.saturating_add(1);
    }
    NornHistogram {
        kind: kind.into(),
        bins: counts
            .into_iter()
            .map(|(label, rows)| NornBin {
                lower: RuntimeValue::String(label.clone()),
                upper: RuntimeValue::String(label),
                count: count(rows),
            })
            .collect(),
    }
}

fn kind_of(property: Option<&PropertyType>) -> &'static str {
    match property {
        Some(PropertyType::Integer { .. } | PropertyType::Decimal { .. }) => "numeric",
        Some(PropertyType::Timestamp { .. } | PropertyType::Date { .. }) => "temporal",
        Some(PropertyType::Boolean { .. }) => "boolean",
        Some(PropertyType::Text { .. } | PropertyType::Enum { .. }) => "text",
        Some(
            PropertyType::DecimalBinding { .. }
            | PropertyType::Duration { .. }
            | PropertyType::Record { .. }
            | PropertyType::JsonExt { .. }
            | PropertyType::Ref { .. },
        )
        | None => "other",
    }
}

fn profile_property(
    property: &str,
    kind: &'static str,
    values: &[Option<&RuntimeValue>],
) -> NornPropertyProfile {
    // A stored null is absent like a missing value: counted in `nulls`, in no statistic.
    let present: Vec<&RuntimeValue> = values
        .iter()
        .filter_map(|value| *value)
        .filter(|value| !matches!(value, RuntimeValue::Null))
        .collect();
    let nulls = values.len().saturating_sub(present.len());
    let distinct: BTreeSet<String> = present.iter().map(|value| format!("{value:?}")).collect();
    let mut out = NornPropertyProfile {
        property: property.to_owned(),
        kind: kind.to_owned(),
        count: count(present.len()),
        nulls: count(nulls),
        distinct: count(distinct.len()),
        min: None,
        max: None,
        quantiles: Vec::new(),
        histogram: None,
        outliers: None,
    };
    match kind {
        "numeric" => {
            let mut numbers: Vec<_> = present
                .iter()
                .filter_map(|value| match value {
                    RuntimeValue::Number(number) => Some(*number),
                    RuntimeValue::Null
                    | RuntimeValue::Bool(_)
                    | RuntimeValue::String(_)
                    | RuntimeValue::Array(_)
                    | RuntimeValue::Object(_) => None,
                })
                .collect();
            numbers.sort();
            let sorted: Vec<RuntimeValue> = numbers.into_iter().map(RuntimeValue::Number).collect();
            fill(&mut out, &sorted);
            if !sorted.is_empty() {
                out.histogram = Some(numeric_histogram(&sorted));
            }
        }
        "temporal" => {
            let mut texts: Vec<String> = present
                .iter()
                .filter_map(|value| match value {
                    RuntimeValue::String(text) => Some(text.clone()),
                    RuntimeValue::Null
                    | RuntimeValue::Bool(_)
                    | RuntimeValue::Number(_)
                    | RuntimeValue::Array(_)
                    | RuntimeValue::Object(_) => None,
                })
                .collect();
            // Stored instants are normalized UTC RFC 3339 and dates are
            // ISO 8601, so text order is time order.
            texts.sort();
            let sorted: Vec<RuntimeValue> =
                texts.iter().cloned().map(RuntimeValue::String).collect();
            fill(&mut out, &sorted);
            if !texts.is_empty() {
                out.histogram = Some(temporal_histogram(&texts));
            }
        }
        _ => {}
    }
    out
}

/// Tukey's far-out values among `rows` (id, exact number), or `None` when a
/// fence cannot be computed exactly or there are no rows.
fn outliers(rows: &[(&str, Decimal)]) -> Option<NornOutliers> {
    let mut sorted: Vec<Decimal> = rows.iter().map(|(_, value)| *value).collect();
    sorted.sort();
    let (q1, q3) = (at_rank(&sorted, 25)?, at_rank(&sorted, 75)?);
    let iqr = q3.checked_sub(q1)?;
    let span = (1..OUTLIER_K).try_fold(iqr, |total, _| total.checked_add(iqr))?;
    let (lower, upper) = (q1.checked_sub(span)?, q3.checked_add(span)?);
    let mut found: Vec<(Decimal, &str, Decimal)> = rows
        .iter()
        .filter_map(|(id, value)| {
            let distance = if *value < lower {
                lower.checked_sub(*value)
            } else if *value > upper {
                value.checked_sub(upper)
            } else {
                return None;
            };
            distance.map(|distance| (distance, *id, *value))
        })
        .collect();
    found.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(b.1)));
    Some(NornOutliers {
        method: "tukey_far_out".into(),
        k: OUTLIER_K.to_string(),
        q1: RuntimeValue::Number(q1),
        q3: RuntimeValue::Number(q3),
        lower_fence: RuntimeValue::Number(lower),
        upper_fence: RuntimeValue::Number(upper),
        count: count(found.len()),
        examples: found
            .into_iter()
            .take(MAX_OUTLIER_EXAMPLES)
            .map(|(_, id, value)| NornOutlier {
                id: id.to_owned(),
                value: RuntimeValue::Number(value),
            })
            .collect(),
    })
}

fn fill(out: &mut NornPropertyProfile, sorted: &[RuntimeValue]) {
    out.min = sorted.first().cloned();
    out.max = sorted.last().cloned();
    out.quantiles = QUANTILES
        .iter()
        .filter_map(|percent| {
            at_rank(sorted, *percent).map(|value| NornQuantile {
                percent: *percent,
                value,
            })
        })
        .collect();
}

/// HOST-READ-5: profile the caller's authorized snapshot of `object_type`.
///
/// # Errors
/// A denied gate (the opaque 403), an undeclared type (404), an engine that
/// cannot prove one snapshot (503), a commit while the snapshot was read
/// (409 `object.snapshot_moved`, `retryable`), or a host failure.
#[utoipa::path(get, path = "/v1/objects/{type}:profile", tag = "crate::object_routes", params(("type" = String, Path, description = "The concept to profile."), ("orgId" = String, Query), ("environmentId" = String, Query)), responses((status = 200, description = "The profile of the caller's authorized snapshot.", body = NornObjectProfile), (status = 403, description = "The request was refused: scope or authority denied.", body = Refusal), (status = 404, description = "The request was refused: not found.", body = Refusal), (status = 409, description = "The snapshot moved while it was read; retry.", body = Refusal), (status = 500, description = "The request was refused: host failure.", body = Refusal)))]
pub fn get_object_profile<E: SqlEngine>(
    host: &DevHost<E>,
    scope: &ScopeQuery,
    actor: Option<&str>,
    object_type: &str,
    access: &dyn ObjectAccess,
) -> Result<NornObjectProfile, Refusal> {
    let (_epoch, actor) = collection(host, scope, actor, object_type, access)?;
    let properties = {
        let cell = host.cell()?;
        let reader = cell.store().read();
        if !reader.model().has_type(object_type) {
            return Err(not_found());
        }
        reader
            .model()
            .effective_property_types(object_type)
            .map_err(Refusal::host)?
    };
    let scan = scan_listed(
        host,
        access,
        &actor,
        object_type,
        (None, usize::MAX, MAX_QUERY_SCAN),
    )?;
    let readable: Vec<_> = scan
        .listed
        .iter()
        .filter(|listed| listed.readable)
        .collect();
    let withheld = scan.listed.len().saturating_sub(readable.len());
    Ok(NornObjectProfile {
        object_type: object_type.to_owned(),
        rows: count(readable.len()),
        withheld_rows: count(withheld),
        quarantined_rows: scan.quarantined,
        approximate: !scan.exhausted,
        scan_limit: count(MAX_QUERY_SCAN),
        engine: "cell".into(),
        properties: properties
            .iter()
            .map(|(property, declared)| {
                let values: Vec<Option<&RuntimeValue>> = readable
                    .iter()
                    .map(|listed| {
                        listed
                            .row
                            .values
                            .get(property)
                            .filter(|value| **value != RuntimeValue::Null)
                    })
                    .collect();
                let kind = kind_of(declared.as_ref());
                let mut profile = profile_property(property, kind, &values);
                if kind == "numeric" {
                    let rows: Vec<(&str, Decimal)> = readable
                        .iter()
                        .filter_map(|listed| match listed.row.values.get(property) {
                            Some(RuntimeValue::Number(number)) => {
                                Some((listed.row.id.as_str(), *number))
                            }
                            Some(
                                RuntimeValue::Null
                                | RuntimeValue::Bool(_)
                                | RuntimeValue::String(_)
                                | RuntimeValue::Array(_)
                                | RuntimeValue::Object(_),
                            )
                            | None => None,
                        })
                        .collect();
                    profile.outliers = outliers(&rows);
                }
                profile
            })
            .collect(),
    })
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "rank assertions"
)]
mod tests {
    use super::*;

    #[test]
    fn far_out_values_are_named_most_extreme_first() {
        let decimal = |text: &str| Decimal::parse(text).unwrap();
        let ids: Vec<String> = (0..13).map(|n| format!("row-{n:02}")).collect();
        let mut rows: Vec<(&str, Decimal)> = ids
            .iter()
            .zip([
                "10", "11", "12", "12.5", "13", "14", "15", "15.5", "16", "17", "18",
            ])
            .map(|(id, value)| (id.as_str(), decimal(value)))
            .collect();
        rows.push((ids[11].as_str(), decimal("720")));
        rows.push((ids[12].as_str(), decimal("-200")));
        let found = outliers(&rows).unwrap();
        // q1 = 12 and q3 = 16 by nearest rank (ranks 4 and 10 of 13); IQR 4; fences 0 and 28.
        assert_eq!(
            (&found.q1, &found.q3, &found.lower_fence, &found.upper_fence),
            (
                &RuntimeValue::Number(decimal("12")),
                &RuntimeValue::Number(decimal("16")),
                &RuntimeValue::Number(decimal("0")),
                &RuntimeValue::Number(decimal("28"))
            )
        );
        assert_eq!(found.count, 2);
        assert_eq!(found.examples[0].id, "row-11");
        assert_eq!(found.examples[1].id, "row-12");
        assert!(outliers(&[]).is_none());
    }

    #[test]
    fn nearest_rank_quantiles_never_interpolate() {
        let sorted: Vec<u32> = (1..=10).collect();
        assert_eq!(at_rank(&sorted, 0), Some(1));
        assert_eq!(at_rank(&sorted, 25), Some(3));
        assert_eq!(at_rank(&sorted, 50), Some(5));
        assert_eq!(at_rank(&sorted, 75), Some(8));
        assert_eq!(at_rank(&sorted, 100), Some(10));
        assert_eq!(at_rank(&[7_u32], 50), Some(7));
        assert_eq!(at_rank::<u32>(&[], 50), None);
    }

    /// A nullable column with gaps: nulls are counted, and every statistic is
    /// over the present values only (a null is never 0, never a distinct
    /// value, never in a bin), whether it arrives missing or as a stored null.
    #[test]
    fn nulls_are_counted_and_excluded_from_every_statistic() {
        let number = |text: &str| RuntimeValue::Number(norn_ir::Decimal::parse(text).unwrap());
        let (one, five, nine, null) = (number("1"), number("5"), number("9"), RuntimeValue::Null);
        let values = [
            Some(&one),
            None,
            Some(&five),
            Some(&null),
            Some(&nine),
            None,
        ];
        let profile = profile_property("reading", "numeric", &values);
        assert_eq!((profile.count, profile.nulls, profile.distinct), (3, 3, 3));
        assert_eq!(
            (profile.min, profile.max),
            (Some(one.clone()), Some(nine.clone()))
        );
        assert_eq!(
            profile
                .quantiles
                .iter()
                .find(|q| q.percent == 50)
                .map(|q| q.value.clone()),
            Some(five)
        );
        let histogram = profile.histogram.unwrap();
        assert_eq!(histogram.bins.iter().map(|bin| bin.count).sum::<u64>(), 3);
        assert!(
            histogram
                .bins
                .iter()
                .all(|bin| bin.lower != null && bin.upper != null)
        );
        // All null: counted, and no statistic at all.
        let empty = profile_property("reading", "numeric", &[None, Some(&null)]);
        assert_eq!((empty.count, empty.nulls, empty.distinct), (0, 2, 0));
        assert!(
            empty.min.is_none()
                && empty.max.is_none()
                && empty.quantiles.is_empty()
                && empty.histogram.is_none()
        );
    }

    #[test]
    fn equal_population_bins_cover_every_value_once() {
        let sorted: Vec<RuntimeValue> = (0..23)
            .map(|n| RuntimeValue::Number(norn_ir::Decimal::parse(&format!("{n}.5")).unwrap()))
            .collect();
        let histogram = numeric_histogram(&sorted);
        let total: u64 = histogram.bins.iter().map(|bin| bin.count).sum();
        assert_eq!(total, 23);
        assert_eq!(histogram.bins[0].lower, sorted[0]);
        assert_eq!(histogram.bins.last().unwrap().upper, sorted[22]);
    }
}
