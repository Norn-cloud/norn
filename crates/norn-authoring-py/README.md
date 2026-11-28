# `norn-authoring-py`

Ring-1 native Rust Python authoring compiler, AST sandbox, expression lowerer, and
formatter for Norn authoring modules (`tools/norn-py`), embedding Astral's
`ruff_python_parser`, `ruff_python_ast`, and `ruff_python_formatter` (`0.0.23`, MIT).

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

## `ty_python_semantic@0.0.23` status

`ty_python_semantic@0.0.23` (and `ty_vendored@0.0.23`) pass `deny.toml` license
and yanked checks, but `ty_python_semantic` exposes only `pub trait Db` (no
public concrete database type outside `#[cfg(test)]`), requiring callers to
apply `#[salsa::db]`. In `salsa-macros@0.28.6/src/db.rs:94`, `#[salsa::db]`
expands inside the caller crate to `unsafe impl salsa::plumbing::HasStorage`,
which is rejected by this workspace's pinned `unsafe_code = "forbid"` lint.
In-process `ty` typechecking is therefore deferred from Slice 1 until a
pre-built concrete database wrapper is available without `unsafe` expansion in
Norn crates.
