//! CONS-LINEAR-1: exact coefficient checking, conditional on a trusted lowering.
//!
//! This module accepts a mathematical transition system, not declarations. Its
//! success is not application evidence: the source-bound adapter must establish
//! complete writer coverage, the declared functional and faithful affine deltas.
use std::collections::{BTreeMap, BTreeSet};

use norn_ir::UnitRef;

/// The extracted, proved acceptance decision for one writer (#718 U5).
pub mod decision;
/// Untrusted exact equality-feasibility discovery; candidates require checking.
pub mod discovery;

pub use crate::exact_rational::Rational;

/// One coordinate of the declared conserved functional.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Coordinate {
    /// Stable source coordinate identity.
    pub id: String,
    /// Exact scope bindings, including book/currency/valuation basis when present.
    pub scope: BTreeMap<String, String>,
    /// Exact unit identity. No implicit conversion or cross-unit cancellation.
    pub unit: UnitRef,
    /// Weight required by the declared law, independently of discovery.
    pub weight: Rational,
    /// The symbolic key when this coordinate is one row of a keyed family
    /// (`Bal[from]`); `None` for a coordinate that is always its own cell.
    pub key: Option<Key>,
}

/// The symbolic key of a keyed coordinate such as `Bal[from]`.
///
/// Keyed coordinates of one family, scope and unit with different bindings
/// may still name one runtime cell (`from == to`), so CONS-LINEAR-1 checks
/// every key-equality case a writer does not exclude.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Key {
    /// The keyed state family (`Bal`).
    pub family: String,
    /// The writer's symbolic key (`from`). A binding names one key value
    /// within a writer; different bindings may be equal at run time.
    pub binding: String,
}

/// Diagnostic code of [`Failure::Aliased`]: a writer balances only while its
/// keyed coordinates name distinct cells.
pub const KEY_ALIAS_CODE: &str = "law.conservation.key_alias";
/// Diagnostic code prefixing the [`Failure::Unknown`] reason when a writer has
/// more key-aliasing cases than the checker enumerates.
pub const ALIAS_BOUND_CODE: &str = "law.conservation.alias_bound";
/// Most key-aliasing cases checked per writer, the all-distinct case included.
pub const MAX_ALIAS_CASES: usize = 256;
/// Most distinct bindings one family/scope/unit of a writer may touch.
pub const MAX_ALIASED_KEYS: usize = 6;

/// Each row is one coordinate; columns are independent symbolic parameters,
/// followed by a constant column. Every possible parameter value is checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delta {
    /// Faithfully extracted affine state change.
    Affine(Vec<Vec<Rational>>),
    /// Untranslated writer, including nonlinear or opaque semantics.
    Unknown(String),
}

/// A writer in the complete source-bound closure supplied by the adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Writer {
    /// Stable declaration identity.
    pub id: String,
    /// Proposed delta before target-law filtering.
    pub delta: Delta,
    /// Pairs of key bindings that this writer's guard proves unequal
    /// (`from != to`). The adapter must establish each pair from the guard;
    /// cases that would equate a listed pair are not checked.
    pub distinct: Vec<(String, String)>,
}

/// A refusal explains exactly which symbolic coefficient did not balance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// Invalid shape, duplicate identity, zero functional or altered weights.
    Malformed,
    /// Unsupported semantics or arithmetic/resource limit; no proof claim.
    Unknown(String),
    /// Nonzero coefficient in a single scope/unit partition. This is an
    /// algebraic witness, not a claim of guard-feasible runtime reachability.
    Unbalanced {
        /// Writer declaration identity.
        writer: String,
        /// Representative coordinate identifying the scope and unit partition.
        coordinate: String,
        /// Parameter column (last column is the affine constant).
        column: usize,
        /// Exact nonzero weighted coefficient.
        residual: Rational,
    },
    /// The writer balances while its keyed coordinates are distinct cells, but
    /// not when some of their keys are equal and the writes hit one cell.
    Aliased {
        /// Always [`KEY_ALIAS_CODE`].
        code: &'static str,
        /// Writer declaration identity.
        writer: String,
        /// The aliasing case: each entry lists the coordinates that name one
        /// cell, with how that cell's writes combine.
        cells: Vec<AliasedCell>,
        /// Representative coordinate identifying the scope and unit partition.
        coordinate: String,
        /// Parameter column (last column is the affine constant).
        column: usize,
        /// Exact nonzero weighted coefficient in this case.
        residual: Rational,
    },
}

