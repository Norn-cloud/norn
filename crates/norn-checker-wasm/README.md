# norn-checker-wasm

The checker ABI (T1-CHK-4, #2527): one JSON-in/JSON-out surface over the Rust
checker, so the TypeScript extractor, jdm-editor, SDKs, and the playground get
the same diagnostics as `cargo test` instead of re-implementing or linking
checker internals. Ring 1; see `boundary.json`.

## Functions

The ABI takes source strings and JSON text and returns JSON text. Keep numeric
results as text until a lossless JSON parser consumes them: JavaScript
`JSON.parse` can round exact decimal tokens. Boolean conditions and error
documents can use `JSON.parse`. Invalid input throws an `Error` for the other
functions; `evaluateExpression` returns typed JSON refusals for expected input,
profile, source, and evaluation failures. Invalid artifacts are findings.

| JavaScript | Rust | Result |
|---|---|---|
| `checkArtifactSet(artifactsJson, contextJson)` | `check_artifact_set_json` | `{format: "norn-checker-abi/check@1", admitted, diagnostics, receipt, receiptSha256}` — `diagnostics` is the `norn-checker/diagnostics@1` document, `receipt` the Norn1 `norn-checker/receipt@2` document (both `null`-receipt when an envelope does not decode) |
| `lifecycleDiagrams(artifactsJson)` | `lifecycle_diagrams_json` | `[{type, artifactId, property, mermaid}]` — Mermaid `stateDiagram-v2` per declared lifecycle |
| `checkExpression(expr, envArtifactsJson, expectedType, selfType?)` | `check_expression_json` | `{format: "norn-checker-abi/expression@1", profile: "norn-zen/6", ok, inferred, diagnostics: [{code, message, span}], environment}` |
| `evaluateExpression(expr, inputJson, profile?)` | `evaluate_expression_json` | `{format: "norn-checker-abi/evaluation@1", ok, profile: "norn-zen/6", unitsUsed, value | error}` — bounded `norn-expr` evaluation over explicit WidgetIR inputs |
| `functionContract(manifestJson, invocationJson?, receiptJson?)` | `function_contract_json` | `{format: "norn-checker-abi/function@1", admitted, manifest: {digest, executionProfile}, invocation: {digest, inputIdentity, dependencyToken, actionKey}, receipt: {digest, resultIdentity}, diagnostics}` — the Function contract (T1-FN-1) admitted, with its Norn1 identities; finding pointers are prefixed `/manifest`, `/invocation`, `/receipt` |
| `version()` | `version_json` | `{abi: "norn-checker-abi/1", crateVersion, checkerVersion, diagnosticsFormat, receiptFormat, exprProfile}` |

- `artifactsJson` / `envArtifactsJson`: a JSON array of artifact envelopes.
- `contextJson`: `null` or `{base?, registry?, tenant?: {tenantId, namespace}, limits?}`
  (unknown members are rejected); `base` is the installed layer, `limits`
  defaults to `norn.limits/1`.
- `expectedType`: `bool`, `text`, `integer`, `number`, `date`, `duration`,
  `json`, an object type name (e.g. `retail.Sku`), `list<T>`, or `T?`.
- `selfType`: the object type `self` is bound to (optional).
- `inputJson`: raw JSON text for the WidgetIR input object. Its optional slots
  are `record`, `draft`, and `context` (each a record), and `selection` (a
  list of at most 1,000 records). Omitted slots stay absent; unknown slots and
  null slot values are refused. The input text is limited to 256 KiB. Pass raw
  text, not a JavaScript
  object, to preserve decimal tokens exactly.
- `profile` for `evaluateExpression`: omitted or `"norn-zen/6"`. The runtime
  budget is fixed to the `norn.limits/1` decide fuel of 1,000,000 units;
  `unitsUsed` reports charged evaluator operations, including on failure.
  The source and input contract must already have passed WidgetIR admission
  through `checkArtifactSet`, and runtime inputs must conform to their declared
  types. Evaluation validates source bounds and exact numeric tokens but does
  not repeat environment-dependent type or grammar admission.

`evaluateExpression` returns `ok: true` with an exact JSON `value`, or
`ok: false` with one of the following `error` shapes. Source diagnostic spans
use `{start, end}` byte offsets. Invalid input or source consumes zero evaluator
units; runtime failures retain the charged units.

| Error code | Additional fields |
|---|---|
| `expr.input.invalid`, `expr.profile.invalid` | `argument`, `message` — malformed JSON, invalid slot types, input bounds, unsupported profile, or an unrepresentable input number |
| `expr.syntax.invalid`, `expr.construct.unsupported`, `expr.bound.source`, `expr.bound.depth`, `expr.bound.nodes`, `expr.bound.literal`, `expr.bound.closure_depth` | `diagnostics: [{code, message, span}]` from source validation; a compilation failure has `message` instead |
| `expr.evaluation.failed` | `message` — a runtime type, temporal, or arithmetic failure |
| `expr.budget.exhausted` | `limit`, `used`, `message` — the attempted charge exceeds the fixed budget; top-level `unitsUsed` remains capped at the limit |
| `expr.number.invalid` | `input`, `reason` |
| `expr.result.unsupported` | `typeName` |
| `expr.output.invalid`, `expr.error.unknown` | `message` (and `argument` for output failures) — reserved for an output conversion failure or a future unclassified evaluator error |

## Build the npm package

Use the CI artifact when local builds are prohibited. CI/image maintainers
provision `wasm32-unknown-unknown` and `wasm-bindgen-cli` 0.2.129; agents must
not install global tools on the shared box. In an environment that permits
builds, use the provisioned pinned toolchain:

```bash
crates/norn-checker-wasm/scripts/build-pkg.sh            # -> target/norn-checker-wasm/pkg
crates/norn-checker-wasm/scripts/build-pkg.sh <out-dir>  # or a chosen directory
```

The package (`@norn/checker-wasm`, ES module, `--target web`) holds
`norn_checker_wasm.js`, `.d.ts`, `norn_checker_wasm_bg.wasm`, a
`package.json`, and `SIZE.txt`. The CI job `checker-wasm` builds it on every
PR and uploads it as the `norn-checker-wasm-pkg` artifact
(`gh run download <run-id> -n norn-checker-wasm-pkg`).

```ts
import init, { checkArtifactSet, lifecycleDiagrams } from "@norn/checker-wasm";
await init(); // Vite/bundlers resolve the .wasm next to the module
const result = JSON.parse(checkArtifactSet(JSON.stringify(artifacts), "null"));
```

In Node: `initSync({ module: readFileSync(".../norn_checker_wasm_bg.wasm") })`.

## Parity

`tests/parity.rs` runs every case in `tests/parity/cases.json` natively and
checks it against the checker called directly (the retail receipt matches
`norn-checker`'s pinned hash). The CI job then runs the same cases through the
built package (`tests/parity/parity.mjs`) and requires byte-identical output.

## Deferred

The native C ABI (`norn_check(json_bytes)` over a `cdylib`) is the other half
of #2527 and is not built yet: it needs `unsafe` FFI glue (the workspace
forbids `unsafe_code`) and has no consumer today. It will wrap the same
`*_json` functions, so its output is the same bytes.
