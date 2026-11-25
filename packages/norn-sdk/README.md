# TypeScript core SDK

`Schemas` exposes generated IR and checker contracts. Author definitions with
`tools/norn-ts`; run the checker through its WASM ABI. The native cell is an
embedded Rust library. Regenerate with `norn openapi` and `bun run generate`.

The package ships TypeScript source (`exports` points at `src/index.ts`) and has
no dependencies. Consume it with Bun or a bundler that compiles TypeScript
(Vite, esbuild); it uses only erasable syntax, so Node's type stripping runs it
too.

## Client

`createNornClient<ActionMap>({ baseUrl, scope, definitionsVersion })` lists the
actions a cell offers and calls one. Its wire shapes are the generated
`Schemas[...]` documents. A call answers one of three arms,
`receipt | pending | refused`, each validated against the contract
(`src/shapes.ts`, diffed against the OpenAPI document by its tests): a refusal
is only a whole Refusal@2 envelope. Anything else throws `NornUnavailable` (a
success status with a malformed document: `NornUnexpectedResponse`). Its
`outcome` is `not-applied` only for a route the cell does not serve (404, for
that request alone); otherwise it is `unknown`, because the request may have
been applied before its answer was lost, including when a write is aborted
after it was sent: retry a write with its original idempotency key, never a new
one. A signal aborted before sending throws its abort; nothing was sent.

Mint one idempotency key per user intent with `createIntentKey()` (or
`client.intent(...)`) and resend that key on every retry of that intent; it is
never derived from the payload, so two deliberate identical submissions stay
two calls. A `definitionsVersion` (on the client, from `client.pinned(n)`, or
per call) pins calls, so a cell that has installed other definitions refuses
`defs.stale`; a catalog from another version reads as `stale`. `LiveClient`
holds the `norn.live.v1` subscription socket. There is no mock mode: `norn dev`
serves the real routes.

`norn codegen --ts <artifacts> [--base <artifacts>] --out FILE` (in
`tools/norn-ts`) emits one types-only `.d.ts` with the `ActionMap`, `ObjectMap`
and `DefinitionsDigest` of an admitted definition set; `--check` fails when the
committed file drifts.
