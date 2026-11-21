// ERP-DAY-1: the model the ERP-day bench (`tests/erp_day.rs`) replays as
// commands. Every business document is a header plus line objects that link
// to it, so each line is its own governed commit and each header law sums the
// lines that link to it once the header leaves `draft` (LAW-LINK-MANY-1).
//
// Laws a real ERP enforces, each checked on every commit it touches:
// - a line's amount is its quantity times its rate;
// - a submitted document's total is the sum of its lines, within the
//   customer's credit limit for a sales order;
// - stock on hand never goes negative, and a move states the balance it
//   leaves, checked against the bin it read (re-read at commit);
// - an invoice's outstanding amount stays within 0..total, and a payment's
//   unallocated amount within 0..amount;
// - a posted journal entry balances: debits equal credits over at least two
//   one-sided lines;
// - a purchase order over 5000 waits for a second person's sign-off.
// Every action is decided by the access policy at the end of the module.
// Generated in full (no helpers): the compiler reads each definition literally.
import { Norn } from "@norn/authoring"
import { Schema } from "effect"

const DocStatus = Schema.Literals(["draft", "submitted", "done", "cancelled"])
const Status = Schema.Struct({ status: DocStatus })
const DocRef = Schema.Struct({ doc_id: Schema.String })
const LineShape = Schema.Struct({ line_id: Schema.String, doc_id: Schema.String, item_code: Schema.String, qty: Norn.decimal(0), rate: Norn.decimal(2), amount: Norn.decimal(2) })
const BinShape = Schema.Struct({ bin_id: Schema.String, item_code: Schema.String, warehouse: Schema.String, on_hand: Norn.decimal(0), last_move: Norn.decimal(0) })
const MoveInput = Schema.Struct({ bin_id: Schema.String, on_hand: Norn.decimal(0), last_move: Norn.decimal(0) })
const OrderShape = Schema.Struct({ doc_id: Schema.String, customer_id: Schema.String, total: Norn.decimal(2), status: DocStatus })
const InvoiceShape = Schema.Struct({ doc_id: Schema.String, customer_id: Schema.String, order_id: Schema.String, total: Norn.decimal(2), outstanding: Norn.decimal(2), last_allocated: Norn.decimal(2), status: DocStatus })
const PaymentShape = Schema.Struct({ doc_id: Schema.String, customer_id: Schema.String, amount: Norn.decimal(2), unallocated: Norn.decimal(2) })
const SpendInput = Schema.Struct({ doc_id: Schema.String, unallocated: Norn.decimal(2) })
const AllocateInput = Schema.Struct({ doc_id: Schema.String, outstanding: Norn.decimal(2), last_allocated: Norn.decimal(2) })
const PostingShape = Schema.Struct({ line_id: Schema.String, doc_id: Schema.String, account: Schema.String, debit: Norn.decimal(2), credit: Norn.decimal(2) })

// --- Master data

export const Customer = Norn.type({
  name: "erp.Customer", title: "Customer", version: "0.1.0",
  properties: Schema.Struct({ customer_id: Schema.String, customer_name: Schema.String, credit_limit: Norn.decimal(2) }),
  identity: [{ name: "primary", properties: ["customer_id"] }],
})

export const CustomerCredit = Norn.law({
  id: "erp.customer_credit.law", title: "A customer's credit limit is not negative", version: "0.1.0",
  dependencies: [{ artifactId: "erp.Customer.entity", kind: "entity", version: "0.1.0", required: true }],
  law: { lawVersion: "0.1", id: "erp.customer_credit", owner: "erp", severity: "error", rationale: "A customer's credit limit is not negative",
    provenance: { source: "authoring_sdk", sourcePath: "crates/norn-cell-do/tests/fixtures/erp-day/src/erp.ts" },
    scope: { root: { kind: "entity", id: "erp.Customer" }, objectTypes: ["erp.Customer"] },
    subject: "erp.Customer", modality: { kind: "always" }, response: { kind: "predicate", expression: "self.credit_limit >= 0" } },
})

export const CreateCustomer = Norn.action({
  id: "erp.customer.create", title: "New customer", version: "0.1.0", scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true, access: { artifactId: "erp.access.policy", version: "0.1.0" },
  input: Schema.Struct({ customer_id: Schema.String, customer_name: Schema.String, credit_limit: Norn.decimal(2) }), output: Schema.Struct({ customer_id: Schema.String }),
  writes: [{ type: Customer, change: "created" }],

  effect: { kind: "upsert_input_object", type: Customer, objectIdField: "customer_id", writeMode: { kind: "create_only" } },
})

export const Item = Norn.type({
  name: "erp.Item", title: "Item", version: "0.1.0",
  properties: Schema.Struct({ item_code: Schema.String, item_name: Schema.String, standard_rate: Norn.decimal(2) }),
  identity: [{ name: "primary", properties: ["item_code"] }],
})

export const ItemRate = Norn.law({
  id: "erp.item_rate.law", title: "An item's standard rate is positive", version: "0.1.0",
  dependencies: [{ artifactId: "erp.Item.entity", kind: "entity", version: "0.1.0", required: true }],
  law: { lawVersion: "0.1", id: "erp.item_rate", owner: "erp", severity: "error", rationale: "An item's standard rate is positive",
    provenance: { source: "authoring_sdk", sourcePath: "crates/norn-cell-do/tests/fixtures/erp-day/src/erp.ts" },
    scope: { root: { kind: "entity", id: "erp.Item" }, objectTypes: ["erp.Item"] },
    subject: "erp.Item", modality: { kind: "always" }, response: { kind: "predicate", expression: "self.standard_rate > 0" } },
})

