//! The static cost estimate under the `norn-expr/4` structural grammar and
//! `norn-zen/6` language profile (T1-EXPR-4, #2522): a proved
//! upper bound on the units the metered evaluator charges for one evaluation
//! of an admitted expression on a **conforming slice**.
//!
//! # Metering model (zen fork `metering`, NORN.md "Metering cost model")
//!
//! Every executed opcode costs 1 unit. On top of that the VM charges, before
//! an opcode runs, the *shallow size* of the values it takes (string length in
//! bytes, list length, record key count, 1 for anything else): both operands
//! of `==`/`in` (and of `+` on strings), every argument of a builtin call, the
//! list a closure iterates (`Begin`), and for `flatMap` the length of the
//! collected result plus the length of each collected list (`Flatten`). The
//! estimate follows zen's compiler opcode by opcode for every admitted node.
//! Temporal parsing also pays the text argument length; temporal arithmetic
//! pays the shallow argument sizes plus the fork's `TEMPORAL_ARITHMETIC`
//! constant. Closure bodies are multiplied by the iterated list's length
//! bound: so
//! nested iteration multiplies (zen adoption review F3).
//!
//! Bounded string builtins follow the fork's precharge model: `tokens` adds
//! the regex compile constant and scans the input bytes, `take` adds at most
//! four bytes per requested scalar, and `join` adds twice the one-level list
//! size plus the separator bytes. Their result bounds carry token count and
//! per-token bytes, the scalar-prefix byte cap, and the 16 KiB join cap into
//! subsequent calls and the final result traversal.
//!
//! # Conforming slice
//!
//! The estimate is an upper bound for every evaluation whose context (and, for
//! a unary test, whose subject) conforms to the type environment and to the
//! limit profile's slice bounds: its Norn1 canonical JSON is at most
//! `slice.max_canonical_bytes` (B) bytes, and a list of records holds at most
//! `slice.max_records` (R) records. The kernel enforces both while reading the
//! slice. From them:
//!
//! - a list read from the slice has at most R elements when its elements are
//!   records, and at most `(B - 1) / (m + 1)` elements when its elements take
//!   at least `m` canonical bytes each;
//! - a string or list read from the slice is at most as long as its own
//!   canonical bytes, and the canonical bytes of **disjoint** positions of the
//!   slice sum to at most B.
//!
//! The second fact is what keeps `some(self.lines, #.sku == 'x')` affordable:
//! the `sku` strings of different lines are disjoint positions, so the whole
//! loop reads at most B bytes of them, not R x B. The estimate therefore
//! tracks slice reads symbolically, as *atoms*: the canonical path of a
//! position (`self.lines[*].sku`), where each `[*]` is either bound to the
//! closure that iterates it or already summed over every element. Leaving a
//! closure sums its own `[*]` (distinct iterations read distinct positions)
//! and multiplies everything else by the iteration bound. At the end every
//! atom is a set of positions; positions on one root-to-leaf chain of the
//! path tree can overlap, positions on different chains cannot, so the atoms
//! contribute at most `B x` (the heaviest chain's total weight).
//!
//! Anything the estimate cannot track (a value that is not a direct slice
//! read) is bounded by its largest single value instead, which is always
//! sound.
//!
//! `check_expression_with_root_bounds` additionally supports independently
//! bounded context roots. Each configured root is a separate byte pool with
//! its own record-list cap; its paths use that root's caps throughout value,
//! closure, and result-copy analysis. Default context and unary subject pools
//! retain the limit profile's caps. Losing provenance falls back to the
//! largest possible single-root value, and aliases configured as separate
//! roots deliberately overcount overlapping bytes.

use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

use norn_ir::ResourceLimitProfile;
use zen_expression::functions::{ClosureFunction, FunctionKind, InternalFunction};
use zen_expression::lexer::{ArithmeticOperator, ComparisonOperator, LogicalOperator, Operator};
use zen_expression::parser::Node;

use super::{Rec, RootCostBounds, SUBJECT_VARIABLE, T, address};
use crate::env::ZenTypeEnv;

// ---------------------------------------------------------------------------
// Symbolic sizes
// ---------------------------------------------------------------------------

/// Which elements a `[*]` path step denotes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Each {
    /// Every element (the step has been summed over).
    All,
    /// The element bound by the closure at this nesting level (1-based).
    Level(u32),
    /// The element of a list value not yet iterated (a template, bound to a
    /// level when a closure iterates the list).
    Marker(u32),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Seg {
    Field(Rc<str>),
    Each(Each),
}

/// The canonical path of a slice position, rooted at a variable (or `$`).
type Path = Vec<Seg>;

fn len64(n: usize) -> u64 {
    u64::try_from(n).unwrap_or(u64::MAX)
}

fn literal_u64(node: Option<&Node<'_>>, max: u64) -> u64 {
    match node {
        Some(Node::Number(value)) => value.to_string().parse::<u64>().unwrap_or(max).min(max),
        _ => max,
    }
}

fn child(path: &Path, seg: Seg) -> Path {
    let mut next = path.clone();
    next.push(seg);
    next
}

/// Whether the position changes between iterations of some closure (or of a
/// list not yet iterated).
fn varies(path: &Path) -> bool {
    path.iter()
        .any(|seg| matches!(seg, Seg::Each(Each::Level(_) | Each::Marker(_))))
}

/// One atom of a [`Lin`]: `coef` times the size of the position(s), each
/// single position at most `cap`.
#[derive(Debug, Clone, Copy)]
struct Term {
    coef: u64,
    cap: u64,
}

