//! Exact column profiles on the adapter: counts, distinct values, the values
//! at exact nearest ranks (numbers in numeric order, instants in the order of
//! their original text, as the exact profile orders them), day or month
//! labels of the original text, and Tukey's far-out values from fences
//! computed in checked decimal arithmetic.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use super::super::{
    ColumnKind, ColumnProfile, OutlierSummary, PROFILE_DAY_BINS, PROFILE_OUTLIER_EXAMPLES,
    PROFILE_OUTLIER_K, ProfileRequest, ProfileResponse, Value, nearest_rank, profile_ranks,
};
use super::query::{Row, Scope};
use super::values::{Cell, Dec};
use super::{Engine, Outcome, Tenant};

/// Exact column profiles of a source, naming the snapshot read.
pub(super) fn profile(engine: &Engine, tenant: &mut Tenant, request: &ProfileRequest) -> Outcome<ProfileResponse> {
    let scope = Scope::new(engine, tenant, &request.source, Some(&request.model_pin))?;
    scope.verify_rows(&request.source, false)?;
    let population: Vec<&Row> = scope.population().collect();
    let rows = u64::try_from(population.len()).unwrap_or(u64::MAX);
    let columns = request
        .columns
        .iter()
        .map(|id| column(&scope, &population, id, rows))
        .collect::<Outcome<_>>()?;
    Ok(ProfileResponse {
        rows,
        quarantined: u64::try_from(scope.excluded_present().len()).unwrap_or(u64::MAX),
        columns,
        provenance: scope.provenance(&request.tenant),
    })
}

/// The profile order of two present cells: instants by their text.
fn profile_order(a: &Cell, b: &Cell) -> Ordering {
    match (a, b) {
        (Cell::Instant { text: x, .. }, Cell::Instant { text: y, .. }) => x.as_bytes().cmp(y.as_bytes()),
        (Cell::Instant { text: x, .. }, Cell::Text(y)) | (Cell::Text(x), Cell::Instant { text: y, .. }) => {
            x.as_bytes().cmp(y.as_bytes())
        }
        _ => a.order(b),
    }
}

fn column(scope: &Scope, population: &[&Row], id: &str, rows: u64) -> Outcome<ColumnProfile> {
    let (index, found) = scope.column(id)?;
    let kind = found.kind;
    let temporal = matches!(kind, ColumnKind::Timestamp | ColumnKind::Date);
    let numeric = matches!(kind, ColumnKind::Integer | ColumnKind::Decimal { .. });
    let mut present: Vec<(&Row, &Cell)> = population
        .iter()
        .filter_map(|row| {
            row.cells
                .get(index)
                .filter(|cell| cell.is_present())
                .map(|cell| (*row, cell))
        })
        .collect();
    let count = u64::try_from(present.len()).unwrap_or(u64::MAX);
    let mut distinct: Vec<&Cell> = present.iter().map(|(_, cell)| *cell).collect();
    distinct.sort_by(|a, b| profile_order(a, b));
    distinct.dedup_by(|a, b| profile_order(a, b).is_eq());
    let mut ranks = BTreeMap::new();
    if (numeric || temporal) && count > 0 {
        present.sort_by(|(_, a), (_, b)| profile_order(a, b));
        for rank in profile_ranks(count) {
            let at = usize::try_from(rank.saturating_sub(1)).unwrap_or(usize::MAX);
            if let Some(value) = present.get(at).and_then(|(_, cell)| cell.present(kind)) {
                ranks.insert(rank, value);
            }
        }
    }
    let labels = (temporal && count > 0).then(|| labels(&present));
    let outliers = match kind {
        ColumnKind::Integer if count > 0 => outliers(&present, 0, &ranks, count),
        ColumnKind::Decimal { scale } if count > 0 => outliers(&present, scale, &ranks, count),
        ColumnKind::Integer
        | ColumnKind::Decimal { .. }
        | ColumnKind::Text
        | ColumnKind::Boolean
        | ColumnKind::Timestamp
        | ColumnKind::Date
        | ColumnKind::Opaque => None,
    };
    Ok(ColumnProfile {
        column: id.to_owned(),
        count,
        nulls: rows.saturating_sub(count),
        distinct: u64::try_from(distinct.len()).unwrap_or(u64::MAX),
        ranks,
        labels,
        outliers,
    })
}

