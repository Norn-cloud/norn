import { Schema } from "effect"
import { Norn } from "@norn/authoring"

/**
 * Purchase-to-pay over a public SAP log: the OCEL 2.0 procure-to-pay sample
 * (ocel-standard.org, Zenodo record 8412920). Every OCEL object type is an
 * ontology type, every OCEL event a `p2p.ProcessEvent`, every event-to-object
 * relation a `p2p.EventObject` carrying its qualifier, and every
 * object-to-object relation a typed link named after its qualifier.
 *
 * The log is loaded by `demo/p2p/ocel_load.py`, which reads the OCEL JSON and
 * writes a recorded snapshot for `observations:load` / `norn dev --load`.
 * Each event's own time is a property (`occurred_at`, and on each object the
 * time of the event that created it), so it is the valid time questions
 * window on, distinct from the commit that recorded it.
 *
 * The log carries no currency: amounts are typed `USD` as an assumption, said
 * so here and in the loader.
 */

const Money = Norn.decimal(2, { currency: "currency", additivity: { kind: "additive" } })
const Price = Norn.decimal(2, { currency: "currency" })
const Currency = Schema.Literals(["USD"])
const Timestamp = Norn.ref("iso8601.timestamp")

export const Requisition = Norn.type({
  name: "p2p.Requisition", title: "Purchase requisition", version: "0.1.0", role: "thing",
  description: "An internal request to buy materials (SAP EBAN), before any vendor is chosen.",
  properties: Schema.Struct({
    id: Schema.String, purchasing_group: Schema.NullOr(Schema.String), release_indicator: Schema.NullOr(Schema.String),
    created_at: Schema.NullOr(Timestamp),
  }),
  identity: [{ name: "primary", properties: ["id"] }],
})

export const Quotation = Norn.type({
  name: "p2p.Quotation", title: "Request for quotation", version: "0.1.0", role: "thing",
  description: "A request for quotation sent to a vendor for a requisition (SAP EBAN, RFQ type).",
  properties: Schema.Struct({
    id: Schema.String, purchasing_group: Schema.NullOr(Schema.String), rfq_type: Schema.NullOr(Schema.String),
    purchasing_org: Schema.NullOr(Schema.String), vendor: Schema.NullOr(Schema.String), created_at: Schema.NullOr(Timestamp),
  }),
  identity: [{ name: "primary", properties: ["id"] }],
  links: [{ id: "requisition", description: "OCEL O2O `Quotation of PR`.", target: Requisition, cardinality: { to: "one", from: "many" }, inverse: "quotations" }],
})

export const PurchaseOrder = Norn.type({
  name: "p2p.PurchaseOrder", title: "Purchase order", version: "0.1.0", role: "thing",
  description: "A purchase order to a vendor (SAP EKKO). Its lines are the `p2p.OrderLine` objects that link to it; the log states no order total, so an order's value is the sum of its lines.",
  properties: Schema.Struct({
    id: Schema.String, purchasing_group: Schema.NullOr(Schema.String), purchasing_org: Schema.NullOr(Schema.String),
    document_type: Schema.NullOr(Schema.String), release_status: Schema.NullOr(Schema.String), vendor: Schema.String,
    currency: Currency, created_at: Schema.NullOr(Timestamp), approved_at: Schema.NullOr(Timestamp),
  }),
  identity: [{ name: "primary", properties: ["id"] }],
  links: [{ id: "quotation", description: "OCEL O2O `Purchase Order of Quotation`.", target: Quotation, cardinality: { to: "one", from: "many" }, inverse: "purchase_orders" }],
})

