import { useQuery } from "@tanstack/react-query"
import { useEffect, useMemo, useState } from "react"
import { Background, Controls, Handle, MarkerType, Position, ReactFlow, type Connection, type Edge, type Node, type NodeProps } from "@xyflow/react"
import "@xyflow/react/dist/style.css"
import { Plus, Redo2, Undo2 } from "lucide-react"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Textarea } from "@/components/ui/textarea"
import { Panel, Well } from "@/components/panel"
import { cn } from "@/lib/utils"
import { loadChecker } from "./lib/canvas/checker.ts"
import { introduced, place, type Placed } from "./lib/canvas/diagnostics.ts"
import * as g from "./lib/canvas/graph.ts"
import { compile, decompile, edgeId, lifecycleKinds, lifecyclesIn, type LifecycleRef } from "./lib/canvas/lifecycle.ts"
import { words } from "./lib/format.ts"
import { recordedTransport, type Envelope } from "./lib/transport.ts"

type StateNode = Node<{ label: string; flags: string; findings: readonly Placed[] }, "state">

function StateBox({ data, selected }: NodeProps<StateNode>) {
  const failing = data.findings.length > 0
  return (
    <div title={data.findings.map(({ finding }) => finding.message).join("\n")} className={cn("w-36 rounded-lg border-2 bg-background px-3 py-2 text-center text-sm", selected ? "border-primary" : "border-border", failing && "border-destructive")}>
      <Handle type="target" position={Position.Left} id="in" />
      <Handle type="source" position={Position.Right} id="out" />
      <div className="font-medium">{data.label}</div>
      {data.flags && <div className="text-meta text-muted-foreground">{data.flags}</div>}
      {failing && <div className="text-meta text-destructive">{data.findings.length} {data.findings.length === 1 ? "problem" : "problems"}</div>}
    </div>
  )
}

const NODE_TYPES = { state: StateBox }
const FLAGS = ["initial", "terminal", "immutable"] as const

