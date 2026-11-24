//! MANIFEST-LAWS-1 / LAW-04 on the adapter: every rule checked over the
//! typed rows of a landing in one pass. A rule decides a row only where the
//! decision is exact: every value it reads is present, every number (read,
//! literal or computed) is inside the per-row evaluator's decimal range,
//! every text is already NFC (the per-row evaluator compares NFC forms), the
//! predicate computes, and the row's rules fit its fuel. Anything else is
//! that rule's residue for that row, left to the per-row evaluator; a
//! decided failure is a finding, hold or advisory, carrying the row's values.

use std::collections::{BTreeMap, BTreeSet};

use unicode_normalization::UnicodeNormalization as _;

use super::super::{ColumnKind, PredicateKind, RowPredicate, RuleCheck, TableSchema, row_fuel};
use super::values::{Cell, Dec};
use super::{Outcome, refusal};

const LAW: &str = "bulk.unsupported_law";

/// One typed, admitted row of a landing.
#[derive(Debug, Clone)]
pub(super) struct TypedRow {
    pub row: u64,
    pub cells: Vec<Cell>,
}

/// One held-set row: a row that could not be typed (`rule: None`) or a
/// rule's finding on a typed row, with the row's values as text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct HeldRow {
    pub row: u64,
    pub reason: String,
    pub rule: Option<String>,
    pub holds: bool,
    pub values: Vec<Option<String>>,
}

/// MANIFEST-LAWS-1 (S3): a typed row's held-set values: each value's text, a
/// number as its canonical decimal.
pub(super) fn held_values(row: &TypedRow) -> Vec<Option<String>> {
    row.cells
        .iter()
        .map(|cell| match cell {
            Cell::Dec(value) => Some(value.canonical()),
            Cell::Null | Cell::Text(_) | Cell::Bool(_) | Cell::Instant { .. } => cell.text(),
        })
        .collect()
}

/// A predicate's value over one row.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Val {
    Null,
    Bool(bool),
    Text(String),
    Num(Dec),
}

/// A predicate typed against the landing's own schema.
struct Typed<'p> {
    node: &'p RowPredicate,
    table: &'p TableSchema,
}

impl Typed<'_> {
    fn index(&self, id: &str) -> Option<usize> {
        self.table.columns.iter().position(|c| c.id == id)
    }

    fn scale_of(&self, node: &RowPredicate) -> Option<u32> {
        match node.kind(self.table) {
            Some(PredicateKind::Number(scale)) => Some(scale),
            Some(PredicateKind::Null | PredicateKind::Bool | PredicateKind::Text) | None => None,
        }
    }

    /// The value of `node` over `cells`; `None` when it does not compute.
    fn eval(&self, node: &RowPredicate, cells: &[Cell]) -> Option<Val> {
        use RowPredicate as P;
        Some(match node {
            P::Null => Val::Null,
            P::Bool(flag) => Val::Bool(*flag),
            P::Number(text) => Val::Num(Dec::parse_at(text, self.scale_of(node)?).ok()?),
            P::Text(text) => Val::Text(text.clone()),
            P::Column(id) => match cells.get(self.index(id)?)? {
                Cell::Null => Val::Null,
                Cell::Bool(flag) => Val::Bool(*flag),
                Cell::Dec(value) => Val::Num(*value),
                Cell::Text(text) | Cell::Instant { text, .. } => Val::Text(text.clone()),
            },
            P::Not(inner) => match self.eval(inner, cells)? {
                Val::Bool(flag) => Val::Bool(!flag),
                Val::Null | Val::Text(_) | Val::Num(_) => return None,
            },
            P::Negate(inner) => match self.eval(inner, cells)? {
                Val::Num(value) => Val::Num(Dec {
                    m: value.m.checked_neg()?,
                    s: value.s,
                }),
                Val::Null | Val::Bool(_) | Val::Text(_) => return None,
            },
            P::Add(a, b) | P::Subtract(a, b) | P::Multiply(a, b) => {
                let (Val::Num(x), Val::Num(y)) = (self.eval(a, cells)?, self.eval(b, cells)?) else {
                    return None;
                };
                Val::Num(match node {
                    P::Add(..) => x.checked_add(y)?,
                    P::Subtract(..) => x.checked_add(Dec { m: y.m.checked_neg()?, s: y.s })?,
                    _ => x.checked_mul(y)?,
                })
            }
            P::Equal(a, b) | P::NotEqual(a, b) => {
                let equal = matches!(node, P::Equal(..));
                let (x, y) = (self.eval(a, cells)?, self.eval(b, cells)?);
                let same = match (&x, &y) {
                    (Val::Null, Val::Null) => true,
                    (Val::Null, _) | (_, Val::Null) => false,
                    (Val::Num(p), Val::Num(q)) => p.compare(*q).is_eq(),
                    (Val::Text(p), Val::Text(q)) => p == q,
                    (Val::Bool(p), Val::Bool(q)) => p == q,
                    _ => return None,
                };
                Val::Bool(same == equal)
            }
            P::Less(a, b) | P::LessOrEqual(a, b) | P::Greater(a, b) | P::GreaterOrEqual(a, b) => {
                let (Val::Num(x), Val::Num(y)) = (self.eval(a, cells)?, self.eval(b, cells)?) else {
                    return None;
                };
                let order = x.compare(y);
                Val::Bool(match node {
                    P::Less(..) => order.is_lt(),
                    P::LessOrEqual(..) => order.is_le(),
                    P::Greater(..) => order.is_gt(),
                    _ => order.is_ge(),
                })
            }
            P::And(a, b) | P::Or(a, b) => {
                let (Val::Bool(x), Val::Bool(y)) = (self.eval(a, cells)?, self.eval(b, cells)?) else {
                    return None;
                };
                Val::Bool(if matches!(node, P::And(..)) { x && y } else { x || y })
            }
            P::Conditional(c, a, b) => match self.eval(c, cells)? {
                Val::Bool(true) => self.widen(self.eval(a, cells)?, node)?,
                Val::Bool(false) | Val::Null => self.widen(self.eval(b, cells)?, node)?,
                Val::Text(_) | Val::Num(_) => return None,
            },
        })
    }

    /// A conditional's branch at the conditional's own scale.
    fn widen(&self, value: Val, node: &RowPredicate) -> Option<Val> {
        match (value, self.scale_of(node)) {
            (Val::Num(value), Some(scale)) => Some(Val::Num(value.rescale(scale)?)),
            (value, _) => Some(value),
        }
    }

    /// Every guard of the tree holds over `cells`: each number in the
    /// per-row range at its scale, each text NFC.
    fn guards(&self, node: &RowPredicate, cells: &[Cell]) -> bool {
        use RowPredicate as P;
        let in_range = |n: &RowPredicate| match (self.eval(n, cells), self.scale_of(n)) {
            (Some(Val::Num(value)), Some(scale)) => {
                value.rescale(scale).is_some_and(Dec::in_bulk_range)
            }
            _ => false,
        };
        let nfc = |n: &RowPredicate| match self.eval(n, cells) {
            Some(Val::Text(text)) => text.nfc().eq(text.chars()),
            _ => false,
        };
        let own = match node {
            P::Number(_) => in_range(node),
            P::Text(_) => nfc(node),
            P::Column(_) => match node.kind(self.table) {
                Some(PredicateKind::Number(_)) => in_range(node),
                Some(PredicateKind::Text) => nfc(node),
                Some(PredicateKind::Null | PredicateKind::Bool) | None => true,
            },
            P::Add(..) | P::Subtract(..) | P::Multiply(..) => in_range(node),
            P::Null
            | P::Bool(_)
            | P::Not(_)
            | P::Negate(_)
            | P::Equal(..)
            | P::NotEqual(..)
            | P::Less(..)
            | P::LessOrEqual(..)
            | P::Greater(..)
            | P::GreaterOrEqual(..)
            | P::And(..)
            | P::Or(..)
            | P::Conditional(..) => true,
        };
        own && children(node).iter().all(|child| self.guards(child, cells))
    }
}

