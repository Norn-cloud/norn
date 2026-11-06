import { useQuery } from "@tanstack/react-query"
import { useMemo, useState } from "react"
import { Background, Controls, Handle, MarkerType, Position, ReactFlow, type Edge, type Node, type NodeProps } from "@xyflow/react"
import { Redo2, Undo2 } from "lucide-react"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Textarea } from "@/components/ui/textarea"
import { Panel, Well } from "@/components/panel"
import { cn } from "@/lib/utils"
import { automationKinds, branchesOf, inputsOf, lower, type Condition, type Problem } from "./lib/canvas/automation.ts"
import { loadChecker } from "./lib/canvas/checker.ts"
import * as g from "./lib/canvas/graph.ts"
import { EXAMPLE, RECORDED_SCOPE, RECORDED_TOOLS } from "./lib/canvas/tools.ts"
import { words } from "./lib/format.ts"

const META = { id: "studio.automation", version: "0.1.0" }
const KINDS = { start: "Start", perform: "Tool", route: "Route", sleep: "Wait", succeed: "Outcome" } as const

type StepNode = Node<{ kind: string; title: string; detail: string; problems: readonly Problem[]; outs: readonly string[] }, "step">

function StepBox({ data, selected }: NodeProps<StepNode>) {
  const failing = data.problems.length > 0
  return (
    <div title={data.problems.map(({ message }) => message).join("\n")} className={cn("relative w-44 rounded-lg border-2 bg-background px-3 py-2 text-sm", selected ? "border-primary" : "border-border", failing && "border-destructive")}>
      {data.kind !== "start" && <Handle type="target" position={Position.Left} id="in" />}
      <div className="text-meta text-muted-foreground">{KINDS[data.kind as keyof typeof KINDS] ?? data.kind}</div>
      <div className="font-medium">{data.title}</div>
      {data.detail && <div className="truncate text-meta text-muted-foreground">{data.detail}</div>}
      {failing && <div className="text-meta text-destructive">{data.problems.length} {data.problems.length === 1 ? "problem" : "problems"}</div>}
      {data.outs.map((port, index) => (
        <Handle key={port} type="source" position={Position.Right} id={port} style={{ top: `${((index + 1) / (data.outs.length + 1)) * 100}%` }}>
          <span className="pointer-events-none absolute left-2 -translate-y-1/2 text-[10px] text-muted-foreground">{port === "next" ? "" : port}</span>
        </Handle>
      ))}
    </div>
  )
}

const NODE_TYPES = { step: StepBox }

/** `invoice.status == paid and invoice.amountMinor > 0`, one branch per line. */
const formatBranches = (branches: Condition[][]) => branches.map((all) => all.map(({ left, op, right }) => `${left} ${op} ${right}`).join(" and ")).join("\n")
const parseBranches = (text: string): Condition[][] => text.split("\n").map((line) => line.trim()).filter(Boolean).map((line) => line.split(/\s+and\s+/).map((part) => {
  const [, left = "", op = "==", right = ""] = /^(\S+)\s*(==|!=|<=|>=|<|>)\s*(.+)$/.exec(part.trim()) ?? [, part, "==", ""]
  return { left, op: op as Condition["op"], right }
}))

