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
`receipt | pending | refused`; a route the cell does not serve, an answer it
does not document, or no answer at all (`status` 0) throws `NornUnavailable`.

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
