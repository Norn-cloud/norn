# Build a purchase-request app

This walks [`fixtures/examples/purchase-requests`](../../fixtures/examples/purchase-requests):
types, a lifecycle with a guard, a law, access rules, an approval, and an
ERPNext export mapped in. The commands run from the repository root; the
fixture's test (`tools/norn-ts/test/purchase-requests.test.ts`) runs the same
steps.

Prerequisites: Rust 1.98.0, Bun 1.3.14, `bun install --frozen-lockfile` in
`tools/norn-ts`, and the dev host:

```sh
cargo build -p norn-cell-do --bin norn-dev-host
export NORN_DEV_HOST_BIN="$PWD/target/debug/norn-dev-host"
cp -r fixtures/examples/purchase-requests /tmp/purchase-requests
```

## Types

[`src/purchasing.ts`](../../fixtures/examples/purchase-requests/src/purchasing.ts)
declares a `Supplier` and a `PurchaseRequest`. A request links to its supplier
(`links: [{ id: "supplier", … }]`); the create action makes the link from the
`supplier_id` the caller picks.

The requester is never typed in. The create action declares
`fromCaller: ["requester"]`, so the server sets it to whoever calls. A caller
who sends `requester` is refused with `action.invalid_input`.

## Lifecycle and guard

`status` goes `draft → submitted → approved | rejected`. Requests up to 1000
skip review through their own transition, `draft → approved` (`submit_small`).
Each path has a guard:

```ts
{ id: "submit", from: ["draft"], to: "submitted", guards: [Norn.expression(Amount, (self) => self.amount > 1000)] },
{ id: "submit_small", from: ["draft"], to: "approved", guards: [Norn.expression(Amount, (self) => self.amount <= 1000)] },
```

Studio's Workflows page shows these as "Requires: amount > 1000". A refused
call says the same thing. A record shows only the actions its current state
can take, because the cell narrows `GET /v1/actions?objectKind=…&objectId=…`.

## Law

```ts
Norn.law("currency_matches_supplier", {
  on: PurchaseRequest,
  must: (self) => self.amount > 0 && self.currency === self.supplier.currency,
  because: "A purchase request is for a positive amount in its supplier's currency.",
})
```

The cell checks this law on every write. A request in EGP for a USD supplier
is refused with the law's name and the values it checked.

## Access

`Norn.access` gives the requester the create and submit actions
(`requester eq principalAttribute "id"`). Approve and reject go to an admin
who is not the requester (`requester ne principalAttribute "id"`). The
principal's `id` is the verified caller. `norn dev` enforces these rules for
both local principals, exactly as a hosted cell does. A developer who approves
their own request gets `403 authz.denied`.

## Approval

```ts
Norn.approval({
  id: "purchasing.large_requests", action: Submit, on: PurchaseRequest,
  when: { property: "amount", op: "gt", value: "1000" },
  decider: { roles: ["admin", "owner"] },
  question: "Sign off this purchase request over 1000?", risk: "medium",
})
```

A submit over 1000 does not commit right away. It waits as a decision, and its
requester can neither see nor decide it. `norn dev` installs the approval from
the source the way a hosted installer does: admit, read the server's
restatement, then install signed. The approval commits with the set in one
step, so the submit action is never live without its gate, and an approval the
cell refuses publishes nothing. The `definition` event prints that
restatement.
It reads `A call of purchasing.request.submit that writes a record with
amount > "1000" waits for a person to approve it, …`. A text operand is
quoted and a number is bare, so the words signed pin the operand exactly as
installed: a sign-off of one never installs the other.

## The ERPNext export

[`src/erpnext.ts`](../../fixtures/examples/purchase-requests/src/erpnext.ts) has
two parts:

- the source schema, describing `Supplier` and `Material Request` as ERPNext
  shapes them;
- the mapping onto the purchasing types.

The mapping binds the schema by reference (`source: ErpnextSchema`), so the
checker pins its hash and nothing is pasted. A project with a mapping gets
the cell-owned `norn.ObservedLinkClaim` types automatically.

[`data/erpnext.json`](../../fixtures/examples/purchase-requests/data/erpnext.json)
is a recorded export:

```json
{ "source": "erpnext", "mapping": "purchasing.erpnext.mapping", "connectorInstance": "demo",
  "snapshot": [{ "dataset": "Material Request",
                 "record": { "name": "MAT-MR-2026-00002", "grand_total": "7800.00", "currency": "USD", … } }] }
```

ERPNext sends amounts as exact decimal text (`"7800.00"`), and the mapping
accepts that text for decimal fields. A row it cannot read stops the load and
names the field, the value and the expected type. For example:
`field "grand_total" value "7,800.00" is not a number`.

## Run it

Check the project with the same loader `norn dev` uses. The checker needs
`NORN_CHECKER_WASM_PKG`; see QUICK_START.

```sh
NORN="bun $PWD/tools/norn-ts/src/cli.ts"
$NORN check /tmp/purchase-requests
```

Serve it and load the export (leave it running), then open Studio as the
developer and as the reviewer:

```sh
(cd /tmp/purchase-requests && $NORN dev --load data/erpnext.json)
cd apps/studio && bun install --frozen-lockfile && bun run checker && bun run build
bun run local --project /tmp/purchase-requests                        # http://127.0.0.1:5180
bun run local --project /tmp/purchase-requests --as reviewer --port 5181
```

Then, in the developer's Studio:

1. Create a 2,500 USD request.
2. Submit it. It waits for a decision.

In the reviewer's Studio:

3. Sign off the request in Decisions.
4. Approve it.

The developer's own Approve is refused.

Not covered yet: laws over many records or over time windows (for example, a
monthly spend cap per requester).
