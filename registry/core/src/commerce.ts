/**
 * `commerce` types of the core registry, in typed authoring. This module
 * reproduces the published `commerce.SalesOrder` artifact; norn#59 changes it
 * next, so that "invoiced" is derived from invoice facts instead of authored.
 */
import { Schema } from "effect"
import { Norn } from "@norn/authoring"

export const SalesOrder = Norn.type({
  name: "commerce.SalesOrder",
  title: "SalesOrder",
  version: "0.1.0",
  description: "An order a customer places with the business.",
  implements: ["core.Order"],
  properties: Schema.Struct({
    order_number: Norn.ref("core.document_number"),
    order_date: Norn.ref("iso8601.date"),
    currency: Norn.ref("iso4217.currency_code"),
    total_amount: Norn.ref("core.money_amount"),
    status: Schema.Literals(["draft", "submitted", "on_hold", "approved", "invoiced", "completed", "cancelled"]),
  }),
  defaults: { status: "draft" },
  links: [
    { id: "counterparty", target: "party.Customer", cardinality: { to: "one", from: "many", required: true } },
    { id: "lines", target: "commerce.SalesOrderLine", cardinality: { to: "many", from: "one" } },
    { id: "amended_from", description: "The order this one amends, if any.", target: "commerce.SalesOrder", cardinality: { to: "one", from: "one" } },
  ],
  identity: [{ name: "primary", properties: ["order_number"] }],
  lifecycles: [
    {
      property: "status",
      description: "ERPNext-style sales order: submit, approve or hold, invoice, complete. `amend` returns a submitted or held order to draft; ERPNext's cancel-then-amend copy is a new order linked through amended_from.",
      initial: ["draft"],
      terminal: ["completed", "cancelled"],
      transitions: [
        { id: "submit", from: ["draft"], to: "submitted" },
        { id: "approve", from: ["submitted", "on_hold"], to: "approved" },
        { id: "hold", from: ["submitted", "approved"], to: "on_hold" },
        { id: "amend", description: "Revise the order before approval.", from: ["submitted", "on_hold"], to: "draft" },
        { id: "invoice", from: ["approved"], to: "invoiced" },
        { id: "complete", from: ["invoiced"], to: "completed" },
        { id: "cancel", from: ["draft", "submitted", "on_hold", "approved"], to: "cancelled" },
      ],
    },
  ],
})
