/**
 * The canvas contact sheet: each state in `sheet-states.ts`, rendered from the
 * real components with Studio's real tokens. `?state=<id>` renders one state
 * (atlas mounts these as iframes); no parameter renders them all on one page.
 */
import { QueryClient, QueryClientProvider } from "@tanstack/react-query"
import { ReactFlow, ReactFlowProvider, type Node } from "@xyflow/react"
import "@xyflow/react/dist/style.css"
import { StrictMode, type ReactNode } from "react"
import { createRoot } from "react-dom/client"
import { Badge } from "@/components/ui/badge"
import { CanvasFrame, writeDraft, type Issue } from "./canvas-frame.tsx"
import { CanvasPageView, StateBox, lifecycleFrame } from "./canvas.tsx"
import { StepBox, automationFrame } from "./automation-canvas.tsx"
import { EXAMPLE } from "./lib/canvas/tools.ts"
import * as g from "./lib/canvas/graph.ts"
import { lifecyclesIn } from "./lib/canvas/lifecycle.ts"
import type { Envelope } from "./lib/transport.ts"
import { SHEET } from "./sheet-states.ts"
import "./index.css"

const DEFINITIONS = Object.values(import.meta.glob<string>("../../../registry/core/artifacts/*.json", { query: "?raw", import: "default", eager: true })).map((source) => JSON.parse(source) as Envelope)
const ORDER = lifecyclesIn(DEFINITIONS).find(({ objectType }) => objectType === "commerce.SalesOrder")
const ORDER_ENVELOPE = DEFINITIONS.find(({ artifactId }) => artifactId === ORDER?.artifactId)

function Box(data: { readonly label: string; readonly flags?: string; readonly issues?: readonly Issue[]; readonly selected?: boolean }) {
  const node: Node = { id: "s", type: "state", position: { x: 20, y: 20 }, selected: data.selected ?? false, data: { label: data.label, flags: data.flags ?? "", issues: data.issues ?? [] } }
  return <div className="h-32 w-64"><ReactFlowProvider><ReactFlow nodes={[node]} edges={[]} nodeTypes={{ state: StateBox }} proOptions={{ hideAttribution: true }} panOnDrag={false} zoomOnScroll={false} /></ReactFlowProvider></div>
}

function Step({ id, issues = [] }: { readonly id: string; readonly issues?: readonly Issue[] }) {
  const step = EXAMPLE.nodes.find((one) => one.id === id)
  if (!step) return null
  const node: Node = automationFrame(() => ({ issues: [], status: null })).toNode({ ...step, position: { x: 20, y: 20 } }, issues, false)
  return <div className="h-36 w-72"><ReactFlowProvider><ReactFlow nodes={[node]} edges={[]} nodeTypes={{ step: StepBox }} proOptions={{ hideAttribution: true }} panOnDrag={false} zoomOnScroll={false} /></ReactFlowProvider></div>
}

const automation = (id: string, extra: Partial<Parameters<typeof CanvasFrame>[0]> = {}) =>
  <CanvasFrame {...automationFrame(() => ({ issues: [], status: <Badge variant="outline">Checker: no findings</Badge> }))} draftKey={`sheet:${id}`} {...extra} />

const problem: Issue = { code: "lifecycle.validation.dead_end_state", message: 'state "invoiced" has no outgoing transition', node: "invoiced" }

function frame(id: string, issues: readonly Issue[], status: ReactNode, extra: Partial<Parameters<typeof CanvasFrame>[0]> = {}) {
  if (!ORDER || !ORDER_ENVELOPE) return null
  const props = lifecycleFrame(ORDER_ENVELOPE, ORDER, () => ({ issues, status }), [])
  return <CanvasFrame {...props} draftKey={`sheet:${id}`} {...extra} />
}

