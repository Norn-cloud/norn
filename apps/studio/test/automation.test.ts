import { expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import { loadInvoice, notify } from "../../../tools/norn-ts/fixtures/flows/support/billing.builder.ts"
import { CANVAS_EXTENSION, branchesOf, flowDefinitionPayload, lower, redraw, setBranches, type Condition } from "../src/lib/canvas/automation.ts"
import type { CanvasEdge, CanvasNode, Graph } from "../src/lib/canvas/graph.ts"

const tools = [loadInvoice, notify]
const meta = { id: "billing.route", version: "1.0.0" }
const options = { scope: { orgId: "org_demo", environmentId: "dev" }, sourcePath: "tools/norn-ts/fixtures/flows/route.flow.ts" }
const golden = JSON.parse(readFileSync(join(import.meta.dir, "../../../tools/norn-ts/fixtures/flows/route.ir.json"), "utf8")) as unknown[]

const node = (id: string, kind: string, config: Record<string, string> = {}): CanvasNode => ({ id, kind, position: { x: 0, y: 0 }, config })
const edge = (source: string, sourcePort: string, target: string): CanvasEdge => ({ id: `${source}.${sourcePort}`, source, sourcePort, target, targetPort: "in", config: {} })
const branches = (...all: Condition[][]) => JSON.stringify(all)

/** The route fixture of tools/norn-ts, drawn on the canvas. */
const route: Graph = {
  nodes: [
    node("start", "start"),
    node("load", "perform", { tool: "billing.load_invoice", bind: "invoice", input: JSON.stringify({ invoiceId: "inv-1" }) }),
    node("settled", "route", { branches: branches([{ left: "invoice.status", op: "==", right: "paid" }], [{ left: "invoice.status", op: "==", right: "draft" }]) }),
    node("paid", "succeed", { outcome: "already-paid" }),
    node("draft", "succeed", { outcome: "not-issued" }),
    node("overdue", "route", { branches: branches([{ left: "invoice.overdue", op: "==", right: "true" }, { left: "invoice.amountMinor", op: ">", right: "0" }]) }),
    node("remind", "perform", { tool: "billing.notify", input: JSON.stringify({ invoiceId: "invoice.id", outcome: "overdue" }) }),
    node("open", "succeed", { outcome: "open" }),
  ],
  edges: [
    edge("start", "next", "load"), edge("load", "next", "settled"),
    edge("settled", "b0", "paid"), edge("settled", "b1", "draft"), edge("settled", "next", "overdue"),
    edge("overdue", "b0", "remind"), edge("overdue", "next", "open"),
  ],
}

const withoutLayout = (artifacts: readonly unknown[]) => JSON.parse(JSON.stringify(artifacts, (key, value) => key === "extensions" ? undefined : value)) as unknown[]

test("the drawn route lowers to exactly the artifacts its typed definition lowers to", () => {
  const { lowered, problems } = lower(route, tools, meta, options)
  expect(problems).toEqual([])
  expect(withoutLayout(lowered?.artifacts ?? [])).toEqual(golden)
  expect(lowered?.states.get("remind")).toContain("awaiting_perform_remind")
})

test("a lowered automation redraws as the same graph, and a rule changed elsewhere does not", () => {
  const artifacts = lower(route, tools, meta, options).lowered?.artifacts ?? []
  expect(redraw(artifacts, tools, meta, options)).toEqual(route)
  const tampered = artifacts.map((artifact) => artifact.kind === "rule" ? { ...artifact, payload: { ...artifact.payload, ruleId: "billing.other" } } : artifact)
  expect(redraw(tampered, tools, meta, options)).toBeUndefined()
  expect(artifacts.find(({ kind }) => kind === "rule")?.payload.extensions).toHaveProperty([CANVAS_EXTENSION])
})

test("the canvas names joins, loops, orphans and unknown tools on the step", () => {
  const broken: Graph = {
    nodes: [...route.nodes, node("stray", "sleep", { millis: "1000" }), node("ghost", "perform", { tool: "nowhere.tool" })],
    edges: [...route.edges.filter(({ id }) => id !== "overdue.next"), edge("overdue", "next", "ghost"), edge("ghost", "next", "open"), edge("remind", "next", "open")],
  }
  const codes = lower(broken, tools, meta).problems.map(({ code, node }) => `${code}@${node}`)
  expect(codes).toContain("canvas.flow.join@open")
  expect(codes).toContain("canvas.flow.unreachable@stray")
  expect(codes).toContain("canvas.tool.unknown@ghost")
})

test("a lowering finding lands on the step that caused it", () => {
  const wrong: Graph = { ...route, nodes: route.nodes.map((one) => one.id === "remind" ? { ...one, config: { ...one.config, input: JSON.stringify({ invoiceId: "invoice.amountMinor", outcome: "overdue" }) } } : one) }
  const { problems } = lower(wrong, tools, meta)
  expect(problems.map(({ code, node }) => `${code}@${node}`)).toContain("NORN-FLOW-VALUE@remind")
})

test("reordering a route's branches carries each branch's exit; removing a branch removes its exit", () => {
  const settled = route.nodes.find(({ id }) => id === "settled")!
  const [paid, draft] = branchesOf(settled) as [Condition[], Condition[]]
  const swapped = setBranches(route, "settled", [draft, paid], [1, 0])
  expect(swapped.edges.filter(({ source, sourcePort }) => source === "settled" && sourcePort.startsWith("b")).map(({ sourcePort, target }) => `${sourcePort}->${target}`).sort()).toEqual(["b0->draft", "b1->paid"])
  expect(lower(swapped, tools, meta, options).problems).toEqual([])
  const dropped = setBranches(route, "settled", [draft], [1])
  expect(dropped.edges.filter(({ source }) => source === "settled").map(({ sourcePort, target }) => `${sourcePort}->${target}`).sort()).toEqual(["b0->draft", "next->overdue"])
})

test("the canvas's titles reach the flow definition payload: the flow's title and each tool and wait step's label", () => {
  const titled = { ...meta, title: "Invoice follow-up" }
  const waiting: Graph = {
    nodes: [...route.nodes.filter(({ id }) => id !== "remind"), node("pause", "sleep", { millis: "60000" }), node("remind", "perform", { tool: "billing.notify", input: JSON.stringify({ invoiceId: "invoice.id", outcome: "overdue" }) })],
    edges: [...route.edges.filter(({ target }) => target !== "remind"), edge("overdue", "b0", "pause"), edge("pause", "next", "remind")],
  }
  const { lowered, problems } = lower(waiting, tools, titled, options)
  expect(problems).toEqual([])
  expect(lowered?.flow).toEqual({
    id: "billing.route", version: "1.0.0", title: "Invoice follow-up",
    steps: [{ id: "load", label: "Load" }, { id: "pause", label: "Pause" }, { id: "remind", label: "Remind" }],
  })
  const payload = flowDefinitionPayload(lowered!)
  expect(payload.flow).toBe(lowered!.flow)
  expect(payload.artifacts).toBe(lowered!.artifacts)
  // Display-only: the untitled lowering runs exactly the same artifacts.
  expect(withoutLayout(lower(route, tools, meta, options).lowered?.artifacts ?? [])).toEqual(golden)
})

test("a second connection from one exit is named on the step it leaves, not only on where it lands", () => {
  const doubled: Graph = { nodes: [...route.nodes, node("extra", "succeed", {})], edges: [...route.edges, { ...edge("remind", "next", "extra"), id: "remind.next.again" }] }
  const codes = lower(doubled, tools, meta).problems.map(({ code, node }) => `${code}@${node}`)
  expect(codes).toContain("canvas.flow.exit_twice@remind")
})
