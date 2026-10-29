# norn-expr

`norn-expr` is a ring 0 crate (pure, portable semantic core) and Norn's only wrapper over ZEN Expression. Its dependency boundary is in `boundary.json` and enforced by `norn-skeleton-guard`.

It depends on `zen-expression` from the `Norn-cloud/zen` fork at the pinned tag recorded in `knowledge/architecture/build-plan-2026-09-09/zen-fork-ledger.md`. Upstream default features are off. The Norn profile enables `regex-lite`, `deterministic-maps`, `deterministic-temporal`, `metering`, and the default-off `bounded-strings` feature.

The public API is Norn-owned only, with no `zen-*` types (`public-api-boundary` CI job):

- `Expression::compile` parses and compiles a standard expression.
- `Expression::evaluate` evaluates it (inputs NFC-normalized, as Norn1 canonicalizes them) against a JSON-shaped `Value` under an explicit `EvaluationBudget` of deterministic operation units, returning an `Evaluation` (the result plus the units used) or a typed `ExprError`. Every evaluation is metered; there is no unmetered path. `EvaluationBudget::decide(profile)` (and `Default`, `norn.limits/1`) is the limit profile's per-decision fuel; running out is `ExprError::BudgetExhausted`. Copying the result out is charged too: 1 per value it holds, a string its length when longer, measured up to the remaining budget.
- `Number` is an exact decimal. A value the engine cannot represent exactly is rejected, never rounded.

The type environment and checker (T1-EXPR-2):