/// One runtime cell named by several keyed coordinates in an aliasing case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AliasedCell {
    /// Coordinate ids whose keys are equal in this case.
    pub coordinates: Vec<String>,
    /// The coordinate whose write survives (each write was computed from the
    /// pre-state), or `None` when the writes accumulate in sequence.
    pub survivor: Option<String>,
}

fn validate(
    coordinates: &[Coordinate],
    writers: &[Writer],
    certificate: &[Rational],
) -> Result<(), Failure> {
    let malformed = || Failure::Malformed;
    if coordinates.is_empty()
        || coordinates.len() > 256
        || writers.len() > 256
        || certificate.len() != coordinates.len()
        || !coordinates.iter().any(|c| c.weight.numerator != 0)
    {
        return Err(malformed());
    }
    let mut ids = BTreeSet::new();
    for (coordinate, weight) in coordinates.iter().zip(certificate) {
        if coordinate.id.is_empty()
            || coordinate.unit.code.is_empty()
            || coordinate.scope.is_empty()
            || coordinate
                .scope
                .iter()
                .any(|(k, v)| k.is_empty() || v.is_empty())
            || !ids.insert(&coordinate.id)
            || coordinate.weight != *weight
        {
            return Err(malformed());
        }
    }
    // One cell per (family, binding, scope, unit); one weight per family cell class.
    let mut cells = BTreeSet::new();
    let mut family_weights = BTreeMap::new();
    for coordinate in coordinates {
        let Some(key) = &coordinate.key else { continue };
        if key.family.is_empty()
            || key.binding.is_empty()
            || !cells.insert((key, &coordinate.scope, &coordinate.unit))
            || *family_weights
                .entry((&key.family, &coordinate.scope, &coordinate.unit))
                .or_insert(coordinate.weight)
                != coordinate.weight
        {
            return Err(malformed());
        }
    }
    let mut writer_ids = BTreeSet::new();
    if writers.iter().any(|w| {
        w.id.is_empty()
            || !writer_ids.insert(&w.id)
            || w.distinct
                .iter()
                .any(|(a, b)| a.is_empty() || b.is_empty() || a == b)
    }) {
        return Err(malformed());
    }
    Ok(())
}

/// The keyed coordinates a writer touches (nonzero row), grouped by family,
/// scope and unit in deterministic order; only groups that could alias.
fn alias_groups(coordinates: &[Coordinate], rows: &[Vec<Rational>]) -> Vec<Vec<usize>> {
    let mut groups: BTreeMap<_, Vec<usize>> = BTreeMap::new();
    for (index, (coordinate, row)) in coordinates.iter().zip(rows).enumerate() {
        if let Some(key) = &coordinate.key
            && row.iter().any(|q| q.numerator != 0)
        {
            groups
                .entry((&key.family, &coordinate.scope, &coordinate.unit))
                .or_default()
                .push(index);
        }
    }
    groups.into_values().filter(|g| g.len() > 1).collect()
}

/// Every partition of `members` into equal-key blocks that keeps each
/// guard-distinct pair apart, in a fixed order. `None` past `limit`.
fn key_partitions(
    members: &[usize],
    apart: &dyn Fn(usize, usize) -> bool,
    limit: usize,
) -> Option<Vec<Vec<Vec<usize>>>> {
    fn grow(
        rest: &[usize],
        blocks: &mut Vec<Vec<usize>>,
        apart: &dyn Fn(usize, usize) -> bool,
        out: &mut Vec<Vec<Vec<usize>>>,
        limit: usize,
    ) -> bool {
        let Some((&next, rest)) = rest.split_first() else {
            out.push(blocks.clone());
            return out.len() <= limit;
        };
        for b in 0..blocks.len() {
            let fits = blocks
                .get(b)
                .is_some_and(|block| block.iter().all(|&m| !apart(m, next)));
            if fits {
                if let Some(block) = blocks.get_mut(b) {
                    block.push(next);
                }
                let ok = grow(rest, blocks, apart, out, limit);
                if let Some(block) = blocks.get_mut(b) {
                    block.pop();
                }
                if !ok {
                    return false;
                }
            }
        }
        blocks.push(vec![next]);
        let ok = grow(rest, blocks, apart, out, limit);
        blocks.pop();
        ok
    }
    let mut out = Vec::new();
    grow(members, &mut Vec::new(), apart, &mut out, limit).then_some(out)
}

