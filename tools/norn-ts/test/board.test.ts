import { expect, test } from "bun:test"
import { join } from "node:path"
import { canonicalJson } from "../src/canonical.ts"
import * as B from "../src/builder/board.ts"
import * as E from "../src/builder/entity.ts"
import { compileDefinitions } from "../src/compiler/definitions.ts"
import { TOOL_ROOT } from "../src/profile.ts"

const scope = { orgId: "org_demo", environmentId: "dev" }
const compile = (file: string) => compileDefinitions(join(TOOL_ROOT, "fixtures/definitions", file), { scope, sourcePath: file })

test("Norn.board lowers typed metrics to one norn.board/1 document with a builder twin", () => {
  const result = compile("board.source.ts")
  if (!result.ok) throw new Error(JSON.stringify(result.diagnostics))
  expect(result.boards.length).toBe(1)
  expect(result.artifacts).toMatchObject([{ artifactId: "boards.Invoice.entity" }, { artifactId: "boards.Row.entity" }])
  const open = B.metric({ id: "boards.open", title: "Open", over: "boards.Invoice", measure: { sum: "open" }, time: "due",
    where: [{ property: "state", op: "eq", value: "submitted" }], owner: "boards" })
  const always = { start: "1900-01-01T00:00:00Z", end: "2200-01-01T00:00:00Z" }
  const twin = B.lowerBoard(B.board({ id: "boards.open_board", title: "Open", version: "0.1.0", rows: "boards.Row", results: "boards.Row", panels: [
    { id: "total", title: "Total", metric: open, window: always, maxAgeMs: 60000 },
    { id: "by_customer", title: "By customer", metric: open, groupBy: "customer", window: always, maxAgeMs: 60000 },
  ] }), scope)
  const [board] = result.boards
  if (board === undefined) throw new Error("board missing")
  expect(canonicalJson(board)).toBe(canonicalJson(twin))
  expect(board).toMatchObject({ panels: [ {}, { question: { irVersion: "0.1", name: "boards.open_board.by_customer", objectSet: "boards.Invoice",
    aggregate: [{ metric: "boards.open" }], groupBy: [{ path: { property: "customer" } }], window: always, freshness: { kind: "live", maxAgeMs: 60000 } } }] })
  expect(board).toMatchObject({ panels: [{ definition: { type_params: { expr: "open", metric_aggregation_params: { semantic_model: "boards.Invoice", agg: "sum", agg_time_dimension: "due" } } } }, {}] })
})

test("Norn.json lowers to EntityIR json-ext", () => {
  const result = compile("board.source.ts")
  if (!result.ok) throw new Error(JSON.stringify(result.diagnostics))
  const row = result.artifacts.find((a) => typeof a === "object" && a !== null && !Array.isArray(a) && "artifactId" in a && a.artifactId === "boards.Row.entity")
  expect(row).toMatchObject({ payload: { objectType: { properties: expect.arrayContaining([{ id: "buckets", type: { kind: "json-ext" } }]) } } })
  expect(E.json).toEqual({ kind: "json-ext" })
})

for (const fixture of ["board-unknown-property", "board-unknown-group"]) test(`${fixture} is a type error`, () => {
  const result = compile(`invalid/${fixture}.ts`)
  expect(result.ok).toBe(false)
  if (!result.ok) expect(result.diagnostics.some((d) => d.code === "NORN-FLOW-TYPE")).toBe(true)
})


test("metrics and board groups resolve object and transitive interface properties", () => {
  const result = compile("board-inherited.source.ts")
  if (!result.ok) throw new Error(JSON.stringify(result.diagnostics))
  expect(result.boards).toMatchObject([{ panels: [{ definition: { type_params: {
    expr: "open", metric_aggregation_params: { agg_time_dimension: "due" },
  } }, question: { groupBy: [{ path: { property: "customer" } }] } }] }])
})
