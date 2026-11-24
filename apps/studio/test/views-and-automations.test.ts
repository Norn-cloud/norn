import { expect, test } from "bun:test"
import { encodeBulkSharedInputs } from "../src/actions/bulk-action-sheet.tsx"
import type { FormField } from "../src/lib/actions.ts"
import type { Field } from "../src/lib/catalog.ts"
import type { Fetch } from "../src/lib/cell-read.ts"
import {
  automationField,
  blankGuardClause,
  canRenderGuardVisually,
  cellAutomations,
  cleanAutomation,
  compileGuard,
  describeThen,
  describeWhen,
  duplicateAutomation,
  durationParts,
  encodeStaticActionInputs,
  msFromDuration,
  newAutomationId,
  parseGuard,
  validateGuardClauses,
  type AutomationDefinition,
} from "../src/lib/automations.ts"
import { cellViews, cleanView, duplicateView, newViewId, sortKeys, sortText, viewField, type SavedView } from "../src/lib/views.ts"

const FIELDS: readonly Field[] = [
  { id: "stage", type: "enum", optional: false, decimal: undefined, scalar: { kind: "enum", values: ["started", "stalled", "assisted"] } },
  { id: "score", type: "integer", optional: false, decimal: undefined, scalar: { kind: "integer" } },
  { id: "flag", type: "boolean", optional: false, decimal: undefined, scalar: { kind: "boolean" } },
  { id: "label_code", type: "text", optional: false, decimal: undefined, scalar: { kind: "text" } },
]

test("views helpers normalize payloads, preserve space and offered actions, duplicate and map pointers", () => {
  const draft: SavedView = {
    title: "  Items needing review  ",
    description: "  ",
    space: "  Operations  ",
    objectType: "demo.Record",
    filter: { all: [{ property: "score", op: "lt", value: "60" }] },
    sort: sortText([{ property: "score", direction: "asc" }]),
    columns: ["stage", "score"],
    rowActions: ["demo.record.act"],
    bulkActions: ["demo.record.act"],
  }
  const cleaned = cleanView(draft)
  expect(cleaned.title).toBe("Items needing review")
  expect(cleaned.description).toBeUndefined()
  expect(cleaned.space).toBe("Operations")
  expect(cleaned.rowActions).toEqual(["demo.record.act"])
  expect(cleaned.bulkActions).toEqual(["demo.record.act"])
  expect(sortKeys(cleaned.sort)).toEqual([{ property: "score", direction: "asc" }])

  const copy = duplicateView(cleaned)
  expect(copy.title).toBe("Items needing review (copy)")
  expect(copy.space).toBe("Operations")
  expect(copy.rowActions).toEqual(["demo.record.act"])

  expect(newViewId("demo.Record", "Needs help!", () => "abcd1234")).toBe("studio.view.demo_record.needs_help_abcd1234")
  expect(viewField("/payload/rowActions/0")).toBe("rowActions")
  expect(viewField("/space")).toBe("space")
  expect(viewField("/filter/all/0/value")).toBe("filter")
  expect(viewField("/unknown")).toBeNull()
})

test("automation visual guard compiles to ZEN syntax and round-trips back to clauses", () => {
  const clauses = [
    { property: "score", op: "lt" as const, value: "60" },
    { property: "stage", op: "eq" as const, value: "stalled" },
    { property: "flag", op: "eq" as const, value: "true" },
  ]
  const expr = compileGuard(clauses, FIELDS)
  expect(expr).toBe("self.score < 60 and self.stage == 'stalled' and self.flag == true")
  expect(parseGuard(expr)).toEqual(clauses)

  // Advanced expressions with `or` / parentheses keep raw mode (null).
  expect(parseGuard("self.score < 60 or self.stage == 'stalled'")).toBeNull()
  expect(parseGuard("")).toEqual([])
  expect(blankGuardClause(FIELDS[1]!)).toEqual({ property: "score", op: "lt", value: "" })
})

test("automation helpers clean, duplicate, describe triggers/actions and convert durations", () => {
  const def: AutomationDefinition = {
    title: "  Follow up on stalled items  ",
    description: "",
    enabled: true,
    objectType: "demo.Record",
    when: { kind: "lifecycle_entered", property: "stage", state: "stalled" },
    if: { expression: "  self.score < 60  " },
    then: {
      kind: "propose",
      actionId: "demo.record.act",
      question: "  Run follow-up on {id}?  ",
      risk: "medium",
      decider: { roles: ["admin"] },
      expiresInMs: msFromDuration(2, "days"),
    },
  }
  const cleaned = cleanAutomation(def)
  expect(cleaned.title).toBe("Follow up on stalled items")
  expect(cleaned.description).toBeUndefined()
  expect(cleaned.if?.expression).toBe("self.score < 60")
  expect(cleaned.then.kind === "propose" ? cleaned.then.question : "").toBe("Run follow-up on {id}?")

  expect(duplicateAutomation(cleaned).title).toBe("Follow up on stalled items (copy)")
  expect(newAutomationId("Follow up", () => "12345678")).toBe("studio.automation.follow_up_12345678")
  expect(automationField("/if/expression")).toBe("if")
  expect(automationField("/payload/then/actionId")).toBe("then")
  expect(automationField("/when/state")).toBe("when")

  expect(describeWhen(cleaned.when)).toBe("stage enters stalled")
  expect(describeWhen({ kind: "stale", afterMs: msFromDuration(4, "hours"), property: "stage", state: "stalled" })).toBe("unchanged for 4 hours in stage = stalled")
  expect(describeThen(cleaned.then, "Offer help")).toBe("propose Offer help to Inbox (admin)")
  expect(durationParts(msFromDuration(3, "days"))).toEqual({ amount: 3, unit: "days" })
  expect(durationParts(msFromDuration(45, "minutes"))).toEqual({ amount: 45, unit: "minutes" })
})

