import { Schema } from "effect"
import { Norn } from "@norn/authoring"

// Lane A folds immutable economic facts into these three stored inputs.
// No public action writes fold inputs or the read-only economic projections.
export const SalesOrder = Norn.type({
  name: "alpha.SalesOrder", title: "Alpha sales order", version: "0.1.0",
  extends: "commerce.SalesOrder",
  properties: Schema.Struct({
    status: Schema.Literals(["draft", "submitted", "approved", "cancelled"]),
    invoice_count: Schema.Int,
    invoiced_total: Norn.decimal(2),
    allocated_total: Norn.decimal(2),
    invoiced: Schema.Boolean,
    completed: Schema.Boolean,
    outstanding: Norn.decimal(2),
  }),
  defaults: { status: "draft", invoice_count: 0, invoiced_total: 0, allocated_total: 0 },
  computed: {
    invoiced: Norn.unsafeZen<boolean>("self.invoice_count > 0"),
    completed: Norn.unsafeZen<boolean>("self.invoice_count > 0 and self.allocated_total >= self.invoiced_total"),
    outstanding: Norn.unsafeZen<number>("self.invoiced_total - self.allocated_total"),
  },
  lifecycles: [{
    property: "status", initial: ["draft"], terminal: ["cancelled"],
    transitions: [
      { id: "submit", from: ["draft"], to: "submitted" },
      { id: "approve", from: ["submitted"], to: "approved" },
      { id: "cancel", from: ["draft", "submitted", "approved"], to: "cancelled",
        guards: [Norn.unsafeZen<boolean>("self.invoice_count == 0")] },
    ],
  }],
})

export const SalesInvoice = Norn.type({
  name: "alpha.SalesInvoice", title: "Invoice fact", version: "0.1.0",
  properties: Schema.Struct({ id: Schema.String, order_id: Schema.String, amount: Norn.decimal(2), reference: Schema.String }),
  identity: [{ name: "primary", properties: ["id"] }],
  links: [{ id: "order", target: SalesOrder, cardinality: { to: "one", from: "many", required: true } }],
})

export const PaymentAllocation = Norn.type({
  name: "alpha.PaymentAllocation", title: "Payment allocation fact", version: "0.1.0",
  properties: Schema.Struct({ id: Schema.String, order_id: Schema.String, amount: Norn.decimal(2) }),
  identity: [{ name: "primary", properties: ["id"] }],
  links: [{ id: "order", target: SalesOrder, cardinality: { to: "one", from: "many", required: true } }],
})

const InvoiceInput = Schema.Struct({ id: Schema.String, order_id: Schema.String, amount: Norn.decimal(2), reference: Schema.String })
export const RecordInvoice = Norn.action({
  id: "alpha.order.record_invoice", title: "Record invoice", version: "0.1.0",
  input: InvoiceInput, output: Schema.Struct({ id: Schema.String }),
  scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  slice: [{ name: "order", type: SalesOrder, key: { property: { from: "input", property: "order_id" } } }],
  preconditions: [Norn.unsafeZen<boolean>('slice.order.status == "approved" and input.amount > 0')],
  createLinks: [{ link: "order", targetType: SalesOrder, targetIdField: "order_id" }],
  writes: [{ type: SalesInvoice, change: "created" }],
  effect: { kind: "upsert_input_object", type: SalesInvoice, objectIdField: "id", writeMode: { kind: "create_only" } },
})

const PaymentInput = Schema.Struct({ id: Schema.String, order_id: Schema.String, amount: Norn.decimal(2) })
export const RecordPayment = Norn.action({
  id: "alpha.order.record_payment", title: "Record payment allocation", version: "0.1.0",
  input: PaymentInput, output: Schema.Struct({ id: Schema.String }),
  scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  slice: [{ name: "order", type: SalesOrder, key: { property: { from: "input", property: "order_id" } } }],
  preconditions: [Norn.unsafeZen<boolean>('slice.order.status == "approved" and input.amount > 0')],
  createLinks: [{ link: "order", targetType: SalesOrder, targetIdField: "order_id" }],
  writes: [{ type: PaymentAllocation, change: "created" }],
  effect: { kind: "upsert_input_object", type: PaymentAllocation, objectIdField: "id", writeMode: { kind: "create_only" } },
})

const OrderInput = Schema.Struct({ id: Schema.String })
export const Submit = Norn.action({
  id: "alpha.order.submit", title: "Submit order", version: "0.1.0",
  input: OrderInput, output: OrderInput,
  scopeRequired: ["orgId", "environmentId"], idempotent: false, ledgered: true,
  writes: [{ type: SalesOrder, change: "modified" }],
  effect: { kind: "fire_transition", type: SalesOrder, objectIdField: "id", lifecycle: "status", transition: "submit" },
})
export const Approve = Norn.action({
  id: "alpha.order.approve", title: "Approve order", version: "0.1.0",
  input: OrderInput, output: OrderInput,
  scopeRequired: ["orgId", "environmentId"], idempotent: false, ledgered: true,
  writes: [{ type: SalesOrder, change: "modified" }],
  effect: { kind: "fire_transition", type: SalesOrder, objectIdField: "id", lifecycle: "status", transition: "approve" },
})
export const Cancel = Norn.action({
  id: "alpha.order.cancel", title: "Cancel order", version: "0.1.0",
  input: OrderInput, output: OrderInput,
  scopeRequired: ["orgId", "environmentId"], idempotent: false, ledgered: true,
  writes: [{ type: SalesOrder, change: "modified" }],
  effect: { kind: "fire_transition", type: SalesOrder, objectIdField: "id", lifecycle: "status", transition: "cancel" },
})
export const Access = Norn.access({
  id: "alpha.order.access", title: "Alpha duty separation", version: "0.1.0",
  rules: [
    { ruleId: "sales_submit", actions: [Submit], resourceType: SalesOrder, scope: { dimensions: ["org", "environment"] }, allOf: [{ role: { anyOf: ["sales"] } }] },
    { ruleId: "approver_decides", actions: [Approve, Cancel], resourceType: SalesOrder, scope: { dimensions: ["org", "environment"] }, allOf: [{ role: { anyOf: ["approver"] } }] },
    { ruleId: "billing_invoices", actions: [RecordInvoice], resourceType: SalesInvoice, scope: { dimensions: ["org", "environment"] }, allOf: [{ role: { anyOf: ["billing"] } }] },
    { ruleId: "treasury_allocates", actions: [RecordPayment], resourceType: PaymentAllocation, scope: { dimensions: ["org", "environment"] }, allOf: [{ role: { anyOf: ["treasury"] } }] },
  ],
})