/// `c + sum(coef x atom)`: a bound in units (or in bytes/elements, for a
/// value's size), linear in the sizes of slice positions.
#[derive(Debug, Clone, Default)]
struct Lin {
    c: u64,
    atoms: BTreeMap<Path, Term>,
}

impl Lin {
    fn constant(c: u64) -> Self {
        Self {
            c,
            atoms: BTreeMap::new(),
        }
    }

    fn atom(path: Path, cap: u64) -> Self {
        let mut lin = Self::default();
        lin.add_term(path, Term { coef: 1, cap });
        lin
    }

    fn add_term(&mut self, path: Path, term: Term) {
        let entry = self.atoms.entry(path).or_insert(Term {
            coef: 0,
            cap: term.cap,
        });
        entry.coef = entry.coef.saturating_add(term.coef);
        entry.cap = entry.cap.max(term.cap);
    }

    fn add(&mut self, other: &Self) {
        self.c = self.c.saturating_add(other.c);
        for (path, term) in &other.atoms {
            self.add_term(path.clone(), *term);
        }
    }

    fn plus(mut self, c: u64) -> Self {
        self.c = self.c.saturating_add(c);
        self
    }

    fn scaled(&self, k: u64) -> Self {
        Self {
            c: self.c.saturating_mul(k),
            atoms: self
                .atoms
                .iter()
                .map(|(path, term)| {
                    (
                        path.clone(),
                        Term {
                            coef: term.coef.saturating_mul(k),
                            cap: term.cap,
                        },
                    )
                })
                .collect(),
        }
    }

    /// Componentwise maximum: a bound for either of two values.
    fn max(&self, other: &Self) -> Self {
        let mut out = Self::constant(self.c.max(other.c));
        for (path, term) in self.atoms.iter().chain(&other.atoms) {
            let entry = out.atoms.entry(path.clone()).or_insert(*term);
            entry.coef = entry.coef.max(term.coef);
            entry.cap = entry.cap.max(term.cap);
        }
        out
    }

    /// Rewrites every atom's path; `None` forgets the atom, bounding it by
    /// its single-position cap instead.
    fn map_paths(&self, f: &impl Fn(&Path) -> Option<Path>) -> Self {
        let mut out = Self::constant(self.c);
        for (path, term) in &self.atoms {
            match f(path) {
                Some(path) => out.add_term(path, *term),
                None => out.c = out.c.saturating_add(term.coef.saturating_mul(term.cap)),
            }
        }
        out
    }

    /// At least 1 (the size of `null`). An atom is at least 1: every JSON
    /// value takes at least one canonical byte.
    fn at_least_one(mut self) -> Self {
        if self.atoms.values().all(|term| term.coef == 0) {
            self.c = self.c.max(1);
        }
        self
    }
}

/// The sum over the iterations of the closure at `level`, at most `n` of
/// them, of a per-iteration bound: the closure's own element positions are
/// distinct across iterations, so their atoms are summed (`[*]` becomes
/// every element); everything else is multiplied by `n`.
fn loop_sum(lin: &Lin, level: u32, n: u64) -> Lin {
    let mut out = Lin::constant(lin.c.saturating_mul(n));
    for (path, term) in &lin.atoms {
        if path.contains(&Seg::Each(Each::Level(level))) {
            let summed = path
                .iter()
                .map(|seg| match seg {
                    Seg::Each(Each::Level(l)) if *l == level => Seg::Each(Each::All),
                    other @ (Seg::Field(_) | Seg::Each(_)) => other.clone(),
                })
                .collect();
            out.add_term(summed, *term);
        } else {
            out.add_term(
                path.clone(),
                Term {
                    coef: term.coef.saturating_mul(n),
                    cap: term.cap,
                },
            );
        }
    }
    out
}

/// The abstract value of a node: bounds on what one evaluation produces.
#[derive(Debug, Clone, Default)]
struct Val {
    /// The shallow size (what an opcode taking the value is charged).
    size: Lin,
    /// The recursive size after projecting a checked temporal Dynamic to its
    /// canonical JSON string. Other values use `size` for their copy bound.
    copy_size: Option<Lin>,
    /// Lists: the maximum number of elements.
    count: u64,
    /// Lists: an aggregate upper bound on the shallow bytes of all elements.
    /// This is tighter than multiplying the per-element maximum for producers
    /// such as `tokens`, whose output bytes cannot exceed its input bytes.
    items_copy_size: Option<Lin>,
    /// The slice position the value is, if it is a direct slice read.
    origin: Option<Path>,
    /// Lists: a bound for every element.
    elem: Option<Box<Self>>,
    /// Lists: the markers in `elem` that denote "this element" (a template:
    /// distinct elements are distinct slice positions).
    markers: Vec<u32>,
    /// Record literals: the fields.
    fields: Option<BTreeMap<String, Self>>,
    /// A record read from the slice (its nested size is bounded by the
    /// slice, not by `size`).
    record: bool,
}

impl Val {
    fn scalar() -> Self {
        Self::sized(Lin::constant(1))
    }

    fn sized(size: Lin) -> Self {
        Self {
            size,
            ..Self::default()
        }
    }

