import { expect, test } from "bun:test"
import { LANE, laneRoute, rounded } from "../src/lib/canvas/routes.ts"

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
