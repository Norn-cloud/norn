import { describe, expect, test } from "bun:test"
import { readFileSync, mkdtempSync, writeFileSync, rmSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { pathToFileURL } from "node:url"
import { E, L, S, X } from "../src/builder/index.ts"
import { canonicalJson } from "../src/canonical.ts"
import { compileDefinitionFiles, compileDefinitions } from "../src/compiler/definitions.ts"
import { fromStatement, toStatement, parseSentence, renderSentence, serializeStatement, parseStatement, renderCounterexample } from "../src/law-statement.ts"
import { TOOL_ROOT } from "../src/profile.ts"
import { renderDeontic } from "../src/builder/deontic.ts"
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

test.skipIf(!process.env[CHECKER_PKG_ENV])("explain_uncovered_writer", async () => {
  const pkg = process.env[CHECKER_PKG_ENV]
  if (!pkg) throw new Error(`${CHECKER_PKG_ENV} is required for explanation qualification`)
  const abi: unknown = await import(pathToFileURL(join(pkg, "norn_checker_wasm.js")).href)
  if (typeof abi !== "object" || abi === null || !("explainArtifactSet" in abi) || typeof abi.explainArtifactSet !== "function") {
    throw new Error("The checker package must export explainArtifactSet")
  }
  const entities: unknown = JSON.parse(readFileSync(join(ROOT, "entities.json"), "utf8"))
  if (!Array.isArray(entities)) throw new Error("Expected the law fixture's entity array")
  const response: unknown = JSON.parse(abi.explainArtifactSet(JSON.stringify([...entities, ...goldens]), "{}", "law", "erpnext.no_unpaid_ship", "{}"))
  expect(response).toMatchObject({
    format: "norn/explain@2",
    admitted: true,
    diagnostics: [],
    report: {
      laws: [expect.objectContaining({
        proof: "unknown",
        sourcePath: "fixtures/law-ir/laws.source.ts",
        whyUnknown: expect.arrayContaining([
          expect.objectContaining({ writer: expect.objectContaining({ kind: "transition", objectType: "erpnext.SalesOrder" }), footprint: expect.any(String), fix: expect.any(String) }),
          expect.objectContaining({ footprint: "migration", fix: expect.any(String) }),
        ]),
      })],
    },
  })
})

test("law exceptions never pass through a statement or render from two homes", () => {
  const law = goldens[0]!.payload
  const exception = { id: "exempt", overrides: law.id, condition: "true", rationale: "exempt", source: { artifactId: "a", pointer: "" } }
  const excepted: L.LawIR = { ...law, exceptions: [exception] }
  expect(() => toStatement(excepted)).toThrow("cannot carry exceptions")
  const rule = { modality: "obligation", law: excepted, authorityId: "a", authorityVersion: "1", source: exception.source } as const
  expect(renderDeontic({ ...rule })).toContain(`EXCEPT "exempt" WHEN "true"`)
  expect(() => renderDeontic({ ...rule, exceptions: [exception] })).toThrow("law.exception.two_homes")
})

// A1 (#1176): closures, `sum` and calendar-day helpers render the norn-zen/6
// forms (`crates/norn-expr/README.md`) and the Rust checker admits them in laws.
describe("closures, sum and day helpers", () => {
  const line = S.record({ amount: S.integer, sku: S.text() })
  const entry = S.record({ status: S.text(["draft", "posted"]), issue_date: S.text(), due_date: S.text(), debit: S.integer, credit: S.integer, debit_sku: S.text(), credit_sku: S.text(), lines: S.array(line) })
  // The kernel binds a law's `self` to the subject's own properties, so these laws iterate a list built from them.
  // A law sums over linked objects through an inverse role (LAW-LINK-MANY-1, `canon.ItemizedDocument`'s law); a
  // to-many forward link of an object type stays refused.
  const entryType = E.objectType({
    name: "books.JournalEntry", title: "Journal entry", version: "0.1.0",
    properties: [E.property("status", E.enumeration("draft", "posted")), E.property("issue_date", E.date), E.property("due_date", E.date), E.property("debit", E.integer), E.property("credit", E.integer), E.property("debit_sku", E.text()), E.property("credit_sku", E.text())],
  })
  const self = (...path: string[]) => X.ref("self", ...path)
  const lines = X.list(X.record({ amount: self("debit"), sku: self("debit_sku") }), X.record({ amount: self("credit"), sku: self("credit_sku") }))
  const LINES = "[{amount: self.debit, sku: self.debit_sku}, {amount: self.credit, sku: self.credit_sku}]"
  const cases: ReadonlyArray<readonly [string, X.Expr, string]> = [
    ["balanced", X.eq(X.sum(X.map(lines, "l", (l) => X.ref("l", "amount"))), X.lit(0)), `sum(map(${LINES} as l, l.amount)) == 0`],
    ["no_zero_lines", X.all(lines, "l", (l) => X.ne(X.ref("l", "amount"), X.lit(0))), `all(${LINES} as l, l.amount != 0)`],
    ["has_sku", X.some(lines, "l", (l) => X.call("startsWith", X.ref("l", "sku"), X.lit("SKU-"))), `some(${LINES} as l, startsWith(l.sku, 'SKU-'))`],
    ["none_negative", X.none(lines, "l", X.lt(X.ref("l", "amount"), X.lit(0))), `none(${LINES} as l, l.amount < 0)`],
    ["one_big", X.one(lines, "l", X.gt(X.ref("l", "amount"), X.lit(1000))), `one(${LINES} as l, l.amount > 1000)`],
    ["few_lines", X.le(X.count(lines, "l", X.gt(X.ref("l", "amount"), X.lit(0))), X.lit(50)), `count(${LINES} as l, l.amount > 0) <= 50`],
    ["filtered", X.ge(X.call("len", X.filter(lines, "l", X.gt(X.ref("l", "amount"), X.lit(0)))), X.lit(1)), `len(filter(${LINES} as l, l.amount > 0)) >= 1`],
    // Nested closures bind distinct aliases. A string read through a built list is costed at its largest single
    // value (`norn-expr` cost.rs), so the inner body compares amounts; a nested string test would exceed the budget.
    ["nested", X.all(lines, "a", (a) => X.some(X.list(X.lit(0), X.lit(1)), "p", (p) => X.gt(X.ref("a", "amount"), p))), `all(${LINES} as a, some([0, 1] as p, a.amount > p))`],
    ["terms", X.le(X.daysBetween(self("issue_date"), self("due_date")), X.lit(30)), "days_between(date(self.issue_date), date(self.due_date)) <= 30"],
    ["due_by", X.ge(X.addDays(self("issue_date"), X.lit(30)), X.call("date", self("due_date"))), "add_days(date(self.issue_date), 30) >= date(self.due_date)"],
    ["posted_balanced", X.or(X.ne(self("status"), X.lit("posted")), X.eq(X.sum(X.map(lines, "l", (l) => X.ref("l", "amount"))), X.lit(0))), `(self.status != 'posted') or (sum(map(${LINES} as l, l.amount)) == 0)`],
  ]

  test("render the norn-zen/6 closure, sum and day forms", () => {
    for (const [, expression, zen] of cases) expect(L.zenPredicate(expression, entry)).toBe(zen)
    expect(X.typeOf(X.map(self("lines"), "l", (l) => X.ref("l", "sku")), new Map([["self", { zen: "self", schema: entry }]]))).toEqual(S.array(S.text()))
    // A parsed date is not wrapped twice.
    expect(L.zenPredicate(X.le(X.daysBetween(X.call("date", self("issue_date")), X.addDays(self("due_date"), X.lit(-1))), X.lit(0)), entry)).toBe("days_between(date(self.issue_date), add_days(date(self.due_date), -1)) <= 0")
  })

  test("refuse what norn-zen/6 refuses, before the checker sees it", () => {
    const refuse = (expression: () => X.Expr, message: RegExp) => expect(() => L.zenPredicate(expression(), entry)).toThrow(message)
    refuse(() => X.some(self("status"), "l", X.lit(true)), /iterates a list/)
    refuse(() => X.some(self("lines"), "l", X.ref("l", "amount")), /body must be Boolean/)
    refuse(() => X.eq(X.call("len", X.map(self("lines"), "l", X.lit(null))), X.lit(0)), /map body must not be null/)
    refuse(() => X.some(self("lines"), "self", X.lit(true)), /plain identifier/)
    refuse(() => X.some(self("lines"), "not-an-ident", X.lit(true)), /plain identifier/)
    refuse(() => X.some(self("lines"), "l", (l) => X.some(self("lines"), "l", X.eq(X.ref("l", "sku"), X.ref("l", "sku")))), /shadows a binding/)
    refuse(() => X.eq(X.sum(X.map(self("lines"), "l", (l) => X.ref("l", "sku"))), X.lit(0)), /sum expects a list of numbers/)
    refuse(() => X.le(X.call("days_between", self("issue_date"), self("due_date")), X.lit(1)), /parsed dates/)
    refuse(() => X.ge(X.addDays(self("issue_date"), X.lit("x")), X.call("date", self("due_date"))), /whole number of days/)
    refuse(() => X.ge(X.addDays(self("issue_date"), X.lit(1.5)), X.call("date", self("due_date"))), /whole number of days, not number/)
    refuse(() => X.ge(X.addDays(self("issue_date"), X.lit(1)), X.call("timestamp", self("due_date"))), /qualified for numbers only/)
  })

  test("the Rust checker admits every rendered law", () => {
    if (!checker) return
    const entities = [entryType].map((def) => { const r = E.lowerEntity(def, options); if (!r.ok) throw new Error(JSON.stringify(r.diagnostics)); return r.artifact })
    const laws = cases.map(([name, expression]) => L.lowerLaw(L.law({
      id: `books.JournalEntry.${name}.law`, title: name, version: "0.1.0",
      law: { lawVersion: "0.1", id: `books.JournalEntry.${name}`, owner: "books", severity: "error", rationale: name, provenance: { source: "authoring_sdk" }, scope: { root: { kind: "app", id: "books" }, objectTypes: ["books.JournalEntry"] }, subject: "books.JournalEntry", modality: { kind: "always" }, response: { kind: "predicate", expression: L.zenPredicate(expression, entry) } },
      dependencies: [{ artifactId: "books.JournalEntry.entity", kind: "entity", version: "0.1.0", required: true }],
    }), options).artifact)
    const checked = checker.checkArtifactSet([...entities, ...laws])
    expect(checked.diagnostics.diagnostics).toEqual([]); expect(checked.admitted).toBe(true)
  }, 30_000)
})

// A2 (#1176): TypeScript lambdas in laws and guards lower statically to the A1 forms.
test("typed lambdas in laws and guards compile to norn-zen/6 and are admitted", () => {
  const compiled = compileDefinitions(join(TOOL_ROOT, "corpus/accept/law-closures.ts"), { scope: options.scope, sourcePath: "corpus/accept/law-closures.ts" })
  if (!compiled.ok) throw new Error(JSON.stringify(compiled.diagnostics))
  const byId = new Map(compiled.artifacts.map((a) => [(a as { artifactId: string }).artifactId, a as { payload: Record<string, any> }]))
  const law = byId.get("books.JournalEntry.balanced.law")!.payload
  expect(law.condition).toBe("self.status == 'posted'")
  expect(law.response.expression).toBe("(((sum(map([{amount: self.debit, sku: self.debit_sku}, {amount: self.credit, sku: self.credit_sku}] as l, l.amount)) == 0) and all([{amount: self.debit, sku: self.debit_sku}, {amount: self.credit, sku: self.credit_sku}] as l, l.amount != 0)) and (not some([{amount: self.debit, sku: self.debit_sku}, {amount: self.credit, sku: self.credit_sku}] as l, startsWith(l.sku, 'VOID-') and some([1, 2] as p, l.amount == p)))) and (add_days(date(self.issue_date), 30) >= date(self.due_date))")
  const guard = byId.get("books.JournalEntry.entity")!.payload.objectType.lifecycles[0].transitions[0].guards[0]
  expect(guard).toBe("(count(self.lines as l, l.amount != 0) >= 2) and (days_between(date(self.issue_date), date(self.due_date)) <= 90)")
  // Each lambda maps back to its source line.
  expect(compiled.sourceMap.some((s) => s.artifactId === "books.JournalEntry.balanced.law" && s.pointer === "/payload/response/expression" && s.at.line === 48)).toBe(true)
  if (checker) {
    const checked = checker.checkArtifactSet(compiled.artifacts)
    expect(checked.diagnostics.diagnostics).toEqual([]); expect(checked.admitted).toBe(true)
  }
}, 30_000)

// A3 (#1176): law(name, { on, ... }) infers everything but the meaning.
describe("law v2", () => {
  const source = "corpus/accept/law-v2.ts"
  const compiled = compileDefinitions(join(TOOL_ROOT, source), { scope: options.scope, sourcePath: source })
  const laws = () => {
    if (!compiled.ok) throw new Error(JSON.stringify(compiled.diagnostics))
    return new Map(compiled.artifacts.filter((a) => (a as { kind: string }).kind === "law").map((a) => [(a as { artifactId: string }).artifactId, a as unknown as { payload: L.LawIR; dependencies: unknown; name: string; version: string }]))
  }

  test("ids, scope, owner, provenance, version and dependency are inferred", () => {
    const law = laws().get("books.Invoice.lines_add_up.law")!
    expect(law.payload).toEqual({
      lawVersion: "0.1", id: "books.Invoice.lines_add_up", owner: "books", severity: "error",
      rationale: "An issued invoice totals its net and tax.",
      provenance: { source: "authoring_sdk", sourcePath: source },
      scope: { root: { kind: "entity", id: "books.Invoice" }, objectTypes: ["books.Invoice"] },
      subject: "books.Invoice",
      condition: "self.status != 'draft'",
      modality: { kind: "always" },
      response: { kind: "predicate", expression: "(sum(map([{amount: self.net}, {amount: self.tax}] as l, l.amount)) == self.total) and all([{amount: self.net}, {amount: self.tax}] as l, l.amount > 0)" },
    })
    expect(law.dependencies).toEqual([{ artifactId: "books.Invoice.entity", kind: "entity", version: "0.1.0", required: true }])
    expect(law.name).toBe("An issued invoice totals its net and tax.")
    expect(law.version).toBe("0.1.0")
  })

  test("interface laws, raw temporal ordering, timing, severity and a kept id", () => {
    const all = laws()
    expect([...all.keys()].sort()).toEqual(["books.Invoice.due_after_issue.law", "books.Invoice.issued_promptly.law", "books.Invoice.lines_add_up.law", "books.Invoice.paid_in_time.law", "books.Payable.not_overpaid.law", "books.payment_terms.law"])
    const payable = all.get("books.Payable.not_overpaid.law")!.payload
    expect(payable.subject).toBe("books.Payable")
    expect(payable.scope).toEqual({ root: { kind: "entity", id: "books.Payable" }, objectTypes: ["books.Payable"] })
    expect(payable.response).toEqual({ kind: "predicate", expression: "self.paid <= self.total" })
    const due = all.get("books.Invoice.due_after_issue.law")!.payload
    expect(due.modality).toEqual({ kind: "never" })
    expect(due.severity).toBe("warning")
    expect(due.response).toEqual({ kind: "predicate", expression: "date(self.due_date) < date(self.issue_date)" })
    const paid = all.get("books.Invoice.paid_in_time.law")!.payload
    expect(paid.modality).toEqual({ kind: "eventually" })
    expect(paid.timing).toEqual({ kind: "before_deadline", property: "pay_by" })
    const terms = all.get("books.payment_terms.law")!.payload
    expect(terms.id).toBe("books.payment_terms")
    expect(terms.timing).toBeUndefined()
    expect(all.get("books.Invoice.issued_promptly.law")!.payload.timing).toEqual({ kind: "within", millis: "259200000" })
    expect(all.get("books.Payable.not_overpaid.law")!.dependencies).toEqual([{ artifactId: "books.Payable.interface", kind: "entity", version: "0.1.0", required: true }])
    expect(terms.response).toEqual({ kind: "predicate", expression: "days_between(date(self.issue_date), date(self.due_date)) <= 90" })
    if (!compiled.ok) return
    expect(compiled.sourceMap.find((s) => s.artifactId === "books.Invoice.lines_add_up.law" && s.pointer === "/payload/response/expression")?.at.line).toBe(50)
  })

  test("the Rust checker admits the v2 laws with their subjects", () => {
    if (!checker || !compiled.ok) return
    const checked = checker.checkArtifactSet(compiled.artifacts)
    expect(checked.diagnostics.diagnostics).toEqual([]); expect(checked.admitted).toBe(true)
  }, 30_000)

  const compileLaw = (spec: string, typeName = "books.Account") => {
    const dir = mkdtempSync(join(tmpdir(), "norn-law-v2-"))
    try {
      const file = join(dir, "law.ts")
      writeFileSync(file, `import { Schema } from "effect"
import { Norn } from "@norn/authoring"
export const Account = Norn.type({ name: "${typeName}", title: "Account", version: "0.1.0", properties: Schema.Struct({ account_id: Schema.String, balance: Schema.Int, opened: Norn.date(), closes_at: Norn.timestamp("utc") }), identity: [{ name: "primary", properties: ["account_id"] }] })
export const law = Norn.law("solvent", { on: Account, ${spec}, because: "x" })
`)
      return compileDefinitions(file, { scope: options.scope, sourcePath: "law.ts" })
    } finally { rmSync(dir, { recursive: true, force: true }) }
  }

  test("refusals name the construct at the author's line", () => {
    const refuse = (spec: string, typeName?: string): { codes: string[]; messages: string[]; line: number } => {
      const result = compileLaw(spec, typeName)
      if (result.ok) throw new Error(`accepted: ${spec}`)
      return { codes: result.diagnostics.map((d) => d.code), messages: result.diagnostics.map((d) => d.message), line: result.diagnostics[0]!.start.line }
    }
    expect(refuse("must: (self) => self.balance >= 0, never: (self) => self.balance < 0")).toMatchObject({ codes: expect.arrayContaining(["NORN-FLOW-TYPE"]), line: 4 })
    expect(refuse("must: (self) => self.missing >= 0")).toMatchObject({ codes: expect.arrayContaining(["NORN-FLOW-TYPE"]), line: 4 })
    expect(refuse("must: (self) => self.balance >= 0, within: { before: (self) => self.opened }")).toMatchObject({ codes: expect.arrayContaining(["NORN-FLOW-SLICE"]), line: 4 })
    expect(refuse("must: (self) => self.balance >= 0, within: { before: (self) => Norn.addDays(self.opened, 3) }")).toMatchObject({ codes: expect.arrayContaining(["NORN-FLOW-EXPRESSION"]), line: 4 })
    expect(refuse("must: (self) => self.balance >= 0, within: { days: 0 }")).toMatchObject({ codes: expect.arrayContaining(["NORN-FLOW-LITERAL"]), line: 4 })
    expect(refuse("must: (self) => self.account_id > \"a\"")).toMatchObject({ codes: expect.arrayContaining(["NORN-FLOW-EXPRESSION"]), line: 4 })
    // A date ordered against a timestamp is refused, not parsed to one kind (#1253 review).
    expect(refuse("must: (self) => self.opened < self.closes_at")).toMatchObject({ codes: expect.arrayContaining(["NORN-FLOW-TYPE"]), line: 4 })
    // Past 2^53 a Number round trip would round this down; the exact value is past u64 milliseconds.
    expect(refuse("must: (self) => self.balance >= 0, within: { days: 9007199254740993 }")).toMatchObject({ codes: expect.arrayContaining(["NORN-FLOW-LITERAL"]), line: 4 })
    // A subject without a namespace has no owner to infer.
    expect(refuse("must: (self) => self.balance >= 0", "Account").messages).toEqual(expect.arrayContaining([expect.stringContaining("law subject \"Account\" is not a qualified type name")]))
  })

  test("within keeps the literal's exact value up to u64 milliseconds", () => {
    const result = compileLaw("must: (self) => self.balance >= 0, within: { days: 213503982334 }")
    if (!result.ok) throw new Error(JSON.stringify(result.diagnostics))
    const law = result.artifacts.find((a) => (a as { kind: string }).kind === "law") as unknown as { payload: L.LawIR }
    expect(law.payload.timing).toEqual({ kind: "within", millis: "18446744073657600000" })
  })
})
