//! Compiled expressions and budgeted evaluation.

use std::fmt;
use std::sync::Arc;

use norn_ir::ResourceLimitProfile;

use zen_expression::expression::{Standard, Unary};
use zen_expression::meter::Meter;
use zen_expression::temporal::{LocalWallTime, ResolvedLocalOffset, Temporal, TzdbLookup};
use zen_expression::vm::{VM, VMError};
use zen_expression::{IsolateError, Scope, Variable, compile_expression, compile_unary_expression};

use crate::convert::{from_variable, to_variable};
use crate::temporal::{LocalCivilWallTime, LocalOffsetResolution, TzdbBundle};
use crate::{ExprError, Value};

/// Private bridge from Norn's [`TzdbBundle`] to `zen-expression`'s [`TzdbLookup`],
/// keeping all `zen-*` traits out of `norn-expr`'s public API boundary while
/// resolving against timezones pre-parsed at bundle admission.
#[derive(Debug)]
struct ZenTzdbAdapter(TzdbBundle);

impl TzdbLookup for ZenTzdbAdapter {
    fn resolve_local_offset(&self, name: &str, wall: LocalWallTime) -> Option<ResolvedLocalOffset> {
        let wall = LocalCivilWallTime {
            year: wall.year,
            month: wall.month,
            day: wall.day,
            hour: wall.hour,
            minute: wall.minute,
            second: wall.second,
            nanosecond: wall.nanosecond,
        };
        Some(match self.0.resolve_local_offset(name, wall)? {
            LocalOffsetResolution::Unambiguous { offset_seconds } => {
                ResolvedLocalOffset::Unambiguous(offset_seconds)
            }
            LocalOffsetResolution::Gap => ResolvedLocalOffset::Gap,
            LocalOffsetResolution::Fold {
                before_seconds,
                after_seconds,
            } => ResolvedLocalOffset::Fold {
                before: before_seconds,
                after: after_seconds,
            },
        })
    }
}

fn zen_tzdb(bundle: &TzdbBundle) -> Arc<dyn TzdbLookup> {
    Arc::new(ZenTzdbAdapter(bundle.clone()))
}

/// A compiled ZEN standard expression, ready to evaluate repeatedly.
///
/// Compilation checks syntax only; it does not type-check the expression
/// against an environment (T1-EXPR-2/3) or enforce the Norn profile
/// (T1-EXPR-6).
#[derive(Clone)]
pub struct Expression {
    source: String,
    compiled: zen_expression::Expression<Standard>,
}

impl Expression {
    /// Compiles `source` as a standard expression.
    ///
    /// # Errors
    ///
    /// [`ExprError::Compile`] if `source` does not lex, parse, or compile, or
    /// exceeds the `norn-expr/4` source bounds (16 KiB, brackets 128 deep).
    pub fn compile(source: &str) -> Result<Self, ExprError> {
        norn_ir::counters::bump(norn_ir::counters::Counter::ExpressionCompiles);
        source_bounds(source, false)?;
        let compiled = compile_expression(source).map_err(map_isolate_error)?;
        Ok(Self {
            source: source.to_owned(),
            compiled,
        })
    }

    /// The expression source text.
    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Evaluates the expression with `context` as its root scope, charging
    /// every operation to a fresh meter limited by `budget`, and then the
    /// recursive size of the result (the copy into a [`Value`]).
    ///
    /// # Errors
    ///
    /// - [`ExprError::BudgetExhausted`] if the evaluation (with the result
    ///   copy) needs more units than `budget`.
    /// - [`ExprError::Evaluation`] if the engine fails at runtime.
    /// - [`ExprError::InvalidNumber`] if `context` holds a number the engine
    ///   cannot represent.
    /// - [`ExprError::UnsupportedResult`] if the result has no [`Value`]
    ///   representation (for example a date).
    pub fn evaluate(
        &self,
        context: &Value,
        budget: EvaluationBudget,
    ) -> Result<Evaluation, ExprError> {
        self.evaluate_in(context, budget, TzdbBundle::default_bundle())
    }

