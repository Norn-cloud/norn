import { expect, test } from "bun:test"
import { LABEL_ROOM_MAX, LABEL_ROOM_PAD, exitLabels, labelWidth, spaceForLabels } from "../src/lib/canvas/exits.ts"
import type { CanvasNode, Graph } from "../src/lib/canvas/graph.ts"
import { DECISION_EXAMPLE, EXAMPLE } from "../src/lib/canvas/tools.ts"

const route = (id: string, branches: [string, string, string][][], x = 0): CanvasNode =>
  ({ id, kind: "route", position: { x, y: 0 }, config: { branches: JSON.stringify(branches.map((all) => all.map(([left, op, right]) => ({ left, op, right })))) } })

test("sibling branch exits drop the words they share, so each reads by what differs", () => {
  const labels = exitLabels(route("settled", [[["invoice.status", "==", "paid"]], [["invoice.status", "==", "draft"]]]))
  expect([...labels.values()].map(({ label }) => label)).toEqual(["paid", "draft"])
  expect(labels.get("b0")?.full).toBe("invoice.status == paid")
})

test("nothing is elided when siblings share no whole word, or when eliding would leave one empty", () => {
  expect([...exitLabels(route("r", [[["a", "==", "1"]], [["b", "==", "1"]]])).values()].map(({ label }) => label)).toEqual(["a == 1", "b == 1"])
  const nested = exitLabels(route("r", [[["x", "==", "1"]], [["x", "==", "1"], ["y", ">", "0"]]]))
  expect([...nested.values()].map(({ label }) => label)).toEqual(["x == 1", "x == 1 and y > 0"])
  // A single branch has no siblings to differ from.
  expect([...exitLabels(route("r", [[["invoice.overdue", "==", "true"]]])).values()][0]?.label).toBe("overdue == true")
})

test("the layout leaves the widest label plus padding after a step with labelled exits, capped", () => {
  const graph: Graph = {
    nodes: [route("r", [[["invoice.amountMinor", ">", "1000000"]], [["invoice.amountMinor", "<", "0"]]]), { id: "t", kind: "succeed", position: { x: 230, y: 0 }, config: {} }, { id: "after", kind: "succeed", position: { x: 500, y: 0 }, config: {} }],
    edges: [{ id: "e", source: "r", sourcePort: "b0", target: "t", targetPort: "in", config: {} }],
  }
  const spaced = spaceForLabels(graph)
  const room = Math.min(LABEL_ROOM_MAX, labelWidth("> 1000000") + LABEL_ROOM_PAD)
  expect(spaced.nodes.find(({ id }) => id === "t")!.position.x).toBe(208 + room)
  // What lay to the right moves with it, keeping the order.
  expect(spaced.nodes.find(({ id }) => id === "after")!.position.x).toBe(500 + (208 + room - 230))
})

test("the examples Studio draws already leave room for every label", () => {
  for (const example of [EXAMPLE, DECISION_EXAMPLE]) expect(spaceForLabels(example)).toEqual(example)
})
