import { expect, test } from "bun:test"
import { QueryClient, QueryClientProvider } from "@tanstack/react-query"
import { GitCompareArrows } from "lucide-react"
import { readFileSync } from "node:fs"
import { renderToStaticMarkup } from "react-dom/server"
import { ActionForm } from "../src/actions/action-form.tsx"
import { LearningGate } from "../src/learning/gate.tsx"
import { listActions } from "../src/lib/actions.ts"
import { type ActionSummary } from "../../../packages/norn-sdk/src/index.ts"
import type { StudioScope } from "../src/lib/definitions.ts"
import { readDocument } from "../src/lib/exact.ts"
import { isSectionVisible, learningCapabilitiesOf } from "../src/lib/learning/capabilities.ts"
import { decodeCuratedCase, decodeEvalSet, listCuratedCases, listEvalSets } from "../src/lib/learning/cases.ts"
import { listEpisodes, readEpisode } from "../src/lib/learning/episode.ts"
import {
  compareExactDecimal, compareMisfitRows, compareRateFigures, decodeMisfitByLaw, decodeTriageOverTime,
  parseExactDecimal, parseExactNonNegInt, readMisfitByLaw, readTriageOverTime, topRateRow,
  type MisfitLawRow, type RateFigure,
} from "../src/lib/learning/misfit.ts"
import { decodeGapProposal, listGapProposals, proposalFilterPredicate, proposalStatusFromServed, readGapProposal } from "../src/lib/learning/proposals.ts"
import type { Mode } from "../src/lib/live-transport.ts"
import { EMPTY_QUERY } from "../src/lib/records-query.ts"
import type { Answer, Artifact, ObjectRecord } from "../src/lib/transport.ts"

const casesFixture = JSON.parse(readFileSync(new URL("./fixtures/learning-cases.json", import.meta.url), "utf8")) as {
  readonly cases: readonly ObjectRecord[]
  readonly evalSets: readonly ObjectRecord[]
}
const misfitRaw = readFileSync(new URL("./fixtures/learning-misfit.json", import.meta.url), "utf8")
const misfitFixture = JSON.parse(misfitRaw) as { readonly misfitByLaw: Answer; readonly triageOverTime: Answer }
const proposalsFixture = JSON.parse(readFileSync(new URL("./fixtures/learning-proposals.json", import.meta.url), "utf8")) as {
  readonly records: readonly ObjectRecord[]
}

interface OpenApiSchema {
  readonly type?: string | readonly string[]
  readonly enum?: readonly string[]
  readonly required?: readonly string[]
  readonly properties?: Readonly<Record<string, { readonly type?: string | readonly string[]; readonly enum?: readonly string[]; readonly $ref?: string }>>
  readonly additionalProperties?: boolean | object
}
const openapi = JSON.parse(readFileSync(new URL("../../../sdks/openapi/norn-api.openapi.json", import.meta.url), "utf8")) as {
  readonly components: { readonly schemas: Readonly<Record<string, OpenApiSchema>> }
}

function checkType(value: unknown, type: string | readonly string[]): boolean {
  const allowed = Array.isArray(type) ? type : [type]
  return allowed.some((t) => {
    if (t === "null") return value === null
    if (t === "string") return typeof value === "string"
    if (t === "boolean") return typeof value === "boolean"
    if (t === "integer") return typeof value === "number" && Number.isSafeInteger(value)
    if (t === "number") return typeof value === "number" && Number.isFinite(value)
    if (t === "array") return Array.isArray(value)
    if (t === "object") return typeof value === "object" && value !== null && !Array.isArray(value)
    return false
  })
}

function assertMatchesSchema(doc: Record<string, unknown>, schemaName: string) {
  const schema = openapi.components.schemas[schemaName]!
  expect(schema, `schema ${schemaName}`).toBeDefined()
  for (const key of schema.required ?? []) expect(doc, `${schemaName} requires ${key}`).toHaveProperty(key)
  if (schema.additionalProperties === false) {
    for (const key of Object.keys(doc)) expect(Object.keys(schema.properties ?? {}), `${schemaName} allows ${key}`).toContain(key)
  }
  for (const [key, property] of Object.entries(schema.properties ?? {})) {
    if (!(schema.required ?? []).includes(key) && !(key in doc)) continue
    const val = doc[key]
    if (property.type !== undefined) expect(checkType(val, property.type), `${schemaName}.${key}`).toBe(true)
    if (property.$ref !== undefined) {
      const refName = property.$ref.split("/").pop()!
      const refSchema = openapi.components.schemas[refName]!
      if (refSchema.enum !== undefined) expect(refSchema.enum, `${schemaName}.${key}`).toContain(val as string)
      else if (refSchema.type === "string") expect(typeof val, `${schemaName}.${key}`).toBe("string")
      else if (refSchema.properties !== undefined) assertMatchesSchema(val as Record<string, unknown>, refName)
    }
  }
}

function leaves(value: unknown, at = ""): [string, unknown][] {
  if (Array.isArray(value)) return value.flatMap((v, i) => leaves(v, `${at}/${i}`))
  if (typeof value === "object" && value !== null) return Object.entries(value).flatMap(([k, v]) => leaves(v, `${at}/${k}`))
  return [[at, value]]
}

function replaced(record: ObjectRecord, pointer: string, value: unknown): ObjectRecord {
  const copy = structuredClone(record) as unknown as Record<string, unknown>
  const parts = pointer.split("/").slice(1)
  let node = copy as Record<string, unknown>
  for (const part of parts.slice(0, -1)) node = node[part] as Record<string, unknown>
  node[parts.at(-1)!] = value
  return copy as unknown as ObjectRecord
}

const ME = { user: { id: "u" }, organization: { id: "org_demo", name: "Demo", role: "member" }, environmentId: "analytics" }
function cell(host: (url: URL) => Response, me: unknown = ME) {
  const calls: URL[] = []
  const fetcher = async (input: string) => {
    const url = new URL(input, "https://studio.test")
    if (url.pathname === "/api/me") return Response.json(me)
    calls.push(url)
    return host(url)
  }
  return { calls, fetcher }
}

