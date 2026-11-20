// Purchase requests: the regression fixture for the builder loop (#1450).
// Types, lifecycle, guard, law, actions, access and approval, in one module.
import { Norn } from "@norn/authoring"
import { Schema } from "effect"

const Currency = Schema.Literals(["EGP", "USD"])
const Amount = Schema.Struct({ amount: Norn.decimal(2) })

/** What a supplier and a request share: the currency they deal in. */
export const Priced = Norn.interfaceType({
  name: "purchasing.Priced", title: "Priced", version: "0.1.0",
  properties: Schema.Struct({ currency: Currency }),
})

export const Supplier = Norn.type({
  name: "purchasing.Supplier", title: "Supplier", version: "0.1.0", implements: [Priced],
  properties: Schema.Struct({ supplier_id: Schema.String, supplier_name: Schema.String, currency: Currency }),
  identity: [{ name: "primary", properties: ["supplier_id"] }],
})

export const PurchaseRequest = Norn.type({
  name: "purchasing.PurchaseRequest", title: "Purchase request", version: "0.1.0", implements: [Priced],
  properties: Schema.Struct({
    request_id: Schema.String,
    amount: Norn.decimal(2),
    currency: Currency,
    requester: Schema.String,
    supplier_id: Schema.String,
    status: Schema.Literals(["draft", "submitted", "approved", "rejected"]),
  }),
  identity: [{ name: "primary", properties: ["request_id"] }],
  links: [{ id: "supplier", target: Supplier, cardinality: { to: "one", from: "many", required: true } }],
  defaults: { status: "draft" },
  lifecycles: [{
    property: "status",
    initial: ["draft"],
    terminal: ["approved", "rejected"],
    transitions: [
      // Over 1000 a request waits in `submitted` for purchasing.
      { id: "submit", from: ["draft"], to: "submitted", guards: [Norn.expression(Amount, (self) => self.amount > 1000)] },
      // Up to 1000 it is approved at once: a separate path, not a skipped state.
      { id: "submit_small", from: ["draft"], to: "approved", guards: [Norn.expression(Amount, (self) => self.amount <= 1000)] },
      { id: "approve", from: ["submitted"], to: "approved" },
      { id: "reject", from: ["submitted"], to: "rejected" },
    ],
  }],
})

export const CurrencyMatchesSupplier = Norn.law("currency_matches_supplier", {
  on: PurchaseRequest,
  must: (self) => self.amount > 0 && self.currency === self.supplier.currency,
  because: "A purchase request is for a positive amount in its supplier's currency.",
})

const RequestRef = Schema.Struct({ request_id: Schema.String })

export const CreateRequest = Norn.action({
  id: "purchasing.request.create", title: "New purchase request", version: "0.1.0",
  scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  input: Schema.Struct({
    request_id: Schema.String, amount: Norn.decimal(2), currency: Currency, supplier_id: Schema.String, requester: Schema.String,
  }),
  output: RequestRef,
  // The requester is whoever calls: the server sets it, a caller cannot.
  fromCaller: ["requester"],
  slice: [{ name: "supplier", type: Supplier, key: { property: { from: "input", property: "supplier_id" } } }],
  createLinks: [{ link: "supplier", targetType: Supplier, targetIdField: "supplier_id" }],
  writes: [{ type: PurchaseRequest, change: "created" }],
  effect: { kind: "upsert_input_object", type: PurchaseRequest, objectIdField: "request_id", writeMode: { kind: "create_only" } },
  access: { artifactId: "purchasing.requests.policy", version: "0.1.0" },
})

export const Submit = Norn.action({
  id: "purchasing.request.submit", title: "Submit for approval", version: "0.1.0",
  scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  input: RequestRef, output: RequestRef,
  writes: [{ type: PurchaseRequest, change: "modified" }],
  effect: { kind: "fire_transition", type: PurchaseRequest, objectIdField: "request_id", lifecycle: "status", transition: "submit" },
  access: { artifactId: "purchasing.requests.policy", version: "0.1.0" },
})

export const SubmitSmall = Norn.action({
  id: "purchasing.request.submit_small", title: "Submit (up to 1000, approved at once)", version: "0.1.0",
  scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  input: RequestRef, output: RequestRef,
  writes: [{ type: PurchaseRequest, change: "modified" }],
  effect: { kind: "fire_transition", type: PurchaseRequest, objectIdField: "request_id", lifecycle: "status", transition: "submit_small" },
  access: { artifactId: "purchasing.requests.policy", version: "0.1.0" },
})

export const Approve = Norn.action({
  id: "purchasing.request.approve", title: "Approve", version: "0.1.0",
  scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  input: RequestRef, output: RequestRef,
  writes: [{ type: PurchaseRequest, change: "modified" }],
  effect: { kind: "fire_transition", type: PurchaseRequest, objectIdField: "request_id", lifecycle: "status", transition: "approve" },
  access: { artifactId: "purchasing.requests.policy", version: "0.1.0" },
})

export const Reject = Norn.action({
  id: "purchasing.request.reject", title: "Reject", version: "0.1.0",
  scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  input: RequestRef, output: RequestRef,
  writes: [{ type: PurchaseRequest, change: "modified" }],
  effect: { kind: "fire_transition", type: PurchaseRequest, objectIdField: "request_id", lifecycle: "status", transition: "reject" },
  access: { artifactId: "purchasing.requests.policy", version: "0.1.0" },
})

export const RequestsPolicy = Norn.access({
  id: "purchasing.requests", title: "Purchase request access", version: "0.1.0",
  rules: [
    {
      // Members raise requests; only the requester submits their own.
      ruleId: "requesters_raise_and_submit",
      actions: [CreateRequest, Submit, SubmitSmall],
      resourceType: PurchaseRequest,
      scope: { dimensions: ["org", "environment"] },
      allOf: [{ role: { anyOf: ["member", "admin", "owner"] } }, { resource: { name: "requester", op: "eq", value: { principalAttribute: "id" } } }],
    },
    {
      // Purchasing decides, and never on a request they raised.
      ruleId: "purchasing_decides_others_requests",
      actions: [Approve, Reject],
      resourceType: PurchaseRequest,
      scope: { dimensions: ["org", "environment"] },
      allOf: [{ role: { anyOf: ["admin", "owner"] } }, { resource: { name: "requester", op: "ne", value: { principalAttribute: "id" } } }],
    },
  ],
})

// A request over 1000 is signed off by a manager before it is submitted. The
// cell parks the call; a decider who is not the requester approves it.
export const LargeRequests = Norn.approval({
  id: "purchasing.large_requests",
  action: Submit,
  on: PurchaseRequest,
  when: { property: "amount", op: "gt", value: "1000" },
  decider: { roles: ["admin", "owner"] },
  question: "Sign off this purchase request over 1000?",
  risk: "medium",
})
