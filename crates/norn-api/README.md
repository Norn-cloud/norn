# norn-api

Host-neutral services shared by every Norn transport, plus the wire contracts.
It projects the Rust IR and checker schemas into OpenAPI components for the
TypeScript SDK; run `norn openapi` to regenerate the contract. This crate
supplies no HTTP server or listener: transports call its services. The Durable
Object host and the native dev host (`norn-dev-host`) in `norn-cell-do`, and the
`norn` CLI, are its transports.

Services in `src/`, each named in `lib.rs`:

- Reads: definitions (`routes`), typed objects, links, facets and reviews
  (`object_*`), exact question answers and numeric traces (`question_routes`),
  typed transform rows and lineage (`transform_routes`), and the work queue
  (`inbox`).
- Governed actions: `actions` (HOST-ACTION-1) is the one action service shared
  by transports; it authorizes, admits scope and dispatches to the kernel's
  `commit_action`. `action_catalog` lists what an actor may call; `approvals`
  parks an approval-gated call as a decision (`decisions`).
- Definitions: `cell_definitions` plans and applies a running cell's kernel
  definitions (HOST-INSTALL-1, `POST /v1/cell/definitions:plan` and `:apply`);
  `definition_routes` admits, installs and retires retained definitions;
  `provision` is the tenant provisioning contract.
- Runs: `run_routes` starts and reads runs of installed flows.
- Analytics: `analytics_routes` answers questions, loads datasets and
  exports rows on the tenant analytics engine. It takes typed operations only,
  never SQL, and the cell verifies every engine answer.
- Data in: `sources` and `pull` sync declared HTTP sources; `uploads` signs
  storage URLs, profiles CSV and loads rows.
- Privacy: `privacy` projects marked properties and governed reveals;
  `privacy_guard` tokenizes model prompts.
- Identity and refusals: `principal` (verified host tokens), `authz`
  (default-deny server authorization), `scope`, `refusal` and
  `refusal_disclosure`.

The replay clock is a host prerequisite: the pure `Clock` interface supplies
kernel logical time, and `ManualClock` supports deterministic replay through
shared `Arc` handles. Wall-clock reads stay in the hosts.

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
permissions from tool names, schemas or annotations. `norn mcp` advertises
only `norn.capabilities` and `norn.check`; the CLI's other commands (`explain`,
`cell plan`/`cell apply`, `eval`, `remote`) are not MCP tools there and refuse
as `backend unavailable`, not stubbed. A hosted cell serves its own catalog,
including one tool per action the caller may call, at `POST /v1/mcp` (see
`norn-cell-do`).

Scripted discovery/check/refusal/repair acceptance (not a live-LLM evaluation):
`python3 scripts/demo/mcp-check-task.py <norn-binary> <compiled-quickstart.json>`.
Protocol correspondence tests additionally retain a real kernel invariant pin
and its operands, and prove that an already-redacted refusal stays redacted.

JSON-RPC parameter refusals preserve the validated request ID and use -32602;
malformed-parameter notifications stay silent. Ping returns an empty result
before or after initialization and does not unlock tool access.