    /// Evaluates the expression with an explicit versioned [`TzdbBundle`],
    /// charging every operation to a fresh meter limited by `budget`.
    ///
    /// # Errors
    ///
    /// As [`Self::evaluate`].
    pub fn evaluate_in(
        &self,
        context: &Value,
        budget: EvaluationBudget,
        tzdb: &TzdbBundle,
    ) -> Result<Evaluation, ExprError> {
        self.evaluate_metered_in(context, budget, tzdb).result
    }

    /// Evaluates the expression and returns meter usage even when evaluation
    /// fails. This lets a caller carry one fuel budget across several
    /// independent expressions without refunding the work before a runtime
    /// error.
    ///
    /// `units_used` reports charged work, capped at `budget.units()`. A charge
    /// that exceeds the remaining budget can exhaust evaluation before all
    /// remaining units are spent.
    #[must_use]
    pub fn evaluate_metered(&self, context: &Value, budget: EvaluationBudget) -> EvaluationAttempt {
        self.evaluate_metered_in(context, budget, TzdbBundle::default_bundle())
    }

    /// Evaluates the expression with an explicit versioned [`TzdbBundle`] and
    /// returns meter usage even when evaluation fails.
    #[must_use]
    pub fn evaluate_metered_in(
        &self,
        context: &Value,
        budget: EvaluationBudget,
        tzdb: &TzdbBundle,
    ) -> EvaluationAttempt {
        self.evaluate_with_meter(context, &Meter::new(budget.units()), tzdb)
    }

    /// VAL-ROUND: check the shared type grammar and the declared exact-site profile.
    ///
    /// # Errors
    /// Returns structural/type findings or refusals from the exact arithmetic profile.
    pub fn check_typed(
        &self,
        env: &crate::ZenTypeEnv,
        expected: &crate::Type,
    ) -> Result<crate::CheckedExpression, Vec<crate::Diagnostic>> {
        self.check_typed_with(env, expected, &ResourceLimitProfile::norn_limits_v1())
    }

    /// VAL-ROUND: check the shared type grammar and the declared exact-site profile
    /// against an explicit resource limit profile, falling back to the plain profile
    /// when no exact decimal read, exact destination, or explicit three-argument
    /// `round(..., scale, mode)` site is present.
    ///
    /// # Errors
    /// Returns structural/type findings or refusals from the exact arithmetic profile.
    pub fn check_typed_with(
        &self,
        env: &crate::ZenTypeEnv,
        expected: &crate::Type,
        limits: &ResourceLimitProfile,
    ) -> Result<crate::CheckedExpression, Vec<crate::Diagnostic>> {
        crate::check::check_typed_expression_in(
            self,
            env,
            expected,
            limits,
            TzdbBundle::default_bundle(),
        )
    }

    /// VAL-ROUND: [`Self::check_typed`] against an explicit versioned [`TzdbBundle`].
    ///
    /// # Errors
    /// Returns structural/type findings or refusals from the exact arithmetic profile.
    pub fn check_typed_in(
        &self,
        env: &crate::ZenTypeEnv,
        expected: &crate::Type,
        tzdb: &TzdbBundle,
    ) -> Result<crate::CheckedExpression, Vec<crate::Diagnostic>> {
        crate::check::check_typed_expression_in(
            self,
            env,
            expected,
            &ResourceLimitProfile::norn_limits_v1(),
            tzdb,
        )
    }

    /// VAL-EXACT-1: the compile [`Self::evaluate_admitted_metered`] runs before
    /// evaluating, as an admission check. An expression this accepts compiles
    /// at every evaluation; one it refuses would refuse every evaluation.
    ///
    /// # Errors
    /// The exact profile's findings, when the expression needs that profile.
    pub fn check_admitted(
        &self,
        env: &crate::ZenTypeEnv,
        expected: &crate::Type,
    ) -> Result<(), Vec<crate::Diagnostic>> {
        self.check_admitted_in(env, expected, TzdbBundle::default_bundle())
    }

