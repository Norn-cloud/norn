// T7-EFX-4 (#2787): Effect Schema syntax -> EntityIR v2. Golden: invoice.ir.json.
// Covers text (branded), integer, exact decimals (with a unit), an ISO 4217
// registry ref, timestamp/date value types, an enum from Schema.Literals and
// from a literal union, Schema.NullOr, exact numeric defaults, an interface,
// a key, and a declared lifecycle.
import { Schema } from "effect"
import { Norn } from "@norn/authoring"

export const Billable = Norn.interfaceType({
  name: "billing.Billable",
  title: "Billable",
  version: "0.1.0",
  description: "Anything with an exact amount in an ISO 4217 currency.",
  properties: Schema.Struct({
    amount: Norn.decimal(2),
    currency: Norn.ref("iso4217.currency_code"),
  }),
})

const InvoiceNumber = Schema.String.pipe(Schema.brand("InvoiceNumber"))
const InvoiceStatus = Schema.Literals(["draft", "issued", "paid", "void"])

export const Invoice = Norn.type({
  name: "billing.Invoice",
  title: "Invoice",
  version: "0.1.0",
  description: "An invoice for an exact amount.",
  implements: [Billable],
  properties: Schema.Struct({
    number: InvoiceNumber,
    amount: Norn.decimal(2),
    currency: Norn.ref("iso4217.currency_code"),
    weight: Norn.decimal(3, { unit: { system: "ucum", code: "kg" } }),
    lines: Schema.Int,
    issued_at: Schema.NullOr(Norn.timestamp("utc")),
    due_on: Schema.NullOr(Norn.date()),
    paid: Schema.Boolean,
    channel: Schema.NullOr(Schema.Union([Schema.Literal("web"), Schema.Literal("pos")])),
    status: InvoiceStatus,
  }),
  identity: [{ name: "primary", properties: ["number"] }],
  defaults: { amount: 0.00, weight: 19.990, lines: 0, paid: false, status: "draft" },
  lifecycles: [
    {
      property: "status",
      description: "Issue, then pay or void.",
      initial: ["draft"],
      terminal: ["paid", "void"],
      transitions: [
        { id: "issue", from: ["draft"], to: "issued" },
        { id: "pay", description: "Record full payment.", from: ["issued"], to: "paid" },
        { id: "cancel", from: ["draft", "issued"], to: "void" },
      ],
    },
  ],
})
