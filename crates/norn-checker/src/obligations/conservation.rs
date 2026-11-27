//! CONS-LINEAR-1: exact coefficient checking, conditional on a trusted lowering.
//!
//! This module accepts a mathematical transition system, not declarations. Its
//! success is not application evidence: the source-bound adapter must establish
//! complete writer coverage, the declared functional and faithful affine deltas.
use std::collections::{BTreeMap, BTreeSet};

use norn_expr::{RowComparison, RowExpr};
use norn_ir::function::Digest;
use norn_ir::runtime::RuntimeValue;
use norn_ir::{ActionRoot, ArtifactEnvelope, ArtifactKind, UnitRef, ZenExpression};

/// The extracted, proved acceptance decision for one writer (#718 U5).
pub mod decision;
/// Untrusted exact equality-feasibility discovery; candidates require checking.
pub mod discovery;
/// Exact-rational auto-lowering from compiled ledger packs and quantity actions.
pub mod ledger;

pub use crate::exact_rational::Rational;
pub use ledger::{
    LedgerPackCertification, LoweredLedgerSystem, QuantityActionsCertification,
    certify_ledger_pack, certify_ledger_pack_ir, certify_ledger_pack_seal,
    certify_quantity_actions, lower_ledger_pack, lower_quantity_action,
};

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
    /// The stored row member this coordinate reads and writes, or `None` for
    /// a purely mathematical coordinate that is always its own cell. A system
    /// mixing the two cannot be checked for aliasing and is unknown.
    pub cell: Option<Cell>,
}

/// The storage identity of a coordinate such as `Account.balance[from]`.
///
/// Rows are identified the way the kernel identifies them: the root type of
/// the object's hierarchy plus the key (`CommitModel::canonical`). Labels,
/// scope bindings (book, currency) and units never separate storage, so two
/// coordinates of one root and member may name one cell at run time, and
/// CONS-LINEAR-1 checks every case a writer's preconditions do not exclude.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Cell {
    /// The object type the writer names (`Wallet`).
    pub object_type: String,
    /// The root of `object_type`'s hierarchy (`Account`); a parent and a
    /// child type with one key are one row.
    pub root: String,
    /// The stored property (`balance`).
    pub member: String,
    /// The row key.
    pub key: CellKey,
}

/// How a cell's row key is computed.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum CellKey {
    /// A constant canonical key.
    Fixed(String),
    /// The effective action input field `input.<field>` (after the action's
    /// derivations, as its preconditions see it), used verbatim as the
    /// canonical key. A repeated field is one key. A key that normalizes,
    /// converts or looks up its input is [`CellKey::Computed`].
    Input(String),
    /// Any other key expression, labelled for diagnostics. It may equal any
    /// key, including another computed key with the same label.
    Computed(String),
}

/// Diagnostic code of [`Failure::Aliased`]: a writer balances only while its
/// coordinates name distinct cells.
pub const KEY_ALIAS_CODE: &str = "law.conservation.key_alias";
/// Diagnostic code prefixing the [`Failure::Unknown`] reason when a writer has
/// more key-aliasing cases than the checker enumerates.
pub const ALIAS_BOUND_CODE: &str = "law.conservation.alias_bound";
/// Diagnostic code prefixing the [`Failure::Unknown`] reason when a possible
/// collision cannot be checked faithfully: cells that may be one row carry
/// different weights, scopes or units, or only some coordinates are stored.
pub const ALIAS_UNLOWERABLE_CODE: &str = "law.conservation.alias_unlowerable";
/// Most key-aliasing cases checked per writer, the all-distinct case included.
pub const MAX_ALIAS_CASES: usize = 256;
/// Most assigned coordinates one root and member of a writer may touch.
pub const MAX_ALIASED_KEYS: usize = 6;
/// The expression root of action input fields in preconditions.
const INPUT_ROOT: &str = "input";

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
    /// The write footprint: ids of every coordinate this writer assigns,
    /// including assignments that leave the value unchanged
    /// (`Bal[k] := pre.Bal[k]`). Such a write can still overwrite another
    /// write to the same cell. Every coordinate with a nonzero delta row must
    /// be listed.
    pub assigns: Vec<String>,
    /// The executable definition this writer was lowered from. Only a bound
    /// action's own preconditions can exclude a key-equality case.
    pub source: WriterSource,
}

/// The executable writer a [`Writer`]'s delta was lowered from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriterSource {
    /// No executable definition is bound (mined observations, discovery
    /// candidates): every key-equality case stays possible.
    Unbound,
    /// The action definition the writer's delta was lowered from. Its id
    /// must be the writer's. Its guards count only when it is the exact
    /// revision [`InstalledActions`] pins for that id; otherwise the writer
    /// is checked as [`WriterSource::Unbound`].
    Action(BoundAction),
}

/// The guards of one action definition, with its content digest.
///
/// It has no public constructor other than [`BoundAction::of`], so no caller
/// can attach a precondition outside an [`ActionRoot`]. That root is still
/// the caller's: its guards exclude a key-equality case only when its digest
/// equals the [`InstalledActions`] pin for its id, so the root is the exact
/// revision the kernel checks over the effective input before any effect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundAction {
    action_id: String,
    /// `None` when the root does not canonicalize; it then matches no pin.
    digest: Option<Digest>,
    preconditions: Vec<ZenExpression>,
}

impl BoundAction {
    /// Binds an action definition, pinned by its content digest.
    #[must_use]
    pub fn of(action: &ActionRoot) -> Self {
        Self {
            action_id: action.action_id.clone(),
            digest: action_digest(action),
            preconditions: action.preconditions.clone(),
        }
    }

    /// The bound action's id.
    #[must_use]
    pub fn action_id(&self) -> &str {
        &self.action_id
    }
}

