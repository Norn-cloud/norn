import { expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import { assignable, catchTag, flow, fn, lowerFlow, perform, S, succeed, taggedError, X } from "../src/builder/index.ts"
import { compileModule } from "../src/compile-stdin.ts"
import { compileFile } from "../src/compiler/index.ts"
import { TOOL_ROOT } from "../src/profile.ts"

const options = { scope: { orgId: "org_demo", environmentId: "dev" }, sourcePath: "module.ts" }
const corpus = (name: string) => join(TOOL_ROOT, "corpus", name + ".ts")
const source = (name: string) => readFileSync(corpus(name), "utf8")

test("TS2: seeded alpha renamings preserve the compiled artifacts and free columns", () => {
  const original = source("accept/binder-fields")
  const baseline = compileModule(original, options.scope)
  if (!("artifacts" in baseline)) throw new Error(JSON.stringify(baseline))
  expect(JSON.stringify(baseline.artifacts)).toContain("some(values as b1, all(values as b2, (b2 > b1) and (b1 > price)))")
  const names = ["price", "values", "b0", "b1", "self", "input", "actor", "item"]
  let seed = 29017
  function pick(): string {
    seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0
    return names[(seed >>> 16) % names.length]!
  }
  for (let trial = 0; trial < 8; trial++) {
    const outer = pick()
    let inner = pick()
    while (inner === outer) inner = pick()
    const renamed = original.replace("some(price => root.values.every(item => item > price && price > root.price))",
      `some(${outer} => root.values.every(${inner} => ${inner} > ${outer} && ${outer} > root.price))`)
    const compiled = compileModule(renamed, options.scope)
    if (!("artifacts" in compiled)) throw new Error(JSON.stringify(compiled))
    expect(compiled.artifacts).toEqual(baseline.artifacts)
  }
}, 60_000)

test("TS1: a break-only branch continues, and stacked labels share a return", () => {
  const result = compileFile(corpus("accept/switch-bindings"), options)
  if (!result.ok) throw new Error(JSON.stringify(result.diagnostics))
  const branch = result.definition.body[1]
  if (branch?._tag !== "Route") throw new Error("expected switch route")
  expect(branch.branches.map(b => [b.when._tag, b.then.map(s => s._tag)])).toEqual([
    ["Compare", []], ["Logical", ["Succeed"]],
  ])
  expect(branch.otherwise[0]?._tag).toBe("Succeed")
})

for (const [fixture, code, message] of [
  ["perform-input", "NORN-FLOW-TYPE", "is not assignable"],
  ["conditional-subdecision", "NORN-FLOW-EXPRESSION", "before branching"],
  ["plain-decimal", "NORN-FLOW-TYPE", "closed schemas"],
] as const) test(`reject/compiler/${fixture}`, () => {
  const result = compileModule(source("reject/compiler/" + fixture), options.scope)
  expect(result.ok).toBe(false)
  if ("diagnostics" in result) expect(result.diagnostics).toContainEqual(expect.objectContaining({ code, message: expect.stringContaining(message) }))
})

test("TS3: enum inclusion agrees with finite set inclusion, including in optional record fields", () => {
  const keys = ["a", "b", "c"]
  for (let a = 1; a < 8; a++) for (let b = 1; b < 8; b++) {
    const subset = (mask: number) => S.text(keys.filter((_, i) => mask & (1 << i)))
    for (const fromOptional of [false, true]) for (const toOptional of [false, true]) {
      const from = S.record({ values: S.array(subset(a)) }, fromOptional ? ["values"] : [])
      const to = S.record({ values: S.array(subset(b)) }, toOptional ? ["values"] : [])
      expect(assignable(from, to) === undefined).toBe((a & b) === a && (!fromOptional || toOptional))
    }
  }
})

const meta = { id: "probe.order", version: "1" }
test("TS3: perform input checks the output contract of the previous perform", () => {
  const want = S.record({ status: S.text(["ready", "done"]) })
  for (const [output, ok] of [
    [S.record({ status: S.text(["ready"]) }), true],
    [S.record({ status: S.text(["ready"]) }, ["status"]), false],
    [S.record({ status: S.text(["other"]) }), false],
  ] as const) {
    const load = fn({ ...meta, id: "probe.load", input: S.emptyRecord, output })
    const store = fn({ ...meta, id: "probe.store", input: want, output: S.emptyRecord })
    const result = lowerFlow(flow(meta, [
      perform("load", load, X.record({}), { bind: "row" }),
      perform("store", store, X.ref("row")), succeed(X.lit(true)),
    ]), options)
    expect(result.ok).toBe(ok)
    if (!result.ok) expect(result.diagnostics[0]?.code).toBe("NORN-FLOW-VALUE")
  }
})

test("TS3: conflicting caught failure fields and colliding status names refuse in either order", () => {
  for (const [left, right] of [["Disk", "Load"], ["HTTP", "Http"]] as const) {
    const errors = [taggedError(left, S.record({ detail: S.text() })), taggedError(right, S.record({ detail: S.integer }))]
    for (const order of [errors, [...errors].reverse()]) {
      const handle = fn({ ...meta, input: S.emptyRecord, output: S.emptyRecord, errors: order })
      const recover = [succeed(X.lit(false))]
      const run = perform("load", handle, X.record({}))
      const result = lowerFlow(flow(meta, [catchTag([catchTag([run], left, recover)], right, recover), succeed(X.lit(true))]), options)
      expect(result.ok).toBe(false)
      if (!result.ok) expect(result.diagnostics).toContainEqual(expect.objectContaining({ code: "NORN-FLOW-TAG", message: expect.stringContaining(left === "HTTP" ? "failed_http" : "incompatible types") }))
    }
  }
})

test("generated Run names and conditional array and nullable schemas", () => {
  for (const [id, ok] of [["ops.pay_roll", true], ["ops.pay__roll", false], ["ops.pay_2", false]] as const) {
    expect(lowerFlow(flow({ ...meta, id }, [succeed(X.lit(true))]), options).ok).toBe(ok)
  }
  const either = (other: X.Expr) => X.typeOf(X.conditional(X.lit(true), X.list(), other), new Map())
  expect(() => either(X.lit(false))).toThrow(/array/)
  expect(either(X.list(X.lit(true)))).toEqual(S.array(S.boolean))
  const nullable = S.nullable(S.text(["x"]))
  expect(S.mergeSchemas(nullable, S.text())).toEqual(S.nullable(S.text()))
  expect(S.mergeSchemas(S.text(), nullable)).toEqual(S.nullable(S.text()))
})

test("decimal assignment retains scale and units, including through nullable fields", () => {
  const mass = (scale: number, code: string): S.ValueSchema => ({
    type: "number", nornType: { kind: "decimal", scale, unit: { system: "ucum", code } },
  })
  const amount = mass(2, "g")
  expect(assignable(S.nullable(amount), S.nullable(mass(2, "g")))).toBeUndefined()
  for (const other of [S.number, S.integer, mass(3, "g"), mass(2, "kg")]) {
    expect(assignable(other, S.nullable(amount))).toContain("exact decimal")
  }
})
