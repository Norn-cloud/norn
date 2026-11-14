// Closure callbacks are one inline arrow with one plain element parameter and
// an expression body; anything else is outside the lambda subset (dx-spec §4.5).
import { Schema } from "effect"
import { Norn } from "@norn/authoring"

const Entry = Schema.Struct({ lines: Schema.Array(Schema.Struct({ amount: Schema.Int })) })
const positive = (l: { readonly amount: number }) => l.amount > 0
function big(n: number): boolean {
  return n > 100
}

export const valueCallback = Norn.expression(Entry, (self) => self.lines.some(positive))
export const blockBody = Norn.expression(Entry, (self) => self.lines.every((l) => { return l.amount > 0 }))
export const indexParam = Norn.expression(Entry, (self) => self.lines.some((l, i) => i > 0))
export const reservedName = Norn.expression(Entry, (self) => self.lines.some((self) => self.amount > 0))
export const shadowed = Norn.expression(Entry, (self) => self.lines.some((l) => self.lines.every((l) => l.amount > 0)))
export const reduce = Norn.expression(Entry, (self) => self.lines.reduce((n, l) => n + l.amount, 0) === 0)
export const helper = Norn.expression(Entry, (self) => self.lines.some((l) => big(l.amount)))
export const ambient = Norn.expression(Entry, (self) => self.lines.some((l) => l.amount > Date.now()))
