import { useQuery } from "@tanstack/react-query"
import { useCallback } from "react"
import { Handle, MarkerType, Position, type Connection, type Edge, type Node, type NodeProps } from "@xyflow/react"
import { ArrowDown, ArrowUp, Plus, Trash2 } from "lucide-react"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { cn } from "@/lib/utils"
import { CanvasFrame, IssueCard, type CanvasFrameProps, type Editing, type Issue } from "./canvas-frame.tsx"
import { SAVE_BLOCKED } from "./canvas.tsx"
import { automationKinds, branchesOf, inputsOf, lower, setBranches, type Condition } from "./lib/canvas/automation.ts"
import { loadChecker } from "./lib/canvas/checker.ts"
import * as g from "./lib/canvas/graph.ts"
import { EXAMPLE, RECORDED_SCOPE, RECORDED_TOOLS } from "./lib/canvas/tools.ts"
import { words } from "./lib/format.ts"

const META = { id: "studio.automation", version: "0.1.0" }
export const STEP_KINDS = { start: "Start", perform: "Tool", route: "Route", sleep: "Wait", succeed: "Outcome" } as const
export const RUN_BLOCKED = "Running happens in the cell with receipts; the run route is not live yet."

type StepNode = Node<{ kind: string; title: string; detail: string; issues: readonly Issue[]; outs: readonly string[] }, "step">

export function StepBox({ data, selected }: NodeProps<StepNode>) {
  const failing = data.issues.length > 0
  return (
    <div title={data.issues.map(({ message }) => message).join("\n")} className={cn("relative w-44 rounded-lg border-2 bg-background px-3 py-2 text-sm", selected ? "border-primary ring-2 ring-primary/30" : "border-border", failing && "border-destructive")}>
      {data.kind !== "start" && <Handle type="target" position={Position.Left} id="in" />}
      <div className="text-meta text-muted-foreground">{STEP_KINDS[data.kind as keyof typeof STEP_KINDS] ?? data.kind}</div>
      <div className="font-medium break-words">{data.title}</div>
      {data.detail && <div className="truncate text-meta text-muted-foreground">{data.detail}</div>}
      {failing && <div className="text-meta text-destructive">{data.issues.length} {data.issues.length === 1 ? "problem" : "problems"}</div>}
      {data.outs.map((port, index) => (
        <Handle key={port} type="source" position={Position.Right} id={port} title={port} style={{ top: `${((index + 1) / (data.outs.length + 1)) * 100}%` }} />
      ))}
    </div>
  )
}

const NODE_TYPES = { step: StepBox }

const formatCondition = (all: readonly Condition[]) => all.map(({ left, op, right }) => `${left} ${op} ${right}`).join(" and ")
const parseCondition = (line: string): Condition[] => line.split(/\s+and\s+/).map((part) => part.trim()).filter(Boolean).map((part) => {
  const [, left = part, op = "==", right = ""] = /^(\S+)\s*(==|!=|<=|>=|<|>)\s*(.+)$/.exec(part) ?? []
  return { left, op: op as Condition["op"], right }
})

/** A route's branches: edit each condition, add, delete and reorder; exits follow their branch. */
function Branches({ editing, id, branches }: { readonly editing: Editing; readonly id: string; readonly branches: Condition[][] }) {
  const { graph, edit } = editing
  const set = (order: number[], next = order.map((at) => branches[at] ?? [])) => edit(setBranches(graph, id, next, order))
  const indices = branches.map((_, index) => index)
  const swap = (a: number, b: number) => { const order = [...indices]; order[a] = b; order[b] = a; set(order) }
  return (
    <div className="space-y-2">
      <div className="text-meta text-muted-foreground">Branches, first match wins. Exits b0, b1, … then otherwise, then next (continue after the route).</div>
      {branches.map((all, index) => (
        <div key={`${id}:${index}:${formatCondition(all)}`} className="flex items-center gap-1">
          <span className="w-6 font-mono text-xs text-muted-foreground">b{index}</span>
          <Input aria-label={`Branch b${index}`} className="font-mono text-xs" defaultValue={formatCondition(all)} placeholder="invoice.status == paid"
            onBlur={(event) => { const next = branches.map((one, at) => at === index ? parseCondition(event.target.value) : one); if (formatCondition(next[index] ?? []) !== formatCondition(all)) set(indices, next) }} />
          <Button size="icon" variant="ghost" aria-label={`Move b${index} up`} disabled={index === 0} onClick={() => swap(index - 1, index)}><ArrowUp /></Button>
          <Button size="icon" variant="ghost" aria-label={`Move b${index} down`} disabled={index === branches.length - 1} onClick={() => swap(index, index + 1)}><ArrowDown /></Button>
          <Button size="icon" variant="ghost" aria-label={`Delete b${index}`} onClick={() => set(indices.filter((at) => at !== index))}><Trash2 /></Button>
        </div>
      ))}
      <Button size="sm" variant="outline" onClick={() => set([...indices, -1], [...branches, []])}><Plus />Branch</Button>
    </div>
  )
}

