import { expect, test } from "bun:test"
import { drawingOf } from "../src/canvas.tsx"
import { focusCss } from "../src/lib/canvas/focus.ts"
import type { Graph } from "../src/lib/canvas/graph.ts"
import { LANE, bundleRoute } from "../src/lib/canvas/routes.ts"

// The sales order as laid out (280px columns, 150px rows): Cancel from four states and Hold from two, one step each.
const state = (id: string, x: number, y: number) => ({ id, kind: "state", position: { x, y }, config: {} })
const step = (transition: string, source: string, target: string) => ({ id: `${transition}:${source}`, source, sourcePort: "out", target, targetPort: "in", config: { transition, guards: "", description: "" } })
const order: Graph = {
  nodes: [state("draft", 0, 0), state("submitted", 280, 0), state("approved", 560, 0), state("invoiced", 840, 0), state("cancelled", 280, 150), state("on_hold", 560, 150)],
  edges: [
    step("submit", "draft", "submitted"), step("approve", "submitted", "approved"), step("invoice", "approved", "invoiced"),
    step("cancel", "draft", "cancelled"), step("cancel", "submitted", "cancelled"), step("cancel", "approved", "cancelled"), step("cancel", "on_hold", "cancelled"),
    step("hold", "submitted", "on_hold"), step("hold", "approved", "on_hold"), step("approve", "on_hold", "approved"),
  ],
}

test("steps of one step into one state through one lane draw as one bundle; the rest stay their own", () => {
  const { bundles, rails } = drawingOf(order)
  const cancel = bundles.find(({ id }) => id === "bundle:cancel:cancelled")!
  expect(cancel.label).toBe("Cancel · from 4 states")
  expect(cancel.sources).toEqual(["draft", "submitted", "approved", "on_hold"])
  expect(bundles.find(({ id }) => id === "bundle:hold:on_hold")?.label).toBe("Hold · from 2 states")
  // Approve from On hold is the only lane step of Approve (the other is straight), so it is drawn on its own.
  expect(bundles.some(({ id }) => id.startsWith("bundle:approve"))).toBe(false)
  expect(rails.has("approve:on_hold")).toBe(true)
  expect(rails.has("cancel:draft")).toBe(false)
})

test("a bundle's drawing: a tick where each source joins, one rail, a drop that ends on the target", () => {
  const routed = bundleRoute([{ x: 168, y: 29 }, { x: 448, y: 29 }, { x: 728, y: 29 }], { x: 280, y: 179 }, 120)
  expect(routed.ticks).toEqual([{ x: 168 + LANE.stub, y: 104 }, { x: 448 + LANE.stub, y: 104 }, { x: 728 + LANE.stub, y: 104 }])
  expect(routed.path.endsWith("L 280 179")).toBe(true)
  expect(routed.label.y).toBe(104)
})

test("a bundle in focus lights its steps and its sources", () => {
  const { bundles } = drawingOf(order)
  const css = focusCss(order, { nodes: [], edges: ["bundle:cancel:cancelled"] }, "c", bundles)
  expect(css).toContain('.react-flow__edge[data-id="bundle:cancel:cancelled"]')
  for (const source of ["draft", "submitted", "approved", "on_hold"]) expect(css).toContain(`.react-flow__node[data-id="${source}"] .canvas-node`)
})