export const OrderLine = Norn.type({
  name: "p2p.OrderLine", title: "Purchase order line", version: "0.1.0", role: "thing",
  description: "One material line of a purchase order (OCEL `material`, SAP EKPO): quantity at a net price. Its amount is quantity times net price, computed by the loader, since the log states none.",
  properties: Schema.Struct({
    id: Schema.String, material: Schema.NullOr(Schema.String), plant: Schema.NullOr(Schema.String), storage_location: Schema.NullOr(Schema.String),
    quantity: Norn.decimal(3), net_price: Price, currency: Currency, amount: Money, delivery_date: Schema.NullOr(Timestamp),
  }),
  identity: [{ name: "primary", properties: ["id"] }],
  links: [
    { id: "purchase_order", description: "OCEL O2O `Materials of Purchase Order`.", target: PurchaseOrder, cardinality: { to: "one", from: "many" }, inverse: "lines" },
    { id: "requisition", description: "OCEL O2O `assigned_materials of PR`.", target: Requisition, cardinality: { to: "one", from: "many" }, inverse: "materials" },
    { id: "quotation", description: "OCEL O2O `Materials of Quotation`.", target: Quotation, cardinality: { to: "one", from: "many" }, inverse: "materials" },
  ],
})

export const InvoiceReceipt = Norn.type({
  name: "p2p.InvoiceReceipt", title: "Vendor invoice", version: "0.1.0", role: "thing",
  description: "A vendor invoice as received (SAP RSEG/BSEG): its debit amount, its payment block, and the purchase order it bills.",
  properties: Schema.Struct({
    id: Schema.String, vendor: Schema.NullOr(Schema.String), currency: Currency, amount: Money, credit_amount: Schema.NullOr(Money),
    payment_block: Schema.NullOr(Schema.String), received_at: Schema.NullOr(Timestamp),
  }),
  identity: [{ name: "primary", properties: ["id"] }],
  links: [{ id: "purchase_order", description: "OCEL O2O `Invoice Receipts of Purchase Order`.", target: PurchaseOrder, cardinality: { to: "one", from: "many" }, inverse: "invoices" }],
})

export const GoodsReceipt = Norn.type({
  name: "p2p.GoodsReceipt", title: "Goods receipt", version: "0.1.0", role: "happening",
  description: "A goods receipt posted against a purchase order (SAP MKPF/MSEG). The log records no received quantity.",
  properties: Schema.Struct({
    id: Schema.String, movement_type: Schema.NullOr(Schema.String), invoice_expected: Schema.NullOr(Schema.String),
    posting_date: Schema.NullOr(Timestamp), received_at: Schema.NullOr(Timestamp),
  }),
  identity: [{ name: "primary", properties: ["id"] }],
  links: [
    { id: "purchase_order", description: "OCEL O2O `Good Receipts of Purchase Order`.", target: PurchaseOrder, cardinality: { to: "one", from: "many" }, inverse: "goods_receipts" },
    { id: "invoice", description: "OCEL O2O `Invoice Receipt of Goods Receipt`.", target: InvoiceReceipt, cardinality: { to: "one", from: "many" }, inverse: "goods_receipts" },
  ],
})

export const Payment = Norn.type({
  name: "p2p.Payment", title: "Payment", version: "0.1.0", role: "happening",
  description: "A payment run's payment of a vendor invoice (SAP payment method ZLSCH, amount DMBTR). `paid_at` is the time of its `Execute Payment` event.",
  properties: Schema.Struct({
    id: Schema.String, method: Schema.NullOr(Schema.String), currency: Currency, amount: Money, paid_at: Schema.NullOr(Timestamp),
  }),
  identity: [{ name: "primary", properties: ["id"] }],
  links: [
    { id: "invoice", description: "OCEL O2O `invoice_receipt_pm`.", target: InvoiceReceipt, cardinality: { to: "one", from: "many" }, inverse: "payments" },
    { id: "goods_receipt", description: "OCEL O2O `goods_receipt_pm`.", target: GoodsReceipt, cardinality: { to: "one", from: "many" }, inverse: "payments" },
    { id: "purchase_order", description: "OCEL O2O `order_pm`.", target: PurchaseOrder, cardinality: { to: "one", from: "many" }, inverse: "payments" },
  ],
})

