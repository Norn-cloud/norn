---
name: norn-performers
description: Write thin, stateless external world-boundary performers (payment rails, messaging bots, CP-SAT/ML solvers, read-only legacy syncs) for Norn Cells using Norn.fn, atomic outbox dispatch, durable idempotency, and inquiry-before-retry recovery.
---

# Norn World-Boundary Performers (`norn-performers`)

Use this skill whenever a Norn application must talk to the outside world: payment/payout rails, messaging platforms (Telegram/WhatsApp/SMS), optimization/ML solvers, or read-only legacy ERP scrapers.

## The Boundary Rule

**Performers NEVER own business truth.**
- A performer is **stateless glue** translating between an external wire protocol and the Norn Cell's checked actions/outbox.
- **No shadow databases:** Performers must not maintain their own business tables, balances, or approval state. All durable truth lives in the Norn Cell.
- **No policy shortcuts:** Performers never bypass cell authorization, law checks, or approval gates. Every inbound webhook (e.g., Telegram button click, payment callback) invokes a governed cell action with explicit actor/scope credentials.

## Outbox & Money-Moving Safety (D03)

For any non-idempotent external side effect (especially payments/payouts):

1. **Stable operation identity:** Generate a deterministic `client_reference_id` / `merchantTransferId` derived from the immutable Norn object ID (e.g., `PAY-<period>-<employeeCode>`).
2. **Neutral state machine inside the cell:** Map provider-specific status codes onto Norn's neutral states:
   `created -> pending -> succeeded | failed | rejected | returned | unknown`
3. **Never blindly retry on ambiguous outcomes:**
   - Only a **documented pre-creation refusal** (e.g., an explicit HTTP 400 validation schema response proving nothing was created) transitions directly to `rejected`.
   - Any timeout, network drop, HTTP 5xx, or unrecognized status **MUST** transition to `unknown` (`reconciliation-required`). Recovery happens **only via provider inquiry** by `client_reference_id` before any new attempt.
4. **Reversible / returnable settlements stay open until final:**
   - If a provider marks a transfer delivered with a returnable flag (e.g., `openForReturn: true`), keep it `pending` until settled; map post-delivery bank/wallet returns to `returned` so accounting reverses cleanly.
5. **Transport guardrails:** Default to `simulated` or explicit `emulator` / `sandbox` origins unless live credentials and production environment pins are explicitly supplied via host secrets (Infisical / Worker Secrets).

## Solver & Compute Performers (e.g., OR-Tools CP-SAT)

- Accept an unmarked operational snapshot from the cell (PII/salaries already stripped by `norn.reveal`).
- Return a deterministic output + canonical SHA-256 **solver receipt digest** (or structured infeasibility diagnostics).
- The cell's governed action verifies the receipt digest and checks every domain law before committing proposed assignments.

## Messaging & Webhook Performers (e.g., Telegram Bot / Mini App)

- Verify cryptographic signatures at the edge (webhook secret header + Mini App `initData` HMAC-SHA256).
- Map verified user identities (e.g., shared mobile phone number) to their Norn principal.
- Translate interactive button callbacks (`callback_data`) directly into `POST /v1/actions/{actionId}` calls against the Rivet/DO Cell, surfacing any cell refusal cleanly back to the user in their locale.

## What to Test

Unlike domain rules (which `norn-checker` proves), external wire adapters **do** merit small, fast contract tests covering:
- Provider status/code normalization (`pending`, `succeeded`, `returned`, `unknown`)
- Documented HTTP 400 refusal vs. ambiguous error handling
- Request payload shaping, cryptographic signatures/HMACs, and CSV/export formatting.
