# `norn-authoring-py`

Ring-1 native Rust Python authoring compiler, AST sandbox, expression lowerer,
declarative schema elaborator, and formatter for Norn authoring modules
(`tools/norn-py`), embedding Astral's `ruff_python_parser`, `ruff_python_ast`,
and `ruff_python_formatter` (`0.0.23`, MIT) alongside `norn-canonical`.

## Slice 1 scope

- `sandbox`: fail-closed AST preflight mirroring `tools/norn-py/norn_py/sandbox.py`
  (import and symbol allowlists, forbidden builtins/dunders/attributes, forbidden
  imperative syntax and control flow, exact source byte / AST node / AST depth
  budgets, and deterministic top-level source-span capture).
- `expr`: closed expression AST (`ZenExpr`), exact `Decimal` and ZEN string
  validation, and direct AST-to-ZEN (`norn-zen/7`) callback lowering with zero
  Python runtime execution.
- `fmt`: in-process deterministic Python formatting via `ruff_python_formatter`
  with zero external `ruff` CLI subprocesses.

## Slice 2 scope

- `canonical`: `irVersion: "0.1"` artifact envelopes (`EnvelopeOptions`,
  `DependencyPin`, `build_envelope`), UTF-16 sorted-key golden text
  (`canonical_json`), and `Norn1` compact bytes + domain-separated SHA-256
  (`canonical_norn1_bytes`, `canonical_norn1_sha256`) backed directly by
  `norn-canonical`.
- `schema`: zero-CPython-execution static lowering of `tools/norn-py` authoring
  modules (`MarkingsCatalog`, `Concept` object types & interfaces, `StrEnum`,
  `Annotated[Field[...], ...]` consistency checks, `Money`/`Quantity`/`Rate`,
  `Link`/`link(...)`, `KeySpec`, `lifecycle`/`transition` guards, `@invariant`,
  `@law`, `fn_ref`, `PackManifest`/`compose_packs`, and `DeploymentSpec`) with
  byte-for-byte golden parity against `tools/norn-py/examples/hr_pilot.py`,
  `registry/core/artifacts/core.Agent.interface.json`, and
  `registry/core/artifacts/core.TimeInterval.interface.json`.

## `ty_python_semantic@0.0.23` status

`ty_python_semantic@0.0.23` (and `ty_vendored@0.0.23`) pass `deny.toml` license
and yanked checks, but `ty_python_semantic` exposes only `pub trait Db` (no
public concrete database type outside `#[cfg(test)]`), requiring callers to
apply `#[salsa::db]`. In `salsa-macros@0.28.6/src/db.rs:94`, `#[salsa::db]`
expands inside the caller crate to `unsafe impl salsa::plumbing::HasStorage`,
which is rejected by this workspace's pinned `unsafe_code = "forbid"` lint.
In-process `ty` typechecking remains deferred from Slice 2 until a pre-built
concrete database wrapper is available without `unsafe` expansion in Norn
crates.
