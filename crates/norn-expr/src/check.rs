//! The static type check of one ZEN standard expression against a closed
//! [`ZenTypeEnv`] (T1-EXPR-2, #2519), and of one unary test against a typed
//! subject (T1-EXPR-3b, #2521), under the `norn-expr/4` structural grammar,
//! published as `norn-zen/6`, and its structural and cost bounds
//! (T1-EXPR-4, #2522).
//!
//! The checker admits a small positive grammar with explicit typed runtime
//! errors for invalid dynamic temporal text and non-integer temporal amounts.
//! Everything outside the grammar is rejected with
//! `expr.construct.unsupported`. It does not block-list zen's surface: zen's
//! own analysis is gradual (zen adoption review F2: `o[k] - 1` checks as
//! `Number` and fails at runtime), and patching its gaps one by one does not
//! converge. Norn types the admitted grammar itself, and then runs zen's
//! strict analysis as a cross-check (any upstream error rejects).
//!
//! # `norn-zen/6`
//!
//! | construct | admitted form |
//! | --- | --- |
//! | literals | `null`, `true`/`false`, exact numbers, strings; list and record literals whose elements all have exactly the same shape. An empty list `[]` needs an expected list type (from the site or the other branch/element context); otherwise it is rejected |
//! | variables | `self`, `actor`, `input` as bound; `$` (the subject) in a unary test; `#` (the innermost element) and `as` aliases inside an admitted closure |
//! | member reads | `record.field` or `record['field']` with a literal name, on a non-null env or literal record. A required to-one link reads as the record; an optional to-one link or a nullable property is a nullable read; a to-many link or inverse is a list. `json-ext` is opaque. Links traverse to the environment's link depth |
//! | index | `[a, b, c][i]` with `i` an integer literal below the list literal's length (`expr.read.index` otherwise, a string key into a list included: zen reads `null` out of bounds) |
//! | nullable values | only as the left of `x ?? fallback` (the fallback has the same non-null type; the result is non-null), in `x == null` / `x != null`, as a `?:` branch, a record-literal field, or the whole result under an optional expected type |
//! | arithmetic | `+ - * /` and unary `-`/`+` on numbers; `+` on two strings concatenates. Division returns null on zero or quotient overflow, so its result is nullable unless the divisor is a literal with absolute value at least one; other decimal overflow is a typed runtime error |
//! | comparison | `==`/`!=` on two non-null scalars of one type (strings and enum values are one type; raw date/timestamp wire values compare for same-kind equality; parsed date/timestamp values compare semantically); `< <= > >=` on numbers or two parsed values of one temporal kind |
//! | membership | `x in list` / `x not in list` with `x` a non-null bool, number, or string and `list` a non-null list of that scalar kind (disjoint literal sets and `[]` are always-constant); `n in [a..b]` (any brackets) with number `n`, `a`, `b`. A range is admitted only as the right side of `in` |
//! | logic | `and`, `or`, `not` on booleans; `c ? a : b` with a boolean condition and branches of one type |
//! | builtins | `len(string or list)`; `split(string, "")` (Unicode scalars plus empty ends); `lower`/`upper`/`trim(string)`; `contains`/`startsWith`/`endsWith(string, string)`; `tokens(text, 'alnum', maxCount, maxLength)` with literal integer caps 0..=256; `take(text, maxScalars)` with a literal integer cap 0..=16384; `join(list of strings, separator)` with a literal separator of at most 256 bytes; `abs`/`floor`/`ceil(number)`; `round`/`trunc(number[, p])` with `p` an integer literal 0..=28; `sum(list of numbers)`; `date(text)` / `timestamp(text)`; `days_between(date, date)` / `add_days(date, integer)`; `seconds_between(timestamp, timestamp)` / `add_seconds(timestamp, integer)` |
//! | closures | `some`/`all`/`none`/`one`/`filter`/`count(list, bool body)`, `map(list, body)` with a non-null body, `flatMap(list, list body)`; the body is itself `norn-expr/4` |
//! | unary tests | ([`check_unary`]) parts joined by `,`/`or`/`and`; each part, as zen desugars it (`> 5` is `$ > 5`, `'a'` is `$ == 'a'`, `[1..5]` and `[1, 2]` are `$ in ...`, a boolean part is used as is), a non-null `bool` under the rules above. `""` is the wildcard |
//! | result | representable as a Norn [`Value`](crate::Value): semantic dates and timestamps are typed JSON strings; checked ZEN temporal values project to canonical JSON strings; intervals are not admitted |
//!
//! Date and timestamp properties remain strings on the wire, but have distinct
//! checker types. They may be compared for equality with the same semantic
//! kind. Call `date(text)` or `timestamp(text)` to obtain a checked ZEN temporal
//! value; only those parsed values support same-kind equality and ordering.
//! Raw wire strings are never compared lexicographically or implicitly parsed.
//! The constructors accept ordinary strings too, so invalid dynamic text can
//! fail with a typed evaluation error; literal arguments are validated during
//! checking. Temporal arithmetic accepts integer-valued numbers at runtime,
//! rejects fractional literals during checking, and fails with a typed
//! evaluation error for fractional dynamic values or range overflow.
//!
//! `tokens` recognizes maximal ASCII `[A-Za-z0-9]+` runs; non-ASCII scalars
//! are boundaries after Norn's NFC input normalization. `take` counts Unicode
//! scalar values. `join` refuses output above 16 KiB. The bounded string
//! builtins are unconditional in the vendored engine (#3404).
//!
//! Not in `norn-zen/6`: user-defined functions and recursion (zen has
//! neither beyond assignments, which are rejected), string templates, slices, indexing other than the
//! in-bounds literal form, dynamic reads, date/time operations (`d()`, date
//! methods, date ranges), a range anywhere but the right of `in`, conversions
//! (`string`/`number`/`bool`), `merge`/`mergeDeep`/`values`/`keys`, regex,
//! aggregates over possibly-empty lists other than `sum`/`count`, `in` on a
//! record (key test), `%`/`^`, assignments, `$root`, `$` outside a unary
//! test, and every other builtin.
//! Widening the profile further belongs to T1-EXPR-6 (#2766), each addition
//! proven total.
//!
//! # Bounds (T1-EXPR-4, #2522)
//!
//! "Non-recursive" is not "bounded" (zen adoption review F3), so an admitted
//! expression is also bounded, structurally and in cost:
//!
//! - Before parsing, the source is at most
//!   [`StructuralLimits::max_source_bytes`] and its brackets nest at most
//!   [`StructuralLimits::max_depth`] deep (`expr.bound.source`,
//!   `expr.bound.depth`). Before any recursive walk, an iterative pass bounds
//!   the AST's node count, depth, and literal sizes (`expr.bound.nodes`,
//!   `expr.bound.depth`, `expr.bound.literal`), and closures nest at most the
//!   limit profile's slice relation depth (`expr.bound.closure_depth`). The
//!   checker's own recursion is therefore bounded.
//! - After it type-checks, a static estimate bounds the metering units of one
//!   evaluation on a conforming slice ([`CostEstimate`]): it follows zen's
//!   compiled opcodes node by node, charges data-proportional opcodes by the
//!   size bounds of their operands, and multiplies closure bodies by the
//!   iterated list's cardinality bound (to-many links: the profile's slice
//!   record cap; other slice lists: the slice byte cap; literals: their
//!   length). An estimate above the profile's decide fuel budget is
//!   `expr.cost.exceeds_budget`, reported with the estimate.
//! - Evaluation is always metered ([`EvaluationBudget`](crate::EvaluationBudget));
//!   running out is the typed [`ExprError::BudgetExhausted`](crate::ExprError).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::rc::Rc;

use zen_expression::functions::{
    ClosureFunction, FunctionKind, InternalFunction, MAX_TAKE_SCALARS, MAX_TOKEN_BYTES,
    MAX_TOKEN_COUNT,
};
use zen_expression::intellisense::diagnostic::Severity;
use zen_expression::intellisense::{AstMetadata, IntelliSense};
use zen_expression::lexer::{ArithmeticOperator, ComparisonOperator, LogicalOperator, Operator};
use zen_expression::parser::Node;
use zen_expression::variable::VariableType;

use norn_ir::ResourceLimitProfile;

use crate::env::{FieldSource, Type, ZenTypeEnv};
use crate::{Expression, Number, UnaryTest};

pub(crate) mod bounds;
mod cost;
mod sites;
mod units;
use units::{Operand, Operation};

pub use bounds::StructuralLimits;

/// The checker profile: the admitted grammar documented in this module, its
/// structural bounds ([`StructuralLimits::NORN_EXPR_4`]), and the static cost
/// bound against the limit profile's decide budget.
pub const CHECKER_PROFILE: &str = "norn-zen/6";

/// Stable diagnostic codes of [`check_expression`].
pub mod codes {
    /// The source does not lex or parse.
    pub const SYNTAX: &str = "expr.syntax.invalid";
    /// A construct or builtin outside the `norn-zen/6` profile.
    pub const UNSUPPORTED_CONSTRUCT: &str = "expr.construct.unsupported";
    /// Operand or branch types that the admitted rules do not accept.
    pub const TYPE_ERROR: &str = "expr.type.error";
    /// A comparison that is always `true` or always `false`.
    pub const CONSTANT_COMPARISON: &str = "expr.type.constant_comparison";
    /// A nullable value used where a non-null value is needed.
    pub const NULLABLE: &str = "expr.type.nullable";
    /// An empty list literal with no expected list type.
    pub const UNTYPED_EMPTY_LIST: &str = "expr.type.untyped_empty_list";
    /// A record member read by a computed key (`o[k]`).
    pub const DYNAMIC_READ: &str = "expr.read.dynamic";
    /// A read of an opaque `json-ext` property.
    pub const OPAQUE_READ: &str = "expr.read.opaque";
    /// A member the record does not have (or a link beyond the link depth).
    pub const UNKNOWN_MEMBER: &str = "expr.member.unknown";
    /// A variable the environment does not bind.
    pub const UNKNOWN_VARIABLE: &str = "expr.variable.unknown";
    /// zen's strict analysis rejects an expression Norn's rules admitted.
    pub const UPSTREAM: &str = "expr.type.upstream";
    /// The result is not assignable to the expected type.
    pub const RESULT_MISMATCH: &str = "expr.result.mismatch";
    /// An index that is not provably in bounds: anything but an integer
    /// literal below the length of an indexed list literal.
    pub const INDEX: &str = "expr.read.index";
    /// The source is longer than the profile's maximum source length.
    pub const BOUND_SOURCE: &str = "expr.bound.source";
    /// The expression has more AST nodes than the profile admits.
    pub const BOUND_NODES: &str = "expr.bound.nodes";
    /// The expression (or its brackets) nests deeper than the profile admits.
    pub const BOUND_DEPTH: &str = "expr.bound.depth";
    /// A string, list, or record literal is larger than the profile admits.
    pub const BOUND_LITERAL: &str = "expr.bound.literal";
    /// Closures nest deeper than the limit profile's slice relation depth.
    pub const BOUND_CLOSURE_DEPTH: &str = "expr.bound.closure_depth";
    /// The static worst-case cost on a conforming slice exceeds the limit
    /// profile's decide fuel budget. The message reports the estimate.
    pub const COST_EXCEEDS_BUDGET: &str = "expr.cost.exceeds_budget";

    /// Mixed, unresolved or unsupported numeric dimensions.
    pub const UNIT: &str = "core.values.unit_mismatch";
    /// Informational diagnostic naming the unit inherited by a literal.
    pub const LITERAL_UNIT: &str = "core.values.literal_unit";

    /// Every code, for registries and exhaustiveness tests.
    pub const ALL: [&str; 21] = [
        UNIT,
        LITERAL_UNIT,
        SYNTAX,
        UNSUPPORTED_CONSTRUCT,
        TYPE_ERROR,
        CONSTANT_COMPARISON,
        NULLABLE,
        UNTYPED_EMPTY_LIST,
        DYNAMIC_READ,
        OPAQUE_READ,
        UNKNOWN_MEMBER,
        UNKNOWN_VARIABLE,
        UPSTREAM,
        RESULT_MISMATCH,
        INDEX,
        BOUND_SOURCE,
        BOUND_NODES,
        BOUND_DEPTH,
        BOUND_LITERAL,
        BOUND_CLOSURE_DEPTH,
        COST_EXCEEDS_BUDGET,
    ];
}

/// One finding of [`check_expression`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Diagnostic {
    /// A stable code from [`codes`].
    pub code: &'static str,
    /// A human-readable message (not a stable contract).
    pub message: String,
    /// Byte span `(start, end)` in the source, when known.
    pub span: (u32, u32),
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} at {}..{}: {}",
            self.code, self.span.0, self.span.1, self.message
        )
    }
}

/// The type the checker proved for an expression. Every variant is
/// representable as a Norn [`Value`](crate::Value). `Date` and `Timestamp`
/// are JSON strings on the wire, but retain their semantic type here so they
/// cannot be used as ordinary text in expressions.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum InferredType {
    /// Always `null`.
    Null,
    /// A boolean.
    Bool,
    /// A string.
    String,
    /// A validated `YYYY-MM-DD` date encoded as a JSON string.
    Date,
    /// A validated UTC timestamp encoded as a JSON string.
    Timestamp,
    /// One string literal.
    Const(String),
    /// One of these strings.
    Enum(Vec<String>),
    /// An exact decimal number.
    Number,
    /// A list.
    List(Box<InferredType>),
    /// A record with exactly these fields.
    Record(BTreeMap<String, InferredType>),
    /// A value or `null`.
    Optional(Box<InferredType>),
}