const DEFAULT_SCOPE: StudioScope = { orgId: "org_demo", environmentId: "analytics" }

function liveMode(
  objectTypes: readonly string[],
  publishedQuestions: readonly string[] = [],
  retiredTypes: readonly string[] = [],
  scope: StudioScope = DEFAULT_SCOPE,
): Mode {
  const artifacts = [
    ...objectTypes.map((name) => ({ artifactId: `${name}.entity`, kind: "entity", lifecycle: "active", payload: { objectType: { name } } })),
    ...retiredTypes.map((name) => ({ artifactId: `${name}.entity`, kind: "entity", lifecycle: "retired", payload: { objectType: { name } } })),
  ] as unknown as readonly Artifact[]
  return {
    kind: "live",
    scope,
    catalog: { document: artifacts, numbers: new Map(), origin: "live" },
    counts: {},
    publishedQuestions: new Set(publishedQuestions),
  }
}

// ---- S1 Capability gating -----------------------------------------------------------------------

test("capability gating fails closed on missing, pending, failed, recorded, partial, and mismatched-scope catalogs", () => {
  expect(learningCapabilitiesOf(undefined, { isPending: true })).toMatchObject({ status: "pending", scope: null, episodes: false, cases: false, misfit: false, proposals: false })
  expect(learningCapabilitiesOf(undefined, { isError: true })).toMatchObject({ status: "error", scope: null, episodes: false, cases: false, misfit: false, proposals: false })
  expect(learningCapabilitiesOf({ kind: "recorded" })).toMatchObject({ status: "unavailable", scope: null, episodes: false, cases: false, misfit: false, proposals: false })
  expect(learningCapabilitiesOf(liveMode([]))).toMatchObject({ status: "ready", scope: DEFAULT_SCOPE, episodes: false, cases: false, misfit: false, proposals: false })

  // Same org, different environment: cached mode from env-a MUST fail closed when expectedScope is env-b.
  const modeA = liveMode(
    ["learning.Episode", "learning.CuratedCase", "learning.EvalSet", "learning.GapProposal"],
    ["learning.misfit_by_law", "learning.triage_over_time"],
    [],
    { orgId: "org_demo", environmentId: "env-a" },
  )
  expect(learningCapabilitiesOf(modeA, { expectedScope: { orgId: "org_demo", environmentId: "env-b" } })).toMatchObject({
    status: "pending",
    scope: null,
    episodes: false,
    cases: false,
    misfit: false,
    proposals: false,
  })
  expect(learningCapabilitiesOf(modeA, { expectedScope: null })).toMatchObject({
    status: "pending",
    scope: null,
    episodes: false,
  })
  expect(learningCapabilitiesOf(modeA, { expectedScope: { orgId: "org_demo", environmentId: "env-a" } })).toMatchObject({
    status: "ready",
    scope: { orgId: "org_demo", environmentId: "env-a" },
    episodes: true,
    cases: true,
    misfit: true,
    proposals: true,
  })

  // Retired entities do not count as advertised.
  expect(learningCapabilitiesOf(liveMode([], [], ["learning.Episode"]))).toMatchObject({ episodes: false })

  // Cases requires BOTH CuratedCase and EvalSet; Misfit requires BOTH published questions.
  const partial = learningCapabilitiesOf(liveMode(["learning.Episode", "learning.CuratedCase"], ["learning.misfit_by_law"]))
  expect(partial).toMatchObject({ episodes: true, cases: false, misfit: false, proposals: false })
  expect(isSectionVisible("/learning/episodes", partial)).toBe(true)
  expect(isSectionVisible("/learning/cases", partial)).toBe(false)
  expect(isSectionVisible("/learning/misfit", partial)).toBe(false)
  expect(isSectionVisible("/learning/proposals", partial)).toBe(false)
  expect(isSectionVisible("/metrics", partial)).toBe(true)

  const full = learningCapabilitiesOf(
    liveMode(["learning.Episode", "learning.CuratedCase", "learning.EvalSet", "learning.GapProposal"], ["learning.misfit_by_law", "learning.triage_over_time"]),
  )
  expect(full).toMatchObject({ status: "ready", episodes: true, cases: true, misfit: true, proposals: true })
  for (const path of ["/learning/episodes", "/learning/cases", "/learning/misfit", "/learning/proposals"]) {
    expect(isSectionVisible(path, full)).toBe(true)
  }

  // LearningGate renders the unavailable state and never mounts children when capability is pending.
  let mounted = false
  const Child = () => { mounted = true; return <div>secret child</div> }
  const html = renderToStaticMarkup(
    <QueryClientProvider client={new QueryClient()}>
      <LearningGate surface="episodes" icon={GitCompareArrows} title="Episodes"><Child /></LearningGate>
    </QueryClientProvider>,
  )
  expect(mounted).toBe(false)
  expect(html).toContain("Unavailable in this workspace")
  expect(html).not.toContain("secret child")
})

test("scope-bound Learning readers refuse to hit cell B when /api/me moved to a new environment (S1)", async () => {
  const scopeA: StudioScope = { orgId: "org_demo", environmentId: "env-a" }
  const scopeB: StudioScope = { orgId: "org_demo", environmentId: "env-b" }
  const mismatches: (StudioScope | null)[] = []
  const moved = cell(
    () => { throw new Error("must not send any Learning request to env-b before catalog") },
    { ...ME, environmentId: "env-b" },
  )
  const opts = { scope: scopeA, onScopeMismatch: (actual: StudioScope | null) => { mismatches.push(actual) } }

  expect(await listEpisodes(EMPTY_QUERY, moved.fetcher, opts)).toEqual({ kind: "scope-changed" })
  expect(await readEpisode("ep_1", moved.fetcher, opts)).toEqual({ kind: "scope-changed" })
  expect(await readMisfitByLaw(moved.fetcher, opts)).toEqual({ kind: "scope-changed" })
  expect(await readTriageOverTime(moved.fetcher, opts)).toEqual({ kind: "scope-changed" })
  expect(await listActions("learning.Episode", moved.fetcher, undefined, opts)).toEqual({ kind: "scope-changed" })
  expect(moved.calls).toEqual([])
  expect(mismatches).toEqual([scopeB, scopeB, scopeB, scopeB, scopeB])
})

