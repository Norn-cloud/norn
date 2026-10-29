/**
 * T7-TS-3 (#2691): RuleIR v2 builders and the jdm-editor round trip.
 *
 * editor → JSON → TS → JSON: a decision as `jdm-editor` exports it (RuleIR JSON
 * with canvas positions) is printed as a `Norn.decision` module
 * (`src/printer.ts`), compiled by the syntax compiler, and must reproduce the
 * artifact's canonical bytes, with ZEN Expression cells untouched; the
 * checker admits it with the same diagnostics as the original.
 *
 * Run: `NORN_CHECKER_WASM_PKG=/path/to/pkg bun --cwd tools/norn-ts test decisions`.
 */
import { describe, expect, test } from "bun:test"
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs"
import { join } from "node:path"
import { R, S } from "../src/builder/index.ts"
import { canonicalJson } from "../src/canonical.ts"
import { compileDefinitions } from "../src/compiler/definitions.ts"
import { checkPack, compilePack } from "../src/pack.ts"
import { UnprintableArtifact, printDecision } from "../src/printer.ts"
import { TOOL_ROOT } from "../src/profile.ts"
import { CHECKER_PKG_ENV, checker } from "./checker.ts"

if (!checker && process.env.CI) throw new Error(`${CHECKER_PKG_ENV} is required in CI`)
const PACK = join(TOOL_ROOT, "fixtures/decisions/base")
const read = (path: string): Record<string, unknown> => JSON.parse(readFileSync(path, "utf8")) as Record<string, unknown>
/** The entity definitions the sales-order rules read (checker context for admission). */
const entities = ["commerce-order.interface.json", "commerce-customer.entity.json", "commerce-sales-order.entity.json", "work-task.entity.json"].map((f) => read(join(PACK, f)))

const editorExports = [
  join(TOOL_ROOT, "../../fixtures/rule-ir-v2/valid/credit-hold.rule.json"),
  join(PACK, "commerce-sales-order-credit-call.rule.json"),
  join(TOOL_ROOT, "fixtures/decisions/credit-hold.editor.rule.json"),
]

