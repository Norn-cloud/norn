# TypeScript core SDK

`Schemas` exposes generated IR and checker contracts. Author definitions with
`tools/norn-ts`; run the checker through its WASM ABI. The native cell is an
embedded Rust library. Regenerate with `norn openapi` and `bun run generate`.

`createNornClient` is the typed client for a cell's routes: every route, its
parameters, body and success document come from the generated contract. Each
call names the client's scope, returns the exact text of every number beside the
document (`numbers`, by RFC 6901 pointer), and throws `NornRefusalError` (code,
category, diagnostics) for a valid `norn/refusal@2` document; any other failure,
a malformed refusal included, is `NornUnavailableError`. `get` and `post` take
document routes; a route that answers with a file (an export) is read with
`download`, as bytes and its content type. `fetch`, the base URL and auth (a host
token, or cookies through a fronting app) are pluggable. The live-read protocol
`norn.live.v1` is typed as `LiveClientMessage` and `LiveServerMessage`;
`parseLiveFrame` reads a frame with its snapshot body's exact numbers.