impl fmt::Display for InferredType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Null => f.write_str("null"),
            Self::Bool => f.write_str("bool"),
            Self::String => f.write_str("string"),
            Self::Date => f.write_str("date"),
            Self::Timestamp => f.write_str("timestamp"),
            Self::Const(value) => write!(f, "{value:?}"),
            Self::Enum(values) => write!(f, "enum{values:?}"),
            Self::Number => f.write_str("number"),
            Self::List(item) => write!(f, "list<{item}>"),
            Self::Record(fields) => {
                let names: Vec<&str> = fields.keys().map(String::as_str).collect();
                write!(f, "record{{{}}}", names.join(", "))
            }
            Self::Optional(item) => write!(f, "{item}?"),
        }
    }
}

/// A proved upper bound on the metering units one evaluation of an admitted
/// expression (or unary test) charges on a conforming slice: an evaluation
/// under a budget of at least this many units never exhausts it.
///
/// "Conforming" means the context (and a unary test's subject) conforms to
/// the type environment, its Norn1 canonical JSON is at most the limit
/// profile's `slice.max_canonical_bytes`, and every list of records holds at
/// most `slice.max_records` records. The kernel's slice reader enforces both
/// bounds; the estimate is not a bound for other inputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CostEstimate(u64);

impl CostEstimate {
    /// The bound, in metering units.
    #[must_use]
    pub const fn units(self) -> u64 {
        self.0
    }
}

impl fmt::Display for CostEstimate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} units", self.0)
    }
}

/// Replayable fuel claim for one expression, conditional on conforming slice bounds.
/// This is not a whole graph, writer, IO or allocation certificate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuelCertificate {
    /// Exact bound, not a floating-point estimate or an unchecked solver answer.
    pub units: u64,
    /// Identity of the exact source, environment, expected type and declared limits.
    pub statement: String,
    /// Content identity of the checker and metered implementation.
    pub semantics: String,
}

impl FuelCertificate {
    /// Reconstruct the derivation from authoritative source, environment and limits.
    /// Unknown expressions and over-budget claims never produce evidence.
    ///
    /// # Errors
    /// Refuses a stale semantics identity, changed bound, or failed admission.
    pub fn verify(
        &self,
        expression: &Expression,
        env: &ZenTypeEnv,
        expected: &Type,
        limits: &ResourceLimitProfile,
    ) -> Result<CheckedExpression, String> {
        if self.semantics.len() != 64
            || self.statement.len() != 64
            || self.semantics != fuel_semantics()?
        {
            return Err("stale expression fuel semantics".into());
        }
        let (checked, reconstructed) = certify_expression(expression, env, expected, limits)?;
        if self.statement != reconstructed.statement {
            return Err("expression fuel certificate statement changed".into());
        }
        if checked.cost().units() != self.units || self.units == u64::MAX {
            return Err("expression fuel certificate bound differs from derivation".into());
        }
        Ok(checked)
    }
}

/// Admit one assignment and retain replayable cost evidence. All limits and
/// types are supplied by admission authority, never taken from a certificate.
///
/// # Errors
/// Refuses unsupported expressions, over-budget or overflowing bounds.
pub fn certify_expression(
    expression: &Expression,
    env: &ZenTypeEnv,
    expected: &Type,
    limits: &ResourceLimitProfile,
) -> Result<(CheckedExpression, FuelCertificate), String> {
    let checked = check_expression_with_sources_and_root_bounds(
        expression, env, expected, limits, None, true,
    )
    .map(|(checked, _)| checked)
    .map_err(|errors| format!("expression cost admission refused: {errors:?}"))?;
    if checked.cost().units() == u64::MAX {
        return Err("unknown or overflowing expression cost".into());
    }
    let statement = norn_ir::canonical::domain_hash_hex(
        b"norn.expression-cost-statement/1\0",
        &(
            expression.source().as_bytes(),
            norn_ir::canonical::serialize(&env.to_canonical_value())
                .map_err(|error| error.to_string())?,
            norn_ir::canonical::serialize(
                &expected.to_canonical_value(crate::env::TypeEnvVersion::V2),
            )
            .map_err(|error| error.to_string())?,
            limits,
        ),
    )
    .map_err(|error| error.to_string())?;
    let certificate = FuelCertificate {
        units: checked.cost().units(),
        statement,
        semantics: fuel_semantics()?,
    };
    Ok((checked, certificate))
}

/// Fingerprint cost-bearing implementation sources once per process. Source edits
/// invalidate old evidence even when an author forgets to bump the profile name.
fn fuel_semantics() -> Result<String, String> {
    static IDENTITY: std::sync::OnceLock<Result<String, String>> = std::sync::OnceLock::new();
    IDENTITY
        .get_or_init(|| fuel_semantics_for(FUEL_SOURCES))
        .clone()
}

const FUEL_SOURCES: &[(&str, &str)] = include!(concat!(env!("OUT_DIR"), "/fuel_sources.rs"));

fn fuel_semantics_for(sources: &[(&str, &str)]) -> Result<String, String> {
    norn_ir::canonical::domain_hash_hex(b"norn.expression-fuel/1\0", &sources)
        .map_err(|error| error.to_string())
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "regression assertions")]
mod fuel_identity_tests {
    use super::*;
    use crate::EnvOptions;

    #[test]
    fn dependency_changes_invalidate_old_evidence() {
        let env =
            ZenTypeEnv::input_only(BTreeMap::new(), EnvOptions::default()).expect("environment");
        let expression = Expression::compile("1").expect("compile");
        let limits = ResourceLimitProfile::norn_limits_v1();
        let (_, certificate) =
            certify_expression(&expression, &env, &Type::Number, &limits).expect("certificate");
        certificate
            .verify(&expression, &env, &Type::Number, &limits)
            .expect("replay");
        for (path, before, after) in [
            (
                "vendor/zen/core/expression/src/functions/mod.rs",
                "MAX_JOIN_BYTES: usize = 16_384",
                "MAX_JOIN_BYTES: usize = 32_768",
            ),
            (
                "vendor/zen/core/expression/src/functions/defs.rs",
                "",
                "// changed definition\n",
            ),
            (
                "crates/norn-expr/src/check/sites.rs",
                "",
                "// changed lowering\n",
            ),
        ] {
            let mut sources = FUEL_SOURCES.to_vec();
            let entry = sources
                .iter_mut()
                .find(|(name, _)| *name == path)
                .expect("dependency included");
            let changed = entry.1.replacen(before, after, 1);
            assert_ne!(changed, entry.1);
            entry.1 = &changed;
            let mut old = certificate.clone();
            old.semantics = fuel_semantics_for(&sources).expect("changed identity");
            assert_ne!(old.semantics, certificate.semantics);
            assert_eq!(
                old.verify(&expression, &env, &Type::Number, &limits),
                Err("stale expression fuel semantics".into())
            );
        }
    }
}

/// An admitted expression: its proved type and its static cost bound.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckedExpression {
    inferred: InferredType,
    cost: CostEstimate,
    exact: bool,
    notes: Vec<Diagnostic>,
    runtime_source: String,
}

/// Owned source provenance for a bounded, checked expression's construction.
/// This records syntax rather than evaluated values or inferred numeric types.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConstructionSource {
    /// The exact source of a value that is not an object or array construction.
    Scalar(String),
    /// An object construction with literal, unique member names.
    Record(BTreeMap<String, ConstructionSource>),
    /// An array construction retaining every element separately.
    List(Vec<ConstructionSource>),
}

impl CheckedExpression {
    /// VAL-ROUND: source with declared result sites lowered to shared rescale.
    #[must_use]
    pub fn runtime_source(&self) -> &str {
        &self.runtime_source
    }

    /// VAL-UNIT: informational literal-unit inference evidence.
    #[must_use]
    pub fn notes(&self) -> &[Diagnostic] {
        &self.notes
    }

    /// VAL-EXACT-1: the expression reads or produces a declared Decimal.
    #[must_use]
    pub const fn uses_exact_arithmetic(&self) -> bool {
        self.exact
    }

    /// The proved type of the result.
    #[must_use]
    pub const fn inferred(&self) -> &InferredType {
        &self.inferred
    }

    /// Consumes the result, returning the proved type.
    #[must_use]
    pub fn into_inferred(self) -> InferredType {
        self.inferred
    }

    /// The worst-case metering cost of one evaluation on a conforming slice,
    /// at most the limit profile's decide fuel budget.
    #[must_use]
    pub const fn cost(&self) -> CostEstimate {
        self.cost
    }
}

/// Type-checks `expr` in `env` under [`CHECKER_PROFILE`] and the shipped limit
/// profile `norn.limits/1`, and checks its result against `expected`.
///
/// [`check_expression_with`] under
/// [`ResourceLimitProfile::norn_limits_v1`], returning only the type.
///
/// # Errors
///
/// Every [`Diagnostic`] found.
pub fn check_expression(
    expr: &Expression,
    env: &ZenTypeEnv,
    expected: &Type,
) -> Result<InferredType, Vec<Diagnostic>> {
    check_expression_with(expr, env, expected, &ResourceLimitProfile::norn_limits_v1())
        .map(CheckedExpression::into_inferred)
}

/// Validates the source-level bounds and exact numeric literals used by an
/// expression before evaluation, without checking its type environment or
/// admitting its full grammar.
///
/// This is for runtimes that receive an expression already admitted against
/// its owning definition. It applies the same `norn-expr/4` structural bounds
/// as [`check_expression_with`], including rejecting numeric tokens whose
/// original spelling is not an exact plain decimal and enforcing the literal
/// controls on bounded string builtins. It does not replace
/// [`check_expression_with`] or prove that the source is in [`CHECKER_PROFILE`].
///
/// # Errors
///
/// Every source-level diagnostic found.
pub fn validate_evaluation_source(
    source: &str,
    limits: &ResourceLimitProfile,
) -> Result<(), Vec<Diagnostic>> {
    validate_source(source, limits, false)
}

/// Validates source-level bounds for a JDM unary-test cell, including the
/// literal-only controls of bounded string builtins, without a type
/// environment. This is used by the decision loader as a fail-closed check
/// of the syntax-only artifact boundary.
///
/// # Errors
///
/// Every source-level diagnostic found.
pub fn validate_unary_evaluation_source(
    source: &str,
    limits: &ResourceLimitProfile,
) -> Result<(), Vec<Diagnostic>> {
    validate_source(source, limits, true)
}

fn validate_source(
    source: &str,
    limits: &ResourceLimitProfile,
    unary: bool,
) -> Result<(), Vec<Diagnostic>> {
    let structural = StructuralLimits::NORN_EXPR_4;
    if let Some(found) = bounds::lexical(source, &structural, unary) {
        return Err(vec![found]);
    }
    let mut intellisense = IntelliSense::new().with_strict(true);
    let outcome = intellisense.with_ast(source, unary, |ast, metadata| {
        let span = |node: &Node<'_>| metadata.get(&address(node)).map_or((0, 0), |m| m.span);
        bounds::structure(
            ast,
            source,
            &structural,
            limits.slice.max_relation_depth,
            span,
        )
    });
    match outcome {
        Some(diagnostics) if diagnostics.is_empty() => Ok(()),
        Some(diagnostics) => Err(diagnostics),
        None => Err(vec![Diagnostic {
            code: codes::SYNTAX,
            message: "expression does not parse".to_owned(),
            span: (0, 0),
        }]),
    }
}

/// Type-checks `expr` in `env` under [`CHECKER_PROFILE`] and checks its result
/// against `expected`, bounding it by `limits`.
///
/// Returns the proved [`InferredType`] of the result and its [`CostEstimate`],
/// or every finding. See the module docs for the admitted grammar. Numeric
/// scale and integrality are not proved statically: a `number` result is
/// assignable to `integer`/`decimal`. Date and timestamp properties retain
/// distinct inferred types and may be projected to unformatted string-shaped
/// rule outputs; shared temporal property invariants enforce their wire formats
/// before admitted expressions run on committed values. Duration remains an
/// ordinary string.
///
/// Before any recursive walk the expression must pass the structural bounds
/// ([`StructuralLimits::NORN_EXPR_4`]; closures nest at most
/// `limits.slice.max_relation_depth` deep). After it type-checks, its static
/// worst-case cost on a conforming slice must be at most
/// `limits.decide.fuel_units` (`expr.cost.exceeds_budget` otherwise, with the
/// estimate in the message).
///
/// # Errors
///
/// Every [`Diagnostic`] found.
pub fn check_expression_with(
    expr: &Expression,
    env: &ZenTypeEnv,
    expected: &Type,
    limits: &ResourceLimitProfile,
) -> Result<CheckedExpression, Vec<Diagnostic>> {
    check_expression_with_sources(expr, env, expected, limits).map(|(checked, _)| checked)
}

/// Enforced bounds on one independently bounded expression-context root.
///
/// These are assumptions of the cost proof, not runtime enforcement or
/// authorization. Callers must enforce both caps before evaluating the
/// expression. Aliases of the same value may be assigned separate bounds;
/// this conservatively counts their overlapping reads independently.
#[derive(Debug, Clone, Copy)]
pub struct RootCostBounds {
    /// Maximum canonical JSON bytes of this root, including all descendants.
    pub max_canonical_bytes: u64,
    /// Maximum records in a list read from this root. Use `u64::MAX` when
    /// only the canonical byte cap bounds record-list cardinality.
    pub max_records: u64,
}

