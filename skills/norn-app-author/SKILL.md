---
name: norn-app-author
description: Build a Norn-native business application in typed TypeScript (@norn/authoring) verified by norn-checker and installed directly onto a Rivet Actor or Durable Object Norn Cell — with zero custom CRUD backend, zero shadow SQLite/Postgres, and minimal bespoke tests.
---

# Norn App Authoring (`norn-app-author`)

Use this skill whenever designing or building a business application, vertical pack, or tenant overlay on Norn.

## Non-Negotiable Architecture Rules

1. **The Norn Cell IS the backend.**
   - **Never** build a parallel Bun/Hono, Express, FastAPI, Django, or Frappe CRUD server with its own SQLite/Postgres tables to "mirror" Norn definitions.
   - All entities, lifecycles, laws, actions, PII markings, reveal policies, and localized labels compile to canonical JSON IR and install directly into the tenant's **Rivet Actor (`norn-cell-rivet`)** or **Durable Object (`norn-cell-do`)** cell.
2. **Typed authoring only (`@norn/authoring`).**
   - Author entities (`Norn.type`), laws (`Norn.law`), concrete positive/negative witnesses (`Norn.example`), governed actions (`Norn.action`), fail-closed authorization (`Norn.access`), PII markings (`Norn.markings`), and localized labels (`Norn.labels`) in TypeScript.
   - Never hand-write JSON IR. Never use `Norn.unsafeZen` when a typed `Norn.expression(Schema, self => ...)` works.
3. **`norn-checker` replaces bespoke business-logic unit tests and heavy CI.**
   - Do **not** write redundant unit tests proving that a lifecycle transition, positive-number guard, or role restriction works — declare it as a `Norn.law`, `lifecycles` guard, or `Norn.access` rule with `Norn.example` witnesses and let `norn check` + cell admission prove it before install.
   - Reserve traditional automated tests strictly for external world-boundary wire adapters (see `norn-performers`).

## Modeling Checklist

1. **Entities (`Norn.type`):**
   - Separate `role: "thing"` (durable records: people, branches, policies, templates) from `role: "happening"` (time-anchored commitments, events, requests, runs, payouts).
   - Reference related objects with `links` (never bare `*_id` strings).
   - Use exact decimals (`Norn.decimal(scale)`), `Norn.date()`, and `Norn.timestamp("utc")` — never floats for money or quantities.
   - Mark sensitive fields via `propertyMarkings` (`pii.compensation`, `pii.gov_id`, `pii.demographic`, `pii.biometric_photo`, `pii.medical`).
2. **Lifecycles:**
   - Declare `initial`, `terminal`, `immutable`, and explicit `transitions`.
   - **No self-transitions:** A transition must leave one state and enter a different state (`from: ["draft"], to: "calculated"`, with a separate `reopen` transition if recalculation is allowed).
3. **Laws + Witnesses (`Norn.law` + `Norn.example`):**
   - Every critical business invariant gets a `Norn.law` paired with at least one `expect: "holds"` and one `expect: "violated"` `Norn.example`.
4. **Fail-Closed Authority (`Norn.access` + `Norn.markings`):**
   - Every action declares `scopeRequired: ["orgId", "environmentId"]`, `idempotent: true`, `ledgered: true`, and `access: { artifactId, version }`.
   - Define a reveal policy (`Norn.access` with `actions: ["norn.reveal"], resourceType: "norn.Marking"`) so restricted fields (e.g., compensation, government IDs) are redacted **by the cell before serialization**.
5. **Multilingual Labels (`Norn.labels`):**
   - Attach operator-facing labels and enum translations (e.g., Arabic + English) directly to the IR so UIs stay thin and consistent.

## Authoring & Verification Loop (CLI + MCP)

Run against the Rust checker WASM (`NORN_CHECKER_WASM_PKG`) or a live cell:

```bash
# 1. Verify syntax stays inside the supported authoring profile
norn profile src/

# 2. Run authoritative Rust checker admission (must return admitted: true, 0 diagnostics)
norn check src/ --org <orgId> --env <envId>

# 3. Compile canonical IR bundle (for cell provisioning / archival)
norn compile src/ --org <orgId> --env <envId> --out dist/<pack>.ir.json

# 4. Inspect laws, reads, writes, and refusal frames
norn explain action <action.id>
norn explain law <law.id>

# 5. Plan & apply directly to a live Rivet or DO cell (no custom CI needed!)
norn plan src/ --url https://<cell-host> --org <orgId> --env <envId> --token-env NORN_TOKEN
norn apply src/ --url https://<cell-host> --org <orgId> --env <envId> --token-env NORN_TOKEN
```

When using the Norn MCP server (`norn mcp`), prefer discovering installed capabilities and calling `explain` / `plan` / `apply` directly rather than inventing shell glue.