export const CreateItem = Norn.action({
  id: "erp.item.create", title: "New item", version: "0.1.0", scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true, access: { artifactId: "erp.access.policy", version: "0.1.0" },
  input: Schema.Struct({ item_code: Schema.String, item_name: Schema.String, standard_rate: Norn.decimal(2) }), output: Schema.Struct({ item_code: Schema.String }),
  writes: [{ type: Item, change: "created" }],

  effect: { kind: "upsert_input_object", type: Item, objectIdField: "item_code", writeMode: { kind: "create_only" } },
})

// --- Stock

export const Bin = Norn.type({
  name: "erp.Bin", title: "Stock bin", version: "0.1.0",
  properties: BinShape,
  identity: [{ name: "primary", properties: ["bin_id"] }],
  links: [{ id: "item", target: Item, cardinality: { to: "one", from: "many", required: true } }],
})

export const StockNeverNegative = Norn.law({
  id: "erp.stock_never_negative.law", title: "Stock on hand never goes negative", version: "0.1.0",
  dependencies: [{ artifactId: "erp.Bin.entity", kind: "entity", version: "0.1.0", required: true }],
  law: { lawVersion: "0.1", id: "erp.stock_never_negative", owner: "erp", severity: "error", rationale: "Stock on hand never goes negative",
    provenance: { source: "authoring_sdk", sourcePath: "crates/norn-cell-do/tests/fixtures/erp-day/src/erp.ts" },
    scope: { root: { kind: "entity", id: "erp.Bin" }, objectTypes: ["erp.Bin"] },
    subject: "erp.Bin", modality: { kind: "always" }, response: { kind: "predicate", expression: "self.on_hand >= 0" } },
})

export const OpenBin = Norn.action({
  id: "erp.bin.open", title: "Open a stock bin", version: "0.1.0", scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true, access: { artifactId: "erp.access.policy", version: "0.1.0" },
  input: BinShape, output: Schema.Struct({ bin_id: Schema.String }),
  slice: [{ name: "item", type: Item, key: { property: { from: "input", property: "item_code" } } }],
  createLinks: [{ link: "item", targetType: Item, targetIdField: "item_code" }],
  writes: [{ type: Bin, change: "created" }],

  effect: { kind: "upsert_input_object", type: Bin, objectIdField: "bin_id", writeMode: { kind: "create_only" } },
})

export const MoveStock = Norn.action({
  id: "erp.stock.move", title: "Move stock", version: "0.1.0", scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true, access: { artifactId: "erp.access.policy", version: "0.1.0" },
  input: MoveInput, output: Schema.Struct({ bin_id: Schema.String }),
  slice: [{ name: "bin", type: Bin, key: { property: { from: "input", property: "bin_id" } } }],
  preconditions: [Norn.expression(Schema.Struct({ input: MoveInput, slice: Schema.Struct({ bin: BinShape }) }), (x) => x.input.last_move !== 0 && x.input.on_hand === x.slice.bin.on_hand + x.input.last_move)],
  writes: [{ type: Bin, change: "modified" }, { type: Bin, change: "created" }],

  effect: { kind: "upsert_input_object", type: Bin, objectIdField: "bin_id", writeMode: { kind: "upsert" } },
})

// --- Sales order

export const SalesOrder = Norn.type({
  name: "erp.SalesOrder", title: "Sales order", version: "0.1.0", role: "happening",
  properties: OrderShape,
  identity: [{ name: "primary", properties: ["doc_id"] }],
  links: [{ id: "customer", target: Customer, cardinality: { to: "one", from: "many", required: true } }],
  defaults: { status: "draft" },
  lifecycles: [{ property: "status", initial: ["draft"], terminal: ["done", "cancelled"], immutable: ["done", "cancelled"], transitions: [{ id: "submit", from: ["draft"], to: "submitted" }, { id: "complete", from: ["submitted"], to: "done" }, { id: "cancel", from: ["draft", "submitted"], to: "cancelled" }] }],
})

export const OrderTotal = Norn.law({
  id: "erp.sales_order_total.law", title: "A submitted sales order's total is the sum of its lines, within the customer's credit limit", version: "0.1.0",
  dependencies: [{ artifactId: "erp.SalesOrder.entity", kind: "entity", version: "0.1.0", required: true }],
  law: { lawVersion: "0.1", id: "erp.sales_order_total", owner: "erp", severity: "error", rationale: "A submitted sales order's total is the sum of its lines, within the customer's credit limit",
    provenance: { source: "authoring_sdk", sourcePath: "crates/norn-cell-do/tests/fixtures/erp-day/src/erp.ts" },
    scope: { root: { kind: "entity", id: "erp.SalesOrder" }, objectTypes: ["erp.SalesOrder"] },
    subject: "erp.SalesOrder", modality: { kind: "always" }, condition: "self.status != 'draft'", response: { kind: "predicate", expression: "len(self.lines) > 0 and self.total == sum(map(self.lines, #.amount)) and self.total <= self.customer.credit_limit" } },
})