export const ProcessEvent = Norn.type({
  name: "p2p.ProcessEvent", title: "Process event", version: "0.1.0", role: "happening",
  description: "One OCEL event: an activity that happened at `occurred_at`, performed by a resource. The objects it touched are its `p2p.EventObject` relations, each with the OCEL qualifier.",
  properties: Schema.Struct({
    id: Schema.String, activity: Schema.String, occurred_at: Timestamp, resource: Schema.NullOr(Schema.String), lifecycle: Schema.NullOr(Schema.String),
  }),
  identity: [{ name: "primary", properties: ["id"] }],
})

export const EventObject = Norn.type({
  name: "p2p.EventObject", title: "Event-to-object relation", version: "0.1.0", role: "thing",
  description: "One OCEL event-to-object relation: the event, the object it touched (by OCEL id and type) and the qualifier naming the object's role in it.",
  properties: Schema.Struct({
    id: Schema.String, event_id: Schema.String, object_id: Schema.String, object_type: Schema.String, qualifier: Schema.String,
  }),
  identity: [{ name: "primary", properties: ["id"] }],
  links: [{ id: "event", target: ProcessEvent, cardinality: { to: "one", from: "many", required: true }, inverse: "objects" }],
})

const law = (id: string, title: string, subject: string, rationale: string, condition: string, expression: string, entity: string) => Norn.law({
  id: `${id}.law`, title, version: "0.1.0",
  dependencies: [{ artifactId: `${entity}.entity`, kind: "entity", version: "0.1.0", required: true }],
  law: {
    lawVersion: "0.1", id, owner: "p2p", severity: "error", rationale,
    provenance: { source: "authoring_sdk", sourcePath: "registry/packs/p2p/p2p.ts" },
    subject, scope: { root: { kind: "entity", id: subject }, objectTypes: [subject] },
    condition, modality: { kind: "always" }, response: { kind: "predicate", expression },
  },
})

/** No invoice without a purchase order: an invoice must bill an order. */
export const InvoiceHasOrder = law("p2p.InvoiceReceipt.has_order", "An invoice bills a purchase order", "p2p.InvoiceReceipt",
  "An invoice that names no purchase order cannot be matched; it is paid on trust.",
  "true", "self.purchase_order != null", "p2p.InvoiceReceipt")

/** Three-way match, amounts: an invoice bills the value of its order's lines. */
export const InvoiceMatchesOrder = law("p2p.InvoiceReceipt.matches_order", "An invoice's amount equals its order's lines", "p2p.InvoiceReceipt",
  "An invoice must bill what was ordered: its amount equals the sum of its purchase order's line amounts.",
  "self.purchase_order != null && len(self.purchase_order.lines) > 0", "self.amount == sum(map(self.purchase_order.lines, #.amount))", "p2p.InvoiceReceipt")

/** A payment pays what its invoice bills. */
export const PaymentMatchesInvoice = law("p2p.Payment.matches_invoice", "A payment pays its invoice's amount", "p2p.Payment",
  "A payment that differs from the invoice it settles over- or under-pays the vendor.",
  "self.invoice != null", "self.amount == self.invoice.amount", "p2p.Payment")

/** Payment after goods receipt: nothing is paid before it is received. */
export const PaidAfterReceipt = law("p2p.Payment.after_goods_receipt", "A payment follows its goods receipt", "p2p.Payment",
  "Paying before the goods are received pays for goods that may never arrive.",
  "self.goods_receipt != null && self.paid_at != null && self.goods_receipt.received_at != null", "self.goods_receipt.received_at <= self.paid_at", "p2p.Payment")