fn children(node: &RowPredicate) -> Vec<&RowPredicate> {
    use RowPredicate as P;
    match node {
        P::Null | P::Bool(_) | P::Number(_) | P::Text(_) | P::Column(_) => Vec::new(),
        P::Not(inner) | P::Negate(inner) => vec![inner],
        P::Add(a, b)
        | P::Subtract(a, b)
        | P::Multiply(a, b)
        | P::Equal(a, b)
        | P::NotEqual(a, b)
        | P::Less(a, b)
        | P::LessOrEqual(a, b)
        | P::Greater(a, b)
        | P::GreaterOrEqual(a, b)
        | P::And(a, b)
        | P::Or(a, b) => vec![a, b],
        P::Conditional(c, a, b) => vec![c, a, b],
    }
}

/// ZEN's shallow size of `node`'s value: a text's bytes, else 1.
fn value_size(node: &RowPredicate, table: &TableSchema, cells: &[Cell]) -> u128 {
    use RowPredicate as P;
    match node {
        P::Text(text) => u128::try_from(text.len().max(1)).unwrap_or(u128::MAX),
        P::Column(id) if node.kind(table) == Some(PredicateKind::Text) => table
            .columns
            .iter()
            .position(|c| c.id == *id)
            .and_then(|i| cells.get(i))
            .and_then(Cell::as_str)
            .map_or(0, |text| u128::try_from(text.len()).unwrap_or(u128::MAX)),
        P::Conditional(_, a, b) => value_size(a, table, cells).max(value_size(b, table, cells)),
        _ => 1,
    }
}

/// The data-proportional fuel of `node` and everything under it.
fn data_fuel(node: &RowPredicate, table: &TableSchema, cells: &[Cell]) -> u128 {
    use RowPredicate as P;
    match node {
        P::Null | P::Bool(_) | P::Number(_) | P::Text(_) | P::Column(_) => 0,
        P::Not(inner) | P::Negate(inner) => data_fuel(inner, table, cells),
        P::Conditional(c, a, b) => data_fuel(c, table, cells)
            .saturating_add(data_fuel(a, table, cells))
            .saturating_add(data_fuel(b, table, cells)),
        _ => children(node).iter().fold(0_u128, |total, child| {
            total
                .saturating_add(data_fuel(child, table, cells))
                .saturating_add(value_size(child, table, cells))
        }),
    }
}

