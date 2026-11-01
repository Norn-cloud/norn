import { expect, test } from "bun:test"
import {
  referenceBalance, referenceInverse, referencePost, referenceShare,
  type ReferencePosition, type ReferencePosting,
} from "../src/parity/stock-reference.ts"

function conservation(posting: ReferencePosting) {
  const before = referenceBalance(posting.before)
  const after = referenceBalance(posting.after)
  expect(after.quantity - before.quantity).toBe(posting.stock.quantity)
  expect(after.value - before.value).toBe(posting.stock.value)
  expect(posting.stock.quantity + posting.counterpart.quantity).toBe(0n)
  expect(posting.stock.value + posting.counterpart.value).toBe(0n)
  expect(after.quantity >= 0n && after.value >= 0n).toBe(true)
  if (after.quantity === 0n) expect(after.value).toBe(0n)
  const inverse = referenceInverse(posting)
  expect(inverse.after).toEqual(posting.before)
  expect(referenceInverse(inverse)).toEqual(posting)
  expect(posting.stock.quantity + inverse.stock.quantity).toBe(0n)
  expect(posting.stock.value + inverse.stock.value).toBe(0n)
}

test("bounded FIFO histories conserve quantity/value and reverse each transition exactly", () => {
  // 4 quantities x 5 values per receipt; all two-receipt cases and first issues,
  // followed by each possible second issue through full depletion.
  for (let q1 = 1n; q1 <= 4n; q1++) for (let q2 = 1n; q2 <= 4n; q2++) {
    for (let v1 = 0n; v1 <= 4n; v1++) for (let v2 = 0n; v2 <= 4n; v2++) {
      const first = referencePost({ layers: [] }, { kind: "receipt", id: "a", quantity: q1, value: v1 })
      const second = referencePost(first.after, { kind: "receipt", id: "b", quantity: q2, value: v2 })
      conservation(first)
      conservation(second)
      for (let issue = 1n; issue <= q1 + q2; issue++) {
        const posting = referencePost(second.after, { kind: "delivery", id: "ship", quantity: issue })
        conservation(posting)
        expect(posting.allocations.map(layer => layer.quantity)).toEqual([issue < q1 ? issue : q1, issue > q1 ? issue - q1 : 0n])
        expect(posting.allocations.reduce((sum, layer) => sum + layer.value, 0n)).toBe(-posting.stock.value)
        const remaining = q1 + q2 - issue
        for (let next = 1n; next <= remaining; next++) {
          conservation(referencePost(posting.after, { kind: "delivery", id: "next", quantity: next }))
        }
      }
    }
  }
})

test("half-up residuals, large integers and zero-cost receipts are exact", () => {
  expect(referenceShare(1n, { quantity: 2n, value: 1n })).toBe(1n)
  let position: ReferencePosition = { layers: [] }
  const quantity = 999999999999999999n
  const received = referencePost(position, { kind: "receipt", id: "large", quantity, value: quantity * quantity })
  conservation(received)
  position = received.after
  const partial = referencePost(position, { kind: "delivery", id: "part", quantity: quantity - 1n })
  conservation(partial)
  const final = referencePost(partial.after, { kind: "delivery", id: "last", quantity: 1n })
  conservation(final)
  expect(final.after.layers).toEqual([])
})

test("invalid or overselling movements leave their input untouched", () => {
  const before = { layers: [{ receipt: "a", quantity: 2n, value: 1n }] }
  const snapshot = structuredClone(before)
  for (const quantity of [-1n, 0n, 3n]) {
    expect(() => referencePost(before, { kind: "delivery", id: "bad", quantity })).toThrow()
    expect(before).toEqual(snapshot)
  }
  expect(() => referencePost(before, { kind: "receipt", id: "bad", quantity: 1n, value: -1n })).toThrow()
  expect(before).toEqual(snapshot)
})