export const Source = Norn.sourceSchema({
  id: "p2p.ocel.source", title: "OCEL 2.0 procure-to-pay log", version: "0.1.0",
  source: { system: "ocel2-p2p", instance: "zenodo-8412920", version: "ocel2-p2p.json" },
  observedAt: "2023-10-04T00:00:00Z", adapter: { name: "norn-ocel-load", version: "0.1.0" },
  rawMetadata: { uri: "https://zenodo.org/records/8412920/files/ocel2-p2p.json", sha256: "33a8a412ad9d7b124c50e89581f243344dc4d7b80f5bdd38b5306edb9b9fa2ec" },
  datasets: [
    { id: "purchase_requisition", nativeName: "OCEL object type purchase_requisition", kind: "document" },
    { id: "quotation", nativeName: "OCEL object type quotation", kind: "document" },
    { id: "purchase_order", nativeName: "OCEL object type purchase_order", kind: "document" },
    { id: "material", nativeName: "OCEL object type material", kind: "document" },
    { id: "invoice receipt", nativeName: "OCEL object type invoice receipt", kind: "document" },
    { id: "goods receipt", nativeName: "OCEL object type goods receipt", kind: "document" },
    { id: "payment", nativeName: "OCEL object type payment", kind: "document" },
    { id: "event", nativeName: "OCEL events", kind: "document" },
    { id: "e2o", nativeName: "OCEL event-to-object relations", kind: "document" },
  ],
  fields: [
    { dataset: "purchase_requisition", name: "id", ordinal: 0, nativeType: "Text", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "purchase_requisition", name: "purchasing_group", ordinal: 1, nativeType: "Text", typeCandidate: { type: "string", origin: "declared" }, required: false },
    { dataset: "purchase_requisition", name: "release_indicator", ordinal: 2, nativeType: "Text", typeCandidate: { type: "string", origin: "declared" }, required: false },
    { dataset: "purchase_requisition", name: "created_at", ordinal: 3, nativeType: "Datetime", typeCandidate: { type: "string", origin: "declared" }, required: false },
    { dataset: "quotation", name: "id", ordinal: 0, nativeType: "Text", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "quotation", name: "purchasing_group", ordinal: 1, nativeType: "Text", typeCandidate: { type: "string", origin: "declared" }, required: false },
    { dataset: "quotation", name: "rfq_type", ordinal: 2, nativeType: "Text", typeCandidate: { type: "string", origin: "declared" }, required: false },
    { dataset: "quotation", name: "purchasing_org", ordinal: 3, nativeType: "Text", typeCandidate: { type: "string", origin: "declared" }, required: false },
    { dataset: "quotation", name: "vendor", ordinal: 4, nativeType: "Text", typeCandidate: { type: "string", origin: "declared" }, required: false },
    { dataset: "quotation", name: "created_at", ordinal: 5, nativeType: "Datetime", typeCandidate: { type: "string", origin: "declared" }, required: false },
    { dataset: "quotation", name: "requisition", ordinal: 6, nativeType: "Reference", typeCandidate: { type: "reference", origin: "declared" }, required: false },
    { dataset: "purchase_order", name: "id", ordinal: 0, nativeType: "Text", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "purchase_order", name: "purchasing_group", ordinal: 1, nativeType: "Text", typeCandidate: { type: "string", origin: "declared" }, required: false },
    { dataset: "purchase_order", name: "purchasing_org", ordinal: 2, nativeType: "Text", typeCandidate: { type: "string", origin: "declared" }, required: false },
    { dataset: "purchase_order", name: "document_type", ordinal: 3, nativeType: "Text", typeCandidate: { type: "string", origin: "declared" }, required: false },
    { dataset: "purchase_order", name: "release_status", ordinal: 4, nativeType: "Text", typeCandidate: { type: "string", origin: "declared" }, required: false },
    { dataset: "purchase_order", name: "vendor", ordinal: 5, nativeType: "Text", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "purchase_order", name: "created_at", ordinal: 6, nativeType: "Datetime", typeCandidate: { type: "string", origin: "declared" }, required: false },
    { dataset: "purchase_order", name: "approved_at", ordinal: 7, nativeType: "Datetime", typeCandidate: { type: "string", origin: "declared" }, required: false },
    { dataset: "purchase_order", name: "quotation", ordinal: 8, nativeType: "Reference", typeCandidate: { type: "reference", origin: "declared" }, required: false },
    { dataset: "material", name: "id", ordinal: 0, nativeType: "Text", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "material", name: "material", ordinal: 1, nativeType: "Text", typeCandidate: { type: "string", origin: "declared" }, required: false },
    { dataset: "material", name: "plant", ordinal: 2, nativeType: "Text", typeCandidate: { type: "string", origin: "declared" }, required: false },
    { dataset: "material", name: "storage_location", ordinal: 3, nativeType: "Text", typeCandidate: { type: "string", origin: "declared" }, required: false },
    { dataset: "material", name: "quantity", ordinal: 4, nativeType: "Decimal", typeCandidate: { type: "decimal", origin: "declared" }, required: true },
    { dataset: "material", name: "net_price", ordinal: 5, nativeType: "Decimal", typeCandidate: { type: "decimal", origin: "declared" }, required: true },
    { dataset: "material", name: "amount", ordinal: 6, nativeType: "Decimal", typeCandidate: { type: "decimal", origin: "declared" }, required: true },
    { dataset: "material", name: "delivery_date", ordinal: 7, nativeType: "Datetime", typeCandidate: { type: "string", origin: "declared" }, required: false },
    { dataset: "material", name: "purchase_order", ordinal: 8, nativeType: "Reference", typeCandidate: { type: "reference", origin: "declared" }, required: false },
    { dataset: "material", name: "requisition", ordinal: 9, nativeType: "Reference", typeCandidate: { type: "reference", origin: "declared" }, required: false },
    { dataset: "material", name: "quotation", ordinal: 10, nativeType: "Reference", typeCandidate: { type: "reference", origin: "declared" }, required: false },
    { dataset: "invoice receipt", name: "id", ordinal: 0, nativeType: "Text", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "invoice receipt", name: "vendor", ordinal: 1, nativeType: "Text", typeCandidate: { type: "string", origin: "declared" }, required: false },
    { dataset: "invoice receipt", name: "amount", ordinal: 2, nativeType: "Decimal", typeCandidate: { type: "decimal", origin: "declared" }, required: true },
    { dataset: "invoice receipt", name: "credit_amount", ordinal: 3, nativeType: "Decimal", typeCandidate: { type: "decimal", origin: "declared" }, required: false },
    { dataset: "invoice receipt", name: "payment_block", ordinal: 4, nativeType: "Text", typeCandidate: { type: "string", origin: "declared" }, required: false },
    { dataset: "invoice receipt", name: "received_at", ordinal: 5, nativeType: "Datetime", typeCandidate: { type: "string", origin: "declared" }, required: false },
    { dataset: "invoice receipt", name: "purchase_order", ordinal: 6, nativeType: "Reference", typeCandidate: { type: "reference", origin: "declared" }, required: false },
    { dataset: "goods receipt", name: "id", ordinal: 0, nativeType: "Text", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "goods receipt", name: "movement_type", ordinal: 1, nativeType: "Text", typeCandidate: { type: "string", origin: "declared" }, required: false },
    { dataset: "goods receipt", name: "invoice_expected", ordinal: 2, nativeType: "Text", typeCandidate: { type: "string", origin: "declared" }, required: false },
    { dataset: "goods receipt", name: "posting_date", ordinal: 3, nativeType: "Datetime", typeCandidate: { type: "string", origin: "declared" }, required: false },
    { dataset: "goods receipt", name: "received_at", ordinal: 4, nativeType: "Datetime", typeCandidate: { type: "string", origin: "declared" }, required: false },
    { dataset: "goods receipt", name: "purchase_order", ordinal: 5, nativeType: "Reference", typeCandidate: { type: "reference", origin: "declared" }, required: false },
    { dataset: "goods receipt", name: "invoice", ordinal: 6, nativeType: "Reference", typeCandidate: { type: "reference", origin: "declared" }, required: false },
    { dataset: "payment", name: "id", ordinal: 0, nativeType: "Text", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "payment", name: "method", ordinal: 1, nativeType: "Text", typeCandidate: { type: "string", origin: "declared" }, required: false },
    { dataset: "payment", name: "amount", ordinal: 2, nativeType: "Decimal", typeCandidate: { type: "decimal", origin: "declared" }, required: true },
    { dataset: "payment", name: "paid_at", ordinal: 3, nativeType: "Datetime", typeCandidate: { type: "string", origin: "declared" }, required: false },
    { dataset: "payment", name: "invoice", ordinal: 4, nativeType: "Reference", typeCandidate: { type: "reference", origin: "declared" }, required: false },
    { dataset: "payment", name: "goods_receipt", ordinal: 5, nativeType: "Reference", typeCandidate: { type: "reference", origin: "declared" }, required: false },
    { dataset: "payment", name: "purchase_order", ordinal: 6, nativeType: "Reference", typeCandidate: { type: "reference", origin: "declared" }, required: false },
    { dataset: "event", name: "id", ordinal: 0, nativeType: "Text", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "event", name: "activity", ordinal: 1, nativeType: "Text", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "event", name: "occurred_at", ordinal: 2, nativeType: "Datetime", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "event", name: "resource", ordinal: 3, nativeType: "Text", typeCandidate: { type: "string", origin: "declared" }, required: false },
    { dataset: "event", name: "lifecycle", ordinal: 4, nativeType: "Text", typeCandidate: { type: "string", origin: "declared" }, required: false },
    { dataset: "e2o", name: "id", ordinal: 0, nativeType: "Text", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "e2o", name: "event", ordinal: 1, nativeType: "Reference", typeCandidate: { type: "reference", origin: "declared" }, required: true },
    { dataset: "e2o", name: "object_id", ordinal: 2, nativeType: "Text", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "e2o", name: "object_type", ordinal: 3, nativeType: "Text", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "e2o", name: "qualifier", ordinal: 4, nativeType: "Text", typeCandidate: { type: "string", origin: "declared" }, required: true },
  ],
  constraints: [
    { id: "requisition_identity", kind: "primary_key", dataset: "purchase_requisition", fields: ["id"], origin: "declared" },
    { id: "quotation_identity", kind: "primary_key", dataset: "quotation", fields: ["id"], origin: "declared" },
    { id: "order_identity", kind: "primary_key", dataset: "purchase_order", fields: ["id"], origin: "declared" },
    { id: "material_identity", kind: "primary_key", dataset: "material", fields: ["id"], origin: "declared" },
    { id: "invoice_identity", kind: "primary_key", dataset: "invoice receipt", fields: ["id"], origin: "declared" },
    { id: "receipt_identity", kind: "primary_key", dataset: "goods receipt", fields: ["id"], origin: "declared" },
    { id: "payment_identity", kind: "primary_key", dataset: "payment", fields: ["id"], origin: "declared" },
    { id: "event_identity", kind: "primary_key", dataset: "event", fields: ["id"], origin: "declared" },
    { id: "e2o_identity", kind: "primary_key", dataset: "e2o", fields: ["id"], origin: "declared" },
  ],
})