/// One aliasing case: merged cells (coordinate indices) and how each combines.
type AliasCase = Vec<(Vec<usize>, Option<usize>)>;

/// The aliasing cases of one writer beyond the all-distinct one: each case is
/// a list of merged cells (coordinate indices, at least two) with the choice of
/// how they combine (`Some(i)`: index `i` of the cell survives; `None`: the
/// writes accumulate). `Err` names the bound that was exceeded.
fn alias_cases(
    coordinates: &[Coordinate],
    writer: &Writer,
    rows: &[Vec<Rational>],
) -> Result<Vec<AliasCase>, String> {
    let groups = alias_groups(coordinates, rows);
    let binding = |i: usize| {
        coordinates
            .get(i)
            .and_then(|c| c.key.as_ref())
            .map(|k| k.binding.as_str())
    };
    let apart = |a: usize, b: usize| {
        writer.distinct.iter().any(|(x, y)| {
            (binding(a), binding(b)) == (Some(x), Some(y))
                || (binding(a), binding(b)) == (Some(y), Some(x))
        })
    };
    // Merged-cell sets per combination of group partitions.
    let mut merges: Vec<Vec<Vec<usize>>> = vec![Vec::new()];
    for group in &groups {
        if group.len() > MAX_ALIASED_KEYS {
            return Err(format!(
                "{} keyed coordinates of one family alias (limit {MAX_ALIASED_KEYS})",
                group.len()
            ));
        }
        let partitions = key_partitions(group, &apart, MAX_ALIAS_CASES)
            .ok_or_else(|| format!("more than {MAX_ALIAS_CASES} key-equality cases"))?;
        let mut next = Vec::new();
        for merge in &merges {
            for partition in &partitions {
                let mut combined = merge.clone();
                combined.extend(partition.iter().filter(|b| b.len() > 1).cloned());
                next.push(combined);
                if next.len() > MAX_ALIAS_CASES {
                    return Err(format!("more than {MAX_ALIAS_CASES} key-equality cases"));
                }
            }
        }
        merges = next;
    }
    // Each merged cell's writes either accumulate or one of them survives.
    let mut cases = Vec::new();
    for merge in merges.into_iter().filter(|m| !m.is_empty()) {
        let mut choice = vec![0_usize; merge.len()];
        loop {
            cases.push(
                merge
                    .iter()
                    .zip(&choice)
                    .map(|(cell, &c)| (cell.clone(), c.checked_sub(1)))
                    .collect(),
            );
            if cases.len() >= MAX_ALIAS_CASES {
                return Err(format!("more than {MAX_ALIAS_CASES} key-aliasing cases"));
            }
            // Odometer over choices 0 (accumulate) ..= cell.len() (survivor).
            let mut wrapped = true;
            for (slot, cell) in choice.iter_mut().zip(&merge) {
                if *slot < cell.len() {
                    *slot = slot.saturating_add(1);
                    wrapped = false;
                    break;
                }
                *slot = 0;
            }
            if wrapped {
                break;
            }
        }
    }
    Ok(cases)
}

/// The rows of one aliasing case: each merged cell's delta lands on its first
/// coordinate (the sum, or the survivor's row) and the others become zero.
fn aliased_rows(
    rows: &[Vec<Rational>],
    case: &[(Vec<usize>, Option<usize>)],
) -> Option<Vec<Vec<Rational>>> {
    let mut out = rows.to_vec();
    for (cell, survivor) in case {
        let width = rows.first()?.len();
        let mut merged = vec![Rational::new(0, 1)?; width];
        for (position, &index) in cell.iter().enumerate() {
            if survivor.is_none_or(|s| s == position) {
                for (total, q) in merged.iter_mut().zip(rows.get(index)?) {
                    *total = total.add(*q)?;
                }
            }
        }
        for &index in cell {
            *out.get_mut(index)? = vec![Rational::new(0, 1)?; width];
        }
        *out.get_mut(*cell.first()?)? = merged;
    }
    Some(out)
}

