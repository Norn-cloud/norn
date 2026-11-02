import { expect, test } from "bun:test"
import { compileModule } from "../src/compile-stdin.ts"
import * as E from "../src/builder/entity.ts"

const source = (scale: string, currency = '"currency"') => `
import { Schema } from "effect"
import { Norn } from "@norn/authoring"
export const Quote = Norn.type({ name: "demo.Quote", title: "Quote", version: "1.0.0",
 properties: Schema.Struct({ id: Schema.String, currency: Schema.Literal("USD"),
 price: Norn.rate(${scale}, {currency: ${currency}, perUnit: {system: "ucum", code: "g"}, rounding: "half_even"}) }),
 identity: [{ name: "primary", properties: ["id"] }] })`

test("VAL-RATE typed authoring retains currency, denominator and one rounding site", () => {
 const result = compileModule(source("6"), {orgId: "org_demo", environmentId: "dev"})
 if (!result.ok) throw new Error(JSON.stringify(result.diagnostics))
 expect(result.artifacts[0]).toMatchObject({payload: {objectType: {properties: expect.arrayContaining([{id: "price", type: E.rate(6, "currency", {system: "ucum", code: "g"}, "half_even")}])}}})
})
test("VAL-RATE refuses an unbounded scale or a nonliteral currency binding", () => {
 expect(compileModule(source("19"), {orgId: "org_demo", environmentId: "dev"}).ok).toBe(false)
 expect(compileModule(source("6", "missing"), {orgId: "org_demo", environmentId: "dev"}).ok).toBe(false)
})
