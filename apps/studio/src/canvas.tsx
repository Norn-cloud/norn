import { useQuery } from "@tanstack/react-query"
import { useCallback, useMemo, useState } from "react"
import { Handle, MarkerType, Position, type Connection, type Edge, type Node, type NodeProps } from "@xyflow/react"
import { Network, Plus, Trash2 } from "lucide-react"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Textarea } from "@/components/ui/textarea"
import { EmptyState, Panel, Well } from "@/components/panel"
import { cn } from "@/lib/utils"
import { CanvasFrame, IssueCard, type CanvasFrameProps, type Editing, type Issue } from "./canvas-frame.tsx"
import { loadChecker } from "./lib/canvas/checker.ts"
import { introduced, place, type Finding } from "./lib/canvas/diagnostics.ts"
import * as g from "./lib/canvas/graph.ts"
import { compile, decompile, edgeId, lifecycleKinds, lifecyclesIn, type LifecycleRef } from "./lib/canvas/lifecycle.ts"
import { words } from "./lib/format.ts"
import type { Envelope } from "./lib/transport.ts"

export const SAVE_BLOCKED = "Saving to the server needs its admission and governed-install route, which is not live yet."

type StateNode = Node<{ label: string; flags: string; issues: readonly Issue[] }, "state">

/** The recorded definitions, loaded with the query so importing this page needs no Vite (the transport finds recordings with import.meta.glob). */
const recordedDefinitions = async () => (await import("./lib/transport.ts")).recordedTransport.definitions()

export function StateBox({ data, selected }: NodeProps<StateNode>) {
  const failing = data.issues.length > 0
  return (
    <div data-testid={`node-${data.label}`} title={data.issues.map(({ message }) => message).join("\n")} className={cn("w-36 rounded-lg border-2 bg-background px-3 py-2 text-center text-sm", selected ? "border-primary ring-2 ring-primary/30" : "border-border", failing && "border-destructive")}>
      <Handle type="target" position={Position.Left} id="in" />
      <Handle type="source" position={Position.Right} id="out" />
      <div className="font-medium">{data.label}</div>
      {data.flags && <div className="text-meta text-muted-foreground">{data.flags}</div>}
      {failing && <div className="text-meta text-destructive">{data.issues.length} {data.issues.length === 1 ? "problem" : "problems"}</div>}
    </div>
  )
}

const NODE_TYPES = { state: StateBox }
const FLAGS = ["initial", "terminal", "immutable"] as const

