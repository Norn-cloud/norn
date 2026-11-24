import { describe, expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { join, relative } from "node:path"
import { compileDefinitionFiles } from "../src/compiler/definitions.ts"
import { runExamples } from "../src/examples.ts"
import { TOOL_ROOT } from "../src/profile.ts"
import { checker } from "./checker.ts"

const REPO = join(TOOL_ROOT, "../..")
const ORG_FILE = join(REPO, "registry/packs/org/org.ts")
const WORKFORCE_PACK_FILE = join(REPO, "registry/packs/org/workforce-canon.pack.ts")
const TIME_FILE = join(REPO, "registry/packs/time/calendars.ts")

function compileFoundation() {
  const files = [ORG_FILE, WORKFORCE_PACK_FILE, TIME_FILE]
  const results = compileDefinitionFiles(
    files,
    (path) => ({
      scope: {},
      sourcePath: relative(REPO, path),
      registryPackagePath: relative(REPO, join(path, "..")).split("\\").join("/"),
    }),
    {},
    { forbidUnsafeZen: true },
  )
  const artifacts: any[] = []
  const examples = []
  for (const result of results) {
    if (!result.ok) throw new Error(JSON.stringify(result.diagnostics, null, 2))
    expect(result.warnings).toEqual([])
    artifacts.push(...(result.artifacts as any[]))
    examples.push(...result.examples)
  }
  return { artifacts, examples }
}

function loadBase(): any[] {
  const core = join(REPO, "registry/core/artifacts")
  const names = [
    "core.Agent.interface.json",
    "core.AgentRole.interface.json",
    "core.TimeInterval.interface.json",
    "identity.BusinessUnit.entity.json",
    "identity.Directory.interface.json",
    "party.Organization.entity.json",
    "core.value_types.entity.json",
    "std.value_types.entity.json",
    "core.units.json",
    "core.time.json",
  ]
  return names.map((name) => JSON.parse(readFileSync(join(core, name), "utf8")))
}

/**
 * Closed-shape membership projection validator mirroring `crates/norn-checker/src/ontology.rs`
 * (`validate_instance_values`) for `term_ref` fields and closed field sets.
 */
function validateClosedProjection(
  packPayload: {
    readonly terms: ReadonlyArray<{ readonly termId: string; readonly status: string; readonly parentTermId?: string }>
    readonly shapes: ReadonlyArray<{
      readonly shapeId: string
      readonly termId: string
      readonly closed: boolean
      readonly fields: ReadonlyArray<{
        readonly fieldId: string
        readonly valueType: string
        readonly targetTermId?: string
        readonly minCount: number
        readonly maxCount: number
      }>
    }>
  },
  instance: { readonly termId: string; readonly values: Record<string, ReadonlyArray<string>> },
): string[] {
  const errors: string[] = []
  const byId = new Map(packPayload.terms.map((t) => [t.termId, t]))
  const isActiveMemberOf = (termId: string, targetTermId: string): boolean => {
    const start = byId.get(termId)
    const target = byId.get(targetTermId)
    if (!start || !target || start.status !== "active" || target.status !== "active") return false
    const seen = new Set<string>()
    let current: string | undefined = termId
    while (current) {
      if (current === targetTermId) return true
      if (seen.has(current)) return false
      seen.add(current)
      current = byId.get(current)?.parentTermId
    }
    return false
  }
  const shape = packPayload.shapes.find((s) => s.termId === instance.termId)
  if (!shape || !shape.closed) return [`no closed shape for ${instance.termId}`]
  const allowed = new Set(shape.fields.map((f) => f.fieldId))
  for (const fieldId of Object.keys(instance.values)) {
    if (!allowed.has(fieldId)) errors.push(`unknown field ${fieldId}`)
  }
  for (const field of shape.fields) {
    const values = instance.values[field.fieldId]
    if (!values) {
      if (field.minCount > 0) errors.push(`missing required field ${field.fieldId}`)
      continue
    }
    if (values.length < field.minCount || values.length > field.maxCount) {
      errors.push(`field ${field.fieldId} cardinality out of range`)
    }
    if (field.valueType === "term_ref" && field.targetTermId) {
      for (const ref of values) {
        if (!isActiveMemberOf(ref, field.targetTermId)) {
          errors.push(`field ${field.fieldId} value ${ref} is not an active member of ${field.targetTermId}`)
        }
      }
    }
  }
  return errors
}

describe("time & org foundation packs", () => {
  const { artifacts, examples } = compileFoundation()
  const byId = new Map(artifacts.map((a) => [a.artifactId as string, a]))

  test("org.OrgUnit narrows kind to organization and links to identity.BusinessUnit", () => {
    const unit = byId.get("org.OrgUnit.entity")!.payload.objectType
    const kind = unit.properties.find((p: any) => p.id === "kind")
    expect(kind.type).toEqual({ kind: "enum", values: [{ value: "organization" }] })
    expect(unit.properties.some((p: any) => p.id === "business_unit_code")).toBe(false)
    const buLink = unit.links.find((l: any) => l.id === "business_unit")
    expect(buLink).toMatchObject({
      id: "business_unit",
      target: "identity.BusinessUnit",
      cardinality: { to: "one", from: "many" },
    })
    const law = byId.get("org.OrgUnit.kind_is_organization.law")!.payload
    expect(law.response).toEqual({ kind: "predicate", expression: "self.kind == 'organization'" })
  })

  test("org.Membership requires valid_from and enforces valid_from < valid_to via law", () => {
    const membership = byId.get("org.Membership.entity")!.payload.objectType
    const validFrom = membership.properties.find((p: any) => p.id === "valid_from")
    const validTo = membership.properties.find((p: any) => p.id === "valid_to")
    expect(validFrom.type).toEqual({ kind: "ref", name: "iso8601.date" })
    expect(validFrom.constraints).toBeUndefined()
    expect(validTo.type).toEqual({ kind: "ref", name: "iso8601.date" })
    expect(validTo.constraints).toEqual({ nullable: true })
    const law = byId.get("org.Membership.valid_date_range.law")!.payload
    expect(law.response).toEqual({
      kind: "predicate",
      expression: "(self.valid_to == null) or (date(self.valid_from) < date(self.valid_to ?? self.valid_from))",
    })
  })

  test("time pack uses shared core.time.* value types, pins tzdb 2025b, and materializes core.TimeInterval via resolve_local_timestamp", () => {
    const calendar = byId.get("time.Calendar.entity")!.payload.objectType
    expect(calendar.properties.find((p: any) => p.id === "iana_timezone").type).toEqual({ kind: "ref", name: "core.time.iana_timezone" })
    expect(calendar.properties.find((p: any) => p.id === "dst_policy").type).toEqual({ kind: "ref", name: "core.time.dst_policy" })
    expect(calendar.properties.find((p: any) => p.id === "tzdb_version").type).toEqual({ kind: "enum", values: [{ value: "2025b" }] })
    expect(calendar.properties.find((p: any) => p.id === "tzdb_version").default).toBe("2025b")

    const exception = byId.get("time.CalendarException.entity")!.payload.objectType
    expect(exception.properties.find((p: any) => p.id === "calendar_date").type).toEqual({ kind: "ref", name: "core.time.date" })

    const workingWindow = byId.get("time.WorkingWindow.entity")!
    expect(workingWindow.version).toBe("0.2.0")
    expect(workingWindow.payload.objectType.properties.find((p: any) => p.id === "end_day_offset")).toEqual({
      id: "end_day_offset",
      type: { kind: "integer" },
      default: "0",
    })
    const windowLaw = byId.get("time.WorkingWindow.valid_local_window.law")!.payload
    expect(windowLaw.response.expression).toContain("resolve_local_timestamp(")
    expect(windowLaw.response.expression).toContain("self.end_day_offset in [0, 1]")
    expect(windowLaw.response.expression).toContain("(self.end_day_offset == 1) ? 86400 : 0")

    const materialized = byId.get("time.MaterializedWindow.entity")!.payload.objectType
    expect(materialized.implements).toEqual(["core.TimeInterval"])
    expect(materialized.role).toBe("happening")
    expect(materialized.links.map((l: any) => [l.id, l.target, l.cardinality.required])).toEqual([
      ["calendar", "time.Calendar", true],
      ["working_window", "time.WorkingWindow", true],
    ])

    const matLaw = byId.get("time.MaterializedWindow.resolved_matches_bounds.law")!.payload
    expect(matLaw.response.expression).toContain("resolve_local_timestamp(self.start_local_at, self.timezone, self.dst_policy)")
    expect(matLaw.response.expression).toContain("resolve_local_timestamp(self.end_local_at, self.timezone, self.dst_policy)")

    const rule = byId.get("time.window.materialize.rule")!
    const tableNode = rule.payload.graph.nodes.find((n: any) => n.type === "decisionTableNode")
    const row = tableNode.content.rules[0]
    const byKey = Object.fromEntries(tableNode.content.outputs.map((o: any) => [o.field, row[o.id]]))
    expect(byKey.starts_at).toBe("resolve_local_timestamp(start_local_at, iana_timezone, dst_policy)")
    expect(byKey.ends_at).toBe("resolve_local_timestamp(end_local_at, iana_timezone, dst_policy)")

    const action = byId.get("time.window.materialize_interval.action")!.payload
    expect(action.slice).toEqual([
      { name: "calendar", objectType: "time.Calendar", key: { property: { from: "input", property: "calendar_id" } } },
      { name: "working_window", objectType: "time.WorkingWindow", key: { property: { from: "input", property: "working_window_id" } } },
      { name: "window_calendar", objectType: "time.Calendar", key: { link: { from: "working_window", link: "calendar" } } },
    ])
    expect(action.createLinks).toEqual([
      { link: "calendar", targetType: "time.Calendar", targetIdField: "calendar_id" },
      { link: "working_window", targetType: "time.WorkingWindow", targetIdField: "working_window_id" },
    ])
    expect(action.effectProgram).toMatchObject({
      kind: "upsert_input_object",
      objectKind: "time.MaterializedWindow",
      objectIdField: "id",
      writeMode: { kind: "create_only" },
    })
    expect(action.preconditions).toHaveLength(6)
    const pre = (action.preconditions as string[]).join(" && ")
    expect(pre).toContain("slice.calendar.id in map(slice.window_calendar as cal, cal.id)")
    expect(pre).toContain("input.timezone == slice.calendar.iana_timezone")
    expect(pre).toContain("input.dst_policy == slice.calendar.dst_policy")
    expect(pre).toContain("input.tzdb_version == slice.calendar.tzdb_version")
    expect(pre).toContain("slice.working_window.start_local")
    expect(pre).toContain("slice.working_window.end_local")
    expect(pre).toContain("days_between(date(input.calendar_date), date(take(input.end_local_at, 10))) == slice.working_window.end_day_offset")
    expect(byId.get("time.window.materialize_interval.action")!.version).toBe("0.2.0")
    expect(pre).toContain("days_between(date('2024-01-01'), date(input.calendar_date))")
    expect(pre).toContain("slice.working_window.weekday")
    expect(pre).toContain("resolve_local_timestamp(input.start_local_at, slice.calendar.iana_timezone, slice.calendar.dst_policy)")
    expect(pre).toContain("resolve_local_timestamp(input.end_local_at, slice.calendar.iana_timezone, slice.calendar.dst_policy)")
  })

  test("workforce ontology pack pins exact upstream licenses/versions and validates membership projections against closed shape", () => {
    const pack = byId.get("norn.ontology-pack.workforce")!.payload
    const w3cOrg = pack.sources.find((s: any) => s.sourceId === "source.w3c-org")
    const w3cTime = pack.sources.find((s: any) => s.sourceId === "source.w3c-time")
    expect(w3cOrg).toMatchObject({ releaseId: "0.8@2014-04-12", releasedAt: "2014-04-12", license: "PDDL-1.0" })
    expect(w3cTime).toMatchObject({ releaseId: "2016@2017-04-06", releasedAt: "2017-04-06", license: "CC-BY-4.0" })

    const membershipShape = pack.shapes.find((s: any) => s.shapeId === "shape.norn.org.membership")
    expect(membershipShape).toEqual({
      shapeId: "shape.norn.org.membership",
      termId: "norn.org.membership",
      closed: true,
      fields: [
        { fieldId: "person", valueType: "term_ref", minCount: 1, maxCount: 1, targetTermId: "norn.org.person" },
        { fieldId: "unit", valueType: "term_ref", minCount: 1, maxCount: 1, targetTermId: "norn.org.unit" },
      ],
    })

    // Representative valid membership projection passes closed-shape validation.
    expect(
      validateClosedProjection(pack, {
        termId: "norn.org.membership",
        values: { person: ["norn.org.person"], unit: ["norn.org.unit"] },
      }),
    ).toEqual([])

    // Missing required `unit` fails closed-shape validation.
    expect(
      validateClosedProjection(pack, {
        termId: "norn.org.membership",
        values: { person: ["norn.org.person"] },
      }),
    ).toEqual(["missing required field unit"])

    // Unknown field is rejected by closed shape.
    expect(
      validateClosedProjection(pack, {
        termId: "norn.org.membership",
        values: { person: ["norn.org.person"], unit: ["norn.org.unit"], role: ["goldsmith"] },
      }),
    ).toEqual(["unknown field role"])
  })

  test.skipIf(!checker)("Rust checker admits org/time foundation, verifies all LAW-EXAMPLE-1 cases, and refuses DST gaps and folds", () => {
    const checked = checker!.checkArtifactSet(artifacts, { base: loadBase() })
    if (!checked.admitted) throw new Error(JSON.stringify(checked.diagnostics, null, 2))
    expect(checked.diagnostics.diagnostics).toEqual([])

    // All authored LAW-EXAMPLE-1 cases pass.
    expect(examples.length).toBeGreaterThanOrEqual(15)
    expect(runExamples(checker!, artifacts, examples)).toEqual([])

    // Exercise MaterializedWindow.resolved_matches_bounds law through evaluateLaw on DST gap & fold.
    const matLaw = byId.get("time.MaterializedWindow.resolved_matches_bounds.law")!.payload
    expect(checker!.evaluateLaw).toBeDefined()

    // DST spring-forward gap (America/New_York 2026-03-08T02:30:00 does not exist) must refuse under every policy.
    for (const dstPolicy of ["reject", "earlier", "later"] as const) {
      const gap = checker!.evaluateLaw!(matLaw, {
        id: "mw-gap",
        calendar_date: "2026-03-08",
        start_local_at: "2026-03-08T02:30:00",
        end_local_at: "2026-03-08T10:30:00",
        starts_at: "2026-03-08T07:30:00Z",
        ends_at: "2026-03-08T14:30:00Z",
        timezone: "America/New_York",
        dst_policy: dstPolicy,
        tzdb_version: "2025b",
      })
      expect(gap.ok).toBe(false)
      if (!gap.ok) expect(gap.error.toLowerCase()).toContain("gap")
    }

    // DST fall-back fold (America/New_York 2026-11-01T01:30:00 occurs twice) must refuse under "reject".
    const foldReject = checker!.evaluateLaw!(matLaw, {
      id: "mw-fold-reject",
      calendar_date: "2026-11-01",
      start_local_at: "2026-11-01T01:30:00",
      end_local_at: "2026-11-01T09:30:00",
      starts_at: "2026-11-01T05:30:00Z",
      ends_at: "2026-11-01T14:30:00Z",
      timezone: "America/New_York",
      dst_policy: "reject",
      tzdb_version: "2025b",
    })
    expect(foldReject.ok).toBe(false)
    if (!foldReject.ok) expect(foldReject.error.toLowerCase()).toContain("ambiguous")

    // Same DST fold disambiguates deterministically under "earlier" (UTC-4 -> 05:30Z) and "later" (UTC-5 -> 06:30Z),
    // and swapping the resolved UTC instant is rejected as violated.
    const foldEarlierMismatch = checker!.evaluateLaw!(matLaw, {
      id: "mw-fold-earlier-wrong",
      calendar_date: "2026-11-01",
      start_local_at: "2026-11-01T01:30:00",
      end_local_at: "2026-11-01T09:30:00",
      starts_at: "2026-11-01T06:30:00Z",
      ends_at: "2026-11-01T14:30:00Z",
      timezone: "America/New_York",
      dst_policy: "earlier",
      tzdb_version: "2025b",
    })
    expect(foldEarlierMismatch).toEqual({ ok: true, verdict: "violated" })

    // WorkingWindow local-shape law: offset 0 needs start < end, offset 1 admits any valid pair,
    // and only offsets 0 and 1 are admitted.
    const windowLaw = byId.get("time.WorkingWindow.valid_local_window.law")!.payload
    const window = (start_local: string, end_local: string, end_day_offset: number) =>
      checker!.evaluateLaw!(windowLaw, { id: "ww", weekday: "sunday", start_local, end_local, end_day_offset })
    expect(window("09:00", "17:00", 0)).toEqual({ ok: true, verdict: "holds" })
    expect(window("22:00", "06:00", 0)).toEqual({ ok: true, verdict: "violated" })
    expect(window("22:00", "06:00", 1)).toEqual({ ok: true, verdict: "holds" })
    expect(window("08:00", "08:00", 0)).toEqual({ ok: true, verdict: "violated" })
    expect(window("08:00", "08:00", 1)).toEqual({ ok: true, verdict: "holds" })
    expect(window("00:00", "23:59:59", 1)).toEqual({ ok: true, verdict: "holds" })
    for (const offset of [-1, 2]) expect(window("09:00", "17:00", offset)).toEqual({ ok: true, verdict: "violated" })

    // An overnight end that lands in the spring-forward gap refuses under every policy.
    for (const dstPolicy of ["reject", "earlier", "later"] as const) {
      const overnightGap = checker!.evaluateLaw!(matLaw, {
        id: "mw-overnight-gap",
        calendar_date: "2026-03-07",
        start_local_at: "2026-03-07T22:00:00",
        end_local_at: "2026-03-08T02:30:00",
        starts_at: "2026-03-08T03:00:00Z",
        ends_at: "2026-03-08T07:30:00Z",
        timezone: "America/New_York",
        dst_policy: dstPolicy,
        tzdb_version: "2025b",
      })
      expect(overnightGap.ok).toBe(false)
      if (!overnightGap.ok) expect(overnightGap.error.toLowerCase()).toContain("gap")
    }
  }, 60_000)
})