/// CONS-LINEAR-1: verify all coefficients of wᵀΔ independently per scope/unit.
/// A certificate may not replace the declared functional with another nullspace
/// vector. This checks all parameter valuations without quantity enumeration.
/// Empty writer sets prove only the mathematical no-step system.
/// # Errors
/// Malformed certificate/system, unsupported writer, resource/number limit, or
/// a nonzero symbolic coefficient. No failure grants application authority.
pub fn check(
    coordinates: &[Coordinate],
    writers: &[Writer],
    certificate: &[Rational],
) -> Result<(), Failure> {
    validate(coordinates, writers, certificate)?;
    // Label each coordinate with the first coordinate of its scope/unit class.
    // The ordered map also fixes which unbalanced class is reported first.
    let mut classes = BTreeMap::new();
    let partition: Vec<usize> = coordinates
        .iter()
        .enumerate()
        .map(|(index, c)| *classes.entry((&c.scope, &c.unit)).or_insert(index))
        .collect();
    let weights: Vec<Rational> = coordinates.iter().map(|c| c.weight).collect();
    let mut work = 0_usize;
    for writer in writers {
        let rows = match &writer.delta {
            Delta::Affine(rows) => rows,
            Delta::Unknown(reason) => {
                return Err(Failure::Unknown(format!("{}: {reason}", writer.id)));
            }
        };
        let columns = rows.first().map_or(0, Vec::len);
        if rows.len() != coordinates.len()
            || columns == 0
            || rows.iter().any(|r| r.len() != columns)
        {
            return Err(Failure::Malformed);
        }
        work = work.saturating_add(rows.len().saturating_mul(columns));
        if work > 65_536 {
            return Err(Failure::Unknown("coefficient budget exceeded".into()));
        }
        let decide = |rows: &[Vec<Rational>]| -> Result<(), Failure> {
            match decision::check_writer(&partition, &weights, rows, columns) {
                Ok(()) => Ok(()),
                Err(decision::Refusal::Malformed) => Err(Failure::Malformed),
                Err(decision::Refusal::Overflow) => Err(Failure::Unknown(format!(
                    "{}: rational representation limit",
                    writer.id
                ))),
                Err(decision::Refusal::Unbalanced(totals)) => {
                    Err(first_unbalanced(&writer.id, coordinates, &classes, &totals))
                }
            }
        };
        decide(rows)?;
        // Keys the writer does not prove distinct may be equal at run time;
        // the writer must balance in every such case as well.
        let cases = alias_cases(coordinates, writer, rows)
            .map_err(|why| Failure::Unknown(format!("{ALIAS_BOUND_CODE}: {}: {why}", writer.id)))?;
        for case in &cases {
            work = work.saturating_add(rows.len().saturating_mul(columns));
            if work > 65_536 {
                return Err(Failure::Unknown("coefficient budget exceeded".into()));
            }
            let merged = aliased_rows(rows, case).ok_or_else(|| {
                Failure::Unknown(format!("{}: rational representation limit", writer.id))
            })?;
            match decide(&merged) {
                Ok(()) => {}
                Err(Failure::Unbalanced {
                    coordinate,
                    column,
                    residual,
                    ..
                }) => {
                    return Err(Failure::Aliased {
                        code: KEY_ALIAS_CODE,
                        writer: writer.id.clone(),
                        cells: describe_case(coordinates, case),
                        coordinate,
                        column,
                        residual,
                    });
                }
                Err(other) => return Err(other),
            }
        }
    }
    Ok(())
}