// ---- OpenAPI generic envelope checks (pack `values`/`bucket` remain provisional until backend fixtures land) ----

test("every curated case, eval set and gap proposal fixture matches the generic NornObjectRecord envelope in OpenAPI (values remain provisional)", () => {
  for (const record of [...casesFixture.cases, ...casesFixture.evalSets, ...proposalsFixture.records]) {
    assertMatchesSchema(record as unknown as Record<string, unknown>, "NornObjectRecord")
  }
})

test("learning.misfit_by_law and learning.triage_over_time fixtures match the generic NornQueryResult/Row/Member envelopes in OpenAPI (buckets remain provisional)", () => {
  for (const answer of [misfitFixture.misfitByLaw, misfitFixture.triageOverTime]) {
    assertMatchesSchema(answer as unknown as Record<string, unknown>, "NornQueryResult")
    for (const row of Object.values(answer.rows)) {
      assertMatchesSchema(row as unknown as Record<string, unknown>, "NornQueryRow")
      for (const member of Object.values(row.members)) {
        assertMatchesSchema(member as unknown as Record<string, unknown>, "NornQueryMember")
      }
    }
  }
})

// ---- Curated cases and eval sets (L-3) ----------------------------------------------------------

test("curated cases and eval sets decode fail-closed on wrong-kind leaves and seal contradictions", () => {
  const cases = casesFixture.cases.map((r) => { const d = decodeCuratedCase(r); if (d.kind !== "ok") throw new Error(d.problem); return d.item })
  expect(cases.map((c) => c.protected)).toEqual([true, false])
  for (const record of casesFixture.cases) {
    for (const [pointer, value] of leaves(record)) {
      if (pointer === "/scope" || value === null) continue
      const wrong = typeof value === "boolean" ? "not-a-bool" : true
      expect({ pointer, kind: decodeCuratedCase(replaced(record, pointer, wrong)).kind }).toEqual({ pointer, kind: "malformed" })
    }
  }

  const sets = casesFixture.evalSets.map((r) => { const d = decodeEvalSet(r); if (d.kind !== "ok") throw new Error(d.problem); return d.item })
  expect(sets.map((s) => s.status)).toEqual(["unsealed", "sealed"])
  for (const record of casesFixture.evalSets) {
    for (const [pointer, value] of leaves(record)) {
      if (pointer === "/scope" || value === null) continue
      const wrong = typeof value === "boolean" ? "not-a-bool" : true
      expect({ pointer, kind: decodeEvalSet(replaced(record, pointer, wrong)).kind }).toEqual({ pointer, kind: "malformed" })
    }
  }
  const [unsealed, sealed] = casesFixture.evalSets as [ObjectRecord, ObjectRecord]
  expect(decodeEvalSet(replaced(sealed, "/values/sealedAtMs", null)).kind).toBe("malformed")
  expect(decodeEvalSet(replaced(sealed, "/values/sealedBy", null)).kind).toBe("malformed")
  expect(decodeEvalSet(replaced(unsealed, "/values/sealedAtMs", 1234)).kind).toBe("malformed")
  expect(decodeEvalSet(replaced(unsealed, "/values/setVersion", 0)).kind).toBe("malformed")
  expect(decodeEvalSet(replaced(unsealed, "/objectType", "learning.CuratedCase")).kind).toBe("malformed")
})

// ---- Misfit board and triage over time (L-4, S5 exactness & S6 strict validation) ---------------

test("exact decimal, rational, and BigInt helpers compare and parse without floating-point loss (S5)", () => {
  expect(compareExactDecimal(parseExactDecimal("0.41701")!, parseExactDecimal("0.41704")!)).toBeLessThan(0)
  expect(compareExactDecimal(parseExactDecimal("0.41704")!, parseExactDecimal("0.41701")!)).toBeGreaterThan(0)
  expect(compareExactDecimal(parseExactDecimal("0.41700")!, parseExactDecimal("0.417")!)).toBe(0)
  expect(compareExactDecimal(parseExactDecimal("4.1704e-1")!, parseExactDecimal("0.41701")!)).toBeGreaterThan(0)
  expect(parseExactDecimal("1e50000")).toBeNull()

  expect(parseExactNonNegInt("0")).toBe(0n)
  expect(parseExactNonNegInt("12.00")).toBe(12n)
  expect(parseExactNonNegInt("9007199254740993")).toBe(9007199254740993n)
  expect(parseExactNonNegInt("-1")).toBeNull()
  expect(parseExactNonNegInt("1.5")).toBeNull()
  expect(parseExactNonNegInt("NaN")).toBeNull()

  // Exact rational tie-breaker when rounded decimal values match — consistent with rows lacking totals.
  const oneThird = { exact: "0.333333", percent: "33.33%", plotPercent: 33.3, numerator: "1", denominator: "3", suppressed: false }
  const noTotals = { exact: "0.333333", percent: "33.33%", plotPercent: 33.3, numerator: null, denominator: null, suppressed: false }
  const truncated = { exact: "0.333333", percent: "33.33%", plotPercent: 33.3, numerator: "333333", denominator: "1000000", suppressed: false }
  expect(compareRateFigures(oneThird, truncated)).toBeGreaterThan(0)
  expect(compareRateFigures(oneThird, noTotals)).toBeGreaterThan(0)
  expect(compareRateFigures(noTotals, truncated)).toBe(0)
})

function permutations<T>(items: readonly T[]): T[][] {
  if (items.length <= 1) return [[...items]]
  const out: T[][] = []
  for (let i = 0; i < items.length; i += 1) {
    const rest = [...items.slice(0, i), ...items.slice(i + 1)]
    for (const tail of permutations(rest)) out.push([items[i]!, ...tail])
  }
  return out
}