export const CreateOrder = Norn.action({
  id: "erp.sales_order.create", title: "New sales order", version: "0.1.0", scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true, access: { artifactId: "erp.access.policy", version: "0.1.0" },
  input: Schema.Struct({ doc_id: Schema.String, customer_id: Schema.String, total: Norn.decimal(2) }), output: DocRef,
  slice: [{ name: "customer", type: Customer, key: { property: { from: "input", property: "customer_id" } } }],
  createLinks: [{ link: "customer", targetType: Customer, targetIdField: "customer_id" }],
  writes: [{ type: SalesOrder, change: "created" }],

  effect: { kind: "upsert_input_object", type: SalesOrder, objectIdField: "doc_id", writeMode: { kind: "create_only" } },
})

export const SalesOrderLine = Norn.type({
  name: "erp.SalesOrderLine", title: "Sales order line", version: "0.1.0",
  properties: LineShape,
  identity: [{ name: "primary", properties: ["line_id"] }],
  links: [{ id: "doc", target: SalesOrder, cardinality: { to: "one", from: "many", required: true }, inverse: "lines" }, { id: "item", target: Item, cardinality: { to: "one", from: "many", required: true } }],
})

export const SalesOrderLineAmount = Norn.law({
  id: "erp.salesorderline_amount.law", title: "A line's amount is its quantity times its rate", version: "0.1.0",
  dependencies: [{ artifactId: "erp.SalesOrderLine.entity", kind: "entity", version: "0.1.0", required: true }],
  law: { lawVersion: "0.1", id: "erp.salesorderline_amount", owner: "erp", severity: "error", rationale: "A line's amount is its quantity times its rate",
    provenance: { source: "authoring_sdk", sourcePath: "crates/norn-cell-do/tests/fixtures/erp-day/src/erp.ts" },
    scope: { root: { kind: "entity", id: "erp.SalesOrderLine" }, objectTypes: ["erp.SalesOrderLine"] },
    subject: "erp.SalesOrderLine", modality: { kind: "always" }, response: { kind: "predicate", expression: "self.qty > 0 and self.rate > 0 and self.amount == self.qty * self.rate" } },
})

export const AddOrderLine = Norn.action({
  id: "erp.sales_order.add_line", title: "Add a line to a sales order", version: "0.1.0", scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true, access: { artifactId: "erp.access.policy", version: "0.1.0" },
  input: LineShape, output: Schema.Struct({ line_id: Schema.String }),
  slice: [{ name: "doc", type: SalesOrder, key: { property: { from: "input", property: "doc_id" } } }, { name: "item", type: Item, key: { property: { from: "input", property: "item_code" } } }],
  preconditions: [Norn.expression(Schema.Struct({ slice: Schema.Struct({ doc: Status }) }), (x) => x.slice.doc.status === "draft")],
  createLinks: [{ link: "doc", targetType: SalesOrder, targetIdField: "doc_id" }, { link: "item", targetType: Item, targetIdField: "item_code" }],
  writes: [{ type: SalesOrderLine, change: "created" }],

  effect: { kind: "upsert_input_object", type: SalesOrderLine, objectIdField: "line_id", writeMode: { kind: "create_only" } },
})

export const SubmitOrder = Norn.action({
  id: "erp.sales_order.submit", title: "Submit a sales order", version: "0.1.0", scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true, access: { artifactId: "erp.access.policy", version: "0.1.0" },
  input: DocRef, output: DocRef,
  writes: [{ type: SalesOrder, change: "modified" }],
  effect: { kind: "fire_transition", type: SalesOrder, objectIdField: "doc_id", lifecycle: "status", transition: "submit" },
})

export const CompleteOrder = Norn.action({
  id: "erp.sales_order.complete", title: "Mark a sales order delivered", version: "0.1.0", scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true, access: { artifactId: "erp.access.policy", version: "0.1.0" },
  input: DocRef, output: DocRef,
  writes: [{ type: SalesOrder, change: "modified" }],
  effect: { kind: "fire_transition", type: SalesOrder, objectIdField: "doc_id", lifecycle: "status", transition: "complete" },
})

// --- Delivery note: its lines are stock moves

export const Delivery = Norn.type({
  name: "erp.Delivery", title: "Delivery note", version: "0.1.0", role: "happening",
  properties: Schema.Struct({ doc_id: Schema.String, order_id: Schema.String, status: DocStatus }),
  identity: [{ name: "primary", properties: ["doc_id"] }],
  links: [{ id: "order", target: SalesOrder, cardinality: { to: "one", from: "many", required: true } }],
  defaults: { status: "draft" },
  lifecycles: [{ property: "status", initial: ["draft"], terminal: ["done", "cancelled"], immutable: ["done", "cancelled"], transitions: [{ id: "submit", from: ["draft"], to: "submitted" }, { id: "complete", from: ["submitted"], to: "done" }, { id: "cancel", from: ["draft", "submitted"], to: "cancelled" }] }],
})