fn describe_case(
    coordinates: &[Coordinate],
    case: &[(Vec<usize>, Option<usize>)],
) -> Vec<AliasedCell> {
    let id = |i: usize| coordinates.get(i).map(|c| c.id.clone()).unwrap_or_default();
    case.iter()
        .map(|(cell, survivor)| AliasedCell {
            coordinates: cell.iter().map(|&i| id(i)).collect(),
            survivor: survivor.and_then(|s| cell.get(s)).map(|&i| id(i)),
        })
        .collect()
}

impl decision::ExactArithmetic for Rational {
    fn exact_zero() -> Self {
        Self {
            numerator: 0,
            denominator: 1,
        }
    }
    fn exact_is_zero(self) -> bool {
        self.numerator == 0
    }
    fn exact_add(self, rhs: Self) -> Option<Self> {
        self.add(rhs)
    }
    fn exact_mul(self, rhs: Self) -> Option<Self> {
        self.mul(rhs)
    }
}

/// Report the first nonzero total in scope/unit order, naming the class's
/// first coordinate. Only reached after the decision core refused.
fn first_unbalanced(
    writer: &str,
    coordinates: &[Coordinate],
    classes: &BTreeMap<(&BTreeMap<String, String>, &UnitRef), usize>,
    totals: &[Vec<Rational>],
) -> Failure {
    classes
        .values()
        .find_map(|&class| {
            let (column, residual) = totals
                .get(class)?
                .iter()
                .enumerate()
                .find(|(_, residual)| residual.numerator != 0)?;
            Some(Failure::Unbalanced {
                writer: writer.to_owned(),
                coordinate: coordinates.get(class)?.id.clone(),
                column,
                residual: *residual,
            })
        })
        .unwrap_or(Failure::Malformed)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "small exact certificate fixtures"
)]
mod tests {
    use super::*;
    use norn_ir::UnitSystem;
    use proptest::prelude::*;

    fn q(n: i64) -> Rational {
        Rational::new(n, 1).unwrap()
    }
    fn system() -> (Vec<Coordinate>, Vec<Writer>) {
        let coordinates = ["input", "output", "loss"]
            .into_iter()
            .map(|id| Coordinate {
                id: id.into(),
                scope: BTreeMap::from([("book".into(), "primary".into())]),
                unit: UnitRef {
                    system: UnitSystem::Ucum,
                    code: "g".into(),
                },
                weight: q(1),
                key: None,
            })
            .collect();
        let writer = Writer {
            id: "convert".into(),
            delta: Delta::Affine(vec![
                vec![q(-1), q(0), q(0)],
                vec![q(0), q(1), q(0)],
                vec![q(1), q(-1), q(0)],
            ]),
            distinct: Vec::new(),
        };
        (coordinates, vec![writer])
    }

    #[test]
    fn cons_linear_valid_and_omitted_loss() {
        let (c, mut w) = system();
        assert_eq!(check(&c, &w, &[q(1); 3]), Ok(()));
        let Delta::Affine(rows) = &mut w[0].delta else {
            unreachable!()
        };
        rows[2] = vec![q(0); 3];
        assert!(
            matches!(check(&c, &w, &[q(1); 3]), Err(Failure::Unbalanced { column: 0, residual, .. }) if residual == q(-1))
        );
    }

    #[test]
    fn cons_linear_units_books_and_functional_cannot_be_substituted() {
        let (mut c, w) = system();
        c[2].unit.code = "mg".into();
        assert!(matches!(
            check(&c, &w, &[q(1); 3]),
            Err(Failure::Unbalanced { .. })
        ));
        c[2].unit.code = "g".into();
        c[2].scope.insert("book".into(), "other".into());
        assert!(matches!(
            check(&c, &w, &[q(1); 3]),
            Err(Failure::Unbalanced { .. })
        ));
        assert_eq!(check(&c, &w, &[q(0); 3]), Err(Failure::Malformed));
        assert_eq!(check(&c, &w, &[q(1); 2]), Err(Failure::Malformed));
    }

