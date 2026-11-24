use crate::lexer::Bracket;
use crate::variable::DynamicVariable;
use crate::vm::error::{VMError, VMResult};
use crate::vm::VmDate;
use crate::Variable;
use anyhow::anyhow;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use serde_json::Value;
use std::any::Any;
use std::fmt::{Display, Formatter};

#[derive(Debug, Clone)]
pub(crate) struct VmInterval {
    pub left_bracket: Bracket,
    pub right_bracket: Bracket,
    pub left: VmIntervalData,
    pub right: VmIntervalData,
}

impl DynamicVariable for VmInterval {
    fn type_name(&self) -> &'static str {
        "interval"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn to_value(&self) -> Value {
        Value::String(self.to_string())
    }
}

/// Norn (#1711): the most elements an interval may materialize, on every target.
/// `Begin` charges the full logical count first, so a metered run that cannot pay for
/// it is `BudgetExhausted` identically natively and on wasm32; a run that can (or an
/// unmetered one) past this ceiling is `IntervalTooLarge`, before any allocation.
const MAX_INTERVAL_ELEMENTS: u64 = 1_000_000;

impl VmInterval {
    /// Norn (#1711): the number of integers the interval enumerates, independent of
    /// the target's `usize` and saturating at `u64::MAX`; `None` if the interval does
    /// not enumerate integers (a date interval, a bound beyond `i64`). This is the
    /// count `Begin` is charged.
    pub fn element_count(&self) -> Option<u64> {
        self.range().map(|(_, count)| count)
    }

    /// Norn (#1711): the integers of the interval, materialized at exactly
    /// [`Self::element_count`] elements, or `IntervalTooLarge` past
    /// `MAX_INTERVAL_ELEMENTS` or when the allocation fails.
    pub fn to_array(&self) -> Option<VMResult<Vec<Variable>>> {
        let (start, count) = self.range()?;
        Some(self.materialize(start, count))
    }

    fn materialize(&self, start: i128, count: u64) -> VMResult<Vec<Variable>> {
        let len = usize::try_from(count)
            .ok()
            .filter(|_| count <= MAX_INTERVAL_ELEMENTS)
            .ok_or_else(|| self.too_large())?;
        let mut list = Vec::new();
        list.try_reserve_exact(len).map_err(|_| self.too_large())?;
        for n in (start..).take(len) {
            let n = i64::try_from(n).map_err(|_| self.too_large())?;
            list.push(Variable::Number(Decimal::from(n)));
        }

        Ok(list)
    }

    /// The first integer and the element count, computed once from the widened
    /// bounds (an empty interval counts zero).
    fn range(&self) -> Option<(i128, u64)> {
        let (start, end) = self.bounds()?;
        let span = end.saturating_sub(start).saturating_add(1).max(0);
        Some((start, u64::try_from(span).unwrap_or(u64::MAX)))
    }

    fn too_large(&self) -> VMError {
        VMError::IntervalTooLarge {
            interval: self.to_string(),
        }
    }

    /// The first and last integers, widened to `i128` so an open bound at `i64::MIN`
    /// or `i64::MAX` cannot overflow.
    fn bounds(&self) -> Option<(i128, i128)> {
        let (left, right) = match (&self.left, &self.right) {
            (VmIntervalData::Number(l), VmIntervalData::Number(r)) => (*l, *r),
            _ => return None,
        };

        let start = match &self.left_bracket {
            Bracket::LeftParenthesis => i128::from(left.to_i64()?) + 1,
            Bracket::LeftSquareBracket => i128::from(left.to_i64()?),
            _ => return None,
        };

        let end = match &self.right_bracket {
            Bracket::RightParenthesis => i128::from(right.to_i64()?) - 1,
            Bracket::RightSquareBracket => i128::from(right.to_i64()?),
            _ => return None,
        };

        Some((start, end))
    }

    pub fn includes(&self, v: VmIntervalData) -> anyhow::Result<bool> {
        let mut is_open = false;
        let l = &self.left;
        let r = &self.right;

        let first = match &self.left_bracket {
            Bracket::LeftParenthesis => l < &v,
            Bracket::LeftSquareBracket => l <= &v,
            Bracket::RightParenthesis => {
                is_open = true;
                l > &v
            }
            Bracket::RightSquareBracket => {
                is_open = true;
                l >= &v
            }
            _ => return Err(anyhow!("Unsupported bracket")),
        };

        let second = match &self.right_bracket {
            Bracket::RightParenthesis => r > &v,
            Bracket::RightSquareBracket => r >= &v,
            Bracket::LeftParenthesis => r < &v,
            Bracket::LeftSquareBracket => r <= &v,
            _ => return Err(anyhow!("Unsupported bracket")),
        };

        let open_stmt = is_open && (first || second);
        let closed_stmt = !is_open && first && second;

        Ok(open_stmt || closed_stmt)
    }
}

impl Display for VmInterval {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}{}..{}{}",
            self.left_bracket, self.left, self.right, self.right_bracket
        )
    }
}

#[derive(Debug, Clone, PartialOrd, PartialEq, Ord, Eq)]
pub(crate) enum VmIntervalData {
    Number(Decimal),
    Date(VmDate),
}

impl Display for VmIntervalData {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            VmIntervalData::Number(n) => write!(f, "{n}"),
            VmIntervalData::Date(d) => write!(f, "{d}"),
        }
    }
}
