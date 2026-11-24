//! QUERY-ENGINE-1 / MEMBERSHIP-DIGEST-1 on the adapter: a lowered question's
//! projection, per row its bucket, operand coefficients (exact millionths,
//! `|c| <= 9e18`), null flags and whether the exact engine would refuse on
//! reaching it; then per bucket (nulls first) the sum, the sum of magnitudes
//! and the null count of each operand, computed independently in `i128` so
//! cancellation never hides an overflow, and the digest over the members in
//! identity order, whatever order the scan returned them in.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use unicode_normalization::UnicodeNormalization as _;

use super::super::{
    BucketTotals, ColumnKind, Dimension, EvaluateRequest, EvaluateResponse, Grain, GroupKey,
    Lowered, MemberRow, MembersRequest, MembersResponse, MembershipDigest, Source,
    TIME_LINK_COLUMN,
};
use super::query::{self, Linked, Row, Scope, TypedFilter};
use super::values::{Cell, Dec, period_start};
use super::{Engine, Outcome, Tenant, UNSUPPORTED, engine_error, refusal};

/// A dimension's grouping value: equal values share a bucket.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Group {
    Null,
    Text(String),
    Number(Dec),
    Bool(bool),
    Day(i64),
}

impl Group {
    /// Bucket order: nulls first, then each kind's own order.
    fn order(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Self::Null, Self::Null) => Ordering::Equal,
            (Self::Null, _) => Ordering::Less,
            (_, Self::Null) => Ordering::Greater,
            (Self::Text(a), Self::Text(b)) => a.as_bytes().cmp(b.as_bytes()),
            (Self::Number(a), Self::Number(b)) => a.compare(*b),
            (Self::Bool(a), Self::Bool(b)) => a.cmp(b),
            (Self::Day(a), Self::Day(b)) => a.cmp(b),
            (a, b) => format!("{a:?}").cmp(&format!("{b:?}")),
        }
    }
}

fn nfc(text: &str) -> String {
    text.nfc().collect()
}

/// How one dimension groups.
enum Dim {
    Label,
    Period { index: Option<usize>, grain: Grain },
    Text { index: usize, normalized: bool },
    Number(usize),
    Bool(usize),
}

/// One summed operand.
struct Op {
    index: usize,
    filters: Vec<TypedFilter>,
}

/// A lowered question typed against its scope.
struct Plan {
    filters: Vec<TypedFilter>,
    /// The window's column (`None`: the linked time).
    window: Option<usize>,
    start: (i64, u32),
    end: (i64, u32),
    dims: Vec<Dim>,
    ops: Vec<Op>,
    linked: bool,
    as_of: bool,
}

fn instant_or_link(scope: &Scope, column: &str, linked: bool) -> Outcome<Option<usize>> {
    if linked && column == TIME_LINK_COLUMN {
        return Ok(None);
    }
    let (index, found) = scope.column(column)?;
    if matches!(found.kind, ColumnKind::Timestamp | ColumnKind::Date) {
        Ok(Some(index))
    } else {
        Err(refusal(UNSUPPORTED, "the window is not on an instant"))
    }
}

impl Plan {
    fn build(query: &Lowered, scope: &Scope) -> Outcome<Self> {
        let linked = query.time_link.is_some();
        let window = instant_or_link(scope, &query.window.column, linked)?;
        let filters = query::type_filters(scope, &query.filters)?;
        let mut dims = Vec::new();
        for dimension in &query.dimensions {
            dims.push(match dimension {
                Dimension::Period { .. } if query.as_of.is_some() => Dim::Label,
                Dimension::Period { column, grain } => Dim::Period {
                    index: instant_or_link(scope, column, linked).map_err(|_e| {
                        refusal(UNSUPPORTED, "a period of a value that is not an instant")
                    })?,
                    grain: *grain,
                },
                Dimension::Column { column } => {
                    let (index, found) = scope.column(column)?;
                    match found.kind {
                        ColumnKind::Text | ColumnKind::Timestamp | ColumnKind::Date => Dim::Text {
                            index,
                            normalized: scope.model.is_some(),
                        },
                        ColumnKind::Integer | ColumnKind::Decimal { .. } => Dim::Number(index),
                        ColumnKind::Boolean => Dim::Bool(index),
                        ColumnKind::Opaque => return Err(refusal(UNSUPPORTED, "an opaque bucket")),
                    }
                }
            });
        }
        let mut ops = Vec::new();
        for operand in &query.operands {
            let (index, column) = scope.column(&operand.column)?;
            let filters = query::type_filters(scope, &operand.filters)?;
            if !matches!(column.kind, ColumnKind::Integer | ColumnKind::Decimal { .. }) {
                return Err(refusal(UNSUPPORTED, "a summed column that is not numeric"));
            }
            ops.push(Op { index, filters });
        }
        Ok(Self {
            filters,
            window,
            start: query.window.start,
            end: query.window.end,
            dims,
            ops,
            linked,
            as_of: query.as_of.is_some(),
        })
    }
}

