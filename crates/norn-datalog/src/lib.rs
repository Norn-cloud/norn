//! Portable shared Datalog data and admission contract. No executor or IO.
//!
//! Function-free, range-restricted rules over finite inputs terminate, including
//! positive recursion. Negation must be stratified. All aggregates are refused
//! in v1: even min/max need an admitted finite-domain lattice implementation.
//! Modes annotate intent; neither mode grants OWL runtime semantics or authority.
#![forbid(unsafe_code)]

pub mod overlap;
mod validate;

pub use norn_ir::facts::Value;
use serde::{Deserialize, Serialize};
pub use validate::{Budget, Code, Diagnostic, Group, Plan, validate};

/// Shared AST wire version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Version {
    /// Finite, function-free, stratified Datalog without aggregates.
    #[serde(rename = "norn.datalog.v1")]
    V1,
}

/// Rule intent, distinct from evaluation or authorization policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// Closed-world integrity checking.
    Validation,
    /// Derived relations; runtime OWL semantics remain unapproved.
    Entailment,
}

/// Explicit distinction between supplied inputs and rule-defined relations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelationKind {
    /// Supplied as a finite input, possibly empty. Not writable by rules.
    Input,
    /// Must have at least one defining rule.
    Derived,
}

/// Declared relation signature; v1 has no arithmetic or executable term types.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Predicate {
    /// Stable ASCII symbol.
    pub name: String,
    /// Exact tuple width, including any explicit scope columns.
    pub arity: usize,
    /// Whether supplied externally or defined by rules.
    pub kind: RelationKind,
}

/// Flat term data. Functions, constructors, arithmetic, and fresh IDs have no
/// representation and fail serde decoding. Aggregates have explicit refusal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum Term {
    /// Named variable, bound by a positive body atom.
    Variable(String),
    /// A finite literal in the program's active domain.
    Constant(Value),
    /// Reserved syntax, always rejected by the v1 validator.
    Aggregate(Aggregate),
}

/// Reserved aggregate operations. No backend may silently accept any in v1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Aggregate {
    /// Requires a future finite-domain lattice contract.
    Min,
    /// Requires a future finite-domain lattice contract.
    Max,
    /// Not admitted, including recursive sums that invent values.
    Sum,
    /// Not admitted, including recursive counts that invent values.
    Count,
}

/// One relational atom.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Atom {
    /// Declared relation name.
    pub predicate: String,
    /// Arguments in declared column order.
    pub terms: Vec<Term>,
}

/// Body conjunction. Positive atoms bind variables independent of body order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "atom",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum Literal {
    /// Positive relational premise.
    Positive(Atom),
    /// Closed-world negation over a strictly lower stratum.
    Negative(Atom),
}

impl Literal {
    /// The relational atom, without interpreting its polarity.
    #[must_use]
    pub const fn atom(&self) -> &Atom {
        match self {
            Self::Positive(atom) | Self::Negative(atom) => atom,
        }
    }
}

/// A named clause. A ground, empty-body clause is a finite constant seed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Rule {
    /// Unique stable ASCII identity, also used in witnesses.
    pub id: String,
    /// Validation and entailment remain explicitly distinguished.
    pub mode: Mode,
    /// Derived atom.
    pub head: Atom,
    /// Conjunctive premises; no callbacks or arbitrary executable filters.
    pub body: Vec<Literal>,
}

/// Both future backends consume this data and the same validator result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Program {
    /// Explicit schema.
    pub version: Version,
    /// All external and derived relation signatures.
    pub predicates: Vec<Predicate>,
    /// The rule pack, never generated executable source on this boundary.
    pub rules: Vec<Rule>,
}

impl Program {
    /// Encode through the repository's canonical JSON owner.
    ///
    /// Array/declaration order is retained; this is wire canonicalization,
    /// not logical equivalence or admission. Call [`validate()`] separately.
    ///
    /// # Errors
    /// Returns the existing canonical encoder's size/serialization refusals.
    pub fn canonical_json(&self) -> Result<Vec<u8>, norn_canonical::CanonicalJsonError> {
        norn_canonical::canonical_json_of(self)
    }
}

/// Explanation sidecar only. It must never participate in fact identity,
/// distinctness, or recursive feedback. This slice does not compute witnesses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Provenance {
    /// `None` for a base fact (height zero), otherwise the selected rule ID.
    pub rule_id: Option<String>,
    /// Minimum over current derivations of one plus maximal premise height.
    /// Can increase when alternative support is retracted.
    pub minimal_derivation_height: u64,
    /// Opaque explained disclosure class, never an authorization decision.
    pub class: String,
    /// Opaque explained snapshot/frontier; witnesses expire with that context.
    pub frontier: String,
}
