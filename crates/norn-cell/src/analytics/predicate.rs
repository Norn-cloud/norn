//! MANIFEST-LAWS-1: how a [`RowPredicate`] types over a table and what it
//! reads; both engines and the cell type a landing's rules by this one code.

use super::{ColumnKind, MAX_PREDICATE_SCALE, PredicateKind, RowPredicate, TableSchema};

const MAX_PREDICATE_DEPTH: u32 = 128;

impl RowPredicate {
    /// MANIFEST-LAWS-1: the kind of this node over `table`, when every node is
    /// typed the way both the per-row evaluator and the engine compute it
    /// exactly: numbers compare and compute only with numbers, text and
    /// Booleans only compare for equality, `null` only appears as an
    /// operand of `==` or `!=`, and no scale passes
    /// [`MAX_PREDICATE_SCALE`]. `None` for anything else; such a check is
    /// left to the per-row evaluator.
    #[must_use]
    pub fn kind(&self, table: &TableSchema) -> Option<PredicateKind> {
        self.kind_in(table, false, 0)
    }

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
                ColumnKind::Timestamp | ColumnKind::Date | ColumnKind::Opaque => return None,
            },
            Self::Not(inner) => {
                (inner.kind_in(table, false, next)? == K::Bool).then_some(K::Bool)?
            }
            Self::Negate(inner) => match inner.kind_in(table, false, next)? {
                K::Number(scale) => K::Number(scale),
                K::Null | K::Bool | K::Text => return None,
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
                    (K::Number(_), K::Number(_)) | (K::Null, _) | (_, K::Null) => true,
                    (x, y) => x == y,
                };
                same.then_some(K::Bool)?
            }
            Self::Less(a, b)
            | Self::LessOrEqual(a, b)
            | Self::Greater(a, b)
            | Self::GreaterOrEqual(a, b) => {
                number(a, b)?;
                K::Bool
            }
            Self::And(a, b) | Self::Or(a, b) => (a.kind_in(table, false, next)? == K::Bool
                && b.kind_in(table, false, next)? == K::Bool)
                .then_some(K::Bool)?,
            Self::Conditional(test, then, otherwise) => {
                if test.kind_in(table, false, next)? != K::Bool {
                    return None;
                }
                match (
                    then.kind_in(table, false, next)?,
                    otherwise.kind_in(table, false, next)?,
                ) {
                    (K::Number(left), K::Number(right)) => K::Number(left.max(right)),
                    (left, right) if left == right => left,
                    _ => return None,
                }
            }
        };
        match kind {
            K::Number(scale) if scale > MAX_PREDICATE_SCALE => None,
            kind @ (K::Null | K::Bool | K::Text | K::Number(_)) => Some(kind),
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
            Self::Not(inner) | Self::Negate(inner) => inner.collect_reads(out),
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
            | Self::Or(a, b) => {
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