/// One projected row.
struct Projected {
    id: String,
    v: u64,
    groups: Vec<Group>,
    keys: Vec<GroupKey>,
    amounts: Vec<Option<i64>>,
    nulls: Vec<bool>,
    refuse: bool,
    unlinked: bool,
}

impl Projected {
    fn contributes(&self) -> bool {
        self.amounts
            .iter()
            .zip(&self.nulls)
            .any(|(amount, null)| amount.is_some() || *null)
    }
}

fn at_or_after(key: Option<(i64, i32)>, (secs, nanos): (i64, u32), after: bool) -> bool {
    key.is_some_and(|(t, n)| {
        let n = i64::from(n);
        let nanos = i64::from(nanos);
        if after {
            t > secs || (t == secs && n >= nanos)
        } else {
            t < secs || (t == secs && n < nanos)
        }
    })
}

/// Project one base row (with its period label and linked time).
fn project(plan: &Plan, row: &Row, label: Option<&String>, link: &Linked) -> Projected {
    let (q, question_refuses) = query::filters_hold(&plan.filters, &row.cells);
    let time = match plan.window {
        Some(index) => row.cells.get(index).cloned().unwrap_or(Cell::Null),
        None => link.time.clone().unwrap_or(Cell::Null),
    };
    let reach = if plan.as_of {
        q
    } else {
        q && time.is_present()
            && at_or_after(time.instant(), plan.start, true)
            && at_or_after(time.instant(), plan.end, false)
    };
    let mut refuse = question_refuses;
    if !plan.as_of {
        refuse |= if plan.linked {
            (q && link.one && !time.is_present()) || link.many
        } else {
            q && !time.is_present()
        };
    }
    let mut groups = Vec::new();
    let mut keys = Vec::new();
    for dim in &plan.dims {
        let (group, key) = match dim {
            Dim::Label => label.map_or((Group::Null, GroupKey::Null), |label| {
                (Group::Text(label.clone()), GroupKey::Text(label.clone()))
            }),
            Dim::Period { index, grain } => {
                let cell = index.map_or_else(
                    || link.time.clone().unwrap_or(Cell::Null),
                    |index| row.cells.get(index).cloned().unwrap_or(Cell::Null),
                );
                refuse |= reach && !cell.is_present();
                cell.instant().map_or((Group::Null, GroupKey::Null), |(t, _)| {
                    let day = period_start(t, *grain);
                    (Group::Day(day), GroupKey::Period { grain: *grain, day })
                })
            }
            Dim::Text { index, normalized } => {
                row.cells.get(*index).and_then(Cell::as_str).map_or(
                    (Group::Null, GroupKey::Null),
                    |text| {
                        let normal = nfc(text);
                        let shown = if *normalized { normal.clone() } else { text.to_owned() };
                        (Group::Text(normal), GroupKey::Text(shown))
                    },
                )
            }
            Dim::Number(index) => match row.cells.get(*index) {
                Some(Cell::Dec(value)) => (Group::Number(*value), GroupKey::Number(value.canonical())),
                _ => (Group::Null, GroupKey::Null),
            },
            Dim::Bool(index) => match row.cells.get(*index) {
                Some(Cell::Bool(flag)) => (Group::Bool(*flag), GroupKey::Bool(*flag)),
                _ => (Group::Null, GroupKey::Null),
            },
        };
        groups.push(group);
        keys.push(key);
    }
    let mut amounts = Vec::new();
    let mut nulls = Vec::new();
    let mut unlinked = false;
    for op in &plan.ops {
        let (own, own_refuses) = query::filters_hold(&op.filters, &row.cells);
        if plan.linked {
            let none = q && !link.one && !link.many;
            refuse |= none && own_refuses;
            unlinked |= none && own;
        }
        let pass = reach && own;
        let cell = row.cells.get(op.index).unwrap_or(&Cell::Null);
        let coefficient = match cell {
            Cell::Dec(value) => value.millionths(),
            Cell::Null | Cell::Text(_) | Cell::Bool(_) | Cell::Instant { .. } => None,
        };
        refuse |= reach && own_refuses;
        refuse |= pass && cell.is_present() && coefficient.is_none();
        amounts.push(if pass { coefficient } else { None });
        nulls.push(pass && !cell.is_present());
    }
    Projected {
        id: row.id.clone(),
        v: row.v,
        groups,
        keys,
        amounts,
        nulls,
        refuse,
        unlinked,
    }
}