/** Selected states and steps: rename, flags, rules, or delete. */
function Inspector({ editing, issues, standing }: { readonly editing: Editing; readonly issues: readonly Issue[]; readonly standing: readonly Finding[] }) {
  const { graph, selection, edit, select } = editing
  const count = selection.nodes.length + selection.edges.length
  const remove = () => { edit(g.remove(graph, selection.nodes, selection.edges)); select({ nodes: [], edges: [] }) }
  if (count === 0) return (
    <>
      <p className="text-muted-foreground">Select a state or a step. Drag from a state's right edge to another state to add a step, or drag a step's end to reconnect it. Shift-click or shift-drag selects several.</p>
      <p className="text-meta text-muted-foreground">⌘Z undo · ⌘⇧Z redo · ⌘D duplicate · ⌘C/⌘V copy and paste · ⌘A select all · ⌘S save draft · Delete removes</p>
      {standing.length > 0 && (
        <details className="text-meta text-muted-foreground">
          <summary className="cursor-pointer select-none">{standing.length} findings the loaded definitions already had</summary>
          <div className="space-y-2 pt-2">{standing.map((finding, index) => <IssueCard key={index} issue={finding} />)}</div>
        </details>
      )}
    </>
  )
  if (count > 1) return (
    <>
      <p>{selection.nodes.length} states and {selection.edges.length} steps selected.</p>
      <Button size="sm" variant="outline" onClick={remove}><Trash2 />Delete selected</Button>
    </>
  )
  const node = graph.nodes.find(({ id }) => id === selection.nodes[0])
  const edge = graph.edges.find(({ id }) => id === selection.edges[0])
  const siblings = edge ? graph.edges.filter(({ config }) => config.transition === edge.config.transition).map(({ id }) => id) : []
  const own = issues.filter((issue) => (node && issue.node === node.id) || (edge && issue.edge === edge.id))
  return (
    <>
      {node && <>
        <label className="block space-y-1"><span className="text-meta text-muted-foreground">Name</span>
          <Input aria-label="Name" defaultValue={node.id} key={`name:${node.id}`} onBlur={(event) => {
            const to = event.target.value.trim()
            if (!to || to === node.id) return
            if (graph.nodes.some(({ id }) => id === to)) { edit({ code: "canvas.node.duplicate", message: `There is already a state named ${to}` }); event.target.value = node.id; return }
            edit(g.renameNode(graph, node.id, to)); select({ nodes: [to], edges: [] })
          }} /></label>
        {FLAGS.map((flag) => (
          <label key={flag} className="flex items-center gap-2"><input type="checkbox" aria-label={words(flag)} checked={node.config[flag] === "yes"} onChange={(event) => edit(g.configureNode(graph, node.id, { [flag]: event.target.checked ? "yes" : "" }))} />{words(flag)}</label>
        ))}
      </>}
      {edge && <>
        <p className="text-meta text-muted-foreground">{words(edge.source)} → {words(edge.target)}</p>
        <label className="block space-y-1"><span className="text-meta text-muted-foreground">Step id</span>
          <Input aria-label="Step id" defaultValue={edge.config.transition} key={`${edge.id}:id:${edge.config.transition}`} onBlur={(event) => { const id = event.target.value.trim(); if (id) edit(g.configureEdges(graph, siblings, { transition: id })) }} /></label>
        <label className="block space-y-1"><span className="text-meta text-muted-foreground">Allowed only when (one rule per line)</span>
          <Textarea aria-label="Guards" defaultValue={edge.config.guards} key={`${edge.id}:guards:${edge.config.guards}`} className="font-mono text-xs" onBlur={(event) => edit(g.configureEdges(graph, siblings, { guards: event.target.value }))} /></label>
        <label className="block space-y-1"><span className="text-meta text-muted-foreground">Description</span>
          <Input aria-label="Description" defaultValue={edge.config.description} key={`${edge.id}:description:${edge.config.description}`} onBlur={(event) => edit(g.configureEdges(graph, siblings, { description: event.target.value }))} /></label>
      </>}
      <Button size="sm" variant="outline" onClick={remove}><Trash2 />Delete</Button>
      {own.map((issue, index) => <IssueCard key={index} issue={issue} />)}
    </>
  )
}

/** The frame's props for one lifecycle; the sheet renders the same props with fixed findings. */
export function lifecycleFrame(envelope: Envelope, lifecycle: LifecycleRef, issues: CanvasFrameProps["issues"], standing: readonly Finding[]): CanvasFrameProps {
  return {
    draftKey: `lifecycle:${lifecycle.artifactId}:${lifecycle.index}`,
    label: `${words(lifecycle.objectType)}: ${words(lifecycle.property).toLowerCase()}`,
    initial: decompile(envelope, lifecycle), kinds: lifecycleKinds(lifecycle), nodeTypes: NODE_TYPES, issues, saveBlocked: SAVE_BLOCKED,
    empty: "This lifecycle has no states. Add one to start.",
    toNode: (node, found, selected): StateNode => ({ id: node.id, type: "state", position: node.position, selected, data: { label: words(node.id), flags: FLAGS.filter((flag) => node.config[flag] === "yes").join(" · "), issues: found } }),
    toEdge: (edge, found, selected): Edge => {
      const stroke = found.length > 0 ? "var(--destructive)" : selected ? "var(--primary)" : "var(--muted-foreground)"
      return {
        id: edge.id, source: edge.source, target: edge.target, sourceHandle: edge.sourcePort, targetHandle: edge.targetPort, selected,
        label: words(edge.config.transition || edge.id), markerEnd: { type: MarkerType.ArrowClosed, color: stroke }, style: { stroke, strokeWidth: found.length > 0 || selected ? 2 : 1 },
        labelBgStyle: { fill: "var(--card)" }, labelStyle: { fill: found.length > 0 ? "var(--destructive)" : "var(--foreground)", fontSize: 12 },
      }
    },
    newEdge: ({ source, target, sourceHandle, targetHandle }: Connection, graph) => {
      const transition = g.freshName(graph.edges.map(({ config }) => config.transition ?? ""), `${source}_to_${target}`)
      return { id: g.freshName(graph.edges.map(({ id }) => id), edgeId(transition, source)), source, target, sourcePort: sourceHandle ?? "out", targetPort: targetHandle ?? "in", config: { transition, guards: "", description: "" } }
    },
    copyEdgeConfig: (edge, taken) => ({ ...edge.config, transition: g.freshName(taken, `${edge.config.transition || "step"}_copy`) }),
    palette: ({ graph, edit, select }) => (
      <Button size="sm" variant="outline" onClick={() => { const id = g.freshId(graph, "new_state"); edit(g.addNode(graph, { id, kind: "state", position: { x: 40, y: 40 + graph.nodes.length * 20 }, config: {} })); select({ nodes: [id], edges: [] }) }}><Plus />State</Button>
    ),
    inspector: (editing, found) => <Inspector editing={editing} issues={found} standing={standing} />,
  }
}

