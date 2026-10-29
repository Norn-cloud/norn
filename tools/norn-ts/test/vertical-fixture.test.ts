/**
 * T7-EFX-3 (#2786): the vertical flow fixture. One reviewed flow
 * (`crm.route_message`: sequence -> route -> typed failure -> finite retry
 * timer -> accepted proposal -> terminal fact) authored twice, as `Effect.gen`
 * source and with the data builder, produces identical checked JSON
 * (`fixtures/vertical/route-message.ir.json`). Its perform targets the T1-FN-1
 * Function of `fixtures/function` (the WASM intent classifier). The golden
 * event trace `fixtures/vertical/trace.json` runs in
 * `crates/norn-decisions/tests/flow_vertical.rs`; the T2-RES-1 binding of
 * receipts runs in `crates/norn-kernel/tests/result_reconciliation.rs`.
 *
 * Run: `NORN_CHECKER_WASM_PKG=/path/to/pkg bun --cwd tools/norn-ts test vertical-fixture`
 * (`UPDATE_GOLDENS=1` refreshes the golden; review the diff).
 */
import { describe, expect, test } from "bun:test"
import { existsSync, readFileSync, readdirSync, writeFileSync } from "node:fs"
import { join } from "node:path"
import type { FlowDef } from "../src/builder/index.ts"
import { lowerFlow } from "../src/builder/index.ts"
import { canonicalJson } from "../src/canonical.ts"
import { compileFile } from "../src/compiler/index.ts"
import { TOOL_ROOT } from "../src/profile.ts"
import { CHECKER_PKG_ENV, checker } from "./checker.ts"

if (!checker && process.env.CI) throw new Error(`${CHECKER_PKG_ENV} is required in CI`)
const UPDATE = process.env.UPDATE_GOLDENS === "1"
const DIR = join(TOOL_ROOT, "fixtures", "vertical")
const REPO = join(TOOL_ROOT, "..", "..")
const options = { scope: { orgId: "tenant_acme", environmentId: "main" }, sourcePath: "tools/norn-ts/fixtures/vertical/route-message.source.ts" }

type Artifact = { kind: string; payload: { inputSchema?: { properties: { event: { properties: Record<string, { enum?: string[] }> } } }; graph?: unknown } }

const compiled = compileFile(join(DIR, "route-message.source.ts"), options)
if (!compiled.ok) throw new Error(JSON.stringify(compiled.diagnostics, null, 2))
const json = canonicalJson(compiled.artifacts)

describe("vertical flow: builder == compiler == golden", () => {
  test("the Effect.gen source and the data builder lower to identical canonical JSON", async () => {
    const twin = ((await import(join(DIR, "route-message.builder.ts"))) as { default: FlowDef }).default
    const built = lowerFlow(twin, options)
    if (!built.ok) throw new Error(JSON.stringify(built.diagnostics, null, 2))
    expect(json).toBe(canonicalJson(built.artifacts))
  })

  test("matches the golden route-message.ir.json", () => {
    const golden = join(DIR, "route-message.ir.json")
    if (UPDATE) writeFileSync(golden, json)
    if (!existsSync(golden)) console.log(`---- route-message.ir.json ----\n${json}---- end ----`)
    expect(existsSync(golden)).toBe(true)
    expect(json).toBe(readFileSync(golden, "utf8"))
  })

  test("the Rust checker admits it", () => {
    if (!checker) return
    const admitted = checker.checkArtifactSet(compiled.artifacts)
    expect(admitted.diagnostics.diagnostics).toEqual([])
    expect(admitted.admitted).toBe(true)
  })

  test("every perform targets the T1-FN-1 manifest with the invocation's inputs", () => {
    const manifest = JSON.parse(readFileSync(join(REPO, "fixtures/function/manifest.wasm.json"), "utf8")) as { functionId: string; version: number }
    const invocation = JSON.parse(readFileSync(join(REPO, "fixtures/function/invocation.wasm.json"), "utf8")) as { inputs: Record<string, string> }
    const graph = JSON.stringify((compiled.artifacts as Artifact[]).find((a) => a.kind === "rule")!.payload.graph)
    expect(graph).toContain(`function: '${manifest.functionId}', functionVersion: '${manifest.version}'`)
    for (const [name, value] of Object.entries(invocation.inputs)) expect(graph).toContain(`${name}: ${JSON.stringify(value).replace(/^"|"$/g, "'")}`)
  })

  test("the golden trace's events are events of this rule", () => {
    const trace = JSON.parse(readFileSync(join(DIR, "trace.json"), "utf8")) as { runs: Array<{ steps: Array<{ event?: { kind: string; step?: string } }> }> }
    const event = (compiled.artifacts as Artifact[]).find((a) => a.kind === "rule")!.payload.inputSchema!.properties.event.properties
    for (const step of trace.runs.flatMap((r) => r.steps)) {
      if (!step.event) continue
      expect(event.kind!.enum).toContain(step.event.kind)
      if (step.event.step) expect(event.step!.enum).toContain(step.event.step)
    }
    const labels = trace.runs.flatMap((r) => r.steps.map((s) => JSON.stringify(s)))
    expect(labels.some((s) => s.includes("stale"))).toBe(true)
    expect(labels.some((s) => s.includes("duplicate"))).toBe(true)
  })
})

describe("source-mapped negative fixtures", () => {
  const dir = join(DIR, "invalid")
  for (const file of readdirSync(dir).filter((f) => f.endsWith(".ts")).sort()) {
    test(file, () => {
      const path = join(dir, file)
      const text = readFileSync(path, "utf8")
      const header = /^\/\/ expect: (NORN-FLOW-[A-Z-]+) at line containing: (.+)$/m.exec(text)
      if (!header) throw new Error(`${file} has no expect header`)
      const result = compileFile(path, options)
      expect(result.ok).toBe(false)
      if (result.ok) return
      const hit = result.diagnostics.find((d) => d.code === header[1])
      expect(hit?.code).toBe(header[1]!)
      // The diagnostic points at the offending source line.
      expect(text.split("\n")[hit!.start.line - 1]).toContain(header[2]!)
    })
  }
})
