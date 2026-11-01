import { expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { Schema } from "effect"
import { averageReferencePost, type AverageReference } from "../src/parity/stock-average-reference.ts"

const Amount = Schema.Struct({ quantity: Schema.String, value: Schema.String })
const Movement = Schema.Union([
  Schema.Struct({ kind: Schema.Literal("receipt"), id: Schema.String, quantity: Schema.String, value: Schema.String }),
  Schema.Struct({ kind: Schema.Literal("delivery"), id: Schema.String, quantity: Schema.String }),
  Schema.Struct({ kind: Schema.Literal("return"), id: Schema.String, delivery: Schema.String, quantity: Schema.String }),
])
const Corpus = Schema.Array(Schema.Array(Schema.Struct({ event: Movement, balance: Amount, allocation: Amount })))
const wire = (amount: { quantity: bigint; value: bigint }) => ({ quantity: String(amount.quantity), value: String(amount.value) })

test("native moving-average corpus matches the independent original-cost reference at every prefix", () => {
  const file = new URL("../../../crates/norn-kernel/tests/fixtures/stock-average-reference.json", import.meta.url)
  const cases = Schema.decodeUnknownSync(Corpus)(JSON.parse(readFileSync(file, "utf8")))
  expect(cases).toHaveLength(648)
  for (const history of cases) {
    let state: AverageReference = { quantity: 0n, value: 0n, issues: new Map() }
    for (const step of history) {
      const event = step.event
      const common = { ...event, quantity: BigInt(event.quantity) }
      const movement = event.kind === "receipt" ? { ...common, kind: event.kind, value: BigInt(event.value) }
        : event.kind === "delivery" ? { ...common, kind: event.kind }
        : { ...common, kind: event.kind, delivery: event.delivery }
      const posting = averageReferencePost(state, movement)
      state = posting.after
      expect(wire(state)).toEqual(step.balance)
      const sign = event.kind === "delivery" ? -1n : 1n
      expect(wire({ quantity: sign * posting.stock.quantity, value: sign * posting.stock.value })).toEqual(step.allocation)
    }
  }
})
