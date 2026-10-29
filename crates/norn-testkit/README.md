# norn-testkit

Shared proptest-state-machine harness for Norn Deciders (T6-PSM-1, #2581). Test support only: take it as a dev-dependency (see `boundary.json`).

## What it checks

Implement `DeciderModel` for a `norn_kernel::Decider`: an independent, deliberately simpler reference model (a transition table over an abstract state) that predicts, per input, `Prediction::Accept { next, shape }` or `Prediction::Reject`, plus `project` from decider state to model state. `DeciderMachine<M>` adapts it to `proptest-state-machine`, and on every generated turn (input + `Observations` + `EnvelopeLimits`) asserts the kernel's Decider laws:

- `decide` and `step` are deterministic for identical state, input, and observations;
- `step` equals an independent oracle: historical world refused before `decide`, rejection → `StepError::Rejected`, envelope caps checked in order (events, commands, timers), `perform` only in the main world, then `evolve` over the new events; a refused turn has zero events/effects and leaves state untouched;
- `replay(ledger) == state` and `fold(replay(prefix), suffix) == replay(ledger)` after every turn;
- the decider refines the model: same verdict, same decision shape, `project(state) == model`.

World and envelope rules live in the harness, so a model only predicts domain behaviour.

## Running

```rust
use norn_testkit::{check, Settings, KERNEL_OUTCOMES};

#[test]
fn psm_my_decider() {
    let coverage = check::<MyModel>(&Settings::new(file!()));
    coverage.assert_covered(KERNEL_OUTCOMES);
    coverage.assert_covered(&["input:submit", "effect:perform"]);
}
```

- **Deterministic by default:** a fixed seed (`DEFAULT_SEED`) and `DEFAULT_CASES` (64) cases of 1–31 turns. `PROPTEST_CASES=<n>` raises the budget (CI soak), `PROPTEST_RNG_SEED=<n>` explores another seed.
- **Minimal counterexamples:** failures shrink to the shortest failing turn sequence; `check` panics with it, `run` returns it as `Failure`.
- **Regression files:** a failure is persisted to `<test stem>.proptest-regressions` beside the test source and replayed first on every later run. Commit it with the fix. `Settings::without_persistence` is only for tests that expect a failure (mutants).
- **Coverage:** `Coverage` counts `input:*`, `event:*`, `outcome:*`, `world:*`, and `effect:*` labels; assert the ones your suite must reach.

Worked example: `crates/norn-kernel/tests/psm_order_lifecycle.rs`. Harness self-tests (three caught mutants, shrinking, seed reproducibility): `tests/harness.rs`.

`norn-decisions` suites can use the same template by wrapping the evaluator in a test-local `Decider`; the harness itself never becomes a dependency of shipped code.
