import { expect, test } from "bun:test"
import { readFileSync, readdirSync } from "node:fs"
import { join } from "node:path"
import type { Envelope } from "../src/lib/transport.ts"
import { anchorOf, backOf, pathTo, spineOf, stepId, treeOf, visibleOf } from "../src/lib/tree.ts"
import { guardedFields, workflows } from "../src/lib/workflows.ts"

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



test("a guard reads as the fields it requires, in words", () => {
  const approve = named("core.analytics.Restatement")?.steps.find(({ transition }) => transition.id === "approve")
  expect(guardedFields(approve?.transition.guards ?? [])).toEqual(["reason", "slice ids", "candidate", "candidate digest"])
  expect(guardedFields(["(self.candidate != '') and (self.candidate_digest != '')"])).toEqual(["candidate", "candidate digest"])
})

test("the spine runs from the start to the furthest declared end, by its shortest path", () => {
  const order = named("commerce.SalesOrder")
  expect(order && spineOf(order.lifecycle)).toEqual(["draft", "submitted", "approved", "invoiced", "completed"])
  const restatement = named("core.analytics.Restatement")
  expect(restatement && spineOf(restatement.lifecycle)).toEqual(["building", "ready", "approved", "active"])
})

test("steps back to an earlier state become chips on their source, not arcs", () => {
  const order = named("commerce.SalesOrder")
  if (order === undefined) throw new Error("SalesOrder")
  const tree = treeOf(order.lifecycle)
  const backs = (state: string) => (tree.backs.get(state) ?? []).map(({ transition, to }) => `${transition.id}→${to}`).sort()
  expect(backs("submitted")).toEqual(["amend→draft", "cancel→cancelled"])
  expect(tree.nodes.filter((node) => node.kind === "state" && node.row === 0).map(({ id }) => id)).toEqual(["draft", "submitted", "approved", "invoiced", "completed"])
  expect(new Set(tree.nodes.filter(({ kind }) => kind === "state").map(({ id }) => id))).toEqual(new Set(["draft", "submitted", "approved", "invoiced", "completed", "cancelled", "on_hold"]))
})

test("a pinned step lights its path from the start and the options after it", () => {
  const order = named("commerce.SalesOrder")
  if (order === undefined) throw new Error("SalesOrder")
  const tree = treeOf(order.lifecycle)
  const invoice = stepId({ id: "invoice", from: ["approved"], to: "invoiced" }, "approved")
  const { path, next } = pathTo(tree, invoice)
  expect(path).toEqual(["draft", stepId({ id: "submit", from: ["draft"], to: "submitted" }, "draft"), "submitted", stepId({ id: "approve", from: ["submitted"], to: "approved" }, "submitted"), "approved", invoice])
  expect([...next]).toContain("invoiced")
})

test("a twenty-state lifecycle folds to the focused path and two hops on, and unfolds on request", () => {
  const states = Array.from({ length: 20 }, (_, index) => `s${index}`)
  const lifecycle = { property: "status", initial: ["s0"], terminal: ["s19"], transitions: [
    ...states.slice(1).map((to, index) => ({ id: `next${index}`, from: [`s${index}`], to })),
    ...states.slice(2).map((from, index) => ({ id: `cancel${index}`, from: [from], to: "s0" })),
  ] }
  const tree = treeOf(lifecycle)
  expect(tree.spine).toHaveLength(20)
  const folded = visibleOf(tree, "s5", false)
  const shownStates = tree.nodes.filter(({ kind, id }) => kind === "state" && folded.shown.has(id)).map(({ id }) => id)
  expect(shownStates).toEqual(["s0", "s1", "s2", "s3", "s4", "s5", "s6", "s7"])
  expect(folded.hidden).toBe(12)
  expect(visibleOf(tree, "s5", true).hidden).toBe(0)
  expect(visibleOf(treeOf(named("commerce.SalesOrder")?.lifecycle ?? lifecycle), undefined, false).hidden).toBe(0)
})


test("a step back is found by its id, so its chip can open the step itself", () => {
  const order = named("commerce.SalesOrder")
  if (order === undefined) throw new Error("SalesOrder")
  const tree = treeOf(order.lifecycle)
  const amend = backOf(tree, stepId({ id: "amend", from: ["submitted"], to: "draft" }, "submitted"))
  expect(amend && { id: amend.transition.id, from: amend.from, to: amend.to }).toEqual({ id: "amend", from: "submitted", to: "draft" })
  expect(backOf(tree, "draft")).toBeUndefined()
})

test("pinning a step back on a folded lifecycle keeps its source path in view", () => {
  const states = Array.from({ length: 20 }, (_, index) => `s${index}`)
  const lifecycle = { property: "status", initial: ["s0"], terminal: ["s19"], transitions: [
    ...states.slice(1).map((to, index) => ({ id: `next${index}`, from: [`s${index}`], to })),
    ...states.slice(2).map((from, index) => ({ id: `cancel${index}`, from: [from], to: "s0" })),
  ] }
  const tree = treeOf(lifecycle)
  const back = stepId({ id: "cancel8", from: ["s10"], to: "s0" }, "s10")
  expect(backOf(tree, back)?.from).toBe("s10")
  expect(anchorOf(tree, back)).toBe("s10")
  const { shown, hidden } = visibleOf(tree, back, false)
  for (const state of ["s0", "s5", "s10", "s11", "s12"]) expect(shown.has(state)).toBe(true)
  expect(hidden).toBe(7)
})