test("misfit rate ordering and KPI selection form a strict permutation-invariant total order across rows with and without totals (S5)", () => {
  // Exact 3-row counterexample from Sol round 2: z (1/3), m (no totals), a (333333/1000000), all decimal 0.333333.
  const threeEntries: readonly [string, object][] = [
    ['["z","override"]', { bucket: ["z", "override"], totals: [1, 3], value: 0.333333, members: {} }],
    ['["m","override"]', { bucket: ["m", "override"], totals: [], value: 0.333333, members: {} }],
    ['["a","override"]', { bucket: ["a", "override"], totals: [333333, 1000000], value: 0.333333, members: {} }],
  ]
  for (const perm of permutations(threeEntries)) {
    const doc = { ...misfitFixture.misfitByLaw, rows: Object.fromEntries(perm) }
    const decoded = decodeMisfitByLaw(readDocument<Answer>(JSON.stringify(doc), "live"))
    if (decoded.kind !== "ok") throw new Error(decoded.problem)
    expect(decoded.data.rows.map((r) => r.lawId)).toEqual(["z", "a", "m"])
    expect(topRateRow(decoded.data.rows, (r) => r.override)?.lawId).toBe("z")
  }

  // Property check over all 120 permutations of 5 mixed rows (totals > decimal, == decimal, < decimal, no totals, suppressed).
  const fiveEntries: readonly [string, object][] = [
    ['["law.z","override"]', { bucket: ["law.z", "override"], totals: [1, 3], value: 0.333333, members: {} }],
    ['["law.m","override"]', { bucket: ["law.m", "override"], totals: [], value: 0.333333, members: {} }],
    ['["law.a","override"]', { bucket: ["law.a", "override"], totals: [333333, 1000000], value: 0.333333, members: {} }],
    ['["law.b","override"]', { bucket: ["law.b", "override"], totals: [5, 12], value: 0.417, members: {} }],
    ['["law.c","override"]', { bucket: ["law.c", "override"], totals: [], value: null, suppressed: true, members: {} }],
  ]
  const expectedOrder = ["law.b", "law.z", "law.a", "law.m", "law.c"]
  const allPermutations = permutations(fiveEntries)
  expect(allPermutations).toHaveLength(120)
  for (const perm of allPermutations) {
    const doc = { ...misfitFixture.misfitByLaw, rows: Object.fromEntries(perm) }
    const decoded = decodeMisfitByLaw(readDocument<Answer>(JSON.stringify(doc), "live"))
    if (decoded.kind !== "ok") throw new Error(decoded.problem)
    expect(decoded.data.rows.map((r) => r.lawId)).toEqual(expectedOrder)
    expect(topRateRow(decoded.data.rows, (r) => r.override)?.lawId).toBe("law.b")
  }

  // Exhaustive reflexivity, antisymmetry, and transitivity across all triples of RateFigures & MisfitLawRows.
  const figures: readonly (RateFigure | null)[] = [
    null,
    { exact: null, percent: null, plotPercent: null, numerator: null, denominator: null, suppressed: true },
    { exact: null, percent: null, plotPercent: null, numerator: "0", denominator: "0", suppressed: false },
    { exact: "0.333333", percent: "33.33%", plotPercent: 33.3, numerator: "3333329", denominator: "10000000", suppressed: false },
    { exact: "0.333333", percent: "33.33%", plotPercent: 33.3, numerator: null, denominator: null, suppressed: false },
    { exact: "0.333333", percent: "33.33%", plotPercent: 33.3, numerator: "333333", denominator: "1000000", suppressed: false },
    { exact: "0.333333", percent: "33.33%", plotPercent: 33.3, numerator: "1", denominator: "3", suppressed: false },
    { exact: "0.41701", percent: "41.70%", plotPercent: 41.7, numerator: null, denominator: null, suppressed: false },
    { exact: "0.41704", percent: "41.70%", plotPercent: 41.7, numerator: "41704", denominator: "100000", suppressed: false },
  ]
  const sign = (n: number) => (n < 0 ? -1 : n > 0 ? 1 : 0)
  for (const x of figures) {
    expect(compareRateFigures(x, x)).toBe(0)
    for (const y of figures) {
      expect(sign(compareRateFigures(x, y)) + sign(compareRateFigures(y, x))).toBe(0)
      for (const z of figures) {
        const xy = compareRateFigures(x, y)
        const yz = compareRateFigures(y, z)
        const xz = compareRateFigures(x, z)
        if (xy <= 0 && yz <= 0) expect(xz).toBeLessThanOrEqual(0)
        if (xy === 0 && yz === 0) expect(xz).toBe(0)
      }
    }
  }
  const sampleRows: readonly MisfitLawRow[] = figures.map((fig, i) => ({
    lawId: `law_${i}`,
    override: fig,
    exception: figures[(i + 3) % figures.length]!,
    episodeCount: "10",
    memberIds: [],
    anySuppressed: fig?.suppressed ?? false,
  }))
  for (const x of sampleRows) {
    expect(compareMisfitRows(x, x)).toBe(0)
    for (const y of sampleRows) {
      expect(sign(compareMisfitRows(x, y)) + sign(compareMisfitRows(y, x))).toBe(0)
      for (const z of sampleRows) {
        if (compareMisfitRows(x, y) <= 0 && compareMisfitRows(y, z) <= 0) {
          expect(compareMisfitRows(x, z)).toBeLessThanOrEqual(0)
        }
      }
    }
  }
})

