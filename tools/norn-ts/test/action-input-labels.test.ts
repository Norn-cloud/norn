/**
 * D66: an action's input carries display-only labels and wizard groups into
 * its lowered `inputSchema`, unchanged; the Rust checker owns every rule
 * over them. The schema's type and required fields are untouched.
 */
import { describe, expect, test } from "bun:test"
import { action, lowerAction, upsertInputObject } from "../src/builder/action.ts"
import { record, text } from "../src/builder/schema.ts"

const options = { scope: { orgId: "org_demo", environmentId: "dev" }, sourcePath: "tools/norn-ts/test/action-input-labels.test.ts" }
const policy = { name: "canon.ValuationPolicy", version: "0.1.0" }
const input = record({ id: text(), valuation_method: text(["fifo", "moving_average"]) })

const lowered = (extra: Partial<Parameters<typeof action>[0]>) => {
  const built = lowerAction(
    action({
      id: "canon.valuation_policy.set", title: "Set stock valuation policy", version: "0.1.0",
      scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
      input, output: record({ id: text() }),
      writes: [{ entityKind: policy.name, change: "modified" }],
      effect: upsertInputObject(policy, "id"),
      ...extra,
    }),
    options,
  )
  if (!built.ok) throw new Error(JSON.stringify(built.diagnostics))
  return (built.artifact as { payload: { inputSchema: Record<string, unknown> } }).payload.inputSchema
}

describe("action input labels and groups", () => {
  test("labels land on their fields and groups on the root", () => {
    const schema = lowered({
      inputLabels: { valuation_method: { title: "Valuation method", description: "How stock is valued." } },
      inputGroups: [{ id: "valuation", title: "Valuation", fields: ["/valuation_method"] }],
    })
    expect(schema.properties).toEqual({
      id: { type: "string" },
      valuation_method: { type: "string", enum: ["fifo", "moving_average"], title: "Valuation method", description: "How stock is valued." },
    })
    expect(schema["x-norn"]).toEqual({ groups: [{ id: "valuation", title: "Valuation", fields: ["/valuation_method"] }] })
    expect(schema.required).toEqual(["id", "valuation_method"])
  })

  test("without labels or groups the input is emitted as authored", () => {
    expect<unknown>(lowered({})).toEqual(input)
  })
})

describe("flow display data", () => {
  test("lowerFlow reports the flow title and each labelled step, and names its artifacts by the title", async () => {
    const { X, flow, fn, lowerFlow, perform, route, sleep, succeed, seconds } = await import("../src/builder/index.ts")
    const notify = fn({ id: "norn.decision", version: "1", input: record({ text: text() }), output: record({ ok: text() }) })
    const def = flow({ id: "studio.flow.approve", version: "1.0.0", title: "Approve purchase" }, [
      perform("ask", notify, X.record({ text: X.lit("approve?") }), { label: "Ask the approver" }),
      route([{ when: X.lit(true), then: [sleep("wait", seconds(5), { label: "Wait a moment" })] }]),
      perform("unlabelled", notify, X.record({ text: X.lit("x") })),
      succeed(X.record({ done: X.lit(true) })),
    ])
    const built = lowerFlow(def, { scope: { orgId: "org_demo", environmentId: "dev" }, sourcePath: "test" })
    if (!built.ok) throw new Error(JSON.stringify(built.diagnostics))
    expect(built.flow).toEqual({
      id: "studio.flow.approve", version: "1.0.0", title: "Approve purchase",
      steps: [{ id: "ask", label: "Ask the approver" }, { id: "wait", label: "Wait a moment" }],
    })
    expect((built.artifacts as Array<{ name: string }>).map((artifact) => artifact.name)).toEqual(["Approve purchase run", "Approve purchase"])
  })
})
