import { expect, test } from "bun:test"
import { compileModule } from "../src/compile-stdin.ts"
import * as E from "../src/builder/entity.ts"

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