test("misfit_by_law ranks by exact ratios (0.41704 beats 0.41701), keeps BigInt episode counts, preserves D53 suppression and rejects bad envelopes/buckets (S5, S6)", () => {
  const read = readDocument<Answer>(JSON.stringify(misfitFixture.misfitByLaw), "live")
  const decoded = decodeMisfitByLaw(read)
  if (decoded.kind !== "ok") throw new Error(decoded.problem)
  expect(decoded.data.rows.map((r) => r.lawId)).toEqual([
    "demo.credit.hold_over_limit",
    "demo.discount.max_margin",
    "demo.refund.window_days",
  ])
  expect(decoded.data.rows[0]!.override).toMatchObject({ exact: "0.417", percent: "41.70%", numerator: "5", denominator: "12", suppressed: false })
  expect(decoded.data.rows[0]!.exception).toMatchObject({ exact: "0.25", percent: "25.00%", numerator: "3", denominator: "12", suppressed: false })
  expect(decoded.data.rows[0]!.episodeCount).toBe("12")
  expect(decoded.data.rows[2]!.override).toMatchObject({ suppressed: true, exact: null, percent: null, plotPercent: null })
  expect(decoded.data.rows[2]!.anySuppressed).toBe(true)

  // S5: 0.41701 vs 0.41704 tie at 41.7% in chart plotPercent, yet 0.41704 must rank strictly ahead.
  // Also verifies denominator beyond Number.MAX_SAFE_INTEGER preserves exact BigInt digits.
  const closeRatesJson = JSON.stringify({
    ...misfitFixture.misfitByLaw,
    rows: {
      '["a.law","override"]': { bucket: ["a.law", "override"], totals: [41701, 100000], value: 0.41701, members: {} },
      '["z.law","override"]': { bucket: ["z.law", "override"], totals: [41704, 100000], value: 0.41704, members: {} },
    },
  }).replace("100000", "9007199254740993")
  const closeDecoded = decodeMisfitByLaw(readDocument<Answer>(closeRatesJson, "live"))
  if (closeDecoded.kind !== "ok") throw new Error(closeDecoded.problem)
  expect(closeDecoded.data.rows.map((r) => r.lawId)).toEqual(["z.law", "a.law"])
  expect(closeDecoded.data.rows[0]!.override?.plotPercent).toBe(41.7)
  expect(closeDecoded.data.rows[1]!.override?.plotPercent).toBe(41.7)
  expect(closeDecoded.data.rows[1]!.episodeCount).toBe("9007199254740993")

  // Single-key lawId grouping is also accepted.
  const singleKey = structuredClone(misfitFixture.misfitByLaw)
  singleKey.rows = {
    '"demo.credit.hold_over_limit"': { bucket: "demo.credit.hold_over_limit", totals: [1, 4], value: 0.25, members: { ep_1: { version: 1, amounts: [1, 1] } } },
  }
  const singleDecoded = decodeMisfitByLaw(readDocument<Answer>(JSON.stringify(singleKey), "live"))
  expect(singleDecoded.kind === "ok" && singleDecoded.data.rows[0]!.override?.percent).toBe("25.00%")

  // S6: Missing exclusions, invalid engine, non-boolean complete, contradictory suppressed row, negative/fractional counts all fail closed (never throw).
  const noExclusions = structuredClone(misfitFixture.misfitByLaw) as unknown as Record<string, unknown>
  delete noExclusions.exclusions
  expect(decodeMisfitByLaw(readDocument<Answer>(JSON.stringify(noExclusions), "live")).kind).toBe("malformed")

  const badEngine = { ...misfitFixture.misfitByLaw, engine: "unknown" }
  expect(decodeMisfitByLaw(readDocument<Answer>(JSON.stringify(badEngine), "live")).kind).toBe("malformed")

  const badSuppressed = structuredClone(misfitFixture.misfitByLaw)
  badSuppressed.rows = { bad: { bucket: ["demo.law", "override"], totals: [1, 2], value: 0.5, suppressed: true, members: {} } as never }
  expect(decodeMisfitByLaw(readDocument<Answer>(JSON.stringify(badSuppressed), "live")).kind).toBe("malformed")

  const fractionalDen = structuredClone(misfitFixture.misfitByLaw)
  fractionalDen.rows = { bad: { bucket: ["demo.law", "override"], totals: [1, 2.5], value: 0.4, members: {} } }
  expect(decodeMisfitByLaw(readDocument<Answer>(JSON.stringify(fractionalDen), "live")).kind).toBe("malformed")

  // S6: Fractional/negative/contradictory memberCount is rejected even when a denominator is present or row is suppressed.
  const fractionalMemberWithDen = structuredClone(misfitFixture.misfitByLaw)
  fractionalMemberWithDen.rows = {
    bad: { bucket: ["demo.law", "override"], totals: [5, 12], value: 0.417, memberCount: 1.5, members: {} } as never,
  }
  expect(decodeMisfitByLaw(readDocument<Answer>(JSON.stringify(fractionalMemberWithDen), "live")).kind).toBe("malformed")

  const fractionalMemberNoDen = structuredClone(misfitFixture.misfitByLaw)
  fractionalMemberNoDen.rows = {
    bad: { bucket: ["demo.law", "override"], totals: [], value: 0.5, memberCount: 1.5, members: {} } as never,
  }
  expect(decodeMisfitByLaw(readDocument<Answer>(JSON.stringify(fractionalMemberNoDen), "live")).kind).toBe("malformed")

  const fractionalMemberSuppressed = structuredClone(misfitFixture.misfitByLaw)
  fractionalMemberSuppressed.rows = {
    bad: { bucket: ["demo.law", "override"], totals: [], value: null, suppressed: true, memberCount: 1.5, members: {} } as never,
  }
  expect(decodeMisfitByLaw(readDocument<Answer>(JSON.stringify(fractionalMemberSuppressed), "live")).kind).toBe("malformed")

  const tooSmallMemberCount = structuredClone(misfitFixture.misfitByLaw)
  tooSmallMemberCount.rows = {
    bad: {
      bucket: ["demo.law", "override"],
      totals: [1, 2],
      value: 0.5,
      memberCount: 1,
      members: { ep_1: { version: 1, amounts: [1, 1] }, ep_2: { version: 1, amounts: [0, 1] } },
    } as never,
  }
  expect(decodeMisfitByLaw(readDocument<Answer>(JSON.stringify(tooSmallMemberCount), "live")).kind).toBe("malformed")

  const zeroDenWithValue = structuredClone(misfitFixture.misfitByLaw)
  zeroDenWithValue.rows = {
    bad: { bucket: ["demo.law", "override"], totals: [0, 0], value: 0, members: {} },
  }
  expect(decodeMisfitByLaw(readDocument<Answer>(JSON.stringify(zeroDenWithValue), "live")).kind).toBe("malformed")

  // Valid exact BigInt memberCount (> MAX_SAFE_INTEGER) is accepted.
  const bigMemberJson = JSON.stringify({
    ...misfitFixture.misfitByLaw,
    rows: { ok: { bucket: ["demo.law", "override"], totals: [], value: 0.5, memberCount: 7, members: {} } },
  }).replace('"memberCount":7', '"memberCount":9007199254740993')
  const bigMemberDecoded = decodeMisfitByLaw(readDocument<Answer>(bigMemberJson, "live"))
  expect(bigMemberDecoded.kind === "ok" && bigMemberDecoded.data.rows[0]!.episodeCount).toBe("9007199254740993")

  // Invalid bucket and duplicate law+signal fail closed.
  const badBucket = structuredClone(misfitFixture.misfitByLaw)
  badBucket.rows = { bad: { bucket: ["demo.credit", "unknown_signal"], totals: [1, 2], value: 0.5, members: {} } }
  expect(decodeMisfitByLaw(readDocument<Answer>(JSON.stringify(badBucket), "live")).kind).toBe("malformed")

  const dup = structuredClone(misfitFixture.misfitByLaw)
  dup.rows = {
    a: { bucket: ["demo.law", "override"], totals: [1, 2], value: 0.5, members: {} },
    b: { bucket: ["demo.law", "override"], totals: [1, 4], value: 0.25, members: {} },
  }
  expect(decodeMisfitByLaw(readDocument<Answer>(JSON.stringify(dup), "live")).kind).toBe("malformed")
})

