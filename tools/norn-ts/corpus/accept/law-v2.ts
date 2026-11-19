// Intent: law(name, { on, must | never, when, within, because }) (A3, #1176;
// dx-spec §4.5). Id, scope, objectTypes, lawVersion, owner, provenance and the
// entity dependency are inferred; `self` is the subject's properties (and a to-one link's target).
import { Schema } from "effect"
import { Norn } from "@norn/authoring"

export const Payable = Norn.interfaceType({
  name: "books.Payable",
  title: "Payable",
  version: "0.1.0",
  properties: Schema.Struct({ total: Schema.Int, paid: Schema.Int }),
})

export const InvoiceLine = Norn.type({
  name: "books.InvoiceLine",
  title: "Invoice line",
  version: "0.1.0",
  properties: Schema.Struct({ line_id: Schema.String, amount: Schema.Int }),
  identity: [{ name: "primary", properties: ["line_id"] }],
})

export const Invoice = Norn.type({
  name: "books.Invoice",
  title: "Invoice",
  version: "0.1.0",
  implements: [Payable],
  properties: Schema.Struct({
    invoice_id: Schema.String,
    total: Schema.Int, net: Schema.Int, tax: Schema.Int,
    paid: Schema.Int,
    status: Schema.Literals(["draft", "issued", "paid"]),
    issue_date: Norn.date(),
    due_date: Norn.date(),
    pay_by: Norn.timestamp("utc"),
  }),
  identity: [{ name: "primary", properties: ["invoice_id"] }],
  links: [{ id: "lines", target: InvoiceLine, cardinality: { to: "many", from: "one" } }],
})

// Declared once on the interface; every implementation inherits it.
export const notOverpaid = Norn.law("not_overpaid", {
  on: Payable,
  must: (self) => self.paid <= self.total,
  because: "Nothing is paid beyond its total.",
})

export const linesAddUp = Norn.law("lines_add_up", {
  on: Invoice,
  when: (self) => self.status !== "draft",
  must: (self) => Norn.sum([{ amount: self.net }, { amount: self.tax }].map((l) => l.amount)) === self.total && [{ amount: self.net }, { amount: self.tax }].every((l) => l.amount > 0),
  because: "An issued invoice totals its net and tax.",
})

export const dueAfterIssue = Norn.law("due_after_issue", {
  on: Invoice,
  never: (self) => self.due_date < self.issue_date,
  severity: "warning",
  because: "An invoice is never due before it is issued.",
})

export const paidInTime = Norn.law("paid_in_time", {
  on: Invoice,
  when: (self) => self.status === "issued",
  eventually: (self) => self.status === "paid",
  within: { before: (self) => self.pay_by },
  because: "An issued invoice is paid by its deadline.",
})

export const keptId = Norn.law("terms", {
  on: Invoice,
  id: "books.payment_terms",
  must: (self) => Norn.daysBetween(self.issue_date, self.due_date) <= 90,
  because: "Payment terms are at most 90 days.",
})

export const issuedPromptly = Norn.law("issued_promptly", {
  on: Invoice,
  when: (self) => self.status === "draft",
  eventually: (self) => self.status !== "draft",
  within: { days: 3 },
  because: "A draft invoice is issued within three days.",
})