    /// VAL-EXACT-1: [`Self::check_admitted`] against an explicit versioned [`TzdbBundle`].
    ///
    /// # Errors
    /// The exact profile's findings, when the expression needs that profile.
    pub fn check_admitted_in(
        &self,
        env: &crate::ZenTypeEnv,
        expected: &crate::Type,
        tzdb: &TzdbBundle,
    ) -> Result<(), Vec<crate::Diagnostic>> {
        if crate::check::requires_exact_profile(self, env, expected) {
            self.check_typed_in(env, expected, tzdb).map(|_| ())
        } else {
            Ok(())
        }
    }

    /// VAL-EXACT-1: execute an admitted consumer while retaining legacy non-exact behavior.
    /// Admission owns static type/cost checks; every runtime path retains the same meter.
    /// Exact reads, destinations and explicit round sites still use the strict typed binding.
    #[must_use]
    pub fn evaluate_admitted_metered(
        &self,
        context: &Value,
        budget: EvaluationBudget,
        env: &crate::ZenTypeEnv,
        expected: &crate::Type,
    ) -> EvaluationAttempt {
        self.evaluate_admitted_metered_in(
            context,
            budget,
            env,
            expected,
            TzdbBundle::default_bundle(),
        )
    }

    /// VAL-EXACT-1: [`Self::evaluate_admitted_metered`] with an explicit versioned [`TzdbBundle`].
    #[must_use]
    pub fn evaluate_admitted_metered_in(
        &self,
        context: &Value,
        budget: EvaluationBudget,
        env: &crate::ZenTypeEnv,
        expected: &crate::Type,
        tzdb: &TzdbBundle,
    ) -> EvaluationAttempt {
        self.run_prepared(
            &self.admission_kind(env, expected, tzdb),
            context,
            budget,
            env,
            expected,
            tzdb,
        )
    }

    /// Decide once how [`Self::evaluate_admitted_metered`] runs this expression
    /// in `env` for `expected`: the exact-profile test, the typed check and the
    /// lowering depend only on those three, not on the evaluated values.
    #[must_use]
    pub fn prepare_admitted(
        &self,
        env: &crate::ZenTypeEnv,
        expected: &crate::Type,
    ) -> PreparedAdmission {
        self.prepare_admitted_in(env, expected, TzdbBundle::default_bundle())
    }

    /// [`Self::prepare_admitted`] against an explicit versioned [`TzdbBundle`].
    #[must_use]
    pub fn prepare_admitted_in(
        &self,
        env: &crate::ZenTypeEnv,
        expected: &crate::Type,
        tzdb: &TzdbBundle,
    ) -> PreparedAdmission {
        PreparedAdmission {
            source: self.source.clone(),
            tzdb_sha256: tzdb.sha256().to_owned(),
            kind: self.admission_kind(env, expected, tzdb),
        }
    }

    fn admission_kind(
        &self,
        env: &crate::ZenTypeEnv,
        expected: &crate::Type,
        tzdb: &TzdbBundle,
    ) -> Prepared {
        if crate::check::requires_exact_profile(self, env, expected) {
            self.prepare_typed(env, expected, tzdb)
        } else {
            Prepared::Plain
        }
    }

    /// Evaluate with a [`PreparedAdmission`] this expression produced for the
    /// same `env` and `expected`. Equal to [`Self::evaluate_admitted_metered`]
    /// or [`Self::evaluate_typed_metered`], whichever prepared it.
    #[must_use]
    pub fn evaluate_prepared(
        &self,
        prepared: &PreparedAdmission,
        context: &Value,
        budget: EvaluationBudget,
        env: &crate::ZenTypeEnv,
        expected: &crate::Type,
    ) -> EvaluationAttempt {
        self.evaluate_prepared_in(
            prepared,
            context,
            budget,
            env,
            expected,
            TzdbBundle::default_bundle(),
        )
    }

