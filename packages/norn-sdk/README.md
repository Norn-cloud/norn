# TypeScript core SDK

`Schemas` exposes generated IR and checker contracts. Author definitions with
`tools/norn-ts`; run the checker through its WASM ABI. The native cell is an
embedded Rust library. Regenerate with `norn openapi` and `bun run generate`.

## Client

`createNornClient<ActionMap>({ baseUrl, scope, definitionsVersion })` lists the
actions a cell offers and calls one. A call answers one of three arms,
`receipt | pending | refused`; a route the cell does not serve, or an answer it
does not document, throws `NornUnavailable`. Mint one idempotency key per user
intent with `createIntentKey()` (or `client.intent(...)`) and resend that key
on every retry of that intent; it is never derived from the payload, so two
deliberate identical submissions stay two calls. An optional
`definitionsVersion` pins every call, so a cell that has installed other
definitions refuses `defs.stale`. `LiveClient` holds the `norn.live.v1`
subscription socket. There is no mock mode: `norn dev` serves the real routes.

`norn codegen --ts <artifacts> --out FILE` (in `tools/norn-ts`) emits one
types-only `.d.ts` with the `ActionMap`, `ObjectMap` and `DefinitionsDigest` of
an admitted definition set; `--check` fails when the committed file drifts.