/// The exact content digest of an action definition: the SHA-256 of its
/// typed root's exact canonical JSON
/// ([`norn_canonical::canonical_json_exact_of`]). It does not normalize
/// Unicode: ZEN compares member names and string literals byte for byte, so
/// two roots share it only when every field the kernel reads, preconditions
/// included, has the same bytes.
fn action_digest(action: &ActionRoot) -> Option<Digest> {
    use sha2::{Digest as _, Sha256};
    let bytes = norn_canonical::canonical_json_exact_of(action).ok()?;
    Some(Digest::from_hex(&format!("{:x}", Sha256::digest(bytes))))
}

/// The installed action revisions, by id, pinned by content digest.
///
/// The only source of trust for a [`BoundAction`]'s guards. It is read from
/// an installed definition set's canonical artifact list
/// (`InstalledDefinitionSet::canonical_set`), decoded the way the runtime
/// loads the actions it executes, so a pin names the revision the kernel
/// evaluates, never one a caller supplies beside it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InstalledActions {
    pins: BTreeMap<String, Digest>,
}

impl InstalledActions {
    /// No installed action: every bound writer is checked as unbound.
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }

    /// Pins the actions of an installed set's canonical artifact list.
    ///
    /// # Errors
    ///
    /// [`Failure::Malformed`] when the list or an action does not decode, or
    /// two actions share an id.
    pub fn from_canonical_set(canonical_set: &str) -> Result<Self, Failure> {
        let Ok(RuntimeValue::Array(sources)) = RuntimeValue::from_json_text(canonical_set) else {
            return Err(Failure::Malformed);
        };
        let mut actions = Vec::new();
        for source in &sources {
            let artifact: ArtifactEnvelope =
                norn_ir::runtime::decode(source).map_err(|_undecodable| Failure::Malformed)?;
            if artifact.kind() == ArtifactKind::Action {
                let payload = source.get("payload").ok_or(Failure::Malformed)?;
                actions.push(
                    norn_ir::runtime::decode::<ActionRoot>(payload)
                        .map_err(|_undecodable| Failure::Malformed)?,
                );
            }
        }
        Self::pin(&actions)
    }

    fn pin(actions: &[ActionRoot]) -> Result<Self, Failure> {
        let mut pins = BTreeMap::new();
        for action in actions {
            let digest = action_digest(action).ok_or(Failure::Malformed)?;
            if pins.insert(action.action_id.clone(), digest).is_some() {
                return Err(Failure::Malformed);
            }
        }
        Ok(Self { pins })
    }

    /// The preconditions of `source` when it is the installed revision of
    /// its action; `None` keeps every key-equality case.
    fn guards<'a>(&self, source: &'a WriterSource) -> Option<&'a [ZenExpression]> {
        let WriterSource::Action(action) = source else {
            return None;
        };
        let pinned = self.pins.get(&action.action_id)?;
        (action.digest.as_ref() == Some(pinned)).then_some(action.preconditions.as_slice())
    }
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
    /// The writer balances while its coordinates are distinct cells, but not
    /// when some of them name one cell and their writes combine.
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

/// One runtime cell named by several coordinates in an aliasing case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AliasedCell {
    /// Coordinate ids that name one cell in this case.
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
    // Each named object type resolves to one root.
    let mut roots = BTreeMap::new();
    for cell in coordinates.iter().filter_map(|c| c.cell.as_ref()) {
        let key = match &cell.key {
            CellKey::Fixed(key) | CellKey::Input(key) | CellKey::Computed(key) => key,
        };
        if cell.object_type.is_empty()
            || cell.root.is_empty()
            || cell.member.is_empty()
            || key.is_empty()
            || *roots.entry(&cell.object_type).or_insert(&cell.root) != &cell.root
        {
            return Err(malformed());
        }
    }
    let mut writer_ids = BTreeSet::new();
    for writer in writers {
        let mut assigned = BTreeSet::new();
        if writer.id.is_empty()
            || !writer_ids.insert(&writer.id)
            || matches!(&writer.source, WriterSource::Action(action) if action.action_id != writer.id)
            || writer
                .assigns
                .iter()
                .any(|id| !ids.contains(id) || !assigned.insert(id))
        {
            return Err(malformed());
        }
    }
    Ok(())
}

/// Input-field pairs, ordered, that an installed action's preconditions
/// prove unequal. Only top-level conjuncts `input.a != input.b` and
/// `not (input.a == input.b)` count; source outside the lowerable subset, an
/// unbound writer and an action that is not the installed revision prove
/// nothing.
fn proved_unequal(guards: Option<&[ZenExpression]>) -> BTreeSet<(String, String)> {
    fn pair(a: &RowExpr, b: &RowExpr, out: &mut BTreeSet<(String, String)>) {
        if let (RowExpr::Column(a), RowExpr::Column(b)) = (a, b)
            && a != b
        {
            out.insert((a.min(b).clone(), a.max(b).clone()));
        }
    }
    fn conjuncts(tree: &RowExpr, out: &mut BTreeSet<(String, String)>) {
        match tree {
            RowExpr::And(a, b) => {
                conjuncts(a, out);
                conjuncts(b, out);
            }
            RowExpr::Compare(RowComparison::NotEqual, a, b) => pair(a, b, out),
            RowExpr::Not(inner) => {
                if let RowExpr::Compare(RowComparison::Equal, a, b) = &**inner {
                    pair(a, b, out);
                }
            }
            // A disjunction, a negation of anything else, or any other
            // conjunct proves no inequality.
            RowExpr::Null
            | RowExpr::Bool(_)
            | RowExpr::Number(_)
            | RowExpr::Text(_)
            | RowExpr::Column(_)
            | RowExpr::Negate(_)
            | RowExpr::Arithmetic(..)
            | RowExpr::Compare(..)
            | RowExpr::Or(..)
            | RowExpr::Coalesce(..)
            | RowExpr::Conditional(..)
            | RowExpr::CalendarDate(_)
            | RowExpr::Timestamp(_) => {}
        }
    }
    let mut out = BTreeSet::new();
    for precondition in guards.unwrap_or_default() {
        if let Ok(Some(tree)) = norn_expr::row_expression(precondition.as_str(), INPUT_ROOT) {
            conjuncts(&tree, &mut out);
        }
    }
    out
}