export const CreateDelivery = Norn.action({
  id: "erp.delivery.create", title: "New delivery note", version: "0.1.0", scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true, access: { artifactId: "erp.access.policy", version: "0.1.0" },
  input: Schema.Struct({ doc_id: Schema.String, order_id: Schema.String }), output: DocRef,
  slice: [{ name: "order", type: SalesOrder, key: { property: { from: "input", property: "order_id" } } }],
  preconditions: [Norn.expression(Schema.Struct({ slice: Schema.Struct({ order: Status }) }), (x) => x.slice.order.status === "submitted")],
  createLinks: [{ link: "order", targetType: SalesOrder, targetIdField: "order_id" }],
  writes: [{ type: Delivery, change: "created" }],

  effect: { kind: "upsert_input_object", type: Delivery, objectIdField: "doc_id", writeMode: { kind: "create_only" } },
})

export const SubmitDelivery = Norn.action({
  id: "erp.delivery.submit", title: "Submit a delivery note", version: "0.1.0", scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true, access: { artifactId: "erp.access.policy", version: "0.1.0" },
  input: DocRef, output: DocRef,
  writes: [{ type: Delivery, change: "modified" }],
  effect: { kind: "fire_transition", type: Delivery, objectIdField: "doc_id", lifecycle: "status", transition: "submit" },
})

// --- Sales invoice

export const SalesInvoice = Norn.type({
  name: "erp.SalesInvoice", title: "Sales invoice", version: "0.1.0", role: "happening",
  properties: InvoiceShape,
  identity: [{ name: "primary", properties: ["doc_id"] }],
  links: [{ id: "customer", target: Customer, cardinality: { to: "one", from: "many", required: true } }, { id: "order", target: SalesOrder, cardinality: { to: "one", from: "many", required: true } }],
  defaults: { status: "draft", last_allocated: 0 },
  lifecycles: [{ property: "status", initial: ["draft"], terminal: ["done", "cancelled"], immutable: ["done", "cancelled"], transitions: [{ id: "submit", from: ["draft"], to: "submitted" }, { id: "complete", from: ["submitted"], to: "done" }, { id: "cancel", from: ["draft", "submitted"], to: "cancelled" }] }],
})

export const InvoiceTotal = Norn.law({
  id: "erp.sales_invoice_total.law", title: "A submitted invoice's total is the sum of its lines", version: "0.1.0",
  dependencies: [{ artifactId: "erp.SalesInvoice.entity", kind: "entity", version: "0.1.0", required: true }],
  law: { lawVersion: "0.1", id: "erp.sales_invoice_total", owner: "erp", severity: "error", rationale: "A submitted invoice's total is the sum of its lines",
    provenance: { source: "authoring_sdk", sourcePath: "crates/norn-cell-do/tests/fixtures/erp-day/src/erp.ts" },
    scope: { root: { kind: "entity", id: "erp.SalesInvoice" }, objectTypes: ["erp.SalesInvoice"] },
    subject: "erp.SalesInvoice", modality: { kind: "always" }, condition: "self.status != 'draft'", response: { kind: "predicate", expression: "len(self.lines) > 0 and self.total == sum(map(self.lines, #.amount))" } },
})

export const InvoiceOutstanding = Norn.law({
  id: "erp.sales_invoice_outstanding.law", title: "An invoice's outstanding amount stays between zero and its total", version: "0.1.0",
  dependencies: [{ artifactId: "erp.SalesInvoice.entity", kind: "entity", version: "0.1.0", required: true }],
  law: { lawVersion: "0.1", id: "erp.sales_invoice_outstanding", owner: "erp", severity: "error", rationale: "An invoice's outstanding amount stays between zero and its total",
    provenance: { source: "authoring_sdk", sourcePath: "crates/norn-cell-do/tests/fixtures/erp-day/src/erp.ts" },
    scope: { root: { kind: "entity", id: "erp.SalesInvoice" }, objectTypes: ["erp.SalesInvoice"] },
    subject: "erp.SalesInvoice", modality: { kind: "always" }, response: { kind: "predicate", expression: "self.outstanding >= 0 and self.outstanding <= self.total" } },
})

export const CreateInvoice = Norn.action({
  id: "erp.sales_invoice.create", title: "New sales invoice", version: "0.1.0", scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true, access: { artifactId: "erp.access.policy", version: "0.1.0" },
  input: Schema.Struct({ doc_id: Schema.String, customer_id: Schema.String, order_id: Schema.String, total: Norn.decimal(2), outstanding: Norn.decimal(2) }), output: DocRef,
  slice: [{ name: "order", type: SalesOrder, key: { property: { from: "input", property: "order_id" } } }, { name: "customer", type: Customer, key: { property: { from: "input", property: "customer_id" } } }],
  preconditions: [Norn.expression(Schema.Struct({ slice: Schema.Struct({ order: OrderShape }), input: Schema.Struct({ customer_id: Schema.String, total: Norn.decimal(2) }) }), (x) => x.slice.order.status !== "draft" && x.slice.order.customer_id === x.input.customer_id && x.slice.order.total === x.input.total)],
  createLinks: [{ link: "customer", targetType: Customer, targetIdField: "customer_id" }, { link: "order", targetType: SalesOrder, targetIdField: "order_id" }],
  writes: [{ type: SalesInvoice, change: "created" }],

  effect: { kind: "upsert_input_object", type: SalesInvoice, objectIdField: "doc_id", writeMode: { kind: "create_only" } },
})