function roundTrip(artifact: Record<string, unknown>): { source: string; compiled: unknown } {
  const source = printDecision(artifact)
  const dir = mkdtempSync(join(TOOL_ROOT, "fixtures/decisions/printed-"))
  try {
    const file = join(dir, "decision.ts")
    writeFileSync(file, source)
    const provenance = artifact.provenance as { sourcePath: string }
    const result = compileDefinitions(file, { scope: artifact.scope as Record<string, string>, sourcePath: provenance.sourcePath })
    if (!result.ok) throw new Error(`${JSON.stringify(result.diagnostics, null, 2)}\n${source}`)
    expect(result.artifacts.length).toBe(1)
    return { source, compiled: result.artifacts[0] }
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
}

describe("jdm-editor round trip: editor -> JSON -> TS -> JSON", () => {
  for (const path of editorExports) {
    test(path.split("/").pop()!, () => {
      const original = read(path)
      const { source, compiled } = roundTrip(original)
      expect(canonicalJson(compiled)).toBe(canonicalJson(original))
      // Printing the compiled artifact again gives the same module: the printer is a fixed point.
      expect(printDecision(compiled)).toBe(source)
      if (checker) {
        const before = checker.checkArtifactSet([...entities, original])
        const after = checker.checkArtifactSet([...entities, compiled])
        expect(after.diagnostics).toEqual(before.diagnostics)
        expect(after.admitted).toBe(true)
      }
    })
  }

  test("nullable decision records round trip and admit without widening other profiles", () => {
    const artifact = read(editorExports[2]!)
    const payload = artifact.payload as { triggers?: unknown; slice?: unknown; inputSchema: { properties: { slice: { properties: Record<string, unknown> } } } }
    // An offline comparison contract has no hosted slice declarations.
    delete payload.triggers
    delete payload.slice
    payload.inputSchema.properties.slice.properties.note = { type: ["object", "null"], properties: { text: { type: ["string", "null"] } }, required: ["text"], additionalProperties: false }
    const { source, compiled } = roundTrip(artifact)
    expect(source).toContain("Schema.NullOr(Schema.Struct")
    expect(canonicalJson(compiled)).toBe(canonicalJson(artifact))
    expect(printDecision(compiled)).toBe(source)
    if (checker) {
      const checked = checker.checkArtifactSet([...entities, compiled])
      expect(checked.diagnostics.diagnostics).toEqual([])
      expect(checked.admitted).toBe(true)
    }
  })

  test("an imported editor rule with inline layout passes checker admission", () => {
    if (!checker) return
    const dir = mkdtempSync(join(TOOL_ROOT, "fixtures/decisions/imported-layout-"))
    try {
      const artifact = read(editorExports[2]!)
      const payload = artifact.payload as {
        graph: {
          viewport?: unknown
          nodes: Array<Record<string, unknown>>
          edges: Array<Record<string, unknown>>
        }
      }
      payload.graph.viewport = { x: 12.5, y: -4, zoom: 1.25 }
      payload.graph.nodes[0]!.description = "editor-only node description"
      payload.graph.edges[0]!.targetHandle = "editor-target"
      const sources = [...entities, artifact]
      sources.forEach((source, index) => {
        writeFileSync(join(dir, `artifact-${index}.json`), JSON.stringify(source))
      })

      const pack = compilePack(dir, {
        scope: { orgId: "org_demo", environmentId: "dev" },
        imports: [dir],
        root: join(TOOL_ROOT, "..", ".."),
      })
      if (!pack.ok) throw new Error(JSON.stringify(pack.failures, null, 2))
      expect(pack.artifacts).toHaveLength(sources.length)
      expect(pack.artifacts.map((candidate) => (candidate as { artifactId: string }).artifactId))
        .toContain((artifact as { artifactId: string }).artifactId)

      const checked = checkPack(checker, pack.artifacts, pack.baseArtifacts)
      expect(checked.diagnostics.diagnostics).toEqual([])
      expect(checked.admitted).toBe(true)
    } finally {
      rmSync(dir, { recursive: true, force: true })
    }
  })

  test("ZEN Expression cells survive verbatim", () => {
    const original = read(join(TOOL_ROOT, "../../fixtures/rule-ir-v2/valid/credit-hold.rule.json"))
    const { source } = roundTrip(original)
    expect(source).toContain(JSON.stringify("slice.order.total + slice.customer.exposure > slice.customer.credit_limit"))
  })

  test("artifacts outside the authored form are refused, not approximated", () => {
    const original = read(join(TOOL_ROOT, "../../fixtures/rule-ir-v2/valid/credit-hold.rule.json"))
    const payload = original.payload as Record<string, unknown>
    expect(() => printDecision({ ...original, lifecycle: "active" })).toThrow(UnprintableArtifact)
    expect(() => printDecision({ ...original, payload: { ...payload, inputSchema: { type: "object", properties: {} } } })).toThrow(UnprintableArtifact)
    expect(() => printDecision({ ...original, payload: { ...payload, defaults: {} } })).toThrow(UnprintableArtifact)
    // Explicit empty lists the builder omits.
    expect(() => printDecision({ ...original, payload: { ...payload, triggers: [] } })).toThrow(UnprintableArtifact)
    expect(() => printDecision({ ...original, payload: { ...payload, slice: [] } })).toThrow(UnprintableArtifact)
    // Provenance the build options cannot restore.
    expect(() => printDecision({ ...original, provenance: { source: "import", sourcePath: "x" } })).toThrow(UnprintableArtifact)
    expect(() => printDecision({ ...original, provenance: { source: "authoring_sdk", sourcePath: "x", actorId: "a" } })).toThrow(UnprintableArtifact)
    // Schema representations the projection cannot reproduce exactly.
    const closed = (schema: unknown) => ({ ...original, payload: { ...payload, inputSchema: schema } })
    expect(() => printDecision(closed({ type: "object", properties: {}, required: [], additionalProperties: false }))).toThrow(UnprintableArtifact)
    expect(() => printDecision(closed({ type: "object", properties: { "10": { type: "string" }, "2": { type: "string" } }, required: ["10", "2"], additionalProperties: false }))).toThrow(UnprintableArtifact)
    expect(() => printDecision(closed(JSON.parse('{"type":"object","properties":{"__proto__":{"type":"string"}},"additionalProperties":false}')))).toThrow(UnprintableArtifact)
  })

  test("a JDM member named __proto__ is refused, not dropped", () => {
    const original = read(join(TOOL_ROOT, "../../fixtures/rule-ir-v2/valid/credit-hold.rule.json"))
    const text = JSON.stringify(original).replace('"c_over":"slice.order.total', '"__proto__":"x","c_over":"slice.order.total')
    expect(() => printDecision(JSON.parse(text))).toThrow(UnprintableArtifact)
  })
})

describe("RuleIR v2 builder", () => {
  test("the hand-written builder produces the credit-call rule", () => {
    const original = read(join(PACK, "commerce-sales-order-credit-call.rule.json"))
    const payload = original.payload as { graph: R.JdmGraph }
    const order = S.record({ so_number: S.text(), status: S.text(), customer: S.text(), total: S.integer })
    const event = S.record({
      op: S.text(),
      key: S.record({ objectType: S.text(), key: S.text() }),
      lifecycle: S.text(),
      transition: S.text(),
      from: S.text(),
      to: S.text(),
    })
    const timers = S.array(S.record({ afterMillis: S.integer, timerId: S.text(), kind: S.text(), order: S.text() }))
    const def = R.decision({
      id: "commerce.rule.sales_order_credit_call",
      artifactId: "commerce.sales_order.credit_call.rule",
      title: "Sales order credit call task",
      version: "0.1.0",
      contract: "decide",
      triggers: [{ on: "transitioned", objectType: "commerce.SalesOrder", transition: "hold" }],
      slice: [{ name: "order", objectType: "commerce.SalesOrder", key: "triggerObject" }],
      input: S.record({ slice: S.record({ order }), event }),
      output: S.record({
        facts: S.array(
          S.record(
            {
              op: S.text(["create"]),
              objectType: S.text(["work.Task"]),
              key: S.text(),
              values: S.record({ task_id: S.text(), kind: S.text(), subject: S.text(), state: S.text() }),
            },
            ["values"],
          ),
        ),
        commands: S.array(S.record({ kind: S.text() })),
        timers,
      }),
      scopeRequired: ["orgId", "environmentId"],
      graph: payload.graph,
    })
    const built = R.lowerDecision(def, { scope: { orgId: "org_demo", environmentId: "dev" }, sourcePath: "fixtures/dev/sales-order/commerce-sales-order-credit-call.rule.json" })
    if (!built.ok) throw new Error(JSON.stringify(built.diagnostics))
    expect(canonicalJson(built.artifact)).toBe(canonicalJson(original))
  })
})
