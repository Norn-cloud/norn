# Core wire contracts

Projects the Rust IR and checker schemas into OpenAPI components for the TypeScript SDK.
Run `norn openapi` to regenerate the contract. Cell execution uses `norn-cell` and
`norn-kernel` directly; this crate supplies no server.

## Host export contract (slice 1a)

This slice exports the existing host's scope and refusal perimeter without a
server or store. Transport adapters will map `Refusal.status` to their response.

- HOST-SCOPE: missing or blank dimensions refuse with `scope.missing` (400).
  Both dimensions must match exactly; a mismatch is `scope.denied` (403).
  A scope key percent-escapes `%` and `/` once per component.
- HOST-WIRE: scope uses camelCase, refusal categories use snake_case, and status
  stays outside the JSON body. Missing request dimensions remain explicit nulls.
- HOST-REFUSAL: status, stable code and category survive diagnostic concealment.
  Opaque host diagnostics never enter the explanation. Explicit source categories
  override status-derived categories. Commit-wrapper projection follows with
  action dispatch; this slice does not claim read or traversal authorization.
- HOST-IDENTITY: artifact visibility consumes the canonical `norn_ir::ScopeRef`.

Correspondence: `tests/host_perimeter.rs` covers these properties and exact JSON.
Denial-before-store-read and default-deny authorization belong to the host wiring
slice; the perimeter itself performs no IO.

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
boundaries. Numeric admission refusal, scope admission, installed-revision binding
and the definition epoch are host-wiring obligations, not projection guarantees.