test("guard validation preserves operands, blocks incomplete/invalid clauses, and keeps unsupported ops in raw mode", () => {
  // Invalid numeric literal is rejected, never silently turned into 0.
  expect(validateGuardClauses([{ property: "score", op: "lt", value: "60x" }], FIELDS).length).toBeGreaterThan(0)
  expect(compileGuard([{ property: "score", op: "lt", value: "60x" }], FIELDS)).toBeUndefined()
  // Blank clause is flagged as incomplete rather than silently dropped.
  expect(validateGuardClauses([{ property: "score", op: "lt", value: "" }], FIELDS).length).toBeGreaterThan(0)
  // Explicitly empty clause list is valid (no guard).
  expect(validateGuardClauses([], FIELDS)).toEqual([])
  expect(compileGuard([], FIELDS)).toBeUndefined()
  // Leading/trailing whitespace in text operands is preserved verbatim.
  expect(compileGuard([{ property: "label_code", op: "eq", value: " north " }], FIELDS)).toBe("self.label_code == ' north '")
  // Boolean only supports `eq` in visual controls, so `!=` stays in raw mode.
  expect(canRenderGuardVisually(parseGuard("self.flag != false"), FIELDS)).toBe(false)
  expect(canRenderGuardVisually(parseGuard("self.flag == false"), FIELDS)).toBe(true)
})

test("bulk and static action inputs reject blank required numbers, allow explicit 0, and encode declared scalar types", () => {
  const formFields: readonly FormField[] = [
    { name: "count", label: "Count", description: null, pointer: "/count", required: true, nullable: false, spec: { kind: "integer" } },
    { name: "note", label: "Note", description: null, pointer: "/note", required: false, nullable: false, spec: { kind: "text" } },
    { name: "urgent", label: "Urgent", description: null, pointer: "/urgent", required: false, nullable: false, spec: { kind: "boolean" } },
  ]
  expect(encodeBulkSharedInputs(formFields, { count: "", note: "", urgent: "false" }).ok).toBe(false)
  expect(encodeBulkSharedInputs(formFields, { count: "0", note: "", urgent: "true" })).toEqual({ ok: true, shared: { count: 0, urgent: true } })
  expect(encodeBulkSharedInputs(formFields, { count: "2.5", note: "", urgent: "false" }).ok).toBe(false)

  expect(encodeStaticActionInputs(formFields, { count: "2", urgent: "true" })).toEqual({ input: { count: 2, urgent: true }, errors: [] })
  expect(encodeStaticActionInputs(formFields, { count: "2.5" }).errors.length).toBeGreaterThan(0)
})

test("views and automations list operations pin one scope and surface non-404 read failures", async () => {
  let scopeOrg = "org_a"
  const seenScopes: string[] = []
  const fetcher: Fetch = async (input) => {
    const url = String(input)
    if (url === "/api/me") {
      return new Response(JSON.stringify({ organization: { id: scopeOrg }, environmentId: "dev" }), { status: 200 })
    }
    const parsed = new URL(url, "https://studio.test")
    seenScopes.push(parsed.searchParams.get("orgId") ?? "")
    if (parsed.pathname.endsWith("/v1/definitions:list")) {
      scopeOrg = "org_b"
      return new Response(JSON.stringify({ items: [{ id: "studio.view.a", updatedAtMs: 1 }], cursor: null }), { status: 200 })
    }
    if (parsed.pathname.endsWith("/v1/definitions/studio.view.a")) {
      return new Response(JSON.stringify({ id: "studio.view.a", version: 1, payload: { title: "A", objectType: "demo.Record" } }), { status: 200 })
    }
    return new Response(JSON.stringify({ code: "host.unavailable", message: "down" }), { status: 503 })
  }
  const views = await cellViews(fetcher).listAll()
  expect(views.length).toBe(1)
  expect(seenScopes).toEqual(["org_a", "org_a"])

  // A 503 on per-id definition GET throws instead of returning an empty list.
  const failingFetch: Fetch = async (input) => {
    const url = String(input)
    if (url === "/api/me") return new Response(JSON.stringify({ organization: { id: "org_a" }, environmentId: "dev" }), { status: 200 })
    if (url.includes("/v1/definitions:list")) return new Response(JSON.stringify({ items: [{ id: "studio.automation.a" }], cursor: null }), { status: 200 })
    return new Response(JSON.stringify({ code: "host.error", message: "service unavailable" }), { status: 503 })
  }
  await expect(cellAutomations(failingFetch).list()).rejects.toThrow()
})
