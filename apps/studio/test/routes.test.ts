import { expect, test } from "bun:test"
import { LANE, RAIL_GAP, laneRoute, placeLabels, railsOf, rounded } from "../src/lib/canvas/routes.ts"

test("a step along the row goes straight across, its label mid-gap", () => {
  expect(laneRoute({ x: 144, y: 29 }, { x: 240, y: 29 })).toEqual({ path: "M 144 29 L 240 29", label: { x: 192, y: 29, anchor: "middle" }, straight: true })
})

test("a step down to the next row runs in the lane between the rows, its label under its own source", () => {
  const routed = laneRoute({ x: 144, y: 29 }, { x: 240, y: 139 })
  expect(routed.straight).toBe(false)
  expect(routed.label).toEqual({ x: 144 + LANE.stub + 10, y: 139 - LANE.pitch / 2, anchor: "start" })
  // Two sources dropping to the same target sit at different places along the lane.
  expect(laneRoute({ x: 384, y: 29 }, { x: 240, y: 139 }).label.x).not.toBe(routed.label.x)
})

test("a step back along the row loops above it and enters its target from the left", () => {
  const routed = laneRoute({ x: 384, y: 29 }, { x: 240, y: 29 })
  expect(routed.label).toEqual({ x: 384 + LANE.stub - 10, y: 29 - LANE.pitch / 2, anchor: "end" })
  expect(routed.path.startsWith("M 384 29")).toBe(true)
  expect(routed.path.endsWith("L 240 29")).toBe(true)
})

test("corners are rounded, never past half of the shorter leg", () => {
  expect(rounded([{ x: 0, y: 0 }, { x: 4, y: 0 }, { x: 4, y: 40 }], 10)).toBe("M 0 0 L 2 0 Q 4 0 4 2 L 4 40")
})

// The sales order's lifecycle as laid out: Draft, Submitted, Approved, Invoiced on the main row; Cancelled and On hold below.
const at = { draft: { x: 0, y: 0 }, submitted: { x: 280, y: 0 }, approved: { x: 560, y: 0 }, invoiced: { x: 840, y: 0 }, cancelled: { x: 280, y: 110 }, on_hold: { x: 560, y: 110 } } as const
const step = (id: string, source: keyof typeof at, target: keyof typeof at) => ({ id, source: at[source], target: at[target] })
const steps = [
  step("submit", "draft", "submitted"), step("approve", "submitted", "approved"), step("invoice", "approved", "invoiced"),
  step("cancel_draft", "draft", "cancelled"), step("cancel_submitted", "submitted", "cancelled"), step("cancel_approved", "approved", "cancelled"),
  step("hold", "approved", "on_hold"), step("resume", "on_hold", "approved"), step("amend", "submitted", "draft"),
]

test("every lane step gets its own rail, RAIL_GAP apart; straight steps need none", () => {
  const rails = railsOf(steps)
  expect(rails.has("submit")).toBe(false)
  const between = ["cancel_draft", "cancel_submitted", "cancel_approved", "hold", "resume"].map((id) => rails.get(id)!.offset)
  expect(new Set(between).size).toBe(between.length)
  expect(Math.min(...between.slice(1).map((v, i) => Math.abs(v - between[i]!)))).toBeGreaterThanOrEqual(RAIL_GAP)
  // Amend, the only step back along the row, has the lane above to itself.
  expect(rails.get("amend")?.offset).toBe(0)
})

test("steps into one target land at distinct points on its left side, and leave one source at distinct turns", () => {
  const rails = railsOf(steps)
  const ports = ["cancel_draft", "cancel_submitted", "cancel_approved"].map((id) => rails.get(id)!.port)
  expect(new Set(ports).size).toBe(3)
  // Approved sends two steps down (Cancel and Hold): they turn at different places.
  expect(rails.get("cancel_approved")!.out).not.toBe(rails.get("hold")!.out)
  // Submitted is reached by a straight step (Submit), so the step back into it lands beside the handle, not on it.
  expect(railsOf([step("submit", "draft", "submitted"), step("from_hold", "on_hold", "submitted")]).get("from_hold")!.port).not.toBe(0)
})

test("a rail moves the lane, the turns and the landing point", () => {
  const routed = laneRoute({ x: 144, y: 29 }, { x: 424, y: 139 }, LANE, { offset: 8, out: 8, into: 16, port: -8 })
  expect(routed.label).toEqual({ x: 144 + LANE.stub + 8 + 10, y: 139 - LANE.pitch / 2 + 8, anchor: "start" })
  expect(routed.path.endsWith("L 424 131")).toBe(true)
})

test("lane labels never overlap: each slides along its own run until clear of the others", () => {
  const handles = steps.map(({ id, source, target }) => ({ id, source: { x: source.x + 168, y: source.y + 29 }, target: { x: target.x, y: target.y + 29 }, width: 52 }))
  const placed = placeLabels(handles, railsOf(handles))
  const boxes = handles.flatMap((step) => {
    const rail = placed.get(step.id)
    if (!rail || rail.along === undefined) return []
    const { label } = laneRoute(step.source, step.target, LANE, rail)
    return [{ id: step.id, x1: label.x - 26, x2: label.x + 26, y: label.y }]
  })
  expect(boxes.length).toBe(6)
  for (const [i, a] of boxes.entries()) for (const b of boxes.slice(i + 1)) {
    const overlap = a.x1 < b.x2 && b.x1 < a.x2 && Math.abs(a.y - b.y) < 18
    expect(overlap, `${a.id} and ${b.id}`).toBe(false)
  }
})