    /// [`Self::evaluate_prepared`] with an explicit versioned [`TzdbBundle`].
    #[must_use]
    pub fn evaluate_prepared_in(
        &self,
        prepared: &PreparedAdmission,
        context: &Value,
        budget: EvaluationBudget,
        env: &crate::ZenTypeEnv,
        expected: &crate::Type,
        tzdb: &TzdbBundle,
    ) -> EvaluationAttempt {
        if prepared.source != self.source {
            return EvaluationAttempt {
                result: Err(ExprError::Compile {
                    message: format!(
                        "the prepared admission is for `{}`, not `{}`",
                        prepared.source, self.source
                    ),
                }),
                units_used: 0,
            };
        }
        if prepared.tzdb_sha256 != tzdb.sha256() {
            return EvaluationAttempt {
                result: Err(ExprError::TzdbMismatch {
                    admitted: prepared.tzdb_sha256.clone(),
                    evaluated: tzdb.sha256().to_owned(),
                }),
                units_used: 0,
            };
        }
        self.run_prepared(&prepared.kind, context, budget, env, expected, tzdb)
    }

    fn run_prepared(
        &self,
        kind: &Prepared,
        context: &Value,
        budget: EvaluationBudget,
        env: &crate::ZenTypeEnv,
        expected: &crate::Type,
        tzdb: &TzdbBundle,
    ) -> EvaluationAttempt {
        match kind {
            Prepared::Plain => self.evaluate_metered_in(context, budget, tzdb),
            Prepared::Refused(error) => EvaluationAttempt {
                result: Err(error.clone()),
                units_used: 0,
            },
            Prepared::Exact(lowered) => {
                let mut attempt = lowered.evaluate_exact_site(context, budget, None, false, tzdb);
                if let Ok(evaluation) = &mut attempt.result
                    && let Err(error) = materialize(expected, env, &mut evaluation.value)
                {
                    attempt.result = Err(error);
                }
                attempt
            }
        }
    }

    fn prepare_typed(
        &self,
        env: &crate::ZenTypeEnv,
        expected: &crate::Type,
        tzdb: &TzdbBundle,
    ) -> Prepared {
        let checked = match self.check_typed_in(env, expected, tzdb) {
            Ok(checked) => checked,
            Err(found) => {
                return Prepared::Refused(ExprError::Compile {
                    message: found
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join("; "),
                });
            }
        };
        if checked.uses_exact_arithmetic() {
            match Self::compile(checked.runtime_source()) {
                Ok(lowered) => Prepared::Exact(Box::new(lowered)),
                Err(error) => Prepared::Refused(error),
            }
        } else {
            Prepared::Plain
        }
    }

    /// VAL-EXACT-1 / VAL-ROUND: bind admission and execution to the same source
    /// and declared environment; plain Number retains its existing evaluator.
    #[must_use]
    pub fn evaluate_typed_metered(
        &self,
        context: &Value,
        budget: EvaluationBudget,
        env: &crate::ZenTypeEnv,
        expected: &crate::Type,
    ) -> EvaluationAttempt {
        self.evaluate_typed_metered_in(context, budget, env, expected, TzdbBundle::default_bundle())
    }

    /// VAL-EXACT-1 / VAL-ROUND: [`Self::evaluate_typed_metered`] with an
    /// explicit versioned [`TzdbBundle`].
    #[must_use]
    pub fn evaluate_typed_metered_in(
        &self,
        context: &Value,
        budget: EvaluationBudget,
        env: &crate::ZenTypeEnv,
        expected: &crate::Type,
        tzdb: &TzdbBundle,
    ) -> EvaluationAttempt {
        self.run_prepared(
            &self.prepare_typed(env, expected, tzdb),
            context,
            budget,
            env,
            expected,
            tzdb,
        )
    }

    /// VAL-EXACT-1 / VAL-ROUND: use exact intermediates and an optional destination.
    /// A destination supplies its declared mode, defaulting to exact/refuse.
    #[must_use]
    pub fn evaluate_exact_metered(
        &self,
        context: &Value,
        budget: EvaluationBudget,
        site: Option<&norn_ir::PropertyType>,
    ) -> EvaluationAttempt {
        self.evaluate_exact_metered_in(context, budget, site, TzdbBundle::default_bundle())
    }

