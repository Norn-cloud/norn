---
name: norn-app-author
description: Build a Norn-native business application in typed TypeScript (@norn/authoring) verified by norn-checker and installed directly onto a Norn Cell — with zero custom CRUD backend and zero shadow SQLite/Postgres.
---

# Norn App Authoring (`norn-app-author`)

Use this skill whenever designing or building a business application, vertical pack, or tenant overlay on Norn.

## Non-Negotiable Architecture Rules

1. **The Norn Cell IS the backend.**
   - **Never** build a parallel Bun/Hono, Express, FastAPI, or bespoke SQLite/Postgres CRUD server to mirror Norn definitions.
   - All entities, lifecycles, laws, actions, markings, reveal policies, and localized labels compile to canonical JSON IR and install directly into the tenant's Norn Cell.
2. **Typed authoring only (`@norn/authoring`).**
   - Author entities (`Norn.type`), laws (`Norn.law`), witnesses (`Norn.example`), governed actions (`Norn.action`), fail-closed authorization (`Norn.access`), markings (`Norn.markings`), and localized labels (`Norn.labels`) in TypeScript.
   - Never hand-write JSON IR. Never use `Norn.unsafeZen` when a typed `Norn.expression(Schema, self => ...)` works.
3. **Know what `norn check` and `Norn.example` prove — and what still needs tests.**
   - `Norn.example(name, { of, given, expect })` evaluates one declared law or norm on one concrete value. It does **not** certify all lifecycle, permission, replay, or concurrency behavior and does **not** replace CI.
   - Pair `norn check` + `Norn.example` with model-based / integration tests on `norn dev` (or cell testkit) and contract tests on external world-boundary adapters (see `norn-performers`).

## Modeling Checklist

1. **Entities (`Norn.type`):**
   - Separate `role: "thing"` (durable records such as accounts, locations, policies, templates) from `role: "happening"` (time-anchored commitments, events, requests, runs, transfers).
   - Reference related records with `links` (never bare string foreign keys).
   - Use exact scalar constructors (`Norn.decimal(scale)`, `Norn.date()`, `Norn.timestamp("utc")`) — never floats for money or quantities.
   - Declare domain markings via `Norn.markings(...)` and attach them to sensitive properties via `propertyMarkings` (including correlated/derived fields that could leak a marked value).
2. **Lifecycles:**
   - Declare `initial`, `terminal`, `immutable`, and explicit `transitions`.
   - **No self-transitions:** Every transition must leave one state and enter a different state (`from: ["draft"], to: "calculated"`, with a separate `reopen` transition if recalculation is allowed).
3. **Laws + Witnesses (`Norn.law` + `Norn.example`):**
   - Pair every critical business invariant with at least one `expect: "holds"` and one `expect: "violated"` `Norn.example`.
4. **Fail-Closed Authority (`Norn.access` + `Norn.markings`):**
   - Every governed action should declare `scopeRequired: ["orgId", "environmentId"]`, `idempotent: true`, `ledgered: true`, and `access` (accepting either a definition reference or an `{ artifactId, version }` pin).
   - Structure access policies with `Norn.access({ id, title, version, rules: [...] })`. Every rule requires `ruleId`, `actions`, `resourceType`, and `scope: { dimensions: [...] }`, plus explicit conditions for authorized roles and target markings.
   - Authorize disclosure of restricted fields with a rule granting `actions: ["norn.reveal"]` on `resourceType: "norn.Marking"` gated by marking and role/scope conditions; the cell redacts unauthorized fields **before serialization**.
5. **Multilingual Labels (`Norn.labels`):**
   - Attach operator-facing labels and enum translations directly to the IR so UIs stay thin and consistent across locales.

## Authoring & Verification Loop (CLI + MCP)

`norn check` requires the Rust checker WASM locally (`NORN_CHECKER_WASM_PKG`) and can admit with warnings — inspect diagnostics even when `admitted: true`.

```bash
# 1. Verify a source file stays inside the supported authoring profile
norn profile "$SOURCE_FILE"

# 2. Run authoritative Rust checker admission
norn check src/ --org "$ORG_ID" --env "$ENV_ID"

# 3. Compile canonical IR bundle (for provisioning or archival)
norn compile src/ --org "$ORG_ID" --env "$ENV_ID" --out "dist/${PACK_NAME}.ir.json"

# 4. Inspect laws, reads, writes, and refusal frames (run in project context)
norn explain action "$ACTION_ID"
norn explain law "$LAW_ID"

# 5. Plan & apply against a hosted Rivet or DO cell
norn plan src/ --url "$CELL_URL" --org "$ORG_ID" --env "$ENV_ID" --token-env NORN_TOKEN
norn apply src/ --url "$CELL_URL" --org "$ORG_ID" --env "$ENV_ID" --token-env NORN_TOKEN
```

### MCP Interfaces
- **Local stdio (`norn mcp`):** Exposes `norn.capabilities` and `norn.check` only (do not assume `explain`, `plan`, or `apply` exist on stdio MCP).
- **Installed-cell discovery:** Query the cell's HTTP MCP endpoint (`POST /v1/mcp`) for the capabilities admitted on a live tenant scope.