    /// Rewrites every path in the value (see [`Lin::map_paths`]).
    fn map_paths(&self, f: &impl Fn(&Path) -> Option<Path>) -> Self {
        Self {
            size: self.size.map_paths(f),
            copy_size: self.copy_size.as_ref().map(|size| size.map_paths(f)),
            count: self.count,
            items_copy_size: self.items_copy_size.as_ref().map(|size| size.map_paths(f)),
            origin: self.origin.as_ref().and_then(f),
            elem: self.elem.as_ref().map(|elem| Box::new(elem.map_paths(f))),
            markers: self.markers.clone(),
            fields: self.fields.as_ref().map(|fields| {
                fields
                    .iter()
                    .map(|(name, field)| (name.clone(), field.map_paths(f)))
                    .collect()
            }),
            record: self.record,
        }
    }

    /// Binds this list element template's `markers` to the closure `level`.
    fn bind(&self, markers: &[u32], level: u32) -> Self {
        if markers.is_empty() {
            return self.clone();
        }
        self.map_paths(&|path: &Path| {
            Some(
                path.iter()
                    .map(|seg| match seg {
                        Seg::Each(Each::Marker(m)) if markers.contains(m) => {
                            Seg::Each(Each::Level(level))
                        }
                        other @ (Seg::Field(_) | Seg::Each(_)) => other.clone(),
                    })
                    .collect(),
            )
        })
    }
}

/// A bound for either of two values.
fn join(a: &Val, b: &Val) -> Val {
    let elem = match (&a.elem, &b.elem) {
        (Some(x), Some(y)) => Some(Box::new(join(x, y))),
        (Some(x), None) | (None, Some(x)) => Some(x.clone()),
        (None, None) => None,
    };
    let mut markers = a.markers.clone();
    markers.extend(b.markers.iter().filter(|m| !a.markers.contains(m)));
    let fields = match (&a.fields, &b.fields) {
        (Some(x), Some(y)) if x.len() == y.len() && x.keys().all(|k| y.contains_key(k)) => Some(
            x.iter()
                .filter_map(|(name, left)| Some((name.clone(), join(left, y.get(name)?))))
                .collect(),
        ),
        _ => None,
    };
    Val {
        size: a.size.max(&b.size),
        copy_size: match (&a.copy_size, &b.copy_size) {
            (Some(left), Some(right)) => Some(left.max(right)),
            (Some(left), None) => Some(left.max(&b.size)),
            (None, Some(right)) => Some(right.max(&a.size)),
            (None, None) => None,
        },
        count: a.count.max(b.count),
        items_copy_size: match (&a.items_copy_size, &b.items_copy_size) {
            (Some(left), Some(right)) => Some(left.max(right)),
            _ => None,
        },
        origin: if a.origin == b.origin {
            a.origin.clone()
        } else {
            None
        },
        elem,
        markers,
        fields,
        record: a.record || b.record,
    }
}

/// Executed-opcode overhead of a closure builtin, from zen's compiler:
/// `(per iteration, after the loop)`, excluding the list, `Begin`, and the
/// body. Per iteration: `Jump(IfEnd)`, `IncrementIt`, `Jump(Backward)` plus
/// the builtin's own test/collect opcodes; after: the final `Jump(IfEnd)`
/// plus the builtin's epilogue (`End`, result opcodes).
const fn overhead(function: ClosureFunction) -> (u64, u64) {
    match function {
        // + Jump(IfFalse|IfTrue), Pop | PushBool, End
        ClosureFunction::All | ClosureFunction::Some => (5, 3),
        // + Not, Jump(IfFalse), Pop | PushBool, End
        ClosureFunction::None => (6, 3),
        // + Jump(IfFalse), Pop, IncrementCount, Jump(Forward)
        //   | GetCount, End ...
        ClosureFunction::Count => (7, 3),
        //   | GetCount, PushNumber, Equal (+2 data), End
        ClosureFunction::One => (7, 7),
        // + Jump(IfFalse), Pop, IncrementCount, Pointer, Jump(Forward)
        //   | GetCount, End, Array
        ClosureFunction::Filter => (8, 4),
        //   | GetLen, End, Array
        ClosureFunction::Map => (3, 4),
        //   | GetLen, End, Array, Flatten (+ data, charged separately)
        ClosureFunction::FlatMap => (3, 5),
    }
}

// ---------------------------------------------------------------------------
// The estimator
// ---------------------------------------------------------------------------

struct Out {
    cost: Lin,
    val: Val,
}

struct Scope<'n> {
    alias: Option<&'n str>,
    element: Val,
}

pub(super) struct Estimator<'a, 'n> {
    env: &'a ZenTypeEnv,
    types: &'a HashMap<usize, T>,
    /// B: canonical bytes of the slice.
    bytes: u64,
    default_bytes: u64,
    /// R: records of the slice.
    records: u64,
    default_records: u64,
    root_bounds: Option<&'a BTreeMap<String, RootCostBounds>>,
    scopes: Vec<Scope<'n>>,
    next_marker: u32,
}

impl<'a, 'n> Estimator<'a, 'n> {
    pub(super) fn new(
        env: &'a ZenTypeEnv,
        types: &'a HashMap<usize, T>,
        limits: &ResourceLimitProfile,
    ) -> Self {
        Self {
            env,
            types,
            bytes: limits.slice.max_canonical_bytes,
            default_bytes: limits.slice.max_canonical_bytes,
            records: u64::from(limits.slice.max_records),
            default_records: u64::from(limits.slice.max_records),
            root_bounds: None,
            scopes: Vec::new(),
            next_marker: 0,
        }
    }