- `ZenTypeEnv::from_artifact_set` derives `norn.expr.env/1` from the decoded EntityIR v2 definitions of an artifact set (base layer included): every object type (with inherited and extension members) and interface becomes a closed record. Properties are typed fields (`decimal` keeps its scale and unit, `timestamp` its timezone policy, `enum` is a literal union, `nullable` anywhere on the `ref` chain makes the field optional). Links are typed references: `to: one` is the record (optional unless `required`), `to: many` a list; each inverse role appears on the target, its subtypes, and, for an interface target, every implementor, with multiplicity `cardinality.from`. `json-ext` is opaque. Link properties are not in `/1`. Derivation fails closed on unresolved names, cycles, and member collisions.
- Variables: `actor` is always bound to the closed actor operand (#2419): exactly `actorKind`, `subjectId`, `agentSessionId?`, `scope` (`orgId`, optional `tenantId`/`workspaceId`/`environmentId`/`businessUnitId`), `capability`, and the derived `roles` and `authMethod`. A site binds `self` (`bind_self`) and `input` (`bind_input`). Nothing else is ambient.
- Links expand to `EnvOptions::max_link_depth`, by default the `norn.limits/1` slice relation depth (3); a deeper link is not a member.
- The environment is content-addressed (Norn1 canonical JSON, SHA-256): `ZenTypeEnv::reference()` is what a checker receipt records. The golden is `tests/golden/retail_env.json`.
- `check_expression(expr, env, expected)` admits the `norn-expr/4` structural grammar plus the temporal and bounded-string forms in profile `norn-zen/6`, within structural and cost bounds under `norn.limits/1`; `check_expression_with(expr, env, expected, limits)` also returns the expression's static `CostEstimate`. Norn types the admitted grammar itself and then runs zen's strict analysis as a cross-check. Diagnostics carry stable codes (`norn_expr::check::codes::ALL`) and spans. Wire dates and timestamps retain distinct inferred types; parsed temporal Dynamics project to canonical JSON strings. `temporal::validate_date` validates EntityIR dates and `temporal::canonical_timestamp` produces the stored UTC timestamp form.
- Unary tests (T1-EXPR-3b): `UnaryTest::compile` compiles a decision-table input cell (the empty cell is the wildcard, which the decision engine skips) and `UnaryTest::evaluate(context, subject, budget)` evaluates it in zen's unary mode with the column value as `$`. `check_unary(test, env, subject_type)` (and `check_unary_with`, which returns the `CostEstimate`) checks it against the column's declared type under the same allowlist; see the unary-test row below.

## Profile `norn-zen/6` (structural grammar `norn-expr/4`)

The published, versioned profile is [`PROFILE.md`](PROFILE.md) (`norn-zen/6`, bounded string normalization follow-up to #3116): the structural grammar, engine pin, and error, ordering, numeric, temporal, and string contracts. `CHECKER_PROFILE` is `norn-zen/6`; `tests/profile_conformance.rs` is its conformance suite. The fork enables `deterministic-temporal` and `bounded-strings` alongside the explicit no-default-features profile. The qualified source pin is `norn-v2.0.1-12`; its C12 zip 9 archive-loader update preserves the `norn-zen/6` semantics and explicit features.

### Grammar `norn-expr/4` with the `norn-zen/6` temporal and bounded-string extensions

An allowlist: a small positive grammar in which unsupported nodes and builtins are refused, mostly with `expr.construct.unsupported`. Invalid dynamic temporal text and fractional or overflowing temporal amounts can return typed evaluation errors. Generated soundness tests (`tests/check.rs`) check admitted standard expressions and unary tests against conforming values. `tests/corpus.rs` runs the fork's own zen-expression corpus (`tests/fixtures/zen_corpus.json`): every admitted row must reproduce the corpus output, and the verdict of every row, including the rejection code of every excluded construct, is pinned in `tests/golden/zen_corpus_verdicts.txt` (`NORN_BLESS=1` regenerates it).

| construct | admitted form |
| --- | --- |
| literals | `null`, booleans, exact numbers, strings. List and record literals whose elements all have exactly the same shape. An empty list `[]` needs an expected list type (from the site, the other `?:` branch, or the `??` left side); otherwise it is rejected (`expr.type.untyped_empty_list`) |
| variables | `self`, `actor`, `input` as bound; `$`, the subject, in a unary test only; `#` (the innermost closure's element) and `as` aliases inside an admitted closure |
| member reads | `record.field` or `record['field']` with a literal name, on a non-null record. A required to-one link reads as the record. An optional to-one link or a nullable property is a nullable read. A to-many link or inverse is a list. `json-ext` is opaque (`expr.read.opaque`). Links traverse to the link depth |
| nullable values | only as the left of `x ?? fallback` (fallback of the same non-null type; the result is non-null), in `x == null` / `x != null` (on a non-nullable value these are `expr.type.constant_comparison`), as a `?:` branch, a record-literal field, or the whole result under an optional expected type. Anywhere else: `expr.type.nullable` |
| index | `[a, b, c][i]` with `i` an integer literal below the list literal's length. Any other index, a string key into a list (`[1]['0']`) included, is `expr.read.index`: zen reads `null` out of bounds and fails on a negative or fractional index. zen's strict analysis types every index read as nullable, so the cross-check rejects an index result used as an operand |
| arithmetic | `+ - * /` and unary `-`/`+` on numbers; `+` on two strings concatenates. Division by zero and decimal overflow are typed runtime errors, owned by T1-EXPR-6's numeric contract |
| comparison | `==`/`!=` on two non-null scalars of one type. Strings and enum values count as one type; same-kind raw temporal properties compare for equality, while parsed temporal values compare semantically. Comparing disjoint literal sets is always-constant and rejected. `< <= > >=` on numbers or same-kind parsed date/timestamp values only |
| membership | `x in list` / `x not in list` with `x` a non-null bool, number, or string and `list` a non-null list of that scalar kind; disjoint literal sets and `[]` are always-constant (`expr.type.constant_comparison`). `n in [a..b]` (any bracket pair) with numbers `n`, `a`, `b`. A range is admitted only as the right side of `in` |
| logic | `and`, `or`, `not` on booleans; `c ? a : b` with branches of one type |
| builtins | `len(string or list)`; `split(string, "")` (Unicode scalars plus empty ends); `lower`/`upper`/`trim(string)`; `contains`/`startsWith`/`endsWith(string, string)`; `tokens(text, 'alnum', maxCount, maxLength)` (integer literals 0..=256); `take(text, maxScalars)` (integer literal 0..=16384); `join(list of strings, separator)` (literal separator ≤256 bytes, output ≤16 KiB); `abs`/`floor`/`ceil(number)`; `round`/`trunc(number[, p])` with `p` an integer literal 0..=28; `sum(list of numbers)`; `date(text)`, `timestamp(text)`, `days_between(date, date)`, `add_days(date, integer)`, `seconds_between(timestamp, timestamp)`, `add_seconds(timestamp, integer)` |
| closures | `some`/`all`/`none`/`one`/`filter`/`count(list, bool body)`, `map(list, body)` with a non-null body, and `flatMap(list, body)` with a non-null list body (zen splices one level, so the result has the body's element type). Bodies use the same `norn-zen/6` checker profile |
| unary tests | parts joined by `,` (`or`), `or`, or `and`. zen desugars each part: one starting with a comparison operator compares `$` (`> 5` is `$ > 5`); a list, range, `map`, or `filter` is a membership test (`[1..5]` is `$ in [1..5]`); a boolean-valued part (`$ > 5`, `contains($, 'x')`, `some($, # > 1)`) is used as is; any other value is an equality test (`'gold'` is `$ == 'gold'`). Each desugared part must check as a non-null `bool` under the rules above, so a nullable subject needs `$ == null`, `$ != null`, or `$ ?? fallback`, and a value outside an enum subject is `expr.type.constant_comparison`. Only the empty cell is the wildcard |
| result | representable as a Norn `Value`: wire temporal values are typed JSON strings; checked temporal Dynamic values project to canonical JSON strings; intervals are not admitted |

Not in `norn-zen/6`:
- user-defined functions and recursion
- string templates, slices, indexing other than the in-bounds literal form, dynamic reads
- ambient clock forms (`d()`, `now`, `today`, `yesterday`), legacy `d(...)` constructors, date methods/ranges, and a range anywhere but the right of `in`
- conversions (`string`/`number`/`bool`)
- `merge`/`mergeDeep`/`values`/`keys`
- regex
- aggregates over possibly-empty lists other than `sum`/`count`
- `in` on a record (key test), `%`, `^`
- assignments, `$root`, `$` outside a unary test, `rand`, and every other builtin

Date and timestamp fields preserve semantic kinds across optional, list, record, and conditional results. `date(text)` and `timestamp(text)` explicitly parse a matching wire property or ordinary string and return distinct parsed kinds; raw/parsed mixtures and mixed date/timestamp comparisons are rejected. Parsed values support same-kind equality and all four ordering operators; date/time arithmetic uses the six pure S5 builtins. Constructor literals are validated during checking, while invalid dynamic text, fractional dynamic amounts, and checked range overflow produce typed fatal evaluation errors. Values project to canonical JSON strings. The shared cell decoder canonicalizes valid stored timestamps before authorization, rule inputs, and guards; write admission canonicalizes new values before guards and persistence; temporal property invariants validate values, and invalid observations remain source evidence for findings. Entity/invariant expression sites run this type checker. JDM graph expression admission remains syntax-only (T3-DEC-3), so malformed temporal operands can fail at S5 runtime with typed errors and zero effects; this adoption does not add JDM graph type-checking.

## Bounds (T1-EXPR-4, #2522)

Non-recursive is not bounded (zen adoption review F3). An admitted expression is also bounded:

| bound | value | code |
| --- | --- | --- |
| source length, checked before parsing | 16 KiB | `expr.bound.source` |
| bracket nesting (before parsing) and AST depth | 128 | `expr.bound.depth` |
| recursion-opening characters (before parsing): every bracket (closing ones too), member dot, operator character, and `and`/`or`/`not`/`in` (in unary tests every comma), counted statelessly over the raw source, literals included, never decremented: an upper bound on zen's recursion depth (the in-fork depth guard is #2875) | 512 | `expr.bound.depth` |
| AST nodes | 4,096 | `expr.bound.nodes` |
| string literal | 4,096 bytes | `expr.bound.literal` |
| list literal elements, record literal entries | 1,024 | `expr.bound.literal` |
| closure nesting | the limit profile's `slice.max_relation_depth` (3 in `norn.limits/1`) | `expr.bound.closure_depth` |
| static worst-case cost of one evaluation | the limit profile's `decide.fuel_units` (1,000,000 in `norn.limits/1`) | `expr.cost.exceeds_budget` |

The structural bounds (`StructuralLimits::NORN_EXPR_4`) belong to the language profile. The source bounds are checked before zen parses (by `Expression::compile`, `UnaryTest::compile`, and `check_unary_syntax` too): zen's parser and compiler recurse once per bracket and per operator of a left-deep chain (`a.a.a...`, `a[0][0]...`, `1 + 1 + ...`, `- - x`). Every AST level is opened by one such token, so their total bounds that recursion whatever the construct. The AST bounds are checked by an iterative pass before the recursive checker. A regression test runs the worst admitted shapes on a 2 MiB thread.

The static cost estimate (`src/check/cost.rs`) is a proved upper bound on the zen fork's metering units for one evaluation on a **conforming slice**: the context conforms to the type environment, its canonical JSON is at most `slice.max_canonical_bytes` (256 KiB), and every list of records holds at most `slice.max_records` (1,000) records. The kernel enforces both while reading the slice. The evaluator reads its inputs in NFC, the form Norn1 canonicalizes strings to, so no string it sees is longer than its canonical bytes. The estimate follows zen's compiled opcodes node by node and charges each data-proportional opcode (`==`/`in`, string `+`, builtin arguments, loop setup, `flatMap`'s flatten) by the size bound of its operands. A closure body is multiplied by the iterated list's cardinality bound: a to-many link holds at most the record cap, another slice list at most what fits in the byte cap, and a literal list its length. So nested iteration multiplies. Strings and lists read from the slice are charged symbolically by their canonical path (`self.lines[*].sku`), because disjoint positions of the slice add up to at most the byte cap: `some(self.lines, #.sku == 'x')` reads each `sku` once, not 1,000 times 256 KiB. `upper` and `lower` may grow text 3x and 1.5x. The generated soundness tests evaluate every admitted expression and unary test under a budget of exactly its estimate, including on max-cardinality slices (1,000 records, near-256-KiB strings and lists).

Under the placeholder `norn.limits/1` numbers (1,000,000 units for a 256 KiB slice, about 4 units per byte), a closure over a scalar list read from the slice (for example `all(actor.roles, # != 'guest')`, where 256 KiB can hold 87,381 strings) or two nested closures over to-many links (1,000 x 1,000) is rejected. Calibrating those numbers is T3-BUDGET-1 (#2775).

History: `norn-expr/1` (#2852) was this table without string `+`, `in`/`not in`, ranges under `in`, and literal indexing; T1-EXPR-3a (#2520) added those as `norn-expr/2`. T1-EXPR-3b (#2521) added unary tests (`$`), `one`, and `flatMap`, and made a string key into a list an index read, as `norn-expr/3`. T1-EXPR-4 (#2522) added the structural and cost bounds as `norn-expr/4`; the grammar is unchanged.

The checker, runtime wrapper, static cost estimator, versioned `norn-zen/6` profile, and profile conformance suite are implemented. Compilation alone never implies admission; every typed call site must run the checker for its declared type environment and expected result. Syntax-only JDM admission separately enforces bounded-string literal controls and structural source bounds.

Tests: `cargo test -p norn-expr` runs natively. For `wasm32-unknown-unknown`, the CI `wasm32` job builds the crate, and the fork's CI runs the metering unit-count tests on wasm32.

<!-- profile:start -->
# Norn ZEN Expression / JDM profile `norn-zen/6`

This is the versioned contract for every ZEN Expression and every JDM decision that Norn admits (T1-EXPR-6, #2766). It is the value of `norn_expr::check::CHECKER_PROFILE`, and `norn-checker` records it in every check receipt (`exprProfile`) beside the type-environment hash. `norn-decisions` records it in each decision's `DecisionBinding`.

**Changing any rule below is a new profile id.** This includes the admitted grammar, a bound, the numeric or error semantics, and any semantic change from an engine update. Qualified compatibility-only source updates retain the profile id and re-pin this document’s hash. `tests/profile_conformance.rs` pins this file's hash together with the id, so a rule change that does not bump the id fails CI.

`norn-zen/6` (2026-10-09, #3116) adds the default-off fork feature `bounded-strings`: bounded ASCII alphanumeric token runs, Unicode-scalar prefixes, and bounded string joins. The syntax-only decision loader enforces all literal controls as well as the typed checker. The S7 release was qualified at `norn-v2.0.1-11`, with native/wasm32 bounded-string and metering parity validated on Boat before publication. The current `norn-v2.0.1-12` source adds C12 zip 9 archive-loader compatibility with no change to this profile’s features or semantics.

`norn-zen/5` (2026-10-06, temporal comparison follow-up to #3112) adopts ZEN fork S5 deterministic temporal ordering and pure date/time arithmetic. Entity temporal properties remain typed JSON strings with same-kind wire equality; expressions explicitly parse them with `date(text)` or `timestamp(text)` to use semantic equality and ordering. Six pure temporal builtins provide checked calendar-day and instant arithmetic. Invalid dynamic text and non-integer dynamic amounts fail with typed runtime errors.

`norn-zen/4` (2026-10-05, #3112) admitted semantic date and timestamp property values as typed JSON strings with same-kind equality. Profile `/5` preserves that wire contract and adds explicit parsing, parsed-value comparison and checked arithmetic.

`norn-zen/3` (2026-10-02, #3055) admits only `split(text, '')`, reusing the pinned engine's Unicode-scalar splitting for schema-compatible character counts. `len(split(text, '')) - 2` counts Unicode scalar values in NFC input (the two empty end entries are removed). Its result bound is the input byte bound plus two entries, each at most four bytes. The engine pin and `len` semantics are unchanged.

`norn-zen/2` (2026-10-02, #2770) supersedes `norn-zen/1`: decision outputs are exact runtime values (an output decimal that `f64` cannot hold, which `/1` refused, is now carried exactly), and numeric output defaults are admitted from the artifact's exact source text. Grammar, bounds, arithmetic, and the engine pin are unchanged.

Enforcement is Norn's: the `norn-expr` checker (T2 admission), the `norn-checker` JDM profile, and `norn-decisions` admission. The `Norn-cloud/zen` fork carries only engine-internal patches (D32).

## Engine

- **Fork pin:** `Norn-cloud/zen` at the tag in `crates/norn-skeleton-guard/zen-pin.json`. The current pin is `norn-v2.0.1-12`.
- **Engine profile:** default features off, plus `regex-lite`, `strict-errors`, `deterministic-maps`, `deterministic-temporal`, `metering`, and `bounded-strings` on `zen-engine`, `zen-expression`, and `zen-tmpl`. There is no JavaScript, HTTP, JSON Schema, swc, or OS randomness. An engine update that changes these semantics requires a new profile id; the qualified C12 compatibility update preserves `norn-zen/6`.

## Grammar and builtins

The admitted grammar is the checker's `norn-zen/6` allowlist (`src/check.rs`, module docs), built on the `norn-expr/4` structural grammar with nullable quotient typing and semantic temporal values. It is a positive grammar; invalid dynamic temporal text and fractional or overflowing temporal amounts can fail with typed evaluation errors. Everything else is `expr.construct.unsupported`.

**Admitted builtins:**
- `len` (UTF-8 bytes for a string, element count for a list)
- `split(text, '')` (only an empty string literal separator; Unicode scalar values with an empty entry at each end)
- `lower`, `upper`, `trim`
- `contains`, `startsWith`, `endsWith`
- `tokens(text, 'alnum', maxCount, maxLength)`: returns the first `maxCount` maximal ASCII `[A-Za-z0-9]+` runs, truncating each at `maxLength` bytes; both caps are integer literals in `0..=256`. Zero for either cap yields `[]`. Every non-ASCII scalar and every other non-alphanumeric character is a separator; case is preserved, and a long run is truncated rather than split into additional tokens. The builtin does not normalize or case-fold. Runtime values converted from Norn `Value`s are NFC-normalized before ZEN evaluation, while expression literals retain their authored spelling unless the authoring or artifact-canonicalization path normalized them. Callers that need normalization-independent tokenization should NFC-normalize literal text before authoring it.
- `take(text, maxScalars)`: returns a UTF-8-safe prefix measured in Unicode scalar values; the cap is an integer literal in `0..=16384`. A scalar boundary can split a grapheme cluster, including a base letter from its combining mark, a ZWJ emoji sequence, or a flag; this API does not promise grapheme-cluster boundaries. Its maximum result is 65,536 UTF-8 bytes, independent of `join`'s separate 16,384-byte cap.
- `join(strings, separator)`: joins a list of strings using a literal separator no longer than 256 UTF-8 bytes; output up to and including 16,384 UTF-8 bytes is allowed, and larger output is a typed runtime error. An empty list returns `""`; non-string elements fail at runtime.
- `abs`, `floor`, `ceil`
- `round` and `trunc`, with an integer-literal precision 0..=28
- `sum` (a list of numbers)
- `date(text)` and `timestamp(text)` (explicitly parse a string or matching typed temporal property)
- `days_between(date, date)`, `add_days(date, integer)`
- `seconds_between(timestamp, timestamp)`, `add_seconds(timestamp, integer)`
- the closures `some`, `all`, `none`, `one`, `filter`, `count`, `map`, `flatMap`

**Never admitted:**
- `rand`
- every ambient clock form: `d()`, `now`, `today`, `yesterday`; legacy `d(...)`, date methods, and date ranges
- conversions: `string`, `number`, `bool`
- `keys`, `values`, `merge`, `mergeDeep`
- regex: `matches`, `extract`
- `max`, `min`, `avg`, `median`, `mode`
- `%` and `^`
- templates, slices, assignments, `$root`
- every other builtin

Bounded string controls are source literals even at syntax-only JDM sites; dynamic modes, caps, or separators are rejected during admission. The fork's `tokens` mode is checked at runtime as well. `join` accepts only a statically proven list of ordinary strings in typed expression sites. The fork meters scan/copy work before execution; `norn-expr`'s cost model includes these precharges and the result traversal. Norn converts runtime `Value` strings and keys to NFC before evaluation, but this does not imply that source literals are normalized: their spelling depends on the authoring or artifact-canonicalization path.

**JDM nodes** (enforced by the `norn-checker` JDM profile and re-checked by `norn-decisions`):
- Admitted: `inputNode`, `outputNode`, `expressionNode`, `decisionTableNode` (`first`/`collect`), `switchNode` (`first`/`collect`), and `decisionNode` (a bounded sub-decision closure).
- Rejected: `functionNode` and `customNode`, including custom-node template fields, which are never decoded (`rule.validation.jdm_custom_node_rejected`, D33 v1). Graph imports and node-level schemas are also rejected.

## Types

- Typed entity/invariant expression sites use the closed environment (`norn.expr.env/1`). There is no residual `Any` at any intermediate site and no dynamic read at those sites. JDM graph expression admission remains syntax-only until T3-DEC-3 (see the temporal boundary below).
  - `o[k]` with a non-literal `k` is `expr.read.dynamic`.
  - `o['x'] - 1` with `o.x: text` is `expr.type.error`.
- Nullable values are admitted only in the documented positions (`??`, `== null`, a branch, a record field, an optional result).
- Results are representable as a Norn `Value`. Entity temporal properties remain typed JSON strings. Constructor/arithmetic results are checked ZEN temporal `Dynamic` values internally and project to canonical JSON strings. Intervals are not admitted.
- `InferredType::Date` and `InferredType::Timestamp` preserve their kinds recursively in optionals, lists, records, and conditionals. They may be projected to unformatted JSON string outputs, but cannot be compared with ordinary strings or passed to string builtins. Date literals are not implicitly converted to dates.
- `date(text)` parses exactly `YYYY-MM-DD` in years `0001..9999`; `timestamp(text)` parses the fork's known-offset RFC 3339 subset and normalizes to UTC. Both accept a matching typed temporal property or ordinary text. Constructor results have parsed kinds distinct from wire properties, so raw/parsed comparisons and mixed date/timestamp values are rejected. Invalid string literals are rejected during checking; invalid dynamic text is a typed evaluation error.
- `days_between(start, end)` returns signed calendar days as `end - start`; `add_days(date, integer)` adds signed days. `seconds_between(start, end)` returns exact signed decimal seconds including nanoseconds; `add_seconds(timestamp, integer)` adds signed seconds while preserving the fraction. Additions use checked arithmetic and reject overflow or results outside years `0001..9999`.
- A date is exactly `YYYY-MM-DD`, four ASCII year digits, and a real proleptic-Gregorian calendar day; year `0000` is admitted. A timestamp is the supported RFC 3339 subset with uppercase `T`/`Z`, an explicit known offset, seconds 00–59, and up to nine fractional digits. Lowercase separators, offset-less values, `-00:00`, leap seconds, and greater precision are refused.
- This preserves the wire/parser boundary: the existing EntityIR date and timestamp validators may accept year `0000`, but S5 `date(...)` and `timestamp(...)` constructors reject year `0000`, including on a typed wire value. Such a value therefore remains valid wire data but cannot be converted to an S5 parsed temporal.
- The shared cell decoder canonicalizes valid stored timestamps from either timezone policy to UTC against the effective property contract before any value consumer runs; write admission canonicalizes new values before guards and persistence. Shared temporal property invariants validate date/timestamp values; invalid observed strings remain source evidence for finding validation, and write paths refuse invalid values. UTC rollover outside years `0000`–`9999` is refused. Consequently admitted expressions compare stored canonical timestamp instants.

## Errors

- **Typed and fatal:** a failure is a typed error and never a silent value.
  - Expressions fail with `ExprError` (`Evaluation`, `BudgetExhausted`, `InvalidNumber`, `UnsupportedResult`).
  - Decisions fail with `DecisionError`.
- **Zero effects:** a failing decision has zero effects.
- **Strict tables and switches:** a decision-table cell or switch condition that fails, or is not a boolean, aborts the decision (`strict-errors`). It never falls through to another row or branch.
- **Temporal errors:** invalid dynamic input to `date`/`timestamp`, fractional dynamic arithmetic amounts, integer conversion overflow, checked arithmetic overflow, and result years outside `0001..9999` are typed fatal `ExprError::Evaluation`s. Static invalid temporal literals and fractional literal amounts are refused by the checker. Budget exhaustion remains typed and fatal.

## Ordering

- Lists keep their order in every admitted operation, and closures iterate in list order.
- `==`/`!=` admit same-kind wire-property equality and same-kind parsed temporal equality. Parsed timestamps compare normalized instants; parsed dates compare calendar days.
- `<`, `<=`, `>`, and `>=` admit numbers and same-kind parsed dates or timestamps only. Raw temporal properties, raw/parsed pairs, mixed date/timestamp values, and strings are refused; Norn does not compare temporal text lexicographically.
- Map-iteration results (`keys`, `values`, `merge`) are not admitted.
- Engine maps are insertion-ordered at every size (`deterministic-maps`). Norn values are key-ordered records, so an evaluation's result never depends on hash seeds.

## Numbers

- **Representation:** exact decimals. A value or literal is the engine's 96-bit decimal, scale ≤ 28. Literals use plain decimal text; scientific notation is outside this profile, including exact exponent forms. Admission validates the engine lexer's original numeric token spans with Norn's exact `Number` parser, so scientific mantissa rounding cannot become an admitted value. An unrepresentable input value is `InvalidNumber`; an unsupported literal is an admission refusal, never rounded. Numbers never pass through `f64` or serde inside the evaluator. `9007199254740993` round-trips exactly.
- **Arithmetic:** `+`, `-`, `*`, `/` use the pinned engine's checked 96-bit decimal arithmetic. Results may reduce scale to fit (at most 28 fractional digits), including multiplication underflow to zero; arithmetic precision reduction uses ties to even, distinct from explicit `round`. Fixtures pin `0.1 + 0.2`, multiplication at the scale boundary, and `1 / 3`.
- **Rounding:** `round(x, p)` rounds half away from zero, and `trunc` truncates. The mode is pinned by fixtures.
- **Division:** division by zero or quotient overflow returns null at this pin. A quotient is nullable unless the divisor is a numeric literal with absolute value at least one. A required numeric result must handle a nullable quotient, for example `(a / b) ?? 0`. Fixtures pin both zero and quotient overflow.
- **Errors:** overflow in `+`, `-`, `*` and `sum` is a typed evaluation error.
- **Wire:**
  - Inside the kernel boundary, numbers travel as exact decimal text: Norn1 canonical JSON, via `value_from_canonical` / `value_to_canonical`.
  - The canonical `norn_ir::DecisionOutput` carries exact runtime values (`norn_ir::RuntimeValue`, #2770): an output decimal is never narrowed or rounded. The same exact values carry facts, the cell store, ledger events, commands and timers, and the dev host's HTTP JSON (exact number tokens). Definition IR keeps `f64` `JsonValue` and its hashes (D37).
  - Runtime decimals are this profile's numbers: 96-bit mantissa, scale ≤ 28, refused (never rounded) outside. Canonical form and hashing are Norn1's number form (`1.50` → `1.5`, `-0` → `0`); the golden corpus is `crates/norn-ir/tests/golden/runtime-decimals.json`, and the checker ABI's `canonicalRuntimeValue` is byte-identical natively and on `wasm32` (CI `checker-wasm`).
  - Numeric RuleIR output `defaults` are admitted from the artifact's exact source text (`AdmittedDecision::admit_with_defaults`; `norn dev` passes it); without the source text they are refused, because IR JSON is `f64`. EntityIR numeric property defaults are exact decimal strings (the checker refuses JSON numbers there).

## Temporal validation and fork boundary

- No host clock, host timezone, or zone database is admitted. `date`/`timestamp` parse explicitly using the fork's pure deterministic implementation. Valid entity timestamps are canonicalized before guards run; shared temporal property invariants validate stored values, while invalid observations remain source evidence for findings. Typed entity-expression sites receive invariant-conforming committed JSON strings.
- Timestamp parsing uses the already pinned `chrono` 0.4.45 crate with default features disabled and `std` only. No clock, locale, or timezone database is consulted.
- Both timestamp timezone policies store the same canonical UTC instant. Explicit offsets are required; `-00:00` is rejected because it denotes an unknown offset. Leap seconds and precision beyond nanoseconds are refused rather than discarded. Normalization outside years `0000`–`9999` is rejected.
- The pinned ZEN fork's S5 `Compare` opcode supports numbers and same-kind Dynamic temporal pairs; ordinary strings are never parsed implicitly. The checker keeps wire and parsed kinds separate, and the public Norn boundary converts only registered `Temporal` Dynamics into canonical strings.
- `norn-expr` typed entity/invariant sites run this checker and reject raw-wire or mixed temporal ordering at admission. JDM graph expression admission remains syntax-only (T3-DEC-3): malformed temporal operands may therefore fail at S5 runtime with a typed error and zero effects; this adoption does not add JDM graph type-checking.

## Regex and string bounds

- No regex builtin is admitted. If one is ever added, it uses the pinned `regex-lite` backend (ASCII classes, no `\p{..}`, `O(m * n)`).
- Strings are bounded:
  - A source is at most 16 KiB.
  - A string literal is at most the structural literal bound (`StructuralLimits::NORN_EXPR_4`).
  - Slice strings are bounded by the limit profile's slice byte cap.
  - String `+` and string builtins are metered by the size of their operands.

## Bounds and metering

Structural bounds (`StructuralLimits::NORN_EXPR_4`) and the static cost bound against the decide fuel of `norn.limits/1` are part of the profile (T1-EXPR-4). Evaluation is always metered. Temporal parsing pays the input text's shallow byte length plus the call opcode; each temporal arithmetic builtin pays argument shallow sizes, `cost::TEMPORAL_ARITHMETIC = 8`, and the opcode. Temporal result copy bounds use canonical string lengths (10 bytes for dates and at most 30 for timestamps).

## Conformance

`cargo test -p norn-expr --test profile_conformance` has one positive and one negative fixture per rule section above, plus the mandatory counterexamples of the zen adoption review (§3, F1/F2/F5):
- `o[k] - 1`
- `keys(o)` over a 40-key object
- `rand` and the ambient-now forms
- `9007199254740993`

Three other suites also cover the profile:
- `tests/check.rs` (per-rule checker fixtures)
- `tests/corpus.rs` (the fork's whole expression corpus, with verdicts)
- `norn-decisions`' `zen_corpus` and `dmn_tck` gates
<!-- profile:end -->