export const InvoiceLine = Norn.type({
  name: "erp.InvoiceLine", title: "Sales invoice line", version: "0.1.0",
  properties: LineShape,
  identity: [{ name: "primary", properties: ["line_id"] }],
  links: [{ id: "doc", target: SalesInvoice, cardinality: { to: "one", from: "many", required: true }, inverse: "lines" }, { id: "item", target: Item, cardinality: { to: "one", from: "many", required: true } }],
})

export const InvoiceLineAmount = Norn.law({
  id: "erp.invoiceline_amount.law", title: "A line's amount is its quantity times its rate", version: "0.1.0",
  dependencies: [{ artifactId: "erp.InvoiceLine.entity", kind: "entity", version: "0.1.0", required: true }],
  law: { lawVersion: "0.1", id: "erp.invoiceline_amount", owner: "erp", severity: "error", rationale: "A line's amount is its quantity times its rate",
    provenance: { source: "authoring_sdk", sourcePath: "crates/norn-cell-do/tests/fixtures/erp-day/src/erp.ts" },
    scope: { root: { kind: "entity", id: "erp.InvoiceLine" }, objectTypes: ["erp.InvoiceLine"] },
    subject: "erp.InvoiceLine", modality: { kind: "always" }, response: { kind: "predicate", expression: "self.qty > 0 and self.rate > 0 and self.amount == self.qty * self.rate" } },
})

export const AddInvoiceLine = Norn.action({
  id: "erp.sales_invoice.add_line", title: "Add a line to a sales invoice", version: "0.1.0", scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true, access: { artifactId: "erp.access.policy", version: "0.1.0" },
  input: LineShape, output: Schema.Struct({ line_id: Schema.String }),
  slice: [{ name: "doc", type: SalesInvoice, key: { property: { from: "input", property: "doc_id" } } }, { name: "item", type: Item, key: { property: { from: "input", property: "item_code" } } }],
  preconditions: [Norn.expression(Schema.Struct({ slice: Schema.Struct({ doc: Status }) }), (x) => x.slice.doc.status === "draft")],
  createLinks: [{ link: "doc", targetType: SalesInvoice, targetIdField: "doc_id" }, { link: "item", targetType: Item, targetIdField: "item_code" }],
  writes: [{ type: InvoiceLine, change: "created" }],

  effect: { kind: "upsert_input_object", type: InvoiceLine, objectIdField: "line_id", writeMode: { kind: "create_only" } },
})

export const SubmitInvoice = Norn.action({
  id: "erp.sales_invoice.submit", title: "Submit a sales invoice", version: "0.1.0", scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true, access: { artifactId: "erp.access.policy", version: "0.1.0" },
  input: DocRef, output: DocRef,
  writes: [{ type: SalesInvoice, change: "modified" }],
  effect: { kind: "fire_transition", type: SalesInvoice, objectIdField: "doc_id", lifecycle: "status", transition: "submit" },
})

export const AllocateInvoice = Norn.action({
  id: "erp.sales_invoice.allocate", title: "Allocate a payment to an invoice", version: "0.1.0", scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true, access: { artifactId: "erp.access.policy", version: "0.1.0" },
  input: AllocateInput, output: DocRef,
  slice: [{ name: "invoice", type: SalesInvoice, key: { property: { from: "input", property: "doc_id" } } }],
  preconditions: [Norn.expression(Schema.Struct({ input: AllocateInput, slice: Schema.Struct({ invoice: InvoiceShape }) }), (x) => x.slice.invoice.status === "submitted" && x.input.last_allocated > 0 && x.input.outstanding === x.slice.invoice.outstanding - x.input.last_allocated)],
  writes: [{ type: SalesInvoice, change: "modified" }, { type: SalesInvoice, change: "created" }],

  effect: { kind: "upsert_input_object", type: SalesInvoice, objectIdField: "doc_id", writeMode: { kind: "upsert" } },
})

export const CloseInvoice = Norn.action({
  id: "erp.sales_invoice.complete", title: "Mark a sales invoice paid", version: "0.1.0", scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true, access: { artifactId: "erp.access.policy", version: "0.1.0" },
  input: DocRef, output: DocRef,
  writes: [{ type: SalesInvoice, change: "modified" }],
  effect: { kind: "fire_transition", type: SalesInvoice, objectIdField: "doc_id", lifecycle: "status", transition: "complete" },
})

// --- Payment

export const Payment = Norn.type({
  name: "erp.Payment", title: "Payment received", version: "0.1.0", role: "happening",
  properties: PaymentShape,
  identity: [{ name: "primary", properties: ["doc_id"] }],
  links: [{ id: "customer", target: Customer, cardinality: { to: "one", from: "many", required: true } }],
})

export const PaymentUnallocated = Norn.law({
  id: "erp.payment_unallocated.law", title: "A payment's unallocated amount stays between zero and the amount received", version: "0.1.0",
  dependencies: [{ artifactId: "erp.Payment.entity", kind: "entity", version: "0.1.0", required: true }],
  law: { lawVersion: "0.1", id: "erp.payment_unallocated", owner: "erp", severity: "error", rationale: "A payment's unallocated amount stays between zero and the amount received",
    provenance: { source: "authoring_sdk", sourcePath: "crates/norn-cell-do/tests/fixtures/erp-day/src/erp.ts" },
    scope: { root: { kind: "entity", id: "erp.Payment" }, objectTypes: ["erp.Payment"] },
    subject: "erp.Payment", modality: { kind: "always" }, response: { kind: "predicate", expression: "self.amount > 0 and self.unallocated >= 0 and self.unallocated <= self.amount" } },
})

