# norn-decisions

`norn-decisions` is a ring 0 crate (pure, portable semantic core): the `zen-engine` JDM evaluator behind a Norn-owned, synchronous `decide(slice, event) -> { facts, commands, timers }` API (T3-DEC-2, #2670). Its dependency boundary is in `boundary.json` and enforced by `norn-skeleton-guard`.

- `AdmittedDecision::admit(root, sub_decisions, binding)` loads a `RuleIR` v2 `decide` graph and its exact sub-decision closure through an in-memory, immediately-ready loader, re-checking the Norn profile (no `functionNode`/`customNode`, no imports, no node-level schemas, closure bounds, closed schemas).
- `validate_slice` / `type_event` check the inputs against the decide input schema; `evaluate(&decision, &slice, &event, budget)` polls the engine once (suspension is `DecisionError::UnexpectedPending`), meters it, and validates the output against the declared output schema. Every error means zero effects. `norn-testkit`'s `decide_commit` test drives the output through `norn_kernel::commit_decision`.
- Inside the evaluator values are `norn_expr::Value` with exact decimals, never `f64`. `evaluate` returns `Evaluated { output: norn_ir::DecisionOutput, units_used }`; output payloads carry exact `norn_ir::RuntimeValue`, so decimals are never narrowed or rounded.
- Engine: the vendored zen crate set (`vendor/zen`, imported from `Norn-cloud/zen` at `ENGINE_TAG`; `crates/norn-skeleton-guard/zen-pin.json`), Norn profile `regex-lite` + `strict-errors` + `deterministic-maps` + `deterministic-temporal` + `metering` + `bounded-strings`, no default features. No `zen-*` type is public. JDM admission rechecks expression-source bounds and the bounded-string literal controls.

Tests: `cargo test -p norn-decisions` (the `zen_corpus` test is the blocking CI job `zen-corpus`); the crate builds for `wasm32-unknown-unknown` in the `wasm32` job.
