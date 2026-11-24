import { expect, test } from "bun:test"
import type { Field } from "../src/lib/catalog.ts"
import {
  automationField,
  blankGuardClause,
  cleanAutomation,
  compileGuard,
  describeThen,
  describeWhen,
  duplicateAutomation,
  durationParts,
  msFromDuration,
  newAutomationId,
  parseGuard,
  type AutomationDefinition,
} from "../src/lib/automations.ts"
import { cleanView, duplicateView, newViewId, sortKeys, sortText, viewField, type SavedView } from "../src/lib/views.ts"

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
