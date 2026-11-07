import { useQuery } from "@tanstack/react-query"
import { useCallback, useMemo, useState } from "react"
import { BaseEdge, EdgeLabelRenderer, Handle, Position, useStore, useStoreApi, type EdgeProps, type Connection, type Edge, type Node, type NodeProps } from "@xyflow/react"
import { Network, Trash2 } from "lucide-react"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Textarea } from "@/components/ui/textarea"
import { EmptyState, Panel, Well } from "@/components/panel"
import { CARD, EDGE_PILL, InspectorHead, KindChip, NodeCard, cardHeight } from "@/components/canvas/node-card"
import { cn } from "@/lib/utils"
import { LANE, bundleRoute, laneRoute, laneYOf, placeLabels, railsOf, type Rail } from "./lib/canvas/routes.ts"
import type { Bundle } from "./lib/canvas/focus.ts"
import { labelWidth } from "./lib/canvas/exits.ts"
import { CanvasFrame, IssueCard, type CanvasFrameProps, type Editing, type Issue } from "./canvas-frame.tsx"
import { loadChecker } from "./lib/canvas/checker.ts"
import { introduced, place, type Finding } from "./lib/canvas/diagnostics.ts"
import * as g from "./lib/canvas/graph.ts"
import { STATE_SPACING, tidy } from "./lib/canvas/layout.ts"
import { compile, decompile, edgeId, lifecycleKinds, lifecyclesIn, stepKey, type LifecycleRef } from "./lib/canvas/lifecycle.ts"
import { AutomationEditor } from "./automation-canvas.tsx"
import { words } from "./lib/format.ts"
import type { Envelope } from "./lib/transport.ts"

export const SAVE_BLOCKED = "Saving to the server needs its admission and governed-install route, which is not live yet."

type StateNode = Node<{ label: string; flags: string; issues: readonly Issue[] }, "state">

/** The recorded definitions, loaded with the query so importing this page needs no Vite (the transport finds recordings with import.meta.glob). */
const recordedDefinitions = async () => (await import("./lib/transport.ts")).recordedTransport.definitions()

export function StateBox({ data, selected }: NodeProps<StateNode>) {
  return (
    <div title={data.issues.map(({ message }) => message).join("\n")}>
      <NodeCard testId={`node-${data.label}`} kind={data.flags.includes("initial") ? "initial" : data.flags.includes("terminal") ? "terminal" : "state"} kindLabel={data.flags || "State"} title={data.label} problems={data.issues.length} selected={selected} className="w-[168px]">
        <Handle className="canvas-pill" type="target" position={Position.Left} id="in" />
        <Handle className="canvas-pill" type="source" position={Position.Right} id="out" />
      </NodeCard>
    </div>
  )
}

const NODE_TYPES = { state: StateBox }

type StepEdgeData = { readonly label: string; readonly failing: boolean; readonly rail?: Rail }

/**
 * How the steps are drawn, worked out once per drawing from where the states sit: steps of one lifecycle step into
 * one state through one lane draw as a bundle (one rail, one pill, a tick per source); every other lane step gets its
 * own rail; labels are placed clear of each other and of the states.
 */