test("triage_over_time sums exact BigInt totals, rejects duplicates & invalid counts, and treats omitted buckets as unknown (S5, S6)", () => {
  const read = readDocument<Answer>(JSON.stringify(misfitFixture.triageOverTime), "live")
  const decoded = decodeTriageOverTime(read)
  if (decoded.kind !== "ok") throw new Error(decoded.problem)
  expect(decoded.data.weeks.map((w) => [w.weekStart, w.total])).toEqual([
    ["2026-11-02", "12"],
    ["2026-11-09", "14"],
    ["2026-11-16", "14"],
  ])

  // S5: Weekly total sums exact BigInt tokens beyond Number.MAX_SAFE_INTEGER.
  const bigJson = JSON.stringify({
    ...misfitFixture.triageOverTime,
    rows: {
      a: { bucket: ["2026-11-02", "exception"], totals: [1], value: 1, members: {} },
      b: { bucket: ["2026-11-02", "noise"], totals: [2], value: 2, members: {} },
      c: { bucket: ["2026-11-02", "pattern"], totals: [3], value: 3, members: {} },
      d: { bucket: ["2026-11-02", "gap"], totals: [4], value: 4, members: {} },
    },
  }).replaceAll(":1,", ":9007199254740993,").replaceAll("[1]", "[9007199254740993]")
  const bigDecoded = decodeTriageOverTime(readDocument<Answer>(bigJson, "live"))
  if (bigDecoded.kind !== "ok") throw new Error(bigDecoded.problem)
  expect(bigDecoded.data.weeks[0]!.counts.exception.exact).toBe("9007199254740993")
  expect(bigDecoded.data.weeks[0]!.total).toBe("9007199254741002")

  // Suppressed bucket stays explicitly suppressed and blanks the week total.
  const suppressed = structuredClone(misfitFixture.triageOverTime)
  suppressed.rows['["2026-11-16","gap"]'] = { bucket: ["2026-11-16", "gap"], totals: [], value: null, suppressed: true, members: {} } as never
  const supDecoded = decodeTriageOverTime(readDocument<Answer>(JSON.stringify(suppressed), "live"))
  if (supDecoded.kind !== "ok") throw new Error(supDecoded.problem)
  expect(supDecoded.data.weeks[2]!.counts.gap).toEqual({ status: "suppressed", exact: null, plot: null, suppressed: true })
  expect(supDecoded.data.weeks[2]!.total).toBeNull()

  // S6: Omitted bucket stays explicitly unknown (never coerced to "0"), and week total is null.
  const missingBucket = structuredClone(misfitFixture.triageOverTime)
  missingBucket.complete = false
  delete missingBucket.rows['["2026-11-16","gap"]']
  const missingDecoded = decodeTriageOverTime(readDocument<Answer>(JSON.stringify(missingBucket), "live"))
  if (missingDecoded.kind !== "ok") throw new Error(missingDecoded.problem)
  expect(missingDecoded.data.weeks[2]!.counts.gap).toEqual({ status: "unknown", exact: null, plot: null, suppressed: false })
  expect(missingDecoded.data.weeks[2]!.anyUnknown).toBe(true)
  expect(missingDecoded.data.weeks[2]!.total).toBeNull()

  // S6: Duplicate [weekStart, verdict] fails closed — including when an earlier row was suppressed!
  const dupSuppressed = structuredClone(misfitFixture.triageOverTime)
  dupSuppressed.rows = {
    first: { bucket: ["2026-11-16", "gap"], totals: [], value: null, suppressed: true, members: {} } as never,
    second: { bucket: ["2026-11-16", "gap"], totals: [5], value: 5, members: {} },
  }
  expect(decodeTriageOverTime(readDocument<Answer>(JSON.stringify(dupSuppressed), "live")).kind).toBe("malformed")

  const dupNumeric = structuredClone(misfitFixture.triageOverTime)
  dupNumeric.rows = {
    first: { bucket: ["2026-11-16", "gap"], totals: [2], value: 2, members: {} },
    second: { bucket: ["2026-11-16", "gap"], totals: [3], value: 3, members: {} },
  }
  expect(decodeTriageOverTime(readDocument<Answer>(JSON.stringify(dupNumeric), "live")).kind).toBe("malformed")

  // S6: Invalid counts, calendar dates, verdict names, missing exclusions, and null non-suppressed fail closed.
  const badVerdict = structuredClone(misfitFixture.triageOverTime)
  badVerdict.rows = { bad: { bucket: ["2026-11-16", "made_up"], totals: [1], value: 1, members: {} } }
  expect(decodeTriageOverTime(readDocument<Answer>(JSON.stringify(badVerdict), "live")).kind).toBe("malformed")

  const badDate = structuredClone(misfitFixture.triageOverTime)
  badDate.rows = { bad: { bucket: ["2026-02-31", "gap"], totals: [1], value: 1, members: {} } }
  expect(decodeTriageOverTime(readDocument<Answer>(JSON.stringify(badDate), "live")).kind).toBe("malformed")

  const fractionalCount = structuredClone(misfitFixture.triageOverTime)
  fractionalCount.rows = { bad: { bucket: ["2026-11-16", "gap"], totals: [1.5], value: 1.5, members: {} } }
  expect(decodeTriageOverTime(readDocument<Answer>(JSON.stringify(fractionalCount), "live")).kind).toBe("malformed")

  // S6: Fractional memberCount on a triage row is rejected.
  const fractionalTriageMember = structuredClone(misfitFixture.triageOverTime)
  fractionalTriageMember.rows = { bad: { bucket: ["2026-11-16", "gap"], totals: [4], value: 4, memberCount: 1.5, members: {} } as never }
  expect(decodeTriageOverTime(readDocument<Answer>(JSON.stringify(fractionalTriageMember), "live")).kind).toBe("malformed")

  // S6: Extra operands in totals (e.g. value: 7 with totals: [0, 8]) or contradicting single total are rejected.
  const extraTriageTotals = structuredClone(misfitFixture.triageOverTime)
  extraTriageTotals.rows = { bad: { bucket: ["2026-11-16", "gap"], totals: [0, 8], value: 7, members: {} } }
  expect(decodeTriageOverTime(readDocument<Answer>(JSON.stringify(extraTriageTotals), "live")).kind).toBe("malformed")

  const extraMatchingTotals = structuredClone(misfitFixture.triageOverTime)
  extraMatchingTotals.rows = { bad: { bucket: ["2026-11-16", "gap"], totals: [7, 8], value: 7, members: {} } }
  expect(decodeTriageOverTime(readDocument<Answer>(JSON.stringify(extraMatchingTotals), "live")).kind).toBe("malformed")

  const contradictingTotal = structuredClone(misfitFixture.triageOverTime)
  contradictingTotal.rows = { bad: { bucket: ["2026-11-16", "gap"], totals: [8], value: 7, members: {} } }
  expect(decodeTriageOverTime(readDocument<Answer>(JSON.stringify(contradictingTotal), "live")).kind).toBe("malformed")

  const emptyTotalsTriage = structuredClone(misfitFixture.triageOverTime)
  emptyTotalsTriage.rows = { ok: { bucket: ["2026-11-16", "gap"], totals: [], value: 7, members: {} } }
  const emptyTotalsDecoded = decodeTriageOverTime(readDocument<Answer>(JSON.stringify(emptyTotalsTriage), "live"))
  expect(emptyTotalsDecoded.kind === "ok" && emptyTotalsDecoded.data.weeks[0]!.counts.gap.exact).toBe("7")

  const negativeCount = structuredClone(misfitFixture.triageOverTime)
  negativeCount.rows = { bad: { bucket: ["2026-11-16", "gap"], totals: [-1], value: -1, members: {} } }
  expect(decodeTriageOverTime(readDocument<Answer>(JSON.stringify(negativeCount), "live")).kind).toBe("malformed")

  const nullNonSuppressed = structuredClone(misfitFixture.triageOverTime)
  nullNonSuppressed.rows = { bad: { bucket: ["2026-11-16", "gap"], totals: [], value: null, members: {} } }
  expect(decodeTriageOverTime(readDocument<Answer>(JSON.stringify(nullNonSuppressed), "live")).kind).toBe("malformed")

  const noExclusions = structuredClone(misfitFixture.triageOverTime) as unknown as Record<string, unknown>
  delete noExclusions.exclusions
  expect(decodeTriageOverTime(readDocument<Answer>(JSON.stringify(noExclusions), "live")).kind).toBe("malformed")
})

