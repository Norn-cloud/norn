import { expect, test } from "bun:test"
import { averageReferencePost, type AverageReference } from "../src/parity/stock-average-reference.ts"

const empty: AverageReference = { quantity: 0n, value: 0n, issues: new Map() }

function conserved(posting: ReturnType<typeof averageReferencePost>) {
  expect(posting.after.quantity - posting.before.quantity).toBe(posting.stock.quantity)
  expect(posting.after.value - posting.before.value).toBe(posting.stock.value)
  expect(posting.stock.quantity + posting.counterpart.quantity).toBe(0n)
  expect(posting.stock.value + posting.counterpart.value).toBe(0n)
  expect(posting.after.quantity >= 0n && posting.after.value >= 0n).toBe(true)
  if (posting.after.quantity === 0n) expect(posting.after.value).toBe(0n)
}

test("average depletion and every two-part original-cost return conserve exact residuals", () => {
  for (let q = 1n; q <= 8n; q++) for (let value = 0n; value <= 8n; value++) {
    const received = averageReferencePost(empty, { kind: "receipt", id: "receipt", quantity: q, value })
    conserved(received)
    for (let issue = 1n; issue <= q; issue++) {
      const shipped = averageReferencePost(received.after, { kind: "delivery", id: "ship", quantity: issue })
      conserved(shipped)
      for (let partial = 1n; partial <= issue; partial++) {
        const returned = averageReferencePost(shipped.after, { kind: "return", id: "first", delivery: "ship", quantity: partial })
        conserved(returned)
        const complete = partial === issue ? returned : averageReferencePost(returned.after, {
          kind: "return", id: "last", delivery: "ship", quantity: issue - partial,
        })
        conserved(complete)
        expect(complete.after.quantity).toBe(received.after.quantity)
        expect(complete.after.value).toBe(received.after.value)
        const original = complete.after.issues.get("ship")
        expect(original?.returned).toEqual({ quantity: issue, value: -shipped.stock.value })
      }
    }
  }
})

test("returns use original issue cost after the moving average changes", () => {
  const received = averageReferencePost(empty, { kind: "receipt", id: "a", quantity: 2n, value: 4n })
  const shipped = averageReferencePost(received.after, { kind: "delivery", id: "ship", quantity: 1n })
  const repriced = averageReferencePost(shipped.after, { kind: "receipt", id: "b", quantity: 1n, value: 100n })
  const returned = averageReferencePost(repriced.after, { kind: "return", id: "return", delivery: "ship", quantity: 1n })
  conserved(returned)
  expect(returned.stock.value).toBe(2n)
  expect(returned.after.value).toBe(104n)
})

test("repeated tiny returns cap cumulative cost and give final return the residual", () => {
  let state = averageReferencePost(empty, { kind: "receipt", id: "a", quantity: 3n, value: 1n }).after
  state = averageReferencePost(state, { kind: "delivery", id: "ship", quantity: 3n }).after
  const values: bigint[] = []
  for (const id of ["one", "two", "three"]) {
    const returned = averageReferencePost(state, { kind: "return", id, delivery: "ship", quantity: 1n })
    conserved(returned)
    values.push(returned.stock.value)
    state = returned.after
  }
  expect(values).toEqual([0n, 0n, 1n])
  const snapshot = structuredClone(state)
  expect(() => averageReferencePost(state, { kind: "return", id: "four", delivery: "ship", quantity: 1n })).toThrow("over-return")
  expect(state).toEqual(snapshot)
})


test("rounded partial returns never exceed the original issue value", () => {
  let state = averageReferencePost(empty, { kind: "receipt", id: "a", quantity: 5n, value: 3n }).after
  state = averageReferencePost(state, { kind: "delivery", id: "ship", quantity: 5n }).after
  const values: bigint[] = []
  for (let index = 0; index < 5; index++) {
    const returned = averageReferencePost(state, { kind: "return", id: String(index), delivery: "ship", quantity: 1n })
    conserved(returned)
    values.push(returned.stock.value)
    state = returned.after
  }
  expect(values).toEqual([1n, 1n, 1n, 0n, 0n])
  expect(state.value).toBe(3n)
})


test("different receipt costs combine into a quantity-weighted moving average", () => {
  const first = averageReferencePost(empty, { kind: "receipt", id: "a", quantity: 2n, value: 4n })
  const second = averageReferencePost(first.after, { kind: "receipt", id: "b", quantity: 1n, value: 11n })
  const issue = averageReferencePost(second.after, { kind: "delivery", id: "ship", quantity: 1n })
  conserved(issue)
  expect(issue.stock.value).toBe(-5n)
  const depleted = averageReferencePost(issue.after, { kind: "delivery", id: "last", quantity: 2n })
  conserved(depleted)
  expect(depleted.stock.value).toBe(-10n)
  expect(depleted.after.quantity).toBe(0n)
  expect(depleted.after.value).toBe(0n)
})
