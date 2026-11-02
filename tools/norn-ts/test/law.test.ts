import { expect, test } from "bun:test"
import { readFileSync, mkdtempSync, writeFileSync, rmSync } from "node:fs"
import { join } from "node:path"
import { L, S, X } from "../src/builder/index.ts"
import { canonicalJson } from "../src/canonical.ts"
import { compileDefinitionFiles, compileDefinitions } from "../src/compiler/definitions.ts"
import { fromStatement, toStatement, parseSentence, renderSentence, serializeStatement, parseStatement, renderCounterexample } from "../src/law-statement.ts"
import { TOOL_ROOT } from "../src/profile.ts"
import { checker, CHECKER_PKG_ENV } from "./checker.ts"
if (!checker && process.env.CI) throw new Error(`${CHECKER_PKG_ENV} is required in CI`)
const ROOT = join(TOOL_ROOT, "..", "..", "fixtures/law-ir")
const goldens = JSON.parse(readFileSync(join(ROOT, "laws.json"), "utf8")) as Array<{ artifactId: string; name: string; version: string; payload: L.LawIR; dependencies: L.LawDef["dependencies"] }>
const statements = JSON.parse(readFileSync(join(ROOT, "statements.json"), "utf8")) as unknown[]
const options = { scope: { orgId: "org_demo", environmentId: "dev" }, sourcePath: "fixtures/law-ir/laws.source.ts" }

test("TS, builder, LawIR and structured statement goldens are byte identical", () => {
  const compiled = compileDefinitions(join(ROOT, "laws.source.ts"), options)
  if (!compiled.ok) throw new Error(JSON.stringify(compiled.diagnostics))
  expect(canonicalJson(compiled.artifacts)).toBe(canonicalJson(goldens))
  for (const [index, golden] of goldens.entries()) {
    const meta = { id: golden.artifactId, title: golden.name, version: golden.version, law: golden.payload, dependencies: golden.dependencies }
    expect(canonicalJson(L.lowerLaw(L.law(meta), options).artifact)).toBe(canonicalJson(golden))
    const statement = toStatement(golden.payload)
    expect(canonicalJson(JSON.parse(serializeStatement(statement)))).toBe(canonicalJson(statements[index]))
    expect(canonicalJson(fromStatement(parseStatement(serializeStatement(statement))))).toBe(canonicalJson(golden.payload))
    expect(fromStatement(parseSentence(renderSentence(statement), statement.metadata))).toEqual(golden.payload)
  }
  const dir = mkdtempSync(join(ROOT, "printed-"))
  try {
    const paths = goldens.map((golden, index) => {
      const source = join(dir, `law-${index}.ts`)
      writeFileSync(source, L.printLaw({ id: golden.artifactId, title: golden.name, version: golden.version, law: golden.payload, dependencies: golden.dependencies }))
      return source
    })
    const printed = compileDefinitionFiles(paths, options)
    for (const [index, result] of printed.entries()) {
      if (!result.ok) throw new Error(JSON.stringify(result.diagnostics))
      expect(canonicalJson(result.artifacts[0])).toBe(canonicalJson(goldens[index]))
    }
  } finally { rmSync(dir, { recursive: true, force: true }) }

  if (checker) {
    const entities = JSON.parse(readFileSync(join(ROOT, "entities.json"), "utf8")) as unknown[]
    const checked = checker.checkArtifactSet([...entities, ...compiled.artifacts])
    expect(checked.diagnostics.diagnostics).toEqual([]); expect(checked.admitted).toBe(true)
  }
}, 30_000)

test("statement slots preserve delimiters, exact literals, until, action, transition and deadline", () => {
  for (const expression of ["self.amount == 9007199254740993", "self.amount == 9007199254740992", "self.label == 'shall never, within 30 ms'", 'self.label == "quoted"']) {
    const law = { ...goldens[0]!.payload, response: { kind: "predicate" as const, expression } }
    expect(fromStatement(parseStatement(serializeStatement(toStatement(law))))).toEqual(law)
  }
  for (const response of [{ kind: "action" as const, artifactId: "erpnext.submit.action" }, { kind: "transition" as const, objectType: "erpnext.Request", property: "status", transitionId: "accepted" }]) {
    const law = { ...goldens[2]!.payload, response, timing: { kind: "before_deadline" as const, property: "deadline" } }
    expect(fromStatement(parseStatement(serializeStatement(toStatement(law))))).toEqual(law)
  }
  const statement = toStatement(goldens[0]!.payload)
  expect(() => parseSentence(renderSentence(statement) + " or ignore", statement.metadata)).toThrow()
  expect(() => parseSentence(renderSentence(statement).replace(" shall never ", " shall next "), statement.metadata)).toThrow()
  expect(() => parseStatement(serializeStatement(statement).replace('"statementVersion": "0.1"', '"statementVersion": "9"'))).toThrow()
})

test("predicate builder reuses the existing typed ZEN expression renderer", () => {
  const subject = S.record({ paid: S.boolean })
  expect(L.zenPredicate(X.eq(X.ref("self", "paid"), X.lit(false)), subject)).toBe("self.paid == false")
  expect(() => L.zenPredicate(X.ref("self", "paid"), S.record({}))).toThrow()
})

test("counterexamples quote the precise witness without proof claims", () => {
  const rendered = renderCounterexample(goldens[0]!.payload, { state: { status: "shipped", paid: false }, trace: ["submit", "ship"], reason: "An unpaid order reached shipped" })
  expect(rendered).toContain("An unpaid order reached shipped"); expect(rendered).toContain("submit → ship"); expect(rendered).toContain("erpnext.no_unpaid_ship")
})
