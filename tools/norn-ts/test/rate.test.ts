import { expect, test } from "bun:test"
import { compileModule } from "../src/compile-stdin.ts"
import * as E from "../src/builder/entity.ts"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import { canonicalJson } from "../src/canonical.ts"
import { compileDefinitions } from "../src/compiler/definitions.ts"
import { TOOL_ROOT } from "../src/profile.ts"

const source = (scale: string, currency = '"currency"') => `
import { Schema } from "effect"
import { Norn } from "@norn/authoring"
export const Quote = Norn.type({ name: "demo.Quote", title: "Quote", version: "1.0.0", dependencies:[{artifactId:"core.units",version:"1.0.0",kind:"entity"}],
 properties: Schema.Struct({ id: Schema.String, currency: Schema.Literal("USD"),
 price: Norn.rate(${scale}, {currency: ${currency}, perUnit: {system: "ucum", code: "g"}, rounding: "half_even"}) }),
 identity: [{ name: "primary", properties: ["id"] }] })`

test("VAL-RATE typed authoring retains currency, denominator and one rounding site", () => {
 const result = compileModule(source("6"), {orgId: "org_demo", environmentId: "dev"})
 if (!result.ok || result.artifacts === undefined) throw new Error(JSON.stringify(result))
 expect(result.artifacts[0]).toMatchObject({payload: {objectType: {properties: expect.arrayContaining([{id: "price", type: E.rate(6, "currency", {system: "ucum", code: "g"}, "half_even")}])}}})
})
test("VAL-RATE refuses an unbounded scale or a nonliteral currency binding", () => {
 expect(compileModule(source("19"), {orgId: "org_demo", environmentId: "dev"}).ok).toBe(false)
 expect(compileModule(source("6", "missing"), {orgId: "org_demo", environmentId: "dev"}).ok).toBe(false)
})

test("VAL-RATE closed numeric schemas retain exact declarations in a direct callback", () => {
 const result = compileModule(`import { Schema } from "effect"
 import { Norn } from "@norn/authoring"
 export const Pricing = Norn.decision({id:"demo.rule.pricing",title:"Pricing",version:"1.0.0",contract:"decide",scopeRequired:[],dependencies:[{artifactId:"core.units",version:"1.0.0",kind:"entity"}],
 input:Schema.Struct({slice:Schema.Struct({currency:Schema.Literal("USD"),rate:Norn.rate(6,{currency:"currency",perUnit:{system:"ucum",code:"g"}}),grams:Norn.decimal(3,{unit:{system:"ucum",code:"g"}})}),event:Schema.Struct({})}),
 output:Schema.Struct({facts:Schema.Array(Schema.Struct({id:Schema.String})),commands:Schema.Array(Schema.Struct({actionId:Schema.String,currency:Schema.Literal("USD"),amount:Norn.decimal(2,{currency:"currency",rounding:"half_even"})})),timers:Schema.Array(Schema.Struct({id:Schema.String}))})
 }, input => ({facts:[],commands:[{actionId:"quote",currency:"USD",amount:input.slice.rate*input.slice.grams}],timers:[]}))`,{orgId:"org_demo",environmentId:"dev"})
 if (!result.ok || result.artifacts === undefined) throw new Error(JSON.stringify(result))
 expect(result.artifacts[0]).toMatchObject({dependencies:[{artifactId:"core.units",version:"1.0.0",kind:"entity",required:true}],payload:{inputSchema:{properties:{slice:{properties:{rate:{type:"number",nornType:E.rate(6,"currency",{system:"ucum",code:"g"})}}}}},graph:{nodes:expect.arrayContaining([{id:"decision",name:"decision",type:"expressionNode",content:{expressions:expect.any(Array)}}])}}})
})

test("JEWEL-PRICE native acceptance uses freshly compiled typed declarations", () => {
 const sourcePath = "tools/norn-ts/fixtures/definitions/jewelry-pricing.source.ts"
 const result = compileDefinitions(join(TOOL_ROOT, "fixtures/definitions/jewelry-pricing.source.ts"), {
  scope: {orgId:"org_demo",environmentId:"dev"}, sourcePath,
 })
 if (!result.ok) throw new Error(JSON.stringify(result.diagnostics))
 expect(result.warnings).toEqual([])
 const retained = JSON.parse(readFileSync(join(TOOL_ROOT,"fixtures/definitions/jewelry-pricing.ir.json"),"utf8"))
 expect(canonicalJson(result.artifacts)).toBe(canonicalJson(retained))
 expect(result.artifacts).toEqual(expect.arrayContaining([expect.objectContaining({kind:"action",payload:expect.objectContaining({derive:[{field:"amount",expression:"input.price_per_g * input.fine_g",inputRequired:false}]})})]))
})