/// Checks an expression whose named context roots have independent enforced
/// bounds. Costs from separate roots add; their bytes are never pooled.
/// Unlisted roots retain the default shared context bound from `limits`.
/// Structural, type, and fuel gates are identical to [`check_expression_with`].
///
/// # Errors
///
/// Every [`Diagnostic`] found by the expression checker.
pub fn check_expression_with_root_bounds(
    expr: &Expression,
    env: &ZenTypeEnv,
    expected: &Type,
    limits: &ResourceLimitProfile,
    root_bounds: &BTreeMap<String, RootCostBounds>,
) -> Result<CheckedExpression, Vec<Diagnostic>> {
    check_expression_with_sources_and_root_bounds(
        expr,
        env,
        expected,
        limits,
        Some(root_bounds),
        true,
    )
    .map(|(checked, _)| checked)
}

/// Checks an expression with the same bounds and grammar as
/// [`check_expression_with`], retaining its owned construction sources.
///
/// Provenance is unavailable when any required AST span or literal member key
/// cannot be projected faithfully. Parentheses are unwrapped, while arbitrary
/// operations remain scalar sources; this is not an integrality proof.
///
/// # Errors
///
/// Every [`Diagnostic`] found by the expression checker.
pub fn check_expression_with_sources(
    expr: &Expression,
    env: &ZenTypeEnv,
    expected: &Type,
    limits: &ResourceLimitProfile,
) -> Result<(CheckedExpression, Option<ConstructionSource>), Vec<Diagnostic>> {
    check_expression_with_sources_and_root_bounds(expr, env, expected, limits, None, false)
}

pub(crate) fn check_typed_expression(
    expr: &Expression,
    env: &ZenTypeEnv,
    expected: &Type,
) -> Result<CheckedExpression, Vec<Diagnostic>> {
    check_expression_with_sources_and_root_bounds(
        expr,
        env,
        expected,
        &ResourceLimitProfile::norn_limits_v1(),
        None,
        true,
    )
    .map(|(checked, _)| checked)
}

/// VAL-EXACT-1: select the exact runtime only for declared reads/results or an explicit site.
pub(crate) fn requires_exact_profile(expr: &Expression, env: &ZenTypeEnv, expected: &Type) -> bool {
    if expected_exact_type(expected, env) {
        return true;
    }
    let limits = ResourceLimitProfile::norn_limits_v1();
    IntelliSense::new().with_ast(expr.source(), false, |ast, metadata| {
        let span = |node: &Node<'_>| metadata.get(&address(node)).map_or((0, 0), |m| m.span);
        if !bounds::structure(ast, expr.source(), &StructuralLimits::NORN_EXPR_4, limits.slice.max_relation_depth, span).is_empty() { return true; }
        let mut pending = vec![ast];
        while let Some(node) = pending.pop() {
            if matches!(node, Node::FunctionCall { kind: FunctionKind::Internal(InternalFunction::Round), arguments } if arguments.len() == 3) { return true; }
            if let Some(path) = crate::reads::literal_path(node) {
                if let Some((root, members)) = path.split_first() && let Some(ty) = env.variables().get(root) && read_requires_exact(ty, members, env) { return true; }
            } else {
                bounds::children(node, &mut pending);
            }
        }
        false
    }).unwrap_or(true)
}

fn read_requires_exact<'a>(mut ty: &'a Type, members: &[String], env: &'a ZenTypeEnv) -> bool {
    for member in members {
        while let Type::Optional(inner) | Type::List(inner) = ty {
            ty = inner;
        }
        let Type::Record(name) = ty else {
            return exact_type(ty);
        };
        let Some(field) = env
            .record(name)
            .and_then(|record| record.fields.get(member))
        else {
            return expected_exact_type(ty, env);
        };
        ty = &field.ty;
    }
    expected_exact_type(ty, env)
}

struct AnalysisContext<'a> {
    source: &'a str,
    env: &'a ZenTypeEnv,
    expected: &'a Type,
    limits: &'a ResourceLimitProfile,
    root_bounds: Option<&'a BTreeMap<String, RootCostBounds>>,
    exact_enabled: bool,
    /// Read paths a guarding condition proved non-null.
    assumed: &'a [Vec<String>],
}
struct Analysis {
    inferred: Option<InferredType>,
    diagnostics: Vec<Diagnostic>,
    span: (u32, u32),
    cost: Option<u64>,
    construction: Option<ConstructionSource>,
    exact: bool,
    notes: Vec<Diagnostic>,
    runtime_source: Result<String, String>,
}
fn analyze_expression(
    ast: &Node<'_>,
    metadata: &AstMetadata,
    context: &AnalysisContext<'_>,
) -> Analysis {
    let AnalysisContext {
        source,
        env,
        expected,
        limits,
        root_bounds,
        exact_enabled,
        assumed,
    } = *context;
    let structural = StructuralLimits::NORN_EXPR_4;
    let span = |node: &Node<'_>| metadata.get(&address(node)).map_or((0, 0), |m| m.span);
    let whole = span(ast);
    let found = bounds::structure(
        ast,
        source,
        &structural,
        limits.slice.max_relation_depth,
        span,
    );
    if !found.is_empty() {
        return Analysis {
            inferred: None,
            diagnostics: found,
            span: whole,
            cost: None,
            construction: None,
            exact: false,
            notes: Vec::new(),
            runtime_source: Ok(source.to_owned()),
        };
    }
    let mut checker = Checker::new(env, metadata, None);
    checker.exact_math = exact_enabled;
    checker.non_null = assumed.to_vec();
    let expectation = checker.expectation(expected);
    let found = checker.infer(ast, expectation.as_ref());
    if exact_enabled && let (Some(found), Some(expected)) = (&found, &expectation) {
        checker.check_destination(ast, found, expected);
    }
    let inferred = found.and_then(|ty| checker.representable(&ty, whole));
    let cost = (inferred.is_some() && checker.diagnostics.is_empty()).then(|| {
        cost::Estimator::new(env, &checker.types, limits)
            .with_root_bounds(root_bounds)
            .estimate(ast)
    });
    let construction = construction_source(ast, metadata, source);
    let exact = checker.exact || expected_exact_type(expected, env);
    let runtime_source = if exact_enabled && exact {
        if checker
            .types
            .values()
            .any(|ty| matches!(ty, T::Number(Operand::Legacy)))
        {
            checker.report::<()>(
                ast,
                "core.values.inexact",
                "bare number cannot enter a declared exact site".into(),
            );
        }
        sites::lower(ast, expected, env, metadata, source)
    } else {
        Ok(source.to_owned())
    };

    Analysis {
        inferred,
        diagnostics: checker.diagnostics,
        span: whole,
        cost,
        construction,
        exact,
        notes: checker.notes,
        runtime_source,
    }
}

fn check_expression_with_sources_and_root_bounds(
    expr: &Expression,
    env: &ZenTypeEnv,
    expected: &Type,
    limits: &ResourceLimitProfile,
    root_bounds: Option<&BTreeMap<String, RootCostBounds>>,
    exact_enabled: bool,
) -> Result<(CheckedExpression, Option<ConstructionSource>), Vec<Diagnostic>> {
    check_guarded(expr, env, expected, limits, root_bounds, exact_enabled, &[])
}

/// [`check_expression`] for an expression evaluated only when a guarding
/// condition holds (a law's response under its condition): every literal read
/// path the condition proves non-null is read as non-null.
///
/// # Errors
///
/// Every [`Diagnostic`] found.
pub fn check_expression_under(
    expr: &Expression,
    condition: &Expression,
    env: &ZenTypeEnv,
    expected: &Type,
) -> Result<InferredType, Vec<Diagnostic>> {
    check_guarded(
        expr,
        env,
        expected,
        &ResourceLimitProfile::norn_limits_v1(),
        None,
        false,
        &condition_null_guards(condition),
    )
    .map(|(checked, _)| checked.into_inferred())
}

fn check_guarded(
    expr: &Expression,
    env: &ZenTypeEnv,
    expected: &Type,
    limits: &ResourceLimitProfile,
    root_bounds: Option<&BTreeMap<String, RootCostBounds>>,
    exact_enabled: bool,
    assumed: &[Vec<String>],
) -> Result<(CheckedExpression, Option<ConstructionSource>), Vec<Diagnostic>> {
    let source = expr.source();
    let structural = StructuralLimits::NORN_EXPR_4;
    if let Some(found) = bounds::lexical(source, &structural, false) {
        return Err(vec![found]);
    }
    let mut intellisense = IntelliSense::new().with_strict(true);
    let context = AnalysisContext {
        source,
        env,
        expected,
        limits,
        root_bounds,
        exact_enabled,
        assumed,
    };
    let outcome = intellisense.with_ast(source, false, |ast, metadata| {
        analyze_expression(ast, metadata, &context)
    });
    let Some(Analysis {
        inferred,
        mut diagnostics,
        span,
        cost,
        construction,
        exact,
        notes,
        runtime_source,
    }) = outcome
    else {
        return Err(vec![Diagnostic {
            code: codes::SYNTAX,
            message: "expression does not parse".to_owned(),
            span: (0, 0),
        }]);
    };
    // Only declared exact sites use error-on-zero division. Plain Number keeps
    // the legacy nullable division contract in this same checker.
    if exact_enabled && !exact {
        return check_guarded(expr, env, expected, limits, root_bounds, false, assumed);
    }
    if !diagnostics.is_empty() {
        return Err(diagnostics);
    }
    let (Some(inferred), Some(cost)) = (inferred, cost) else {
        return Err(vec![Diagnostic {
            code: codes::UNSUPPORTED_CONSTRUCT,
            message: format!("the expression is outside {CHECKER_PROFILE}"),
            span,
        }]);
    };
    let runtime_source = runtime_source.map_err(|message| {
        vec![Diagnostic {
            code: "core.values.inexact",
            message,
            span,
        }]
    })?;
    if exact_enabled && exact {
        diagnostics.extend(validate_lowered_source(&runtime_source, span));
    }
    // Cross-check: zen's strict analysis must not report an error either.
    let root = ZenEnv::new(env).root(None);
    let analysis = intellisense.analyze(source, &root);
    diagnostics.extend(upstream_errors(&analysis.diagnostics));
    if !diagnostics.is_empty() {
        return Err(diagnostics);
    }
    if !assignable(&inferred, expected, env) {
        return Err(vec![Diagnostic {
            code: codes::RESULT_MISMATCH,
            message: format!("expected `{expected}`, found `{inferred}`"),
            span,
        }]);
    }
    let cost = lowered_cost(source, &runtime_source, cost, limits, span)?;
    Ok((
        CheckedExpression {
            inferred,
            cost,
            exact,
            notes,
            runtime_source,
        },
        construction,
    ))
}

fn lowered_cost(
    source: &str,
    runtime_source: &str,
    cost: u64,
    limits: &ResourceLimitProfile,
    span: (u32, u32),
) -> Result<CostEstimate, Vec<Diagnostic>> {
    let quantizers = runtime_source
        .matches("round((")
        .count()
        .saturating_sub(source.matches("round((").count());
    within_budget(
        cost.saturating_mul(if runtime_source.contains("== null ? null : round") {
            2
        } else {
            1
        })
        .saturating_add(
            u64::try_from(quantizers)
                .unwrap_or(u64::MAX)
                .saturating_mul(8),
        ),
        limits,
        span,
    )
}

fn validate_lowered_source(source: &str, span: (u32, u32)) -> Vec<Diagnostic> {
    let mut parser = IntelliSense::new();
    parser
        .with_ast(source, false, |ast, metadata| {
            exact_admission(ast, metadata, false)
        })
        .unwrap_or_else(|| {
            vec![Diagnostic {
                code: codes::SYNTAX,
                message: "lowered exact sites do not parse".into(),
                span,
            }]
        })
}

fn construction_source(
    node: &Node<'_>,
    metadata: &AstMetadata,
    source: &str,
) -> Option<ConstructionSource> {
    let text = node_source_text(node, metadata, source)?;
    match node {
        Node::Parenthesized(inner) => construction_source(inner, metadata, source),
        Node::Array(items) => items
            .iter()
            .map(|item| construction_source(item, metadata, source))
            .collect::<Option<Vec<_>>>()
            .map(ConstructionSource::List),
        Node::Object(entries) => {
            let mut fields = BTreeMap::new();
            for (key, value) in *entries {
                node_source_text(key, metadata, source)?;
                let Node::String(name) = key else {
                    return None;
                };
                let value = construction_source(value, metadata, source)?;
                if fields.insert((*name).to_owned(), value).is_some() {
                    return None;
                }
            }
            Some(ConstructionSource::Record(fields))
        }
        Node::Null
        | Node::Bool(_)
        | Node::Number(_)
        | Node::String(_)
        | Node::TemplateString(_)
        | Node::Pointer
        | Node::Assignments { .. }
        | Node::Identifier(_)
        | Node::Closure { .. }
        | Node::Root
        | Node::Member { .. }
        | Node::Slice { .. }
        | Node::Interval { .. }
        | Node::Conditional { .. }
        | Node::Unary { .. }
        | Node::Binary { .. }
        | Node::FunctionCall { .. }
        | Node::MethodCall { .. }
        | Node::Error { .. } => Some(ConstructionSource::Scalar(text.to_owned())),
    }
}

fn node_source_text<'s>(
    node: &Node<'_>,
    metadata: &AstMetadata,
    source: &'s str,
) -> Option<&'s str> {
    let (start, end) = metadata.get(&address(node))?.span;
    let text = source.get(usize::try_from(start).ok()?..usize::try_from(end).ok()?)?;
    (!text.is_empty()).then_some(text)
}

/// `units` as a [`CostEstimate`] if it is within the decide fuel budget.
fn within_budget(
    units: u64,
    limits: &ResourceLimitProfile,
    span: (u32, u32),
) -> Result<CostEstimate, Vec<Diagnostic>> {
    if units <= limits.decide.fuel_units {
        Ok(CostEstimate(units))
    } else {
        Err(vec![Diagnostic {
            code: codes::COST_EXCEEDS_BUDGET,
            message: format!(
                "the worst-case cost on a conforming slice is {units} units, more than the \
                 decide budget of {} units of `{}/{}`",
                limits.decide.fuel_units, limits.id, limits.version
            ),
            span,
        }])
    }
}

