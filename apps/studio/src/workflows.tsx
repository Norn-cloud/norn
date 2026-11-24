import { useQuery } from "@tanstack/react-query"
import { Link } from "@tanstack/react-router"
import { Database, Workflow as WorkflowIcon } from "lucide-react"
import { guardsWords } from "./lib/guard-words.ts"
import { createContext, use, useEffect, useMemo, useRef, useState, type KeyboardEvent } from "react"
import { Background, Handle, MarkerType, Position, ReactFlow, type Edge, type Node, type NodeProps, type ReactFlowInstance } from "@xyflow/react"
import "@xyflow/react/dist/style.css"
import { LoadState } from "@/components/load-state"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { PageHeader, Panel, Well } from "@/components/panel"
import { cn } from "@/lib/utils"
import { words } from "./lib/format.ts"
import { transport } from "./lib/transport.ts"
import { anchorOf, backOf, pathTo, stepId, treeOf, visibleOf, type Tree } from "./lib/tree.ts"
import { guardedFields, workflows, type Workflow } from "./lib/workflows.ts"
import type { MenuEntry } from "./shell.tsx"
import { Automations } from "./automations.tsx"
import { flowName, useRunTypeFlows, type RunFlow } from "./lib/flows.ts"
import { useView } from "@/lib/view"

/**
 * How a lifecycle's states and steps are named: in words by default; a generated flow's run in the
 * flow's own step labels (the raw names stay under Technical).
 */
const Names = createContext<(name: string) => string>(words)
const useNames = () => use(Names)

type Pick = (id: string | undefined) => void
type StateData = { pinned: string | undefined; label: string; start: boolean; terminal: boolean; laws: number; backs: { id: string; label: string; to: string; toLabel: string }[]; faded: boolean; lit: boolean; onPick: Pick; onHover: Pick }
type StepData = { pinned: string | undefined; order: number; label: string; sub: string; faded: boolean; lit: boolean; onPick: Pick; onHover: Pick; id: string }
type MoreData = { count: number; onExpand: () => void }

/** Every card on the chart floats with the shared node depth; the lit path takes the accent ring, the rest fades. */
const tile = (faded: boolean, lit: boolean) => cn("canvas-node rounded-node border border-node-border bg-card text-card-foreground transition-opacity duration-200", faded && "opacity-35", lit && "canvas-node-selected")
/** A state's kind, as a tinted dot: where records start, where they end, or a stop on the way. */
const dot = (start: boolean, terminal: boolean) => <span aria-hidden className="mt-1.5 size-2 shrink-0 rounded-full" style={{ background: start ? "var(--tint-start)" : terminal ? "var(--tint-outcome)" : "var(--tint-state)" }} />
const hidden = "!opacity-0 !border-0 !bg-transparent"

/**
 * One size for every card, one gap between layers and rows: the tree's own layers (a state, then the
 * steps out of it) are the layout, so nothing overlaps and every edge runs the same way. The whole
 * tree is fitted to the panel's width, never cut off at the right.
 */
const CARD_W = 232
const CARD_H = 92
const GAP_X = 40
const GAP_Y = 44

/**
 * Activate on pointer down for a mouse (hover re-renders the cards, which can swallow a later click)
 * and on click for the keyboard (Enter and Space make a click with no pointer detail), never twice.
 */
const activate = (run: () => void) => ({
  onPointerDown: (event: React.PointerEvent) => { if (event.button === 0) { event.stopPropagation(); run() } },
  onClick: (event: React.MouseEvent) => { event.stopPropagation(); if (event.detail === 0) run() },
})