/// The projection of a question over its scope, after the refusals the
/// exact engine gives before reading any value.
fn projection(engine: &Engine, tenant: &Tenant, scope: &Scope, query: &Lowered) -> Outcome<(Vec<Projected>, usize)> {
    if let Some(link) = &query.time_link {
        query::check_link(tenant, scope, link)?;
    }
    let plan = Plan::build(query, scope)?;
    let none = Linked::default();
    let rows: Vec<Projected> = match &query.as_of {
        Some(as_of) => {
            let (picked, untimed) = query::as_of_versions(scope, as_of)?;
            if untimed {
                return Err(refusal(UNSUPPORTED, "a snapshot version without its valid time"));
            }
            picked
                .iter()
                .map(|(label, row)| project(&plan, row, label.as_ref(), &none))
                .collect()
        }
        None => {
            let links = match &query.time_link {
                Some(link) => query::linked_times(engine, tenant, scope, link)?,
                None => BTreeMap::new(),
            };
            scope
                .population()
                .map(|row| project(&plan, row, None, links.get(&row.id).unwrap_or(&none)))
                .collect()
        }
    };
    if rows.iter().any(|row| row.refuse) {
        return Err(refusal(UNSUPPORTED, "a row is outside the bounded profile"));
    }
    Ok((rows, plan.dims.len()))
}

fn bucket_key(keys: &[GroupKey]) -> GroupKey {
    match keys {
        [] => GroupKey::Null,
        [one] => one.clone(),
        many => GroupKey::Tuple(many.to_vec()),
    }
}

fn compare_groups(a: &[Group], b: &[Group]) -> Ordering {
    a.iter()
        .zip(b)
        .map(|(x, y)| x.order(y))
        .find(|order| order.is_ne())
        .unwrap_or(Ordering::Equal)
}

/// Per bucket: totals, magnitudes, null counts, count and digest, and the
/// members when `inline`.
fn buckets(mut rows: Vec<Projected>, arity: usize, inline: bool) -> Outcome<Vec<BucketTotals>> {
    rows.retain(Projected::contributes);
    rows.sort_by(|a, b| {
        compare_groups(&a.groups, &b.groups).then_with(|| a.id.as_bytes().cmp(b.id.as_bytes()))
    });
    let mut out: Vec<BucketTotals> = Vec::new();
    let mut start = 0;
    while let Some(first) = rows.get(start) {
        let end = rows
            .iter()
            .skip(start)
            .position(|row| compare_groups(&row.groups, &first.groups).is_ne())
            .map_or(rows.len(), |offset| start.saturating_add(offset));
        let members: Vec<MemberRow> = rows
            .get(start..end)
            .unwrap_or_default()
            .iter()
            .map(|row| MemberRow {
                id: row.id.clone(),
                version: row.v,
                amounts: row.amounts.clone(),
            })
            .collect();
        let mut totals = vec![0_i128; arity];
        let mut magnitude = vec![0_i128; arity];
        let mut nulls = vec![0_u64; arity];
        for row in rows.get(start..end).unwrap_or_default() {
            for (k, (amount, null)) in row.amounts.iter().zip(&row.nulls).enumerate() {
                if let (Some(amount), Some(total), Some(size)) =
                    (amount, totals.get_mut(k), magnitude.get_mut(k))
                {
                    *total = total
                        .checked_add(i128::from(*amount))
                        .ok_or_else(|| engine_error("a bucket total overflows"))?;
                    *size = size
                        .checked_add(i128::from(amount.unsigned_abs()))
                        .ok_or_else(|| engine_error("a bucket magnitude overflows"))?;
                }
                if let (true, Some(count)) = (*null, nulls.get_mut(k)) {
                    *count = count.saturating_add(1);
                }
            }
        }
        out.push(BucketTotals {
            group: bucket_key(&first.keys),
            totals: totals.iter().map(ToString::to_string).collect(),
            magnitude: magnitude.iter().map(ToString::to_string).collect(),
            nulls,
            count: u64::try_from(members.len()).unwrap_or(u64::MAX),
            digest: MembershipDigest::of(&members),
            members: inline.then_some(members),
        });
        start = end;
    }
    Ok(out)
}