interface Drawing {
  readonly rails: ReadonlyMap<string, Rail>
  readonly bundles: readonly (Bundle & { readonly label: string; readonly offset: number })[]
}
const drawings = new WeakMap<g.Graph, Drawing>()
export function drawingOf(graph: g.Graph): Drawing {
  const cached = drawings.get(graph)
  if (cached) return cached
  // Each step between its handles: a state's right edge out, its left edge in, at the card's middle.
  const at = new Map(graph.nodes.map(({ id: node, position }) => [node, position]))
  const middle = cardHeight({}) / 2
  const handles = new Map(graph.edges.flatMap((edge) => {
    const source = at.get(edge.source), target = at.get(edge.target)
    return source && target ? [[edge.id, { source: { x: source.x + CARD.state, y: source.y + middle }, target: { x: target.x, y: target.y + middle } }] as const] : []
  }))
  const straight = (id: string) => { const h = handles.get(id)!; return Math.abs(h.source.y - h.target.y) < 2 && h.target.x > h.source.x }
  const groups = new Map<string, g.CanvasEdge[]>()
  for (const edge of graph.edges) {
    const h = handles.get(edge.id)
    if (!h || straight(edge.id)) continue
    const key = `${stepKey(edge)}|${edge.target}|${laneYOf(h.source, h.target)}`
    groups.set(key, [...groups.get(key) ?? [], edge])
  }
  // A bundle's id is its step and target; two bundles that share both (different guards or lanes) keep distinct ids.
  const ids = new Set<string>()
  const bundles = [...groups.values()].filter((members) => members.length > 1).map((members) => {
    const first = members[0]!
    const sorted = [...members].sort((a, b) => handles.get(a.id)!.source.x - handles.get(b.id)!.source.x)
    const base = `bundle:${first.config.transition || first.id}:${first.target}`
    const id = ids.has(base) ? `${base}:${sorted[0]!.id}` : base
    ids.add(id)
    return { id, members: sorted.map(({ id }) => id), sources: sorted.map(({ source }) => source), target: first.target, label: `${words(first.config.transition || first.id)} · from ${members.length} states` }
  })
  const bundled = new Set(bundles.flatMap(({ members }) => members))
  // A bundle holds the middle of its lane; the other steps take rails around it.
  const steps = [
    ...graph.edges.filter(({ id }) => handles.has(id) && !bundled.has(id)).map((edge) => ({ id: edge.id, ...handles.get(edge.id)!, width: labelWidth(words(edge.config.transition || edge.id)) })),
    ...bundles.map((bundle) => ({ id: bundle.id, ...handles.get(bundle.members[0]!)!, width: 0 })),
  ]
  const assigned = railsOf(steps)
  const placedBundles = bundles.map((bundle) => ({ ...bundle, offset: assigned.get(bundle.id)?.offset ?? 0 }))
  const taken = placedBundles.map((bundle) => {
    const width = labelWidth(bundle.label)
    const { label } = bundleRoute(bundle.members.map((id) => handles.get(id)!.source), handles.get(bundle.members[0]!)!.target, width, LANE, bundle.offset)
    return { x1: label.x - width / 2, x2: label.x + width / 2, y: label.y }
  })
  const rails = placeLabels(steps.filter(({ id }) => !ids.has(id)), assigned, LANE, graph.nodes.map(({ position }) => ({ ...position, width: CARD.state, height: middle * 2 })), taken)
  const drawing = { rails, bundles: placedBundles }
  drawings.set(graph, drawing)
  return drawing
}
const railOf = (graph: g.Graph, id: string): Rail | undefined => drawingOf(graph).rails.get(id)

type BundleEdgeData = { readonly label: string; readonly sources: readonly string[]; readonly failing: boolean; readonly offset: number }

/** A bundle: each source turns into one rail (a tick where it joins), the rail drops into the target; one pill. */
export function BundleEdge({ id, targetX, targetY, markerEnd, data }: EdgeProps<Edge<BundleEdgeData, "bundle">>) {
  const store = useStoreApi()
  // Each source's right-hand handle, from React Flow's own measured nodes (a string, so the selector stays stable).
  const sourcesKey = useStore((state) => (data?.sources ?? []).map((source) => {
    const node = state.nodeLookup.get(source)
    if (!node) return ""
    const { x, y } = node.internals.positionAbsolute
    return `${x + (node.measured.width ?? CARD.state)},${y + (node.measured.height ?? cardHeight({})) / 2}`
  }).join(";"))
  const sources = sourcesKey.split(";").filter(Boolean).map((pair) => { const [x, y] = pair.split(",").map(Number); return { x: x!, y: y! } })
  if (!data || sources.length === 0) return null
  const width = labelWidth(data.label)
  const routed = bundleRoute(sources, { x: targetX, y: targetY }, width, LANE, data.offset)
  return (
    <>
      <BaseEdge id={id} path={routed.path} markerEnd={markerEnd} />
      {routed.ticks.map((tick, index) => <circle key={index} cx={tick.x} cy={tick.y} r={3} className="canvas-bundle-tick" />)}
      <EdgeLabelRenderer>
        <div data-edge-label={id} title={`${data.label}: ${data.sources.map(words).join(", ")}`} className={cn(EDGE_PILL, "cursor-pointer", data.failing && "text-destructive")} onClick={() => store.getState().addSelectedEdges([id])}
          style={{ maxWidth: 220, transform: `translate(-50%, -50%) translate(${routed.label.x}px, ${routed.label.y}px)` }}>{data.label}</div>
      </EdgeLabelRenderer>
    </>
  )
}