const p = (property: string, value = `input.${property}`) => ({ property, value })

export const Mapping = Norn.mapping({
  id: "p2p.ocel.mapping", title: "OCEL 2.0 procure-to-pay objects, events and relations", version: "0.1.0",
  source: { schemaId: "p2p.ocel.source", version: "0.1.0", sha256: "0000000000000000000000000000000000000000000000000000000000000000" },
  sourceContract: { kind: "generic" },
  maps: [
    { id: "requisition", source: { dataset: "purchase_requisition" }, target: { type: Requisition }, subject: "input.id", sourceRecord: "'ocel2-p2p/' + input.id",
      properties: [p("id"), p("purchasing_group"), p("release_indicator"), p("created_at")] },
    { id: "quotation", source: { dataset: "quotation" }, target: { type: Quotation }, subject: "input.id", sourceRecord: "'ocel2-p2p/' + input.id",
      properties: [p("id"), p("purchasing_group"), p("rfq_type"), p("purchasing_org"), p("vendor"), p("created_at")],
      links: [{ link: "requisition", targetMap: "requisition", key: "input.requisition" }] },
    { id: "purchase_order", source: { dataset: "purchase_order" }, target: { type: PurchaseOrder }, subject: "input.id", sourceRecord: "'ocel2-p2p/' + input.id",
      properties: [p("id"), p("purchasing_group"), p("purchasing_org"), p("document_type"), p("release_status"), p("vendor"), p("currency", "'USD'"), p("created_at"), p("approved_at")],
      links: [{ link: "quotation", targetMap: "quotation", key: "input.quotation" }] },
    { id: "line", source: { dataset: "material" }, target: { type: OrderLine }, subject: "input.id", sourceRecord: "'ocel2-p2p/' + input.id",
      properties: [p("id"), p("material"), p("plant"), p("storage_location"), p("quantity"), p("net_price"), p("currency", "'USD'"), p("amount"), p("delivery_date")],
      links: [
        { link: "purchase_order", targetMap: "purchase_order", key: "input.purchase_order" },
        { link: "requisition", targetMap: "requisition", key: "input.requisition" },
        { link: "quotation", targetMap: "quotation", key: "input.quotation" },
      ] },
    { id: "invoice", source: { dataset: "invoice receipt" }, target: { type: InvoiceReceipt }, subject: "input.id", sourceRecord: "'ocel2-p2p/' + input.id",
      properties: [p("id"), p("vendor"), p("currency", "'USD'"), p("amount"), p("credit_amount"), p("payment_block"), p("received_at")],
      links: [{ link: "purchase_order", targetMap: "purchase_order", key: "input.purchase_order" }] },
    { id: "goods_receipt", source: { dataset: "goods receipt" }, target: { type: GoodsReceipt }, subject: "input.id", sourceRecord: "'ocel2-p2p/' + input.id",
      properties: [p("id"), p("movement_type"), p("invoice_expected"), p("posting_date"), p("received_at")],
      links: [
        { link: "purchase_order", targetMap: "purchase_order", key: "input.purchase_order" },
        { link: "invoice", targetMap: "invoice", key: "input.invoice" },
      ] },
    { id: "payment", source: { dataset: "payment" }, target: { type: Payment }, subject: "input.id", sourceRecord: "'ocel2-p2p/' + input.id",
      properties: [p("id"), p("method"), p("currency", "'USD'"), p("amount"), p("paid_at")],
      links: [
        { link: "invoice", targetMap: "invoice", key: "input.invoice" },
        { link: "goods_receipt", targetMap: "goods_receipt", key: "input.goods_receipt" },
        { link: "purchase_order", targetMap: "purchase_order", key: "input.purchase_order" },
      ] },
    { id: "event", source: { dataset: "event" }, target: { type: ProcessEvent }, subject: "input.id", sourceRecord: "'ocel2-p2p/' + input.id",
      properties: [p("id"), p("activity"), p("occurred_at"), p("resource"), p("lifecycle")] },
    { id: "e2o", source: { dataset: "e2o" }, target: { type: EventObject }, subject: "input.id", sourceRecord: "'ocel2-p2p/' + input.id",
      properties: [p("id"), p("event_id", "input.event"), p("object_id"), p("object_type"), p("qualifier")],
      links: [{ link: "event", targetMap: "event", key: "input.event" }] },
  ],
})
