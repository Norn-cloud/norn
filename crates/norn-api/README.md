# Core wire contracts

Projects the Rust IR and checker schemas into OpenAPI components for the TypeScript SDK.
Run `norn openapi` to regenerate the contract. Cell execution uses `norn-cell` and
`norn-kernel` directly; this crate supplies no server.

The replay clock is a host prerequisite: the pure `Clock` interface supplies
kernel logical time, and `ManualClock` supports deterministic replay through
shared `Arc` handles. Wall-clock access, the native server, connector host and
analytics executor remain deferred to subsequent ports.

## Host export contract (slice 1a)

Exports scope and refusal infrastructure without a server or store.
Transport adapters map `Refusal.status` to their response.

- HOST-SCOPE: missing/blank dimensions refuse with `scope.missing` (400);
  exact dimension mismatches refuse with `scope.denied` (403).
  Keys percent-escape `%` and `/` once per component.
- HOST-WIRE: camelCase scopes, snake_case categories, status outside JSON.
- HOST-REFUSAL: conceal opaque diagnostics while retaining code/category/status;
  explicit categories override status defaults. Commit projection follows later.
- HOST-IDENTITY: visibility uses canonical `norn_ir::ScopeRef`.

Correspondence: `tests/host_perimeter.rs`. Denial-before-read and default-deny
belong to host wiring; the perimeter performs no IO.

## Definition projection contract (slice 1b)

`defs::Admitted` retains the host's definition data; `install::defs_response`
projects it. These infrastructure types do not install or serve definitions.
The host admission and authorized GET path follow in the next slice.

- HOST-IDENTITY: envelopes, layout and flow source maps use canonical IR types.
- HOST-WIRE: preserve the existing definition response fields, camelCase
  revisions, empty arrays and omission of an absent flow source map.
- HOST-EXACT: artifacts are raw JSON decoded directly from the admitted canonical
  source array, never reserialized through floating-point JSON values.
- HOST-RECEIPT: copy the checker receipt and hash unchanged; derive base artifact
  ids and the optional source map from that same receipt. Invalid receipt shape
  or malformed source text refuses opaquely with `host.error`.

Correspondence: `tests/definition_projection.rs` checks synthetic checker receipts,
key-order independence, semantic changes, exact large integers and scale-28
boundaries. HOST-RECEIPT-EXACT-INTEGER and HOST-RECEIPT-EXACT-DECIMAL use the
checker source-aware admission path and distinguish both adjacent-literal pairs;
a present flow source map is retained unchanged. Numeric admission refusal, scope admission, installed-revision binding
and the definition epoch are host-wiring obligations, not projection guarantees.

HOST-ADMISSION-FLAG: response projection requires the checker receipt's `admitted`
field to be explicitly `true`; missing, false, null or non-boolean claims refuse
with opaque `host.error`. This consistency check does not replace host admission.

HOST-RECEIPT-SHAPE: projection delegates format/field consistency to the checker parser; mapless receipts carrying a flow map refuse opaquely.

ADMITTED-TEXT-NFC source closure: receipt writers retain raw-source NFC findings before typed projection, including unknown scope keys/values that a typed envelope can omit. Candidate and base admission return the stable diagnostic with a rejected receipt and no type environment. Retained-source substitution and independent installed-context rebinding remain strict refusals. Native and real WASM parity scenarios cover the three source positions in both layers.

## Host target and base contracts

HOST-TARGET: native targets enable the native cell adapter; Wasm uses the portable
cell implementation. Default library builds and the fixture command need no flags.
HOST-BASE: admission accepts a complete standalone tenant artifact set with no
external base catalog. Extensions requiring a separate trusted base refuse; the
host never infers base authority from submitted definitions.
