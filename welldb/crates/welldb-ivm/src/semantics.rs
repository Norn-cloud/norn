//! Ordered row capture, exact scalar checks and canonical result finalization.
use crate::{
    Aggregate, Bucket, BucketAgg, CaptureKey, ClassId, ExactSum, GroupKey, JoinedTarget, Lineage,
    Member, MetricError, MetricResult, Placement, Plan, Property, RecordView, RowRef, Value,
    VisibilityOutcome,
};
use std::collections::BTreeMap;
/// Parse admitted UTC ISO tokens exactly, without the native i64 nanosecond limit.
#[must_use]
#[allow(
    clippy::arithmetic_side_effects,
    reason = "validated 4-digit Gregorian components are bounded far inside i128"
)]
pub fn parse_time(text: &str) -> Option<i128> {
    let num = |a, b| {
        let token = text.get(a..b)?;
        if !token.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        token.parse::<i128>().ok()
    };
    if text.get(4..5)? != "-" || text.get(7..8)? != "-" {
        return None;
    }
    let (year, month, day) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    if !(0..=9999).contains(&year) || !(1..=12).contains(&month) {
        return None;
    }
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days = match month {
        2 => {
            if leap {
                29
            } else {
                28
            }
        }
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    if day < 1 || day > days {
        return None;
    }
    let y = year - i128::from(month <= 2);
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = month + if month > 2 { -3 } else { 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let epoch_days = era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468;
    let mut nanos = epoch_days * 86400 * 1_000_000_000;
    if text.len() == 10 {
        return Some(nanos);
    }
    if text.get(10..11)? != "T" || text.get(13..14)? != ":" || text.get(16..17)? != ":" {
        return None;
    }
    let (hour, minute, second) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    if hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    nanos += (hour * 3600 + minute * 60 + second) * 1_000_000_000;
    let (body, offset) = if let Some(body) = text.strip_suffix('Z') {
        (body, 0)
    } else {
        let offset_start = text.len().checked_sub(6)?;
        let zone = text.get(offset_start..)?;
        if zone == "-00:00" || zone.get(3..4)? != ":" {
            return None;
        }
        let sign = match zone.get(..1)? {
            "+" => 1,
            "-" => -1,
            _ => return None,
        };
        let offset_hour = num(offset_start + 1, offset_start + 3)?;
        let offset_minute = num(offset_start + 4, offset_start + 6)?;
        if offset_hour > 23 || offset_minute > 59 {
            return None;
        }
        (
            text.get(..offset_start)?,
            sign * (offset_hour * 3600 + offset_minute * 60) * 1_000_000_000,
        )
    };
    let suffix = body.get(19..)?;
    if !suffix.is_empty() {
        let fraction = suffix.strip_prefix('.')?;
        if fraction.is_empty()
            || fraction.len() > 9
            || !fraction.bytes().all(|b| b.is_ascii_digit())
        {
            return None;
        }
        let places = u32::try_from(9_usize.checked_sub(fraction.len())?).ok()?;
        nanos += fraction.parse::<i128>().ok()? * 10_i128.pow(places);
    }
    nanos -= offset;
    // UTC normalization must retain a four-digit year, as Norn requires.
    if !(-62_167_219_200_000_000_000..253_402_300_800_000_000_000).contains(&nanos) {
        return None;
    }
    Some(nanos)
}

/// Ordered row refusal, suitable for an incrementally maintained index.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct RowRefusal {
    /// Capture/execution location.
    pub key: CaptureKey,
    /// Whole-result refusal kind.
    pub kind: MetricError,
}
/// Minimum-key index; insert/remove per changed row, no recomputation needed.
pub type RefusalIndex = BTreeMap<CaptureKey, MetricError>;
/// One visible selected measure's exact contribution.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Contribution {
    /// Group (None when ungrouped).
    pub group: GroupKey,
    /// Single-row aggregate at plan scale.
    pub aggregate: BucketAgg,
    /// Last-write lineage.
    pub member: Member,
}
/// Pure capture outcome. The optional row contains all captured values, even
/// when outside the window, for the test-side native classifier.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Captured {
    /// Visibility gate outcome in field evaluation order.
    pub visibility: VisibilityOutcome,
    /// Selected contribution, if valid and in-window/unfiltered.
    pub contribution: Option<Contribution>,
    /// First refusal for this row; capture refusals globally precede validity.
    pub refusal: Option<RowRefusal>,
    /// Values in field order after successful capture (not necessarily valid).
    pub row: Option<Vec<Value>>,
}
/// Finalize independently maintained refusal and bucket indices (§24.3).
/// Row refusals precede per-bucket checks; within group order disclosure precedes
/// exact Decimal representability. Empty grouped buckets are omitted.
/// # Errors
/// The first canonical refusal, including the strict u64 population bound.
pub fn finalize(
    plan: &Plan,
    row_refusals: &RefusalIndex,
    buckets: &BTreeMap<GroupKey, BucketAgg>,
) -> MetricResult {
    if let Some((_, kind)) = row_refusals.first_key_value() {
        return Err(kind.clone());
    }
    let empty = BTreeMap::from([(None, BucketAgg::default())]);
    let buckets = if plan.group.is_none() && buckets.is_empty() {
        &empty
    } else {
        buckets
    };
    let mut result = Vec::new();
    for (group, aggregate) in buckets {
        if plan.group.is_some() && aggregate.population == 0 {
            continue;
        }
        if aggregate.population == u64::MAX {
            return Err(MetricError::PopulationOverflow);
        }
        if aggregate.population < plan.min_group_size {
            return Err(MetricError::Disclosure);
        }
        let (scale, currency) = match &plan.aggregate {
            Aggregate::Count => (0, None),
            Aggregate::Sum {
                scale, currency, ..
            } => (*scale, currency.as_ref().map(|(_, code)| code.clone())),
        };
        result.push(Bucket {
            group: group.clone(),
            value: aggregate.sum.decimal(scale)?,
            population: aggregate.population,
            currency,
        });
    }
    Ok(result)
}
/// Lineage from retained contributions (does not consult output bucket policy).
#[must_use]
pub fn lineage(contributions: &[Contribution], group: &GroupKey) -> Lineage {
    let mut members: Lineage = contributions
        .iter()
        .filter(|c| &c.group == group)
        .map(|c| c.member.clone())
        .collect();
    members.sort_by(|a, b| a.key.cmp(&b.key));
    members
}