function Inspector({ editing, issues }: { readonly editing: Editing; readonly issues: readonly Issue[] }) {
  const { graph, selection, edit, select } = editing
  const count = selection.nodes.length + selection.edges.length
  const removable = selection.nodes.filter((id) => graph.nodes.find((node) => node.id === id)?.kind !== "start")
  const remove = () => { edit(g.remove(graph, removable, selection.edges)); select({ nodes: [], edges: [] }) }
  if (count === 0) return (
    <>
      <p className="text-muted-foreground">Select a step. Drag from a step's right edge to the next one, or drag a connection's end to reconnect it.</p>
      <p className="text-meta text-muted-foreground">⌘Z undo · ⌘⇧Z redo · ⌘D duplicate · ⌘C/⌘V copy and paste · ⌘A select all · ⌘S save draft · Delete removes</p>
    </>
  )
  if (count > 1) return (
    <>
      <p>{selection.nodes.length} steps and {selection.edges.length} connections selected.</p>
      <Button size="sm" variant="outline" onClick={remove}><Trash2 />Delete selected</Button>
    </>
  )
  const edge = graph.edges.find(({ id }) => id === selection.edges[0])
  if (edge) return (
    <>
      <p>{words(edge.source)} ({edge.sourcePort}) → {words(edge.target)}</p>
      <p className="text-meta text-muted-foreground">A connection carries no settings of its own; drag either end to reconnect it.</p>
      <Button size="sm" variant="outline" onClick={remove}><Trash2 />Delete</Button>
    </>
  )
  const node = graph.nodes.find(({ id }) => id === selection.nodes[0])
  if (!node) return null
  const tool = RECORDED_TOOLS.find(({ id }) => id === node.config.tool)
  const field = (label: string, key: string, value: string | undefined, onSave: (text: string) => void, extra: { type?: string; placeholder?: string } = {}) => (
    <label className="block space-y-1"><span className="text-meta text-muted-foreground">{label}</span>
      <Input aria-label={label} key={`${node.id}:${key}:${value ?? ""}`} defaultValue={value} onBlur={(event) => onSave(event.target.value)} {...extra} /></label>
  )
  return (
    <>
      <div className="text-meta text-muted-foreground">{STEP_KINDS[node.kind as keyof typeof STEP_KINDS]} · {node.id}</div>
      {node.kind === "start" && <p className="text-muted-foreground">Every automation has one start. Connect it to the first step.</p>}
      {node.kind === "perform" && <>
        <label className="block space-y-1"><span className="text-meta text-muted-foreground">Tool</span>
          <select aria-label="Tool" className="w-full rounded-md border bg-background px-2 py-1" value={node.config.tool} onChange={(event) => edit(g.configureNode(graph, node.id, { tool: event.target.value, input: "{}" }))}>
            {!tool && <option value={node.config.tool}>{node.config.tool} (not connected)</option>}
            {RECORDED_TOOLS.map(({ id }) => <option key={id} value={id}>{id}</option>)}
          </select></label>
        {field("Name its result", "bind", node.config.bind, (text) => edit(g.configureNode(graph, node.id, { bind: text.trim() })))}
        {Object.entries(tool?.input.properties ?? {}).map(([name, schema]) => (
          <div key={name}>{field(`${name} (${String(schema.type)})`, `input.${name}`, inputsOf(node)[name], (text) => edit(g.configureNode(graph, node.id, { input: JSON.stringify({ ...inputsOf(node), [name]: text }) })), { placeholder: "a value, or result.field" })}</div>
        ))}
      </>}
      {node.kind === "route" && <Branches editing={editing} id={node.id} branches={branchesOf(node)} />}
      {node.kind === "sleep" && field("Wait (milliseconds)", "millis", node.config.millis, (text) => edit(g.configureNode(graph, node.id, { millis: text })), { type: "number" })}
      {node.kind === "succeed" && field("Outcome", "outcome", node.config.outcome, (text) => edit(g.configureNode(graph, node.id, { outcome: text })))}
      {node.kind !== "start" && <Button size="sm" variant="outline" onClick={remove}><Trash2 />Delete</Button>}
      {issues.filter((issue) => issue.node === node.id).map((issue, index) => <IssueCard key={index} issue={issue} />)}
    </>
  )
}

const DEFAULTS: Record<string, Record<string, string>> = {
  perform: { tool: RECORDED_TOOLS[0]?.id ?? "", input: "{}" },
  route: { branches: "[[]]" },
  sleep: { millis: "60000" },
  succeed: { outcome: "done" },
}

