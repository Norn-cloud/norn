import { expect, test } from "bun:test"
import { INSET, READABLE, WITH_MINIMAP, clearOfMinimap, fitsReadable, openingViewport } from "../src/lib/canvas/viewport.ts"

const card = (x: number, y: number) => ({ x, y, width: 144, height: 58 })

test("a graph that fits at the readable zoom or larger opens whole and centred, never above 1:1", () => {
  const view = openingViewport([card(0, 0), card(200, 110)], { width: 1200, height: 600 })
  expect(view.zoom).toBe(1)
  // Centred in the room the toolbar and margins leave.
  expect(view.x + (0 + 344) / 2 * view.zoom).toBeCloseTo(INSET.side + (1200 - 2 * INSET.side) / 2)
})

test("a graph too wide to fit readable opens at the floor, framed on its anchor at the left", () => {
  const nodes = [0, 1, 2, 3, 4, 5].map((column) => card(column * 200, 0))
  const view = openingViewport(nodes, { width: 700, height: 600 }, nodes[0])
  expect(view.zoom).toBe(READABLE)
  expect(view.x).toBe(INSET.side)
})

test("without an anchor the leftmost node frames it; an empty graph or container opens at 1:1", () => {
  const nodes = [card(600, 0), card(-400, 50), card(800, 0)]
  expect(openingViewport(nodes, { width: 500, height: 600 }).x).toBeCloseTo(INSET.side + 400 * READABLE)
  expect(openingViewport([], { width: 500, height: 600 })).toEqual({ x: 0, y: 0, zoom: 1 })
  expect(openingViewport(nodes, { width: 0, height: 0 })).toEqual({ x: 0, y: 0, zoom: 1 })
})

test("the minimap's band is kept clear: a framed graph sits above it", () => {
  const nodes = [0, 1, 2, 3, 4, 5].map((column) => card(column * 200, column % 2 ? 110 : 0))
  const container = { width: 700, height: 600 }
  expect(fitsReadable(nodes, container)).toBe(false)
  const view = openingViewport(nodes, container, nodes[0], WITH_MINIMAP)
  const lowest = Math.max(...nodes.map(({ y, height }) => (y + height) * view.zoom + view.y))
  expect(lowest).toBeLessThanOrEqual(container.height - WITH_MINIMAP.bottom)
  expect(fitsReadable([card(0, 0)], container)).toBe(true)
})

test("a graph wider and taller than the view leaves no clear corner, so the minimap starts hidden", () => {
  const container = { width: 700, height: 400 }
  const grid = [0, 1, 2, 3, 4, 5].flatMap((column) => [0, 1, 2, 3, 4].map((row) => card(column * 200, row * 110)))
  const view = openingViewport(grid, container, grid[0], WITH_MINIMAP)
  expect(clearOfMinimap(grid, view, container)).toBe(false)
  const row = [0, 1, 2, 3, 4, 5].map((column) => card(column * 200, 0))
  expect(clearOfMinimap(row, openingViewport(row, { width: 700, height: 600 }, row[0], WITH_MINIMAP), { width: 700, height: 600 })).toBe(true)
})
