# Vendored ZEN (Norn-owned fork)

This directory is the ZEN rules engine and expression language from
[gorules/zen](https://github.com/gorules/zen), vendored into norn-platform on
2026-10-21 (#3404). It was imported byte-identical from the Norn fork tag
`Norn-cloud/zen` `norn-v2.0.1-11` (`1f6f794a`, upstream base `zen-engine-v2.0.1`) with
`git subtree add --squash`; every later change is a reviewed commit in this repository.
The provenance record is `crates/norn-skeleton-guard/zen-pin.json` and the history is
`knowledge/architecture/build-plan-2026-09-09/zen-fork-ledger.md`. `NORN.md` is the
fork's patch ledger up to the import.

The code is MIT-licensed (`LICENSE`, Copyright 2023 GoRules.io); keep that notice.

## Crates

| Crate | Directory | Norn consumer |
| --- | --- | --- |
| `zen-engine` | `core/engine` | `norn-decisions` |
| `zen-expression` | `core/expression` | `norn-expr` (and `zen-engine`, `zen-tmpl`) |
| `zen-tmpl` | `core/template` | `zen-engine` |
| `zen-types` | `core/types` | `zen-engine`, `zen-expression` |
| `zen-macros` | `core/macros` | `zen-expression` |

They are workspace members with no default features; consumers enable the Norn profile
features explicitly. The engine keeps the JDM graph evaluator with input, output,
expression, decision-table, switch and sub-decision nodes. Function (JavaScript) and
custom nodes, the HTTP module, policies, workspace analysis, node-level JSON Schema
validation, the filesystem/closure/zip loaders, the language bindings, benchmarks and
the REPL were removed (#3404).

## Taking upstream fixes

Cherry-pick under the vendored prefix: `git cherry-pick -X subtree=vendor/zen <sha>`.
See the ledger's maintenance runbook.
