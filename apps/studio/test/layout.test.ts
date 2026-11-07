import { expect, test } from "bun:test"
import { STATE_SPACING, STEP_SPACING, tidy } from "../src/lib/canvas/layout.ts"
import { DECISION_EXAMPLE, EXAMPLE } from "../src/lib/canvas/tools.ts"
import type { Graph } from "../src/lib/canvas/graph.ts"

test("tidy lays an automation out in columns from its start, every connection running left to right", () => {
  for (const example of [EXAMPLE, DECISION_EXAMPLE]) {
    const tidied = tidy(example, STEP_SPACING)
    const x = new Map(tidied.nodes.map(({ id, position }) => [id, position.x]))
    expect(x.get("start")).toBe(0)
    for (const { source, target } of tidied.edges) expect(x.get(target)!).toBeGreaterThan(x.get(source)!)
    // It only moves nodes: same ids, same config, same edges.
    expect(tidied.edges).toEqual(example.edges)
    expect(tidied.nodes.map(({ id, config }) => ({ id, config }))).toEqual(example.nodes.map(({ id, config }) => ({ id, config })))
  }
})

test("tidy starts a lifecycle at its initial state; unreached states follow in a column of their own", () => {
  const graph: Graph = {
    nodes: [{ id: "b", kind: "state", position: { x: 9, y: 9 }, config: {} }, { id: "a", kind: "state", position: { x: 5, y: 5 }, config: { initial: "yes" } }, { id: "lost", kind: "state", position: { x: 0, y: 0 }, config: {} }],
    edges: [{ id: "a-b", source: "a", sourcePort: "out", target: "b", targetPort: "in", config: {} }],
  }
  const at = new Map(tidy(graph, STATE_SPACING).nodes.map(({ id, position }) => [id, position]))
  expect(at.get("a")).toEqual({ x: 0, y: 0 })
  expect(at.get("b")).toEqual({ x: STATE_SPACING.width + STATE_SPACING.gap, y: 0 })
  expect(at.get("lost")?.x).toBe(2 * (STATE_SPACING.width + STATE_SPACING.gap))
  expect(tidy({ nodes: [], edges: [] }, STATE_SPACING)).toEqual({ nodes: [], edges: [] })
})

test("tidy pushes the next row down below a card taller than the row pitch, so cards in a column never overlap", () => {
  const graph = {
    nodes: [
      { id: "s", kind: "start", position: { x: 0, y: 0 }, config: {} },
      { id: "tall", kind: "route", position: { x: 0, y: 0 }, config: {} },
      { id: "after", kind: "tool", position: { x: 0, y: 0 }, config: {} },
    ],
    edges: [{ id: "e1", source: "s", sourcePort: "next", target: "tall", targetPort: "in", config: {} }, { id: "e2", source: "s", sourcePort: "other", target: "after", targetPort: "in", config: {} }],
  } as unknown as Parameters<typeof tidy>[0]
  const at = new Map(tidy(graph, STEP_SPACING, (node) => node.id === "tall" ? 134 : 74).nodes.map(({ id, position }) => [id, position]))
  expect(at.get("after")!.x).toBe(at.get("tall")!.x)
  expect(at.get("after")!.y - at.get("tall")!.y).toBeGreaterThanOrEqual(134 + 24)
  // Without heights, the pitch stays the row pitch.
  const plain = new Map(tidy(graph, STEP_SPACING).nodes.map(({ id, position }) => [id, position]))
  expect(plain.get("after")!.y - plain.get("tall")!.y).toBe(STEP_SPACING.row)
})
