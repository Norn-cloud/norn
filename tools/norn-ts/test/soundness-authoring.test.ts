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
    status: Schema.Literals(["draft", "confirmed", "refunding", "shipped", "cancelled", "refunded"]),
  }),
  identity: [{ name: "primary", properties: ["order_id"] }],
  lifecycles: [{
    property: "status",
    initial: "draft",
    events: { carrier_lost: { ignore: "nothing is in transit" }, carrier_ping: { ignore: "telemetry only" } },
    states: {
      draft: { on: { confirm: { to: "confirmed", guards: [Norn.expression(Schema.Struct({ total: Schema.Int }), (o) => o.total > 0)] }, discard: "cancelled" } },
      confirmed: {
        on: { ship: { to: "shipped", mode: "fact" }, cancel: "refunding", carrier_lost: "refunding" },
        after: { days: 30, to: "refunding", id: "expire" },
      },
      refunding: { on: { refund: "refunded" } },
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
      { id: "carrier_lost", from: ["confirmed"], to: "refunding", trigger: { kind: "event", event: "carrier_lost" } },
      { id: "expire", from: ["confirmed"], to: "refunding", trigger: { kind: "timer", millis: "2592000000" } },
      { id: "refund", from: ["refunding"], to: "refunded" },
    ],
    endings: [{ state: "shipped", outcome: "completed" }, { state: "cancelled", outcome: "cancelled" }, { state: "refunded", outcome: "compensated" }],
    events: [
      { id: "carrier_lost", ignoredIn: [{ states: ["draft", "refunding"], reason: "nothing is in transit" }] },
      { id: "carrier_ping", ignoredIn: [{ states: ["draft", "confirmed", "refunding"], reason: "telemetry only" }] },
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
  // confirmed and refunding only lead to each other: every state has an exit, none completes.
  const trap = SOURCE
    .replace(`ship: { to: "shipped", mode: "fact" }, `, ``)
    .replace(`refunding: { on: { refund: "refunded" } }`, `refunding: { on: { reconsider: "confirmed" } }`)
  const trapped = codes(artifacts(trap)).map(([code]) => code)
  expect(trapped).toContain("lifecycle.soundness.trap")
  // cancelling straight to cancelled after confirm dispatched skips the refund.
  const skip = SOURCE
    .replace(`confirm: { to: "confirmed",`, `confirm: { to: "confirmed", compensatedBy: "refund",`)
    .replace(`cancel: "refunding"`, `cancel: "cancelled"`)
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
  properties: Schema.Struct({ sale_id: Schema.String, total: Schema.Int, cash: Schema.Boolean, exempt: Schema.Boolean, reported: Schema.Boolean }),
  identity: [{ name: "primary", properties: ["sale_id"] }],
})

export const regulatorExemption = Norn.permission("regulator_exemption", {
  on: Sale,
  may: (s) => s.exempt,
  cite: { authority: "regulator", version: "art-9" },
  because: "The regulator exempts listed merchants.",
})

export const noCashOverLimit = Norn.prohibition("no_cash_over_limit", {
  on: Sale,
  never: (s) => s.cash && s.total > 10000,
  cite: { authority: "credit_policy", version: "2026" },
  yieldsTo: [regulatorExemption],
  because: "Large sales are not paid in cash.",
})

export const reportLarge = Norn.obligation("report_large", {
  on: Sale,
  when: (s) => s.total > 100000,
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
  expect(rules[2]!.law.condition).toBe("self.total > 100000")
  expect(rules[2]!.law.exceptions).toEqual([{ id: "exempt", overrides: "shop.Sale.report_large", condition: "self.exempt", rationale: "Exempt merchants report yearly", source: { artifactId: "shop.Sale.entity", pointer: "/payload/extensions/norn.normative~11.0.0/2/law/exceptions/0" } }])
  for (const rule of rules) expect(rule.law.scope).toEqual({ root: { kind: "entity", id: "shop.Sale" }, objectTypes: ["shop.Sale"] })
  // No norm becomes an artifact of its own.
  expect(artifacts(NORMS).map((a) => a.artifactId)).toEqual(["shop.Sale.entity"])
})

test("the checker's verdict on the authored norms", () => {
  if (!checker) return
  // Printed so the admission result of each norm kind is visible in the test log.
  const verdict = checker.checkArtifactSet(artifacts(NORMS))
  console.log(JSON.stringify(verdict.diagnostics.diagnostics.map((d: { code: string; message: string }) => [d.code, d.message.slice(0, 160)])))
  expect(verdict.admitted).toBe(true)
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
