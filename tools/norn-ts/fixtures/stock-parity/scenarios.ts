import type { StockCommand, StockOperation, StockTrace } from "../../src/parity/stock-trace.ts"

type Step = StockTrace["steps"][number]
type Serial = Extract<Step, { kind: "serial" }>
const at = "2026-01-02T12:00:00Z"
const command = (requestId: string, operation: StockOperation, effectiveAt = at): StockCommand => ({ requestId, effectiveAt, operation })
const serial = (requestId: string, operation: StockOperation, expected: Serial["expected"] = { kind: "accepted" }, effectiveAt = at): Serial =>
  ({ kind: "serial", command: command(requestId, operation, effectiveAt), expected })
const receipt = (quantity = "10", rate = "2"): StockOperation => ({ kind: "receipt", warehouse: "a", quantity, rate })
const delivery = (quantity = "3", warehouse: "a" | "b" = "a"): StockOperation => ({ kind: "delivery", warehouse, quantity })
const cancel = (target: string): StockOperation => ({ kind: "cancel", target })
const refused = (reason: Extract<Serial["expected"], { kind: "refused" }>["reason"]): Serial["expected"] => ({ kind: "refused", reason })
const seed = serial("receipt", receipt())
const cases: ReadonlyArray<{ id: string; steps: ReadonlyArray<Step> }> = [
  { id: "receive-transfer-sell-return-correct", steps: [
    seed, serial("transfer", { kind: "transfer", from: "a", to: "b", quantity: "4.5" }),
    serial("delivery", delivery("3", "b")), serial("return", { kind: "return", delivery: "delivery", quantity: "1.25" }),
    serial("cancel-return", cancel("return")), serial("cancel-delivery", cancel("delivery")),
    serial("cancel-transfer", cancel("transfer")), serial("cancel-receipt", cancel("receipt")),
    serial("corrected-receipt", receipt("10", "2.1")),
  ] },
  { id: "layers-and-residual-depletion", steps: [
    serial("first", receipt("1.5", "1.003333")), serial("second", receipt("2", "2.005")),
    serial("partial", delivery("0.5")), serial("cross-layer", delivery("1.5")), serial("deplete", delivery("1.5")),
  ] },
  { id: "partial-returns-capped", steps: [
    seed, serial("delivery", delivery()),
    serial("first-return", { kind: "return", delivery: "delivery", quantity: "1.1" }),
    serial("over-return", { kind: "return", delivery: "delivery", quantity: "2" }, refused("over-return")),
    serial("final-return", { kind: "return", delivery: "delivery", quantity: "1.9" }),
  ] },
  { id: "reconcile-quantity-and-value", steps: [
    seed, serial("count", { kind: "reconcile", warehouse: "a", quantity: "8.5", rate: "2.5" }),
    serial("revalue", { kind: "reconcile", warehouse: "a", quantity: "8.5", rate: "3" }),
    serial("empty", { kind: "reconcile", warehouse: "a", quantity: "0", rate: "0" }),
  ] },
  { id: "duplicate-and-conflict", steps: [
    seed, serial("receipt", receipt(), { kind: "replayed" }),
    serial("receipt", receipt("11"), refused("idempotency-conflict")),
    serial("empty-stock", delivery("11"), refused("insufficient-stock")),
  ] },
  { id: "dependent-and-double-cancel", steps: [
    seed, serial("delivery", delivery()), serial("blocked", cancel("receipt"), refused("live-dependents")),
    serial("cancel-delivery", cancel("delivery")), serial("cancel-again", cancel("delivery"), refused("already-cancelled")),
    serial("cancel-receipt", cancel("receipt")),
  ] },
  { id: "historical-backdating", steps: [
    seed, serial("old", receipt("1"), refused("backdated"), "2026-01-01T12:00:00Z"),
    serial("tie", receipt("1")),
  ] },
  { id: "concurrent-oversell", steps: [seed, {
    kind: "concurrent", commands: [command("sell-a", delivery("7")), command("sell-b", delivery("7"))], expected: "one-winner",
  }] },
  { id: "concurrent-duplicate", steps: [{
    kind: "concurrent", commands: [command("receipt", receipt()), command("receipt", receipt())], expected: "deduplicated",
  }] },
]

/** A fresh empty company/tenant and stock accounts a/b are required per trace.
 * These are corpus inputs, not results from either engine. */
export const stockTraces: ReadonlyArray<StockTrace> = (["fifo", "moving-average"] satisfies StockTrace["method"][])
  .flatMap(method => cases.map(({ id, steps }) => ({
    format: "norn.stock-trace/1", id: `${id}-${method}`, company: "demo-company", currency: "USD",
    item: "demo-item", uom: "unit", method, quantityScale: 6, valueScale: 6, glScale: 2, rounding: "half-up", steps,
  })))
