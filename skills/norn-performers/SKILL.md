---
name: norn-performers
description: Write thin, stateless external world-boundary performers for Norn Cells behind Norn.fn / Norn.perform using Core-07 / D03 uncertainty recovery (outcome-unknown -> reconciliation), scoped principal identity binding, and authenticated Function receipts.
---

# Norn World-Boundary Performers (`norn-performers`)

Use this skill whenever a Norn application must interact with an external system: payment/transfer providers, messaging/webhook platforms, external compute/optimization solvers, or read-only legacy data sources.

## The Boundary Rule

**Performers NEVER own business truth.**
- A performer is **stateless glue** translating between an external wire protocol and governed Norn Cell actions/outbox steps.
- **No shadow databases:** Performers must not maintain parallel business tables, balances, or approval state. All durable truth lives in the Norn Cell.
- **No policy shortcuts:** Performers never bypass cell authorization, law checks, or approval gates.

## Authoring External Calls (`Norn.fn` + `Norn.perform`)

Declare external capabilities with typed signatures and invoke them from compiled flows rather than ad-hoc side effects:

```ts
const dispatchTransfer = Norn.fn<TransferInput, TransferOutput, TransferError>({
  id: "acme.provider.dispatch_transfer",
  version: "1",
})

// Inside a compiled Norn.flow Effect.gen body (never discard the yielded effect):
const out = yield* Norn.perform("dispatch", dispatchTransfer, input)
```

## Outbox & External Effect Safety (Core-07 / D03)

For any non-idempotent external side effect:

1. **Neutral D03 lifecycle inside the cell:**
   `requested -> dispatched -> succeeded | failed | outcome-unknown`
   - Reserve `failed` strictly for documented pre-effect refusals proving the external side effect did not occur.
   - Any transport timeout, dropped connection, HTTP 5xx, or ambiguous response **MUST** record **`outcome-unknown`** — never blindly retry a non-idempotent effect.
2. **Preserve the operation ID across attempts:**
   - Mint one stable **operation ID** (not merely the target object ID, which would conflate two separate effects on the same object) and preserve it across retries and inquiries.
3. **Recovery and compensation:**
   - Resolve `outcome-unknown` via the provider's explicit idempotency or status-lookup (`inquire`) contract, or require explicit reconciliation evidence before marking terminal.
   - Post-settlement reversals or refunds require **separately authorized compensation** (never silent mutation of a completed effect).
   - Label any provider-specific uncertainty limits honestly (e.g., if a provider lacks lookup-by-operation-ID, require manual reconciliation).
4. **Transport guardrails:**
   - Default adapters to explicit `emulator` / `sandbox` transports unless live credentials and production pins are explicitly supplied via host secrets.

## Compute & Solver Performers

- Pass only unmarked operational inputs authorized by the cell's reveal rules.
- Return a scoped, **authenticated Function receipt** bound to the function manifest, run, and generation (Core-07 receipt acceptance).
- Never trust a bare digest over arbitrary solver JSON as proof of authenticity or law satisfaction; the cell action must still verify the authenticated receipt binding and enforce all domain laws before committing.

## Inbound Webhooks & Identity Binding

- Verify cryptographic signatures at the edge (webhook secret header + constant-time HMAC verification + timestamp/freshness window to prevent replay).
- Bind an **authenticated external provider subject** (verified user ID from the signed provider payload) to a scoped Norn principal — never rely on a shared or unverified phone number, which cannot distinguish two callers, and never reuse a single service-wide token for end-user actions.
- Dispatch `POST /v1/actions/{actionId}` with `{ input, idempotencyKey }` in the JSON body (minting one opaque `idempotencyKey` per user intent and reusing it on retry) and handle all three public `CallResult` outcomes: **`receipt | pending | refused`** (HTTP `202` = pending decision, not committed).

## What to Test

External wire adapters merit focused, fast contract tests covering:
- Provider status/code normalization onto `requested | dispatched | succeeded | failed | outcome-unknown`
- Pre-flight auth/validation refusal vs. post-dispatch `outcome-unknown` + `inquire` recovery
- Payload shaping, constant-time HMAC + replay window checks, and injection-safe serialization (e.g., neutralizing spreadsheet formula prefixes on CSV exports).
