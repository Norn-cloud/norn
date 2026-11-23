# `tools/norn-py` — Typed Python Authoring Spike (#1389 Option B)

Experimental, isolated symbolic-tracing Python 3.12+ authoring front end for Norn,
approved under **Astra** and **Fable** rulings (2026-11-23, Epic #1595).

## Architectural Contract

- **Python types are DX-only.** `mypy --strict` / `pyright --strict` catch unit,
  currency, float, state, and link bugs in the editor. **Rust `norn-checker` stays
  the sole admission authority** feeding Quint/Apalache, SMT, and Lean.
- **Two-layer deterministic sandbox (`norn_py/sandbox.py`):**
  1. AST pre-flight check before any Python executes (strict import allowlist,
     forbids `eval`/`exec`/`compile`/`__import__`/`open`, `global`/`nonlocal`,
     `async`/`await`, `yield`, and `try/except` swallowing tracing errors).
  2. Isolated subprocess (`python3 -I -B`, `PYTHONHASHSEED=0`, `TZ=UTC`, `LC_ALL=C`)
     with `sys.addaudithook` failing closed on IO/network/process events and
     byte-identical determinism verification.
- **Hard symbolic-tracing refusals (`norn_py/expr.py`):**
  - `Expr.__bool__`, `__len__`, `__iter__`, `__contains__`, `__index__` all raise
    `NornTracingError` with actionable fixes (`all_of`, `any_of`, `not_`, `when`, `between`).
  - Bare `float` is refused everywhere (`NornFloatRefusalError`); only exact
    `Decimal` / string decimal literals and `int` are admitted.
  - Zero `unsafeZen` escape hatch and zero surviving callbacks.
- **Byte-identical canonical JSON parity** against `tools/norn-ts` (`#1391`),
  with AST-captured `(file, line, col)` source spans.

## Quick Verification

```bash
cd tools/norn-py
python3 -m unittest discover -s tests -v
mypy --strict norn_py examples tests
ruff check norn_py examples tests
```