/** A lifecycle step: straight along the main row, else in a lane with rounded corners (lib/canvas/routes); its label a pill. */
export function StepEdge({ id, sourceX, sourceY, targetX, targetY, markerEnd, data }: EdgeProps<Edge<StepEdgeData, "step">>) {
  const store = useStoreApi()
  const routed = laneRoute({ x: sourceX, y: sourceY }, { x: targetX, y: targetY }, LANE, data?.rail)
  const shift = routed.label.anchor === "middle" ? "-50%" : routed.label.anchor === "end" ? "-100%" : "0"
  // A straight step's label stays inside the gap, clear of both ports.
  const room = routed.straight ? Math.max(0, targetX - sourceX - 20) : 160
  return (
    <>
      <BaseEdge id={id} path={routed.path} markerEnd={markerEnd} />
      {data?.label && room >= 28 && (
        <EdgeLabelRenderer>
          <div data-edge-label={id} title={data.label} className={cn(EDGE_PILL, "cursor-pointer", data.failing && "text-destructive-text")} onClick={() => store.getState().addSelectedEdges([id])}
            style={{ maxWidth: room, transform: `translate(${shift}, -50%) translate(${routed.label.x}px, ${routed.label.y}px)` }}>{data.label}</div>
        </EdgeLabelRenderer>
      )}
    </>
  )
}

const EDGE_TYPES = { step: StepEdge, bundle: BundleEdge }
const FLAGS = ["initial", "terminal", "immutable"] as const

/** Selected states and steps: rename, flags, rules, or delete. */
function Inspector({ editing, issues, standing, kinds }: { readonly editing: Editing; readonly issues: readonly Issue[]; readonly standing: readonly Finding[]; readonly kinds: g.Kinds }) {
  const { graph, selection, edit, select } = editing
  const count = selection.nodes.length + selection.edges.length
  const remove = () => { edit(g.remove(graph, selection.nodes, selection.edges)); select({ nodes: [], edges: [] }) }
  if (count === 0) return (
    <>
      <p className="text-muted-foreground">Select a state or a step. Drag from a state's right edge to another state to add a step, or drag a step's end to reconnect it. Shift-click or shift-drag selects several.</p>
      <p className="text-meta text-muted-foreground">⌘Z undo · ⌘⇧Z redo · ⌘D duplicate · ⌘C/⌘V copy and paste · ⌘A select all · ⌘S save draft · ⇧⌘L tidy up · Delete removes</p>
      {standing.length > 0 && (
        <details className="text-meta text-muted-foreground">
          <summary className="cursor-pointer select-none">{standing.length} findings the loaded definitions already had</summary>
          <div className="space-y-2 pt-2">{standing.map((finding, index) => <IssueCard key={index} issue={finding} />)}</div>
        </details>
      )}
    </>
  )
  // A bundle (or any selection of one step's edges) is that one step: show it, with the states it comes from.
  const chosen = selection.edges.map((id) => graph.edges.find((one) => one.id === id))
  const oneStep = selection.nodes.length === 0 && chosen.length > 1 && chosen.every(Boolean) && new Set(chosen.map((one) => stepKey(one!))).size === 1
  if (count > 1 && !oneStep) return (
    <>
      <p>{selection.nodes.length} states and {selection.edges.length} steps selected.</p>
      <Button size="sm" variant="outline" onClick={remove}><Trash2 />Delete selected</Button>
    </>
  )
  const node = graph.nodes.find(({ id }) => id === selection.nodes[0])
  const edge = graph.edges.find(({ id }) => id === selection.edges[0])
  // The edges of this one step, by the same key compilation groups them with.
  const siblings = edge ? graph.edges.filter((other) => stepKey(other) === stepKey(edge)).map(({ id }) => id) : []
  const own = issues.filter((issue) => (node && issue.node === node.id) || (edge && issue.edge === edge.id))
  return (
    <>
      {node && <InspectorHead kind="state" id={node.id} />}
      {edge && <InspectorHead kind="route" kindLabel="Step" id={`${words(edge.source)} → ${words(edge.target)}`} />}
      {own.map((issue, index) => <IssueCard key={index} issue={issue} />)}
      {node && <>
        <label className="block space-y-1.5"><span className="text-xs font-medium text-foreground/80">Name</span>
          <Input aria-label="Name" defaultValue={node.id} key={`name:${node.id}`} onBlur={(event) => {
            const to = event.target.value.trim()
            if (!to || to === node.id) return
            if (graph.nodes.some(({ id }) => id === to)) { edit({ code: "canvas.node.duplicate", message: `There is already a state named ${to}` }); event.target.value = node.id; return }
            edit(g.renameNode(graph, node.id, to)); select({ nodes: [to], edges: [] })
          }} /></label>
        {FLAGS.map((flag) => (
          <label key={flag} className="flex items-center gap-2 text-sm"><input type="checkbox" aria-label={words(flag)} checked={node.config[flag] === "yes"} onChange={(event) => edit(g.configureNode(graph, node.id, { [flag]: event.target.checked ? "yes" : "" }))} />{words(flag)}</label>
        ))}
      </>}
      {edge && siblings.length > 1 && (
        <section className="space-y-2" data-testid="step-sources">
          <h3 className="text-xs font-semibold text-muted-foreground">From {siblings.length} states</h3>
          <ul className="space-y-1.5">
            {siblings.map((id) => { const one = graph.edges.find((other) => other.id === id)!; return (
              <li key={id} className="flex items-center justify-between gap-2 rounded-lg border border-node-border bg-card px-2.5 py-1.5 text-sm">
                <span>{words(one.source)} → {words(one.target)}</span>
                <span className="flex items-center gap-1">
                  {/* Reconnect, for a step drawn inside a bundle: point this one step at another state (undoable). */}
                  <select aria-label={`Change target of the step from ${words(one.source)}`} className="h-8 rounded-3xl bg-input/50 px-2 text-xs" value=""
                    onChange={(event) => {
                      const target = event.target.value
                      if (!target) return
                      edit(g.reconnect(graph, kinds, id, { source: one.source, sourcePort: one.sourcePort, target, targetPort: one.targetPort }))
                      select({ nodes: [], edges: [id] })
                    }}>
                    <option value="">Change target…</option>
                    {graph.nodes.filter(({ id: state }) => state !== one.target && state !== one.source).map(({ id: state }) => <option key={state} value={state}>{words(state)}</option>)}
                  </select>
                  <Button size="sm" variant="ghost" aria-label={`Remove the step from ${words(one.source)}`} onClick={() => { edit(g.remove(graph, [], [id])); select({ nodes: [], edges: siblings.filter((other) => other !== id) }) }}><Trash2 /></Button>
                </span>
              </li>
            ) })}
          </ul>
        </section>
      )}
      {edge && <>
        <label className="block space-y-1.5"><span className="text-xs font-medium text-foreground/80">Step id</span>
          <Input aria-label="Step id" defaultValue={edge.config.transition} key={`${edge.id}:id:${edge.config.transition}`} onBlur={(event) => { const id = event.target.value.trim(); if (id) edit(g.configureEdges(graph, siblings, { transition: id })) }} /></label>
        <label className="block space-y-1.5"><span className="text-xs font-medium text-foreground/80">Allowed only when (one rule per line)</span>
          <Textarea aria-label="Guards" defaultValue={edge.config.guards} key={`${edge.id}:guards:${edge.config.guards}`} className="font-mono text-xs" onBlur={(event) => edit(g.configureEdges(graph, siblings, { guards: event.target.value }))} /></label>
        <label className="block space-y-1.5"><span className="text-xs font-medium text-foreground/80">Description</span>
          <Input aria-label="Description" defaultValue={edge.config.description} key={`${edge.id}:description:${edge.config.description}`} onBlur={(event) => edit(g.configureEdges(graph, siblings, { description: event.target.value }))} /></label>
      </>}
      <div className="border-t border-node-border pt-3"><Button size="sm" variant="ghost" className="text-destructive hover:bg-destructive/10 hover:text-destructive" onClick={remove}><Trash2 />Delete</Button></div>
    </>
  )
}