    #[test]
    fn cons_linear_unknown_and_limits() {
        let (c, mut w) = system();
        w[0].delta = Delta::Unknown("nonlinear multiplication".into());
        assert!(
            matches!(check(&c, &w, &[q(1); 3]), Err(Failure::Unknown(reason)) if reason.contains("nonlinear"))
        );
        assert!(Rational::new(1, 0).is_none());
        assert_eq!(Rational::new(2, -4), Rational::new(-1, 2));
        assert_eq!(Rational::new(i64::MIN, i64::MIN), Some(q(1)));
        assert_eq!(q(i64::MAX).add(q(1)), None);
        assert_eq!(q(i64::MAX).mul(q(i64::MAX)), None);
        w[0].delta = Delta::Affine(vec![vec![q(0)]; 2]);
        assert_eq!(check(&c, &w, &[q(1); 3]), Err(Failure::Malformed));
    }

    /// `Bal[k]` coordinates of one family; columns are `(amount, constant)`.
    fn keyed(bindings: &[&str]) -> Vec<Coordinate> {
        bindings
            .iter()
            .map(|binding| Coordinate {
                id: format!("Bal[{binding}]"),
                scope: BTreeMap::from([("book".into(), "primary".into())]),
                unit: UnitRef {
                    system: UnitSystem::Ucum,
                    code: "USD".into(),
                },
                weight: q(1),
                key: Some(Key {
                    family: "Bal".into(),
                    binding: (*binding).into(),
                }),
            })
            .collect()
    }
    fn transfer(distinct: &[(&str, &str)]) -> Writer {
        Writer {
            id: "transfer".into(),
            // Bal[from] := Bal[from] - amount; Bal[to] := Bal[to] + amount.
            delta: Delta::Affine(vec![vec![q(-1), q(0)], vec![q(1), q(0)]]),
            distinct: distinct
                .iter()
                .map(|(a, b)| ((*a).into(), (*b).into()))
                .collect(),
        }
    }

    #[test]
    fn cons_same_account_transfer_is_refused() {
        // #1457: with from == to both writes hit one row; the credit computed
        // from the pre-state overwrites the debit and mints `amount`.
        let c = keyed(&["from", "to"]);
        let refusal = check(&c, &[transfer(&[])], &[q(1); 2]).unwrap_err();
        let Failure::Aliased {
            code,
            writer,
            cells,
            column,
            ..
        } = refusal
        else {
            panic!("expected an aliasing refusal, got {refusal:?}");
        };
        assert_eq!(
            (code, writer.as_str(), column),
            (KEY_ALIAS_CODE, "transfer", 0)
        );
        assert_eq!(cells.len(), 1);
        assert_eq!(cells[0].coordinates, ["Bal[from]", "Bal[to]"]);
        assert!(cells[0].survivor.is_some());
    }

    #[test]
    fn cons_distinct_account_transfer_is_admitted() {
        // Distinct keys: Bal[from] and Bal[to] cannot alias, either because
        // the guard proves it or because they are not keyed cells at all.
        let mut c = keyed(&["from", "to"]);
        assert_eq!(
            check(&c, &[transfer(&[("from", "to")])], &[q(1); 2]),
            Ok(())
        );
        assert_eq!(
            check(&c, &[transfer(&[("to", "from")])], &[q(1); 2]),
            Ok(())
        );
        // Another family never aliases.
        c[1].key = Some(Key {
            family: "Escrow".into(),
            binding: "to".into(),
        });
        assert_eq!(check(&c, &[transfer(&[])], &[q(1); 2]), Ok(()));
    }

    #[test]
    fn cons_guard_must_name_the_aliased_pair() {
        let c = keyed(&["from", "to"]);
        // A guard on unrelated bindings does not exclude from == to.
        assert!(matches!(
            check(&c, &[transfer(&[("from", "fee")])], &[q(1); 2]),
            Err(Failure::Aliased { .. })
        ));
        // A self-contradictory guard is malformed, not a proof.
        assert_eq!(
            check(&c, &[transfer(&[("from", "from")])], &[q(1); 2]),
            Err(Failure::Malformed)
        );
    }

