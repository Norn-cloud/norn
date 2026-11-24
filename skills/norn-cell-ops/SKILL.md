---
name: norn-cell-ops
description: Provision, update (plan/apply), subscribe to live updates (norn.live.v1), and operate Norn tenant cells hosted on Rivet Actors (norn-cell-rivet), Cloudflare Durable Objects (norn-cell-do), or the local norn dev host via CLI and HTTP MCP.
---

# Norn Cell Operations (`norn-cell-ops`)

Use this skill when provisioning, running, inspecting, upgrading, or troubleshooting a live Norn Cell.

## Host Capability Matrix

All three hosts run the `norn-cell` + `norn-api` Rust kernel, with explicit differences in live subscriptions and definition installation:

| Host | Crate / App | Backing Store | `/v1/subscribe` (`norn.live.v1`) | Hosted `:plan` / `:apply` |
| --- | --- | --- | --- | --- |
| **Rivet Actor Host (Ring 1)** | `crates/norn-cell-rivet`, `apps/norn-cell-rivet` | RivetKit `=3.1.1` SQLite | **Yes** (WebSocket upgrade + caller auth; Studio browsers connect via `/api/host/v1/subscribe`) | **Yes** |
| **Durable Object Host** | `crates/norn-cell-do`, `apps/norn-cell-do` | Cloudflare DO SQLite | **Yes** (WebSocket upgrade + caller auth) | **Yes** |
| **Local Dev Host** | `norn dev` (`norn-dev-host`) | Local SQLite | **No** WebSocket upgrade | **No** — returns `409 defs.dev_sources_only` (local project sources own installation) |

## Core Hosted Cell Endpoints

Scoped via `?orgId=<org>&environmentId=<env>`:

- `POST /v1/cell:provision` — Atomically initializes a vacant scope (`HOST-PROVISION-ATOMIC`) as an authorized owner:
  - A `baseSha256` that does not match the host's trusted base returns `cell.base_mismatch`.
  - A conflicting repeat provision returns `409 cell.provision_conflict`.
- `POST /v1/cell/definitions:plan` — Dry-runs candidate IR updates against live cell state without writing (hosted cells only).
- `POST /v1/cell/definitions:apply` — Atomically admits and installs a checked update into the cell's SQLite store using `expectedVersion` (hosted cells only; a stale revision returns `409 defs.stale`).
- `POST /v1/actions/{actionId}` — Executes a governed action with JSON body `{ input, idempotencyKey }` (header alone is insufficient). Mint one opaque `idempotencyKey` per user intent and reuse it on retry. Returns one of three public outcomes: **`receipt`** (committed — or `outcome: "replayed"` on identical retry — carrying receipt hash/keyed handle, commit sequence, and logical time), **`pending`** (HTTP `202` pending decision, not yet committed), or **`refused`** (structured refusals across `400`, `403`, `409`, `410`, `413`, `422`, and `429`).
- `GET /v1/subscribe` (`norn.live.v1`) — Authenticated WebSocket live read subscription on Rivet and DO hosts (re-evaluates subscribed GET routes under the caller's principal after commits and pushes changed answers).
- `POST /v1/access:decide` — Bounded connector authorization checks for the verified principal.
- `POST /v1/mcp` — HTTP MCP endpoint exposing the cell's installed capabilities to agents (distinct from local stdio `norn mcp`, which exposes `norn.capabilities` and `norn.check` only).

## Operating Rules

1. **Where artifacts, blobs, and secrets live:**
   - **Admitted IR & operational state:** Inside the cell's SQLite store (atomic with business data; survives actor restarts via `HOST-REOPEN-1`).
   - **Tenant connector credentials:** Sealed inside the cell vault (`_norn_vault_sealed`) — never store plaintext credentials in authored IR, ordinary business records, or Git.
   - **Host secrets (signing keys, provider tokens, JWKS config):** Supplied via host/runtime secret management (env / secret store), never in cell tables or Git.
   - **Binary blobs (documents, attachments, exports):** External object storage (e.g., S3/R2 as a deployment choice), storing only scoped references and digests inside the cell.
   - **Authored TypeScript source:** Versioned in Git, checked locally with `norn check`, and deployed via `norn plan` / `norn apply`.
2. **Debugging refusals:**
   - Never bypass a cell refusal (`403`, `409`, `422`). Read the structured refusal code (`core.*`, `cell.*`, `law.*`), run `norn explain`, fix the source definition or input, and re-check.
   - A vacant scope answers unverified callers with the same opaque denial as a provisioned cell (never leaks tenant existence); verified org members receive `404 cell.not_provisioned`.
3. **RivetKit upgrades:**
   - `norn-cell-rivet` pins RivetKit (`=3.1.1`) against `STORAGE_VERSION` and `RIVETKIT_CHANNEL`. Always back up/snapshot before upgrading a Rivet engine release; rollback across RivetKit versions requires snapshot restore.