/** The frame's props for one lifecycle; the sheet renders the same props with fixed findings. */
export function lifecycleFrame(envelope: Envelope, lifecycle: LifecycleRef, issues: CanvasFrameProps["issues"], standing: readonly Finding[]): CanvasFrameProps {
  return {
    draftKey: `lifecycle:${lifecycle.artifactId}:${lifecycle.index}`,
    label: `${words(lifecycle.objectType)}: ${words(lifecycle.property).toLowerCase()}`,
    initial: decompile(envelope, lifecycle), kinds: lifecycleKinds(lifecycle), nodeTypes: NODE_TYPES, edgeTypes: EDGE_TYPES, issues, saveBlocked: SAVE_BLOCKED,
    empty: "This lifecycle has no states. Add one to start.",
    nodeSize: () => ({ width: CARD.state, height: cardHeight({}) }),
    tidy: (graph) => tidy(graph, STATE_SPACING),
    toNode: (node, found, selected): StateNode => ({ id: node.id, type: "state", position: node.position, selected, data: { label: words(node.id), flags: FLAGS.filter((flag) => node.config[flag] === "yes").join(" · "), issues: found } }),
    bundles: (graph) => drawingOf(graph).bundles,
    // Bundled steps are drawn by their bundle: hidden one by one, drawn once, selected and focused as one.
    draw: (edges, graph, selection) => {
      const { bundles } = drawingOf(graph)
      const hidden = new Set(bundles.flatMap(({ members }) => members))
      return [
        ...edges.map((edge) => hidden.has(edge.id) ? { ...edge, hidden: true } : edge),
        ...bundles.map((bundle): Edge => {
          const parts = edges.filter(({ id }) => bundle.members.includes(id))
          const active = parts.some(({ className }) => className?.includes("canvas-edge-active"))
          const failing = parts.some(({ className }) => className?.includes("canvas-edge-failing"))
          return {
            id: bundle.id, source: bundle.sources[0]!, target: bundle.target, sourceHandle: "out", targetHandle: "in", type: "bundle", reconnectable: false,
            selected: bundle.members.every((member) => selection.edges.includes(member)),
            className: failing ? "canvas-edge-failing" : active ? "canvas-edge-active" : undefined, markerEnd: parts[0]?.markerEnd,
            data: { label: bundle.label, sources: bundle.sources, failing, offset: bundle.offset },
          }
        }),
      ]
    },
    toEdge: (edge, found, selected, graph): Edge => ({
      id: edge.id, source: edge.source, target: edge.target, sourceHandle: edge.sourcePort, targetHandle: edge.targetPort, selected, type: "step",
      data: { label: words(edge.config.transition || edge.id), failing: found.length > 0, ...railOf(graph, edge.id) ? { rail: railOf(graph, edge.id) } : {} },
    }),
    newEdge: ({ source, target, sourceHandle, targetHandle }: Connection, graph) => {
      const transition = g.freshName(graph.edges.map(({ config }) => config.transition ?? ""), `${source}_to_${target}`)
      return { id: g.freshName(graph.edges.map(({ id }) => id), edgeId(transition, source)), source, target, sourcePort: sourceHandle ?? "out", targetPort: targetHandle ?? "in", config: { transition, guards: "", description: "" } }
    },
    copyEdgeConfig: (edge, taken) => ({ ...edge.config, transition: g.freshName(taken, `${edge.config.transition || "step"}_copy`) }),
    palette: ({ graph, edit, select }) => (
      <Button size="sm" variant="ghost" className="gap-1.5 pl-1.5" onClick={() => { const id = g.freshId(graph, "new_state"); edit(g.addNode(graph, { id, kind: "state", position: { x: 40, y: 40 + graph.nodes.length * 20 }, config: {} })); select({ nodes: [id], edges: [] }) }}><KindChip kind="state" className="size-5 rounded-md" />State</Button>
    ),
    inspector: (editing, found) => <Inspector editing={editing} issues={found} standing={standing} kinds={lifecycleKinds(lifecycle)} />,
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
  const [kind, setKind] = useState<"lifecycle" | "automation">("lifecycle")
  const lifecycle = found[chosen]
  const envelope = envelopes.find(({ artifactId }) => artifactId === lifecycle?.artifactId)
  return (
    <section className="space-y-6">
      <div className="space-y-2">
        <h1 className="text-title font-medium">Canvas</h1>
        <p className="max-w-2xl text-muted-foreground">Draw how a process moves. Every change is checked as you make it; saving asks the server, which checks again and decides.</p>
      </div>
      <div className="flex flex-wrap gap-1.5">
        {found.map((ref, index) => <Button key={`${ref.artifactId}:${ref.index}`} size="sm" variant={kind === "lifecycle" && index === chosen ? "default" : "outline"} onClick={() => { setKind("lifecycle"); setChosen(index) }}>{words(ref.objectType)}</Button>)}
        <Button size="sm" variant={kind === "automation" ? "default" : "outline"} onClick={() => setKind("automation")}>Automation</Button>
      </div>
      {kind === "automation" && <AutomationEditor />}
      {kind === "lifecycle" && definitions.isPending && <Panel label="Canvas"><Well><p className="p-6 text-sm text-muted-foreground" role="status">Reading definitions…</p></Well></Panel>}
      {kind === "lifecycle" && definitions.error && <Panel label="Canvas"><Well><p className="p-6 text-sm text-destructive" role="alert">The definitions could not be read: {definitions.error.message}</p></Well></Panel>}
      {kind === "lifecycle" && definitions.data && found.length === 0 && <Panel label="Canvas"><Well><EmptyState icon={Network}>No admitted definition declares a lifecycle yet.</EmptyState></Well></Panel>}
      {kind === "lifecycle" && lifecycle && envelope && <Editor key={`${lifecycle.artifactId}:${lifecycle.index}`} definitions={envelopes} envelope={envelope} lifecycle={lifecycle} />}
    </section>
  )
}