/** Edit one lifecycle: every change compiles the entity again and the checker runs over the definition set. */
function Editor({ definitions, envelope, lifecycle }: { readonly definitions: readonly Envelope[]; readonly envelope: Envelope; readonly lifecycle: LifecycleRef }) {
  const checker = useQuery({ queryKey: ["checker"], queryFn: () => loadChecker(), staleTime: Infinity, retry: false })
  // What the loaded definitions already fail on, so the canvas leads with what an edit changed.
  const baseline = useMemo(() => checker.data?.(definitions).diagnostics.diagnostics, [checker.data, definitions])

  const issues = useCallback((graph: g.Graph) => {
    if (checker.isError) return { issues: [], status: <span className="text-destructive" role="alert">Checker not loaded: no diagnostics</span> }
    if (!checker.data || !baseline) return { issues: [], status: <span role="status">Loading the checker…</span> }
    const compiled = compile(envelope, lifecycle, graph)
    const set = definitions.map((other) => other.artifactId === envelope.artifactId ? compiled.envelope : other)
    const placed = place(checker.data(set).diagnostics.diagnostics, envelope.artifactId, compiled.origins, new Set(graph.nodes.map(({ id }) => id)))
    const found: Issue[] = introduced(placed, baseline).added.map(({ finding: { code, message }, node, edge }) => ({ code, message, ...(node ? { node } : {}), ...(edge ? { edge } : {}) }))
    return { issues: found, status: found.length === 0 ? <Badge variant="outline">Checker: no new findings</Badge> : <Badge variant="destructive">Checker: {found.length} new {found.length === 1 ? "finding" : "findings"}</Badge> }
  }, [checker.isError, checker.data, baseline, definitions, envelope, lifecycle])

  const props = useMemo(() => lifecycleFrame(envelope, lifecycle, issues, baseline?.filter(({ artifactId }) => artifactId === envelope.artifactId) ?? []), [envelope, lifecycle, issues, baseline])
  return <CanvasFrame {...props} />
}

/** The page's states, from the definitions read; the sheet renders each directly. */
export interface DefinitionsRead {
  readonly isPending: boolean
  readonly error: Error | null
  readonly data: readonly Envelope[] | undefined
}

export function CanvasPage() {
  const definitions = useQuery({ queryKey: ["definitions"], queryFn: recordedDefinitions })
  const data = useMemo(() => definitions.data?.map(({ document }) => document), [definitions.data])
  return <CanvasPageView definitions={{ isPending: definitions.isPending, error: definitions.error, data }} />
}

export function CanvasPageView({ definitions }: { readonly definitions: DefinitionsRead }) {
  const envelopes = useMemo(() => definitions.data ?? [], [definitions.data])
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
      {definitions.isPending && <Panel label="Canvas"><Well><p className="p-6 text-sm text-muted-foreground" role="status">Reading definitions…</p></Well></Panel>}
      {definitions.error && <Panel label="Canvas"><Well><p className="p-6 text-sm text-destructive" role="alert">The definitions could not be read: {definitions.error.message}</p></Well></Panel>}
      {definitions.data && found.length === 0 && <Panel label="Canvas"><Well><EmptyState icon={Network}>No admitted definition declares a lifecycle yet.</EmptyState></Well></Panel>}
      {lifecycle && envelope && <Editor key={`${lifecycle.artifactId}:${lifecycle.index}`} definitions={envelopes} envelope={envelope} lifecycle={lifecycle} />}
    </section>
  )
}
