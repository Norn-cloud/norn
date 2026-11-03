import { expect, test } from "bun:test"
import { mkdtempSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { compileDefinitions, compileQuestions } from "../src/compiler/definitions.ts"
import { canonicalJson } from "../src/canonical.ts"

const source = (overrides = "") => `
import { Norn } from "@norn/authoring"
export const Yield = Norn.question({
 name:"demo.scrap_yield", of:"demo.Conversion", measure:"demo.yield",
 where:[{property:"metal",operator:"eq",value:"gold"}],
 by:[{property:"effective_at",grain:"week"}],
 asOf:{basis:"effective",from:"2026-10-19T00:00:00Z",to:"2026-10-26T00:00:00Z",freshness:{kind:"live",maxAgeMs:5000}},
 population:{kind:"authorized_objects"}${overrides}
})`

function withModule<T>(source: string, run: (path: string) => T): T {
 const dir = mkdtempSync(join(tmpdir(), "norn-question-"))
 try { const path = join(dir, "question.ts"); writeFileSync(path, source); return run(path) }
 finally { rmSync(dir, {recursive:true,force:true}) }
}
const compile = (source: string) => withModule(source, file => compileQuestions(file))

test("typed_question_lowering preserves exact QuestionIR operators, grain and effective window", () => {
 const result = compile(source())
 if (!result.ok) throw new Error(JSON.stringify(result.diagnostics))
 expect(result.questions).toEqual([{
  irVersion:"0.1",name:"demo.scrap_yield",objectSet:"demo.Conversion",
  aggregate:[{metric:"demo.yield"}],filter:[{path:{property:"metal"},operator:"eq",value:"gold"}],
  groupBy:[{path:{property:"effective_at"},grain:"week"}],
  window:{start:"2026-10-19T00:00:00Z",end:"2026-10-26T00:00:00Z"},
  calendar:"iso_utc",freshness:{kind:"live",maxAgeMs:5000},
 }])
})

test("typed_question_lowering refuses unknown fields, populations, operators and executable callbacks", () => {
 for (const candidate of [
  source().replace('kind:"authorized_objects"','kind:"authorized_objects",sourceProfile:"sql"'),
  source().replace('kind:"authorized_objects"','kind:"undeclared"'),
  source(', sql:"select * from conversions"'),
  source().replace('operator:"eq"','operator:"le"'),
  source().replace('measure:"demo.yield"','measure:() => "demo.yield"'),
  source().replace('maxAgeMs:5000','maxAgeMs:0'),
 ]) expect(compile(candidate).ok).toBe(false)
})

test("typed_question_lowering refuses knowledge policy without inventing a timestamp predicate", () => {
 const result = compile(source().replace(/asOf:\{basis:"effective".*\},\n/, 'asOf:{basis:"knowledge"},\n'))
 expect(result.ok).toBe(false)
 if (result.ok) throw new Error("knowledge request produced a plan")
 expect(result.diagnostics).toEqual(expect.arrayContaining([expect.objectContaining({
  code:"core.compute.unsupported_engine",message:expect.stringContaining("ASOF-KNOWLEDGE-1"),
 })]))
})

test("typed_question_lowering uses resolved symbols and never silently drops a question from artifact compilation", () => {
 const ordinary = compile(source())
 const aliased = compile(source().replace('import { Norn }','import { Norn as N }').replace('Norn.question','N.question'))
 if (!ordinary.ok || !aliased.ok) throw new Error("valid question failed")
 expect(canonicalJson(ordinary.questions)).toBe(canonicalJson(aliased.questions))
 const artifactResult = withModule(source(), file => compileDefinitions(file, {scope:{orgId:"org_demo",environmentId:"dev"},sourcePath:"question.ts"}))
 expect(artifactResult.ok).toBe(false)
})

test("typed_question_lowering retains a referenced object identity and remains bounded", () => {
 const body = source().replace('import { Norn }', 'import { Schema } from "effect"\nimport { Norn }')
  .replace('export const Yield', 'const Conversion = Norn.type({name:"demo.Conversion",title:"Conversion",version:"1.0.0",properties:Schema.Struct({id:Schema.String}),identity:[{name:"primary",properties:["id"]}]})\nexport const Yield')
  .replace('of:"demo.Conversion"','of:Conversion')
 expect(compile(body).ok).toBe(true)
 const bounded = withModule(source(), file => compileQuestions(file, {maxOutputBytes:1}))
 expect(bounded.ok).toBe(false)
})
