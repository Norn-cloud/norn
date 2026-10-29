/**
 * The closure-free flow data builder (T7-FLOW-1, #2649).
 *
 * Run: `bun --cwd tools/norn-ts test flow-builder`. With
 * `NORN_CHECKER_WASM_PKG` set to a built `norn-checker-wasm` package (CI job
 * `norn-ts-authoring`), every golden artifact set must be admitted by the
 * Rust checker; in CI the package is required.
 * Refresh goldens after an intentional change: `UPDATE_GOLDENS=1 bun test flow-builder`.
 */
import { describe, expect, test } from "bun:test"
import { existsSync, readFileSync, readdirSync, writeFileSync } from "node:fs"
import { join } from "node:path"
import { X, flow, lowerFlow, perform, retry, succeed } from "../src/builder/index.ts"
import type { FlowDef } from "../src/builder/index.ts"
import { NonJsonValue, canonicalJson, toJson } from "../src/canonical.ts"
import { TOOL_ROOT } from "../src/profile.ts"
import { chargeCard } from "../fixtures/flows/support/billing.builder.ts"
import { CHECKER_PKG_ENV, checker } from "./checker.ts"

if (!checker && process.env.CI) throw new Error(`${CHECKER_PKG_ENV} must point at a built norn-checker-wasm package in CI`)
const FLOWS = join(TOOL_ROOT, "fixtures", "flows")
const UPDATE = process.env.UPDATE_GOLDENS === "1"
const SCOPE = { orgId: "org_demo", environmentId: "dev" } as const

const options = (name: string) => ({ scope: SCOPE, sourcePath: `tools/norn-ts/fixtures/flows/${name}.flow.ts` })

async function definition(path: string): Promise<FlowDef> {
  return ((await import(path)) as { default: FlowDef }).default
}

function golden(file: string, actual: string): void {
  if (UPDATE) writeFileSync(file, actual)
  expect(existsSync(file)).toBe(true)
  expect(actual).toBe(readFileSync(file, "utf8"))
}

const VALID = ["sequence", "route", "typed-failure", "retry-timer", "terminal-fact"]
const INVALID: Record<string, string> = {
  "undeclared-tag": "NORN-FLOW-TAG",
  "unbounded-retry": "NORN-FLOW-UNBOUNDED",
  "over-bound-retry": "NORN-FLOW-UNBOUNDED",
  "undeclared-read": "NORN-FLOW-SLICE",
  "wrong-input": "NORN-FLOW-VALUE",
  "no-result": "NORN-FLOW-RESULT",
  "duplicate-step": "NORN-FLOW-UNSUPPORTED-SYNTAX",
  "reused-step-object": "NORN-FLOW-UNSUPPORTED-SYNTAX",
}

// Definitions are data: importing a fixture builds it, nothing runs.
const valid = new Map<string, FlowDef>()
for (const name of VALID) valid.set(name, await definition(join(FLOWS, `${name}.builder.ts`)))
const invalid = new Map<string, FlowDef>()
for (const name of Object.keys(INVALID)) invalid.set(name, await definition(join(FLOWS, "invalid", `${name}.builder.ts`)))

function build(name: string) {
  const built = lowerFlow(valid.get(name)!, options(name))
  if (!built.ok) throw new Error(`${name}: ${JSON.stringify(built.diagnostics)}`)
  return built
}

interface RuleRow {
  readonly _id: string
  readonly c_when: string
  readonly o_commands: string
  readonly o_facts: string
  readonly o_timers: string
}

function decisionRows(artifacts: ReadonlyArray<unknown>): ReadonlyArray<RuleRow> {
  const rule = artifacts[1] as {
    payload: {
      graph: {
        nodes: ReadonlyArray<{ type: string; content?: { rules?: ReadonlyArray<RuleRow> } }>
      }
    }
  }
  return rule.payload.graph.nodes.find((node) => node.type === "decisionTableNode")?.content?.rules ?? []
}

describe("valid flows", () => {
  for (const name of VALID) {
    test(`${name}: canonical golden JSON (Run type + decide rule)`, () => {
      golden(join(FLOWS, `${name}.ir.json`), canonicalJson(build(name).artifacts))
    })

    test(`${name}: source map golden; every step entry points at its builder call site`, () => {
      const built = build(name)
      golden(join(FLOWS, `${name}.sourcemap.json`), canonicalJson(built.sourceMap))
      const map = built.sourceMap as unknown as { steps: Record<string, { file: string; line: number } | null> }
      const lines = readFileSync(join(FLOWS, `${name}.builder.ts`), "utf8").split("\n")
      expect(Object.keys(map.steps).length).toBeGreaterThan(0)
      for (const [step, at] of Object.entries(map.steps)) {
        expect({ step, file: at?.file }).toEqual({ step, file: `fixtures/flows/${name}.builder.ts` })
        expect({ step, line: lines[(at?.line ?? 0) - 1]?.includes(`"${step}"`) }).toEqual({ step, line: true })
      }
    })

    test(`${name}: repeated builds are byte-identical`, () => {
      const first = build(name)
      const second = build(name)
      expect(canonicalJson(second.artifacts)).toBe(canonicalJson(first.artifacts))
      expect(canonicalJson(second.sourceMap)).toBe(canonicalJson(first.sourceMap))
    })

    test.skipIf(!checker)(`${name}: the Rust checker admits the artifact set (norn-checker-wasm)`, () => {
      const result = checker!.checkArtifactSet(build(name).artifacts)
      expect(result.diagnostics.diagnostics).toEqual([])
      expect(result.admitted).toBe(true)
      expect(result.receiptSha256).toMatch(/^[0-9a-f]{64}$/)
    })
  }

  test("the artifacts are closure-free data: canonical JSON round-trips them unchanged", () => {
    const built = build("sequence")
    expect(JSON.parse(canonicalJson(built.artifacts))).toEqual(toJson(built.artifacts))
  })
})