    /// VAL-EXACT-1 / VAL-ROUND: [`Self::evaluate_exact_metered`] with an
    /// explicit versioned [`TzdbBundle`].
    #[must_use]
    pub fn evaluate_exact_metered_in(
        &self,
        context: &Value,
        budget: EvaluationBudget,
        site: Option<&norn_ir::PropertyType>,
        tzdb: &TzdbBundle,
    ) -> EvaluationAttempt {
        self.evaluate_exact_site(context, budget, site, false, tzdb)
    }

    fn evaluate_exact_site(
        &self,
        context: &Value,
        budget: EvaluationBudget,
        site: Option<&norn_ir::PropertyType>,
        nullable: bool,
        tzdb: &TzdbBundle,
    ) -> EvaluationAttempt {
        let meter = Meter::new(budget.units());
        let result = (|| {
            let profile = match site {
                Some(norn_ir::PropertyType::Decimal {
                    scale, rounding, ..
                }) => Some(crate::convert::scaled_site(
                    scale.0,
                    rounding.unwrap_or(norn_ir::Rounding::Refuse),
                )?),
                None => None,
                Some(_) => {
                    return Err(ExprError::Evaluation {
                        message: "exact site must be a declared Decimal".to_owned(),
                    });
                }
            };
            let evaluated =
                self.evaluate_with_meter(context, &meter.with_exact_arithmetic(profile), tzdb);
            let mut evaluated = evaluated.result?;
            if nullable && evaluated.value == Value::Null {
                return Ok(evaluated);
            }
            if let Some(site) = site {
                let Value::Number(number) = &evaluated.value else {
                    return Err(ExprError::Evaluation {
                        message: "Decimal site requires a numeric result".to_owned(),
                    });
                };
                evaluated.value = Value::Number(crate::rescale_decimal(site, number)?);
            }
            Ok(evaluated)
        })();
        EvaluationAttempt {
            result,
            units_used: meter.used().min(meter.limit()),
        }
    }

    fn evaluate_with_meter(
        &self,
        context: &Value,
        meter: &Meter,
        tzdb: &TzdbBundle,
    ) -> EvaluationAttempt {
        let mut vm = VM::new();
        vm.set_meter(Some(meter.clone()));
        vm.set_tzdb(Some(zen_tzdb(tzdb)));
        let result = (|| {
            let context = to_variable(context)?;
            let result = self.compiled.evaluate_with(context, &mut vm);
            // A swallowed charge failure still leaves the meter exhausted;
            // report it rather than a partial result.
            if let Some(exhausted) = meter.exhausted() {
                return Err(ExprError::BudgetExhausted {
                    limit: exhausted.limit,
                    used: exhausted.used,
                });
            }
            let result = result.map_err(map_isolate_error)?;
            // Converting the result copies every value it holds, and zen
            // values share structure (`map(xs, xs)` is |xs| handles to one
            // list), so the copy is charged too before it is made (T1-EXPR-4).
            let output = copy_cost(&result, meter.remaining());
            meter
                .charge(output)
                .map_err(|exhausted| ExprError::BudgetExhausted {
                    limit: exhausted.limit,
                    used: exhausted.used,
                })?;
            let value = from_variable(&result)?;
            Ok(Evaluation {
                value,
                units_used: meter.used(),
            })
        })();
        EvaluationAttempt {
            result,
            units_used: meter.used().min(meter.limit()),
        }
    }
}

/// Checks that `source` compiles as a ZEN **unary test**: the form of a
/// decision-table input cell whose column declares a `field` (for example
/// `> 10000`, `"gold", "silver"`, or `[1..10]`), evaluated against that field.
///
/// Syntax only, like [`Expression::compile`]: it neither type-checks the test
/// nor enforces the Norn profile; [`check_unary`](crate::check_unary) does.
/// [`UnaryTest::compile`] also returns the compiled test.
///
/// # Errors
///
/// [`ExprError::Compile`] if `source` does not lex, parse, or compile, or
/// exceeds the `norn-expr/4` source bounds.
pub fn check_unary_syntax(source: &str) -> Result<(), ExprError> {
    source_bounds(source, true)?;
    compile_unary_expression(source)
        .map(|_compiled| ())
        .map_err(map_isolate_error)
}

