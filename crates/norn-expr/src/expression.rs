//! Compiled expressions and budgeted evaluation.

use std::fmt;

use norn_ir::ResourceLimitProfile;

use zen_expression::expression::{Standard, Unary};
use zen_expression::meter::Meter;
use zen_expression::temporal::Temporal;
use zen_expression::vm::{VM, VMError};
use zen_expression::{IsolateError, Scope, Variable, compile_expression, compile_unary_expression};

use crate::convert::{from_variable, to_variable};
use crate::{ExprError, Value};

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
        self.evaluate_metered(context, budget).result
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
        let meter = Meter::new(budget.units());
        let mut vm = VM::new();
        vm.set_meter(Some(meter.clone()));
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
            units_used: meter.used().min(budget.units()),
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
        let Some(compiled) = &self.compiled else {
            return Ok(true);
        };
        let mut scope = Scope::new(to_variable(context)?);
        scope.set_local(Variable::dollar_key(), to_variable(subject)?);
        let meter = Meter::new(budget.units());
        let mut vm = VM::new();
        vm.set_meter(Some(meter.clone()));
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
