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

Any other route of the contract is read with `client.get(route, { path, query })`
and called with `client.post(route, { path, query, body })`, typed by the
generated `paths`: an unserved route, a missing path parameter or a call-level
`orgId` is a compile error. Documents are read exactly (`ExactDocument<T>`, every
number as its source text, as `parseExactJson` reads it). The answer is
`document | refused`; a 404 Refusal@2 is the route's refusal (a record it does
not hold), except `route.not_found`, which like any non-refusal 404, and the 503
a deployment with no host bound answers (`NO_HOST_ERROR`), throws
`NornUnavailable` with `notServed`. Any other non-refusal answer, a gateway's
503 included, is `NornUnavailable` without it: the cell did not answer, and a
read may be repeated. A success that is no JSON document (an empty body, unless
the route declares that success without content in `EMPTY_SUCCESS`, or a body
that is no object or array) is `NornUnexpectedResponse`. A route that answers with a file (an export)
is read with `client.download`, as bytes in a media type the route declares
(`FILE_MEDIA`), never through the JSON reader. `client.path(route, ...)` is the
scoped path a live subscription names. `LiveClient` frames are the contract's
`LiveClientMessage` / `LiveServerMessage`; each snapshot carries its body read
exactly (`exact`) and, for a failing status, the route's checked `refusal`.
Once the client stops reconnecting, every subscriber (and any later one) is
told `LIVE_UNAVAILABLE` through `onRefused`: its answers are no longer followed.

`norn codegen --ts <artifacts> [--base <artifacts>] --out FILE` (in
`tools/norn-ts`) emits one types-only `.d.ts` with the `ActionMap`, `ObjectMap`
and `DefinitionsDigest` of an admitted definition set; `--check` fails when the
committed file drifts.