/// A compiled ZEN **unary test**: the form of a decision-table input cell
/// whose column declares a `field` (for example `> 10000`, `"gold", "silver"`,
/// or `[1..5]`), evaluated against that field's value, the *subject* `$`.
///
/// zen desugars each comma- or `and`/`or`-joined part of a test: a part that
/// starts with a comparison operator compares `$` (`> 5` is `$ > 5`), a list
/// or range is a membership test (`[1..5]` is `$ in [1..5]`), a boolean-valued
/// part (`$ > 5`, `contains($, 'x')`, `some(...)`) is used as is, and any other
/// value is compared for equality (`'gold'` is `$ == 'gold'`). A comma joins
/// parts with `or`.
///
/// The empty source is the wildcard: the decision engine skips an empty cell
/// without evaluating it, so it matches every subject. Any other source must
/// compile.
#[derive(Clone)]
pub struct UnaryTest {
    source: String,
    /// `None` for the wildcard.
    compiled: Option<zen_expression::Expression<Unary>>,
}

impl UnaryTest {
    /// Compiles `source` as a unary test (`""` is the wildcard).
    ///
    /// # Errors
    ///
    /// [`ExprError::Compile`] if a non-empty `source` exceeds the `norn-expr/4`
    /// source bounds or does not lex, parse, or
    /// compile as a unary test.
    pub fn compile(source: &str) -> Result<Self, ExprError> {
        norn_ir::counters::bump(norn_ir::counters::Counter::ExpressionCompiles);
        let compiled = if source.is_empty() {
            None
        } else {
            source_bounds(source, true)?;
            Some(compile_unary_expression(source).map_err(map_isolate_error)?)
        };
        Ok(Self {
            source: source.to_owned(),
            compiled,
        })
    }

    /// The test source text.
    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Whether this is the empty wildcard test, which matches every subject.
    #[must_use]
    pub fn is_wildcard(&self) -> bool {
        self.compiled.is_none()
    }

    /// Evaluates the test with `context` as its root scope and `subject` as
    /// `$`, as the decision engine does for an input cell, charging every
    /// operation to a fresh meter limited by `budget`.
    ///
    /// # Errors
    ///
    /// - [`ExprError::BudgetExhausted`] if the evaluation needs more units
    ///   than `budget`.
    /// - [`ExprError::Evaluation`] if the engine fails at runtime or the test
    ///   does not produce a boolean.
    /// - [`ExprError::InvalidNumber`] if `context` or `subject` holds a number
    ///   the engine cannot represent.
    pub fn evaluate(
        &self,
        context: &Value,
        subject: &Value,
        budget: EvaluationBudget,
    ) -> Result<bool, ExprError> {
        self.evaluate_in(context, subject, budget, TzdbBundle::default_bundle())
    }

    /// Evaluates the unary test against an explicit versioned [`TzdbBundle`].
    ///
    /// # Errors
    ///
    /// As [`Self::evaluate`].
    pub fn evaluate_in(
        &self,
        context: &Value,
        subject: &Value,
        budget: EvaluationBudget,
        tzdb: &TzdbBundle,
    ) -> Result<bool, ExprError> {
        let Some(compiled) = &self.compiled else {
            return Ok(true);
        };
        let mut scope = Scope::new(to_variable(context)?);
        scope.set_local(Variable::dollar_key(), to_variable(subject)?);
        let meter = Meter::new(budget.units());
        let mut vm = VM::new();
        vm.set_meter(Some(meter.clone()));
        vm.set_tzdb(Some(zen_tzdb(tzdb)));
        let result = compiled.evaluate_with_scope(&scope, &mut vm);
        if let Some(exhausted) = meter.exhausted() {
            return Err(ExprError::BudgetExhausted {
                limit: exhausted.limit,
                used: exhausted.used,
            });
        }
        result.map_err(map_isolate_error)
    }
}

impl fmt::Debug for UnaryTest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UnaryTest")
            .field("source", &self.source)
            .finish_non_exhaustive()
    }
}