/** Edit one lifecycle: drag, connect and configure states and steps; the checker runs on every change. */
function Editor({ definitions, envelope, lifecycle }: { readonly definitions: readonly Envelope[]; readonly envelope: Envelope; readonly lifecycle: LifecycleRef }) {
  const kinds = useMemo(() => lifecycleKinds(lifecycle), [lifecycle])
  const [state, setState] = useState(() => g.history(decompile(envelope, lifecycle)))
  const [selected, setSelected] = useState<{ node?: string; edge?: string }>({})
  const [refusal, setRefusal] = useState<g.Refusal | null>(null)
  const checker = useQuery({ queryKey: ["checker"], queryFn: () => loadChecker(), staleTime: Infinity, retry: false })
  const graph = state.present
  const edit = (next: g.Graph) => { setRefusal(null); setState((current) => g.commit(current, next)) }

  const compiled = useMemo(() => compile(envelope, lifecycle, graph), [envelope, lifecycle, graph])
  // What the loaded definitions already fail on, so the canvas leads with what this edit changed.
  const baseline = useMemo(() => checker.data?.(definitions).diagnostics.diagnostics, [checker.data, definitions])
  const findings = useMemo(() => {
    if (!checker.data || !baseline) return undefined
    const set = definitions.map((other) => other.artifactId === envelope.artifactId ? compiled.envelope : other)
    const placed = place(checker.data(set).diagnostics.diagnostics, envelope.artifactId, compiled.origins, new Set(graph.nodes.map(({ id }) => id)))
    return introduced(placed, baseline)
  }, [checker.data, baseline, definitions, envelope.artifactId, compiled, graph.nodes])
  const on = (key: "node" | "edge", id: string) => findings?.added.filter((placed) => placed[key] === id) ?? []

  useEffect(() => {
    const keys = (event: KeyboardEvent) => {
      if (!(event.ctrlKey || event.metaKey) || event.key.toLowerCase() !== "z" || event.target instanceof HTMLInputElement || event.target instanceof HTMLTextAreaElement) return
      event.preventDefault()
      setState((current) => event.shiftKey ? g.redo(current) : g.undo(current))
    }
    window.addEventListener("keydown", keys)
    return () => window.removeEventListener("keydown", keys)
  }, [])

  const nodes: StateNode[] = graph.nodes.map((node) => ({
    id: node.id, type: "state", position: node.position, selected: selected.node === node.id,
    data: { label: words(node.id), flags: FLAGS.filter((flag) => node.config[flag] === "yes").join(" · "), findings: on("node", node.id) },
  }))
  const edges: Edge[] = graph.edges.map((edge) => {
    const failing = on("edge", edge.id).length > 0
    const stroke = failing ? "var(--destructive)" : selected.edge === edge.id ? "var(--primary)" : "var(--muted-foreground)"
    return {
      id: edge.id, source: edge.source, target: edge.target, sourceHandle: edge.sourcePort, targetHandle: edge.targetPort,
      label: words(edge.config.transition ?? edge.id), selected: selected.edge === edge.id,
      markerEnd: { type: MarkerType.ArrowClosed, color: stroke }, style: { stroke, strokeWidth: failing ? 2 : 1 },
      labelBgStyle: { fill: "var(--card)" }, labelStyle: { fill: failing ? "var(--destructive)" : "var(--foreground)", fontSize: 12 },
    }
  })

  const onConnect = ({ source, target, sourceHandle, targetHandle }: Connection) => {
    const transition = g.freshName(graph.edges.map(({ config }) => config.transition ?? ""), `${source}_to_${target}`)
    const id = g.freshName(graph.edges.map((edge) => edge.id), edgeId(transition, source))
    const next = g.connect(graph, kinds, { id, source, target, sourcePort: sourceHandle ?? "out", targetPort: targetHandle ?? "in", config: { transition, guards: "", description: "" } })
    if (g.isRefusal(next)) setRefusal(next); else edit(next)
  }
  const node = graph.nodes.find(({ id }) => id === selected.node)
  const edge = graph.edges.find(({ id }) => id === selected.edge)
  const siblings = edge ? graph.edges.filter(({ config }) => config.transition === edge.config.transition).map(({ id }) => id) : []
  const unplaced = findings?.added.filter((placed) => !placed.node && !placed.edge) ?? []

  return (
    <div className="grid gap-3 2xl:grid-cols-[minmax(0,1fr)_22rem]">
      <Panel label={`${words(lifecycle.objectType)}: ${words(lifecycle.property).toLowerCase()}`} action={
        checker.isError ? <span className="text-destructive">Checker not loaded: no diagnostics</span>
          : !findings ? "Loading the checker…"
            : findings.added.length === 0 ? <Badge variant="outline">Checker: no new findings</Badge> : <Badge variant="destructive">Checker: {findings.added.length} new {findings.added.length === 1 ? "finding" : "findings"}</Badge>
      }>
        <div className="flex flex-wrap gap-1.5 px-2.5 pb-2">
          <Button size="sm" variant="outline" onClick={() => edit(g.addNode(graph, { id: g.freshId(graph, "new_state"), kind: "state", position: { x: 40, y: 40 + graph.nodes.length * 20 }, config: {} }))}><Plus />State</Button>
          <Button size="sm" variant="outline" aria-label="Undo" disabled={state.past.length === 0} onClick={() => setState(g.undo)}><Undo2 /></Button>
          <Button size="sm" variant="outline" aria-label="Redo" disabled={state.future.length === 0} onClick={() => setState(g.redo)}><Redo2 /></Button>
          <Button size="sm" disabled title="Saving goes through server admission and a governed install; the route is not live yet.">Save</Button>
          {refusal && <span className="self-center text-meta text-destructive">{refusal.message}</span>}
        </div>
        <Well>
          <div className="h-[36rem] w-full">
            <ReactFlow nodes={nodes} edges={edges} nodeTypes={NODE_TYPES} fitView fitViewOptions={{ padding: 0.1, maxZoom: 1 }} proOptions={{ hideAttribution: true }}
              isValidConnection={(connection) => !g.isRefusal(g.connect(graph, kinds, { id: "probe", source: connection.source, target: connection.target, sourcePort: connection.sourceHandle ?? "out", targetPort: connection.targetHandle ?? "in", config: {} }))}
              onConnect={onConnect}
              onNodeClick={(_, { id }) => setSelected({ node: id })} onEdgeClick={(_, { id }) => setSelected({ edge: id })} onPaneClick={() => setSelected({})}
              onNodeDragStart={() => setState(g.checkpoint)}
              onNodesChange={(changes) => changes.forEach((change) => { if (change.type === "position" && change.position) setState((current) => g.replace(current, g.moveNode(current.present, change.id, change.position as g.CanvasNode["position"]))) })}
              onDelete={({ nodes: gone, edges: cut }) => edit(g.remove(graph, gone.map(({ id }) => id), cut.map(({ id }) => id)))}>
              <Background /><Controls showInteractive={false} />
            </ReactFlow>
          </div>
        </Well>
      </Panel>
      <div className="space-y-3">
        <Panel label={node ? "State" : edge ? "Step" : "Configure"}>
          <Well className="space-y-3 p-3 text-sm">
            {!node && !edge && <p className="text-muted-foreground">Select a state or a step. Drag from a state's right edge to another state to add a step; press Backspace to delete.</p>}
            {node && <>
              <label className="block space-y-1"><span className="text-meta text-muted-foreground">Name</span>
                <Input defaultValue={node.id} key={`name:${node.id}`} onBlur={(event) => { const to = event.target.value.trim(); if (to && to !== node.id) { edit(g.renameNode(graph, node.id, to)); setSelected({ node: to }) } }} /></label>
              {FLAGS.map((flag) => (
                <label key={flag} className="flex items-center gap-2"><input type="checkbox" checked={node.config[flag] === "yes"} onChange={(event) => edit(g.configureNode(graph, node.id, { [flag]: event.target.checked ? "yes" : "" }))} />{words(flag)}</label>
              ))}
            </>}
            {edge && <>
              <label className="block space-y-1"><span className="text-meta text-muted-foreground">Step id</span>
                <Input defaultValue={edge.config.transition} key={`${edge.id}:id:${edge.config.transition}`} onBlur={(event) => { const id = event.target.value.trim(); if (id) edit(g.configureEdges(graph, siblings, { transition: id })) }} /></label>
              <label className="block space-y-1"><span className="text-meta text-muted-foreground">Allowed only when (one rule per line)</span>
                <Textarea defaultValue={edge.config.guards} key={`${edge.id}:guards:${edge.config.guards}`} className="font-mono text-xs" onBlur={(event) => edit(g.configureEdges(graph, siblings, { guards: event.target.value }))} /></label>
              <label className="block space-y-1"><span className="text-meta text-muted-foreground">Description</span>
                <Input defaultValue={edge.config.description} key={`${edge.id}:description:${edge.config.description}`} onBlur={(event) => edit(g.configureEdges(graph, siblings, { description: event.target.value }))} /></label>
            </>}
            {[...(node ? on("node", node.id) : []), ...(edge ? on("edge", edge.id) : [])].map(({ finding }, index) => <Problem key={index} placed={{ finding }} />)}
          </Well>
        </Panel>
        {unplaced.length > 0 && <Panel label="Whole definition"><Well className="space-y-2 p-3">{unplaced.map((placed, index) => <Problem key={index} placed={placed} />)}</Well></Panel>}
        {findings && findings.standing.length > 0 && (
          <Panel label="Already in the loaded definitions">
            <Well className="p-3 text-meta text-muted-foreground">
              <details>
                <summary className="cursor-pointer select-none">{findings.standing.length} findings the definitions had before this edit</summary>
                <div className="space-y-2 pt-2">{findings.standing.map((placed, index) => <Problem key={index} placed={placed} />)}</div>
              </details>
            </Well>
          </Panel>
        )}
      </div>
    </div>
  )
}

function Problem({ placed: { finding } }: { readonly placed: Placed }) {
  return (
    <div className="space-y-0.5 rounded-md border border-destructive/40 bg-background p-2 text-meta">
      <div className="font-mono text-xs text-destructive">{finding.code}</div>
      <div>{finding.message}</div>
    </div>
  )
}

export function CanvasPage() {
  const definitions = useQuery({ queryKey: ["definitions"], queryFn: recordedTransport.definitions })
  const envelopes = useMemo(() => definitions.data?.map(({ document }) => document) ?? [], [definitions.data])
  const found = useMemo(() => lifecyclesIn(envelopes), [envelopes])
  const [chosen, setChosen] = useState(0)
  const lifecycle = found[chosen]
  const envelope = envelopes.find(({ artifactId }) => artifactId === lifecycle?.artifactId)
  return (
    <section className="space-y-6">
      <div className="space-y-2">
        <h1 className="text-title font-medium">Canvas</h1>
        <p className="max-w-2xl text-muted-foreground">Draw how a process moves. Every change is checked as you make it; saving asks the server, which checks again and decides.</p>
      </div>
      <div className="flex flex-wrap gap-1.5">
        {found.map((ref, index) => <Button key={`${ref.artifactId}:${ref.index}`} size="sm" variant={index === chosen ? "default" : "outline"} onClick={() => setChosen(index)}>{words(ref.objectType)}</Button>)}
      </div>
      {definitions.isError && <p className="text-destructive">{definitions.error.message}</p>}
      {lifecycle && envelope && <Editor key={`${lifecycle.artifactId}:${lifecycle.index}`} definitions={envelopes} envelope={envelope} lifecycle={lifecycle} />}
    </section>
  )
}