/// The name of a unary test's subject, the column value it tests.
pub const SUBJECT_VARIABLE: &str = "$";

/// Type-checks the unary test `test` against a subject `$` of type `subject`
/// in `env`, under the same allowlist as [`check_expression`] and the shipped
/// limit profile `norn.limits/1`.
///
/// [`check_unary_with`] under [`ResourceLimitProfile::norn_limits_v1`].
///
/// # Errors
///
/// Every [`Diagnostic`] found.
pub fn check_unary(
    test: &UnaryTest,
    env: &ZenTypeEnv,
    subject: &Type,
) -> Result<(), Vec<Diagnostic>> {
    check_unary_with(test, env, subject, &ResourceLimitProfile::norn_limits_v1()).map(|_| ())
}

/// Type-checks the unary test `test` against a subject `$` of type `subject`
/// in `env`, under the same allowlist, structural bounds, and cost bound as
/// [`check_expression_with`], and returns its [`CostEstimate`] (one cell
/// evaluation; the subject is bounded like a slice of its own).
///
/// zen desugars each part of a unary test (joined by `,` as `or`, or by `and`
/// / `or`) into a standard expression over `$`; see [`UnaryTest`]. Every part
/// must check as a non-null `bool` under [`CHECKER_PROFILE`]. Invalid dynamic
/// temporal operands can still fail with a typed runtime error. In
/// particular `> 5` on a nullable subject is `expr.type.nullable` (handle it
/// with `$ != null and ($ ?? 0) > 5`, or a non-null column), and `'retired'`
/// against an enum subject that lacks that value is
/// `expr.type.constant_comparison`. The empty wildcard test always checks,
/// at no cost.
///
/// # Errors
///
/// Every [`Diagnostic`] found.
pub fn check_unary_with(
    test: &UnaryTest,
    env: &ZenTypeEnv,
    subject: &Type,
    limits: &ResourceLimitProfile,
) -> Result<CostEstimate, Vec<Diagnostic>> {
    if test.is_wildcard() {
        return Ok(CostEstimate(0));
    }
    let source = test.source();
    let structural = StructuralLimits::NORN_EXPR_4;
    if let Some(found) = bounds::lexical(source, &structural, true) {
        return Err(vec![found]);
    }
    let mut intellisense = IntelliSense::new().with_strict(true);
    let outcome = intellisense.with_ast(source, true, |ast, metadata| {
        let span = |node: &Node<'_>| metadata.get(&address(node)).map_or((0, 0), |m| m.span);
        let whole = span(ast);
        let found = bounds::structure(
            ast,
            source,
            &structural,
            limits.slice.max_relation_depth,
            span,
        );
        if !found.is_empty() {
            return (found, whole, None);
        }
        let mut checker = Checker::new(env, metadata, Some(subject));
        checker.unary_test(ast);
        let cost = checker
            .diagnostics
            .is_empty()
            .then(|| cost::Estimator::new(env, &checker.types, limits).estimate(ast));
        (checker.diagnostics, whole, cost)
    });
    let Some((diagnostics, span, cost)) = outcome else {
        return Err(vec![Diagnostic {
            code: codes::SYNTAX,
            message: "unary test does not parse".to_owned(),
            span: (0, 0),
        }]);
    };
    if !diagnostics.is_empty() {
        return Err(diagnostics);
    }
    let root = ZenEnv::new(env).root(Some(subject));
    let analysis = intellisense.analyze_unary(source, &root);
    let upstream: Vec<Diagnostic> = upstream_errors(&analysis.diagnostics);
    if !upstream.is_empty() {
        return Err(upstream);
    }
    within_budget(cost.unwrap_or(u64::MAX), limits, span)
}

/// zen's strict-analysis errors, as `expr.type.upstream` diagnostics.
fn upstream_errors(
    diagnostics: &[zen_expression::intellisense::diagnostic::Diagnostic],
) -> Vec<Diagnostic> {
    diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .map(|d| Diagnostic {
            code: codes::UPSTREAM,
            message: d.message.clone(),
            span: d.span,
        })
        .collect()
}

/// Whether a value of type `found` may be used where `expected` is declared.
fn assignable(found: &InferredType, expected: &Type, env: &ZenTypeEnv) -> bool {
    use InferredType as I;
    match (found, expected) {
        (I::Optional(inner), Type::Optional(expected)) => assignable(inner, expected, env),
        (I::Optional(_) | I::Null, other) => matches!(other, Type::Optional(_) | Type::Json),
        (_, Type::Json) => true,
        (found, Type::Optional(expected)) => assignable(found, expected, env),
        (found, expected) => {
            matches!(
                (found, expected),
                (I::Bool, Type::Bool)
                    | (
                        I::Number,
                        Type::Number | Type::Integer | Type::Decimal { .. } | Type::DecimalBinding { .. }
                    )
                    | (
                        I::String | I::Const(_) | I::Enum(_),
                        Type::Text { .. } | Type::Duration,
                    )
                    // Temporal values are JSON strings, so a checked rule
                    // output may project them to a closed string schema.
                    // Their inferred type stays temporal and cannot then
                    // participate in ordinary string operations.
                    | (I::Date | I::Timestamp, Type::Text { format: None })
                    | (I::Date, Type::Date)
                    | (I::Timestamp, Type::Timestamp { .. })
            ) || structural(found, expected, env)
        }
    }
}

/// The assignability rules that inspect members of both types.
fn structural(found: &InferredType, expected: &Type, env: &ZenTypeEnv) -> bool {
    use InferredType as I;
    match (found, expected) {
        (I::Const(value), Type::Enum { values }) => values.contains(value),
        (I::Enum(found), Type::Enum { values }) => found.iter().all(|v| values.contains(v)),
        (I::List(item), Type::List(expected)) => assignable(item, expected, env),
        (I::Record(fields), Type::Record(name)) => env.record(name).is_some_and(|record| {
            record.fields.iter().all(|(field_name, field)| {
                !matches!(
                    field.source,
                    FieldSource::Property { .. } | FieldSource::Builtin
                ) || fields
                    .get(field_name)
                    .is_some_and(|found| assignable(found, &field.ty, env))
            })
        }),
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// The environment as a zen root type
// ---------------------------------------------------------------------------

struct ZenEnv<'a> {
    env: &'a ZenTypeEnv,
    memo: HashMap<(String, u32), VariableType>,
}

impl<'a> ZenEnv<'a> {
    fn new(env: &'a ZenTypeEnv) -> Self {
        Self {
            env,
            memo: HashMap::new(),
        }
    }

    /// The root object: one member per bound variable, plus a unary test's
    /// subject `$`. Links expand to the environment's link depth; beyond it
    /// a link is not a member.
    fn root(mut self, subject: Option<&Type>) -> VariableType {
        let depth = self.env.options().max_link_depth;
        let root = VariableType::empty_object();
        if let VariableType::Object(members) = &root {
            for (name, ty) in self.env.variables() {
                let ty = self.lower(ty, depth);
                members.borrow_mut().insert(Rc::from(name.as_str()), ty);
            }
            if let Some(subject) = subject {
                let ty = self.lower(subject, depth);
                members.borrow_mut().insert(Rc::from(SUBJECT_VARIABLE), ty);
            }
        }
        root
    }

    fn lower(&mut self, ty: &Type, depth: u32) -> VariableType {
        match ty {
            Type::Bool => VariableType::Bool,
            // Temporal properties have semantic checker types, but their
            // runtime representation is still the canonical string wire form.
            // Keep ZEN's cross-check aligned with the values it receives.
            Type::Text { .. } | Type::Timestamp { .. } | Type::Date | Type::Duration => {
                VariableType::String
            }
            Type::Integer | Type::Decimal { .. } | Type::DecimalBinding { .. } | Type::Number => {
                VariableType::Number
            }
            Type::Enum { values } => {
                VariableType::Enum(None, values.iter().map(|v| Rc::from(v.as_str())).collect())
            }
            // Opaque: any read of it is rejected (`expr.read.opaque`).
            Type::Json => VariableType::Any,
            Type::Record(name) => self.record(name, depth),
            Type::List(item) => VariableType::Array(Rc::new(self.lower(item, depth))),
            Type::Optional(item) => VariableType::Nullable(Rc::new(self.lower(item, depth))),
        }
    }

    fn record(&mut self, name: &str, depth: u32) -> VariableType {
        let key = (name.to_owned(), depth);
        if let Some(ty) = self.memo.get(&key) {
            return ty.clone();
        }
        let object = VariableType::empty_object();
        if let (Some(record), VariableType::Object(members)) = (self.env.record(name), &object) {
            for (field_name, field) in &record.fields {
                let lowered = match field.source {
                    FieldSource::Link { .. } | FieldSource::Inverse { .. } => {
                        let Some(next) = depth.checked_sub(1) else {
                            continue;
                        };
                        self.lower(&field.ty, next)
                    }
                    FieldSource::Property { .. } | FieldSource::Builtin => {
                        self.lower(&field.ty, depth)
                    }
                };
                members
                    .borrow_mut()
                    .insert(Rc::from(field_name.as_str()), lowered);
            }
        }
        self.memo.insert(key, object.clone());
        object
    }
}

// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// The checker for the norn-expr/4 grammar / norn-zen/6 profile
// ---------------------------------------------------------------------------

/// A checker type: the admitted value shapes.
#[derive(Debug, Clone, PartialEq, Eq)]
enum T {
    Null,
    Bool,
    Number(Operand),
    /// A string; `Some` when its value is one of a known set (enum values,
    /// literals).
    Str(Option<BTreeSet<String>>),
    /// A validated `YYYY-MM-DD` value, represented as a string at runtime.
    Date,
    /// A validated, canonical UTC timestamp, represented as a string at runtime.
    Timestamp,
    /// A checked ZEN date value returned by a temporal constructor or arithmetic.
    ParsedDate,
    /// A checked ZEN timestamp value returned by a temporal constructor or arithmetic.
    ParsedTimestamp,
    List(Box<T>),
    Record(Rec),
    Optional(Box<T>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Rec {
    /// An environment record, with the link depth left from here.
    Env { name: String, depth: u32 },
    /// A record literal.
    Literal(BTreeMap<String, T>),
}

impl fmt::Display for T {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Null => f.write_str("null"),
            Self::Bool => f.write_str("bool"),
            Self::Number(_) => f.write_str("number"),
            Self::Str(None) => f.write_str("string"),
            Self::Str(Some(values)) => write!(f, "string{values:?}"),
            Self::Date => f.write_str("date"),
            Self::Timestamp => f.write_str("timestamp"),
            Self::ParsedDate => f.write_str("parsed date"),
            Self::ParsedTimestamp => f.write_str("parsed timestamp"),
            Self::List(item) => write!(f, "list<{item}>"),
            Self::Record(Rec::Env { name, .. }) => f.write_str(name),
            Self::Record(Rec::Literal(fields)) => {
                let names: Vec<&str> = fields.keys().map(String::as_str).collect();
                write!(f, "{{{}}}", names.join(", "))
            }
            Self::Optional(item) => write!(f, "{item}?"),
        }
    }
}

fn optional(ty: T) -> T {
    match ty {
        T::Optional(_) | T::Null => ty,
        other @ (T::Bool
        | T::Number(_)
        | T::Str(_)
        | T::Date
        | T::Timestamp
        | T::ParsedDate
        | T::ParsedTimestamp
        | T::List(_)
        | T::Record(_)) => T::Optional(Box::new(other)),
    }
}

/// The one type two values of types `a` and `b` share, if any: equal types,
/// strings (known sets join), lists and literal records of joinable shapes
/// with exactly the same fields, and `null` joining into an optional.
fn join(a: &T, b: &T) -> Option<T> {
    Some(match (a, b) {
        (T::Null, T::Null) => T::Null,
        (T::Null, other) | (other, T::Null) => optional(other.clone()),
        (T::Optional(a), T::Optional(b)) => optional(join(a, b)?),
        (T::Optional(a), b) | (b, T::Optional(a)) => optional(join(a, b)?),
        (T::Str(a), T::Str(b)) => T::Str(match (a, b) {
            (Some(a), Some(b)) => Some(a.union(b).cloned().collect()),
            _ => None,
        }),
        (T::Number(a), T::Number(b)) => T::Number(units::join(a, b)?),
        (T::List(a), T::List(b)) => T::List(Box::new(join(a, b)?)),
        (T::Record(Rec::Literal(a)), T::Record(Rec::Literal(b))) => {
            if a.len() != b.len() {
                return None;
            }
            let mut fields = BTreeMap::new();
            for (name, left) in a {
                fields.insert(name.clone(), join(left, b.get(name)?)?);
            }
            T::Record(Rec::Literal(fields))
        }
        (a, b) if a == b => a.clone(),
        _ => return None,
    })
}

/// Lowers a declared type at link depth `depth`. `None`: opaque (`json`).
fn lower(ty: &Type, depth: u32, env: &ZenTypeEnv, owner: Option<&str>, exact: bool) -> Option<T> {
    Some(match ty {
        Type::Bool => T::Bool,
        Type::Text { .. } | Type::Duration => T::Str(None),
        Type::Timestamp { .. } => T::Timestamp,
        Type::Date => T::Date,
        Type::Integer => T::Number(Operand::Dimensionless),
        Type::Number => T::Number(if exact {
            Operand::Legacy
        } else {
            Operand::Dimensionless
        }),
        Type::Decimal {
            unit,
            currency_source,
            per_unit,
            ..
        } => T::Number(if exact {
            units::declared(
                env,
                owner,
                unit.as_ref(),
                currency_source.as_ref(),
                per_unit.as_ref(),
            )
        } else {
            Operand::Dimensionless
        }),
        Type::DecimalBinding { .. } => T::Number(if exact {
            Operand::Unresolved
        } else {
            Operand::Dimensionless
        }),
        Type::Enum { values } => T::Str(Some(values.iter().cloned().collect())),
        Type::Json => return None,
        Type::Record(name) => T::Record(Rec::Env {
            name: name.clone(),
            depth,
        }),
        Type::List(item) => T::List(Box::new(lower(item, depth, env, owner, exact)?)),
        Type::Optional(item) => optional(lower(item, depth, env, owner, exact)?),
    })
}

struct Scope {
    alias: Option<String>,
    element: T,
}

struct Checker<'a> {
    env: &'a ZenTypeEnv,
    metadata: &'a AstMetadata,
    /// The declared type of `$` in a unary test; `None` in a standard
    /// expression, where `$` is not bound.
    subject: Option<&'a Type>,
    scopes: Vec<Scope>,
    diagnostics: Vec<Diagnostic>,
    notes: Vec<Diagnostic>,
    /// The proved type of every inferred node, by address, for the cost
    /// estimate.
    types: HashMap<usize, T>,
    exact: bool,
    exact_math: bool,
    /// Literal read paths a guarding condition proved non-null: a law's
    /// response is evaluated only when its condition holds. Expressions in
    /// the profile itself never narrow.
    non_null: Vec<Vec<String>>,
}