/// How an admitted expression runs in one environment for one expected type.
///
/// Only [`Expression::prepare_admitted`] makes one, so its choice of evaluator
/// cannot be forged, and it remembers the source it was prepared from:
/// [`Expression::evaluate_prepared`] refuses one prepared from a different
/// expression (VAL-EXACT-1).
#[derive(Clone, Debug)]
pub struct PreparedAdmission {
    source: String,
    tzdb_sha256: String,
    kind: Prepared,
}

impl PreparedAdmission {
    /// Returns the SHA-256 digest of the [`TzdbBundle`] this admission was bound to.
    #[must_use]
    pub fn tzdb_sha256(&self) -> &str {
        &self.tzdb_sha256
    }
}

#[derive(Clone, Debug)]
enum Prepared {
    /// The plain metered evaluator.
    Plain,
    /// The exact profile, with the lowered expression it runs.
    Exact(Box<Expression>),
    /// The typed check or lowering refused; every evaluation returns this
    /// error and charges nothing.
    Refused(ExprError),
}

impl fmt::Debug for Expression {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Expression")
            .field("source", &self.source)
            .finish_non_exhaustive()
    }
}

/// The maximum number of deterministic operation units one evaluation may
/// use. Every VM opcode costs one unit; data-proportional opcodes and
/// builtins (string, collection) cost more (the zen fork's metering cost
/// model). Every evaluation is metered: there is no unmetered path.
///
/// A decision evaluates under its limit profile's decide fuel
/// ([`EvaluationBudget::decide`], the [`Default`]). An admitted expression's
/// [`CostEstimate`](crate::CostEstimate) is at most that budget, so on a
/// conforming slice it never exhausts it; any other input is still stopped by
/// the meter with [`ExprError::BudgetExhausted`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EvaluationBudget(u64);

impl Default for EvaluationBudget {
    /// The decide fuel of the shipped limit profile `norn.limits/1`.
    fn default() -> Self {
        Self::decide(&ResourceLimitProfile::norn_limits_v1())
    }
}

impl EvaluationBudget {
    /// The per-decision fuel of `profile` (`decide.fuel_units`).
    #[must_use]
    pub const fn decide(profile: &ResourceLimitProfile) -> Self {
        Self(profile.decide.fuel_units)
    }

    /// A budget of `units` operation units.
    #[must_use]
    pub const fn new(units: u64) -> Self {
        Self(units)
    }

    /// The unit limit.
    #[must_use]
    pub const fn units(self) -> u64 {
        self.0
    }
}

/// A successful evaluation: the result and the units it used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Evaluation {
    value: Value,
    units_used: u64,
}

/// A metered expression attempt, including units spent before any error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EvaluationAttempt {
    /// The successful value or the expression error.
    pub result: Result<Evaluation, ExprError>,
    /// Meter units used, capped at the attempt's budget.
    pub units_used: u64,
}

impl Evaluation {
    /// The expression's result.
    #[must_use]
    pub fn value(&self) -> &Value {
        &self.value
    }

    /// Consumes the evaluation, returning the result.
    #[must_use]
    pub fn into_value(self) -> Value {
        self.value
    }

    /// Operation units charged (at most the budget).
    #[must_use]
    pub fn units_used(&self) -> u64 {
        self.units_used
    }
}

