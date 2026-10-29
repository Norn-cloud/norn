// The data-builder twin of invoice.source.ts: the same definitions written
// directly against the closure-free builder. Both lower to invoice.ir.json.
import { E } from "../../src/builder/index.ts"

const amount = E.decimal(2)
const currency = E.ref("iso4217.currency_code")

export const Billable = E.interfaceType({
  name: "billing.Billable",
  title: "Billable",
  version: "0.1.0",
  description: "Anything with an exact amount in an ISO 4217 currency.",
  properties: [E.property("amount", amount), E.property("currency", currency)],
})

export const Invoice = E.objectType({
  name: "billing.Invoice",
  title: "Invoice",
  version: "0.1.0",
  description: "An invoice for an exact amount.",
  implements: [Billable],
  properties: [
    E.property("number", E.text()),
    E.property("amount", amount, { default: { decimal: "0.00" } }),
    E.property("currency", currency),
    E.property("weight", E.decimal(3, { system: "ucum", code: "kg" }), { default: { decimal: "19.990" } }),
    E.property("lines", E.integer, { default: { decimal: "0" } }),
    E.property("issued_at", E.timestamp("utc"), { nullable: true }),
    E.property("due_on", E.date, { nullable: true }),
    E.property("paid", E.boolean, { default: false }),
    E.property("channel", E.enumeration("web", "pos"), { nullable: true }),
    E.property("status", E.enumeration("draft", "issued", "paid", "void"), { default: "draft" }),
  ],
  identity: [{ name: "primary", properties: ["number"] }],
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

export default [Billable, Invoice]