export const ReceivePayment = Norn.action({
  id: "erp.payment.receive", title: "Receive a payment", version: "0.1.0", scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true, access: { artifactId: "erp.access.policy", version: "0.1.0" },
  input: PaymentShape, output: DocRef,
  slice: [{ name: "customer", type: Customer, key: { property: { from: "input", property: "customer_id" } } }],
  createLinks: [{ link: "customer", targetType: Customer, targetIdField: "customer_id" }],
  writes: [{ type: Payment, change: "created" }],

  effect: { kind: "upsert_input_object", type: Payment, objectIdField: "doc_id", writeMode: { kind: "create_only" } },
})

export const SpendPayment = Norn.action({
  id: "erp.payment.allocate", title: "Allocate part of a payment", version: "0.1.0", scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true, access: { artifactId: "erp.access.policy", version: "0.1.0" },
  input: SpendInput, output: DocRef,
  slice: [{ name: "payment", type: Payment, key: { property: { from: "input", property: "doc_id" } } }],
  preconditions: [Norn.expression(Schema.Struct({ input: SpendInput, slice: Schema.Struct({ payment: PaymentShape }) }), (x) => x.input.unallocated < x.slice.payment.unallocated)],
  writes: [{ type: Payment, change: "modified" }, { type: Payment, change: "created" }],

  effect: { kind: "upsert_input_object", type: Payment, objectIdField: "doc_id", writeMode: { kind: "upsert" } },
})

// --- Purchase order: over 5000 a second person signs off the submit

export const PurchaseOrder = Norn.type({
  name: "erp.PurchaseOrder", title: "Purchase order", version: "0.1.0", role: "happening",
  properties: Schema.Struct({ doc_id: Schema.String, supplier: Schema.String, total: Norn.decimal(2), status: DocStatus }),
  identity: [{ name: "primary", properties: ["doc_id"] }],
  defaults: { status: "draft" },
  lifecycles: [{ property: "status", initial: ["draft"], terminal: ["done", "cancelled"], immutable: ["done", "cancelled"], transitions: [{ id: "submit", from: ["draft"], to: "submitted" }, { id: "complete", from: ["submitted"], to: "done" }, { id: "cancel", from: ["draft", "submitted"], to: "cancelled" }] }],
})

export const PurchaseTotal = Norn.law({
  id: "erp.purchase_order_total.law", title: "A submitted purchase order's total is the sum of its lines", version: "0.1.0",
  dependencies: [{ artifactId: "erp.PurchaseOrder.entity", kind: "entity", version: "0.1.0", required: true }],
  law: { lawVersion: "0.1", id: "erp.purchase_order_total", owner: "erp", severity: "error", rationale: "A submitted purchase order's total is the sum of its lines",
    provenance: { source: "authoring_sdk", sourcePath: "crates/norn-cell-do/tests/fixtures/erp-day/src/erp.ts" },
    scope: { root: { kind: "entity", id: "erp.PurchaseOrder" }, objectTypes: ["erp.PurchaseOrder"] },
    subject: "erp.PurchaseOrder", modality: { kind: "always" }, condition: "self.status != 'draft'", response: { kind: "predicate", expression: "len(self.lines) > 0 and self.total == sum(map(self.lines, #.amount))" } },
})

export const CreatePurchase = Norn.action({
  id: "erp.purchase_order.create", title: "New purchase order", version: "0.1.0", scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true, access: { artifactId: "erp.access.policy", version: "0.1.0" },
  input: Schema.Struct({ doc_id: Schema.String, supplier: Schema.String, total: Norn.decimal(2) }), output: DocRef,
  writes: [{ type: PurchaseOrder, change: "created" }],

  effect: { kind: "upsert_input_object", type: PurchaseOrder, objectIdField: "doc_id", writeMode: { kind: "create_only" } },
})

export const PurchaseOrderLine = Norn.type({
  name: "erp.PurchaseOrderLine", title: "Purchase order line", version: "0.1.0",
  properties: LineShape,
  identity: [{ name: "primary", properties: ["line_id"] }],
  links: [{ id: "doc", target: PurchaseOrder, cardinality: { to: "one", from: "many", required: true }, inverse: "lines" }, { id: "item", target: Item, cardinality: { to: "one", from: "many", required: true } }],
})

export const PurchaseOrderLineAmount = Norn.law({
  id: "erp.purchaseorderline_amount.law", title: "A line's amount is its quantity times its rate", version: "0.1.0",
  dependencies: [{ artifactId: "erp.PurchaseOrderLine.entity", kind: "entity", version: "0.1.0", required: true }],
  law: { lawVersion: "0.1", id: "erp.purchaseorderline_amount", owner: "erp", severity: "error", rationale: "A line's amount is its quantity times its rate",
    provenance: { source: "authoring_sdk", sourcePath: "crates/norn-cell-do/tests/fixtures/erp-day/src/erp.ts" },
    scope: { root: { kind: "entity", id: "erp.PurchaseOrderLine" }, objectTypes: ["erp.PurchaseOrderLine"] },
    subject: "erp.PurchaseOrderLine", modality: { kind: "always" }, response: { kind: "predicate", expression: "self.qty > 0 and self.rate > 0 and self.amount == self.qty * self.rate" } },
})

