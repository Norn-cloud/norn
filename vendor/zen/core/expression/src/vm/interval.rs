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

/// Norn (#1711): the most elements an interval may enumerate: a `Vec<Variable>` of
/// this length is the largest the allocator can be asked for.
const MAX_INTERVAL_ELEMENTS: usize = isize::MAX.unsigned_abs() / std::mem::size_of::<Variable>();

/// Norn (#1711): the integers an interval enumerates, computed once with checked
/// arithmetic. The same approved `len` is charged as fuel and then allocated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct IntervalRange {
    start: i128,
    len: usize,
}

impl IntervalRange {
    /// Norn `metering`: the units charged before the range is materialized.
    pub fn fuel(&self) -> u64 {
        u64::try_from(self.len).unwrap_or(u64::MAX)
    }

    /// Materializes exactly the approved `len` elements, reserving them up front.
    pub fn to_array(&self, interval: &VmInterval) -> VMResult<Vec<Variable>> {
        let mut list = Vec::new();
        list.try_reserve_exact(self.len)
            .map_err(|_| interval.too_large())?;
        for n in (self.start..).take(self.len) {
            let n = i64::try_from(n).map_err(|_| interval.too_large())?;
            list.push(Variable::Number(Decimal::from(n)));
        }

        Ok(list)
    }
}

impl VmInterval {
    /// Norn (#1711): the checked integer range of a number interval; `None` if the
    /// interval does not enumerate integers (a date interval, a bound beyond `i64`),
    /// a typed error if its element count overflows or cannot be allocated.
    pub fn range(&self) -> Option<VMResult<IntervalRange>> {
        let (start, end) = self.bounds()?;
        let len = end
            .checked_sub(start)
            .and_then(|span| span.checked_add(1))
            .map(|len| len.max(0))
            .and_then(|len| usize::try_from(len).ok())
            .filter(|len| *len <= MAX_INTERVAL_ELEMENTS);
        Some(
            len.map(|len| IntervalRange { start, len })
                .ok_or_else(|| self.too_large()),
        )
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
