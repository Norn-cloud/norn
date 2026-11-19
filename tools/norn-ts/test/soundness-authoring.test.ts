// LIFE-SOUND authoring (#1126, #1176 unit B): a lifecycle in machine shape, with endings,
// facts, deadlines, external events and compensations, and a law with labelled exceptions,
// lower to today's IR; the checker proves no dead ends and reports planted ones by code.
import { expect, test } from "bun:test"
import { mkdtempSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { compileDefinitions } from "../src/compiler/definitions.ts"
import { checker, CHECKER_PKG_ENV } from "./checker.ts"
if (!checker && process.env.CI) throw new Error(`${CHECKER_PKG_ENV} is required in CI`)

const SOURCE = `// Intent: a lifecycle in machine shape with outcome-classified endings, a fact,
// a deadline, an external event and a compensation (LIFE-SOUND-1..8, #1126), and
// a law with labelled exceptions (unless, #1126 R4). Everything lowers to
// today's IR; the checker proves no dead ends.
import { Schema } from "effect"
import { Norn } from "@norn/authoring"

export const Order = Norn.type({
  name: "shop.Order",
  title: "Order",
  version: "0.1.0",
  properties: Schema.Struct({
    order_id: Schema.String,
    total: Schema.Int,
    credit_limit: Schema.Int,
    prepaid: Schema.Boolean,
    tier: Schema.Literals(["standard", "vip"]),
    status: Schema.Literals(["draft", "confirmed", "refunding", "expired", "shipped", "cancelled", "refunded"]),
  }),
  identity: [{ name: "primary", properties: ["order_id"] }],
  lifecycles: [{
    property: "status",
    initial: "draft",
    events: { carrier_lost: { ignore: "nothing is in transit" }, carrier_ping: { ignore: "telemetry only" } },
    states: {
      draft: { on: { confirm: { to: "confirmed", guards: [Norn.expression(Schema.Struct({ total: Schema.Int }), (o) => o.total > 0)] }, discard: "cancelled" } },
      confirmed: {
        on: { ship: { to: "shipped", mode: "fact" }, cancel: "refunding", carrier_lost: "cancelled" },
        after: { days: 30, to: "expired", id: "expire" },
      },
      refunding: { on: { refund: "refunded" } },
      expired: { on: { refund: "refunded" } },
      shipped: { final: "completed", immutable: true },
      cancelled: { final: "cancelled" },
      refunded: { final: "compensated" },
    },
  }],
})

export const creditLimit = Norn.law("credit_limit", {
  on: Order,
  when: (o) => o.status === "confirmed",
  must: (o) => o.total <= o.credit_limit,
  unless: {
    vip: { when: (o) => o.tier === "vip", because: "Board resolution 12", unless: { huge: { when: (o) => o.total > 1000000, because: "No exception covers a single order this large" } } },
    prepaid: { when: (o) => o.prepaid, because: "No credit risk" },
  },
  because: "Exposure stays within the customer's limit.",
})
`
const scope = { orgId: "org_demo", environmentId: "dev" }

const compile = (source: string) => {
  const dir = mkdtempSync(join(tmpdir(), "norn-sound-"))
  try {
    const file = join(dir, "shop.ts")
    writeFileSync(file, source)
    return compileDefinitions(file, { scope, sourcePath: "shop.ts" })
  } finally { rmSync(dir, { recursive: true, force: true }) }
}
type Artifact = { kind: string; artifactId: string; payload: Record<string, any> }
const artifacts = (source: string): Artifact[] => {
  const result = compile(source)
  if (!result.ok) throw new Error(JSON.stringify(result.diagnostics))
  return result.artifacts as unknown as Artifact[]
}
const codes = (list: Artifact[]): string[][] => {
  if (!checker) return []
  return checker.checkArtifactSet(list).diagnostics.diagnostics.map((d: { code: string; severity: string }) => [d.code, d.severity])
}

test("a machine lowers to one transition per event, with endings, triggers, modes and ignores", () => {
  const order = artifacts(SOURCE).find((a) => a.artifactId === "shop.Order.entity")!
  expect(order.payload.objectType.lifecycles).toEqual([{
    property: "status",
    initial: ["draft"],
    terminal: ["shipped", "cancelled", "refunded"],
    immutable: ["shipped"],
    transitions: [
      { id: "confirm", from: ["draft"], to: "confirmed", guards: ["self.total > 0"] },
      { id: "discard", from: ["draft"], to: "cancelled" },
      { id: "ship", from: ["confirmed"], to: "shipped", mode: "fact" },
      { id: "cancel", from: ["confirmed"], to: "refunding" },
      { id: "carrier_lost", from: ["confirmed"], to: "cancelled", trigger: { kind: "event", event: "carrier_lost" } },
      { id: "expire", from: ["confirmed"], to: "expired", trigger: { kind: "timer", millis: "2592000000" } },
      { id: "refund", from: ["refunding", "expired"], to: "refunded" },
    ],
    endings: [{ state: "shipped", outcome: "completed" }, { state: "cancelled", outcome: "cancelled" }, { state: "refunded", outcome: "compensated" }],
    events: [
      { id: "carrier_lost", ignoredIn: [{ states: ["draft", "refunding", "expired"], reason: "nothing is in transit" }] },
      { id: "carrier_ping", ignoredIn: [{ states: ["draft", "confirmed", "refunding", "expired"], reason: "telemetry only" }] },
    ],
  }])
})

test("unless lowers to a labelled exception tree whose sources are its own slots", () => {
  const law = artifacts(SOURCE).find((a) => a.artifactId === "shop.Order.credit_limit.law")!
  expect(law.payload.exceptions).toEqual([
    { id: "vip", overrides: "shop.Order.credit_limit", condition: "self.tier == 'vip'", rationale: "Board resolution 12", source: { artifactId: "shop.Order.credit_limit.law", pointer: "/payload/exceptions/0" } },
    { id: "huge", overrides: "vip", condition: "self.total > 1000000", rationale: "No exception covers a single order this large", source: { artifactId: "shop.Order.credit_limit.law", pointer: "/payload/exceptions/1" } },
    { id: "prepaid", overrides: "shop.Order.credit_limit", condition: "self.prepaid", rationale: "No credit risk", source: { artifactId: "shop.Order.credit_limit.law", pointer: "/payload/exceptions/2" } },
  ])
})

test("the checker admits the sound machine and its exception tree", () => {
  if (!checker) return
  expect(codes(artifacts(SOURCE))).toEqual([])
  // With discard guarded too, draft reaches an ending only through guards: Unknown, not a pass.
  const guarded = SOURCE.replace(`discard: "cancelled"`, `discard: { to: "cancelled", guards: [Norn.expression(Schema.Struct({ total: Schema.Int }), (o) => o.total === 0)] }`)
  expect(codes(artifacts(guarded))).toEqual([["lifecycle.soundness.completion_unknown", "info"]])
}, 30_000)

test("planted dead ends come back with the checker's codes", () => {
  if (!checker) return
  // refunding and expired only lead to each other: both have an exit, neither completes.
  const trap = SOURCE
    .replace(`refunding: { on: { refund: "refunded" } }`, `refunding: { on: { hold: "expired" } }`)
    .replace(`expired: { on: { refund: "refunded" } }`, `expired: { on: { resume: "refunding" } }`)
  const trapped = codes(artifacts(trap)).map(([code]) => code)
  expect(trapped).toContain("lifecycle.soundness.trap")
  // confirm dispatches an effect; a lost carrier cancels without the refund.
  const skip = SOURCE.replace(`confirm: { to: "confirmed",`, `confirm: { to: "confirmed", compensatedBy: "refund",`)
  expect(codes(artifacts(skip)).map(([code]) => code)).toContain("lifecycle.soundness.compensation_not_closed")
  // an event nobody handles in a live state.
  const unhandled = SOURCE.replace(`carrier_ping: { ignore: "telemetry only" }`, `carrier_ping: {}`)
  expect(codes(artifacts(unhandled)).map(([code]) => code)).toContain("lifecycle.soundness.unhandled_event")
  // a guarded fact could be refused.
  const guardedFact = SOURCE.replace(`ship: { to: "shipped", mode: "fact" }`, `ship: { to: "shipped", mode: "fact", guards: [Norn.expression(Schema.Struct({ total: Schema.Int }), (o) => o.total > 0)] }`)
  expect(codes(artifacts(guardedFact)).map(([code]) => code)).toContain("lifecycle.soundness.fact_guarded")
}, 30_000)

test("one event that moves differently from two states is refused at compile time", () => {
  const conflict = SOURCE.replace(`refunding: { on: { refund: "refunded" } }`, `refunding: { on: { refund: "refunded", cancel: "cancelled" } }`)
  const result = compile(conflict)
  expect(result.ok).toBe(false)
  if (!result.ok) expect(result.diagnostics.map((d) => d.code)).toContain("NORN-MACHINE-EVENT-CONFLICT")
})

test("exception labels are unique across the tree", () => {
  const twice = SOURCE.replace(`unless: { huge:`, `unless: { prepaid:`)
  const result = compile(twice)
  expect(result.ok).toBe(false)
  if (!result.ok) expect(result.diagnostics.map((d) => d.message).join("\n")).toContain("used twice")
})

const NORMS = `import { Schema } from "effect"
import { Norn } from "@norn/authoring"

export const Sale = Norn.type({
  name: "shop.Sale",
  title: "Sale",
  version: "0.1.0",
  properties: Schema.Struct({ cash: Schema.Boolean, large: Schema.Boolean, exempt: Schema.Boolean, reported: Schema.Boolean }),
})

export const regulatorExemption = Norn.permission("regulator_exemption", {
  on: Sale,
  when: (s) => s.exempt,
  may: (s) => s.cash,
  cite: { authority: "regulator", version: "art-9" },
  because: "The regulator exempts listed merchants.",
})

export const noCashOverLimit = Norn.prohibition("no_cash_over_limit", {
  on: Sale,
  never: (s) => s.cash,
  cite: { authority: "credit_policy", version: "2026" },
  yieldsTo: [regulatorExemption],
  because: "Large sales are not paid in cash.",
})

export const reportLarge = Norn.obligation("report_large", {
  on: Sale,
  when: (s) => s.large,
  must: (s) => s.reported,
  cite: { authority: "credit_policy", version: "2026" },
  unless: { exempt: { when: (s) => s.exempt, because: "Exempt merchants report yearly" } },
  because: "Large sales are reported.",
})
`

test("norms lower into the subject's normative slot with cited authority, precedence and exceptions", () => {
  const sale = artifacts(NORMS).find((a) => a.artifactId === "shop.Sale.entity")!
  const rules = sale.payload.extensions["norn.normative/1.0.0"] as Array<Record<string, any>>
  expect(rules.map((r) => [r.modality, r.law.id, r.law.modality.kind, r.authorityId, r.authorityVersion, r.source.pointer, r.defeatedBy ?? []])).toEqual([
    ["permission", "shop.Sale.regulator_exemption", "always", "regulator", "art-9", "/payload/extensions/norn.normative~11.0.0/0", []],
    ["prohibition", "shop.Sale.no_cash_over_limit", "never", "credit_policy", "2026", "/payload/extensions/norn.normative~11.0.0/1", ["shop.Sale.regulator_exemption"]],
    ["obligation", "shop.Sale.report_large", "always", "credit_policy", "2026", "/payload/extensions/norn.normative~11.0.0/2", []],
  ])
  expect(rules[0]!.law.condition).toBe("self.exempt")
  expect(rules.map((r) => r.law.response.expression)).toEqual(["self.cash", "self.cash", "self.reported"])
  expect(rules[2]!.law.condition).toBe("self.large")
  expect(rules[2]!.law.exceptions).toEqual([{ id: "exempt", overrides: "shop.Sale.report_large", condition: "self.exempt", rationale: "Exempt merchants report yearly", source: { artifactId: "shop.Sale.entity", pointer: "/payload/extensions/norn.normative~11.0.0/2/law/exceptions/0" } }])
  for (const rule of rules) expect(rule.law.scope).toEqual({ root: { kind: "entity", id: "shop.Sale" }, objectTypes: ["shop.Sale"] })
  // No norm becomes an artifact of its own.
  expect(artifacts(NORMS).map((a) => a.artifactId)).toEqual(["shop.Sale.entity"])
})

test("the checker admits a prohibition that yields to a permission, and names its own fragment limit", () => {
  if (!checker) return
  // Declared-order admission grounds norms over one closed Boolean subject and one shared
  // response; the cash pair is inside that fragment.
  const pair = NORMS.slice(0, NORMS.indexOf("export const reportLarge"))
  const admitted = checker.checkArtifactSet(artifacts(pair))
  expect(admitted.diagnostics.diagnostics).toEqual([])
  expect(admitted.admitted).toBe(true)
  // Without yieldsTo the pair clashes, and admission says so.
  const clash = checker.checkArtifactSet(artifacts(pair.replace("  yieldsTo: [regulatorExemption],\n", "")))
  expect(clash.admitted).toBe(false)
  // A third norm with another response is outside the grounded fragment: refused, never passed.
  const mixed = checker.checkArtifactSet(artifacts(NORMS))
  expect(mixed.admitted).toBe(false)
  expect(mixed.diagnostics.diagnostics.map((d: { code: string }) => d.code)).toContain("consistency.diagnosis.unknown")
}, 30_000)

test("a norm over a type from another module is refused", () => {
  const dir = mkdtempSync(join(tmpdir(), "norn-norm-"))
  try {
    writeFileSync(join(dir, "types.ts"), NORMS.slice(0, NORMS.indexOf("export const regulatorExemption")))
    writeFileSync(join(dir, "norms.ts"), `import { Norn } from "@norn/authoring"
import { Sale } from "./types.ts"
export const big = Norn.obligation("big", { on: Sale, must: (s) => s.reported, cite: { authority: "a", version: "1" }, because: "x" })
`)
    const result = compileDefinitions(join(dir, "norms.ts"), { scope, sourcePath: "norms.ts" })
    expect(result.ok).toBe(false)
    if (!result.ok) expect(result.diagnostics.map((d) => d.code)).toContain("NORN-FLOW-MODULE")
  } finally { rmSync(dir, { recursive: true, force: true }) }
})

// TABLE-UNIQUE-1: a unique table's rows must be proved disjoint by the checker.
const table = (rows: string, input = `Schema.Struct({ tier: Schema.Literals(["a", "b", "c"]), vip: Schema.Boolean })`, hit = "unique") => `import { Schema } from "effect"
import { Norn } from "@norn/authoring"
export const Tier = Norn.decision({ id: "shop.rule.tier", title: "Tier", version: "0.1.0", contract: "sub_decision", scopeRequired: ["orgId", "environmentId"], hit: "${hit}",
  input: ${input}, output: Schema.Struct({ r: Schema.String }) }, [
${rows}
])
`
const tableOf = (list: Artifact[]): Record<string, any> =>
  (list[0]!.payload.graph.nodes as Array<Record<string, any>>).find((n) => n.type === "decisionTableNode")!.content
const DISJOINT = `  { when: (i) => i.tier === "a", then: (i) => ({ r: "x" }) },
  { when: (i) => i.tier === "b", then: (i) => ({ r: "y" }) },
  { when: (i) => i.tier === "c", then: (i) => ({ r: "z" }) },`

test("a unique table's rows stand alone and are admitted when disjoint", () => {
  const list = artifacts(table(DISJOINT))
  const content = tableOf(list)
  expect(content.hitPolicy).toBe("unique")
  expect(content.rules.map((r: Record<string, string>) => r.condition)).toEqual(["tier == 'a'", "tier == 'b'", "tier == 'c'"])
  if (!checker) return
  expect(codes(list)).toEqual([])
}, 30_000)

test("overlapping unique rows are refused with a witness; an undecidable table is Unknown, refused", () => {
  if (!checker) return
  const overlapping = checker.checkArtifactSet(artifacts(table(`  { when: (i) => i.tier === "a", then: (i) => ({ r: "x" }) },
  { when: (i) => i.vip, then: (i) => ({ r: "y" }) },`))).diagnostics.diagnostics as ReadonlyArray<{ readonly code: string; readonly message: string }>
  const overlap = overlapping.find((d) => d.code === "rule.validation.jdm_unique_overlap")
  expect(overlap?.message ?? JSON.stringify(overlapping)).toContain("row_0")
  expect(overlap?.message ?? "").toContain("\"vip\":true")
  // The same rows under first are ordered, not refused.
  expect(codes(artifacts(table(`  { when: (i) => i.tier === "a", then: (i) => ({ r: "x" }) },
  { when: (i) => i.vip, then: (i) => ({ r: "y" }) },`, undefined, "first"))).map(([code]) => code)).not.toContain("rule.validation.jdm_unique_overlap")
  const unbounded = codes(artifacts(table(`  { when: (i) => i.amount > 100, then: (i) => ({ r: "x" }) },
  { when: (i) => i.amount <= 100, then: (i) => ({ r: "y" }) },`, `Schema.Struct({ amount: Schema.Int })`))).map(([code]) => code)
  expect(unbounded).toContain("rule.validation.jdm_unique_unproved")
}, 30_000)

test("a decision callback cannot be unique: its branches are ordered", () => {
  const callback = compile(`import { Schema } from "effect"
import { Norn } from "@norn/authoring"
export const Tier = Norn.decision({ id: "shop.rule.tier", title: "Tier", version: "0.1.0", contract: "sub_decision", scopeRequired: ["orgId", "environmentId"], hit: "unique",
  input: Schema.Struct({ vip: Schema.Boolean }), output: Schema.Struct({ r: Schema.String }) }, (i) => i.vip ? { r: "x" } : { r: "y" })
`)
  expect(callback.ok).toBe(false)
})

// LAW-EXAMPLE-1: examples run with the checker's law semantics.
const EXAMPLES = `
export const vipOverLimit = Norn.example("vip over limit is excepted", {
  of: creditLimit,
  given: { order_id: "o1", total: 900, credit_limit: 500, prepaid: false, tier: "vip", status: "confirmed" },
  expect: { excepted: ["vip"] },
})
export const hugeVip = Norn.example("a huge vip order is not excepted", {
  of: creditLimit,
  given: { order_id: "o2", total: 2000000, credit_limit: 500, prepaid: false, tier: "vip", status: "confirmed" },
  expect: "violated",
})
export const draftIgnored = Norn.example("a draft is not governed", {
  of: creditLimit,
  given: { order_id: "o3", total: 900, credit_limit: 500, prepaid: false, tier: "standard", status: "draft" },
  expect: "not_applicable",
})
`

test("examples compile to the law they name and pass with the checker's verdicts", async () => {
  const result = compile(SOURCE + EXAMPLES)
  if (!result.ok) throw new Error(JSON.stringify(result.diagnostics))
  expect(result.examples.map((e) => [e.name, e.artifactId, e.pointer])).toEqual([
    ["vip over limit is excepted", "shop.Order.credit_limit.law", "/payload"],
    ["a huge vip order is not excepted", "shop.Order.credit_limit.law", "/payload"],
    ["a draft is not governed", "shop.Order.credit_limit.law", "/payload"],
  ])
  if (!checker) return
  const { runExamples } = await import("../src/examples.ts")
  expect(runExamples(checker, result.artifacts, result.examples)).toEqual([])
  // A planted wrong expectation is reported with the checker's code and the example's line.
  const wrong = compile(SOURCE + EXAMPLES.replace(`expect: "violated",`, `expect: "holds",`))
  if (!wrong.ok) throw new Error(JSON.stringify(wrong.diagnostics))
  const failed = runExamples(checker, wrong.artifacts, wrong.examples)
  expect(failed.map((d) => d.code)).toEqual(["law.example.mismatch"])
  expect(failed[0]!.message).toContain("expected holds, the checker says violated")
  expect(failed[0]!.source?.line).toBeGreaterThan(0)
}, 30_000)

test("an example of a norm reads the norm's slot in its subject's artifact", async () => {
  const source = NORMS + `
export const exemptCash = Norn.example("exempt cash is permitted", { of: regulatorExemption, given: { cash: true, large: false, exempt: true, reported: false }, expect: "holds" })
export const unreported = Norn.example("a large sale is reported", { of: reportLarge, given: { cash: false, large: true, exempt: false, reported: false }, expect: "violated" })
`
  const result = compile(source)
  if (!result.ok) throw new Error(JSON.stringify(result.diagnostics))
  expect(result.examples.map((e) => e.pointer)).toEqual([
    "/payload/extensions/norn.normative~11.0.0/0/law",
    "/payload/extensions/norn.normative~11.0.0/2/law",
  ])
  if (!checker) return
  const { runExamples } = await import("../src/examples.ts")
  expect(runExamples(checker, result.artifacts, result.examples)).toEqual([])
}, 30_000)