function StateCard({ id, data }: NodeProps<Node<StateData, "state">>) {
  return (
    <div onMouseEnter={() => data.onHover(id)} onMouseLeave={() => data.onHover(undefined)} style={{ width: CARD_W, minHeight: CARD_H }} className={cn(tile(data.faded, data.lit), "px-3.5 py-2.5")}>
      <Handle type="target" position={Position.Top} className={hidden} />
      <Handle type="source" position={Position.Bottom} className={hidden} />
      <button type="button" aria-pressed={data.pinned === id} {...activate(() => data.onPick(id))} className="block w-full rounded-md text-start outline-none focus-visible:ring-3 focus-visible:ring-ring/50">
        <div className="flex items-start gap-2">
          {dot(data.start, data.terminal)}
          <div className="min-w-0">
            <div className={cn("font-medium leading-snug", data.start ? "text-panel" : "text-sm")}>{data.label}</div>
          </div>
        </div>
      </button>
      <div className={cn("flex flex-wrap gap-1 ps-4", (data.start || data.terminal || data.backs.length > 0) && "mt-2")}>
        {data.start && <Badge variant="secondary">Starts here</Badge>}
        {data.terminal && <Badge variant="outline">Ends here</Badge>}
        {data.start && data.laws > 0 && <Badge variant="outline">{data.laws} {data.laws === 1 ? "law" : "laws"}</Badge>}
        {/* A step back: the chip opens the step itself; the arrow goes to the state it returns to. */}
        {data.backs.map((back) => (
          <span key={back.id} className="inline-flex items-center rounded-full border text-meta text-muted-foreground">
            <button type="button" aria-pressed={data.pinned === back.id} {...activate(() => data.onPick(back.id))} className="rounded-s-full px-2 py-0.5 hover:bg-muted hover:text-foreground outline-none focus-visible:ring-3 focus-visible:ring-ring/50">↺ {back.label}</button>
            <button type="button" aria-label={`Go to ${back.toLabel}`} title={`Go to ${back.toLabel}`} {...activate(() => data.onPick(back.to))} className="rounded-e-full border-s px-1.5 py-0.5 hover:bg-muted hover:text-foreground outline-none focus-visible:ring-3 focus-visible:ring-ring/50">→</button>
          </span>
        ))}
      </div>
    </div>
  )
}

function StepCard_({ data }: NodeProps<Node<StepData, "step">>) {
  return (
    <button type="button" aria-pressed={data.pinned === data.id} {...activate(() => data.onPick(data.id))} onMouseEnter={() => data.onHover(data.id)} onMouseLeave={() => data.onHover(undefined)} style={{ width: CARD_W, minHeight: CARD_H }} className={cn(tile(data.faded, data.lit), "block px-3.5 py-2.5 text-start text-sm outline-none focus-visible:ring-3 focus-visible:ring-ring/50")}>
      <Handle type="target" position={Position.Top} className={hidden} />
      <Handle type="source" position={Position.Bottom} className={hidden} />
      <div className="flex items-start gap-2 font-medium leading-snug">
        <span className="mt-px flex size-5 shrink-0 items-center justify-center rounded-full text-[0.6875rem] tabular-nums" style={{ background: "color-mix(in oklab, var(--tint-route) 14%, transparent)", color: "var(--tint-route)" }}>{data.order}</span>
        <span className="line-clamp-2">{data.label}</span>
      </div>
      <div className="line-clamp-2 ps-7 pt-0.5 text-meta text-muted-foreground">{data.sub}</div>
    </button>
  )
}

function More({ data }: NodeProps<Node<MoreData, "more">>) {
  return <Button size="sm" variant="outline" className="rounded-full" onClick={data.onExpand}>+{data.count} more states</Button>
}

const NODE_TYPES = { state: StateCard, step: StepCard_, more: More }

/** Where the arrow keys go from an element: down its steps, up its path, or across to its siblings. */
function moveFrom(tree: Tree, at: string, key: string): string | undefined {
  const node = tree.nodes.find(({ id }) => id === at)
  if (node === undefined) return undefined
  // The chart runs top to bottom: Down follows the steps, Up goes back, Left and Right move between branches.
  if (key === "ArrowDown") return tree.edges.find(({ source }) => source === at)?.target
  if (key === "ArrowUp") return pathTo(tree, at).path.at(-2)
  if (node.kind !== "step") return undefined
  const siblings = tree.nodes.filter((other) => other.kind === "step" && other.from === node.from).sort((a, b) => a.row - b.row)
  const index = siblings.findIndex(({ id }) => id === at)
  return siblings[index + (key === "ArrowRight" ? 1 : -1)]?.id
}

