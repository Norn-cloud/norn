import { Schema } from "effect"

const Alias = Schema.String.check(Schema.isPattern(/^[a-z][a-z0-9-]{0,63}$/))
const Decimal = Schema.String.check(Schema.isPattern(/^(0|[1-9][0-9]{0,11})(\.[0-9]{1,6})?$/))
const Positive = Decimal.check(Schema.isPattern(/[1-9]/))
const Warehouse = Schema.Literals(["a", "b"])
const Timestamp = Schema.String.check(Schema.isPattern(/^20[0-9]{2}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$/))
export const StockOperation = Schema.Union([
  Schema.Struct({ kind: Schema.Literal("receipt"), warehouse: Warehouse, quantity: Positive, rate: Decimal }),
  Schema.Struct({ kind: Schema.Literal("transfer"), from: Warehouse, to: Warehouse, quantity: Positive }),
  Schema.Struct({ kind: Schema.Literal("delivery"), warehouse: Warehouse, quantity: Positive }),
  Schema.Struct({ kind: Schema.Literal("return"), delivery: Alias, quantity: Positive }),
  Schema.Struct({ kind: Schema.Literal("reconcile"), warehouse: Warehouse, quantity: Decimal, rate: Decimal }),
  Schema.Struct({ kind: Schema.Literal("cancel"), target: Alias }),
])
export const StockCommand = Schema.Struct({ requestId: Alias, effectiveAt: Timestamp, operation: StockOperation })
const Refusal = Schema.Literals([
  "insufficient-stock", "over-return", "live-dependents", "already-cancelled", "backdated", "idempotency-conflict",
])
const Expected = Schema.Union([
  Schema.Struct({ kind: Schema.Literal("accepted") }),
  Schema.Struct({ kind: Schema.Literal("replayed") }),
  Schema.Struct({ kind: Schema.Literal("refused"), reason: Refusal }),
])
export const StockTrace = Schema.Struct({
  format: Schema.Literal("norn.stock-trace/1"), id: Alias,
  company: Schema.Literal("demo-company"), currency: Schema.Literal("USD"),
  item: Schema.Literal("demo-item"), uom: Schema.Literal("unit"),
  method: Schema.Literals(["fifo", "moving-average"]),
  quantityScale: Schema.Literal(6), valueScale: Schema.Literal(6), glScale: Schema.Literal(2),
  rounding: Schema.Literal("half-up"),
  steps: Schema.Array(Schema.Union([
    Schema.Struct({ kind: Schema.Literal("serial"), command: StockCommand, expected: Expected }),
    Schema.Struct({
      kind: Schema.Literal("concurrent"), commands: Schema.Array(StockCommand).check(Schema.isMinLength(2)),
      expected: Schema.Literals(["one-winner", "deduplicated"]),
    }),
  ])).check(Schema.isMinLength(1)),
})
export type StockOperation = typeof StockOperation.Type
export type StockCommand = typeof StockCommand.Type
export type StockTrace = typeof StockTrace.Type

/** Parse untrusted JSON before either driver runs. Expectations describe Norn;
 * oracle refusals/differences must be retained and triaged, never filtered out. */
export function parseStockTrace(input: unknown): StockTrace {
  const trace = Schema.decodeUnknownSync(StockTrace)(input, { onExcessProperty: "error" })
  const prior = new Map<string, StockOperation>()
  for (const step of trace.steps) {
    const commands = step.kind === "serial" ? [step.command] : step.commands
    for (const command of commands) {
      const time = new Date(command.effectiveAt)
      if (!Number.isFinite(time.valueOf()) || time.toISOString() !== command.effectiveAt.replace("Z", ".000Z")) {
        throw new Error(`Invalid effective time: ${command.effectiveAt}`)
      }
      const op = command.operation
      if (op.kind === "transfer" && op.from === op.to) throw new Error("Transfer needs distinct warehouses")
      if (op.kind === "return" && prior.get(op.delivery)?.kind !== "delivery") throw new Error("Return needs a prior delivery")
      if (op.kind === "cancel" && (!prior.has(op.target) || prior.get(op.target)?.kind === "cancel")) {
        throw new Error("Cancellation needs a prior non-cancel request")
      }
    }
    // Barrier members cannot reference each other. First payload owns an ID;
    // conflicting later payloads remain inputs for the idempotency-refusal case.
    for (const command of commands) if (!prior.has(command.requestId)) prior.set(command.requestId, command.operation)
  }
  return trace
}
