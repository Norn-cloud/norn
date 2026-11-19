# Verification

Formal models and proofs for parts of Norn, and the scripts that check them. This page says what is here, how the checks run, where they run, and what the last recorded runs found.

## Map

| Path | What it holds |
|---|---|
| `coverage-v1/` | The coverage manifest (`manifest/`), the promoted gate list (`promoted-gates.json`), trusted assumptions, the result schema, and the validators (`validate.py`, `validate_result.py`, `negative_canaries.py`) |
| `stripmine-2026-08-22/` | Quint models (`models/`), Lean, Kani and F* proofs (`proofs/`), and the obligation registry (`obligations.json`) for authority, artifact governance, authorization and quantity ledger rules |
| `kernel-l1/` | Lean reference for the kernel, Kani harnesses, and the Verus version spike (`version_verus.rs`) |
| `kernel-proof-v1/` | Lean proofs over Aeneas extractions of kernel commit, binding and decider code |
| `kernel-v2/` | Quint models of commit admission, quantity ledger and projection trust, their ITF traces (`traces/`) and `results.json` |
| `semantics-v1/` | Shared Lean semantics (`NornSem`) and checker proofs, with pinned statements and axioms |
| `exact-arith-v1/` | Lean proofs over Aeneas extractions of the exact decimal, rational and ledger arithmetic |
| `authz-v1/`, `ontology-v1/`, `linear-conservation-v1/`, `query-compute-v1/`, `specialisation-v1/`, `quantity-batch-atomicity-v1/`, `quantity-unit-dimension-v1/`, `core-physics-v1/` | Lean proofs and Quint models for one subsystem each |
| `cell-protocol-v1/` | Hand-written Quint models of cell protocols, TLC liveness harnesses, traces and `results.json`. Not a promoted gate. |
| `canonical-values-v1/`, `cell-profile-v1/`, `ordered-fold-v1/`, `parameters/`, `values-v1/` | Kani harnesses and contracts. Crates include some of them under `#[cfg(kani)]`. |
| `ontology/` | Ontology packs, registry and their validator |
| `atomic-partitions-v1/`, `definition-catalog-v1/`, `posting-v1/` | Written contracts and specs |
| `profiles/` | An input profile used by the checker's generated Quint obligation tests (`norn-checker/src/obligations/quint/driver.rs`) |
| `archive/` | Retired material |

## Running the checks

`scripts/verify-promoted-subsystems.sh [fast|full]` runs every promoted gate in order. Each gate is one `run_gate` line with a profile, an id and a command.

1. Install the pinned tools: `scripts/bootstrap-verification-tools.sh` (Lean, Quint 0.32.0, Apalache 0.56.1, Kani 0.67.0, Z3, Java), `scripts/bootstrap-ontology-tools.sh` and `scripts/bootstrap-rust-extraction-tools.sh`.
2. Run `scripts/verify-promoted-subsystems.sh full`. This is what `scripts/ci/core.sh formal` does.

- **`fast`** runs only the gates marked `fast`: Quint, Lean, tool-version, registry and ontology checks. It writes no result record.
- **`full`** runs every gate, including Kani, the Aeneas correspondence gates and the F* extraction. It requires the tools under `target/verification-tools`, writes `target/verification-results/formal-results.json`, and checks that file with `coverage-v1/validate_result.py`.

Each gate's log is `target/verification-results/logs/<gate-id>.log`.

What the gates use:

| Tool | Gates |
|---|---|
| Lean | `*-lean` gates, plus the proofs inside the Aeneas gates |
| Quint + Apalache | `*-quint` gates (bounded model checks, mostly `--max-steps 8`) and `*-quint-typecheck` |
| Charon + Aeneas | nine `*-correspondence` gates. Each extracts production Rust and proves the Lean translation against a spec (`scripts/verify-*-aeneas.sh`). |
| Kani | `quantity-kani` (`stripmine-2026-08-22/proofs/kani/quantity_ledger_harness.rs`) |
| F* (via hax) | `artifact-governance-rust-extraction` (`scripts/verify-artifact-governance-extraction.sh`) |
| Verus | none; see [The Verus spike](#the-verus-spike) |

## Quint/Apalache execution policy

Every `quint verify --backend apalache` gate runs through `scripts/quint_apalache.py`. `verify-promoted-subsystems.sh` wraps it automatically; `quint typecheck` gates run directly.

- **One server per check.** The harness starts its own pinned Apalache 0.56.1 server on a fresh loopback port free on both IPv4 and IPv6, waits up to 120 s for it to accept connections, points Quint at it with `--server-endpoint`, then kills the server's process group and waits up to 20 s for the port to be released. Servers are not shared: sequential checks against one reused server hung with the request accepted but never started (#2830).
- **Pinned launcher and JVM.** The JVM runs with fixed heap, processor-count and GC flags. The environment variables that could swap the JAR or change the checker's configuration (`APALACHE_JAR`, `CONFIG_FILE`, `JVM_ARGS`, `JVM_GC_ARGS`, `JAVA_TOOL_OPTIONS`, `JDK_JAVA_OPTIONS`, `_JAVA_OPTIONS`, `OUT_DIR`, `RUN_DIR`, `SMT_ENCODING`, `WRITE_INTERMEDIATE`) are removed, both by the harness and by `verify-promoted-subsystems.sh`.
- **Timeout per attempt.** 300 s by default. On timeout the Quint process group is killed and the gate fails. A timeout of a check the server had started is never retried.
- **Check-start watchdog.** If the server has not begun any check 60 s after Quint started and Quint has printed nothing, the request was lost before model checking. The attempt is killed as a server stall.
- **Lost-response watchdog.** If the server logged an outcome but Quint has not exited 60 s later, the response was lost. A lost `NoError` is retried; any other lost outcome is final.
- **Verdict first.** After each attempt the harness reads the server's logged outcome and Quint's `--out` result before deciding anything. A non-`NoError` outcome, a Quint `violation` status, any diagnostics, or a counterexample marker is final, whatever transport error Quint also printed. A retry can never turn a verdict into a pass.
- **At most one retry** (two attempts), and only when neither side produced a verdict: a server startup, connection or deadline error, a server stall, a lost `NoError`, or a missing result that the server confirms had no verdict. A startup or connection retry reuses the same server if it is still alive and has run no check; any other retry gets a fresh server. A server may be started at most three times.
- **Logged.** Both attempts are logged, with the reason for the retry.
- **Budget.** `coverage-v1/validate.py` (`validate_timeout_budgets`) checks that, for each profile, the slowest per-attempt timeout times the number of attempts, plus the profile's nominal run time, fits the workflow job's timeout, and that the fast profile's worst case stays within 30 minutes. Raising a timeout without updating the budgets fails the `coverage-manifest` gate.

## Where the formal checks run

**The formal checks do not run in GitHub CI.** CI runs `scripts/ci/core.sh` phase by phase and leaves out the `formal` phase: `core.sh plan` drops it ("formal remains a Boat gate"), and `.github/workflows/ci-policy.yml` says CI runs every phase "but formal". No workflow calls `verify-promoted-subsystems.sh`. So Lean, Quint/Apalache, Kani, Aeneas and F* are run by hand on Boat sandboxes, and the results are recorded in the pull request.

What CI does run, on every plan:

- In `cheap`: `scripts/proof_enrollment.py` and its tests (registry checks over the proof sources), and `coverage-v1/negative_canaries.py --workflow-contract`. These check the registries, not the proofs.
- In the test shards: `crates/norn-testkit/tests/kernel_quint.rs` replays the Quint ITF traces committed in `kernel-v2/traces/` against the Rust kernel. It reads the traces; it does not run Quint.

## Last recorded results

No complete `verify-promoted-subsystems.sh full` pass is recorded. The records are per-gate runs on Boat, in pull request bodies:

| Checks | Result | Commit | Source |
|---|---|---|---|
| `verify-kernel-decider-aeneas.sh`, `verify-kernel-binding-aeneas.sh`, `verify-checker-certificates-aeneas.sh`, `verify-norm-precedence-aeneas.sh`, `verify-semantics-lean.sh`, `verify-law-exceptions-aeneas.sh`, `verify-exact-arith-aeneas.sh` (affected mode) | exit 0 each; every seeded mutant refuted | head `07f5325866` | #1254, merged as `7cc65e72d6` |
| `verify-checker-certificates-aeneas.sh`, `verify-kernel-commit-aeneas.sh` | exit 0 | head `de35428b46` | #1254 |
| `verify-kernel-l1-aeneas.sh` | exit 0 | head `96a15b9c16` | #1254 |
| `verify-preservation-aeneas.sh`, `verify-semantics-lean.sh`, `verify-norm-precedence-aeneas.sh`, `verify-checker-certificates-aeneas.sh`, `verify-law-exceptions-aeneas.sh` | pass | `ef24076e5b` | #1257, merged as `4e0c487c6d` |
| `verify-exact-arith-aeneas.sh` with the pinned Mathlib cache | pass; a one-byte change to a cached archive fails it | head `7797aa60e6` | #1354, merged as `ada69efe16` |
| `verify-promoted-subsystems.sh full` | **failed** at `artifact-governance-rust-extraction` (the sccache compiler probe sent C input to the hax Rust driver, exit 101), identically on base `2f9bd4507` and the head; the Quint and Lean gates before it passed | head `24487efd4b` | #372, merged as `7388b7a09b` |
| `cell-protocol-v1` Quint models (Apalache bounded checks, 200k simulated traces each) and TLC liveness | no counterexample at the stated depths; TLC finished for one harness of three | 2026-11-11 | #1240, merged as `c746a98b4c`; `cell-protocol-v1/results.json` |

## The Verus spike

`kernel-l1/version_verus.rs` is a copy of the body of `checked_next_version` from `crates/norn-kernel/src/version_check.rs`, with a Verus `ensures` clause added. It is a model beside the code, not a proof of the shipped module: Verus checks the copy, not the compiled kernel.

`scripts/verify-kernel-l1-version-body.py` ties the copy to production by text: it matches both files against fixed patterns and fails unless the two function bodies are equal after removing whitespace. Its own docstring says this is "textual source correspondence, not Rust/Verus compiler equivalence". No gate in `verify-promoted-subsystems.sh` calls it, and no script in the repository runs Verus.

The same function's proof about the production source is the `kernel-l1-version-correspondence` gate: `scripts/verify-kernel-l1-aeneas.sh` extracts `crates/norn-kernel/src/version_check.rs` itself with Charon and Aeneas and checks the Lean proof against that extraction.
