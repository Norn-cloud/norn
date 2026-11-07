import { expect, test } from "bun:test"
import { focusCss } from "../src/lib/canvas/focus.ts"
import type { Graph } from "../src/lib/canvas/graph.ts"

const edge = (id: string, source: string, target: string) => ({ id, source, sourcePort: "out", target, targetPort: "in", config: {} })
const graph: Graph = { nodes: [], edges: [edge("submit:draft", "draft", "submitted"), edge("invoice:approved", "approved", "invoiced"), edge("cancel:approved", "approved", "cancelled")] }

test("nothing in focus writes no rules", () => {
  expect(focusCss(graph, { nodes: [], edges: [] }, "c1")).toBe("")
})

test("a state in focus lights its own steps and dims the rest, in its own canvas only", () => {
  const css = focusCss(graph, { nodes: ["approved"], edges: [] }, "c1")
  expect(css).toContain('[data-canvas-id="c1"] .react-flow__edge:not(.canvas-edge-failing)')
  expect(css).toContain('.react-flow__edge[data-id="invoice:approved"]')
  expect(css).toContain('.react-flow__edge[data-id="cancel:approved"]')
  expect(css).not.toContain('data-id="submit:draft"')
})

test("ids are quoted, so no id can break out of its selector", () => {
  const odd: Graph = { nodes: [], edges: [edge('a"] { color: red } [x="', "a", "b")] }
  expect(focusCss(odd, { nodes: [], edges: ['a"] { color: red } [x="'] }, "c1")).toContain('[data-id="a\\"] { color: red } [x=\\""]')
})