/** The lifecycle as a left-to-right decision tree: hover or pin an element to light its path from the start and its next options. */
function Chart({ workflow, pinned, onPin }: { readonly workflow: Workflow; readonly pinned: string | undefined; readonly onPin: Pick }) {
  const state = useNames()
  const { lifecycle } = workflow
  const tree = useMemo(() => treeOf(lifecycle), [lifecycle])
  const [hover, setHover] = useState<string>()
  const [expanded, setExpanded] = useState(false)
  const active = hover ?? pinned
  // Keep keyboard focus on the canvas when a pin came from inside it.
  const frame = useRef<HTMLDivElement>(null)
  const pin: Pick = (id) => {
    const inside = frame.current?.contains(document.activeElement) ?? false
    onPin(id)
    if (inside) requestAnimationFrame(() => frame.current?.focus())
  }
  // Scroll the pinned card (a step back's source state) into view inside the sideways-scrolling frame.
  useEffect(() => {
    if (pinned === undefined) return
    const card = frame.current?.querySelector(`[data-id="${CSS.escape(anchorOf(tree, pinned))}"]`)
    card?.scrollIntoView({ inline: "center", block: "nearest", behavior: "smooth" })
  }, [pinned, tree])
  // A step back is a chip on its source state: light the path to that state.
  const back = backOf(tree, active)
  const route = active === undefined ? undefined : pathTo(tree, back?.from ?? active)
  const lit = route && new Set([...route.path, ...route.next, ...back ? [back.from, back.to] : []])
  const { shown, hidden: folded } = visibleOf(tree, pinned, expanded)
  const nodes: Node[] = tree.nodes.filter(({ id }) => shown.has(id)).map((node) => {
    // Top to bottom: each layer (a state, then the steps out of it) is a row; its branches sit side by side.
    const position = { x: node.row * (CARD_W + GAP_X), y: node.column * (CARD_H + GAP_Y) }
    const common = { faded: lit !== undefined && !lit.has(node.id), lit: node.id === active || node.id === back?.from, pinned, onPick: pin, onHover: setHover }
    if (node.kind === "state") {
      return { id: node.id, type: "state", position, data: { ...common, label: state(node.state), start: lifecycle.initial.includes(node.state), terminal: lifecycle.terminal?.includes(node.state) ?? false, laws: workflow.laws.length,
        backs: (tree.backs.get(node.state) ?? []).map(({ transition, to }) => ({ id: stepId(transition, node.state), label: `${state(transition.id)} → ${state(to)}`, to, toLabel: state(to) })) } }
    }
    const guards = node.transition.guards ?? []
    const fields = guardedFields(guards)
    const plain = guardsWords(guards)
    return { id: node.id, type: "step", position, data: { ...common, id: node.id, order: node.order, label: state(node.transition.id), sub: node.transition.description ?? (guards.length === 0 ? "Always" : `Requires: ${plain ?? fields.join(", ")}`) } }
  })
  if (folded > 0) {
    const below = Math.max(...nodes.map(({ position }) => position.y)) + CARD_H + GAP_Y
    nodes.push({ id: "more", type: "more", position: { x: 0, y: below }, data: { count: folded, onExpand: () => setExpanded(true) } })
  }
  const edges: Edge[] = tree.edges.filter(({ source, target }) => shown.has(source) && shown.has(target)).map(({ source, target }) => ({
    id: `${source}>${target}`, source, target, type: "smoothstep", pathOptions: { borderRadius: 10 },
    markerEnd: { type: MarkerType.ArrowClosed, width: 14, height: 14, color: lit?.has(source) && lit.has(target) ? "var(--edge-active)" : "var(--edge)" },
    style: { stroke: lit?.has(source) && lit.has(target) ? "var(--edge-active)" : "var(--edge)", strokeWidth: lit?.has(source) && lit.has(target) ? 2 : 1.5, opacity: lit && !(lit.has(source) && lit.has(target)) ? 0.3 : 1, transition: "opacity 200ms" },
  }))
  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    if (event.key === "Escape") { pin(undefined); setHover(undefined); return }
    // A focused card or chip handles its own Enter and Space as a click.
    if (event.target !== event.currentTarget && (event.key === "Enter" || event.key === " ")) return
    if (event.key === "Enter" && active) { pin(active); return }
    if (!event.key.startsWith("Arrow")) return
    event.preventDefault()
    // A pinned step back is a chip on its source state; move from that state.
    const next = moveFrom(tree, anchorOf(tree, active ?? tree.spine[0] ?? ""), event.key)
    if (next) { setHover(next); pin(next) }
  }
  // Fitted to the panel: refit when the panel or the tree changes size, so the last layer is never cut off.
  const flow = useRef<ReactFlowInstance | null>(null)
  const rows = Math.max(...nodes.map(({ position }) => position.y)) + CARD_H
  useEffect(() => {
    const element = frame.current
    const fit = () => void flow.current?.fitView({ padding: 0.04, maxZoom: 1, minZoom: 0.6 })
    fit()
    const observer = element === null || typeof ResizeObserver === "undefined" ? undefined : new ResizeObserver(fit)
    if (element) observer?.observe(element)
    return () => observer?.disconnect()
  }, [nodes.length, rows])
  return (
    <div ref={frame} role="group" className="w-full rounded-lg outline-none focus-visible:ring-3 focus-visible:ring-ring/30" tabIndex={0} onKeyDown={onKeyDown} aria-label={`${words(workflow.objectType)} lifecycle: arrow keys move along steps, Enter pins, Escape clears`}>
      <div style={{ height: rows + 48 }}>
        <ReactFlow nodes={nodes} edges={edges} nodeTypes={NODE_TYPES}
          // Node handlers also give the cards pointer events; without them React Flow makes static nodes inert.
          onNodeMouseEnter={(_, node) => setHover(node.id === "more" ? undefined : node.id)} onNodeMouseLeave={() => setHover(undefined)}
          onInit={(instance) => { flow.current = instance; void instance.fitView({ padding: 0.04, maxZoom: 1, minZoom: 0.6 }) }}
          colorMode="system" fitView fitViewOptions={{ padding: 0.04, maxZoom: 1, minZoom: 0.6 }} panOnDrag={false} panOnScroll={false} zoomOnScroll={false} zoomOnPinch={false} zoomOnDoubleClick={false} preventScrolling={false}
          nodesDraggable={false} nodesConnectable={false} elementsSelectable={false} nodesFocusable={false} edgesFocusable={false} proOptions={{ hideAttribution: true }}>
          <Background />
        </ReactFlow>
      </div>
    </div>
  )
}