/** The frame's props for an automation; the sheet renders the same props with fixed findings. */
export function automationFrame(issues: CanvasFrameProps["issues"], initial: g.Graph = EXAMPLE): CanvasFrameProps {
  return {
    draftKey: `automation:${META.id}`, label: "Automation", initial, kinds: automationKinds, nodeTypes: NODE_TYPES, issues,
    saveBlocked: SAVE_BLOCKED, empty: "This automation has no steps. Add one; its start comes with it.",
    fixed: (node) => node.kind === "start",
    toNode: (node, found, selected): StepNode => ({
      id: node.id, type: "step", position: node.position, selected,
      data: {
        kind: node.kind, title: words(node.id), issues: found,
        detail: node.kind === "perform" ? node.config.tool ?? "" : node.kind === "sleep" ? `${node.config.millis ?? 0} ms` : node.kind === "succeed" ? node.config.outcome ?? "" : node.kind === "route" ? `${branchesOf(node).length} branches` : "",
        outs: (automationKinds[node.kind]?.(node) ?? []).filter(({ side }) => side === "out").map(({ id }) => id),
      },
    }),
    toEdge: (edge, found, selected): Edge => ({
      id: edge.id, source: edge.source, target: edge.target, sourceHandle: edge.sourcePort, targetHandle: edge.targetPort, selected,
      markerEnd: { type: MarkerType.ArrowClosed }, label: edge.sourcePort === "next" ? undefined : edge.sourcePort,
      style: found.length > 0 ? { stroke: "var(--destructive)", strokeWidth: 2 } : selected ? { stroke: "var(--primary)", strokeWidth: 2 } : undefined,
      labelBgStyle: { fill: "var(--card)" }, labelStyle: { fill: "var(--foreground)", fontSize: 11 },
    }),
    newEdge: ({ source, target, sourceHandle, targetHandle }: Connection, graph) =>
      ({ id: g.freshName(graph.edges.map(({ id }) => id), `${source}.${sourceHandle ?? "next"}`), source, target, sourcePort: sourceHandle ?? "next", targetPort: targetHandle ?? "in", config: {} }),
    palette: ({ graph, edit, select }) => Object.keys(DEFAULTS).map((kind) => (
      <Button key={kind} size="sm" variant="outline" onClick={() => {
        const base = graph.nodes.some((node) => node.kind === "start") ? graph : g.addNode(graph, { id: "start", kind: "start", position: { x: 0, y: 120 }, config: {} })
        const id = g.freshId(base, kind === "perform" ? "call" : kind)
        edit(g.addNode(base, { id, kind, position: { x: 40, y: 360 + graph.nodes.length * 10 }, config: { ...DEFAULTS[kind] } }))
        select({ nodes: [id], edges: [] })
      }}><Plus />{STEP_KINDS[kind as keyof typeof STEP_KINDS]}</Button>
    )),
    inspector: (editing, found) => <Inspector editing={editing} issues={found} />,
  }
}

/** Edit an automation: every change lowers it through lowerFlow and runs the checker over what it lowers to. */
export function AutomationEditor() {
  const checker = useQuery({ queryKey: ["checker"], queryFn: () => loadChecker(), staleTime: Infinity, retry: false })
  const issues = useCallback((graph: g.Graph) => {
    const { lowered, problems } = lower(graph, RECORDED_TOOLS, META, RECORDED_SCOPE)
    const own: Issue[] = problems.map(({ code, message, node }) => ({ code, message, ...(node ? { node } : {}) }))
    if (!lowered) return { issues: own, status: <Badge variant="destructive">{own.length} {own.length === 1 ? "finding" : "findings"}</Badge> }
    if (checker.isError) return { issues: own, status: <span className="text-destructive" role="alert">Checker not loaded: lowering findings only</span> }
    if (!checker.data) return { issues: own, status: <span role="status">Lowered; loading the checker…</span> }
    // The checker's findings name generated states; each state belongs to the step that lowered to it.
    const owner = (message: string) => [...lowered.states].find(([, states]) => states.some((name) => message.includes(`"${name}"`)))?.[0]
    const found: Issue[] = checker.data(lowered.artifacts).diagnostics.diagnostics.map(({ code, message }) => ({ code, message, ...(owner(message) ? { node: owner(message) } : {}) }))
    return { issues: found, status: found.length === 0 ? <Badge variant="outline">Checker: no findings</Badge> : <Badge variant="destructive">Checker: {found.length} {found.length === 1 ? "finding" : "findings"}</Badge> }
  }, [checker.isError, checker.data])
  return (
    <div className="space-y-2">
      <CanvasFrame {...automationFrame(issues)} />
      <div className="flex justify-end"><Button size="sm" disabled title={RUN_BLOCKED}>Run in the cell</Button></div>
    </div>
  )
}
