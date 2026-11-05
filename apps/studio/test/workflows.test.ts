import { expect, test } from "bun:test"
import { readFileSync, readdirSync } from "node:fs"
import { join } from "node:path"
import type { Envelope } from "../src/lib/transport.ts"
import { focusOf, guardedFields, layout, workflows } from "../src/lib/workflows.ts"

// The admitted core definitions the transport replays.
const directory = join(import.meta.dir, "../../../registry/core/artifacts")
const definitions = readdirSync(directory).map((name) => JSON.parse(readFileSync(join(directory, name), "utf8")) as Envelope)
const found = workflows(definitions)
const named = (objectType: string) => found.find((workflow) => workflow.objectType === objectType)

test("every declared lifecycle becomes a workflow", () => {
  expect(found.map(({ objectType, lifecycle }) => `${objectType}.${lifecycle.property}`).sort())
    .toEqual(["commerce.SalesOrder.status", "core.analytics.Restatement.status"])
})

test("a restatement's approval step is fired by its declared action and governed by its law", () => {
  const restatement = named("core.analytics.Restatement")
  const approve = restatement?.steps.find(({ transition }) => transition.id === "approve")
  expect(approve?.transition.from).toEqual(["ready"])
  expect(approve?.transition.to).toBe("approved")
  expect(approve?.actions.map(({ actionId }) => actionId)).toEqual(["core.analytics.restatement.approve"])
  expect(restatement?.laws.map(({ id }) => id)).toEqual(["core.analytics.restatement_policy"])
})

test("states are laid out by their shortest declared path from the start", () => {
  const order = named("commerce.SalesOrder")
  expect(order && Object.fromEntries([...layout(order.lifecycle)].map(([state, { column }]) => [state, column]))).toEqual({
    draft: 0, submitted: 1, cancelled: 1, approved: 2, on_hold: 2, invoiced: 3, completed: 4,
  })
})

test("a state no step reaches still appears, after the reachable ones", () => {
  const positions = layout({ property: "status", initial: ["a"], transitions: [{ id: "go", from: ["a"], to: "b" }, { id: "back", from: ["c"], to: "a" }] })
  expect(positions.get("c")).toEqual({ column: 2, row: 0 })
})

test("a guard reads as the fields it requires, in words", () => {
  const approve = named("core.analytics.Restatement")?.steps.find(({ transition }) => transition.id === "approve")
  expect(guardedFields(approve?.transition.guards ?? [])).toEqual(["reason", "slice ids", "candidate", "candidate digest"])
  expect(guardedFields(["(self.candidate != '') and (self.candidate_digest != '')"])).toEqual(["candidate", "candidate digest"])
})

test("focusing a step brings its states and the steps that touch them into view", () => {
  const restatement = named("core.analytics.Restatement")
  const focus = restatement && focusOf(restatement, "approve")
  expect([...focus?.states ?? []].sort()).toEqual(["approved", "ready"])
  expect([...focus?.adjacent ?? []].sort()).toEqual(["activate", "stage"])
})
