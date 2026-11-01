# Ordered-fold semantics 1.0

Status: bounded runtime execution with declaration-driven oracle conformance.
Ordinary JDM loop mode is not an implementation. This extends an existing RuleIR `decisionNode`, whose `key`
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

A structurally unsupported declaration reports `rule.validation.ordered_fold_invalid`.
The evaluator independently rechecks the closed profile and helper contracts.
The supported fragment is executable; no generated solver proof is claimed.

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
`len`/`flatten` functions. String-to-string conversion supports compiler-generated
record keys; conversion of numbers or other values is refused. Other numeric functions, methods, modulo and power
refuse rather than escape the declared arithmetic. This policy shares the caller's
fuel counter; it never changes another clone's arithmetic policy. The evaluator
must validate numeric input/output leaves as well as numeric literal operands.
`scaled` unit correspondence covers exact results, both midpoint signs,
negative divisors, inexact refusal, zero division, scale and overflow boundaries,
and VM dispatch with shared fuel. The scaled policy is scoped to this fold and its helper closure.

FOLD-STEP-SCHEMA: helper contract equality compares parsed schemas, independent
of object or required-member ordering. The checker and runtime admission share
`fold_step_schemas`; extra, optional, nullable or differently typed step fields
refuse. Each invocation checks its exact input before running any helper node
and its complete output before it becomes state. Nested ordinary calls retain
the fold helper's boundary check. Non-fold helper admission is unchanged.
The checks charge materialization to shared fuel. No schema failure produces
an accumulator or effects.

## Execution correspondence and oracle scope

FOLD-SCHEDULE: inspect the node input under shared fuel, select the array, check
its size/items/unique canonical identities, then sort using metered insertion
sort. Each adjacent lexicographic comparison costs one unit per declared key.
Evaluate `initial` once in the original node scope, check canonical state size,
then invoke the existing helper for each sorted item. Validate each complete
output and canonicalize it before the next step. Empty input still validates
and returns the initial state. Output composition uses the existing transform.
Fuel is shared by selection, initial, sorting, helper graphs and value copies.

FOLD-ORACLES: the unchanged reference fixtures retain 1,008 FIFO histories and
648 moving-average histories; every average prefix checks allocation and
balance, including repricing, capped partial returns and final residuals.
Integer millionths use scale zero and half-away-from-zero division. CP1 checks
80 received, 60 allocated and one residual worth 20. The capacity declaration
checks 5/2 allocation and residual 3 under both enumerations.
Transfer correspondence retains 147 average and 245 cross-layer FIFO histories,
paired value conservation, nonreturnable transfer identities and dependent
reversal order. Declared guards compare live allocation cost/receipt identities
before admitting a replay with removed originals. These are pure derivation
fixtures; they do not implement application posting, a journal, authorization or
atomic multi-partition effects. Those remain the admitted effect bridge's work.
Values outside the core exact decimal range are refusal vectors, including the
retired u128-maximum examples; no wider numeric parity is claimed.

FOLD-REFUSALS: duplicate canonical identity, missing/null keys, excessive input
or canonical state, exhausted shared fuel, malformed helper contracts, a late
step schema failure, zero division and unsupported numeric functions return no
decision. Later success cannot refund or conceal an exhausted meter. Named
correspondence tests exercise these boundaries after earlier successful steps.