/// The literal read paths `node` proves non-null when it evaluates to
/// `holds`: `p != null` (`p == null` when false), a conjunction when true, a
/// disjunction when false, and negation. Nothing else is a guard.
fn null_guards(node: &Node<'_>, holds: bool) -> Vec<Vec<String>> {
    match node {
        Node::Parenthesized(inner) => null_guards(inner, holds),
        Node::Unary {
            node: operand,
            operator: Operator::Logical(LogicalOperator::Not),
        } => null_guards(operand, !holds),
        Node::Binary {
            left,
            operator:
                Operator::Comparison(
                    comparison @ (ComparisonOperator::Equal | ComparisonOperator::NotEqual),
                ),
            right,
        } => {
            let proves = holds == matches!(comparison, ComparisonOperator::NotEqual);
            let path = match (unparenthesized(left), unparenthesized(right)) {
                (Node::Null, other) | (other, Node::Null) => crate::reads::literal_path(other),
                _ => None,
            };
            path.filter(|_| proves).into_iter().collect()
        }
        Node::Binary {
            left,
            operator: Operator::Logical(logical @ (LogicalOperator::And | LogicalOperator::Or)),
            right,
        } if holds == matches!(logical, LogicalOperator::And) => {
            let mut guards = null_guards(left, holds);
            guards.extend(null_guards(right, holds));
            guards
        }
        Node::Null
        | Node::Bool(_)
        | Node::Number(_)
        | Node::String(_)
        | Node::TemplateString(_)
        | Node::Pointer
        | Node::Array(_)
        | Node::Object(_)
        | Node::Assignments { .. }
        | Node::Identifier(_)
        | Node::Closure { .. }
        | Node::Root
        | Node::Member { .. }
        | Node::Slice { .. }
        | Node::Interval { .. }
        | Node::Conditional { .. }
        | Node::Unary { .. }
        | Node::Binary { .. }
        | Node::FunctionCall { .. }
        | Node::MethodCall { .. }
        | Node::Error { .. } => Vec::new(),
    }
}

/// The literal read paths a condition proves non-null when it holds. A law's
/// response is checked under its condition (`when: self.link != null`).
#[must_use]
pub fn condition_null_guards(condition: &Expression) -> Vec<Vec<String>> {
    IntelliSense::new()
        .with_ast(condition.source(), false, |ast, _| null_guards(ast, true))
        .unwrap_or_default()
}

/// The node inside any number of parentheses.
fn unparenthesized<'n>(mut node: &'n Node<'n>) -> &'n Node<'n> {
    while let Node::Parenthesized(inner) = node {
        node = inner;
    }
    node
}

/// Whether a numeric literal (optionally parenthesized and signed) has a
/// fractional component. Dynamic numbers are checked by ZEN at evaluation.
fn fractional_numeric_literal(node: &Node<'_>) -> bool {
    match unparenthesized(node) {
        Node::Number(number) => !number.fract().is_zero(),
        Node::Unary {
            node: operand,
            operator: Operator::Arithmetic(ArithmeticOperator::Add | ArithmeticOperator::Subtract),
        } => fractional_numeric_literal(operand),
        Node::Null
        | Node::Bool(_)
        | Node::String(_)
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
        | Node::Error { .. } => false,
    }
}

fn address(node: &Node<'_>) -> usize {
    std::ptr::from_ref(node).addr()
}

impl<'a> Checker<'a> {
    fn new(env: &'a ZenTypeEnv, metadata: &'a AstMetadata, subject: Option<&'a Type>) -> Self {
        Self {
            env,
            metadata,
            subject,
            scopes: Vec::new(),
            diagnostics: Vec::new(),
            notes: Vec::new(),
            types: HashMap::new(),
            exact: false,
            exact_math: false,
            non_null: Vec::new(),
        }
    }

    /// Whether the guarding condition proved `node`'s literal read path
    /// non-null, and no closure alias shadows its root.
    fn guarded(&self, node: &Node<'_>) -> bool {
        if self.non_null.is_empty() {
            return false;
        }
        let Some(path) = crate::reads::literal_path(node) else {
            return false;
        };
        let shadowed = self
            .scopes
            .iter()
            .any(|scope| scope.alias.as_deref() == path.first().map(String::as_str));
        !shadowed && self.non_null.contains(&path)
    }

    /// The record a member read goes through: a guarded path reads as
    /// non-null (`self.link.amount` under `self.link != null`); the path's
    /// own type is unchanged, so `self.link ?? x` still type-checks.
    fn member_base(&mut self, target: &Node<'_>) -> Option<T> {
        if !self.guarded(target) {
            return self.value(target, None);
        }
        let ty = self.infer(target, None)?;
        if let T::Optional(inner) = &ty {
            return Some((**inner).clone());
        }
        Some(ty)
    }

    fn span(&self, node: &Node<'_>) -> (u32, u32) {
        self.metadata
            .get(&address(node))
            .map_or((0, 0), |meta| meta.span)
    }

    fn report<R>(&mut self, node: &Node<'_>, code: &'static str, message: String) -> Option<R> {
        let span = self.span(node);
        self.diagnostics.push(Diagnostic {
            code,
            message,
            span,
        });
        None
    }

    fn unsupported<R>(&mut self, node: &Node<'_>, what: &str) -> Option<R> {
        self.report(
            node,
            codes::UNSUPPORTED_CONSTRUCT,
            format!("{what} is not in {CHECKER_PROFILE}"),
        )
    }

    /// The checker type a site's expected type implies (`None` for `json`).
    fn expectation(&self, expected: &Type) -> Option<T> {
        lower(
            expected,
            self.env.options().max_link_depth,
            self.env,
            None,
            self.exact_math,
        )
    }

    /// A value that must not be null.
    fn value(&mut self, node: &Node<'_>, expected: Option<&T>) -> Option<T> {
        match self.infer(node, expected)? {
            ty @ (T::Optional(_) | T::Null) => self.report(
                node,
                codes::NULLABLE,
                format!(
                    "this `{ty}` value may be null; handle it with `??` or compare it to `null`"
                ),
            ),
            ty @ (T::Bool
            | T::Number(_)
            | T::Str(_)
            | T::Date
            | T::Timestamp
            | T::ParsedDate
            | T::ParsedTimestamp
            | T::List(_)
            | T::Record(_)) => Some(ty),
        }
    }

    fn numeric(&mut self, node: &Node<'_>) -> Option<Operand> {
        match self.value(node, None)? {
            T::Number(operand) => Some(operand),
            ty @ (T::Null
            | T::Bool
            | T::Str(_)
            | T::Date
            | T::Timestamp
            | T::ParsedDate
            | T::ParsedTimestamp
            | T::List(_)
            | T::Record(_)
            | T::Optional(_)) => self.report(
                node,
                codes::TYPE_ERROR,
                format!("expected a number, found `{ty}`"),
            ),
        }
    }

    fn numeric_operation(
        &mut self,
        node: &Node<'_>,
        op: Operation,
        a: &Operand,
        b: &Operand,
    ) -> Option<Operand> {
        let Some((result, inferred)) = units::admit(op, a, b) else {
            return self.report(
                node,
                codes::UNIT,
                format!("VAL-UNIT / VAL-RATE: {op:?} refuses {a:?} and {b:?}"),
            );
        };
        if let Some(unit) = inferred {
            self.notes.push(Diagnostic {
                code: codes::LITERAL_UNIT,
                message: format!(
                    "numeric literal inherits {:?} unit {}",
                    unit.system, unit.code
                ),
                span: self.span(node),
            });
        }
        Some(result)
    }

    fn numeric_binary(&mut self, left: &Node<'_>, right: &Node<'_>, op: Operation) -> Option<T> {
        let a = self.numeric(left)?;
        let b = self.numeric(right)?;
        self.numeric_operation(left, op, &a, &b).map(T::Number)
    }

    fn check_destination(&mut self, node: &Node<'_>, found: &T, expected: &T) {
        self.check_destination_at(node, found, expected, 0);
    }

