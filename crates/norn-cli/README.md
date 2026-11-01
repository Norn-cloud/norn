# norn

`norn check --artifacts artifacts.json --context context.json` runs the same
checker ABI used by TypeScript authoring and returns its admission report.
`norn openapi` emits the generated core wire contract. Embed `norn-cell` for
transactional cell execution; the public CLI does not embed a product server.

`norn eval --defs DIR --org ID --env ID --rule ARTIFACT_ID` admits a standalone
rule set and evaluates JSON-lines `{slice,event}` inputs through `norn-decisions`.
It emits candidate effects as data under a bounded operation budget.

`norn remote` discovers and calls an external Norn MCP endpoint using its live
catalog, without a local action whitelist:

```sh
norn remote --url https://norn.example/mcp --org example --env dev --token-env NORN_TOKEN list
printf '%s' '{"id":"example"}' | norn remote --url https://norn.example/mcp --org example --env dev --token-env NORN_TOKEN call example.submit --idempotency-key example-submit-1
```

Calls read a JSON object from stdin, preserve exact numeric tokens and forward
the explicit operation identity. Server tool refusals, protocol errors and HTTP
failures exit nonzero and retain a valid JSON-RPC response on stdout. Bearer
tokens come from the named environment variable and require HTTPS. Redirects
are disabled. `--dev-actor` supports an external server explicitly configured
for local actor headers; production identity and authorization remain on the
server. This command does not supply a local server or install definitions.

The client reuses clap, ureq/rustls and the maintained sse-stream decoder;
MCP event data stays text so business decimals never round through floats.
