# norn-code-ontology

The pure `norn.code-ontology/1` contract: which handler reads, writes,
triggers on, calls, schedules, guards or exposes what, each fact with its file,
line, revision and byte digest. Data only.

- `CodeOntology`: an extractor's whole result: handlers, facts, file-level
  diagnostics, per-app coverage and the entity-to-handler matrix.
- `Fact`: one source relation. `confidence` is `resolved` or `unresolved`; an
  unresolved fact always carries a `Diagnostic`. It is a static resolution
  state, not a probability.
- `Diagnostic`: a code string, with constants for the codes the current
  extractor emits.

The facts come from `tools/norn-port/code_ontology.py`, which owns the code
graph and the per-framework source rules. A resolved fact is evidence about
source text: it is not behavioural parity, an authorization decision or a Norn
artifact.

## What version 1 fixes

- `Relation` and `Confidence` are closed, and every object refuses unknown
  members. A new relation, resolution state or member is a new format version.
- `Diagnostic` is open: an unknown code decodes, so adding a code does not
  break an older reader. Compare against the constants; never match
  exhaustively.
- Entity and field names are the source system's own. The one extractor today
  has rules for one framework, and a few diagnostic codes name its source
  languages (`python_parse_error`, `dynamic_client_handlers`). A reader for
  another system emits the same shape, with its own codes where these do not
  fit.

## Not in this format

- Field descriptors (native type, required, keys, code sets, relations). A
  fact says which field is touched, not what the field is. Descriptors come
  from a schema reader's `SourceSchemaIR`.
- The source-unit inventory that facts can be joined to. It arrives with its
  first producer.

## Boundary

- Ring: Ring 1 — native driver and host perimeter. The crate itself performs
  no IO and builds for `wasm32-unknown-unknown`.
- Purpose: decode code facts. No parser, code graph, rule engine, file access
  or source rules live here.
- Allowed dependencies: `serde`; `serde_json` in tests only. No `norn-*`
  edge. The machine-checked rule is `boundary.json`, enforced by
  `norn-skeleton-guard`.
- Nothing here executes or imports source code, grants a permission or
  installs a definition.