/// QUERY-ENGINE-1: evaluate a lowered question exactly.
pub(super) fn evaluate(engine: &Engine, tenant: &mut Tenant, request: &EvaluateRequest) -> Outcome<EvaluateResponse> {
    let scope = Scope::new(engine, tenant, &request.source, Some(&request.model_pin))?;
    let tenant = &*tenant;
    let population = u64::try_from(scope.all.len()).unwrap_or(u64::MAX);
    scope.verify_rows(&request.source, true)?;
    let excluded = scope.excluded_present();
    let (rows, _dims) = projection(engine, tenant, &scope, &request.query)?;
    let unlinked = u64::try_from(rows.iter().filter(|row| row.unlinked).count()).unwrap_or(u64::MAX);
    let buckets = buckets(
        rows,
        request.query.operands.len(),
        population <= request.inline_members,
    )?;
    Ok(EvaluateResponse {
        population,
        excluded,
        buckets,
        unlinked,
        provenance: scope.provenance(&request.tenant),
    })
}

/// Whether a projected row is in the bucket `group` names.
fn in_bucket(row: &Projected, group: &GroupKey, dims: usize) -> Outcome<bool> {
    let wanted: Vec<&GroupKey> = match (group, dims) {
        (_, 0) => Vec::new(),
        (GroupKey::Tuple(members), n) if n > 1 && members.len() == n => members.iter().collect(),
        (one, 1) => vec![one],
        _ => return Err(refusal(UNSUPPORTED, "a bucket of another shape")),
    };
    let mut matches = true;
    for (key, actual) in wanted.into_iter().zip(&row.groups) {
        matches &= match (key, actual) {
            (GroupKey::Null, actual) => *actual == Group::Null,
            (GroupKey::Bool(flag), Group::Bool(b)) => flag == b,
            (GroupKey::Number(text), Group::Number(value)) => {
                norn_ir::Decimal::parse(text).is_ok_and(|d| d.canonical() == value.canonical())
            }
            (GroupKey::Text(text), Group::Text(normal)) => nfc(text) == *normal,
            (GroupKey::Period { day, .. }, Group::Day(actual)) => day == actual,
            (GroupKey::Bool(_) | GroupKey::Number(_) | GroupKey::Text(_) | GroupKey::Period { .. }, Group::Null) => false,
            _ => return Err(refusal(UNSUPPORTED, "a bucket of another kind")),
        };
    }
    Ok(matches)
}

/// Page one bucket's members in identity order, over exactly the snapshot
/// and generation an evaluate of the same request would read, with every
/// refusal it would give.
pub(super) fn members(engine: &Engine, tenant: &mut Tenant, request: &MembersRequest) -> Outcome<MembersResponse> {
    let expected = match &request.source {
        Source::Mirror { .. } => request.model_pin.as_deref(),
        Source::Dataset { .. } => None,
    };
    let scope = Scope::new(engine, tenant, &request.source, expected)?;
    let tenant = &*tenant;
    scope.verify_rows(&request.source, true)?;
    let (rows, dims) = projection(engine, tenant, &scope, &request.query)?;
    let mut members = Vec::new();
    for row in rows.into_iter().filter(Projected::contributes) {
        if in_bucket(&row, &request.group, dims)?
            && request
                .after
                .as_ref()
                .is_none_or(|after| row.id.as_bytes() > after.as_bytes())
        {
            members.push(MemberRow {
                id: row.id,
                version: row.v,
                amounts: row.amounts,
            });
        }
    }
    members.sort_by(|a, b| a.id.as_bytes().cmp(b.id.as_bytes()));
    members.truncate(usize::try_from(request.limit.min(10_000)).unwrap_or(0));
    Ok(MembersResponse { members })
}
