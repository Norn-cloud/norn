//! MANIFEST-LAWS-1: how a [`RowPredicate`] types over a table and what it
//! reads; both engines and the cell type a landing's rules by this one code.

use super::{ColumnKind, MAX_PREDICATE_SCALE, PredicateKind, RowPredicate, TableSchema};

const MAX_PREDICATE_DEPTH: u32 = 128;

/// `0001-01-01T00:00:00Z` in UTC seconds (ZEN `Temporal` spans years `0001..=9999`).
pub(crate) const FIRST_ZEN_SECOND: i64 = -62_135_596_800;
/// `9999-12-31T23:59:59Z` in UTC seconds.
pub(crate) const LAST_ZEN_SECOND: i64 = 253_402_300_799;

/// Exact UTC midnight seconds for a ZEN `date(text)` literal (`YYYY-MM-DD`, years `0001..=9999`).
#[must_use]
pub(crate) fn zen_date_key(text: &str) -> Option<i64> {
    if text.len() != 10 || text.get(..4)? == "0000" {
        return None;
    }
    let (secs, nanos) = norn_checker::metric::compute::contribution::instant_key(text, true)?;
    (nanos == 0 && (FIRST_ZEN_SECOND..=LAST_ZEN_SECOND).contains(&secs)).then_some(secs)
}

/// Exact UTC `(secs, nanos)` for a ZEN `timestamp(text)` literal (RFC 3339, local & UTC years `0001..=9999`).
#[must_use]
pub(crate) fn zen_timestamp_key(text: &str) -> Option<(i64, u32)> {
    if text.len() < 20 || text.get(..4)? == "0000" {
        return None;
    }
    let (secs, nanos) = norn_checker::metric::compute::contribution::instant_key(text, false)?;
    (FIRST_ZEN_SECOND..=LAST_ZEN_SECOND)
        .contains(&secs)
        .then_some((secs, nanos))
}

/// Order-isomorphic total nanoseconds since the Unix epoch for `t in i64, 0 <= n < 10^9`.
#[must_use]
pub(crate) const fn instant_nanos(secs: i64, nanos: u32) -> i128 {
    (secs as i128)
        .saturating_mul(1_000_000_000)
        .saturating_add(nanos as i128)
}

/// Merge two branch kinds (of `Conditional` or `Coalesce`).
fn merge_kinds(left: PredicateKind, right: PredicateKind) -> Option<PredicateKind> {
    use PredicateKind as K;
    match (left, right) {
        (K::Null, other) | (other, K::Null) => Some(other),
        (K::Number(a), K::Number(b)) => Some(K::Number(a.max(b))),
        (K::DateText, K::Text)
        | (K::Text, K::DateText)
        | (K::TimestampText, K::Text)
        | (K::Text, K::TimestampText) => Some(K::Text),
        (a, b) if a == b => Some(a),
        _ => None,
    }
}

/// Whether `node` is an exact temporal argument to `date(...)` (`is_date = true`)
/// or `timestamp(...)` (`is_date = false`), guaranteeing non-null output whenever
/// `null_allowed == false` and all read columns are present.
fn valid_temporal_arg(
    node: &RowPredicate,
    table: &TableSchema,
    is_date: bool,
    null_allowed: bool,
    depth: u32,
) -> bool {
    let Some(next) = depth
        .checked_add(1)
        .filter(|next| *next <= MAX_PREDICATE_DEPTH)
    else {
        return false;
    };
    match node {
        RowPredicate::Null => null_allowed,
        RowPredicate::Text(lit) => {
            if is_date {
                zen_date_key(lit).is_some()
            } else {
                zen_timestamp_key(lit).is_some()
            }
        }
        RowPredicate::Column(id) => table.columns.iter().find(|c| c.id == *id).is_some_and(|c| {
            if is_date {
                c.kind == ColumnKind::Date
            } else {
                c.kind == ColumnKind::Timestamp
            }
        }),
        RowPredicate::Coalesce(a, b) => {
            valid_temporal_arg(a, table, is_date, true, next)
                && valid_temporal_arg(b, table, is_date, null_allowed, next)
        }
        RowPredicate::Conditional(c, a, b) => {
            c.kind_in(table, false, next) == Some(PredicateKind::Bool)
                && valid_temporal_arg(a, table, is_date, null_allowed, next)
                && valid_temporal_arg(b, table, is_date, null_allowed, next)
        }
        RowPredicate::Bool(_)
        | RowPredicate::Number(_)
        | RowPredicate::Not(_)
        | RowPredicate::Negate(_)
        | RowPredicate::Add(..)
        | RowPredicate::Subtract(..)
        | RowPredicate::Multiply(..)
        | RowPredicate::Equal(..)
        | RowPredicate::NotEqual(..)
        | RowPredicate::Less(..)
        | RowPredicate::LessOrEqual(..)
        | RowPredicate::Greater(..)
        | RowPredicate::GreaterOrEqual(..)
        | RowPredicate::And(..)
        | RowPredicate::Or(..)
        | RowPredicate::Date(_)
        | RowPredicate::Timestamp(_) => false,
    }
}