describe("builder diagnostics", () => {
  test("every invalid fixture has an expected code", () => {
    const files = readdirSync(join(FLOWS, "invalid"))
      .filter((f) => f.endsWith(".builder.ts"))
      .map((f) => f.replace(/\.builder\.ts$/, ""))
      .sort()
    expect(files).toEqual(Object.keys(INVALID).sort())
  })

  for (const [name, code] of Object.entries(INVALID)) {
    test(`${name} fails with ${code} at its builder call site`, () => {
      const built = lowerFlow(invalid.get(name)!, options(name))
      expect(built.ok).toBe(false)
      if (built.ok) return
      expect(built.diagnostics.map((d) => d.code)).toEqual([code])
      expect(built.diagnostics[0]?.at?.file).toBe(`fixtures/flows/invalid/${name}.builder.ts`)
    })
  }
})

describe("event correlation", () => {
  function retryRows(): ReadonlyArray<RuleRow> {
    return decisionRows(build("retry-timer").artifacts)
  }

  test("each retry receipt and timer is fenced to its expected run, step, and attempt", () => {
    const rows = retryRows()
    for (const attempt of [1, 2, 3, 4]) {
      const result = rows.find((row) => row._id === `awaiting_retry_charge_attempt_${attempt}/result/1`)
      expect(result?.c_when).toContain(`event.attempt == ${attempt}`)
      expect(result?.c_when).toContain("event.runId == slice.run.run_id")
      expect(result?.c_when).toContain("event.step == 'charge'")

      const failures = rows.filter((row) => row._id.startsWith(`awaiting_retry_charge_attempt_${attempt}/failure/`))
      expect(failures.length).toBe(2)
      for (const row of failures) {
        expect(row.c_when).toContain(`event.attempt == ${attempt}`)
        expect(row.c_when).toContain("event.runId == slice.run.run_id")
        expect(row.c_when).toContain("event.step == 'charge'")
      }
    }

    const sleep = rows.find((row) => row._id === "sleeping_settle/timer/1")
    expect(sleep?.c_when).toContain("event.runId == slice.run.run_id")
    expect(sleep?.c_when).toContain("event.step == 'settle'")
    expect(sleep?.c_when).toContain("event.attempt == 0")
    expect(sleep?.c_when).toContain("event.timerId == slice.run.run_id + ':settle'")

    for (const attempt of [2, 3, 4]) {
      const timer = rows.find((row) => row._id === `retrying_charge_attempt_${attempt}/timer/1`)
      expect(timer?.c_when).toContain(`event.attempt == ${attempt}`)
      expect(timer?.c_when).toContain("event.runId == slice.run.run_id")
      expect(timer?.c_when).toContain("event.step == 'charge'")
      expect(timer?.c_when).toContain(`event.timerId == slice.run.run_id + ':charge:attempt:${attempt}'`)
      expect(timer?.o_commands).toContain(`attempt: ${attempt}`)
    }
    expect(rows.find((row) => row._id === "awaiting_retry_charge_attempt_4/failure/1")?.o_timers).toBe("[]")
  })

  test("one-shot result and failure receipts are fenced to their run and attempt", () => {
    const receipts = decisionRows(build("sequence").artifacts).filter((row) =>
      row.c_when.includes("event.kind == 'result'") || row.c_when.includes("event.kind == 'failure'"),
    )
    expect(receipts.length).toBeGreaterThan(0)
    for (const row of receipts) {
      expect(row.c_when).toContain("event.runId == slice.run.run_id")
      expect(row.c_when).toContain("event.step ==")
      expect(row.c_when).toContain("event.attempt == 1")
    }
  })

  test("one-shot and retry state names stay distinct in either step ordering", () => {
    const invoice = () => X.record({ invoiceId: X.lit("inv-1") })
    const retried = retry(perform("charge", chargeCard, invoice()), {
      recurs: 1,
      delay: { kind: "spaced", millis: 1000 },
    })
    const oneShot = perform("charge_attempt_1", chargeCard, invoice())

    for (const [name, body] of [
      ["retry_first", [retried, oneShot]],
      ["one_shot_first", [oneShot, retried]],
    ] as const) {
      const def = flow({ id: `demo.${name}`, version: "1.0.0" }, [...body, succeed(X.record({ done: X.lit(true) }))])
      const built = lowerFlow(def, options(name))
      expect(built.ok).toBe(true)
      if (!built.ok) continue
      const rows = decisionRows(built.artifacts)
      expect(rows.some((row) => row._id.startsWith("awaiting_retry_charge_attempt_1/result/"))).toBe(true)
      expect(rows.some((row) => row._id.startsWith("awaiting_perform_charge_attempt_1/result/"))).toBe(true)
      expect(rows.some((row) => row.o_facts.includes("transition: 'to_completed'"))).toBe(true)
    }
  })
})

describe("canonical JSON", () => {
  test("rejects functions, class instances, non-finite numbers, and cycles", () => {
    const cyclic: Record<string, unknown> = {}
    cyclic.self = cyclic
    for (const bad of [{ f: () => 1 }, { d: new Date(0) }, { n: Number.NaN }, cyclic, [undefined]]) {
      expect(() => toJson(bad)).toThrow(NonJsonValue)
    }
  })

  test("sorts keys, so insertion order never changes the bytes", () => {
    expect(canonicalJson({ b: 1, a: { d: 2, c: 3 } })).toBe(canonicalJson({ a: { c: 3, d: 2 }, b: 1 }))
  })
})