const SPECIMENS: Record<string, () => ReactNode> = {
  "state/default": () => <Box label="Submitted" />,
  "state/flags": () => <Box label="Draft" flags="initial · immutable" />,
  "state/selected": () => <Box label="Approved" selected />,
  "state/problem": () => <Box label="Invoiced" issues={[problem, problem]} />,
  "nasty/long-name": () => <Box label="Awaiting second approver sign off from finance" />,
  "frame/default": () => frame("default", [], <Badge variant="outline">Checker: no new findings</Badge>),
  "frame/findings": () => frame("findings", [problem, { ...problem, code: "lifecycle.validation.unreachable_state", message: 'state "completed" is not reachable from an initial state', node: "completed" }], <Badge variant="destructive">Checker: 2 new findings</Badge>),
  "frame/empty": () => frame("empty", [], <Badge variant="outline">Checker: no new findings</Badge>, { initial: { nodes: [], edges: [] } }),
  "frame/checker-loading": () => frame("checker-loading", [], <span role="status">Loading the checker…</span>),
  "frame/checker-error": () => frame("checker-error", [], <span className="text-destructive" role="alert">Checker not loaded: no diagnostics</span>),
  "frame/dirty": () => {
    if (!ORDER || !ORDER_ENVELOPE) return null
    const initial = lifecycleFrame(ORDER_ENVELOPE, ORDER, () => ({ issues: [], status: null }), []).initial
    // Seed the dirty specimen once; an edit made to it since is kept across reloads.
    const seeded = (() => { try { return window.localStorage.getItem("norn.canvas.draft.sheet:dirty") !== null } catch { return false } })()
    if (!seeded) writeDraft("sheet:dirty", { graph: g.remove(initial, [], ["cancel:draft"]), at: "2026-11-06T09:00:00.000Z" })
    return frame("dirty", [], <Badge variant="outline">Checker: no new findings</Badge>)
  },
  "frame/refusal": () => frame("refusal", [], <Badge variant="outline">Checker: no new findings</Badge>, { initialNotice: { tone: "refusal", text: "There is already a state named approved" } }),
  "frame/multi-select": () => frame("multi-select", [], <Badge variant="outline">Checker: no new findings</Badge>, { initialSelection: { nodes: ["invoiced", "completed"], edges: ["complete:invoiced"] } }),
  "step/tool": () => <Step id="load" />,
  "step/route": () => <Step id="settled" />,
  "step/start": () => <Step id="start" />,
  "step/problem": () => <Step id="paid" issues={[{ code: "canvas.flow.unreachable", message: "Nothing leads to this step, so it never runs", node: "paid" }]} />,
  "automation/default": () => automation("default"),
  "automation/route-selected": () => automation("route", { initialSelection: { nodes: ["settled"], edges: [] } }),
  "automation/tool-selected": () => automation("tool", { initialSelection: { nodes: ["remind"], edges: [] } }),
  "page/loading": () => <CanvasPageView definitions={{ isPending: true, error: null, data: undefined }} />,
  "page/error": () => <CanvasPageView definitions={{ isPending: false, error: new Error("The cell did not answer (503)"), data: undefined }} />,
  "page/no-lifecycles": () => <CanvasPageView definitions={{ isPending: false, error: null, data: [] }} />,
}

function Sheet() {
  const only = new URLSearchParams(window.location.search).get("state")
  const states = SHEET.filter(({ id }) => only === null || id === only)
  return (
    <main className="space-y-8 bg-background p-6 text-foreground">
      {only === null && <h1 className="text-title font-medium">Canvas: every state of every component</h1>}
      {states.map((state) => (
        <section key={state.id} data-state={state.id} className="space-y-2">
          {only === null && <header><div className="font-mono text-sm">{state.component} · {state.id}</div><p className="text-meta text-muted-foreground">{state.note}</p></header>}
          <div className="rounded-lg border p-3">{SPECIMENS[state.id]?.() ?? <p className="text-destructive">MISSING specimen</p>}</div>
        </section>
      ))}
    </main>
  )
}

const dark = window.matchMedia("(prefers-color-scheme: dark)")
document.documentElement.classList.toggle("dark", dark.matches)
createRoot(document.getElementById("root") as HTMLElement).render(<StrictMode><QueryClientProvider client={new QueryClient()}><Sheet /></QueryClientProvider></StrictMode>)