impl RowPredicate {
    /// MANIFEST-LAWS-1: the kind of this node over `table`, when every node is
    /// typed the way both the per-row evaluator and the engine compute it
    /// exactly: numbers compare and compute only with numbers, civil dates
    /// (`date(...)`) and UTC instants (`timestamp(...)`) order and compare with
    /// their own kind, raw text/date-text/timestamp-text/Booleans only compare
    /// for equality, `null` only appears where nullable (`==`, `!=`, LHS of
    /// `??`, or nullable conditional branches), and no scale passes
    /// [`MAX_PREDICATE_SCALE`]. `None` for anything else; such a check is
    /// left to the per-row evaluator.
    #[must_use]
    pub fn kind(&self, table: &TableSchema) -> Option<PredicateKind> {
        self.kind_in(table, false, 0)
    }

    /// Like [`Self::kind`], allowing `null` at the root (for subexpressions
    /// already typed in a nullable position such as LHS of `??`).
    #[must_use]
    pub(crate) fn kind_nullable(&self, table: &TableSchema) -> Option<PredicateKind> {
        self.kind_in(table, true, 0)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "single exhaustive kind table over RowPredicate keeps typing auditable"
    )]
    fn kind_in(
        &self,
        table: &TableSchema,
        null_allowed: bool,
        depth: u32,
    ) -> Option<PredicateKind> {
        use PredicateKind as K;
        let next = depth
            .checked_add(1)
            .filter(|next| *next <= MAX_PREDICATE_DEPTH)?;
        let number = |a: &Self, b: &Self| match (
            a.kind_in(table, false, next)?,
            b.kind_in(table, false, next)?,
        ) {
            (K::Number(x), K::Number(y)) => Some((x, y)),
            _ => None,
        };
        let kind = match self {
            Self::Null => null_allowed.then_some(K::Null)?,
            Self::Bool(_) => K::Bool,
            Self::Number(text) => {
                let exact = norn_ir::Decimal::parse(text).ok()?;
                if exact.canonical() != *text {
                    return None;
                }
                K::Number(u32::try_from(text.split_once('.').map_or(0, |(_, f)| f.len())).ok()?)
            }
            Self::Text(_) => K::Text,
            Self::Column(id) => match table.columns.iter().find(|c| c.id == *id)?.kind {
                ColumnKind::Integer => K::Number(0),
                ColumnKind::Decimal { scale } => K::Number(scale),
                ColumnKind::Text => K::Text,
                ColumnKind::Boolean => K::Bool,
                ColumnKind::Date => K::DateText,
                ColumnKind::Timestamp => K::TimestampText,
                ColumnKind::Opaque => return None,
            },
            Self::Not(inner) => {
                (inner.kind_in(table, false, next)? == K::Bool).then_some(K::Bool)?
            }
            Self::Negate(inner) => match inner.kind_in(table, false, next)? {
                K::Number(scale) => K::Number(scale),
                K::Null
                | K::Bool
                | K::Text
                | K::DateText
                | K::TimestampText
                | K::Date
                | K::Timestamp => return None,
            },
            Self::Add(a, b) | Self::Subtract(a, b) => {
                let (x, y) = number(a, b)?;
                K::Number(x.max(y))
            }
            Self::Multiply(a, b) => {
                let (x, y) = number(a, b)?;
                K::Number(x.checked_add(y)?)
            }
            Self::Equal(a, b) | Self::NotEqual(a, b) => {
                let (x, y) = (a.kind_in(table, true, next)?, b.kind_in(table, true, next)?);
                let same = match (x, y) {
                    (K::Number(_), K::Number(_))
                    | (K::Null, _)
                    | (_, K::Null)
                    | (K::DateText, K::Text)
                    | (K::Text, K::DateText)
                    | (K::TimestampText, K::Text)
                    | (K::Text, K::TimestampText) => true,
                    (x, y) => x == y,
                };
                same.then_some(K::Bool)?
            }
            Self::Less(a, b)
            | Self::LessOrEqual(a, b)
            | Self::Greater(a, b)
            | Self::GreaterOrEqual(a, b) => match (
                a.kind_in(table, false, next)?,
                b.kind_in(table, false, next)?,
            ) {
                (K::Number(_), K::Number(_))
                | (K::Date, K::Date)
                | (K::Timestamp, K::Timestamp) => K::Bool,
                _ => return None,
            },
            Self::And(a, b) | Self::Or(a, b) => (a.kind_in(table, false, next)? == K::Bool
                && b.kind_in(table, false, next)? == K::Bool)
                .then_some(K::Bool)?,
            Self::Coalesce(a, b) => {
                let left = a.kind_in(table, true, next)?;
                let right = b.kind_in(table, null_allowed, next)?;
                merge_kinds(left, right)?
            }
            Self::Conditional(test, then, otherwise) => {
                if test.kind_in(table, false, next)? != K::Bool {
                    return None;
                }
                let left = then.kind_in(table, null_allowed, next)?;
                let right = otherwise.kind_in(table, null_allowed, next)?;
                merge_kinds(left, right)?
            }
            Self::Date(inner) => {
                valid_temporal_arg(inner, table, true, false, next).then_some(K::Date)?
            }
            Self::Timestamp(inner) => {
                valid_temporal_arg(inner, table, false, false, next).then_some(K::Timestamp)?
            }
        };
        match kind {
            K::Number(scale) if scale > MAX_PREDICATE_SCALE => None,
            kind @ (K::Null
            | K::Bool
            | K::Text
            | K::Number(_)
            | K::DateText
            | K::TimestampText
            | K::Date
            | K::Timestamp) => Some(kind),
        }
    }

    /// Every column the predicate reads, in order of first read.
    #[must_use]
    pub fn reads(&self) -> Vec<String> {
        let mut out = Vec::new();
        self.collect_reads(&mut out);
        out
    }

    fn collect_reads(&self, out: &mut Vec<String>) {
        match self {
            Self::Null | Self::Bool(_) | Self::Number(_) | Self::Text(_) => {}
            Self::Column(id) => {
                if !out.contains(id) {
                    out.push(id.clone());
                }
            }
            Self::Not(inner) | Self::Negate(inner) | Self::Date(inner) | Self::Timestamp(inner) => {
                inner.collect_reads(out)
            }
            Self::Add(a, b)
            | Self::Subtract(a, b)
            | Self::Multiply(a, b)
            | Self::Equal(a, b)
            | Self::NotEqual(a, b)
            | Self::Less(a, b)
            | Self::LessOrEqual(a, b)
            | Self::Greater(a, b)
            | Self::GreaterOrEqual(a, b)
            | Self::And(a, b)
            | Self::Or(a, b)
            | Self::Coalesce(a, b) => {
                a.collect_reads(out);
                b.collect_reads(out);
            }
            Self::Conditional(c, a, b) => {
                c.collect_reads(out);
                a.collect_reads(out);
                b.collect_reads(out);
            }
        }
    }
}