    pub(super) fn with_root_bounds(
        mut self,
        root_bounds: Option<&'a BTreeMap<String, RootCostBounds>>,
    ) -> Self {
        self.root_bounds = root_bounds;
        if let Some(bounds) = root_bounds {
            self.bytes = bounds.values().fold(self.bytes, |bytes, bound| {
                bytes.max(bound.max_canonical_bytes)
            });
            self.records = bounds.values().fold(self.records, |records, bound| {
                records.max(bound.max_records)
            });
        }
        self
    }

    fn root_bound(&self, path: &Path) -> Option<RootCostBounds> {
        let Seg::Field(root) = path.first()? else {
            return None;
        };
        self.root_bounds?.get(root.as_ref()).copied()
    }

    fn path_bytes(&self, path: &Path) -> u64 {
        self.root_bound(path)
            .map_or(self.default_bytes, |bound| bound.max_canonical_bytes)
    }

    /// The upper bound, in metering units, of evaluating `root` once.
    pub(super) fn estimate(mut self, root: &'n Node<'n>) -> u64 {
        let out = self.eval(root);
        let mut cost = out.cost;
        // The evaluator charges the recursive size of the result before it
        // copies it out.
        cost.add(&self.deep(&out.val, 1));
        self.total(&cost)
    }

    /// A bound on the evaluator's charge for copying the value out: 1 per
    /// list, record, or scalar plus its members, and a string's length when
    /// longer than 1. Evaluated with no closure open, so `level` is free.
    ///
    /// A slice position's copy charge is at most its canonical bytes (every
    /// value takes at least one byte, a string its length plus quotes), so a
    /// direct slice read is its own atom. A list's elements are summed like
    /// closure iterations: template elements are distinct positions.
    fn deep(&self, val: &Val, level: u32) -> Lin {
        if let Some(path) = &val.origin {
            return Lin::atom(path.clone(), self.path_bytes(path));
        }
        if let Some(elem) = &val.elem {
            if let Some(items_copy_size) = &val.items_copy_size {
                // `tokens` only returns non-empty strings. Keep the element
                // overhead per token while applying the aggregate byte cap.
                return items_copy_size.clone().plus(val.count).plus(1);
            }
            let elem = elem.bind(&val.markers, level);
            let each = self.deep(&elem, level.saturating_add(1));
            return loop_sum(&each, level, val.count).plus(1);
        }
        if let Some(fields) = &val.fields {
            let mut total = Lin::constant(1);
            for field in fields.values() {
                total.add(&self.deep(field, level));
            }
            return total;
        }
        if val.record {
            // A slice record whose position is not tracked.
            return Lin::constant(self.bytes);
        }
        // A scalar or string: at least 1 (an atom is at least one byte).
        val.copy_size
            .as_ref()
            .unwrap_or(&val.size)
            .clone()
            .at_least_one()
    }

    /// Resolves the atoms: `B x` the heaviest root-to-leaf chain of the path
    /// tree, per pool (the default context, a unary test's subject, and each
    /// independently bounded root). Overlapping alias roots are counted
    /// independently, which is conservative even when their values coincide.
    fn total(&self, cost: &Lin) -> u64 {
        let mut constant = cost.c;
        let mut context: BTreeMap<&Path, u64> = BTreeMap::new();
        let mut subject: BTreeMap<&Path, u64> = BTreeMap::new();
        let mut roots: BTreeMap<&str, BTreeMap<&Path, u64>> = BTreeMap::new();
        for (path, term) in &cost.atoms {
            if varies(path) {
                // Not reachable: every closure sums its own level. Bound it by
                // its single-position cap all the same.
                constant = constant.saturating_add(term.coef.saturating_mul(term.cap));
                continue;
            }
            let pool = match path.first() {
                Some(Seg::Field(root)) if &**root == SUBJECT_VARIABLE => &mut subject,
                Some(Seg::Field(root)) if self.root_bound(path).is_some() => {
                    roots.entry(root.as_ref()).or_default()
                }
                _ => &mut context,
            };
            pool.insert(path, term.coef);
        }
        let default = [context, subject].iter().fold(constant, |total, pool| {
            total.saturating_add(heaviest_chain(pool).saturating_mul(self.default_bytes))
        });
        roots.iter().fold(default, |total, (root, pool)| {
            let bytes = self
                .root_bounds
                .and_then(|bounds| bounds.get(*root))
                .map_or(self.default_bytes, |bound| bound.max_canonical_bytes);
            total.saturating_add(heaviest_chain(pool).saturating_mul(bytes))
        })
    }

    fn level(&self) -> u32 {
        u32::try_from(self.scopes.len()).unwrap_or(u32::MAX)
    }

    fn fresh_marker(&mut self) -> u32 {
        self.next_marker = self.next_marker.saturating_add(1);
        self.next_marker
    }

    /// A value with no type information: bounded by the whole slice.
    fn top(&self) -> Val {
        Val {
            size: Lin::constant(self.bytes),
            count: self.bytes.saturating_sub(1).checked_div(2).unwrap_or(0),
            ..Val::default()
        }
    }