fn visible<R: RecordView>(row: RowRef<'_, R>, class: &ClassId) -> bool {
    !row.tombstoned && row.record.visible(class)
}
fn refused<R>(
    measure: RowRef<'_, R>,
    phase: u8,
    field: usize,
    kind: MetricError,
    row: Option<Vec<Value>>,
) -> Captured {
    Captured {
        visibility: VisibilityOutcome::Visible,
        contribution: None,
        refusal: Some(RowRefusal {
            key: CaptureKey {
                phase,
                measure: measure.key.clone(),
                field,
            },
            kind,
        }),
        row,
    }
}
/// Capture and validate one row, respecting ordered hidden-target/mask checks.
/// Backends call this inside their own map/join; it has no storage or IO.
#[must_use]
#[allow(
    clippy::too_many_lines,
    reason = "canonical ordered capture/refusal phases stay auditable together"
)]
pub fn capture<R: RecordView>(
    plan: &Plan,
    class: &ClassId,
    measure: RowRef<'_, R>,
    target: JoinedTarget<'_, R>,
) -> Captured {
    let empty = |visibility| Captured {
        visibility,
        contribution: None,
        refusal: None,
        row: None,
    };
    if !visible(measure, class) {
        return empty(VisibilityOutcome::HiddenMeasure);
    }
    let mut values = Vec::with_capacity(plan.fields.len());
    for (i, field) in plan.fields.iter().enumerate() {
        let owner = if field.placement == Placement::Measure {
            measure
        } else {
            match target {
                JoinedTarget::Row(row) => {
                    if !visible(row, class) {
                        return empty(VisibilityOutcome::HiddenTarget);
                    }
                    row
                }
                JoinedTarget::Missing | JoinedTarget::NotRequired => {
                    return refused(measure, 0, i, MetricError::SnapshotLink, None);
                }
            }
        };
        if owner.record.masked(class, &field.property) {
            return refused(measure, 0, i, MetricError::MarkingDenied, None);
        }
        values.push(
            owner
                .record
                .value(&field.property)
                .cloned()
                .unwrap_or(Value::Null),
        );
    }
    for (i, (field, value)) in plan.fields.iter().zip(&values).enumerate() {
        if !valid(&field.kind, value) {
            return refused(measure, 1, i, MetricError::InvalidField, Some(values));
        }
    }
    if let Aggregate::Sum {
        currency: Some((index, code)),
        ..
    } = &plan.aggregate
        && values.get(*index) != Some(&Value::Text(code.clone()))
    {
        return refused(
            measure,
            1,
            plan.fields.len(),
            MetricError::MixedCurrency,
            Some(values),
        );
    }
    let mut result = Captured {
        visibility: VisibilityOutcome::Visible,
        contribution: None,
        refusal: None,
        row: Some(values.clone()),
    };
    let time = values.get(plan.time).and_then(value_time);
    if time.is_none_or(|t| t < plan.window.0 || t >= plan.window.1) {
        return result;
    }
    if plan
        .filters
        .iter()
        .any(|f| (values.get(f.field) == Some(&Value::Text(f.value.clone()))) != f.equal)
    {
        return result;
    }
    let sum = match &plan.aggregate {
        Aggregate::Count => ExactSum::from_i128(1),
        Aggregate::Sum { amount, scale, .. } => match values.get(*amount) {
            Some(Value::Decimal(d)) => match ExactSum::scaled(*d, *scale) {
                Ok(s) => s,
                Err(e) => return refused(measure, 1, *amount, e, Some(values)),
            },
            Some(Value::Null | Value::Bool(_) | Value::Text(_)) | None => {
                return refused(measure, 1, *amount, MetricError::InvalidField, Some(values));
            }
        },
    };
    let group = plan.group.and_then(|i| values.get(i)).and_then(|v| {
        if let Value::Text(t) = v {
            Some(t.clone())
        } else {
            None
        }
    });
    result.contribution = Some(Contribution {
        group,
        aggregate: BucketAgg { sum, population: 1 },
        member: Member {
            key: measure.key.clone(),
            seq: measure.record.seq(),
            hash: measure.record.hash().to_owned(),
        },
    });
    result
}
fn valid(kind: &Property, value: &Value) -> bool {
    if let Property::Constrained { base, constraints } = kind {
        return valid(base, value)
            && constraints
                .iter()
                .all(|rules| crate::constraints::valid(base.base(), rules, value));
    }
    match (kind, value) {
        (Property::Text, Value::Text(_)) => true,
        (Property::Enum(allowed), Value::Text(v)) => allowed.contains(v),
        (Property::Date, Value::Text(v)) => v.len() == 10 && parse_time(v).is_some(),
        (Property::Timestamp, Value::Text(v)) => v.len() > 10 && parse_time(v).is_some(),
        (Property::Decimal { scale }, Value::Decimal(d)) => d.scale() <= *scale,
        (
            Property::Constrained { .. }
            | Property::Text
            | Property::Enum(_)
            | Property::Date
            | Property::Timestamp
            | Property::Decimal { .. },
            Value::Null | Value::Bool(_) | Value::Decimal(_) | Value::Text(_),
        ) => false,
    }
}
fn value_time(value: &Value) -> Option<i128> {
    if let Value::Text(v) = value {
        parse_time(v)
    } else {
        None
    }
}