test("learning question readers call the generic question answer route with scope and handle malformed/refusal/unavailable/no-cell", async () => {
  const ok = cell(() => Response.json(misfitFixture.misfitByLaw))
  expect((await readMisfitByLaw(ok.fetcher)).kind).toBe("ok")
  expect(ok.calls[0]!.pathname).toBe("/api/host/v1/questions/learning.misfit_by_law/answer")
  expect(Object.fromEntries(ok.calls[0]!.searchParams)).toEqual({ orgId: "org_demo", environmentId: "analytics" })

  // Missing exclusions returns malformed instead of throwing through readMisfitByLaw.
  const missingExclusions = structuredClone(misfitFixture.misfitByLaw) as unknown as Record<string, unknown>
  delete missingExclusions.exclusions
  expect((await readMisfitByLaw(cell(() => Response.json(missingExclusions)).fetcher)).kind).toBe("malformed")

  const denied = cell(() => Response.json({ code: "authz.denied", message: "Not allowed to read learning metrics" }, { status: 403 }))
  expect(await readTriageOverTime(denied.fetcher)).toEqual({
    kind: "refused",
    refusal: { code: "authz.denied", message: "Not allowed to read learning metrics" },
  })
  expect(await readMisfitByLaw(cell(() => new Response("not found", { status: 404 })).fetcher)).toEqual({ kind: "unavailable" })
  const none = cell(() => { throw new Error("no call") }, { ...ME, environmentId: null })
  expect(await readMisfitByLaw(none.fetcher)).toEqual({ kind: "no-cell" })
  expect(none.calls).toHaveLength(0)
})

// ---- Gap proposals and replay table (L-5) -------------------------------------------------------