export const AddPurchaseLine = Norn.action({
  id: "erp.purchase_order.add_line", title: "Add a line to a purchase order", version: "0.1.0", scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true, access: { artifactId: "erp.access.policy", version: "0.1.0" },
  input: LineShape, output: Schema.Struct({ line_id: Schema.String }),
  slice: [{ name: "doc", type: PurchaseOrder, key: { property: { from: "input", property: "doc_id" } } }, { name: "item", type: Item, key: { property: { from: "input", property: "item_code" } } }],
  preconditions: [Norn.expression(Schema.Struct({ slice: Schema.Struct({ doc: Status }) }), (x) => x.slice.doc.status === "draft")],
  createLinks: [{ link: "doc", targetType: PurchaseOrder, targetIdField: "doc_id" }, { link: "item", targetType: Item, targetIdField: "item_code" }],
  writes: [{ type: PurchaseOrderLine, change: "created" }],

  effect: { kind: "upsert_input_object", type: PurchaseOrderLine, objectIdField: "line_id", writeMode: { kind: "create_only" } },
})

export const SubmitPurchase = Norn.action({
  id: "erp.purchase_order.submit", title: "Submit a purchase order", version: "0.1.0", scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true, access: { artifactId: "erp.access.policy", version: "0.1.0" },
  input: DocRef, output: DocRef,
  writes: [{ type: PurchaseOrder, change: "modified" }],
  effect: { kind: "fire_transition", type: PurchaseOrder, objectIdField: "doc_id", lifecycle: "status", transition: "submit" },
})

export const ReceivePurchase = Norn.action({
  id: "erp.purchase_order.complete", title: "Mark a purchase order received", version: "0.1.0", scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true, access: { artifactId: "erp.access.policy", version: "0.1.0" },
  input: DocRef, output: DocRef,
  writes: [{ type: PurchaseOrder, change: "modified" }],
  effect: { kind: "fire_transition", type: PurchaseOrder, objectIdField: "doc_id", lifecycle: "status", transition: "complete" },
})

export const LargePurchases = Norn.approval({
  id: "erp.large_purchases",
  action: SubmitPurchase,
  on: PurchaseOrder,
  when: { property: "total", op: "gt", value: "5000" },
  decider: { roles: ["admin", "owner"] },
  question: "Sign off this purchase order over 5000?",
  risk: "medium",
})

// --- General ledger: posting checks the balance

export const JournalEntry = Norn.type({
  name: "erp.JournalEntry", title: "Journal entry", version: "0.1.0", role: "happening",
  properties: Schema.Struct({ doc_id: Schema.String, memo: Schema.String, status: DocStatus }),
  identity: [{ name: "primary", properties: ["doc_id"] }],
  defaults: { status: "draft" },
  lifecycles: [{ property: "status", initial: ["draft"], terminal: ["done", "cancelled"], immutable: ["done", "cancelled"], transitions: [{ id: "submit", from: ["draft"], to: "submitted" }, { id: "complete", from: ["submitted"], to: "done" }, { id: "cancel", from: ["draft", "submitted"], to: "cancelled" }] }],
})

export const JournalLine = Norn.type({
  name: "erp.JournalLine", title: "Journal line", version: "0.1.0",
  properties: PostingShape,
  identity: [{ name: "primary", properties: ["line_id"] }],
  links: [{ id: "doc", target: JournalEntry, cardinality: { to: "one", from: "many", required: true }, inverse: "lines" }],
})

export const EntryBalances = Norn.law({
  id: "erp.journal_entry_balanced.law", title: "A posted journal entry balances: debits equal credits over at least two lines", version: "0.1.0",
  dependencies: [{ artifactId: "erp.JournalEntry.entity", kind: "entity", version: "0.1.0", required: true }],
  law: { lawVersion: "0.1", id: "erp.journal_entry_balanced", owner: "erp", severity: "error", rationale: "A posted journal entry balances: debits equal credits over at least two lines",
    provenance: { source: "authoring_sdk", sourcePath: "crates/norn-cell-do/tests/fixtures/erp-day/src/erp.ts" },
    scope: { root: { kind: "entity", id: "erp.JournalEntry" }, objectTypes: ["erp.JournalEntry"] },
    subject: "erp.JournalEntry", modality: { kind: "always" }, condition: "self.status != 'draft'", response: { kind: "predicate", expression: "len(self.lines) >= 2 and sum(map(self.lines, #.debit)) == sum(map(self.lines, #.credit))" } },
})

export const LineOneSided = Norn.law({
  id: "erp.journal_line_one_sided.law", title: "A journal line is a debit or a credit, never both", version: "0.1.0",
  dependencies: [{ artifactId: "erp.JournalLine.entity", kind: "entity", version: "0.1.0", required: true }],
  law: { lawVersion: "0.1", id: "erp.journal_line_one_sided", owner: "erp", severity: "error", rationale: "A journal line is a debit or a credit, never both",
    provenance: { source: "authoring_sdk", sourcePath: "crates/norn-cell-do/tests/fixtures/erp-day/src/erp.ts" },
    scope: { root: { kind: "entity", id: "erp.JournalLine" }, objectTypes: ["erp.JournalLine"] },
    subject: "erp.JournalLine", modality: { kind: "always" }, response: { kind: "predicate", expression: "(self.debit > 0 and self.credit == 0) or (self.credit > 0 and self.debit == 0)" } },
})

