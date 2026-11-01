import { referenceShare, type StockAmount } from "./stock-reference.ts"

export type ReferenceIssue = StockAmount & Readonly<{ returned: StockAmount }>
export type AverageReference = StockAmount & Readonly<{ issues: ReadonlyMap<string, ReferenceIssue> }>
export type AverageMovement =
  | Readonly<{ kind: "receipt"; id: string; quantity: bigint; value: bigint }>
  | Readonly<{ kind: "delivery"; id: string; quantity: bigint }>
  | Readonly<{ kind: "return"; id: string; delivery: string; quantity: bigint }>

/** Exact reference only. Issues retain original cost independently of later receipts.
 * Each partial return rounds its original proportional cost, capped by the
 * remaining issue value. The final return takes the exact residual. */
export function averageReferencePost(before: AverageReference, event: AverageMovement) {
  if (event.quantity <= 0n || before.quantity < 0n || before.value < 0n) throw new Error("Invalid amount")
  if (before.quantity === 0n && before.value !== 0n) throw new Error("Value without quantity")
  const issues = new Map(before.issues)
  let stock: StockAmount
  switch (event.kind) {
    case "receipt":
      if (event.value < 0n) throw new Error("Invalid receipt value")
      stock = { quantity: event.quantity, value: event.value }
      break
    case "delivery": {
      if (event.quantity > before.quantity) throw new Error("insufficient-stock")
      if (issues.has(event.id)) throw new Error("Duplicate delivery")
      const value = referenceShare(event.quantity, before)
      issues.set(event.id, { quantity: event.quantity, value, returned: { quantity: 0n, value: 0n } })
      stock = { quantity: -event.quantity, value: -value }
      break
    }
    case "return": {
      const original = issues.get(event.delivery)
      if (!original) throw new Error("Missing original delivery")
      const quantity = original.returned.quantity + event.quantity
      if (quantity > original.quantity) throw new Error("over-return")
      const remaining = original.value - original.returned.value
      const proportional = referenceShare(event.quantity, original)
      const value = quantity === original.quantity || proportional > remaining ? remaining : proportional
      stock = { quantity: event.quantity, value }
      issues.set(event.delivery, { ...original, returned: { quantity, value: original.returned.value + value } })
      break
    }
  }
  const after: AverageReference = {
    quantity: before.quantity + stock.quantity, value: before.value + stock.value, issues,
  }
  return { before, after, stock, counterpart: { quantity: -stock.quantity, value: -stock.value } }
}
