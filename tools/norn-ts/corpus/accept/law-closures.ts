// Intent: typed lambdas in laws and guards (A2, #1176; dx-spec §4.5):
// array closures, `.length`, Norn.sum and the calendar-day helpers lower to
// norn-zen/6 `some`/`all`/`filter`/`map`/`count`, `sum`, `days_between`, `add_days`.
import { Schema } from "effect"
import { Norn } from "@norn/authoring"

const Line = Schema.Struct({ amount: Schema.Int, sku: Schema.String })
const Entry = Schema.Struct({ status: Schema.Literals(["draft", "posted"]), issue_date: Schema.String, due_date: Schema.String, lines: Schema.Array(Line) })

export const JournalLine = Norn.type({
  name: "books.JournalLine",
  title: "Journal line",
  version: "0.1.0",
  properties: Schema.Struct({ line_id: Schema.String, amount: Schema.Int, sku: Schema.String }),
  identity: [{ name: "primary", properties: ["line_id"] }],
})

export const JournalEntry = Norn.type({
  name: "books.JournalEntry",
  title: "Journal entry",
  version: "0.1.0",
  properties: Schema.Struct({ entry_id: Schema.String, status: Schema.Literals(["draft", "posted"]), issue_date: Norn.date(), due_date: Norn.date() }),
  identity: [{ name: "primary", properties: ["entry_id"] }],
  links: [{ id: "lines", target: JournalLine, cardinality: { to: "many", from: "one" } }],
  lifecycles: [{
    property: "status", initial: ["draft"], terminal: ["posted"],
    transitions: [{ id: "post", from: ["draft"], to: "posted", guards: [Norn.expression(Entry, (self) => self.lines.filter((l) => l.amount !== 0).length >= 2 && Norn.daysBetween(self.issue_date, self.due_date) <= 90)] }],
  }],
})

export const balanced = Norn.law({
  id: "books.JournalEntry.balanced.law",
  title: "A posted entry nets to zero.",
  version: "0.1.0",
  law: {
    id: "books.JournalEntry.balanced",
    lawVersion: "0.1",
    owner: "books",
    severity: "error",
    rationale: "A posted entry nets to zero.",
    provenance: { source: "authoring_sdk" },
    scope: { root: { kind: "app", id: "books" }, objectTypes: ["books.JournalEntry"] },
    subject: "books.JournalEntry",
    condition: Norn.expression(Entry, (self) => self.status === "posted"),
    modality: { kind: "always" },
    response: {
      kind: "predicate",
      expression: Norn.expression(Entry, (self) =>
        Norn.sum(self.lines.map((l) => l.amount)) === 0
        && self.lines.every((l) => l.amount !== 0)
        && !self.lines.some((l) => l.sku.startsWith("VOID-") && ["A", "B"].some((p) => l.sku.endsWith(p)))
        && Norn.addDays(self.issue_date, 30) >= Norn.parseDate(self.due_date)),
    },
  },
  dependencies: [{ artifactId: "books.JournalEntry.entity", kind: "entity", version: "0.1.0", required: true }],
})
