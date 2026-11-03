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

## Agent transport

`norn mcp` serves newline-delimited MCP JSON-RPC over stdio. Initialize with
`protocolVersion: "2025-11-25"`, discover `tools/list`, then invoke `tools/call`.
Only installed human backends are advertised: `norn.capabilities` and `norn.check`.
Their names, descriptions and input schemas come from the human CLI command
catalog. `norn capabilities` returns the same tool IR, catalog digest and bounded
pages as MCP discovery; pass `nextCursor` back as `--cursor` to continue. It
advertises only available operations. `norn.check` calls the same
checker path as `norn check`; checking never installs definitions. Author typed
source, compile it using the existing authoring command, and supply the resulting
artifact/context file paths. The returned checker report retains receipt and
profile digests, diagnostic locations and remediation.

MCP-CATALOG-1 binds pagination to the complete generated tool IR and the server's
scope/actor/revision binding. Page limits default to 32 and are bounded at 100.
After a revision/authority change, discard the cursor and restart discovery.
MCP-PRESERVE-1 carries backend results and refusal@2 unchanged as structured
content plus text; the server decides disclosure before the transport sees a
refusal. An MCP error does not grant access to hidden pins or operands.
Notifications never dispatch operations. Input messages are bounded at 1 MiB.

The generic `Backend` boundary is for the same human service implementation,
with generated `McpTool` IR supplied by its authorized catalog. It does not infer
permissions from tool names, schemas or annotations. Explain awaits DIAG;
plan/apply/dev await INIT; call awaits the governed action service in #500.
Replay has no shared backend. These operations are absent, not stubbed.

Scripted discovery/check/refusal/repair acceptance (not a live-LLM evaluation):
`python3 scripts/demo/mcp-check-task.py <norn-binary> <compiled-quickstart.json>`.
Protocol correspondence tests additionally retain a real kernel invariant pin
and its operands, and prove that an already-redacted refusal stays redacted.

JSON-RPC parameter refusals preserve the validated request ID and use -32602;
malformed-parameter notifications stay silent. Ping returns an empty result
before or after initialization and does not unlock tool access.

The `mcp_workers` example serves `/mcp` and `/catalog` through Rust `mcp_host`.
Only checker content is accepted, using the human command's checker ABI.
Raw tables, columns, physical schemas and SQL are absent; governed typed
bindings remain allowed when available. Actions await #500, documents follow later.
Trusted ORG/ENVIRONMENT/ORIGIN bindings configure the cell; Norn-Org and
Norn-Environment headers select scope, never grant authentication. Sessions
expire after five minutes or eviction. Build like the cell's `workers_sql`
example; copy `mcp-workers` config/glue outside the checkout to `$NORN_MCP_PROJECT`.
Set `NORN_MCP_REFERENCE` to built `mcp_reference`, then run `mcp-workers/check.py`
for byte-exact native/celld v0.6.3 HTTP correspondence and negative-catalog tests.
