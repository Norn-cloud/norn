import { expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { Schema } from "effect"
import { referencePost, type ReferencePosition } from "../src/parity/stock-reference.ts"

const Layer = Schema.Struct({ receipt: Schema.String, quantity: Schema.String, value: Schema.String })
const Movement = Schema.Union([
  Schema.Struct({ kind: Schema.Literal("receipt"), id: Schema.String, quantity: Schema.String, value: Schema.String }),
  Schema.Struct({ kind: Schema.Literal("delivery"), id: Schema.String, quantity: Schema.String }),
])
const Corpus = Schema.Array(Schema.Struct({
  history: Schema.Array(Movement), layers: Schema.Array(Layer), allocations: Schema.Array(Layer),
}))

test("the Rust FIFO correspondence corpus is exactly the interval reference output", () => {
  const file = new URL("../../../crates/norn-kernel/tests/fixtures/stock-fifo-reference.json", import.meta.url)
  const cases = Schema.decodeUnknownSync(Corpus)(JSON.parse(readFileSync(file, "utf8")))
  expect(cases).toHaveLength(1008)
  for (const sample of cases) {
    let state: ReferencePosition = { layers: [] }
    let allocated = state.layers
    for (const event of sample.history) {
      const common = { id: event.id, quantity: BigInt(event.quantity) }
      const movement = event.kind === "receipt"
        ? { ...common, kind: event.kind, value: BigInt(event.value) }
        : { ...common, kind: event.kind }
      const posting = referencePost(state, movement)
      state = posting.after
      allocated = posting.allocations.filter(layer => layer.quantity > 0n)
    }
    const wire = (layers: ReferencePosition["layers"]) => layers.map(layer => ({
      receipt: layer.receipt, quantity: String(layer.quantity), value: String(layer.value),
    }))
    expect(wire(state.layers)).toEqual([...sample.layers])
    expect(wire(allocated)).toEqual([...sample.allocations])
  }
})