/// The units charged for copying `value` out of the evaluator: every value
/// it holds costs at least 1, and a string its length in bytes when longer
/// (so a list of empty strings still costs its element count). The walk uses
/// an explicit stack and stops once the total exceeds `cap` (the meter's
/// remaining units), returning `cap + 1`, so measuring is bounded by the
/// budget too.
fn copy_cost(value: &Variable, cap: u64) -> u64 {
    let mut total: u64 = 0;
    let mut stack: Vec<Variable> = vec![value.clone()];
    while let Some(value) = stack.pop() {
        // A container's children each cost at least 1: count them before
        // pushing them, so the stack never outgrows the budget.
        let (own, children) = match &value {
            Variable::String(text) => (u64::try_from(text.len()).unwrap_or(u64::MAX).max(1), 0),
            Variable::Dynamic(dynamic) => (
                dynamic
                    .as_any()
                    .downcast_ref::<Temporal>()
                    .map_or(1, |temporal| {
                        u64::try_from(temporal.to_string().len())
                            .unwrap_or(u64::MAX)
                            .max(1)
                    }),
                0,
            ),
            Variable::Array(items) => (1, items.borrow().len()),
            Variable::Object(entries) => (1, entries.borrow().len()),
            Variable::Null | Variable::Bool(_) | Variable::Number(_) => (1, 0),
        };
        total = total
            .saturating_add(own)
            .saturating_add(u64::try_from(children).unwrap_or(u64::MAX));
        if total > cap {
            return cap.saturating_add(1);
        }
        // Children were counted at 1 each above; each adds the rest itself.
        total = total.saturating_sub(u64::try_from(children).unwrap_or(u64::MAX));
        match &value {
            Variable::Array(items) => stack.extend(items.borrow().iter().cloned()),
            Variable::Object(entries) => stack.extend(entries.borrow().values().cloned()),
            Variable::Null
            | Variable::Bool(_)
            | Variable::Number(_)
            | Variable::String(_)
            | Variable::Dynamic(_) => {}
        }
    }
    total
}

/// The `norn-expr/4` source bounds (length, bracket nesting), checked before
/// zen parses and compiles recursively: an over-deep source is rejected as a
/// compile error instead of exhausting the stack (T1-EXPR-4, #2852 risk).
fn source_bounds(source: &str, unary: bool) -> Result<(), ExprError> {
    match crate::check::bounds::lexical(source, &crate::check::StructuralLimits::NORN_EXPR_4, unary)
    {
        Some(found) => Err(ExprError::Compile {
            message: found.to_string(),
        }),
        None => Ok(()),
    }
}

fn map_isolate_error(err: IsolateError) -> ExprError {
    match err {
        IsolateError::VMError {
            source: VMError::BudgetExhausted(exhausted),
        } => ExprError::BudgetExhausted {
            limit: exhausted.limit,
            used: exhausted.used,
        },
        IsolateError::VMError { source } => ExprError::Evaluation {
            message: source.to_string(),
        },
        IsolateError::LexerError { .. }
        | IsolateError::ParserError { .. }
        | IsolateError::CompilerError { .. } => ExprError::Compile {
            message: err.to_string(),
        },
        IsolateError::ValueCastError
        | IsolateError::ReferenceError
        | IsolateError::MissingContextReference => ExprError::Evaluation {
            message: err.to_string(),
        },
    }
}

fn materialize(
    ty: &crate::Type,
    env: &crate::ZenTypeEnv,
    value: &mut Value,
) -> Result<(), ExprError> {
    match (ty, value) {
        (crate::Type::Optional(_), Value::Null) => Ok(()),
        (crate::Type::Optional(inner), value) => materialize(inner, env, value),
        (crate::Type::Decimal { .. }, Value::Number(number)) => {
            *number = crate::rescale_decimal(
                &crate::check::decimal_destination(ty).ok_or_else(|| ExprError::Evaluation {
                    message: "invalid Decimal site".into(),
                })?,
                number,
            )?;
            Ok(())
        }
        (crate::Type::Record(name), Value::Object(fields)) => {
            if let Some(record) = env.record(name) {
                for (key, value) in fields {
                    if let Some(field) = record.fields.get(key) {
                        materialize(&field.ty, env, value)?;
                    }
                }
            }
            Ok(())
        }
        (crate::Type::List(item), Value::Array(items)) => {
            for value in items {
                materialize(item, env, value)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::check_unary_syntax;

    #[test]
    fn unary_tests_are_syntax_checked() {
        for admitted in ["> 10000", "'gold', 'silver'", "[1..10]", "$ > 5 and $ < 10"] {
            assert!(check_unary_syntax(admitted).is_ok(), "{admitted}");
        }
        for rejected in ["> > 1", "[1..", "'open"] {
            assert!(check_unary_syntax(rejected).is_err(), "{rejected}");
        }
    }
}
