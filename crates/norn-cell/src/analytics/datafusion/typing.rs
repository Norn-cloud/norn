//! BULK-LOAD-1 on the adapter: typing a source row's text exactly as the
//! `DuckDB` engine types it (shapes checked on characters, casts exact or
//! refused), then its declared pointwise laws, in declared column order.

use super::super::{Column, ColumnKind, PointLaw};
use super::values::{Cell, Dec, DecError, in_bulk_domain, parse_date, parse_timestamp};
use super::{Outcome, refusal};

/// A number the per-row decoder cannot hold exactly (S4).
const OUT_OF_RANGE: &str = "bulk.out_of_range";
const LAW: &str = "bulk.unsupported_law";

/// A column's declared laws are all checkable (refused before any row).
pub(super) fn check_laws(column: &Column) -> Outcome<()> {
    for law in &column.laws {
        match law {
            PointLaw::Min(bound) | PointLaw::Max(bound) => {
                let canonical = norn_ir::Decimal::parse(bound)
                    .ok()
                    .filter(|d| d.canonical() == *bound);
                if canonical.is_none() {
                    return Err(refusal(LAW, "a bound that is not a canonical decimal"));
                }
            }
            PointLaw::Unchecked => {
                return Err(refusal(
                    LAW,
                    format!(
                        "{} declares a constraint the engine does not check",
                        column.id
                    ),
                ));
            }
            PointLaw::OneOf(_) | PointLaw::MinLength(_) | PointLaw::MaxLength(_) => {}
        }
    }
    Ok(())
}

/// One column's text typed: its cell, or the reason it is refused.
pub(super) fn type_text(column: &Column, text: Option<&str>) -> Result<Cell, String> {
    let Some(text) = text else {
        return if column.nullable {
            Ok(Cell::Null)
        } else {
            Err("bulk.required".to_owned())
        };
    };
    let cell = parse(column.kind, text).map_err(str::to_owned)?;
    match first_law(column, &cell) {
        Some(reason) => Err(reason.to_owned()),
        None => Ok(cell),
    }
}

fn is_decimal_shape(text: &str) -> bool {
    let body = text
        .strip_prefix('+')
        .or_else(|| text.strip_prefix('-'))
        .unwrap_or(text);
    match body.split_once('.') {
        Some((whole, fraction)) => {
            !whole.is_empty()
                && !fraction.is_empty()
                && whole.bytes().all(|b| b.is_ascii_digit())
                && fraction.bytes().all(|b| b.is_ascii_digit())
        }
        None => !body.is_empty() && body.bytes().all(|b| b.is_ascii_digit()),
    }
}

fn parse(kind: ColumnKind, text: &str) -> Result<Cell, &'static str> {
    match kind {
        ColumnKind::Text | ColumnKind::Opaque => Ok(Cell::Text(text.to_owned())),
        ColumnKind::Integer => {
            if !text
                .bytes()
                .all(|b| b.is_ascii_digit() || b == b'-' || b == b'+')
                || text.bytes().filter(|b| !b.is_ascii_digit()).count() > 1
            {
                return Err("bulk.not_integer");
            }
            let value = Dec::parse_at(text, 0).map_err(|_e| "bulk.not_integer")?;
            if i64::try_from(value.m).is_err() {
                return Err(OUT_OF_RANGE);
            }
            Ok(Cell::Dec(value))
        }
        ColumnKind::Decimal { scale } => {
            if !is_decimal_shape(text) {
                return Err("bulk.not_decimal");
            }
            let value = match Dec::parse_at(text, scale) {
                Ok(value) => value,
                Err(DecError::Scale) => return Err("bulk.scale_exceeded"),
                Err(DecError::Shape | DecError::Range) => return Err("bulk.not_decimal"),
            };
            if in_bulk_domain(value) {
                Ok(Cell::Dec(value))
            } else {
                Err(OUT_OF_RANGE)
            }
        }
        ColumnKind::Boolean => {
            if text.eq_ignore_ascii_case("true") {
                Ok(Cell::Bool(true))
            } else if text.eq_ignore_ascii_case("false") {
                Ok(Cell::Bool(false))
            } else {
                Err("bulk.not_boolean")
            }
        }
        ColumnKind::Date => parse_date(text)
            .map(|t| Cell::Instant {
                text: text.to_owned(),
                t,
                n: 0,
            })
            .ok_or("bulk.not_date"),
        ColumnKind::Timestamp => parse_timestamp(text)
            .map(|(t, n)| Cell::Instant {
                text: text.to_owned(),
                t,
                n,
            })
            .ok_or("bulk.not_timestamp"),
    }
}

/// The first declared pointwise law a typed value breaks.
fn first_law(column: &Column, cell: &Cell) -> Option<&'static str> {
    if column.laws.is_empty() {
        return None;
    }
    let text = cell.text()?;
    column.laws.iter().find_map(|law| match law {
        PointLaw::OneOf(values) => {
            let found = match cell {
                Cell::Dec(value) => values.iter().any(|v| {
                    norn_ir::Decimal::parse(v).is_ok_and(|d| d.canonical() == value.canonical())
                }),
                Cell::Null | Cell::Text(_) | Cell::Bool(_) | Cell::Instant { .. } => {
                    values.contains(&text)
                }
            };
            (!found).then_some("bulk.not_in_enum")
        }
        PointLaw::Min(bound) | PointLaw::Max(bound) => {
            let bound = Dec::parse_at(bound, decimal_scale(bound)).ok()?;
            let value = match cell {
                Cell::Dec(value) => *value,
                Cell::Null | Cell::Text(_) | Cell::Bool(_) | Cell::Instant { .. } => {
                    Dec::parse_at(&text, decimal_scale(&text)).ok()?
                }
            };
            let order = value.compare(bound);
            match law {
                PointLaw::Min(_) => order.is_lt().then_some("bulk.below_min"),
                PointLaw::Max(_)
                | PointLaw::OneOf(_)
                | PointLaw::MinLength(_)
                | PointLaw::MaxLength(_)
                | PointLaw::Unchecked => order.is_gt().then_some("bulk.above_max"),
            }
        }
        PointLaw::MinLength(n) => (u64::try_from(text.chars().count()).unwrap_or(u64::MAX) < *n)
            .then_some("bulk.too_short"),
        PointLaw::MaxLength(n) => (u64::try_from(text.chars().count()).unwrap_or(u64::MAX) > *n)
            .then_some("bulk.too_long"),
        PointLaw::Unchecked => None,
    })
}

fn decimal_scale(text: &str) -> u32 {
    u32::try_from(text.split_once('.').map_or(0, |(_, f)| f.len())).unwrap_or(0)
}
