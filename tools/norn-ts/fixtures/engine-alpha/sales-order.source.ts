import { Schema } from "effect"
import { Norn } from "@norn/authoring"

export const Customer = Norn.type({
  name: "alpha.Customer", title: "Alpha customer", version: "0.1.0",
  properties: Schema.Struct({ id: Schema.String, name: Schema.String }),
  identity: [{ name: "primary", properties: ["id"] }],
})

// Lane A folds immutable economic facts into these three stored inputs.
// No public action writes fold inputs or the read-only economic projections.
export const SalesOrder = Norn.type({
  name: "alpha.SalesOrder", title: "Alpha sales order", version: "0.1.0",
  properties: Schema.Struct({
    id: Schema.String,
    order_number: Schema.String, order_date: Norn.date(),
    currency: Schema.Literals(["EGP", "USD", "EUR"]), total_amount: Norn.decimal(2),
    status: Schema.Literals(["draft", "submitted", "approved", "cancelled"]),
    invoice_count: Schema.Int,
    invoiced_total: Norn.decimal(2),
    allocated_total: Norn.decimal(2),
    invoiced: Schema.Boolean,
    completed: Schema.Boolean,
    outstanding: Norn.decimal(2),
  }),
  identity: [{ name: "primary", properties: ["id"] }],
  links: [{ id: "counterparty", target: Customer, cardinality: { to: "one", from: "many", required: true } }],
  controls: { format: "norn.controls/1", sensitivePaths: [{
    objectType: "alpha.SalesOrder",
    actions: ["alpha.order.approve", "alpha.order.record_invoice", "alpha.order.record_payment"],
  }] },
  protected: ["invoice_count", "invoiced_total", "allocated_total"],
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
        guards: [Norn.unsafeZen<boolean>("self.invoice_count == 0 and self.allocated_total == 0")] },
    ],
  }],
})

export const SalesInvoice = Norn.type({
  name: "alpha.SalesInvoice", title: "Invoice fact", version: "0.1.0", immutable: true,
  properties: Schema.Struct({ id: Schema.String, order_id: Schema.String, amount: Norn.decimal(2), reference: Schema.String }),
  identity: [{ name: "primary", properties: ["id"] }],
  links: [{ id: "order", target: SalesOrder, cardinality: { to: "one", from: "many", required: true } }],
})

export const PaymentAllocation = Norn.type({
  name: "alpha.PaymentAllocation", title: "Payment allocation fact", version: "0.1.0", immutable: true,
  properties: Schema.Struct({ id: Schema.String, order_id: Schema.String, amount: Norn.decimal(2) }),
  identity: [{ name: "primary", properties: ["id"] }],
  links: [{ id: "order", target: SalesOrder, cardinality: { to: "one", from: "many", required: true } }],
})

export const Create = Norn.action({
  id: "alpha.order.create", title: "Create order", version: "0.1.0",
  input: Schema.Struct({
    id: Schema.String, order_number: Schema.String, order_date: Schema.String,
    currency: Schema.String, counterparty: Schema.String, total_amount: Norn.decimal(2),
  }),
  output: Schema.Struct({ id: Schema.String }),
  scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  slice: [{ name: "counterparty", type: Customer, key: { property: { from: "input", property: "counterparty" } } }],
  createLinks: [{ link: "counterparty", targetType: Customer, targetIdField: "counterparty" }],
  writes: [{ type: SalesOrder, change: "created" }],
  effect: { kind: "upsert_input_object", type: SalesOrder, objectIdField: "id", writeMode: { kind: "create_only" } },
})