/// Day labels (the text's first 10 characters) when there are at most
/// [`PROFILE_DAY_BINS`], else month labels (the first 7).
fn labels(present: &[(&Row, &Cell)]) -> (String, Vec<(String, u64)>) {
    let read = |width: usize| -> Vec<(String, u64)> {
        let mut counts: BTreeMap<Vec<u8>, (String, u64)> = BTreeMap::new();
        for (_, cell) in present {
            let label: String = cell.as_str().unwrap_or_default().chars().take(width).collect();
            let entry = counts
                .entry(label.as_bytes().to_vec())
                .or_insert((label, 0));
            entry.1 = entry.1.saturating_add(1);
        }
        counts.into_values().collect()
    };
    let days = read(10);
    if days.len() <= PROFILE_DAY_BINS {
        ("day".to_owned(), days)
    } else {
        ("month".to_owned(), read(7))
    }
}

/// Tukey's far-out values: fences from the nearest-rank quartiles in exact
/// decimal arithmetic; none when a fence cannot be held at the column's scale.
fn outliers(
    present: &[(&Row, &Cell)],
    scale: u32,
    ranks: &BTreeMap<u64, Value>,
    count: u64,
) -> Option<OutlierSummary> {
    let quartile = |percent| match ranks.get(&nearest_rank(percent, count)) {
        Some(Value::Number(text)) => norn_ir::Decimal::parse(text).ok(),
        _ => None,
    };
    let (q1, q3) = (quartile(25)?, quartile(75)?);
    let iqr = q3.checked_sub(q1)?;
    let span = (1..PROFILE_OUTLIER_K).try_fold(iqr, |total, _| total.checked_add(iqr))?;
    let (lower, upper) = (q1.checked_sub(span)?.canonical(), q3.checked_add(span)?.canonical());
    if [&lower, &upper]
        .iter()
        .any(|fence| norn_ir::Decimal::parse(fence).map_or(true, |d| d.scale() > scale))
    {
        return None;
    }
    let (low, high) = (Dec::cast(&lower, scale)?, Dec::cast(&upper, scale)?);
    let mut far: Vec<(Dec, &Row, Dec)> = present
        .iter()
        .filter_map(|(row, cell)| match cell {
            Cell::Dec(value) if value.compare(low).is_lt() => {
                Some((distance(low, *value)?, *row, *value))
            }
            Cell::Dec(value) if value.compare(high).is_gt() => {
                Some((distance(*value, high)?, *row, *value))
            }
            Cell::Dec(_) | Cell::Null | Cell::Text(_) | Cell::Bool(_) | Cell::Instant { .. } => {
                None
            }
        })
        .collect();
    let total = u64::try_from(far.len()).unwrap_or(u64::MAX);
    far.sort_by(|a, b| {
        b.0.compare(a.0)
            .then_with(|| a.1.id.as_bytes().cmp(b.1.id.as_bytes()))
    });
    let examples = far
        .into_iter()
        .take(usize::try_from(PROFILE_OUTLIER_EXAMPLES).unwrap_or(0))
        .map(|(_, row, value)| (row.id.clone(), value.canonical()))
        .collect();
    Some(OutlierSummary {
        lower,
        upper,
        count: total,
        examples,
    })
}

/// `a - b`, exactly.
fn distance(a: Dec, b: Dec) -> Option<Dec> {
    a.checked_add(Dec {
        m: b.m.checked_neg()?,
        s: b.s,
    })
}