test("gap proposals decode fail-closed on wrong-kind leaves and replay/ruling contradictions", () => {
  const proposals = proposalsFixture.records.map((r) => { const d = decodeGapProposal(r); if (d.kind !== "ok") throw new Error(d.problem); return d.item })
  expect(proposals.map((p) => p.status)).toEqual(["refused", "admitted", "draft", "ratified"])
  expect(proposals[0]!.replay?.protectedFlippedCount).toBe(1)
  expect(proposals[0]!.replay?.admissionStatus).toBe("refused")

  for (const record of proposalsFixture.records) {
    for (const [pointer, value] of leaves(record)) {
      if (pointer === "/scope" || value === null) continue
      const wrong = typeof value === "boolean" ? "not-a-bool" : true
      expect({ pointer, kind: decodeGapProposal(replaced(record, pointer, wrong)).kind }).toEqual({ pointer, kind: "malformed" })
    }
  }

  const [refused, admitted, draft, ratified] = proposalsFixture.records as [ObjectRecord, ObjectRecord, ObjectRecord, ObjectRecord]
  // Protected-case admission law: claiming admitted when an unsuperseded protected case flips is rejected fail-closed.
  expect(decodeGapProposal(replaced(refused, "/values/replay/admissionStatus", "admitted")).kind).toBe("malformed")
  // Flipped flag contradicting beforeOutcome/afterOutcome is rejected.
  expect(decodeGapProposal(replaced(refused, "/values/replay/cases/0/flipped", false)).kind).toBe("malformed")
  // Flipped count contradicting cases list is rejected.
  expect(decodeGapProposal(replaced(refused, "/values/replay/flippedCount", 0)).kind).toBe("malformed")
  // Protected flipped count mismatch is rejected.
  expect(decodeGapProposal(replaced(refused, "/values/replay/protectedFlippedCount", 0)).kind).toBe("malformed")
  // Ratified without a ruling receipt (or non-ratified with a ruling receipt) is rejected.
  expect(decodeGapProposal(replaced(ratified, "/values/ruling", null)).kind).toBe("malformed")
  expect(decodeGapProposal(replaced(draft, "/values/ruling", (ratified.values as Record<string, unknown>).ruling)).kind).toBe("malformed")
  // Wrong objectType is rejected.
  expect(decodeGapProposal(replaced(admitted, "/objectType", "learning.Episode")).kind).toBe("malformed")

  // Status filter round-trip.
  expect(proposalFilterPredicate(undefined)).toBeNull()
  const pred = proposalFilterPredicate("refused")
  expect(pred).toEqual({ all: [{ property: "status", op: "eq", value: "refused" }] })
  expect(proposalStatusFromServed({ sort: [], filter: pred })).toBe("refused")
})

test("cases, eval sets and proposal readers call generic object routes and keep unavailable/no-cell apart", async () => {
  const casesPage = cell(() => Response.json({ objectType: "learning.CuratedCase", records: casesFixture.cases, total: { count: 2, approximate: false }, next: "c_next", quarantineCount: 0 }))
  const cRes = await listCuratedCases({ ...EMPTY_QUERY, after: "c_prev" }, casesPage.fetcher)
  expect(cRes.kind === "page" && cRes.rows.length).toBe(2)
  expect(cRes.kind === "page" && cRes.next).toBe("c_next")
  expect(casesPage.calls[0]!.pathname).toBe("/api/host/v1/objects/learning.CuratedCase")
  expect(casesPage.calls[0]!.searchParams.get("after")).toBe("c_prev")

  const setsPage = cell(() => Response.json({ objectType: "learning.EvalSet", records: casesFixture.evalSets, total: { count: 2, approximate: false }, next: "s_next", quarantineCount: 0 }))
  const sRes = await listEvalSets({ ...EMPTY_QUERY, after: "s_prev" }, setsPage.fetcher)
  expect(sRes.kind === "page" && sRes.rows.length).toBe(2)
  expect(sRes.kind === "page" && sRes.next).toBe("s_next")
  expect(setsPage.calls[0]!.pathname).toBe("/api/host/v1/objects/learning.EvalSet")
  expect(setsPage.calls[0]!.searchParams.get("after")).toBe("s_prev")

  const propPage = cell(() => Response.json({ objectType: "learning.GapProposal", records: proposalsFixture.records, total: { count: 4, approximate: false }, next: null, quarantineCount: 0 }))
  const pRes = await listGapProposals(EMPTY_QUERY, propPage.fetcher)
  expect(pRes.kind === "page" && pRes.rows.length).toBe(4)
  expect(propPage.calls[0]!.pathname).toBe("/api/host/v1/objects/learning.GapProposal")

  const one = cell(() => Response.json(proposalsFixture.records[0]))
  expect((await readGapProposal("prop_demo_credit_tighten", one.fetcher)).kind).toBe("ok")
  expect(one.calls[0]!.pathname).toBe("/api/host/v1/objects/learning.GapProposal/prop_demo_credit_tighten")

  expect(await listGapProposals(EMPTY_QUERY, cell(() => Response.json({ code: "object.type_unknown" }, { status: 404 })).fetcher)).toEqual({ kind: "unavailable" })
  const none = cell(() => { throw new Error("no call") }, { ...ME, environmentId: null })
  expect(await listCuratedCases(EMPTY_QUERY, none.fetcher)).toEqual({ kind: "no-cell" })
  expect(none.calls).toHaveLength(0)
})

// ---- L-i18n enum labels in ActionForm (#1220) ---------------------------------------------------

test("ActionForm renders L-i18n enum labels while preserving raw wire values and falling back when unlabelled", () => {
  const action: ActionSummary = {
    actionId: "learning.triage_episode",
    title: "Triage",
    description: "Triage one episode.",
    objectKind: "learning.Episode",
    effect: "fire_transition",
    objectIdField: "episodeId",
    uiEffects: { irreversible: false, destructive: false, money: [] },
    inputSchema: {
      type: "object",
      additionalProperties: false,
      required: ["episodeId", "verdict"],
      properties: {
        episodeId: { type: "string", title: "Episode" },
        verdict: { type: "string", title: "Verdict", enum: ["exception", "noise", "pattern", "gap"] },
      },
    },
  }
  const labels: Readonly<Record<string, string>> = {
    "/verdict:pattern": "Reusable pattern",
    "/verdict:gap": "System gap",
  }
  const html = renderToStaticMarkup(
    <QueryClientProvider client={new QueryClient()}>
      <ActionForm action={action} objectTitle="Episode" prefill={{ episodeId: "ep_1" }} enumLabel={(pointer, value) => labels[`${pointer}:${value}`]} />
    </QueryClientProvider>,
  )
  // Labelled values show the served label text while keeping the raw enum id in `value="..."`.
  expect(html).toContain('<option value="pattern">Reusable pattern</option>')
  expect(html).toContain('<option value="gap">System gap</option>')
  // Unlabelled values fall back to the raw id without inventing words.
  expect(html).toContain('<option value="exception">exception</option>')
  expect(html).toContain('<option value="noise">noise</option>')
})
