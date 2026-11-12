# norn-code-ontology

The pure `norn.code-ontology/1` contract: which code unit reads, writes,
triggers on, calls, schedules, guards or exposes what, each fact with its file,
line, revision and byte digest. Data only.

- `CodeOntology`: a host tool's whole result: handlers, facts, file-level
  diagnostics, per-app coverage and the entity-to-handler matrix.
- `Fact`: one source relation. `confidence` is `resolved` or `unresolved`; an
  unresolved fact always carries a typed `Diagnostic`. It is a static
  resolution state, not a probability.
- `unit::CodeUnit`: one inventoried source unit. `CodeOntology::facts_for`
  joins facts to a unit only when path, symbol, original-byte digest and
  revision all agree, so stale facts never attach to changed source.

The facts come from `tools/norn-port/code_ontology.py`, which owns the code
graph and the per-framework source rules. The wire names are source-native
(`unknown_doctype`, `python_parse_error`) and stable. A resolved fact is
evidence about source text: it is not behavioural parity, an authorization
decision or a Norn artifact.

## Boundary

- Ring: Ring 1 — native driver and host perimeter. The crate itself performs
  no IO and builds for `wasm32-unknown-unknown`.
- Purpose: decode and join code facts. No parser, code graph, rule engine,
  file access or source rules live here.
- Allowed dependencies: `serde`; `serde_json` in tests only. No `norn-*`
  edge. The machine-checked rule is `boundary.json`, enforced by
  `norn-skeleton-guard`.
- Nothing here executes or imports source code, grants a permission or
  installs a definition.