    fn check_destination_at(&mut self, node: &Node<'_>, found: &T, expected: &T, depth: u32) {
        if depth > 128 {
            self.report::<()>(
                node,
                codes::BOUND_DEPTH,
                "destination type nesting exceeds profile".into(),
            );
            return;
        }
        match (found, expected) {
            (T::Number(a), T::Number(b)) if !units::destination(a, b) => {
                self.report::<()>(
                    node,
                    codes::UNIT,
                    format!("numeric destination {b:?} refuses {a:?}"),
                );
            }
            (T::List(a), T::List(b)) | (T::Optional(a), T::Optional(b)) => {
                self.check_destination_at(node, a, b, depth.saturating_add(1));
            }
            (a, T::Optional(b)) if !matches!(a, T::Null) => {
                self.check_destination_at(node, a, b, depth.saturating_add(1));
            }
            (T::Record(Rec::Env { name, depth: links }), expected) => {
                if let Some(record) = self.env.record(name) {
                    let fields = record
                        .fields
                        .iter()
                        .filter_map(|(field_name, field)| {
                            let next = if matches!(
                                field.source,
                                FieldSource::Link { .. } | FieldSource::Inverse { .. }
                            ) {
                                links.checked_sub(1)?
                            } else {
                                *links
                            };
                            lower(&field.ty, next, self.env, Some(name), self.exact_math)
                                .map(|ty| (field_name.clone(), ty))
                        })
                        .collect();
                    self.check_destination_at(
                        node,
                        &T::Record(Rec::Literal(fields)),
                        expected,
                        depth.saturating_add(1),
                    );
                }
            }
            (T::Record(Rec::Literal(fields)), T::Record(Rec::Env { name, depth: links })) => {
                if let Some(record) = self.env.record(name) {
                    for (field_name, found) in fields {
                        if let Some(expected) = record.fields.get(field_name).and_then(|field| {
                            lower(&field.ty, *links, self.env, Some(name), self.exact_math)
                        }) {
                            self.check_destination_at(
                                node,
                                found,
                                &expected,
                                depth.saturating_add(1),
                            );
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn number(&mut self, node: &Node<'_>) -> Option<()> {
        match self.value(node, Some(&T::Number(Operand::Dimensionless)))? {
            T::Number(_) => Some(()),
            ty @ (T::Null
            | T::Bool
            | T::Str(_)
            | T::Date
            | T::Timestamp
            | T::ParsedDate
            | T::ParsedTimestamp
            | T::List(_)
            | T::Record(_)
            | T::Optional(_)) => self.report(
                node,
                codes::TYPE_ERROR,
                format!("expected a number, found `{ty}`"),
            ),
        }
    }

    fn string(&mut self, node: &Node<'_>) -> Option<()> {
        match self.value(node, None)? {
            T::Str(_) => Some(()),
            ty @ (T::Null
            | T::Bool
            | T::Number(_)
            | T::Date
            | T::Timestamp
            | T::ParsedDate
            | T::ParsedTimestamp
            | T::List(_)
            | T::Record(_)
            | T::Optional(_)) => self.report(
                node,
                codes::TYPE_ERROR,
                format!("expected a string, found `{ty}`"),
            ),
        }
    }

    fn bounded_integer_literal(&mut self, node: &Node<'_>, max: usize, name: &str) -> Option<()> {
        match node {
            Node::Number(value)
                if value
                    .to_string()
                    .parse::<usize>()
                    .is_ok_and(|value| value <= max) =>
            {
                Some(())
            }
            other @ (Node::Null
            | Node::Bool(_)
            | Node::Number(_)
            | Node::String(_)
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
            | Node::Error { .. }) => self.report(
                other,
                codes::UNSUPPORTED_CONSTRUCT,
                format!("`{name}` must be an integer literal from 0 to {max}"),
            ),
        }
    }

    /// Checks an input to a temporal constructor. Entity temporal properties
    /// are strings at runtime but retain their declared kind in this checker,
    /// so only the matching wire kind or an ordinary string is accepted.
    fn temporal_text(&mut self, node: &Node<'_>, date: bool) -> Option<()> {
        let actual = self.value(node, None)?;
        if let Node::String(text) = unparenthesized(node) {
            let valid = if date {
                zen_expression::temporal::Temporal::date(text).is_ok()
            } else {
                zen_expression::temporal::Temporal::timestamp(text).is_ok()
            };
            if !valid {
                return self.report(
                    node,
                    codes::TYPE_ERROR,
                    if date {
                        "`date` needs a valid YYYY-MM-DD literal in years 0001..9999"
                    } else {
                        "`timestamp` needs a valid offset-bearing timestamp literal"
                    }
                    .to_owned(),
                );
            }
        }
        match actual {
            T::Str(_) => {}
            T::Date if date => {}
            T::Timestamp if !date => {}
            actual @ (T::Null
            | T::Bool
            | T::Number(_)
            | T::Date
            | T::Timestamp
            | T::ParsedDate
            | T::ParsedTimestamp
            | T::List(_)
            | T::Record(_)
            | T::Optional(_)) => {
                return self.report(
                    node,
                    codes::TYPE_ERROR,
                    if date {
                        format!("`date` needs text or a date property, found `{actual}`")
                    } else {
                        format!("`timestamp` needs text or a timestamp property, found `{actual}`")
                    },
                );
            }
        }
        Some(())
    }

    /// Checks an amount accepted by temporal arithmetic. ZEN rejects a
    /// fractional dynamic value at runtime; fractional literals are known at
    /// admission and are refused here.
    fn temporal_amount(&mut self, node: &Node<'_>) -> Option<()> {
        self.number(node)?;
        if fractional_numeric_literal(node) {
            return self.report(
                node,
                codes::TYPE_ERROR,
                "temporal arithmetic needs an integer-valued amount".to_owned(),
            );
        }
        Some(())
    }

    /// Requires a value produced by a temporal constructor, not a raw wire
    /// string. ZEN's temporal arithmetic and ordering operate on Dynamic values.
    fn parsed_temporal(&mut self, node: &Node<'_>, date: bool) -> Option<()> {
        let actual = self.value(node, None)?;
        match (&actual, date) {
            (T::ParsedDate, true) | (T::ParsedTimestamp, false) => Some(()),
            _ => self.report(
                node,
                codes::TYPE_ERROR,
                if date {
                    format!("expected a parsed date, found `{actual}`")
                } else {
                    format!("expected a parsed timestamp, found `{actual}`")
                },
            ),
        }
    }

    fn boolean(&mut self, node: &Node<'_>) -> Option<()> {
        match self.value(node, Some(&T::Bool))? {
            T::Bool => Some(()),
            ty @ (T::Null
            | T::Number(_)
            | T::Str(_)
            | T::Date
            | T::Timestamp
            | T::ParsedDate
            | T::ParsedTimestamp
            | T::List(_)
            | T::Record(_)
            | T::Optional(_)) => self.report(
                node,
                codes::TYPE_ERROR,
                format!("expected a bool, found `{ty}`"),
            ),
        }
    }

    fn infer(&mut self, node: &Node<'_>, expected: Option<&T>) -> Option<T> {
        let ty = self.infer_node(node, expected)?;
        self.types.insert(address(node), ty.clone());
        Some(ty)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one arm per admitted construct keeps the allowlist in one place"
    )]
    fn infer_node(&mut self, node: &Node<'_>, expected: Option<&T>) -> Option<T> {
        match node {
            Node::Null => Some(T::Null),
            Node::Bool(_) => Some(T::Bool),
            Node::Number(number) => match number.to_string().parse::<Number>() {
                Ok(_) => Some(T::Number(Operand::Literal)),
                Err(err) => self.report(node, codes::TYPE_ERROR, err.to_string()),
            },
            Node::String(text) => Some(T::Str(Some(BTreeSet::from([(*text).to_owned()])))),
            Node::Parenthesized(inner) => self.infer(inner, expected),
            Node::Array(items) => self.list(node, items, expected),
            Node::Object(entries) => self.record(entries, expected),
            Node::Identifier(name) => self.identifier(node, name),
            Node::Pointer => match self.scopes.last() {
                Some(scope) => Some(scope.element.clone()),
                None => self.unsupported(node, "`#` outside a closure"),
            },
            Node::Member {
                node: target,
                property,
            } => self.member(node, target, property),
            Node::Unary {
                node: operand,
                operator,
            } => match operator {
                Operator::Arithmetic(ArithmeticOperator::Add | ArithmeticOperator::Subtract) => {
                    self.numeric(operand).map(T::Number)
                }
                Operator::Logical(LogicalOperator::Not) => {
                    self.boolean(operand)?;
                    Some(T::Bool)
                }
                other @ (Operator::Arithmetic(_)
                | Operator::Logical(_)
                | Operator::Comparison(_)
                | Operator::Range
                | Operator::Comma
                | Operator::Slice
                | Operator::Dot
                | Operator::QuestionMark
                | Operator::Assign
                | Operator::Semi) => self.unsupported(node, &format!("unary `{other}`")),
            },
            Node::Binary {
                left,
                operator,
                right,
            } => self.binary(node, left, *operator, right, expected),
            Node::Conditional {
                condition,
                on_true,
                on_false,
            } => {
                let condition = self.boolean(condition);
                let empty = |n: &Node<'_>| matches!(n, Node::Array(items) if items.is_empty());
                // An empty-list branch takes the other branch's type.
                let (yes, no) = if expected.is_none() && empty(on_true) {
                    let no = self.infer(on_false, None);
                    (self.infer(on_true, no.as_ref()), no)
                } else {
                    let yes = self.infer(on_true, expected);
                    let no = self.infer(on_false, expected.or(yes.as_ref()));
                    (yes, no)
                };
                let (yes, no) = (yes?, no?);
                condition?;
                match join(&yes, &no) {
                    Some(ty) => Some(ty),
                    None => self.report(
                        node,
                        codes::TYPE_ERROR,
                        format!("branches have different types: `{yes}` and `{no}`"),
                    ),
                }
            }
            Node::FunctionCall { kind, arguments } => self.call(node, kind, arguments),
            Node::Error { error, .. } => self.report(node, codes::SYNTAX, error.to_string()),
            Node::TemplateString(_) => self.unsupported(node, "a string template"),
            Node::Slice { .. } => self.unsupported(node, "a slice"),
            Node::Interval { .. } => self.unsupported(node, "an interval"),
            Node::MethodCall { .. } => self.unsupported(node, "a date method"),
            Node::Assignments { .. } => self.unsupported(node, "an assignment"),
            Node::Root => self.unsupported(node, "`$root`"),
            Node::Closure { .. } => self.unsupported(node, "a closure outside an admitted builtin"),
        }
    }

    /// A unary test as zen parses it: parts joined by `and`/`or` (a comma
    /// is `or`), each either a comparison with `$` on the left (`> 5`,
    /// `'gold'`, `[1..5]`) or a boolean-valued part zen wraps in `bool(...)`.
    fn unary_test(&mut self, node: &Node<'_>) {
        match node {
            Node::Binary {
                left,
                operator: Operator::Logical(LogicalOperator::And | LogicalOperator::Or),
                right,
            } => {
                self.unary_test(left);
                self.unary_test(right);
            }
            // zen's `AsBoolean` wrapper: `bool(x)` on a checked `bool` is `x`.
            // A `bool(...)` the author wrote is compared with `$` instead, so
            // it never reaches this arm.
            Node::FunctionCall {
                kind: FunctionKind::Internal(InternalFunction::Bool),
                arguments: [part],
            } => self.unary_part(node, part),
            part @ (Node::Null
            | Node::Bool(_)
            | Node::Number(_)
            | Node::String(_)
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
            | Node::Error { .. }) => self.unary_part(node, part),
        }
    }

    /// One part of a unary test: a non-null boolean. The implicit `$` zen
    /// inserts has no span, so its findings take the part's span.
    fn unary_part(&mut self, whole: &Node<'_>, part: &Node<'_>) {
        let before = self.diagnostics.len();
        self.boolean(part);
        let span = self.span(whole);
        for diagnostic in self.diagnostics.iter_mut().skip(before) {
            if diagnostic.span == (0, 0) {
                diagnostic.span = span;
            }
        }
    }

    fn list(&mut self, node: &Node<'_>, items: &[&Node<'_>], expected: Option<&T>) -> Option<T> {
        let element_expected = match expected {
            Some(T::List(item)) => Some(item.as_ref().clone()),
            _ => None,
        };
        if items.is_empty() {
            return match element_expected {
                Some(item) => Some(T::List(Box::new(item))),
                None => self.report(
                    node,
                    codes::UNTYPED_EMPTY_LIST,
                    "an empty list needs an expected list type".to_owned(),
                ),
            };
        }
        // A string-shaped output schema is not an expression context that
        // erases semantic date/timestamp kinds. Infer non-empty string lists
        // from their elements, then let the final structural assignability
        // check project typed temporal values to JSON strings.
        let mut element: Option<T> = None;
        let mut failed = false;
        for item in items {
            let Some(ty) = self.value(item, element.as_ref()) else {
                failed = true;
                continue;
            };
            element = match element {
                None => Some(ty),
                Some(current) => {
                    let joined = join(&current, &ty);
                    if joined.is_none() {
                        self.report::<()>(
                            item,
                            codes::TYPE_ERROR,
                            format!("list elements have different types: `{current}` and `{ty}`"),
                        );
                        failed = true;
                    }
                    joined.or(Some(current))
                }
            };
        }
        if failed {
            return None;
        }
        Some(T::List(Box::new(element?)))
    }

    fn record(&mut self, entries: &[(&Node<'_>, &Node<'_>)], expected: Option<&T>) -> Option<T> {
        let mut fields = BTreeMap::new();
        let mut failed = false;
        for (key, value) in entries {
            let Node::String(name) = key else {
                failed = true;
                self.unsupported::<()>(key, "a computed record key");
                continue;
            };
            let field_expected = match expected {
                Some(T::Record(Rec::Literal(fields))) => fields.get(*name).cloned(),
                Some(T::Record(Rec::Env {
                    name: record,
                    depth,
                })) => self
                    .env
                    .record(record)
                    .and_then(|r| r.fields.get(*name))
                    .and_then(|field| {
                        lower(&field.ty, *depth, self.env, Some(record), self.exact_math)
                    }),
                _ => None,
            };
            match self.infer(value, field_expected.as_ref()) {
                Some(ty) => {
                    if fields.insert((*name).to_owned(), ty).is_some() {
                        failed = true;
                        self.report::<()>(
                            key,
                            codes::TYPE_ERROR,
                            format!("duplicate key `{name}`"),
                        );
                    }
                }
                None => failed = true,
            }
        }
        if failed {
            return None;
        }
        Some(T::Record(Rec::Literal(fields)))
    }

    fn identifier(&mut self, node: &Node<'_>, name: &str) -> Option<T> {
        if let Some(scope) = self
            .scopes
            .iter()
            .rev()
            .find(|scope| scope.alias.as_deref() == Some(name))
        {
            return Some(scope.element.clone());
        }
        let depth = self.env.options().max_link_depth;
        let declared = match self.subject {
            Some(subject) if name == SUBJECT_VARIABLE => Some(subject),
            _ => self.env.variables().get(name),
        };
        self.exact |= declared.is_some_and(exact_type);
        match declared.map(|ty| lower(ty, depth, self.env, None, self.exact_math)) {
            Some(Some(ty)) => Some(ty),
            Some(None) => self.report(node, codes::OPAQUE_READ, format!("`{name}` is opaque")),
            None => self.report(
                node,
                codes::UNKNOWN_VARIABLE,
                format!("`{name}` is not a variable of the environment"),
            ),
        }
    }

    fn member(&mut self, node: &Node<'_>, target: &Node<'_>, property: &Node<'_>) -> Option<T> {
        let base = self.member_base(target)?;
        // Every read on a list is an index read, a string key included
        // (`[1]['0']`: zen reads `null`).
        if let T::List(item) = base {
            return self.index(node, target, property, *item);
        }
        let Node::String(key) = property else {
            return match base {
                T::Record(_) => self.report(
                    node,
                    codes::DYNAMIC_READ,
                    "a record member must be read by a literal name, not a computed key".to_owned(),
                ),
                T::Null
                | T::Bool
                | T::Number(_)
                | T::Str(_)
                | T::Date
                | T::Timestamp
                | T::ParsedDate
                | T::ParsedTimestamp
                | T::List(_)
                | T::Optional(_) => self.unsupported(node, "indexing"),
            };
        };
        match base {
            T::Record(Rec::Literal(fields)) => match fields.get(*key) {
                Some(ty) => Some(ty.clone()),
                None => self.report(
                    node,
                    codes::UNKNOWN_MEMBER,
                    format!("`{key}` is not a member of the record"),
                ),
            },
            T::Record(Rec::Env { name, depth }) => {
                let Some(field) = self.env.record(&name).and_then(|r| r.fields.get(*key)) else {
                    return self.report(
                        node,
                        codes::UNKNOWN_MEMBER,
                        format!("`{key}` is not a member of `{name}`"),
                    );
                };
                let depth = match field.source {
                    FieldSource::Link { .. } | FieldSource::Inverse { .. } => {
                        let Some(next) = depth.checked_sub(1) else {
                            return self.report(
                                node,
                                codes::UNKNOWN_MEMBER,
                                format!("`{key}` is a link beyond the link depth"),
                            );
                        };
                        next
                    }
                    FieldSource::Property { .. } | FieldSource::Builtin => depth,
                };
                self.exact |= exact_type(&field.ty);
                match lower(&field.ty, depth, self.env, Some(&name), self.exact_math) {
                    Some(ty) => Some(ty),
                    None => self.report(
                        node,
                        codes::OPAQUE_READ,
                        format!("`{key}` is an opaque json-ext property and cannot be read"),
                    ),
                }
            }
            other @ (T::Null
            | T::Bool
            | T::Number(_)
            | T::Str(_)
            | T::Date
            | T::Timestamp
            | T::ParsedDate
            | T::ParsedTimestamp
            | T::List(_)
            | T::Optional(_)) => self.unsupported(node, &format!("a member read on `{other}`")),
        }
    }

    /// `[a, b, c][i]`: an integer literal index into a list literal, in
    /// bounds by construction. Any other index may miss and read `null`
    /// (zen returns `null` out of bounds and fails on a fractional or
    /// negative index), so it stays outside the profile.
    fn index(
        &mut self,
        node: &Node<'_>,
        target: &Node<'_>,
        property: &Node<'_>,
        item: T,
    ) -> Option<T> {
        let Node::Array(items) = unparenthesized(target) else {
            return self.report(
                node,
                codes::INDEX,
                "only a list literal can be indexed, by an in-bounds integer literal".to_owned(),
            );
        };
        let in_bounds = match property {
            Node::Number(index) => index
                .to_string()
                .parse::<usize>()
                .is_ok_and(|index| index < items.len()),
            Node::Null
            | Node::Bool(_)
            | Node::String(_)
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
            | Node::Error { .. } => false,
        };
        if in_bounds {
            Some(item)
        } else {
            self.report(
                property,
                codes::INDEX,
                format!(
                    "the index must be an integer literal from 0 to {}",
                    items.len().saturating_sub(1)
                ),
            )
        }
    }

    /// `x in list`, `x not in list`, and `x in [a..b]` (a number range).
    fn membership(&mut self, node: &Node<'_>, left: &Node<'_>, right: &Node<'_>) -> Option<T> {
        let needle = self.value(left, None);
        if let Node::Interval {
            left: low,
            right: high,
            ..
        } = unparenthesized(right)
        {
            let low = self.number(low);
            let high = self.number(high);
            let needle = needle?;
            low.and(high)?;
            return match needle {
                T::Number(_) => Some(T::Bool),
                ty @ (T::Null
                | T::Bool
                | T::Str(_)
                | T::Date
                | T::Timestamp
                | T::ParsedDate
                | T::ParsedTimestamp
                | T::List(_)
                | T::Record(_)
                | T::Optional(_)) => self.report(
                    left,
                    codes::TYPE_ERROR,
                    format!("only a number can be tested against a range, found `{ty}`"),
                ),
            };
        }
        // Only an empty literal takes the needle's type; otherwise the list
        // keeps its own element type, so disjoint literal sets are detected.
        let expected = match (&needle, unparenthesized(right)) {
            (Some(ty), Node::Array([])) => Some(T::List(Box::new(ty.clone()))),
            _ => None,
        };
        let haystack = self.value(right, expected.as_ref());
        let (needle, haystack) = (needle?, haystack?);
        let T::List(element) = &haystack else {
            return self.report(
                right,
                codes::TYPE_ERROR,
                format!("`in` needs a list or a number range, found `{haystack}`"),
            );
        };
        match (&needle, element.as_ref()) {
            (T::Str(Some(a)), T::Str(Some(b))) if a.is_disjoint(b) => self.report(
                node,
                codes::CONSTANT_COMPARISON,
                format!("`{needle}` is never an element of `{haystack}`"),
            ),
            _ if matches!(unparenthesized(right), Node::Array([])) => self.report(
                node,
                codes::CONSTANT_COMPARISON,
                "nothing is an element of the empty list".to_owned(),
            ),
            (T::Number(a), T::Number(b)) => self
                .numeric_operation(node, Operation::Compare, a, b)
                .map(|_| T::Bool),
            (T::Bool, T::Bool) | (T::Str(_), T::Str(_)) => Some(T::Bool),
            _ => self.report(
                node,
                codes::TYPE_ERROR,
                format!("cannot test `{needle}` for membership in `{haystack}`"),
            ),
        }
    }

    /// `a + b`: numbers add; strings concatenate. Both sides one kind.
    fn plus(&mut self, left: &Node<'_>, right: &Node<'_>) -> Option<T> {
        match self.value(left, None) {
            Some(T::Str(_)) => self.string(right).map(|()| T::Str(None)),
            Some(T::Number(a)) => {
                let b = self.numeric(right)?;
                self.numeric_operation(left, Operation::Add, &a, &b)
                    .map(T::Number)
            }
            Some(ty) => {
                self.value(right, None);
                self.report(
                    left,
                    codes::TYPE_ERROR,
                    format!("`+` needs two numbers or two strings, found `{ty}`"),
                )
            }
            None => {
                self.value(right, None);
                None
            }
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one arm per admitted operator keeps the allowlist in one place"
    )]
    fn binary(
        &mut self,
        node: &Node<'_>,
        left: &Node<'_>,
        operator: Operator,
        right: &Node<'_>,
        expected: Option<&T>,
    ) -> Option<T> {
        match operator {
            Operator::Arithmetic(ArithmeticOperator::Add) => self.plus(left, right),
            Operator::Comparison(ComparisonOperator::In | ComparisonOperator::NotIn) => {
                self.membership(node, left, right)
            }
            Operator::Arithmetic(ArithmeticOperator::Subtract | ArithmeticOperator::Multiply) => {
                self.numeric_binary(
                    left,
                    right,
                    if matches!(operator, Operator::Arithmetic(ArithmeticOperator::Subtract)) {
                        Operation::Subtract
                    } else {
                        Operation::Multiply
                    },
                )
            }
            Operator::Arithmetic(ArithmeticOperator::Divide) => {
                let result = self.numeric_binary(left, right, Operation::Divide)?;
                let bounded_literal =
                    matches!(right, Node::Number(divisor) if divisor.abs() >= 1.into());
                Some(if bounded_literal || self.exact_math {
                    result
                } else {
                    optional(result)
                })
            }
            Operator::Logical(LogicalOperator::And | LogicalOperator::Or) => {
                let l = self.boolean(left);
                let r = self.boolean(right);
                l.and(r).map(|()| T::Bool)
            }
            Operator::Logical(LogicalOperator::NullishCoalescing) => {
                let inner =
                    match self.infer(left, expected.map(|e| optional(e.clone())).as_ref())? {
                        T::Optional(inner) => *inner,
                        ty @ (T::Null
                        | T::Bool
                        | T::Number(_)
                        | T::Str(_)
                        | T::Date
                        | T::Timestamp
                        | T::ParsedDate
                        | T::ParsedTimestamp
                        | T::List(_)
                        | T::Record(_)) => {
                            return self.report(
                                left,
                                codes::TYPE_ERROR,
                                format!("the left side of `??` is never null (`{ty}`)"),
                            );
                        }
                    };
                let fallback = self.value(right, Some(&inner))?;
                match join(&inner, &fallback) {
                    Some(ty) if !matches!(&ty, T::Null | T::Optional(_)) => Some(ty),
                    _ => self.report(
                        node,
                        codes::TYPE_ERROR,
                        format!("the fallback `{fallback}` does not have the type `{inner}`"),
                    ),
                }
            }
            Operator::Comparison(ComparisonOperator::Equal | ComparisonOperator::NotEqual) => {
                if matches!(right, Node::Null) || matches!(left, Node::Null) {
                    let other = if matches!(right, Node::Null) {
                        left
                    } else {
                        right
                    };
                    return match self.infer(other, None)? {
                        T::Optional(_) => Some(T::Bool),
                        ty @ (T::Null
                        | T::Bool
                        | T::Number(_)
                        | T::Str(_)
                        | T::Date
                        | T::Timestamp
                        | T::ParsedDate
                        | T::ParsedTimestamp
                        | T::List(_)
                        | T::Record(_)) => self.report(
                            node,
                            codes::CONSTANT_COMPARISON,
                            format!("a `{ty}` value is never null"),
                        ),
                    };
                }
                let l = self.value(left, None);
                let r = self.value(right, None);
                let (l, r) = (l?, r?);
                match (&l, &r) {
                    (T::Str(Some(a)), T::Str(Some(b))) if a.is_disjoint(b) => self.report(
                        node,
                        codes::CONSTANT_COMPARISON,
                        format!("`{l}` and `{r}` never share a value"),
                    ),
                    (T::Number(a), T::Number(b)) => self
                        .numeric_operation(node, Operation::Compare, a, b)
                        .map(|_| T::Bool),
                    (T::Bool, T::Bool)
                    | (T::Str(_), T::Str(_))
                    | (T::Date, T::Date)
                    | (T::Timestamp, T::Timestamp)
                    | (T::ParsedDate, T::ParsedDate)
                    | (T::ParsedTimestamp, T::ParsedTimestamp) => Some(T::Bool),
                    _ => self.report(
                        node,
                        codes::TYPE_ERROR,
                        format!("cannot compare `{l}` with `{r}`"),
                    ),
                }
            }
            Operator::Comparison(
                ComparisonOperator::LessThan
                | ComparisonOperator::LessThanOrEqual
                | ComparisonOperator::GreaterThan
                | ComparisonOperator::GreaterThanOrEqual,
            ) => {
                let l = self.value(left, None);
                let r = self.value(right, None);
                let (l, r) = (l?, r?);
                match (&l, &r) {
                    (T::Number(a), T::Number(b)) => self
                        .numeric_operation(node, Operation::Compare, a, b)
                        .map(|_| T::Bool),
                    (T::ParsedDate, T::ParsedDate) | (T::ParsedTimestamp, T::ParsedTimestamp) => {
                        Some(T::Bool)
                    }
                    _ => self.report(
                        node,
                        codes::TYPE_ERROR,
                        format!("cannot order `{l}` with `{r}`"),
                    ),
                }
            }
            other @ (Operator::Arithmetic(_)
            | Operator::Logical(_)
            | Operator::Range
            | Operator::Comma
            | Operator::Slice
            | Operator::Dot
            | Operator::QuestionMark
            | Operator::Assign
            | Operator::Semi) => self.unsupported(node, &format!("the operator `{other}`")),
        }
    }

    fn call(&mut self, node: &Node<'_>, kind: &FunctionKind, arguments: &[&Node<'_>]) -> Option<T> {
        match kind {
            FunctionKind::Internal(function) => self.builtin(node, *function, arguments),
            FunctionKind::Closure(function) => self.closure(node, *function, arguments),
            FunctionKind::Deprecated(function) => {
                self.unsupported(node, &format!("the builtin `{function}`"))
            }
        }
    }

    fn arity<'n>(&mut self, node: &Node<'_>, arguments: &[&'n Node<'n>], n: usize) -> Option<()> {
        if arguments.len() == n {
            Some(())
        } else {
            self.report(node, codes::TYPE_ERROR, format!("expected {n} argument(s)"))
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one arm per admitted builtin keeps signatures in one place"
    )]
    fn builtin(
        &mut self,
        node: &Node<'_>,
        function: InternalFunction,
        arguments: &[&Node<'_>],
    ) -> Option<T> {
        use InternalFunction as F;
        let first = arguments.first().copied();
        match function {
            F::CalendarDate => {
                self.arity(node, arguments, 1)?;
                self.temporal_text(first?, true)?;
                Some(T::ParsedDate)
            }
            F::Timestamp => {
                self.arity(node, arguments, 1)?;
                self.temporal_text(first?, false)?;
                Some(T::ParsedTimestamp)
            }
            F::DaysBetween => {
                self.arity(node, arguments, 2)?;
                let start = self.parsed_temporal(first?, true);
                let end = self.parsed_temporal(arguments.get(1)?, true);
                start.and(end).map(|()| T::Number(Operand::Dimensionless))
            }
            F::AddDays => {
                self.arity(node, arguments, 2)?;
                let date = self.parsed_temporal(first?, true);
                let amount = self.temporal_amount(arguments.get(1)?);
                date.and(amount).map(|()| T::ParsedDate)
            }
            F::SecondsBetween => {
                self.arity(node, arguments, 2)?;
                let start = self.parsed_temporal(first?, false);
                let end = self.parsed_temporal(arguments.get(1)?, false);
                start.and(end).map(|()| T::Number(Operand::Dimensionless))
            }
            F::AddSeconds => {
                self.arity(node, arguments, 2)?;
                let timestamp = self.parsed_temporal(first?, false);
                let amount = self.temporal_amount(arguments.get(1)?);
                timestamp.and(amount).map(|()| T::ParsedTimestamp)
            }
            F::Len => {
                self.arity(node, arguments, 1)?;
                match self.value(first?, None)? {
                    T::Str(_) | T::List(_) => Some(T::Number(Operand::Dimensionless)),
                    ty @ (T::Null
                    | T::Bool
                    | T::Number(_)
                    | T::Date
                    | T::Timestamp
                    | T::ParsedDate
                    | T::ParsedTimestamp
                    | T::Record(_)
                    | T::Optional(_)) => {
                        self.report(node, codes::TYPE_ERROR, format!("`len` of `{ty}`"))
                    }
                }
            }
            F::Split => {
                self.arity(node, arguments, 2)?;
                self.string(first?)?;
                match arguments.get(1) {
                    Some(Node::String("")) => Some(T::List(Box::new(T::Str(None)))),
                    Some(other) => self.report(
                        other,
                        codes::UNSUPPORTED_CONSTRUCT,
                        "`split` requires an empty string literal separator".to_owned(),
                    ),
                    None => None,
                }
            }
            F::Tokens => {
                self.arity(node, arguments, 4)?;
                self.string(first?)?;
                match arguments.get(1) {
                    Some(Node::String("alnum")) => {}
                    Some(other) => {
                        return self.report(
                            other,
                            codes::UNSUPPORTED_CONSTRUCT,
                            "`tokens` requires the literal mode 'alnum'".to_owned(),
                        );
                    }
                    None => return None,
                }
                self.bounded_integer_literal(arguments.get(2)?, MAX_TOKEN_COUNT, "maxCount")?;
                self.bounded_integer_literal(arguments.get(3)?, MAX_TOKEN_BYTES, "maxLength")?;
                Some(T::List(Box::new(T::Str(None))))
            }
            F::Take => {
                self.arity(node, arguments, 2)?;
                self.string(first?)?;
                self.bounded_integer_literal(arguments.get(1)?, MAX_TAKE_SCALARS, "maxScalars")?;
                Some(T::Str(None))
            }
            F::Join => {
                self.arity(node, arguments, 2)?;
                let expected = T::List(Box::new(T::Str(None)));
                let list = self.value(first?, Some(&expected))?;
                match list {
                    T::List(item) if matches!(item.as_ref(), T::Str(_)) => {}
                    ty @ (T::Null
                    | T::Bool
                    | T::Number(_)
                    | T::Str(_)
                    | T::Date
                    | T::Timestamp
                    | T::ParsedDate
                    | T::ParsedTimestamp
                    | T::List(_)
                    | T::Record(_)
                    | T::Optional(_)) => {
                        return self.report(
                            first?,
                            codes::TYPE_ERROR,
                            format!("`join` requires a list of strings, found `{ty}`"),
                        );
                    }
                }
                self.string(arguments.get(1)?)?;
                match arguments.get(1) {
                    Some(Node::String(separator)) if separator.len() <= 256 => Some(T::Str(None)),
                    Some(other) => self.report(
                        other,
                        codes::UNSUPPORTED_CONSTRUCT,
                        "`join` requires a literal separator no longer than 256 bytes".to_owned(),
                    ),
                    None => None,
                }
            }
            F::Lower | F::Upper | F::Trim => {
                self.arity(node, arguments, 1)?;
                self.string(first?)?;
                Some(T::Str(None))
            }
            F::Contains | F::StartsWith | F::EndsWith => {
                self.arity(node, arguments, 2)?;
                let a = self.string(first?);
                let b = self.string(arguments.get(1)?);
                a.and(b).map(|()| T::Bool)
            }
            F::Abs | F::Floor | F::Ceil => {
                self.arity(node, arguments, 1)?;
                self.numeric(first?).map(T::Number)
            }
            F::Round if arguments.len() == 3 => {
                self.exact = true;
                let operand = self.numeric(first?)?;
                self.string(arguments.get(2)?)?;
                if !matches!(arguments.get(1), Some(Node::Number(n)) if n.to_string().parse::<u8>().is_ok_and(|n| n <= 18))
                    || !matches!(
                        arguments.get(2),
                        Some(Node::String(
                            "refuse" | "toward_zero" | "half_even" | "half_away_from_zero"
                        ))
                    )
                {
                    return self.report(
                        node,
                        "core.values.inexact",
                        "round requires a literal scale 0..18 and mode".to_owned(),
                    );
                }
                Some(T::Number(operand))
            }
            F::Round | F::Trunc => {
                if arguments.is_empty() || arguments.len() > 2 {
                    return self.report(
                        node,
                        codes::TYPE_ERROR,
                        "expected 1 or 2 arguments".to_owned(),
                    );
                }
                let operand = self.numeric(first?)?;
                match arguments.get(1) {
                    None => Some(T::Number(operand)),
                    Some(Node::Number(places))
                        if places.to_string().parse::<u8>().is_ok_and(|p| p <= 28) =>
                    {
                        Some(T::Number(operand))
                    }
                    Some(other) => self.report(
                        other,
                        codes::TYPE_ERROR,
                        "the precision must be an integer literal from 0 to 28".to_owned(),
                    ),
                }
            }
            F::Sum => {
                self.arity(node, arguments, 1)?;
                match self.value(
                    first?,
                    Some(&T::List(Box::new(T::Number(Operand::Dimensionless)))),
                )? {
                    T::List(item) if matches!(*item, T::Number(_)) => Some(*item),
                    ty @ (T::Null
                    | T::Bool
                    | T::Number(_)
                    | T::Str(_)
                    | T::Date
                    | T::Timestamp
                    | T::ParsedDate
                    | T::ParsedTimestamp
                    | T::List(_)
                    | T::Record(_)
                    | T::Optional(_)) => {
                        self.report(node, codes::TYPE_ERROR, format!("`sum` of `{ty}`"))
                    }
                }
            }
            other @ (F::Flatten
            | F::Merge
            | F::MergeDeep
            | F::Matches
            | F::Extract
            | F::FuzzyMatch
            | F::Avg
            | F::Min
            | F::Max
            | F::Rand
            | F::Median
            | F::Mode
            | F::IsNumeric
            | F::String
            | F::Number
            | F::Bool
            | F::Type
            | F::Keys
            | F::Values
            | F::Date) => self.unsupported(node, &format!("the builtin `{other}`")),
        }
    }

    fn closure(
        &mut self,
        node: &Node<'_>,
        function: ClosureFunction,
        arguments: &[&Node<'_>],
    ) -> Option<T> {
        use ClosureFunction as C;
        self.arity(node, arguments, 2)?;
        let list = arguments.first().copied()?;
        let element = match self.value(list, None)? {
            T::List(item) => *item,
            ty @ (T::Null
            | T::Bool
            | T::Number(_)
            | T::Str(_)
            | T::Date
            | T::Timestamp
            | T::ParsedDate
            | T::ParsedTimestamp
            | T::Record(_)
            | T::Optional(_)) => {
                return self.report(
                    list,
                    codes::TYPE_ERROR,
                    format!("expected a list, found `{ty}`"),
                );
            }
        };
        let Some(Node::Closure { body, alias }) = arguments.get(1).copied() else {
            return self.unsupported(node, "a closure argument that is not a closure");
        };
        self.scopes.push(Scope {
            alias: alias.map(ToOwned::to_owned),
            element: element.clone(),
        });
        let result = match function {
            C::Map => self.value(body, None).map(|ty| T::List(Box::new(ty))),
            // zen flattens one level: every element of a list body is spliced
            // in, so the result has the body's element type.
            C::FlatMap => match self.value(body, None)? {
                list @ T::List(_) => Some(list),
                ty @ (T::Null
                | T::Bool
                | T::Number(_)
                | T::Str(_)
                | T::Date
                | T::Timestamp
                | T::ParsedDate
                | T::ParsedTimestamp
                | T::Record(_)
                | T::Optional(_)) => self.report(
                    body,
                    codes::TYPE_ERROR,
                    format!("a `flatMap` body must be a list, found `{ty}`"),
                ),
            },
            C::Filter => self.boolean(body).map(|()| T::List(Box::new(element))),
            C::Count => self
                .boolean(body)
                .map(|()| T::Number(Operand::Dimensionless)),
            C::All | C::None | C::Some | C::One => self.boolean(body).map(|()| T::Bool),
        };
        self.scopes.pop();
        result
    }

    /// The result as an [`InferredType`]: every part representable as a
    /// Norn value (an env record's opaque fields are not).
    fn representable(&mut self, ty: &T, span: (u32, u32)) -> Option<InferredType> {
        Some(match ty {
            T::Null => InferredType::Null,
            T::Bool => InferredType::Bool,
            T::Number(_) => InferredType::Number,
            T::Date | T::ParsedDate => InferredType::Date,
            T::Timestamp | T::ParsedTimestamp => InferredType::Timestamp,
            T::Str(None) => InferredType::String,
            T::Str(Some(values)) => match values.len() {
                1 => InferredType::Const(values.iter().next()?.clone()),
                _ => InferredType::Enum(values.iter().cloned().collect()),
            },
            T::List(item) => InferredType::List(Box::new(self.representable(item, span)?)),
            T::Optional(item) => InferredType::Optional(Box::new(self.representable(item, span)?)),
            T::Record(Rec::Literal(fields)) => {
                let mut out = BTreeMap::new();
                for (name, field) in fields {
                    out.insert(name.clone(), self.representable(field, span)?);
                }
                InferredType::Record(out)
            }
            T::Record(Rec::Env { name, depth }) => {
                let record = self.env.record(name)?;
                let mut out = BTreeMap::new();
                for (field_name, field) in &record.fields {
                    let field_depth = match field.source {
                        FieldSource::Link { .. } | FieldSource::Inverse { .. } => {
                            let Some(next) = depth.checked_sub(1) else {
                                continue;
                            };
                            next
                        }
                        FieldSource::Property { .. } | FieldSource::Builtin => *depth,
                    };
                    let Some(lowered) = lower(
                        &field.ty,
                        field_depth,
                        self.env,
                        Some(name),
                        self.exact_math,
                    ) else {
                        self.diagnostics.push(Diagnostic {
                            code: codes::OPAQUE_READ,
                            message: format!("the result includes the opaque field `{field_name}`"),
                            span,
                        });
                        return None;
                    };
                    out.insert(field_name.clone(), self.representable(&lowered, span)?);
                }
                InferredType::Record(out)
            }
        })
    }
}

pub(crate) fn validate_exact_environment(env: &ZenTypeEnv, prefix: &str) -> Result<(), String> {
    for (name, record) in env
        .records()
        .iter()
        .filter(|(name, _)| name.starts_with(prefix))
    {
        for field in record.fields.values() {
            let mut ty = &field.ty;
            while let Type::Optional(inner) | Type::List(inner) = ty {
                ty = inner;
            }
            if let Type::Decimal {
                unit,
                currency_source,
                per_unit,
                ..
            } = ty
                && matches!(
                    units::declared(
                        env,
                        Some(name),
                        unit.as_ref(),
                        currency_source.as_ref(),
                        per_unit.as_ref()
                    ),
                    Operand::Unresolved
                )
            {
                return Err("exact currency requires a fixed singleton currencySource and compatible Decimal bindings".into());
            }
        }
    }
    Ok(())
}

/// VAL-EXACT-1: record references are inspected when their members are read.
fn exact_type(mut ty: &Type) -> bool {
    while let Type::Optional(inner) | Type::List(inner) = ty {
        ty = inner;
    }
    matches!(ty, Type::Decimal { .. })
}

fn expected_exact_type<'a>(ty: &'a Type, env: &'a ZenTypeEnv) -> bool {
    let mut pending = vec![ty];
    let mut visited = BTreeSet::new();
    while let Some(ty) = pending.pop() {
        if exact_type(ty) {
            return true;
        }
        let mut names = Vec::new();
        ty.records(&mut names);
        for name in names {
            if visited.insert(name.clone())
                && let Some(record) = env.record(&name)
            {
                pending.extend(record.fields.values().map(|field| &field.ty));
            }
        }
    }
    false
}

pub(crate) fn decimal_destination(mut ty: &Type) -> Option<norn_ir::PropertyType> {
    while let Type::Optional(inner) = ty {
        ty = inner;
    }
    let Type::Decimal {
        scale,
        unit,
        rounding,
        currency_source,
        per_unit,
    } = ty
    else {
        return None;
    };
    Some(norn_ir::PropertyType::Decimal {
        additivity: norn_ir::PropertyAdditivity::Undeclared {},

        scale: norn_ir::WholeNumber(*scale),
        unit: unit.clone(),
        rounding: Some(*rounding),
        currency_source: currency_source.clone(),
        per_unit: per_unit.clone(),
        unit_source: Vec::new(),
    })
}

/// VAL-ROUND: no information-losing operation enters the exact runtime implicitly.
pub(crate) fn exact_admission<'n>(
    root: &'n Node<'n>,
    metadata: &AstMetadata,
    destination: bool,
) -> Vec<Diagnostic> {
    let mut pending = vec![(root, destination)];
    let mut out = Vec::new();
    while let Some((node, destination)) = pending.pop() {
        let round = matches!(node, Node::FunctionCall { kind: FunctionKind::Internal(InternalFunction::Round), arguments } if arguments.len() >= 2);
        let site = destination
            || matches!(node, Node::FunctionCall { kind: FunctionKind::Internal(InternalFunction::Round), arguments } if arguments.len() == 3);
        let reason = if matches!(
            node,
            Node::Binary {
                operator: Operator::Arithmetic(ArithmeticOperator::Divide),
                ..
            }
        ) && !destination
        {
            Some("division requires a declared Decimal destination or explicit round site")
        } else if matches!(
            node,
            Node::Binary {
                operator: Operator::Comparison(ComparisonOperator::In | ComparisonOperator::NotIn),
                ..
            }
        ) {
            Some("membership is outside the exact arithmetic profile")
        } else if matches!(node, Node::FunctionCall { kind, .. } if !(matches!(kind, FunctionKind::Internal(InternalFunction::Len)) || round && site))
        {
            Some("function requires an explicit supported exact site")
        } else if matches!(node, Node::FunctionCall { kind: FunctionKind::Internal(InternalFunction::Round), arguments } if arguments.get(1).is_some_and(|value| matches!(value, Node::Number(scale) if scale.to_string().parse::<u32>().is_ok_and(|scale| scale > 18))))
        {
            Some("round exceeds the exact arithmetic scale bound")
        } else if !destination && matches!(node, Node::Number(value) if value.scale() > 18) {
            Some("numeric literal exceeds the exact arithmetic scale bound")
        } else {
            None
        };
        if let Some(reason) = reason {
            out.push(Diagnostic {
                code: "core.values.inexact",
                message: reason.to_owned(),
                span: metadata.get(&address(node)).map_or((0, 0), |m| m.span),
            });
        }
        let mut children = Vec::new();
        bounds::children(node, &mut children);
        pending.extend(children.into_iter().map(|child| (child, site)));
    }
    out
}