export const CreateCustomer = Norn.action({
  id: "alpha.customer.create", title: "Create customer", version: "0.1.0",
  input: Schema.Struct({ id: Schema.String, name: Schema.String }), output: Schema.Struct({ id: Schema.String }),
  scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  writes: [{ type: Customer, change: "created" }],
  effect: { kind: "upsert_input_object", type: Customer, objectIdField: "id", writeMode: { kind: "create_only" } },
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
    { ruleId: "sales_create_customer", actions: [CreateCustomer], resourceType: Customer, scope: { dimensions: ["org", "environment"] }, allOf: [{ role: { anyOf: ["sales"] } }] },
    { ruleId: "sales_create", actions: [Create], resourceType: SalesOrder, scope: { dimensions: ["org", "environment"] }, allOf: [{ role: { anyOf: ["sales"] } }] },
    { ruleId: "order_reads", actions: ["norn.read"], resourceType: SalesOrder, scope: { dimensions: ["org", "environment"] }, allOf: [{ role: { anyOf: ["sales", "approver", "billing", "treasury"] } }] },
    { ruleId: "customer_reads", actions: ["norn.read"], resourceType: Customer, scope: { dimensions: ["org", "environment"] }, allOf: [{ role: { anyOf: ["sales"] } }] },
    { ruleId: "sales_submit", actions: [Submit], resourceType: SalesOrder, scope: { dimensions: ["org", "environment"] }, allOf: [{ role: { anyOf: ["sales"] } }] },
    { ruleId: "approver_decides", actions: [Approve, Cancel], resourceType: SalesOrder, scope: { dimensions: ["org", "environment"] }, allOf: [{ role: { anyOf: ["approver"] } }] },
    { ruleId: "billing_invoices", actions: [RecordInvoice], resourceType: SalesInvoice, scope: { dimensions: ["org", "environment"] }, allOf: [{ role: { anyOf: ["billing"] } }] },
    { ruleId: "treasury_allocates", actions: [RecordPayment], resourceType: PaymentAllocation, scope: { dimensions: ["org", "environment"] }, allOf: [{ role: { anyOf: ["treasury"] } }] },
  ],
})

const FoldOrder = Schema.Struct({ id: Schema.String, invoice_count: Schema.Int, invoiced_total: Schema.Finite, allocated_total: Schema.Finite })
const InvoiceFact = Schema.Struct({ id: Schema.String, order_id: Schema.String, amount: Schema.Finite, reference: Schema.String })
const PaymentFact = Schema.Struct({ id: Schema.String, order_id: Schema.String, amount: Schema.Finite })
const EventKey = Schema.Struct({ objectType: Schema.String, key: Schema.String })
const FoldOutput = Schema.Struct({
  facts: Schema.Array(Schema.Struct({ op: Schema.Literal("set"), objectType: Schema.Literal("alpha.SalesOrder"), key: Schema.String, property: Schema.String, value: Schema.Finite })),
  commands: Schema.Array(Schema.Struct({ kind: Schema.String })),
  timers: Schema.Array(Schema.Struct({ timerId: Schema.String, afterMillis: Schema.Int, kind: Schema.String })),
})
export const InvoiceFold = Norn.decision({
  id: "alpha.order.invoice_fold", title: "Fold created invoice", version: "0.1.0", contract: "decide",
  triggers: [{ on: "created", type: SalesInvoice }],
  slice: [
    { name: "invoice", type: SalesInvoice, key: "triggerObject" },
    { name: "order", type: SalesOrder, key: { property: { from: "invoice", property: "order_id" } } },
  ],
  input: Schema.Struct({ slice: Schema.Struct({ invoice: InvoiceFact, order: FoldOrder }), event: Schema.Struct({ op: Schema.String, key: EventKey, values: InvoiceFact }) }),
  output: FoldOutput, scopeRequired: ["orgId", "environmentId"],
}, input => ({
  facts: [
    { op: "set", objectType: "alpha.SalesOrder", key: input.slice.order.id, property: "invoice_count", value: input.slice.order.invoice_count + 1 },
    { op: "set", objectType: "alpha.SalesOrder", key: input.slice.order.id, property: "invoiced_total", value: input.slice.order.invoiced_total + input.slice.invoice.amount },
  ], commands: [], timers: [],
}))
export const PaymentFold = Norn.decision({
  id: "alpha.order.payment_fold", title: "Fold created payment allocation", version: "0.1.0", contract: "decide",
  triggers: [{ on: "created", type: PaymentAllocation }],
  slice: [
    { name: "payment", type: PaymentAllocation, key: "triggerObject" },
    { name: "order", type: SalesOrder, key: { property: { from: "payment", property: "order_id" } } },
  ],
  input: Schema.Struct({ slice: Schema.Struct({ payment: PaymentFact, order: FoldOrder }), event: Schema.Struct({ op: Schema.String, key: EventKey, values: PaymentFact }) }),
  output: FoldOutput, scopeRequired: ["orgId", "environmentId"],
}, input => ({
  facts: [{ op: "set", objectType: "alpha.SalesOrder", key: input.slice.order.id, property: "allocated_total", value: input.slice.order.allocated_total + input.slice.payment.amount }],
  commands: [], timers: [],
}))
