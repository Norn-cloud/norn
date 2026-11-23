import { expect, test } from "bun:test"
import { QueryClient, QueryClientProvider } from "@tanstack/react-query"
import { readFileSync } from "node:fs"
import { renderToStaticMarkup } from "react-dom/server"
import { ActionForm } from "../src/actions/action-form.tsx"
import type { ActionSummary } from "../src/lib/actions.ts"
import { readDocument } from "../src/lib/exact.ts"
import { decodeCuratedCase, decodeEvalSet, listCuratedCases, listEvalSets } from "../src/lib/learning/cases.ts"
import { decodeMisfitByLaw, decodeTriageOverTime, readMisfitByLaw, readTriageOverTime } from "../src/lib/learning/misfit.ts"
import { decodeGapProposal, listGapProposals, proposalFilterPredicate, proposalStatusFromServed, readGapProposal } from "../src/lib/learning/proposals.ts"
import { EMPTY_QUERY } from "../src/lib/records-query.ts"
import type { Answer, ObjectRecord } from "../src/lib/transport.ts"

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

// ---- OpenAPI contract checks --------------------------------------------------------------------

test("every curated case, eval set and gap proposal fixture matches NornObjectRecord in OpenAPI", () => {
  for (const record of [...casesFixture.cases, ...casesFixture.evalSets, ...proposalsFixture.records]) {
    assertMatchesSchema(record as unknown as Record<string, unknown>, "NornObjectRecord")
  }
})

test("learning.misfit_by_law and learning.triage_over_time fixtures match NornQueryResult, NornQueryRow and NornQueryMember in OpenAPI", () => {
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

// ---- Misfit board and triage over time (L-4) ----------------------------------------------------

test("misfit_by_law decodes exact percentages, preserves D53 suppression and rejects bad buckets", () => {
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
  expect(decoded.data.rows[2]!.override).toMatchObject({ suppressed: true, exact: null, percent: null, plotPercent: null })
  expect(decoded.data.rows[2]!.anySuppressed).toBe(true)

  // Single-key lawId grouping is also accepted.
  const singleKey = structuredClone(misfitFixture.misfitByLaw)
  singleKey.rows = {
    '"demo.credit.hold_over_limit"': { bucket: "demo.credit.hold_over_limit", totals: [1, 4], value: 0.25, members: { ep_1: { version: 1, amounts: [1, 1] } } },
  }
  const singleDecoded = decodeMisfitByLaw(readDocument<Answer>(JSON.stringify(singleKey), "live"))
  expect(singleDecoded.kind === "ok" && singleDecoded.data.rows[0]!.override?.percent).toBe("25.00%")

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

test("triage_over_time decodes weekly verdict counts, preserves D53 suppression and rejects invalid rows", () => {
  const read = readDocument<Answer>(JSON.stringify(misfitFixture.triageOverTime), "live")
  const decoded = decodeTriageOverTime(read)
  if (decoded.kind !== "ok") throw new Error(decoded.problem)
  expect(decoded.data.weeks.map((w) => [w.weekStart, w.total])).toEqual([
    ["2026-11-02", 12],
    ["2026-11-09", 14],
    ["2026-11-16", 14],
  ])

  const suppressed = structuredClone(misfitFixture.triageOverTime)
  suppressed.rows['["2026-11-16","gap"]'] = { bucket: ["2026-11-16", "gap"], totals: [], value: null, suppressed: true, members: {} } as never
  const supDecoded = decodeTriageOverTime(readDocument<Answer>(JSON.stringify(suppressed), "live"))
  if (supDecoded.kind !== "ok") throw new Error(supDecoded.problem)
  expect(supDecoded.data.weeks[2]!.counts.gap).toEqual({ exact: null, plot: null, suppressed: true })
  expect(supDecoded.data.weeks[2]!.total).toBeNull()

  const badVerdict = structuredClone(misfitFixture.triageOverTime)
  badVerdict.rows = { bad: { bucket: ["2026-11-16", "made_up"], totals: [1], value: 1, members: {} } }
  expect(decodeTriageOverTime(readDocument<Answer>(JSON.stringify(badVerdict), "live")).kind).toBe("malformed")

  const nullNonSuppressed = structuredClone(misfitFixture.triageOverTime)
  nullNonSuppressed.rows = { bad: { bucket: ["2026-11-16", "gap"], totals: [], value: null, members: {} } }
  expect(decodeTriageOverTime(readDocument<Answer>(JSON.stringify(nullNonSuppressed), "live")).kind).toBe("malformed")
})

test("learning question readers call the generic question answer route with scope and handle refusal, unavailable and no-cell", async () => {
  const ok = cell(() => Response.json(misfitFixture.misfitByLaw))
  expect((await readMisfitByLaw(ok.fetcher)).kind).toBe("ok")
  expect(ok.calls[0]!.pathname).toBe("/api/host/v1/questions/learning.misfit_by_law/answer")
  expect(Object.fromEntries(ok.calls[0]!.searchParams)).toEqual({ orgId: "org_demo", environmentId: "analytics" })

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
  const casesPage = cell(() => Response.json({ objectType: "learning.CuratedCase", records: casesFixture.cases, total: { count: 2, approximate: false }, next: null, quarantineCount: 0 }))
  const cRes = await listCuratedCases(EMPTY_QUERY, casesPage.fetcher)
  expect(cRes.kind === "page" && cRes.rows.length).toBe(2)
  expect(casesPage.calls[0]!.pathname).toBe("/api/host/v1/objects/learning.CuratedCase")

  const setsPage = cell(() => Response.json({ objectType: "learning.EvalSet", records: casesFixture.evalSets, total: { count: 2, approximate: false }, next: null, quarantineCount: 0 }))
  const sRes = await listEvalSets(EMPTY_QUERY, setsPage.fetcher)
  expect(sRes.kind === "page" && sRes.rows.length).toBe(2)
  expect(setsPage.calls[0]!.pathname).toBe("/api/host/v1/objects/learning.EvalSet")

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
