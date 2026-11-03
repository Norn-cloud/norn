# Core wire contracts

Projects the Rust IR and checker schemas into OpenAPI components for the TypeScript SDK.
Run `norn openapi` to regenerate the contract. Cell execution uses `norn-cell` and
`norn-kernel` directly; the fixture example supplies a local definitions server.

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

## Host admission epoch (slice 1b)

HOST-ADMISSION: `DevHost::new` checks a full source array before initializing a
fresh tenant store: unrelated residual schema/state refuses. An identical
initial provisioning request reopens without writing. Kernel action schemas
change the provisioning hashes: pre-schema persisted stores require
reprovisioning; no in-place upgrade is provided.
Only a successful transactional definition install publishes
the host. Trusted installs check before taking the exclusive definition epoch;
failed admission and stale-version installs preserve both revision and ledger.
Readers hold the shared epoch through authorization, store load and projection.
HOST-EXACT: retained source uses the existing RuntimeValue path, and the checker
issues its receipt unchanged. HOST-RECEIPT is **not claimed**: #290/#273 track the
checker identity gap. No additional host canonicalizer is introduced.
HOST-IDENTITY: artifacts, scope and logical time remain the canonical IR/kernel
types. The API now owns a ring-1 host over ring-1 cells; pure contracts stay here.
Correspondence: `host::tests`. HTTP reads and action parity follow in later slices.

HOST-SCOPE/HOST-AUTHORITY: scope and `ReadDefinitions` precede store queries (SQL spy); default deny.
HOST-ADMISSION: one definition epoch covers load and projection; retained versions remain readable.
HOST-WIRE/HOST-EXACT: raw source/receipt JSON, camelCase, empty collections and exact numeric vectors.
HOST-REFUSAL: absent version `defs.version_not_found` (404); malformed query `request.invalid` (400).
HTTP requires an authenticated routing host to strip all client-supplied x-norn-actor
values and set the header from its authenticated identity; never expose the router
directly. Host errors conceal diagnostics.

HOST-PROVISION-ATOMIC: initialization, scope binding, initial definitions, model
snapshot, receipt and ledger publish in one engine transaction. Failure or abrupt
process loss before commit leaves no application schema; after commit a lost
reply leaves the complete initial revision. An identical provisioning request
(scope, projected model and full install input, including logical time) reopens
the current revision without writes, including after later trusted installs; a
changed provisioning request refuses. Residual unrelated schema is never adopted. The transaction owns the
freshness check, so competing requests cannot both initialize the cell.
Correspondence: `definitions_install::host_provision_atomic_at_every_storage_boundary`
injects errors and abrupt subprocess exit after every SQL operation, before
commit and after commit, then reopens, retries and checks conflicting retries.


HOST-FIXTURE-BOOTSTRAP: the fixture executable combines exact artifact arrays,
checks and atomically installs the complete SalesOrder/stock set into a new
in-memory tenant on every start. It binds loopback port 4174. Bearer authentication
removes caller actor headers and supplies a fixed read principal only after a
credential match; anonymous, incorrect-token and spoofed-actor requests deny.
Correspondence: the example's authentication matrix and a real TCP Boat smoke
that compiles stock, starts the command and checks both namespaces via `/v1/defs`.