    fn type_of(&self, node: &Node<'_>) -> Option<&'a T> {
        self.types.get(&address(node))
    }

    /// The fewest canonical bytes a value of `ty` takes.
    fn min_bytes(ty: &T) -> u64 {
        match ty {
            T::Number => 1,
            T::Null | T::Bool => 4,
            T::Str(Some(values)) => {
                2_u64.saturating_add(values.iter().map(|v| len64(v.len())).min().unwrap_or(0))
            }
            T::Str(None)
            | T::Date
            | T::Timestamp
            | T::ParsedDate
            | T::ParsedTimestamp
            | T::List(_)
            | T::Record(_) => 2,
            T::Optional(inner) => Self::min_bytes(inner).min(4),
        }
    }

    /// The maximum length of a slice list with elements of type `item`.
    fn list_cap(item: &T, bytes: u64, records: u64) -> u64 {
        let by_bytes = bytes
            .saturating_sub(1)
            .checked_div(Self::min_bytes(item).saturating_add(1))
            .unwrap_or(0);
        if matches!(item, T::Record(_)) {
            by_bytes.min(records)
        } else {
            by_bytes
        }
    }

    /// The value of a slice read of type `ty` at `origin`.
    fn env_val(&mut self, ty: Option<&T>, origin: Option<Path>) -> Val {
        let Some(ty) = ty else {
            return self.top();
        };
        let bytes = origin
            .as_ref()
            .map_or(self.bytes, |path| self.path_bytes(path));
        let records = origin.as_ref().map_or(self.records, |path| {
            self.root_bound(path)
                .map_or(self.default_records, |bound| bound.max_records)
        });
        match ty {
            T::Optional(inner) => {
                let mut val = self.env_val(Some(inner), origin);
                val.size = val.size.at_least_one();
                val
            }
            T::Null | T::Bool | T::Number => Val::scalar(),
            T::Str(Some(values)) => Val {
                size: Lin::constant(values.iter().map(|v| len64(v.len())).max().unwrap_or(0)),
                origin,
                ..Val::default()
            },
            T::Str(None) => Val {
                size: origin.as_ref().map_or_else(
                    || Lin::constant(bytes),
                    |path| Lin::atom(path.clone(), bytes),
                ),
                origin,
                ..Val::default()
            },
            T::Date | T::Timestamp | T::ParsedDate | T::ParsedTimestamp => Val {
                size: origin.as_ref().map_or_else(
                    || Lin::constant(bytes),
                    |path| Lin::atom(path.clone(), bytes),
                ),
                origin,
                ..Val::default()
            },
            T::Record(Rec::Env { name, .. }) => Val {
                size: Lin::constant(
                    self.env
                        .record(name)
                        .map_or(bytes, |record| len64(record.fields.len())),
                ),
                origin,
                record: true,
                ..Val::default()
            },
            T::Record(Rec::Literal(fields)) => Val {
                size: Lin::constant(len64(fields.len())),
                origin,
                ..Val::default()
            },
            T::List(item) => {
                let count = Self::list_cap(item, bytes, records);
                // Inside a closure the list's length is summed over disjoint
                // positions; otherwise the element bound is tighter.
                let size = match &origin {
                    Some(path) if varies(path) => Lin::atom(path.clone(), count),
                    _ => Lin::constant(count),
                };
                let marker = self.fresh_marker();
                let elem_origin = origin
                    .as_ref()
                    .map(|path| child(path, Seg::Each(Each::Marker(marker))));
                let elem = self.env_val(Some(item), elem_origin);
                Val {
                    size,
                    count,
                    origin,
                    elem: Some(Box::new(elem)),
                    markers: vec![marker],
                    fields: None,
                    record: false,
                    copy_size: None,
                    items_copy_size: None,
                }
            }
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one arm per admitted node, mirroring zen's compiler"
    )]
    fn eval(&mut self, node: &'n Node<'n>) -> Out {
        let one = |val: Val| Out {
            cost: Lin::constant(1),
            val,
        };
        match node {
            Node::Null | Node::Bool(_) | Node::Number(_) => one(Val::scalar()),
            Node::String(text) => one(Val::sized(Lin::constant(len64(text.len())))),
            Node::Parenthesized(inner) => self.eval(inner),
            Node::Array(items) => self.list_literal(items),
            Node::Object(entries) => self.record_literal(entries),
            Node::Identifier(name) => {
                let alias = self
                    .scopes
                    .iter()
                    .rev()
                    .find(|scope| scope.alias == Some(*name))
                    .map(|scope| scope.element.clone());
                let val = alias.unwrap_or_else(|| {
                    let ty = self.type_of(node);
                    self.env_val(ty, Some(vec![Seg::Field(Rc::from(*name))]))
                });
                one(val)
            }
            Node::Pointer => {
                let val = self
                    .scopes
                    .last()
                    .map_or_else(|| self.top(), |scope| scope.element.clone());
                one(val)
            }
            Node::Member {
                node: target,
                property,
            } => self.member(node, target, property),
            Node::Unary {
                node: operand,
                operator,
            } => {
                let inner = self.eval(operand);
                match operator {
                    Operator::Arithmetic(ArithmeticOperator::Add) => inner,
                    Operator::Arithmetic(_)
                    | Operator::Logical(_)
                    | Operator::Comparison(_)
                    | Operator::Range
                    | Operator::Comma
                    | Operator::Slice
                    | Operator::Dot
                    | Operator::QuestionMark
                    | Operator::Assign
                    | Operator::Semi => Out {
                        cost: inner.cost.plus(1),
                        val: Val::scalar(),
                    },
                }
            }
            Node::Binary {
                left,
                operator,
                right,
            } => self.binary(node, left, *operator, right),
            Node::Conditional {
                condition,
                on_true,
                on_false,
            } => {
                let condition = self.eval(condition);
                let yes = self.eval(on_true);
                let no = self.eval(on_false);
                let mut cost = condition.cost.plus(3);
                cost.add(&yes.cost.max(&no.cost));
                Out {
                    cost,
                    val: join(&yes.val, &no.val),
                }
            }
            Node::FunctionCall {
                kind: FunctionKind::Closure(function),
                arguments,
            } => self.closure(*function, arguments),
            Node::FunctionCall { kind, arguments } => self.builtin(kind, arguments),
            Node::Interval { left, right, .. } => {
                let low = self.eval(left);
                let high = self.eval(right);
                let mut cost = low.cost.plus(1);
                cost.add(&high.cost);
                Out {
                    cost,
                    val: Val::scalar(),
                }
            }
            // Outside the admitted grammar: the checker rejected it already.
            Node::TemplateString(_)
            | Node::Slice { .. }
            | Node::MethodCall { .. }
            | Node::Assignments { .. }
            | Node::Root
            | Node::Closure { .. }
            | Node::Error { .. } => Out {
                cost: Lin::constant(u64::MAX),
                val: self.top(),
            },
        }
    }

    /// `[a, b, ...]`: the items, `PushNumber(len)`, `Array`.
    fn list_literal(&mut self, items: &'n [&'n Node<'n>]) -> Out {
        let mut cost = Lin::constant(2);
        let mut elem: Option<Val> = None;
        for item in items {
            let out = self.eval(item);
            cost.add(&out.cost);
            elem = Some(match elem {
                None => out.val,
                Some(current) => join(&current, &out.val),
            });
        }
        let count = len64(items.len());
        Out {
            cost,
            val: Val {
                size: Lin::constant(count),
                count,
                elem: elem.map(Box::new),
                ..Val::default()
            },
        }
    }

    /// `{k: v, ...}`: per entry the key, `string(key)` (charged the key's
    /// length) and the value; then `PushNumber(len)`, `Object`.
    fn record_literal(&mut self, entries: &'n [(&'n Node<'n>, &'n Node<'n>)]) -> Out {
        let mut cost = Lin::constant(2);
        let mut fields = BTreeMap::new();
        for (key, value) in entries {
            let key_out = self.eval(key);
            cost.add(&key_out.cost);
            cost.add(&key_out.val.size.plus(1));
            let out = self.eval(value);
            cost.add(&out.cost);
            if let Node::String(name) = key {
                fields.insert((*name).to_owned(), out.val);
            }
        }
        Out {
            cost,
            val: Val {
                size: Lin::constant(len64(entries.len())),
                fields: Some(fields),
                ..Val::default()
            },
        }
    }

    /// A member read: one `FetchFast` for a path from a variable, otherwise
    /// the target, `PushString`/`PushNumber`, and `Fetch` (at most 2 more).
    fn member(&mut self, node: &'n Node<'n>, target: &'n Node<'n>, property: &Node<'_>) -> Out {
        let base = self.eval(target);
        let cost = base.cost.plus(2);
        let val = if matches!(self.type_of(target), Some(T::List(_))) {
            // An in-bounds literal index into a list literal.
            base.val.elem.map_or_else(|| self.top(), |elem| *elem)
        } else if let (Some(fields), Node::String(key)) = (&base.val.fields, property) {
            fields.get(*key).cloned().unwrap_or_else(|| self.top())
        } else {
            let origin = match property {
                Node::String(key) => base
                    .val
                    .origin
                    .as_ref()
                    .map(|path| child(path, Seg::Field(Rc::from(*key)))),
                Node::Null
                | Node::Bool(_)
                | Node::Number(_)
                | Node::TemplateString(_)
                | Node::Pointer
                | Node::Array(_)
                | Node::Object(_)
                | Node::Assignments { .. }
                | Node::Identifier(_)
                | Node::Closure { .. }
                | Node::Parenthesized(_)
                | Node::Root
                | Node::Member { .. }
                | Node::Slice { .. }
                | Node::Interval { .. }
                | Node::Conditional { .. }
                | Node::Unary { .. }
                | Node::Binary { .. }
                | Node::FunctionCall { .. }
                | Node::MethodCall { .. }
                | Node::Error { .. } => None,
            };
            let ty = self.type_of(node);
            self.env_val(ty, origin)
        };
        Out { cost, val }
    }

    fn binary(
        &mut self,
        node: &Node<'_>,
        left: &'n Node<'n>,
        operator: Operator,
        right: &'n Node<'n>,
    ) -> Out {
        let l = self.eval(left);
        let r = self.eval(right);
        let mut cost = l.cost.clone();
        cost.add(&r.cost);
        // Both operands' shallow sizes, for opcodes that charge them.
        let mut sizes = l.val.size.clone();
        sizes.add(&r.val.size);
        match operator {
            Operator::Arithmetic(ArithmeticOperator::Add) => {
                if matches!(self.type_of(node), Some(T::Str(_))) {
                    cost.add(&sizes);
                    Out {
                        cost: cost.plus(1),
                        val: Val::sized(sizes),
                    }
                } else {
                    Out {
                        cost: cost.plus(1),
                        val: Val::scalar(),
                    }
                }
            }
            Operator::Comparison(ComparisonOperator::Equal | ComparisonOperator::In) => {
                cost.add(&sizes);
                Out {
                    cost: cost.plus(1),
                    val: Val::scalar(),
                }
            }
            // `Equal`/`In` then `Not`.
            Operator::Comparison(ComparisonOperator::NotEqual | ComparisonOperator::NotIn) => {
                cost.add(&sizes);
                Out {
                    cost: cost.plus(2),
                    val: Val::scalar(),
                }
            }
            // The left, a conditional jump, `Pop`, the right.
            Operator::Logical(LogicalOperator::NullishCoalescing) => Out {
                cost: cost.plus(2),
                val: join(&l.val, &r.val),
            },
            Operator::Logical(_) => Out {
                cost: cost.plus(2),
                val: Val::scalar(),
            },
            Operator::Arithmetic(_)
            | Operator::Comparison(_)
            | Operator::Range
            | Operator::Comma
            | Operator::Slice
            | Operator::Dot
            | Operator::QuestionMark
            | Operator::Assign
            | Operator::Semi => Out {
                cost: cost.plus(1),
                val: Val::scalar(),
            },
        }
    }

    /// A builtin: its arguments and one `CallFunction`, charged the shallow
    /// size of every argument.
    #[allow(
        clippy::too_many_lines,
        reason = "keeping each builtin cost beside its result bound makes the profile auditable"
    )]
    fn builtin(&mut self, kind: &FunctionKind, arguments: &'n [&'n Node<'n>]) -> Out {
        let mut cost = Lin::constant(1);
        let mut first: Option<Lin> = None;
        let mut values = Vec::with_capacity(arguments.len());
        for argument in arguments {
            let out = self.eval(argument);
            cost.add(&out.cost);
            cost.add(&out.val.size);
            if first.is_none() {
                first = Some(out.val.size.clone());
            }
            values.push(out.val);
        }
        let first = first.unwrap_or_default();
        let val = match kind {
            FunctionKind::Internal(InternalFunction::CalendarDate | InternalFunction::AddDays) => {
                Val {
                    size: Lin::constant(1),
                    copy_size: Some(Lin::constant(10)),
                    ..Val::default()
                }
            }
            FunctionKind::Internal(InternalFunction::Timestamp | InternalFunction::AddSeconds) => {
                Val {
                    size: Lin::constant(1),
                    copy_size: Some(Lin::constant(30)),
                    ..Val::default()
                }
            }
            // Unicode case mapping grows UTF-8 text by at most 3x (upper:
            // `ΐ`, 2 bytes, is 3 characters, 6 bytes) and 1.5x (lower: `İ`).
            FunctionKind::Internal(InternalFunction::Upper) => Val::sized(first.scaled(3)),
            FunctionKind::Internal(InternalFunction::Lower) => Val::sized(first.scaled(2)),
            FunctionKind::Internal(InternalFunction::Trim) => Val::sized(first),
            // Only the empty separator is admitted: at most one element
            // per UTF-8 byte plus two empty ends, each at most four bytes.
            FunctionKind::Internal(InternalFunction::Split) => Val {
                size: first.clone().plus(2),
                count: self.total(&first).saturating_add(2),
                elem: Some(Box::new(Val::sized(Lin::constant(4)))),
                ..Val::default()
            },
            FunctionKind::Internal(InternalFunction::Tokens) => {
                let max_count = literal_u64(arguments.get(2).copied(), 256);
                let max_length = literal_u64(arguments.get(3).copied(), 256);
                let input_bytes = self.total(&first);
                let count = if max_length == 0 {
                    0
                } else {
                    max_count.min(input_bytes)
                };
                let item_bytes = max_length.min(input_bytes);
                let items_copy_size = input_bytes.min(max_count.saturating_mul(max_length));
                cost.add(&first);
                cost = cost.plus(zen_expression::meter::cost::REGEX_COMPILE);
                Val {
                    size: Lin::constant(count),
                    count,
                    items_copy_size: Some(Lin::constant(items_copy_size)),
                    elem: Some(Box::new(Val::sized(Lin::constant(item_bytes)))),
                    ..Val::default()
                }
            }
            FunctionKind::Internal(InternalFunction::Take) => {
                let max_scalars = literal_u64(arguments.get(1).copied(), 16_384);
                let output_bytes = self.total(&first).min(max_scalars.saturating_mul(4));
                cost.add(&Lin::constant(output_bytes));
                Val::sized(Lin::constant(output_bytes))
            }
            FunctionKind::Internal(InternalFunction::Join) => {
                let list = values.first().cloned().unwrap_or_default();
                let separator = values
                    .get(1)
                    .map_or_else(Lin::default, |value| value.size.clone());
                let nested = self.nested_shallow(&list);
                let separators = list.count.saturating_sub(1);
                cost.add(&nested.scaled(2));
                cost.add(&separator.scaled(separators));
                let item_bytes = list.elem.as_deref().map_or_else(
                    || self.bytes.saturating_mul(list.count),
                    |element| {
                        list.items_copy_size.as_ref().map_or_else(
                            || {
                                let level = self.level().saturating_add(1);
                                let element = element.bind(&list.markers, level);
                                self.total(&loop_sum(&element.size, level, list.count))
                            },
                            |size| self.total(size),
                        )
                    },
                );
                let separator_bytes = self.total(&separator).saturating_mul(separators);
                Val::sized(Lin::constant(
                    item_bytes.saturating_add(separator_bytes).min(16_384),
                ))
            }
            FunctionKind::Internal(_) | FunctionKind::Deprecated(_) | FunctionKind::Closure(_) => {
                Val::scalar()
            }
        };
        if matches!(
            kind,
            FunctionKind::Internal(
                InternalFunction::DaysBetween
                    | InternalFunction::AddDays
                    | InternalFunction::SecondsBetween
                    | InternalFunction::AddSeconds
            )
        ) {
            cost = cost.plus(zen_expression::meter::cost::TEMPORAL_ARITHMETIC);
        }
        Out { cost, val }
    }

    /// The one-level scan charge used by bounded `join`: list length, plus
    /// each direct child's shallow size. The flat-string checker rule makes
    /// this exactly the fork's `nested_size_capped` shape.
    fn nested_shallow(&mut self, list: &Val) -> Lin {
        let mut nested = list.size.clone();
        if let Some(items_copy_size) = &list.items_copy_size {
            nested.add(items_copy_size);
        } else if let Some(element) = &list.elem {
            let level = self.level().saturating_add(1);
            let element = element.bind(&list.markers, level);
            nested.add(&loop_sum(&element.size, level, list.count));
        } else {
            nested.add(&Lin::constant(self.bytes.saturating_mul(list.count)));
        }
        nested
    }

    /// A closure builtin over `list`: the list, `Begin` (charged its length),
    /// then per element the body and the loop overhead, then the epilogue.
    fn closure(&mut self, function: ClosureFunction, arguments: &'n [&'n Node<'n>]) -> Out {
        let (Some(list_node), Some(Node::Closure { body, alias })) =
            (arguments.first(), arguments.get(1).copied())
        else {
            return Out {
                cost: Lin::constant(u64::MAX),
                val: self.top(),
            };
        };
        let list = self.eval(list_node);
        let level = self.level().saturating_add(1);
        let n = list.val.count;
        let element = list
            .val
            .elem
            .as_deref()
            .map_or_else(|| self.top(), |elem| elem.bind(&list.val.markers, level));
        self.scopes.push(Scope {
            alias: *alias,
            element,
        });
        let body = self.eval(body);
        self.scopes.pop();

        let (per_iteration, epilogue) = overhead(function);
        let mut cost = list.cost.clone();
        cost.add(&list.val.size);
        cost.add(&loop_sum(&body.cost.plus(per_iteration), level, n));
        let mut cost = cost.plus(epilogue.saturating_add(1));
        let val = match function {
            ClosureFunction::Filter => Val {
                size: list.val.size.clone(),
                count: n,
                items_copy_size: list.val.items_copy_size.clone(),
                elem: list.val.elem.clone(),
                markers: list.val.markers.clone(),
                ..Val::default()
            },
            ClosureFunction::Map => {
                let (elem, markers) = self.map_element(&body.val, level);
                Val {
                    size: list.val.size.clone(),
                    count: n,
                    elem: Some(Box::new(elem)),
                    markers,
                    ..Val::default()
                }
            }
            ClosureFunction::FlatMap => {
                // `Flatten` is charged the collected list's length plus the
                // length of every collected list.
                let flattened = loop_sum(&body.val.size, level, n);
                cost.add(&list.val.size);
                cost.add(&flattened);
                let forgotten = body.val.markers.clone();
                let elem = body.val.elem.as_ref().map(|elem| {
                    Box::new(elem.map_paths(&|path: &Path| {
                        let dropped = path.iter().any(|seg| match seg {
                            Seg::Each(Each::Level(l)) => *l >= level,
                            Seg::Each(Each::Marker(m)) => forgotten.contains(m),
                            Seg::Field(_) | Seg::Each(_) => false,
                        });
                        (!dropped).then(|| path.clone())
                    }))
                });
                Val {
                    size: flattened,
                    count: n.saturating_mul(body.val.count),
                    elem,
                    ..Val::default()
                }
            }
            ClosureFunction::All
            | ClosureFunction::None
            | ClosureFunction::Some
            | ClosureFunction::One
            | ClosureFunction::Count => Val::scalar(),
        };
        Out { cost, val }
    }

    /// The element bound of `map(list, body)`. Positions read through the
    /// closure's own element (`level`) differ between iterations, so they
    /// become a fresh template marker: iterating the result later reads
    /// distinct positions again.
    fn map_element(&mut self, body: &Val, level: u32) -> (Val, Vec<u32>) {
        let marker = self.fresh_marker();
        let mark = |path: &Path| -> Option<Path> {
            Some(
                path.iter()
                    .map(|seg| match seg {
                        Seg::Each(Each::Level(l)) if *l == level => Seg::Each(Each::Marker(marker)),
                        other @ (Seg::Field(_) | Seg::Each(_)) => other.clone(),
                    })
                    .collect(),
            )
        };
        let elem = body.map_paths(&mark);
        let markers = if contains_marker(&elem, marker) {
            vec![marker]
        } else {
            Vec::new()
        };
        (elem, markers)
    }
}

/// The largest total weight of the atoms on one root-to-leaf chain of the
/// path tree (a path and every recorded prefix of it).
fn heaviest_chain(pool: &BTreeMap<&Path, u64>) -> u64 {
    pool.keys()
        .map(|leaf| {
            pool.iter()
                .filter(|(prefix, _)| leaf.starts_with(prefix))
                .fold(0_u64, |acc, (_, coef)| acc.saturating_add(*coef))
        })
        .max()
        .unwrap_or(0)
}

fn contains_marker(val: &Val, marker: u32) -> bool {
    let in_path = |path: &Path| path.contains(&Seg::Each(Each::Marker(marker)));
    val.size.atoms.keys().any(in_path)
        || val.origin.as_ref().is_some_and(in_path)
        || val
            .elem
            .as_ref()
            .is_some_and(|elem| contains_marker(elem, marker))
        || val
            .fields
            .as_ref()
            .is_some_and(|fields| fields.values().any(|field| contains_marker(field, marker)))
}
