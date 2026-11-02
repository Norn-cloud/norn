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

## Host target and base contracts

HOST-TARGET: native targets enable the native cell adapter; Wasm uses the portable
cell implementation. Default library builds and the fixture command need no flags.
HOST-BASE: admission accepts a complete standalone tenant artifact set with no
external base catalog. Extensions requiring a separate trusted base refuse; the
host never infers base authority from submitted definitions.
