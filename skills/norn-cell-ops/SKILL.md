---
name: norn-cell-ops
description: Provision, update (plan/apply), subscribe to live updates (norn.live.v1), and operate Norn tenant cells hosted on Rivet Actors (norn-cell-rivet), Cloudflare Durable Objects (norn-cell-do), or the local dev host via CLI and MCP.
---

# Norn Cell Operations (`norn-cell-ops`)

Use this skill when provisioning, running, inspecting, upgrading, or troubleshooting a live Norn Cell.

## Host Targets (Same Engine, Same HTTP Contract)

All three hosts run the exact same `norn-cell` + `norn-api` Rust kernel and serve byte-identical routes:

| Host | Crate / App | Backing Store | Best For |
| --- | --- | --- | --- |
| **Rivet Actor Host (Ring 1)** | `crates/norn-cell-rivet`, `apps/norn-cell-rivet` | RivetKit 3.1.1 embedded SQLite (replicated VFS) | Self-hosted / Hetzner / cloud-portable production tenant cells (one actor per tenant cell) |
| **Durable Object Host** | `crates/norn-cell-do`, `apps/norn-cell-do` | Cloudflare DO SQLite | Serverless edge deployment on Cloudflare Workers |
| **Local Dev Host** | `norn dev` (`norn-dev-host`) | Local SQLite | Instant hot-reloading authoring loop |

## Core Cell Endpoints

Every host exposes the same scoped API (`?orgId=<org>&environmentId=<env>`):

- `POST /v1/cell:provision` — Atomically initializes a vacant scope (`HOST-PROVISION-ATOMIC`): submits `{ baseSha256, artifacts }` (<= 1 MiB) as an authorized `owner`. Replays identically on retry; conflicts return `409 cell.provision_conflict`.
- `POST /v1/cell/definitions:plan` — Dry-runs candidate IR updates against live cell state and prints migration/law impact without writing.
- `POST /v1/cell/definitions:apply` — Atomically admits and installs the checked update into the cell's SQLite store.
- `POST /v1/actions/{actionId}` — Executes a governed action (checked against Cedar `AccessIR`, lifecycle transitions, preconditions, and immediate laws in one atomic commit).
- `GET /v1/subscribe` (`norn.live.v1`) — Live WebSocket subscription naming GET routes; after every commit, the cell re-evaluates each subscribed route under the caller's principal and pushes only changed answers.
- `POST /v1/access:decide` — Authoritative permission check for external connectors/surfaces.
- `POST /v1/mcp` — Bounded MCP endpoint exposing the cell's installed capabilities to agents.

## Operating Rules

1. **Where tenant artifacts live:**
   - **Admitted IR & operational policy objects:** Inside the cell's SQLite store (atomic with business data; survives actor restarts via `HOST-REOPEN-1`).
   - **Binary blobs (photos, scans, PDFs, CSV exports):** Object storage (S3 / R2), with only the SHA-256 digest and scoped object key stored in the cell.
   - **Secrets (bot tokens, payment API keys):** Host/Worker secrets (Infisical / env), never in cell tables or git.
   - **Authored TypeScript source:** Tenant's Git repo (linked GitHub App or S3-backed `enroute.sh` in hosted Lovable mode) — compiled and pushed via `:plan` / `:apply`.
2. **Debugging refusals:**
   - Never bypass a cell refusal (`403`, `409`, `422`). Read the structured refusal code (`core.*`, `cell.*`, `law.*`), run `norn explain`, fix the source definition or input, and re-check.
   - A vacant scope answers unverified callers with the same opaque denial as a provisioned cell (never leaks tenant existence); verified org members receive `404 cell.not_provisioned`.
3. **RivetKit upgrades:**
   - `norn-cell-rivet` pins RivetKit (`=3.1.1`) against `STORAGE_VERSION` and `RIVETKIT_CHANNEL`. Always back up/snapshot before upgrading a Rivet engine release; rollback across RivetKit versions requires snapshot restore.
