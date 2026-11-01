# Ordered-fold semantics 1.0

Status: structural declaration and fail-closed refusal. Execution is unavailable
until the evaluator implements this contract; ordinary JDM loop mode is not an
implementation. This extends an existing RuleIR `decisionNode`, whose `key`
retains the existing required dependency and sub-decision closure rules.

## Declaration and transition relation

FOLD-ORDER: `inputField` selects the complete finite item collection. Every item
satisfies `itemSchema`. Sort ascending lexicographically by the declared required
non-null string/integer properties. Strings use canonical NFC scalar order;
integers compare numerically, never by decimal spelling. The final key is
`identity`, a stable unique string. Duplicate canonical identities, missing keys,
nulls and wrong types refuse. Enumeration order cannot break ties.

FOLD-STATE: evaluate `initial` exactly once in the node input scope. The initial
state and every subsequent state satisfy the closed `accumulatorSchema`. The
existing sub-decision receives exactly `{accumulator, item, index}`; `index` is
the zero-based sorted position. Its complete output is the next accumulator.
The step input schema must match those three types; its output schema must match
the accumulator. Residual quantities, allocation identities and provenance are
ordinary declared accumulator members, with no reserved application fields.
An empty collection returns the initial state. The last state is the node result,
using existing `outputPath`/`passThrough` composition, never a new effect engine.

FOLD-BOUNDS: `maxItems` is positive and at most 4096; the canonical encoded initial
and subsequent accumulator is at most `maxAccumulatorBytes` (positive, at most
1048576). The admitted resource profile may narrow these ceilings. Check bounds
before each step and charge all evaluation, comparison and state materialization
to the caller's shared deterministic fuel. Never truncate, reset fuel per item,
or accept an incomplete collection. Recursion and nested folds are unsupported.

FOLD-ARITHMETIC: numeric leaves retain exact core decimal/scaled-integer values,
compatible with quantity-ledger scale 0..18. Addition, subtraction, multiplication
and rescaling are checked; overflow and unrepresentable values refuse. Division
and quantization use the declared result scale and rounding: `refuse` rejects
nonzero remainder; `toward_zero` discards its magnitude; `half_even` selects the
nearest value and the even coefficient at an exact midpoint;
`half_away_from_zero` selects the nearest value and increases the absolute
coefficient at an exact midpoint. Both nearest modes apply symmetrically to
negative inputs. Division by zero refuses, including zero balance unless a
rule explicitly branches before division. No f64 conversion, implicit rounding,
ordinary ZEN division or nullable arithmetic fallback satisfies this policy.

FOLD-NO-PARTIAL: any schema, order, arithmetic, fuel or bound failure produces no
result and no decision effects. Successful results feed the existing list-valued
RuleIR `{facts, commands, timers}`. No writes occur during a fold. Solver statement
generation must use this same transition relation and include every refusal
branch, bounds, rounding mode, declaration digest and semantics version.

## Required conformance scenarios

FOLD-GENERIC: capacities 5 and 5, demand 7 produce allocations 5 and 2 and residual
capacity 3. Reversing enumeration with unchanged keys gives the identical result.
FOLD-CP1: receipts 2 at 10 and 3 at 20, demand 4 produce value 60 and residual
quantity 1/value 20. These are declarations, never evaluator purpose switches.
Preserve the moving-average, transfer, original-cost partial-return and reversal
oracles from #257/#271/#274/#275/#278 when enabling execution. Transfers retain
allocation identity; transfer consumption is not a returnable sale. Midpoint
positive/negative values, exact/nonexact division, zero divisor, overflow, empty
input, duplicate identity, equal primary keys, oversized state, exhausted fuel
and a step failing after earlier success are mandatory refusal witnesses.

A declaration currently reports `rule.validation.ordered_fold_invalid` for a
structural defect, and `rule.validation.ordered_fold_unsupported` for unavailable
execution. Both are errors. The evaluator independently refuses `orderedFold`
before constructing a normal sub-decision, even if the checker was skipped.
No solver evidence or executable application capability is claimed by this cut.

## Scaled VM correspondence (A2 arithmetic cut)

FOLD-SCALED: every numeric operand must be exactly representable at the declared
scale. Coefficients and all intermediate products use checked signed i128;
results must also fit the core exact 96-bit decimal coefficient. An intermediate
overflow refuses even if algebraic cancellation could fit the final result.
Add/subtract operate on aligned coefficients. Multiply divides their product by
10^scale; divide multiplies the numerator by 10^scale before division. Each
quotient uses the declared rounding and exact remainder, never decimal division.
`half_even` tests coefficient parity, including negative values. Scale is 0..18.

FOLD-NUMERIC-CLOSURE: the scaled VM permits the four arithmetic operators,
negation, comparisons, conditional/list/record construction and the non-arithmetic
`len`/`flatten` functions. Compiler-generated record keys permit string-to-string
conversion; converting other types is refused. Other numeric functions, methods, modulo and power
refuse rather than escape the declared arithmetic. This policy shares the caller's
fuel counter; it never changes another clone's arithmetic policy. The evaluator
must validate numeric input/output leaves as well as numeric literal operands.
`scaled` unit correspondence covers exact results, both midpoint signs,
negative divisors, inexact refusal, zero division, scale and overflow boundaries,
and VM dispatch with shared fuel. Fold admission remains refused in this cut.

FOLD-STEP-SCHEMA: helper contract equality compares parsed schemas, independent
of object or required-member ordering. The checker and runtime admission share
`fold_step_schemas`; extra, optional, nullable or differently typed step fields
refuse. Each invocation checks its exact input before running any helper node
and its complete output before it becomes state. Nested ordinary calls retain
the fold helper's boundary check. Non-fold helper admission is unchanged.
The checks charge materialization to shared fuel. No schema failure produces
an accumulator or effects; the execution-unavailable gate still applies here.

FOLD-PREFLIGHT: parse and retain helper/item/state schemas once at admission.
Evaluation checks values against those exact retained contracts; it does not
reparse declaration schemas outside the caller's meter on every invocation.
FOLD-NAMESPACE: effective fold owner/node identities must be unambiguous across
the reachable closure; an internal-plan collision refuses before evaluation.
FOLD-ORDER-FUEL: sorting charges both key vectors before each comparison (one
per integer, one plus UTF-8 bytes per string). Inserting canonical identity n
charges n times one plus its bytes, conservatively bounding identity searches.

FOLD-VM-BOUNDARY: a profiled VM checks numeric leaves in its base scope, locals
and complete result, including identity/negation and nested records/lists. The
iterative walk charges nodes and pending children before allocating traversal
work. No caller can attach a profile and pass excess-precision values through.