/** The pinned element's place on its path, as a breadcrumb. */
function Breadcrumb({ tree, pinned }: { readonly tree: Tree; readonly pinned: string }) {
  const state = useNames()
  const back = backOf(tree, pinned)
  const path = back ? [...pathTo(tree, back.from).path, pinned] : pathTo(tree, pinned).path
  const name = (id: string) => {
    if (back && id === pinned) return `${state(back.transition.id)} (back to ${state(back.to)})`
    const node = tree.nodes.find((each) => each.id === id)
    return node === undefined ? id : state(node.kind === "state" ? node.state : node.transition.id)
  }
  const steps = path.filter((id) => id === pinned && back !== undefined || tree.nodes.find((node) => node.id === id)?.kind === "step").length
  return <p className="px-1 text-meta text-muted-foreground">{steps} {steps === 1 ? "step" : "steps"} from the start · {path.map(name).join(" › ")}</p>
}

/** The step's declared description, else the fields its guard reads, with the declared rule one click away. */
function Condition({ transition }: { readonly transition: Workflow["steps"][number]["transition"] }) {
  const guards = transition.guards ?? []
  if (guards.length === 0) return <span className="text-muted-foreground">Always</span>
  const fields = guardedFields(guards)
  return (
    <div className="space-y-1">
      <div>{transition.description ?? (guardsWords(guards) !== null ? `Requires: ${guardsWords(guards)}` : fields.length > 0 ? `Requires: ${fields.join(", ")}` : "A declared condition")}</div>
      <details className="text-meta text-muted-foreground">
        <summary className="cursor-pointer select-none">Declared rule</summary>
        <code className="block pt-1 font-mono text-xs break-words whitespace-normal">{guards.join(" and ")}</code>
      </details>
    </div>
  )
}