/** Edit an automation as a flow of steps; every edit lowers it and runs the checker over what it lowers to. */
export function AutomationEditor() {
  const [state, setState] = useState(() => g.history(EXAMPLE))
  const [selected, setSelected] = useState<string>()
  const [selectedEdge, setSelectedEdge] = useState<string>()
  const [refusal, setRefusal] = useState<g.Refusal | null>(null)
  const checker = useQuery({ queryKey: ["checker"], queryFn: () => loadChecker(), staleTime: Infinity, retry: false })
  const graph = state.present
  const edit = (next: g.Graph) => { setRefusal(null); setState((current) => g.commit(current, next)) }

  const problems = useMemo((): readonly Problem[] => {
    const { lowered, problems } = lower(graph, RECORDED_TOOLS, META, RECORDED_SCOPE)
    if (!lowered || !checker.data) return problems
    // The checker's findings name generated states; each state belongs to the step that lowered to it.
    const owner = (message: string) => [...lowered.states].find(([, states]) => states.some((name) => message.includes(`"${name}"`)))?.[0]
    return checker.data(lowered.artifacts).diagnostics.diagnostics.map(({ code, message }) => ({ code, message, ...(owner(message) ? { node: owner(message) } : {}) }))
  }, [graph, checker.data])

  const nodes: StepNode[] = graph.nodes.map((node) => ({
    id: node.id, type: "step", position: node.position, selected: selected === node.id,
    data: {
      kind: node.kind, title: words(node.id), problems: problems.filter((problem) => problem.node === node.id),
      detail: node.kind === "perform" ? node.config.tool ?? "" : node.kind === "sleep" ? `${node.config.millis ?? 0} ms` : node.kind === "succeed" ? node.config.outcome ?? "" : "",
      outs: (automationKinds[node.kind]?.(node) ?? []).filter(({ side }) => side === "out").map(({ id }) => id),
    },
  }))
  const edges: Edge[] = graph.edges.map((edge) => ({
    id: edge.id, source: edge.source, target: edge.target, sourceHandle: edge.sourcePort, targetHandle: edge.targetPort, selected: selectedEdge === edge.id,
    markerEnd: { type: MarkerType.ArrowClosed }, style: selectedEdge === edge.id ? { stroke: "var(--primary)", strokeWidth: 2 } : undefined, label: edge.sourcePort === "next" ? undefined : edge.sourcePort,
    labelBgStyle: { fill: "var(--card)" }, labelStyle: { fill: "var(--foreground)", fontSize: 11 },
  }))
  const node = graph.nodes.find(({ id }) => id === selected)
  const tool = RECORDED_TOOLS.find(({ id }) => id === node?.config.tool)
  const add = (kind: keyof typeof KINDS) => {
    const id = g.freshId(graph, kind === "perform" ? "call" : kind)
    edit(g.addNode(graph, { id, kind, position: { x: 40, y: 360 + graph.nodes.length * 10 }, config: kind === "perform" ? { tool: RECORDED_TOOLS[0]?.id ?? "", input: "{}" } : kind === "route" ? { branches: "[[]]" } : {} }))
    setSelected(id)
  }
  const unplaced = problems.filter(({ node }) => !node)

  return (
    <div className="grid gap-3 2xl:grid-cols-[minmax(0,1fr)_22rem]">
      <Panel label="Automation" action={
        checker.isError ? <span className="text-destructive">Checker not loaded: lowering findings only</span>
          : problems.length === 0 ? <Badge variant="outline">{checker.data ? "Checker: no findings" : "Lowered; loading the checker…"}</Badge>
            : <Badge variant="destructive">{problems.length} {problems.length === 1 ? "finding" : "findings"}</Badge>
      }>
        <div className="flex flex-wrap gap-1.5 px-2.5 pb-2">
          {(Object.keys(KINDS) as (keyof typeof KINDS)[]).filter((kind) => kind !== "start").map((kind) => <Button key={kind} size="sm" variant="outline" onClick={() => add(kind)}>+ {KINDS[kind]}</Button>)}
          <Button size="sm" variant="outline" aria-label="Undo" disabled={state.past.length === 0} onClick={() => setState(g.undo)}><Undo2 /></Button>
          <Button size="sm" variant="outline" aria-label="Redo" disabled={state.future.length === 0} onClick={() => setState(g.redo)}><Redo2 /></Button>
          <Button size="sm" disabled title="Saving goes through server admission and a governed install; the route is not live yet.">Save</Button>
          <Button size="sm" disabled title="Running happens in the cell with receipts; the run route is not live yet.">Run</Button>
          {refusal && <span className="self-center text-meta text-destructive">{refusal.message}</span>}
        </div>
        <Well>
          <div className="h-[36rem] w-full">
            <ReactFlow nodes={nodes} edges={edges} nodeTypes={NODE_TYPES} fitView fitViewOptions={{ padding: 0.1, maxZoom: 1 }} proOptions={{ hideAttribution: true }}
              onConnect={({ source, target, sourceHandle, targetHandle }) => {
                const next = g.connect(graph, automationKinds, { id: `${source}.${sourceHandle ?? "next"}`, source, target, sourcePort: sourceHandle ?? "next", targetPort: targetHandle ?? "in", config: {} })
                if (g.isRefusal(next)) setRefusal(next); else edit(next)
              }}
              onNodeClick={(_, { id }) => { setSelected(id); setSelectedEdge(undefined) }} onEdgeClick={(_, { id }) => { setSelectedEdge(id); setSelected(undefined) }}
              onPaneClick={() => { setSelected(undefined); setSelectedEdge(undefined) }}
              onNodeDragStart={() => setState(g.checkpoint)}
              onNodesChange={(changes) => changes.forEach((change) => { if (change.type === "position" && change.position) setState((current) => g.replace(current, g.moveNode(current.present, change.id, change.position as g.CanvasNode["position"]))) })}
              onDelete={({ nodes: gone, edges: cut }) => edit(g.remove(graph, gone.map(({ id }) => id).filter((id) => id !== "start"), cut.map(({ id }) => id)))}>
              <Background /><Controls showInteractive={false} />
            </ReactFlow>
          </div>
        </Well>
      </Panel>
      <div className="space-y-3">
        <Panel label={node ? KINDS[node.kind as keyof typeof KINDS] ?? "Step" : "Configure"}>
          <Well className="space-y-3 p-3 text-sm">
            {!node && <p className="text-muted-foreground">Select a step. Drag from a step's right edge to the next one; a route has one exit per branch, one for otherwise and one to continue after it.</p>}
            {node?.kind === "perform" && <>
              <label className="block space-y-1"><span className="text-meta text-muted-foreground">Tool</span>
                <select className="w-full rounded-md border bg-background px-2 py-1" value={node.config.tool} onChange={(event) => edit(g.configureNode(graph, node.id, { tool: event.target.value, input: "{}" }))}>
                  {RECORDED_TOOLS.map(({ id }) => <option key={id} value={id}>{id}</option>)}
                </select></label>
              <label className="block space-y-1"><span className="text-meta text-muted-foreground">Name its result</span>
                <Input key={`${node.id}:bind:${node.config.bind}`} defaultValue={node.config.bind} onBlur={(event) => edit(g.configureNode(graph, node.id, { bind: event.target.value.trim() }))} /></label>
              {Object.entries(tool?.input.properties ?? {}).map(([field, schema]) => (
                <label key={`${node.id}:${node.config.tool}:${field}:${inputsOf(node)[field]}`} className="block space-y-1"><span className="text-meta text-muted-foreground">{field} <span className="font-mono">({String(schema.type)})</span></span>
                  <Input defaultValue={inputsOf(node)[field]} placeholder="a value, or result.field" onBlur={(event) => edit(g.configureNode(graph, node.id, { input: JSON.stringify({ ...inputsOf(node), [field]: event.target.value }) }))} /></label>
              ))}
            </>}
            {node?.kind === "route" && <label className="block space-y-1"><span className="text-meta text-muted-foreground">Branches, one per line (b0, b1, …)</span>
              <Textarea key={`${node.id}:branches:${node.config.branches}`} className="font-mono text-xs" defaultValue={formatBranches(branchesOf(node))} onBlur={(event) => edit(g.configureNode(graph, node.id, { branches: JSON.stringify(parseBranches(event.target.value)) }))} /></label>}
            {node?.kind === "sleep" && <label className="block space-y-1"><span className="text-meta text-muted-foreground">Wait (milliseconds)</span>
              <Input key={`${node.id}:millis:${node.config.millis}`} type="number" defaultValue={node.config.millis} onBlur={(event) => edit(g.configureNode(graph, node.id, { millis: event.target.value }))} /></label>}
            {node?.kind === "succeed" && <label className="block space-y-1"><span className="text-meta text-muted-foreground">Outcome</span>
              <Input key={`${node.id}:outcome:${node.config.outcome}`} defaultValue={node.config.outcome} onBlur={(event) => edit(g.configureNode(graph, node.id, { outcome: event.target.value }))} /></label>}
            {node && problems.filter((problem) => problem.node === node.id).map((problem, index) => <Finding key={index} problem={problem} />)}
          </Well>
        </Panel>
        {unplaced.length > 0 && <Panel label="Whole automation"><Well className="space-y-2 p-3">{unplaced.map((problem, index) => <Finding key={index} problem={problem} />)}</Well></Panel>}
      </div>
    </div>
  )
}

function Finding({ problem }: { readonly problem: Problem }) {
  return (
    <div className="space-y-0.5 rounded-md border border-destructive/40 bg-background p-2 text-meta">
      <div className="font-mono text-xs text-destructive">{problem.code}</div>
      <div>{problem.message}</div>
    </div>
  )
}
