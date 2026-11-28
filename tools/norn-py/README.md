# `tools/norn-py` — Typed Python Authoring Spike (#1389 Option B)

Experimental, isolated symbolic-tracing Python 3.12+ authoring front end for Norn,
approved under **Astra** and **Fable** rulings (2026-11-23, Epic #1595).

## Architectural Contract

- **Python types are DX-only.** `Field[T]` descriptors, `Timestamp`, `Date`, `Duration`,
  `Quantity[U]`, `Money[C]`, `Rate[C, U]`, `StrEnum`, and `Link[Target]` preserve concrete
  static types under `mypy --strict` so unit, timestamp, float, state, and link bugs fail
  in the editor. **Rust `norn-checker` stays the sole admission authority** feeding
  Quint/Apalache, SMT, and Lean.
- **No `eval()` on annotations and no raw IR hatches (`norn_py/schema.py`, `norn_py/expr.py`):**
  - Field annotations are parsed directly via `ast.parse(..., mode="eval")` and resolved
    only against a closed allowlist of Norn types, primitives, and safe local bindings
    (`eval()` and `typing.get_type_hints()` are never called).
  - `Annotated[base_type, meta...]` consistency is validated at elaboration time.
  - `Expr`, `PropertyTypeSpec`, and `MaskSpec` are closed and unsubclassable; raw ZEN
    strings, raw IR dicts, and duck-typed `.zen` objects are refused.
  - Object `invariants` are emitted into `objectType.invariants`; interface `lifecycles`
    (`INTERFACE-LIFECYCLE-1`, including `endings(...)`) and `disjoint_with`
    (`INTERFACE-DISJOINT-1` -> `disjointWith`) are emitted into `interface` payloads, while
    `disjoint_with` on concrete object types is explicitly rejected.
- **Pre-tracing callback AST validation (`norn_py/expr.py`):**
  - Every invariant, lifecycle guard, and law callback is inspected with `ast.parse`
    BEFORE tracing. Python `if` / `if`-expr, `and` / `or` / `not`, `is` / `is not` /
    `in` / `not in`, chained comparisons, loops, comprehensions, nested lambdas, and
    calls outside the closed expression operator set are rejected with structured
    diagnostics before any callback executes.
  - `Expr.__bool__`, `__len__`, `__iter__`, `__contains__`, `__index__` raise
    `NornTracingError`, and bare `float` is refused everywhere (`NornFloatRefusalError`).
- **Deterministic two-layer sandbox (`norn_py/sandbox.py`):**
  1. Layer 1 (AST pre-flight, zero execution): strict module + symbol import allowlist,
     bans ALL dunder attributes/names (`__subclasses__`, `__class__`, `__globals__`,
     `__builtins__`, etc.), dynamic builtins (`eval`, `exec`, `compile`, `__import__`,
     `open`, `getattr`, `setattr`, `delattr`, `globals`, `locals`, `vars`, `dir`, `type`,
     `breakpoint`, `input`), custom metaclasses, and imperative control flow.
  2. Layer 2 (Isolated subprocess): runs `python3 -s -S -B -P` (intentionally avoiding
     `-I`, which causes CPython 3.12+ to ignore `PYTHONHASHSEED=0`) with a minimal clean
     environment (`LC_ALL=C`, `PYTHONHASHSEED=0`, `PYTHONPATH`, `TZ=UTC`), POSIX
     `resource` CPU / virtual-memory / zero-file-write limits, a strictly closed minimal
     `__builtins__` dict with closed stdlib module proxies, and `sys.addaudithook`.
- **Canonical Norn1 bytes & TS parity (`norn_py/canonical.py`):**
  - `canonical_norn1_bytes(...)` emits compact zero-whitespace Norn1 canonical JSON bytes
    (Unicode NFC normalization, duplicate-after-NFC rejection, code-point sorted keys)
    and `canonical_norn1_sha256(...)` computes the domain-separated `Norn1` SHA-256 digest.
  - `canonical_json(...)` emits human-readable golden-file JSON matching `tools/norn-ts/src/canonical.ts`.
  - ZEN expression rendering matches `tools/norn-ts/src/builder/expr.ts` parenthesization exactly.

## Trust Boundary & Honest Evidence Labels

- **Evidence classification:** `structural` and `bounded` (unit tests, AST pre-flight,
  restricted builtins, audit hook, POSIX `rlimit` caps, and live TS/Python parity tests).
- **Important security boundary:** Python-level AST validation, restricted `__builtins__`,
  and `sys.addaudithook` provide **defense-in-depth for developer authoring workflows**,
  not a kernel-enforced multi-tenant security boundary. **Untrusted multi-tenant input
  MUST additionally run inside an OS-level container, seccomp profile, or microVM.**

## Spike Prototypes Outside the Artifact Collector

`FnRef` (`fn_ref`), `PackManifest` (`compose_packs`), `SecretRef`, and `DeploymentSpec`
are experimental spike prototypes for exploring pack composition and `perform` handles;
they are **not** collected by `collect_artifacts(...)` into the EntityIR/LawIR artifact stream.

## Quick Verification

```bash
cd tools/norn-py
uv run --no-project --with pytest pytest -v
mypy --strict norn_py examples tests
ruff check norn_py examples tests
```