/** The focused step: what it changes, when it is allowed, what fires it and the laws that govern it. */
function StepCard({ workflow, step }: { readonly workflow: Workflow; readonly step: Workflow["steps"][number] }) {
  const state = useNames()
  const { transition, actions } = step
  return (
    <div className="grid gap-4 p-4 text-sm md:grid-cols-[1fr_1fr_1.4fr] 2xl:grid-cols-1">
      <div className="md:col-span-3 2xl:col-span-1">
        <div className="text-panel font-medium">{state(transition.id)}</div>
        <div className="text-muted-foreground">{transition.from.map(state).join(" or ")} → {state(transition.to)}</div>
      </div>
      <div className="space-y-1">
        <div className="text-meta font-medium text-muted-foreground">Allowed only when</div>
        <Condition transition={transition} />
      </div>
      <div className="space-y-1">
        <div className="text-meta font-medium text-muted-foreground">Action</div>
        {actions.length === 0 && <div className="text-muted-foreground">None declared</div>}
        {actions.map((action) => (
          <div key={action.actionId} className="space-y-1">
            <Button size="sm" variant="outline" disabled>{action.title ?? words(action.actionId)}</Button>
            <div className="text-meta text-muted-foreground">Not available yet: the server does not accept actions here yet.</div>
          </div>
        ))}
      </div>
      <div className="space-y-1">
        <div className="text-meta font-medium text-muted-foreground">Laws that govern it</div>
        {workflow.laws.length === 0 && <div className="text-muted-foreground">None declared on {words(workflow.objectType)}</div>}
        {workflow.laws.map((law) => (
          <div key={law.id} className="space-y-1 rounded-md border bg-background p-3">
            <div className="flex flex-wrap items-center gap-2">
              <span className="font-mono text-xs">{law.id}</span>
              <Badge variant={law.severity === "error" ? "secondary" : "outline"}>{law.severity === "error" ? "Must hold" : "Warning"}</Badge>
            </div>
            <p className="text-meta text-muted-foreground">{law.rationale}</p>
          </div>
        ))}
      </div>
    </div>
  )
}

function WorkflowCard({ workflow, title, flow, recorded }: { readonly workflow: Workflow; readonly title: string; readonly flow: RunFlow | undefined; readonly recorded: boolean }) {
  const { lifecycle } = workflow
  const { view } = useView()
  const state = useMemo(() => (name: string) => (view !== "technical" && flow ? flowName(flow, name) : undefined) ?? words(name), [flow, view])
  const tree = useMemo(() => treeOf(lifecycle), [lifecycle])
  const [pinned, setPinned] = useState<string>()
  const node = tree.nodes.find(({ id }) => id === pinned)
  const back = backOf(tree, pinned)
  const stepped = node?.kind === "step" ? node.transition : back?.transition
  const step = stepped && workflow.steps.find(({ transition }) => transition.id === stepped.id)
  // A generated flow's run reads in plain words in the business view; the builder's own note stays under Technical.
  const about = view !== "technical" && flow ? `Each run of the ${flow.title.toLowerCase()} automation moves through these states.` : workflow.description
  return (
    <Names value={state}>
    <section id={anchor(workflow)} className="scroll-mt-4 space-y-3">
      <div className="space-y-1 px-1">
        <div className="flex flex-wrap items-center gap-2">
          <h2 className="text-section font-medium">{title}: {words(lifecycle.property).toLowerCase()}</h2>
          {recorded && <Badge variant="outline" title="Definitions the server admitted, replayed exactly. Not a live connection.">Recorded</Badge>}
        </div>
        <p className="max-w-[80ch] text-meta text-muted-foreground">
          {[about,
            `Starts as ${lifecycle.initial.map(state).join(" or ")}.`,
            lifecycle.terminal?.length ? `Ends as ${lifecycle.terminal.map(state).join(" or ")}.` : undefined,
            lifecycle.immutable?.length ? `Its details are locked while ${lifecycle.immutable.map(state).join(" or ")}.` : undefined,
          ].filter(Boolean).join(" ")}
        </p>
      </div>
      {/* The chart takes the full width; what is pinned opens beside it, and only then. */}
      <div className={cn("grid gap-3", pinned !== undefined && "xl:grid-cols-[minmax(0,1fr)_20rem]")}>
        <Panel label="Steps" action="Select a state or step to see its condition, action and laws; arrow keys move, Esc clears">
          {pinned && <Breadcrumb tree={tree} pinned={pinned} />}
          <Well className="bg-canvas-surface"><Chart workflow={workflow} pinned={pinned} onPin={setPinned} /></Well>
        </Panel>
        {pinned !== undefined && (
          <Panel label={node?.kind === "state" ? "Pinned state" : "Pinned step"} action={<Button type="button" size="sm" variant="ghost" className="h-7 px-2" onClick={() => setPinned(undefined)}>Clear</Button>}>
            <Well>
              {step && <StepCard workflow={workflow} step={step} />}
              {node?.kind === "state" && <StateSummary workflow={workflow} name={node.state} />}
            </Well>
          </Panel>
        )}
      </div>
    </section>
    </Names>
  )
}

