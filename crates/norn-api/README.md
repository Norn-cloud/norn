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

The `mcp_workers` example serves one configured cell's `/mcp` and `/catalog`
through `mcp_host::Host`. Its only capability checks artifact/context **content**
using the same checker ABI as `norn check`; it never reads host file paths.
Raw tables, columns, physical schema browsing and SQL are absent. Typed,
authorized binding questions remain allowed when their shared service is available.
Actions await #500; demo documents and durable action conformance are later units.
Sessions expire after five minutes and end on eviction; business state is untouched.
Configure ORG/ENVIRONMENT/ORIGIN as trusted deployment bindings. Each request
carries Norn-Org/Norn-Environment; these select scope, not an authentication grant.

Build `mcp_workers` like the cell's `workers_sql` example, replacing the package,
example and glue names. Copy `examples/mcp-workers/{worker.mjs,wrangler.json}`
into `$NORN_MCP_PROJECT` outside the checkout. Build native `mcp_reference`, set
`NORN_MCP_REFERENCE` to its executable and run `examples/mcp-workers/check.py`.
This compares actual celld v0.6.3 HTTP responses byte-for-byte with native Rust,
including negative catalog/raw-access cases and ephemeral session restart.