/// One check typed: its predicate and condition, refused if the engine does
/// not type them as Booleans.
struct Check<'c> {
    check: &'c RuleCheck,
    predicate: Option<&'c RowPredicate>,
    reads: Vec<usize>,
}

fn type_check<'c>(check: &'c RuleCheck, table: &TableSchema) -> Outcome<Check<'c>> {
    let Some(predicate) = &check.predicate else {
        return Ok(Check {
            check,
            predicate: None,
            reads: Vec::new(),
        });
    };
    let boolean = |node: &RowPredicate, what: &str| match node.kind(table) {
        None => Err(refusal(LAW, "a rule the engine does not type")),
        Some(PredicateKind::Bool) => Ok(()),
        Some(PredicateKind::Null | PredicateKind::Text | PredicateKind::Number(_)) => {
            Err(refusal(LAW, format!("a rule whose {what} is not Boolean")))
        }
    };
    boolean(predicate, "predicate")?;
    if let Some(condition) = &check.condition {
        boolean(condition, "condition")?;
    }
    let mut names = predicate.reads();
    if let Some(condition) = &check.condition {
        for read in condition.reads() {
            if !names.contains(&read) {
                names.push(read);
            }
        }
    }
    let reads = names
        .iter()
        .filter_map(|name| table.columns.iter().position(|c| c.id == *name))
        .collect();
    Ok(Check {
        check,
        predicate: Some(predicate),
        reads,
    })
}

/// The outcome of every rule over a landing's typed rows: residue by row
/// (each row's undecided rules) and the findings.
pub(super) struct Checked {
    pub residue: BTreeMap<u64, BTreeSet<String>>,
    pub findings: Vec<HeldRow>,
}

/// MANIFEST-LAWS-1 / LAW-04: check every rule over `rows` in one pass.
pub(super) fn check_rules(table: &TableSchema, checks: &[RuleCheck], rows: &[TypedRow]) -> Outcome<Checked> {
    let mut checked = Checked {
        residue: BTreeMap::new(),
        findings: Vec::new(),
    };
    if checks.is_empty() {
        return Ok(checked);
    }
    let typed = checks
        .iter()
        .map(|check| type_check(check, table))
        .collect::<Outcome<Vec<_>>>()?;
    let lowered = typed.iter().all(|check| check.predicate.is_some());
    let budget = u128::from(row_fuel());
    for row in rows {
        // S2: a row whose rules may cost more fuel than a row is given is
        // left to the per-row evaluator for every rule.
        let over = lowered
            && typed
                .iter()
                .fold(0_u128, |total, check| {
                    let mut fuel = u128::from(check.check.fuel);
                    if let Some(predicate) = check.predicate {
                        fuel = fuel.saturating_add(data_fuel(predicate, table, &row.cells));
                    }
                    if let Some(condition) = &check.check.condition {
                        fuel = fuel.saturating_add(data_fuel(condition, table, &row.cells));
                    }
                    total.saturating_add(fuel)
                })
                > budget;
        for check in &typed {
            match decide(check, table, &row.cells, over) {
                None => {
                    checked
                        .residue
                        .entry(row.row)
                        .or_default()
                        .insert(check.check.rule.clone());
                }
                Some(true) => checked.findings.push(finding(check.check, row)),
                Some(false) => {}
            }
        }
    }
    Ok(checked)
}

/// A finding of `check` on `row`, carrying its values.
pub(super) fn finding(check: &RuleCheck, row: &TypedRow) -> HeldRow {
    HeldRow {
        row: row.row,
        reason: check.label.clone(),
        rule: Some(check.rule.clone()),
        holds: check.holds,
        values: held_values(row),
    }
}

/// Whether the row fails the rule; `None` when it is the rule's residue.
fn decide(check: &Check<'_>, table: &TableSchema, cells: &[Cell], over: bool) -> Option<bool> {
    let predicate = check.predicate?;
    if over
        || check
            .reads
            .iter()
            .any(|index| !cells.get(*index).is_some_and(Cell::is_present))
    {
        return None;
    }
    let typed = Typed {
        node: predicate,
        table,
    };
    let mut guarded = typed.guards(typed.node, cells);
    let condition = match &check.check.condition {
        Some(condition) => {
            guarded &= typed.guards(condition, cells);
            match typed.eval(condition, cells)? {
                Val::Bool(flag) => Some(flag),
                Val::Null | Val::Text(_) | Val::Num(_) => return None,
            }
        }
        None => None,
    };
    let Val::Bool(value) = typed.eval(predicate, cells)? else {
        return None;
    };
    if !guarded {
        return None;
    }
    Some(condition.unwrap_or(true) && value != check.check.expect)
}

/// Whether a column kind's values are wide (counted toward a residue page's bytes).
pub(super) const fn is_wide(kind: ColumnKind) -> bool {
    matches!(
        kind,
        ColumnKind::Text | ColumnKind::Opaque | ColumnKind::Timestamp | ColumnKind::Date
    )
}