/** A pinned state: the steps that reach it and leave it, and whether it ends or locks the record. */
function StateSummary({ workflow, name }: { readonly workflow: Workflow; readonly name: string }) {
  const state = useNames()
  const into = workflow.steps.filter(({ transition }) => transition.to === name).map(({ transition }) => state(transition.id))
  const out = workflow.steps.filter(({ transition }) => transition.from.includes(name)).map(({ transition }) => `${state(transition.id)} → ${state(transition.to)}`)
  return (
    <div className="space-y-2 p-4 text-sm">
      <div className="text-panel font-medium">{state(name)}</div>
      <div><span className="text-muted-foreground">Reached by: </span>{into.join(", ") || "nothing; it is where records start"}</div>
      <div><span className="text-muted-foreground">Leads to: </span>{out.join(", ") || "nothing; it ends here"}</div>
      {workflow.lifecycle.immutable?.includes(name) && <div className="text-muted-foreground">Its details are locked here.</div>}
    </div>
  )
}

const anchor = (workflow: Workflow) => `workflow-${workflow.objectType}-${workflow.lifecycle.property}`

/** A workflow's name: the title of the flow whose runs it is, or its object type in words. */
const titleOf = (workflow: Workflow, flows: ReadonlyMap<string, RunFlow>) => flows.get(workflow.objectType)?.title ?? words(workflow.objectType)

/** The admitted lifecycles, for the submenu island. */
export function useWorkflowsMenu(): readonly MenuEntry[] | undefined {
  const definitions = useQuery({ queryKey: ["definitions"], queryFn: transport.definitions })
  const flows = useRunTypeFlows()
  return definitions.data && workflows(definitions.data.map(({ document }) => document))
    .map((workflow) => ({ href: `#${anchor(workflow)}`, label: titleOf(workflow, flows) }))
}

export function WorkflowsPage() {
  const definitions = useQuery({ queryKey: ["definitions"], queryFn: transport.definitions })
  const flows = useRunTypeFlows()
  return (
    <section className="space-y-6">
      <PageHeader title="Workflows" description="How each business process moves: its states, the steps between them, the condition each step requires, and the laws that govern it." />
      <Automations />
      <LoadState
        status={definitions.status}
        data={definitions.data}
        error={definitions.error}
        isFetching={definitions.isFetching}
        onRetry={() => void definitions.refetch()}
        skeleton="cards"
        loadingLabel="Reading workflows"
        isEmpty={(docs) => workflows(docs.map(({ document }) => document)).length === 0}
        empty={{
          icon: WorkflowIcon,
          title: "No workflows yet",
          description: "No record type in this workspace declares lifecycle states yet.",
          next: <Button asChild size="sm" variant="outline"><Link to="/sources"><Database />Open Sources</Link></Button>,
        }}
      >
        {(docs) => (
          <div className="space-y-6">
            {workflows(docs.map(({ document }) => document)).map((workflow) => (
              <WorkflowCard key={`${workflow.objectType}:${workflow.lifecycle.property}`} workflow={workflow} title={titleOf(workflow, flows)} flow={flows.get(workflow.objectType)} recorded={docs.some(({ origin }) => origin === "recorded")} />
            ))}
          </div>
        )}
      </LoadState>
    </section>
  )
}
