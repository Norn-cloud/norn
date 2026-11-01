import { expect, test } from "bun:test"
import { stockTraces } from "../fixtures/stock-parity/scenarios.ts"
import { parseStockTrace } from "../src/parity/stock-trace.ts"

const base = stockTraces.find(trace => trace.id === "duplicate-and-conflict-fifo")
if (!base) throw new Error("Missing duplicate fixture")
const header = { ...base, steps: [] }
const receipt = { kind: "receipt", warehouse: "a", quantity: "1.5", rate: "2.005" }
const command = { requestId: "receipt", effectiveAt: "2026-01-02T12:00:00Z", operation: receipt }
const serial = { kind: "serial", command, expected: { kind: "accepted" } }

test("every synthetic trace round-trips through the shared wire boundary", () => {
  expect(stockTraces).toHaveLength(18)
  expect(new Set(stockTraces.map(trace => trace.id)).size).toBe(18)
  for (const trace of stockTraces) expect(parseStockTrace(JSON.parse(JSON.stringify(trace)))).toEqual(trace)
})

test("rejects lossy numbers, excess precision, invalid dates and undeclared fields", () => {
  for (const quantity of [1.5, "1e2", "-1", "0", "0.0000000", "1000000000000", "1.0000001"]) {
    expect(() => parseStockTrace({ ...header, steps: [{ ...serial, command: { ...command, operation: { ...receipt, quantity } } }] })).toThrow()
  }
  for (const effectiveAt of ["2026-02-30T12:00:00Z", "2026-01-02", "2026-01-02T25:00:00Z"]) {
    expect(() => parseStockTrace({ ...header, steps: [{ ...serial, command: { ...command, effectiveAt } }] })).toThrow()
  }
  expect(() => parseStockTrace({ ...base, postingHook: "custom" })).toThrow()
  expect(() => parseStockTrace({ ...header, steps: [{ ...serial, command: { ...command, operation: { ...receipt, rate: 2.005 } } }] })).toThrow()
})

test("requires prior references, distinct warehouses and real barriers", () => {
  for (const operation of [
    { kind: "return", delivery: "missing", quantity: "1" }, { kind: "cancel", target: "missing" },
    { kind: "transfer", from: "a", to: "a", quantity: "1" }, { kind: "posting-hook" },
  ]) expect(() => parseStockTrace({ ...header, steps: [{ ...serial, command: { ...command, operation } }] })).toThrow()
  expect(() => parseStockTrace({ ...header, steps: [] })).toThrow()
  expect(() => parseStockTrace({ ...header, steps: [{ kind: "concurrent", commands: [command], expected: "deduplicated" }] })).toThrow()
})

test("retains conflicting retries and unordered contenders rather than normalizing them", () => {
  const parsed = parseStockTrace(JSON.parse(JSON.stringify(base)))
  expect(parsed.steps.slice(0, 3).map(step => step.kind === "serial" && step.command.requestId)).toEqual(["receipt", "receipt", "receipt"])
  const race = stockTraces.find(trace => trace.id === "concurrent-oversell-fifo")
  const barrier = parseStockTrace(race).steps.at(-1)
  expect(barrier?.kind).toBe("concurrent")
  if (barrier?.kind === "concurrent") expect(barrier.commands.map(value => value.requestId)).toEqual(["sell-a", "sell-b"])
})