    #[test]
    fn cons_three_key_writer_checks_every_case() {
        // Bal[a] -= x; Bal[b] += y; Bal[c] += x - y: balanced when distinct.
        let c = keyed(&["a", "b", "c"]);
        let writer = |distinct: &[(&str, &str)]| Writer {
            id: "split".into(),
            delta: Delta::Affine(vec![
                vec![q(-1), q(0), q(0)],
                vec![q(0), q(1), q(0)],
                vec![q(1), q(-1), q(0)],
            ]),
            distinct: distinct
                .iter()
                .map(|(a, b)| ((*a).into(), (*b).into()))
                .collect(),
        };
        let all = [("a", "b"), ("b", "c"), ("a", "c")];
        assert_eq!(check(&c, &[writer(&all)], &[q(1); 3]), Ok(()));
        // Two guards leave a == c open.
        let refusal = check(&c, &[writer(&all[..2])], &[q(1); 3]);
        assert!(
            matches!(&refusal, Err(Failure::Aliased { .. })),
            "{refusal:?}"
        );
        let refusal = check(&c, &[writer(&[("a", "b"), ("a", "c")])], &[q(1); 3]).unwrap_err();
        let Failure::Aliased { cells, .. } = refusal else {
            panic!("expected an aliasing refusal, got {refusal:?}");
        };
        assert_eq!(cells.len(), 1);
        assert_eq!(cells[0].coordinates, ["Bal[b]", "Bal[c]"]);
        // Unguarded: refused as well; all 13 aliasing cases are within bound.
        assert!(matches!(
            check(&c, &[writer(&[])], &[q(1); 3]),
            Err(Failure::Aliased { .. })
        ));
    }

    #[test]
    fn cons_aliasing_past_the_bound_is_unknown_and_keys_are_validated() {
        let bindings = ["k0", "k1", "k2", "k3", "k4", "k5", "k6"];
        let c = keyed(&bindings);
        let mut rows = vec![vec![q(1), q(0)]; bindings.len()];
        rows[0] = vec![q(-6), q(0)];
        let writer = Writer {
            id: "fan_out".into(),
            delta: Delta::Affine(rows),
            distinct: Vec::new(),
        };
        let refusal = check(&c, std::slice::from_ref(&writer), &[q(1); 7]);
        assert!(
            matches!(&refusal, Err(Failure::Unknown(why)) if why.starts_with(ALIAS_BOUND_CODE)),
            "{refusal:?}"
        );
        // Six keys fit the per-family limit, but not the case bound.
        let c6 = keyed(&bindings[..6]);
        let mut rows = vec![vec![q(1), q(0)]; 6];
        rows[0] = vec![q(-5), q(0)];
        let writer6 = Writer {
            delta: Delta::Affine(rows),
            ..writer
        };
        assert!(matches!(
            check(&c6, &[writer6], &[q(1); 6]),
            Err(Failure::Unknown(why)) if why.starts_with(ALIAS_BOUND_CODE)
        ));
        // Duplicate cells and unequal weights within one family are malformed.
        let mut dup = keyed(&["from", "to"]);
        dup[1].key = dup[0].key.clone();
        assert_eq!(
            check(&dup, &[transfer(&[])], &[q(1); 2]),
            Err(Failure::Malformed)
        );
        let mut uneven = keyed(&["from", "to"]);
        uneven[1].weight = q(2);
        assert_eq!(
            check(&uneven, &[transfer(&[])], &[q(1), q(2)]),
            Err(Failure::Malformed)
        );
    }

    proptest! {
        #[test]
        fn cons_linear_fractional_balance(n in -10000_i64..10000, d in 1_i64..10000, amount in -10000_i64..10000) {
            let r = Rational::new(n, d).unwrap();
            let neg = Rational::new(n.checked_neg().unwrap(), d).unwrap();
            prop_assert_eq!(r.add(neg), Some(q(0)));
            prop_assert_eq!(r.mul(q(amount)).unwrap().add(neg.mul(q(amount)).unwrap()), Some(q(0)));
            let (mut c, mut w) = system();
            for coordinate in &mut c { coordinate.weight = Rational::new(1, d).unwrap(); }
            prop_assert_eq!(check(&c, &w, &[Rational::new(1, d).unwrap(); 3]), Ok(()));
            let Delta::Affine(rows) = &mut w[0].delta else { unreachable!() };
            rows[2][2] = q(amount);
            prop_assert_eq!(check(&c, &w, &[Rational::new(1, d).unwrap(); 3]).is_ok(), amount == 0);
        }
    }
}