From the repository root (fixture sources require #147 and #227):

```sh
(cd tools/norn-ts && bun install --frozen-lockfile --backend copyfile) && mkdir -p ~/.cache/agent-scratch/norn-demo-host && bun tools/norn-ts/src/cli.ts compile tools/norn-ts/fixtures/engine-alpha --org org_demo --env dev --out ~/.cache/agent-scratch/norn-demo-host/alpha.json && bun tools/norn-ts/src/cli.ts compile tools/norn-ts/fixtures/stock --org org_demo --env dev --out ~/.cache/agent-scratch/norn-demo-host/stock.json && NORN_DEMO_TOKEN=local-demo cargo run --locked -p norn-api --example fixture-server -- ~/.cache/agent-scratch/norn-demo-host/alpha.json ~/.cache/agent-scratch/norn-demo-host/stock.json
```

Read with `Authorization: Bearer local-demo` at
`http://127.0.0.1:4174/v1/defs?orgId=org_demo&environmentId=dev`.
The environment credential is local fixture configuration. This host exposes
definition reads; actions and subscriptions remain the adapter lane's scope.
HOST-MODEL-DEFAULT-SNAPSHOT remains unclaimed: numeric JSON object defaults use
the legacy floating-point IR and need a shared representation migration.
HOST-VERSION-ABSENT: every syntactically valid but unrepresentable revision is
absent (404), including u64::MAX and the first integer above the SQL seam limit.
HOST-HTTP-WORKER: synchronous store access and complete retained-set admission
run on the blocking pool, outside the Tokio request worker; actor/scope admission
still precedes store reads. HTTP correspondence covers allowed/denied actors.

HOST-CACHE-PRIVATE: all definition HTTP responses, including refusals, declare
`Cache-Control: no-store`; scope/actor-sensitive results never authorize shared
cache reuse. The HTTP success/refusal matrices assert the header.

HOST-FIXTURE-SCOPE: both authored sets compile with the same explicit scope;
the original org_alpha golden is never adopted by the org_demo host.

## Governed action surfaces

The shared Rust entrypoint for HTTP and Workers/DO hosts is
`norn_api::actions::call_action_in_epoch`:

```rust,ignore
pub fn call_action_in_epoch<E: norn_cell::engine::SqlEngine>(
    host: &norn_api::host::DevHost<E>,
    scope: &norn_api::routes::DefinitionsQuery,
    actor: Option<&str>,
    request: norn_api::actions::ActionRequest,
) -> Result<norn_api::actions::ActionResponse, norn_api::refusal::Refusal>
```

This service and its request/receipt types are available without `http-host`.
`DefinitionsQuery` supplies plain organization/environment data; the action's
definition revision is `ActionRequest.version`. A trusted adapter supplies the
authenticated actor. The service owns scope admission, authorization, the
definition epoch, governed kernel commit and idempotent replay. Adapters decode
requests and serialize the returned `ActionResponse` or shared `Refusal`.
`ActionResponse.record` is the original kernel `ActionCallRecord`. Input and
receipt OpenAPI schemas are generated from these same Rust types.

SURFACE-ACTION-1: HTTP `POST /v1/actions`, MCP `call_action` and the remote
CLI invoke the same admitted ActionIR through `commit_action`. No application
opcode, stock schema or domain dispatch registry belongs to this host.

SURFACE-ACTION-2: exact input, action identity, definition version and retry
identity are bound to one definition epoch. Scope and authenticated actor are
transport metadata. Stale revisions refuse before mutation.

SURFACE-DENY-1/2: host, action and declared-read policies default to deny.
Refusals preserve kernel code, category and readable message. Policy and guard
refusals leave the complete committed dump unchanged, including call records.
Law refusals preserve business state and retain the kernel audit receipt, as
specified by SURFACE-DEMO-3 below.

SURFACE-RETRY-IDENTITY: conflicting MCP argument and metadata identities refuse.
SURFACE-CLOCK-PIN: turns use the persisted host clock floor.
SURFACE-SCHEMA-1: kernel call/owned-row schemas are registered by the host;
tenant definitions cannot replace their reserved names, including SQLite's
ASCII case-insensitive equivalents.
SURFACE-CLOCK-INSTALL: installing definitions advances both the durable clock
floor and the driver clock used by subsequent host commits.

SURFACE-ALPHA: on three fresh identical admitted alpha cells, create a customer
and SalesOrder, submit, approve and cancel. Compare call records, complete dumps
and refusals through HTTP, MCP and the compiled CLI. Exercise a lifecycle guard
refusal and a forbidden actor, checking no writes for each. This is sampled
correspondence evidence. The stock CP1 vector remains pending declared
receipt/delivery rules; it does not qualify the generic surfaces.

Run the sampled three-surface acceptance with the compiled CLI:

```sh
cargo build --locked -p norn-cli
NORN_CLI="$(cd "${CARGO_TARGET_DIR:-target}" && pwd)/debug/norn" cargo test --locked -p norn-api surface_alpha_parity_and_no_write_refusals -- --ignored
```

The cancellation guard vector starts from an approved order with a trusted
fixture invoice count of one. The refusal code is `decision.guard_false`. It qualifies the declared guard and transport
refusals; it does not claim to run the asynchronous invoice-fold workflow.

SURFACE-EXACT-JSON: nested exact decimals and integers retain their values
in committed JSON fields, HTTP records and MCP structured content/text.

SURFACE-PROTOCOL: MCP failures preserve request id and refusal data without writes.

## Equivalent governed demo surfaces

SURFACE-UI-1: `GET /v1/defs` with `Accept: text/html` renders the same authorized
server document as an action workbench. It submits raw exact input to the existing
`POST /v1/actions`; the renderer never evaluates permissions, guards or laws.
The authenticated hosting adapter remains responsible for replacing actor headers.
HTML and JSON responses are private, non-cacheable and vary by Accept.

SURFACE-DEMO-1: `demo.melt.record` records an immutable measurement implementing
`demo.ConservedFineMass`. Its inherited integrity invariant is the response of
`demo.fine_mass_conservation`: nonnegative input = output + recorded loss, in
fine grams at scale 3. This bounded jewelry measurement is not the full stock,
books, approval or sell/scrap/melt application acceptance.

SURFACE-DEMO-2: four fresh equivalent admitted DevHost tenants execute the same
100.000 → 99.200 + 0.800 intent through HTTP, MCP, the compiled remote CLI and
Chromium clicking the real server-rendered form. Complete action records and
complete tenant dumps agree. A cross-transport retry returns the same record
without another consequence. Browser form submission and displayed responses
are exercised without intercepting or mocking network requests.

SURFACE-DEMO-3: a forbidden actor gets `authz.denied` and leaves the complete dump
unchanged. Omitting loss gets `core.laws.violated`; all business,
action-call, outbox and timer state stays unchanged. The kernel deliberately
retains one `invariant_refused` audit event with its receipt/clock/export journal;
these audit records are included in complete cross-transport dump comparisons.
The browser loads its document as an allowed actor before submitting as the
forbidden actor, proving a previously rendered document grants no authority.

On Boat, install the pinned browser package and Chromium (including its OS
libraries) once, then run the one acceptance command:

```sh
(cd packages/norn-surface-acceptance && bun install --frozen-lockfile --backend copyfile && bunx playwright install --with-deps chromium)
bash scripts/demo/equivalent-surfaces.sh
```

The entrypoint compiles the typed fixture, builds the real CLI and explicitly
runs the ignored acceptance. Missing definitions, CLI, browser or admission are
failures, never skips. The in-process trusted actor adapter and loopback listener
are test infrastructure; this does not authorize a public deployment.
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