test("VAL-EXACT-1 exact callback branches refuse before exceeding expression bounds", () => {
 const branchSource = (count: number, label = "ok") => `
 import { Schema } from "effect"
 import { Norn } from "@norn/authoring"
 export const Branches = Norn.decision({id:"demo.branches",title:"Branches",version:"1.0.0",contract:"sub_decision",scopeRequired:[],
 input:Schema.Struct({amount:Norn.decimal(2)}),output:Schema.Struct({amount:Norn.decimal(2),label:Schema.String})}, input => {
 ${Array.from({length:count}, (_,i) => `if (input.amount > ${i}) return {amount:input.amount,label:${JSON.stringify(label)}}`).join("\n")}
 return {amount:input.amount,label:"fallback"}
 })`
 const scope = {orgId:"org_demo",environmentId:"dev"}
 expect(compileModule(branchSource(2), scope).ok).toBe(true)
 for (const source of [branchSource(160), branchSource(1, "🪙".repeat(5000))]) {
  const result = compileModule(source, scope)
  expect(result.ok).toBe(false)
  if (result.ok) throw new Error("oversized exact callback compiled")
  expect(result.diagnostics).toEqual(expect.arrayContaining([expect.objectContaining({code:"NORN-FLOW-UNBOUNDED",message:expect.stringContaining("norn-expr/4 source bounds")})]))
 }
})


test("METRIC-ADDITIVITY-1 decimal authoring retains explicit classes and refuses incomplete ratios", () => {
  const omitted = source("6").replace(/Norn\.rate\(6,.*?\) /, 'Norn.decimal(6) ')
  const explicit = source("6").replace(/Norn\.rate\(6,.*?\) /, 'Norn.decimal(6, {additivity:{kind:"undeclared"}}) ')
  const scope = {orgId:"org_demo",environmentId:"dev"}
  const omittedResult = compileModule(omitted, scope)
  const explicitResult = compileModule(explicit, scope)
  if (!omittedResult.ok || !explicitResult.ok || omittedResult.artifacts === undefined || explicitResult.artifacts === undefined) throw new Error("undeclared authoring failed")
  expect(canonicalJson(explicitResult.artifacts)).toBe(canonicalJson(omittedResult.artifacts))
  for (const declaration of ['{kind:"additive"}', '{kind:"semi_additive"}', '{kind:"non_additive"}', '{kind:"ratio",numerator:"recovered",denominator:"input"}']) {
    const text = source("6").replace(/Norn\.rate\(6,.*?\) /, `Norn.decimal(6, {additivity:${declaration}}) `)
    const result = compileModule(text, {orgId:"org_demo",environmentId:"dev"})
    if (!result.ok || result.artifacts === undefined) throw new Error(JSON.stringify(result))
    expect(JSON.stringify(result.artifacts)).toContain('"additivity"')
  }
  for (const declaration of ['{kind:"ratio"}', '{kind:"additive",numerator:"x"}', '{kind:"guess"}']) {
    const text = source("6").replace(/Norn\.rate\(6,.*?\) /, `Norn.decimal(6, {additivity:${declaration}}) `)
    expect(compileModule(text, {orgId:"org_demo",environmentId:"dev"}).ok).toBe(false)
  }
})

test("METRIC-ADDITIVITY-REF-1 a ref carries a use-site class; undeclared is the omitted default", () => {
  const scope = { orgId: "org_demo", environmentId: "dev" }
  const authored = (options: string) => compileModule(source("6").replace(/Norn\.rate\(6,.*?\) /, `Norn.ref("core.quantity"${options}) `), scope)
  const omitted = authored("")
  const undeclared = authored(', {additivity:{kind:"undeclared"}}')
  if (!omitted.ok || !undeclared.ok || omitted.artifacts === undefined || undeclared.artifacts === undefined) throw new Error("undeclared ref authoring failed")
  expect(canonicalJson(undeclared.artifacts)).toBe(canonicalJson(omitted.artifacts))
  const additive = authored(', {additivity:{kind:"additive"}}')
  if (!additive.ok || additive.artifacts === undefined) throw new Error(JSON.stringify(additive))
  const price = JSON.parse(JSON.stringify(additive.artifacts)).flatMap((artifact: { payload: { objectType?: { properties: Array<{ id: string; type: unknown }> } } }) => artifact.payload.objectType?.properties ?? []).find((property: { id: string }) => property.id === "price")
  expect(price?.type).toEqual({ kind: "ref", name: "core.quantity", additivity: { kind: "additive" } })
  for (const options of [', {additivity:{kind:"guess"}}', ', {unit:"g"}']) expect(authored(options).ok).toBe(false)
})

test("METRIC-ADDITIVITY-1 ratio operands must be complete PropertyId values", () => {
  for (const field of ["numerator", "denominator"]) {
    for (const value of ["bad.path", "Uppercase", "", "input\n"]) {
      const operands = { numerator: "recovered", denominator: "input", [field]: value }
      const declaration = JSON.stringify({ kind: "ratio", ...operands })
      const text = source("6").replace(/Norn\.rate\(6,.*?\) /, `Norn.decimal(6, {additivity:${declaration}}) `)
      const result = compileModule(text, { orgId: "org_demo", environmentId: "dev" })
      expect(result.ok).toBe(false)
      if (result.ok) throw new Error("invalid ratio operand emitted an artifact")
      expect(result.diagnostics).toEqual(expect.arrayContaining([
        expect.objectContaining({ code: "NORN-FLOW-LITERAL", message: expect.stringContaining("PropertyId") }),
      ]))
    }
  }
})