export const CreateEntry = Norn.action({
  id: "erp.journal.create", title: "New journal entry", version: "0.1.0", scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true, access: { artifactId: "erp.access.policy", version: "0.1.0" },
  input: Schema.Struct({ doc_id: Schema.String, memo: Schema.String }), output: DocRef,
  writes: [{ type: JournalEntry, change: "created" }],

  effect: { kind: "upsert_input_object", type: JournalEntry, objectIdField: "doc_id", writeMode: { kind: "create_only" } },
})

export const AddPosting = Norn.action({
  id: "erp.journal.add_line", title: "Add a journal line", version: "0.1.0", scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true, access: { artifactId: "erp.access.policy", version: "0.1.0" },
  input: PostingShape, output: Schema.Struct({ line_id: Schema.String }),
  slice: [{ name: "doc", type: JournalEntry, key: { property: { from: "input", property: "doc_id" } } }],
  preconditions: [Norn.expression(Schema.Struct({ slice: Schema.Struct({ doc: Status }) }), (x) => x.slice.doc.status === "draft")],
  createLinks: [{ link: "doc", targetType: JournalEntry, targetIdField: "doc_id" }],
  writes: [{ type: JournalLine, change: "created" }],

  effect: { kind: "upsert_input_object", type: JournalLine, objectIdField: "line_id", writeMode: { kind: "create_only" } },
})

export const PostEntry = Norn.action({
  id: "erp.journal.post", title: "Post a journal entry", version: "0.1.0", scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true, access: { artifactId: "erp.access.policy", version: "0.1.0" },
  input: DocRef, output: DocRef,
  writes: [{ type: JournalEntry, change: "modified" }],
  effect: { kind: "fire_transition", type: JournalEntry, objectIdField: "doc_id", lifecycle: "status", transition: "submit" },
})

// --- Access: staff roles act on every document type
export const Access = Norn.access({
  id: "erp.access", title: "ERP staff access", version: "0.1.0",
  rules: [
    { ruleId: "customers", actions: [CreateCustomer], resourceType: Customer, scope: { dimensions: ["org", "environment"] }, allOf: [{ role: { anyOf: ["member", "admin", "owner"] } }] },
    { ruleId: "items", actions: [CreateItem], resourceType: Item, scope: { dimensions: ["org", "environment"] }, allOf: [{ role: { anyOf: ["member", "admin", "owner"] } }] },
    { ruleId: "bins", actions: [OpenBin, MoveStock], resourceType: Bin, scope: { dimensions: ["org", "environment"] }, allOf: [{ role: { anyOf: ["member", "admin", "owner"] } }] },
    { ruleId: "sales_orders", actions: [CreateOrder, SubmitOrder, CompleteOrder], resourceType: SalesOrder, scope: { dimensions: ["org", "environment"] }, allOf: [{ role: { anyOf: ["member", "admin", "owner"] } }] },
    { ruleId: "sales_order_lines", actions: [AddOrderLine], resourceType: SalesOrderLine, scope: { dimensions: ["org", "environment"] }, allOf: [{ role: { anyOf: ["member", "admin", "owner"] } }] },
    { ruleId: "deliveries", actions: [CreateDelivery, SubmitDelivery], resourceType: Delivery, scope: { dimensions: ["org", "environment"] }, allOf: [{ role: { anyOf: ["member", "admin", "owner"] } }] },
    { ruleId: "invoices", actions: [CreateInvoice, SubmitInvoice, AllocateInvoice, CloseInvoice], resourceType: SalesInvoice, scope: { dimensions: ["org", "environment"] }, allOf: [{ role: { anyOf: ["member", "admin", "owner"] } }] },
    { ruleId: "invoice_lines", actions: [AddInvoiceLine], resourceType: InvoiceLine, scope: { dimensions: ["org", "environment"] }, allOf: [{ role: { anyOf: ["member", "admin", "owner"] } }] },
    { ruleId: "payments", actions: [ReceivePayment, SpendPayment], resourceType: Payment, scope: { dimensions: ["org", "environment"] }, allOf: [{ role: { anyOf: ["member", "admin", "owner"] } }] },
    { ruleId: "purchase_orders", actions: [CreatePurchase, SubmitPurchase, ReceivePurchase], resourceType: PurchaseOrder, scope: { dimensions: ["org", "environment"] }, allOf: [{ role: { anyOf: ["member", "admin", "owner"] } }] },
    { ruleId: "purchase_lines", actions: [AddPurchaseLine], resourceType: PurchaseOrderLine, scope: { dimensions: ["org", "environment"] }, allOf: [{ role: { anyOf: ["member", "admin", "owner"] } }] },
    { ruleId: "journal_entries", actions: [CreateEntry, PostEntry], resourceType: JournalEntry, scope: { dimensions: ["org", "environment"] }, allOf: [{ role: { anyOf: ["member", "admin", "owner"] } }] },
    { ruleId: "journal_lines", actions: [AddPosting], resourceType: JournalLine, scope: { dimensions: ["org", "environment"] }, allOf: [{ role: { anyOf: ["member", "admin", "owner"] } }] },
  ],
})