/// Every partition of `members` into equal-key blocks that keeps each
/// proved-unequal pair apart, in a fixed order. `None` past `limit`.
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

/// Why no aliasing case set exists: the code and its detail.
type AliasLimit = (&'static str, String);

/// Whether two keys of one root and member are always one row.
fn same_key(a: &CellKey, b: &CellKey) -> bool {
    match (a, b) {
        (CellKey::Fixed(a), CellKey::Fixed(b)) | (CellKey::Input(a), CellKey::Input(b)) => a == b,
        _ => false,
    }
}

/// Whether two keys of one root and member are never one row: different
/// constants, or input fields the preconditions prove unequal. A computed
/// key, or an input key beside a constant, may be equal.
fn never_same_key(a: &CellKey, b: &CellKey, unequal: &BTreeSet<(String, String)>) -> bool {
    match (a, b) {
        (CellKey::Fixed(a), CellKey::Fixed(b)) => a != b,
        (CellKey::Input(a), CellKey::Input(b)) => {
            unequal.contains(&(a.min(b).clone(), a.max(b).clone()))
        }
        _ => false,
    }
}

/// The key-equality partitions of one storage group's assigned coordinates,
/// each as its merged cells. Coordinates that are always one row stay
/// together; proved-distinct keys stay apart.
fn group_partitions(
    coordinates: &[Coordinate],
    group: &[usize],
    unequal: &BTreeSet<(String, String)>,
) -> Result<Vec<Vec<Vec<usize>>>, AliasLimit> {
    let cell = |i: usize| coordinates.get(i).and_then(|c| c.cell.as_ref());
    // Coordinates that are always one row form one atom.
    let mut atoms: Vec<Vec<usize>> = Vec::new();
    for &index in group {
        let key = cell(index).map(|c| &c.key);
        let atom = atoms.iter_mut().find(|atom| {
            let first = atom.first().and_then(|&i| cell(i)).map(|c| &c.key);
            matches!((first, key), (Some(a), Some(b)) if same_key(a, b))
        });
        match atom {
            Some(atom) => atom.push(index),
            None => atoms.push(vec![index]),
        }
    }
    let apart = |a: usize, b: usize| {
        let key = |atom: usize| {
            atoms
                .get(atom)
                .and_then(|members| members.first())
                .and_then(|&i| cell(i))
                .map(|c| &c.key)
        };
        matches!((key(a), key(b)), (Some(a), Some(b)) if never_same_key(a, b, unequal))
    };
    let indices: Vec<usize> = (0..atoms.len()).collect();
    let partitions = key_partitions(&indices, &apart, MAX_ALIAS_CASES).ok_or_else(|| {
        (
            ALIAS_BOUND_CODE,
            format!("more than {MAX_ALIAS_CASES} key-equality cases"),
        )
    })?;
    // Each partition as its merged cells: blocks naming two or more coordinates.
    Ok(partitions
        .iter()
        .map(|partition| {
            partition
                .iter()
                .filter_map(|block| {
                    let mut members: Vec<usize> = block
                        .iter()
                        .filter_map(|&atom| atoms.get(atom))
                        .flatten()
                        .copied()
                        .collect();
                    members.sort_unstable();
                    (members.len() > 1).then_some(members)
                })
                .collect()
        })
        .collect())
}

/// The aliasing cases of one writer beyond the all-distinct one: each case is
/// a list of merged cells (coordinate indices, at least two) with the choice of
/// how they combine (`Some(i)`: index `i` of the cell survives; `None`: the
/// writes accumulate). Only assigned coordinates write: a zero-delta
/// assignment may still be the write that survives.
///
/// A write also changes every coordinate of the functional, assigned or not,
/// that may name its row. Each merged case lands the row's delta on one of
/// its coordinates and counts the row once, which is faithful only while
/// every coordinate that may name an assigned row carries that row's weight,
/// scope and unit; otherwise the writer is unlowerable.
fn alias_cases(
    coordinates: &[Coordinate],
    footprint: &BTreeSet<usize>,
    unequal: &BTreeSet<(String, String)>,
) -> Result<Vec<AliasCase>, AliasLimit> {
    let bound = |why: String| (ALIAS_BOUND_CODE, why);
    // Every stored coordinate grouped by canonical storage: root and member.
    let mut storage: BTreeMap<(&str, &str), Vec<usize>> = BTreeMap::new();
    for (index, coordinate) in coordinates.iter().enumerate() {
        if let Some(cell) = &coordinate.cell {
            storage
                .entry((&cell.root, &cell.member))
                .or_default()
                .push(index);
        }
    }
    // Each assigned row is counted the same way by every coordinate that may
    // name it, whether or not the writer assigns that coordinate.
    for group in storage.values() {
        for &i in group.iter().filter(|i| footprint.contains(i)) {
            for &j in group.iter().filter(|&&j| j != i) {
                let (Some(a), Some(b)) = (coordinates.get(i), coordinates.get(j)) else {
                    continue;
                };
                let (Some(left), Some(right)) = (&a.cell, &b.cell) else {
                    continue;
                };
                if !never_same_key(&left.key, &right.key, unequal)
                    && (a.weight, &a.scope, &a.unit) != (b.weight, &b.scope, &b.unit)
                {
                    return Err((
                        ALIAS_UNLOWERABLE_CODE,
                        format!(
                            "{} is assigned and may be the row of {}, which differs in weight, scope or unit",
                            a.id, b.id
                        ),
                    ));
                }
            }
        }
    }
    // Merged-cell sets per combination of the assigned coordinates' partitions.
    let mut merges: Vec<Vec<Vec<usize>>> = vec![Vec::new()];
    let assigned = storage
        .into_values()
        .map(|group| -> Vec<usize> {
            group
                .into_iter()
                .filter(|i| footprint.contains(i))
                .collect()
        })
        .filter(|g| g.len() > 1);
    for group in assigned {
        if group.len() > MAX_ALIASED_KEYS {
            return Err(bound(format!(
                "{} assigned coordinates of one stored member (limit {MAX_ALIASED_KEYS})",
                group.len()
            )));
        }
        let partitions = group_partitions(coordinates, &group, unequal)?;
        let mut next = Vec::new();
        for merge in &merges {
            for partition in &partitions {
                let mut combined = merge.clone();
                combined.extend(partition.iter().cloned());
                next.push(combined);
                if next.len() > MAX_ALIAS_CASES {
                    return Err(bound(format!(
                        "more than {MAX_ALIAS_CASES} key-equality cases"
                    )));
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
                return Err(bound(format!(
                    "more than {MAX_ALIAS_CASES} key-aliasing cases"
                )));
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
///
/// `installed` pins the action revisions the kernel executes; a bound
/// writer's guards exclude key-equality cases only when its action is the
/// pinned revision.
pub fn check(
    coordinates: &[Coordinate],
    writers: &[Writer],
    certificate: &[Rational],
    installed: &InstalledActions,
) -> Result<(), Failure> {
    validate(coordinates, writers, certificate)?;
    // A coordinate without storage beside stored ones may be any of their
    // rows; that collision cannot be checked.
    let stored = coordinates.iter().filter(|c| c.cell.is_some()).count();
    if stored != 0 && stored != coordinates.len() {
        return Err(Failure::Unknown(format!(
            "{ALIAS_UNLOWERABLE_CODE}: {} of {} coordinates name no stored cell",
            coordinates.len().saturating_sub(stored),
            coordinates.len()
        )));
    }
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
        // The footprint must cover every changed coordinate.
        let footprint: BTreeSet<usize> = coordinates
            .iter()
            .enumerate()
            .filter(|(_, c)| writer.assigns.contains(&c.id))
            .map(|(index, _)| index)
            .collect();
        if rows
            .iter()
            .enumerate()
            .any(|(index, row)| row.iter().any(|q| q.numerator != 0) && !footprint.contains(&index))
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
        // Assigned cells the preconditions do not prove distinct may be one
        // row at run time; the writer must balance in every such case too.
        // A case set that cannot be built faithfully is unknown first.
        let unequal = proved_unequal(installed.guards(&writer.source));
        let cases = alias_cases(coordinates, &footprint, &unequal)
            .map_err(|(code, why)| Failure::Unknown(format!("{code}: {}: {why}", writer.id)))?;
        decide(rows)?;
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
    let id = |i: usize| coordinates.get(i).map_or_default(|c| c.id.clone());
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
    /// [`super::check`] with the writers' own bound actions installed: the
    /// fixtures model the installed revisions. The pin itself is tested in
    /// `cons_bound_action_must_be_the_installed_revision`.
    fn check(c: &[Coordinate], w: &[Writer], certificate: &[Rational]) -> Result<(), Failure> {
        let pins = w
            .iter()
            .filter_map(|writer| match &writer.source {
                WriterSource::Action(action) => {
                    Some((action.action_id.clone(), action.digest.clone()?))
                }
                WriterSource::Unbound => None,
            })
            .collect();
        super::check(c, w, certificate, &InstalledActions { pins })
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
                cell: None,
            })
            .collect();
        let writer = Writer {
            id: "convert".into(),
            delta: Delta::Affine(vec![
                vec![q(-1), q(0), q(0)],
                vec![q(0), q(1), q(0)],
                vec![q(1), q(-1), q(0)],
            ]),
            assigns: vec!["input".into(), "output".into(), "loss".into()],
            source: WriterSource::Unbound,
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

    /// One stored member of `root`, written through `object_type`.
    fn stored(id: &str, object_type: &str, root: &str, key: CellKey) -> Coordinate {
        Coordinate {
            id: id.into(),
            scope: BTreeMap::from([("book".into(), "primary".into())]),
            unit: UnitRef {
                system: UnitSystem::Ucum,
                code: "USD".into(),
            },
            weight: q(1),
            cell: Some(Cell {
                object_type: object_type.into(),
                root: root.into(),
                member: "balance".into(),
                key,
            }),
        }
    }
    /// `Account.balance[input.k]` coordinates; columns are `(amount, constant)`.
    fn keyed(bindings: &[&str]) -> Vec<Coordinate> {
        bindings
            .iter()
            .map(|binding| {
                stored(
                    &format!("Bal[{binding}]"),
                    "Account",
                    "Account",
                    CellKey::Input((*binding).into()),
                )
            })
            .collect()
    }
    /// The executable action `id` with exactly these preconditions.
    fn action(id: &str, guard: &[&str]) -> ActionRoot {
        serde_json::from_value(serde_json::json!({
            "actionId": id,
            "scopeRequired": [],
            "idempotent": false,
            "ledgered": false,
            "inputSchema": {"type": "object"},
            "outputSchema": {"type": "object"},
            "preconditions": guard,
        }))
        .unwrap()
    }
    /// A writer lowered from the action `id` whose preconditions are `guard`.
    fn writer(id: &str, rows: Vec<Vec<Rational>>, c: &[Coordinate], guard: &[&str]) -> Writer {
        Writer {
            id: id.into(),
            delta: Delta::Affine(rows),
            assigns: c.iter().map(|c| c.id.clone()).collect(),
            source: WriterSource::Action(BoundAction::of(&action(id, guard))),
        }
    }
    /// `Bal[from] := Bal[from] - amount; Bal[to] := Bal[to] + amount`.
    fn transfer(c: &[Coordinate], guard: &[&str]) -> Writer {
        writer(
            "transfer",
            vec![vec![q(-1), q(0)], vec![q(1), q(0)]],
            c,
            guard,
        )
    }

    #[test]
    fn cons_same_account_transfer_is_refused() {
        // #1457: with from == to both writes hit one row; the credit computed
        // from the pre-state overwrites the debit and mints `amount`.
        let c = keyed(&["from", "to"]);
        let refusal = check(&c, &[transfer(&c, &[])], &[q(1); 2]).unwrap_err();
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
    fn cons_guarded_transfer_is_admitted() {
        let c = keyed(&["from", "to"]);
        for guard in [
            "input.from != input.to",
            "input.to != input.from",
            "not (input.from == input.to)",
            "input.amount > 0 and input.from != input.to",
        ] {
            assert_eq!(
                check(&c, &[transfer(&c, &[guard])], &[q(1); 2]),
                Ok(()),
                "{guard}"
            );
        }
        // The guard may be any one of the preconditions.
        assert_eq!(
            check(
                &c,
                &[transfer(
                    &c,
                    &["input.amount > 0", "input.from != input.to"]
                )],
                &[q(1); 2]
            ),
            Ok(())
        );
    }

    #[test]
    fn cons_inequality_comes_only_from_lowered_preconditions() {
        // A caller cannot claim `from != to`: only a precondition the checker
        // lowers to `input.from != input.to` excludes the case.
        let c = keyed(&["from", "to"]);
        for guard in [
            "input.amount > 0",
            "input.from != input.fee",
            "input.from != input.to or input.amount > 0",
            "not (input.from != input.to)",
            "self.from != self.to",
            "input.from != input.to and len(input.memo) > 0",
            "input.from !=",
        ] {
            assert!(
                matches!(
                    check(&c, &[transfer(&c, &[guard])], &[q(1); 2]),
                    Err(Failure::Aliased { .. })
                ),
                "{guard}"
            );
        }
        // Computed keys stay possibly equal whatever the guard says.
        let computed: Vec<_> = ["from", "to"]
            .iter()
            .map(|k| {
                stored(
                    &format!("Bal[{k}]"),
                    "Account",
                    "Account",
                    CellKey::Computed(format!("lower(input.{k})")),
                )
            })
            .collect();
        assert!(matches!(
            check(
                &computed,
                &[transfer(&computed, &["input.from != input.to"])],
                &[q(1); 2]
            ),
            Err(Failure::Aliased { .. })
        ));
        // A repeated input field is one key: Bal[from] under two labels is
        // one row, so the debit and credit of `from` collide.
        let mut repeated = keyed(&["from", "to"]);
        repeated[1].cell.as_mut().unwrap().key = CellKey::Input("from".into());
        assert!(matches!(
            check(
                &repeated,
                &[transfer(&repeated, &["input.from != input.to"])],
                &[q(1); 2]
            ),
            Err(Failure::Aliased { .. })
        ));
    }

    #[test]
    fn cons_guards_come_only_from_the_bound_action() {
        // The executable transfer has no guard. Nothing a caller says about
        // it can exclude from == to: the writer carries no precondition of
        // its own, only the action it was lowered from.
        let c = keyed(&["from", "to"]);
        let unguarded = transfer(&c, &[]);
        assert!(matches!(
            check(&c, std::slice::from_ref(&unguarded), &[q(1); 2]),
            Err(Failure::Aliased { .. })
        ));
        // Binding another action that does carry the guard is malformed:
        // the bound action must be the writer's own.
        let mut borrowed = unguarded.clone();
        borrowed.source = WriterSource::Action(BoundAction::of(&action(
            "guarded_transfer",
            &["input.from != input.to"],
        )));
        assert_eq!(check(&c, &[borrowed], &[q(1); 2]), Err(Failure::Malformed));
        // An unbound writer keeps every equality case.
        let mut unbound = unguarded;
        unbound.source = WriterSource::Unbound;
        assert!(matches!(
            check(&c, &[unbound], &[q(1); 2]),
            Err(Failure::Aliased { .. })
        ));
        // The genuinely guarded action is admitted.
        let guarded = transfer(&c, &["input.from != input.to"]);
        assert_eq!(
            BoundAction::of(&action("transfer", &[])).action_id(),
            "transfer"
        );
        assert_eq!(check(&c, &[guarded], &[q(1); 2]), Ok(()));
    }

    #[test]
    fn cons_bound_action_must_be_the_installed_revision() {
        // Sol round 3: the installed transfer has no guard. A clone of its
        // root with `input.from != input.to` added, bound under the same id,
        // is not the revision the kernel executes and excludes nothing.
        let c = keyed(&["from", "to"]);
        let unguarded = action("transfer", &[]);
        let installed = InstalledActions::pin(std::slice::from_ref(&unguarded)).unwrap();
        let mut forged = unguarded;
        forged
            .preconditions
            .push(ZenExpression("input.from != input.to".into()));
        let mut probe = transfer(&c, &[]);
        probe.source = WriterSource::Action(BoundAction::of(&forged));
        assert!(matches!(
            super::check(&c, std::slice::from_ref(&probe), &[q(1); 2], &installed),
            Err(Failure::Aliased { .. })
        ));
        // Without any installed pin the guard counts no more.
        assert!(matches!(
            super::check(
                &c,
                std::slice::from_ref(&probe),
                &[q(1); 2],
                &InstalledActions::none()
            ),
            Err(Failure::Aliased { .. })
        ));
        // The genuinely installed guarded transfer is admitted.
        let guarded = action("transfer", &["input.from != input.to"]);
        let installed = InstalledActions::pin(std::slice::from_ref(&guarded)).unwrap();
        assert_eq!(
            super::check(
                &c,
                &[transfer(&c, &["input.from != input.to"])],
                &[q(1); 2],
                &installed
            ),
            Ok(())
        );
        // The pin is read from an installed canonical set, decoded the way
        // the runtime loads the actions it executes.
        let set = include_str!("../../../norn-kernel/tests/fixtures/action-writer.json");
        let installed = InstalledActions::from_canonical_set(set).unwrap();
        let sources: Vec<serde_json::Value> = serde_json::from_str(set).unwrap();
        let root: ActionRoot = sources
            .iter()
            .find_map(|source| serde_json::from_value(source.get("payload")?.clone()).ok())
            .unwrap();
        let genuine = WriterSource::Action(BoundAction::of(&root));
        assert!(installed.guards(&genuine).is_some());
        let mut clone = root;
        clone
            .preconditions
            .push(ZenExpression("input.from != input.to".into()));
        assert_eq!(
            installed.guards(&WriterSource::Action(BoundAction::of(&clone))),
            None
        );
        assert_eq!(installed.guards(&WriterSource::Unbound), None);
        // An ambiguous or undecodable installed set pins nothing.
        assert_eq!(
            InstalledActions::pin(&[action("t", &[]), action("t", &[])]),
            Err(Failure::Malformed)
        );
        assert_eq!(
            InstalledActions::from_canonical_set("{}"),
            Err(Failure::Malformed)
        );
    }

    #[test]
    fn cons_bound_action_digest_is_exact_not_nfc() {
        // Sol round 4: ZEN compares member names and string literals byte for
        // byte, so an NFC-equivalent edit is a different revision.
        let (composed, decomposed) = ("\u{e9}", "e\u{301}");
        // A member name: the checker keys the coordinate by the decomposed
        // spelling, which only the edited guard names.
        let c = keyed(&[decomposed, "to"]);
        let original = action("transfer", &[&format!("input['{composed}'] != input.to")]);
        let edited = action("transfer", &[&format!("input['{decomposed}'] != input.to")]);
        let bind = |root: &ActionRoot| {
            let mut writer = transfer(&c, &[]);
            writer.source = WriterSource::Action(BoundAction::of(root));
            writer
        };
        assert_ne!(
            BoundAction::of(&original).digest,
            BoundAction::of(&edited).digest
        );
        let installed = InstalledActions::pin(std::slice::from_ref(&original)).unwrap();
        assert!(matches!(
            super::check(&c, &[bind(&edited)], &[q(1); 2], &installed),
            Err(Failure::Aliased { .. })
        ));
        // Control: installed as itself, the edited guard does exclude.
        let installed = InstalledActions::pin(std::slice::from_ref(&edited)).unwrap();
        assert_eq!(
            super::check(&c, &[bind(&edited)], &[q(1); 2], &installed),
            Ok(())
        );
        // A string literal: `'é' == 'é'` in its two encodings.
        let c = keyed(&["from", "to"]);
        let guard = |right: &str| {
            action(
                "transfer",
                &[
                    "input.from != input.to",
                    &format!("'{composed}' == '{right}'"),
                ],
            )
        };
        let (original, edited) = (guard(composed), guard(decomposed));
        assert_ne!(
            BoundAction::of(&original).digest,
            BoundAction::of(&edited).digest
        );
        let installed = InstalledActions::pin(std::slice::from_ref(&original)).unwrap();
        let mut writer = transfer(&c, &[]);
        writer.source = WriterSource::Action(BoundAction::of(&edited));
        assert!(matches!(
            super::check(&c, &[writer], &[q(1); 2], &installed),
            Err(Failure::Aliased { .. })
        ));
    }

    #[test]
    fn cons_unassigned_alias_cannot_escape() {
        // Sol round 2: two labels on one fixed row, weights [0, 1], and the
        // writer assigns only the zero-weight label. The physical write
        // raises the weight-1 label too, so the total changes by 1.
        let one_row = || {
            vec![
                stored("cash", "Account", "Account", CellKey::Fixed("cash".into())),
                stored(
                    "wallet_cash",
                    "Wallet",
                    "Account",
                    CellKey::Fixed("cash".into()),
                ),
            ]
        };
        let raise = |c: &[Coordinate]| Writer {
            id: "raise".into(),
            delta: Delta::Affine(vec![vec![q(1)], vec![q(0)]]),
            assigns: vec![c[0].id.clone()],
            source: WriterSource::Action(BoundAction::of(&action("raise", &[]))),
        };
        let mut c = one_row();
        c[0].weight = q(0);
        let unlowerable = |c: &[Coordinate]| {
            matches!(
                check(c, &[raise(c)], &[c[0].weight, c[1].weight]),
                Err(Failure::Unknown(why)) if why.starts_with(ALIAS_UNLOWERABLE_CODE)
            )
        };
        assert!(unlowerable(&c));
        // A computed key that may reach the row escapes no better, from
        // either side.
        let mut reach = c.clone();
        reach[1].cell.as_mut().unwrap().key = CellKey::Computed("route(input.to)".into());
        assert!(unlowerable(&reach));
        let mut reach = c.clone();
        reach[0].cell.as_mut().unwrap().key = CellKey::Computed("route(input.to)".into());
        assert!(unlowerable(&reach));
        // An input key may equal the constant row as well.
        let mut input = c.clone();
        input[1].cell.as_mut().unwrap().key = CellKey::Input("to".into());
        assert!(unlowerable(&input));
        // A label that can never be the row may carry any weight.
        let mut apart = c;
        apart[1].cell.as_mut().unwrap().key = CellKey::Fixed("bank".into());
        apart[1].weight = q(5);
        assert_eq!(check(&apart, &[raise(&apart)], &[q(0), q(5)]), Ok(()));
        // With equal weights the write is counted once and must balance:
        // raising one row is a refusal, a move between rows is admitted
        // even beside an unassigned label that may name either.
        let even = one_row();
        assert!(matches!(
            check(&even, &[raise(&even)], &[q(1); 2]),
            Err(Failure::Unbalanced { .. })
        ));
        let mut moved = one_row();
        moved[1].cell.as_mut().unwrap().key = CellKey::Fixed("bank".into());
        moved.push(stored(
            "routed",
            "Wallet",
            "Account",
            CellKey::Computed("route(input.to)".into()),
        ));
        let shift = Writer {
            id: "shift".into(),
            delta: Delta::Affine(vec![vec![q(1)], vec![q(-1)], vec![q(0)]]),
            assigns: vec!["cash".into(), "wallet_cash".into()],
            source: WriterSource::Unbound,
        };
        assert_eq!(
            check(&moved, std::slice::from_ref(&shift), &[q(1); 3]),
            Ok(())
        );
        // ... but not when the label that may name a written row is weighted
        // differently.
        moved[2].weight = q(2);
        assert!(matches!(
            check(&moved, &[shift], &[q(1), q(1), q(2)]),
            Err(Failure::Unknown(why)) if why.starts_with(ALIAS_UNLOWERABLE_CODE)
        ));
    }

    #[test]
    fn cons_zero_delta_write_can_overwrite() {
        // Bal[from] -= amount; Bal[to] += amount; Bal[k] := pre.Bal[k].
        // The last write has zero delta, but with k == to it overwrites the
        // credit computed from the same pre-state.
        let c = keyed(&["from", "to", "k"]);
        let rows = vec![vec![q(-1), q(0)], vec![q(1), q(0)], vec![q(0), q(0)]];
        let refusal = check(
            &c,
            &[writer(
                "touch",
                rows.clone(),
                &c,
                &["input.from != input.to"],
            )],
            &[q(1); 3],
        )
        .unwrap_err();
        let Failure::Aliased { cells, .. } = refusal else {
            panic!("expected an aliasing refusal, got {refusal:?}");
        };
        assert!(
            cells
                .iter()
                .any(|cell| cell.survivor.as_deref() == Some("Bal[k]")),
            "{cells:?}"
        );
        // Guarded distinct from both, k cannot alias.
        let guard = "input.from != input.to and input.k != input.from and input.k != input.to";
        assert_eq!(
            check(
                &c,
                &[writer("touch", rows.clone(), &c, &[guard])],
                &[q(1); 3]
            ),
            Ok(())
        );
        // Read but not assigned, Bal[k] cannot overwrite anything.
        let mut read_only = writer("touch", rows.clone(), &c, &["input.from != input.to"]);
        read_only.assigns.retain(|id| id != "Bal[k]");
        assert_eq!(check(&c, &[read_only], &[q(1); 3]), Ok(()));
        // A changed coordinate missing from the footprint is malformed.
        let mut unlisted = writer("touch", rows, &c, &["input.from != input.to"]);
        unlisted.assigns.retain(|id| id != "Bal[to]");
        assert_eq!(check(&c, &[unlisted], &[q(1); 3]), Err(Failure::Malformed));
    }

    #[test]
    fn cons_aliasing_follows_canonical_storage() {
        // Wallet is a child of Account: Wallet.balance[to] and
        // Account.balance[from] are one row when from == to.
        let c = vec![
            stored(
                "Account[from]",
                "Account",
                "Account",
                CellKey::Input("from".into()),
            ),
            stored(
                "Wallet[to]",
                "Wallet",
                "Account",
                CellKey::Input("to".into()),
            ),
        ];
        assert!(matches!(
            check(&c, &[transfer(&c, &[])], &[q(1); 2]),
            Err(Failure::Aliased { .. })
        ));
        assert_eq!(
            check(&c, &[transfer(&c, &["input.from != input.to"])], &[q(1); 2]),
            Ok(())
        );
        // Another root, or another member, is another row.
        let mut other = c.clone();
        other[1].cell.as_mut().unwrap().object_type = "Escrow".into();
        other[1].cell.as_mut().unwrap().root = "Escrow".into();
        assert_eq!(check(&other, &[transfer(&other, &[])], &[q(1); 2]), Ok(()));
        other[1].cell.as_mut().unwrap().root = "Account".into();
        other[1].cell.as_mut().unwrap().member = "held".into();
        assert_eq!(check(&other, &[transfer(&other, &[])], &[q(1); 2]), Ok(()));
        // One object type cannot resolve to two roots.
        let mut split = c.clone();
        split[1].cell.as_mut().unwrap().object_type = "Account".into();
        split[1].cell.as_mut().unwrap().root = "Party".into();
        assert_eq!(
            check(&split, &[transfer(&split, &[])], &[q(1); 2]),
            Err(Failure::Malformed)
        );
        // Book labels do not separate storage: a possible collision across
        // books cannot be checked faithfully.
        let mut books = c.clone();
        books[1].scope.insert("book".into(), "other".into());
        assert!(matches!(
            check(&books, &[transfer(&books, &[])], &[q(1); 2]),
            Err(Failure::Unknown(why)) if why.starts_with(ALIAS_UNLOWERABLE_CODE)
        ));
        // Proved distinct, the books are separate rows again.
        assert!(matches!(
            check(
                &books,
                &[transfer(&books, &["input.from != input.to"])],
                &[q(1); 2]
            ),
            Err(Failure::Unbalanced { .. })
        ));
        // Stored and unstored coordinates together are unknown.
        let mut mixed = c;
        mixed[1].cell = None;
        assert!(matches!(
            check(&mixed, &[transfer(&mixed, &[])], &[q(1); 2]),
            Err(Failure::Unknown(why)) if why.starts_with(ALIAS_UNLOWERABLE_CODE)
        ));
    }

    #[test]
    fn cons_two_labels_on_one_cell_are_one_cell() {
        // Account.balance["cash"] and Wallet.balance["cash"] are one row.
        // Moving `amount` between the labels is no transfer at all: the
        // writes collide and the surviving one changes the total.
        let c = vec![
            stored("cash", "Account", "Account", CellKey::Fixed("cash".into())),
            stored(
                "wallet_cash",
                "Wallet",
                "Account",
                CellKey::Fixed("cash".into()),
            ),
        ];
        let refusal = check(&c, &[transfer(&c, &[])], &[q(1); 2]).unwrap_err();
        let Failure::Aliased { cells, .. } = refusal else {
            panic!("expected an aliasing refusal, got {refusal:?}");
        };
        assert_eq!(cells[0].coordinates, ["cash", "wallet_cash"]);
        // No guard separates them.
        assert!(matches!(
            check(&c, &[transfer(&c, &["input.from != input.to"])], &[q(1); 2]),
            Err(Failure::Aliased { .. })
        ));
        // Different constants are different rows without any guard.
        let mut apart = c.clone();
        apart[1].cell.as_mut().unwrap().key = CellKey::Fixed("bank".into());
        assert_eq!(check(&apart, &[transfer(&apart, &[])], &[q(1); 2]), Ok(()));
        // A computed key may reach a fixed row.
        let mut reach = c;
        reach[1].cell.as_mut().unwrap().key = CellKey::Computed("route(input.to)".into());
        assert!(matches!(
            check(&reach, &[transfer(&reach, &[])], &[q(1); 2]),
            Err(Failure::Aliased { .. })
        ));
        // An input key may equal a fixed row; inequality to a constant is
        // not lowered, so it stays possible.
        reach[1].cell.as_mut().unwrap().key = CellKey::Input("to".into());
        assert!(matches!(
            check(
                &reach,
                &[transfer(&reach, &["input.to != \"cash\""])],
                &[q(1); 2]
            ),
            Err(Failure::Aliased { .. })
        ));
    }

    #[test]
    fn cons_three_keys_have_exactly_five_partitions() {
        let never = |_: usize, _: usize| false;
        let all = key_partitions(&[0, 1, 2], &never, MAX_ALIAS_CASES).unwrap();
        let expected: Vec<Vec<Vec<usize>>> = vec![
            vec![vec![0, 1, 2]],
            vec![vec![0, 1], vec![2]],
            vec![vec![0, 2], vec![1]],
            vec![vec![0], vec![1, 2]],
            vec![vec![0], vec![1], vec![2]],
        ];
        assert_eq!(all, expected);
        // a != b and b != c: only {a,c} can still merge.
        let ab_bc = |x: usize, y: usize| matches!((x.min(y), x.max(y)), (0, 1) | (1, 2));
        assert_eq!(
            key_partitions(&[0, 1, 2], &ab_bc, MAX_ALIAS_CASES).unwrap(),
            vec![vec![vec![0, 2], vec![1]], vec![vec![0], vec![1], vec![2]]]
        );
    }

    #[test]
    fn cons_three_key_writer_checks_every_case() {
        // Bal[a] -= x; Bal[b] += y; Bal[c] += x - y: balanced when distinct.
        let c = keyed(&["a", "b", "c"]);
        let split = |guard: &[&str]| {
            writer(
                "split",
                vec![
                    vec![q(-1), q(0), q(0)],
                    vec![q(0), q(1), q(0)],
                    vec![q(1), q(-1), q(0)],
                ],
                &c,
                guard,
            )
        };
        let all = "input.a != input.b and input.b != input.c and input.a != input.c";
        assert_eq!(check(&c, &[split(&[all])], &[q(1); 3]), Ok(()));
        // a != b and b != c leave a == c open, and only that case.
        let refusal = check(
            &c,
            &[split(&["input.a != input.b and input.b != input.c"])],
            &[q(1); 3],
        )
        .unwrap_err();
        let Failure::Aliased { cells, .. } = refusal else {
            panic!("expected an aliasing refusal, got {refusal:?}");
        };
        assert_eq!(cells.len(), 1);
        assert_eq!(cells[0].coordinates, ["Bal[a]", "Bal[c]"]);
        let refusal = check(
            &c,
            &[split(&["input.a != input.b", "input.a != input.c"])],
            &[q(1); 3],
        )
        .unwrap_err();
        let Failure::Aliased { cells, .. } = refusal else {
            panic!("expected an aliasing refusal, got {refusal:?}");
        };
        assert_eq!(cells[0].coordinates, ["Bal[b]", "Bal[c]"]);
        // Unguarded the first case checked is {a,b,c} as one cell.
        let refusal = check(&c, &[split(&[])], &[q(1); 3]).unwrap_err();
        let Failure::Aliased { cells, .. } = refusal else {
            panic!("expected an aliasing refusal, got {refusal:?}");
        };
        assert_eq!(cells[0].coordinates, ["Bal[a]", "Bal[b]", "Bal[c]"]);
    }

    #[test]
    fn cons_aliasing_past_the_bound_is_unknown() {
        let bindings = ["k0", "k1", "k2", "k3", "k4", "k5", "k6"];
        let c = keyed(&bindings);
        let mut rows = vec![vec![q(1), q(0)]; bindings.len()];
        rows[0] = vec![q(-6), q(0)];
        let fan_out = writer("fan_out", rows, &c, &[]);
        let refusal = check(&c, std::slice::from_ref(&fan_out), &[q(1); 7]);
        assert!(
            matches!(&refusal, Err(Failure::Unknown(why)) if why.starts_with(ALIAS_BOUND_CODE)),
            "{refusal:?}"
        );
        // Six keys fit the per-member limit, but not the case bound.
        let c6 = keyed(&bindings[..6]);
        let mut rows = vec![vec![q(1), q(0)]; 6];
        rows[0] = vec![q(-5), q(0)];
        assert!(matches!(
            check(&c6, &[writer("fan_out", rows, &c6, &[])], &[q(1); 6]),
            Err(Failure::Unknown(why)) if why.starts_with(ALIAS_BOUND_CODE)
        ));
        // Unequal weights on cells that may be one row cannot be checked.
        let mut uneven = keyed(&["from", "to"]);
        uneven[1].weight = q(2);
        assert!(matches!(
            check(&uneven, &[transfer(&uneven, &[])], &[q(1), q(2)]),
            Err(Failure::Unknown(why)) if why.starts_with(ALIAS_UNLOWERABLE_CODE)
        ));
        // Unknown or repeated footprint ids are malformed.
        let mut ghost = transfer(&uneven, &[]);
        ghost.assigns.push("Bal[nowhere]".into());
        assert_eq!(
            check(&uneven, &[ghost], &[q(1), q(2)]),
            Err(Failure::Malformed)
        );
        let mut twice = transfer(&uneven, &[]);
        twice.assigns.push("Bal[to]".into());
        assert_eq!(
            check(&uneven, &[twice], &[q(1), q(2)]),
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
