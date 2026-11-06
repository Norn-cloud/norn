import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState, useSyncExternalStore, type ReactNode } from "react"
import { Background, BackgroundVariant, Controls, MarkerType, MiniMap, Panel as FlowPanel, ReactFlow, ReactFlowProvider, useReactFlow, type Connection, type Edge, type EdgeTypes, type Node, type NodeTypes } from "@xyflow/react"
import "@xyflow/react/dist/style.css"
import { CircleAlert, Copy, Map as MapIcon, Redo2, Save, Undo2, X } from "lucide-react"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { Panel, Well } from "@/components/panel"
import { cn } from "@/lib/utils"
import * as g from "./lib/canvas/graph.ts"
import { READABLE, openingViewport, type Viewport } from "./lib/canvas/viewport.ts"

/** A finding shown on the canvas, from the canvas itself, from lowering, or from the checker. */
export interface Issue {
  readonly code: string
  readonly message: string
  readonly node?: string
  readonly edge?: string
}

export interface Selection {
  readonly nodes: readonly string[]
  readonly edges: readonly string[]
}

const NONE: Selection = { nodes: [], edges: [] }

/** What an inspector may do: change the graph as one undoable step, change the selection, or report a refusal. */
export interface Editing {
  readonly graph: g.Graph
  readonly selection: Selection
  readonly edit: (next: g.Graph | g.Refusal) => void
  readonly select: (selection: Selection) => void
}

export interface CanvasFrameProps {
  /** Names the draft kept in this browser: `<surface>:<definition>`. */
  readonly draftKey: string
  readonly label: string
  readonly initial: g.Graph
  readonly kinds: g.Kinds
  readonly nodeTypes: NodeTypes
  /** A node's card size, known from its kind and content (cards have fixed widths), for the opening viewport. */
  readonly nodeSize?: (node: g.CanvasNode) => { readonly width: number; readonly height: number }
  /** Custom edge renderers (an automation's exit labels), when a surface needs them. */
  readonly edgeTypes?: EdgeTypes
  readonly issues: (graph: g.Graph) => { readonly issues: readonly Issue[]; readonly status: ReactNode }
  readonly toNode: (node: g.CanvasNode, issues: readonly Issue[], selected: boolean) => Node
  /** Draws one edge; the graph lets a label read its node (a route's condition, a decision's option). */
  readonly toEdge: (edge: g.CanvasEdge, issues: readonly Issue[], selected: boolean, graph: g.Graph) => Edge
  /** The edge a new connection draws; `undefined` lets the frame refuse it. */
  readonly newEdge: (connection: Connection, graph: g.Graph) => g.CanvasEdge
  /** Copies rename what must stay unique (a lifecycle step id). */
  readonly copyEdgeConfig?: (edge: g.CanvasEdge, taken: readonly string[]) => g.CanvasEdge["config"]
  /** Nodes that may never be deleted or copied (an automation's start). */
  readonly fixed?: (node: g.CanvasNode) => boolean
  readonly palette: (editing: Editing) => ReactNode
  readonly inspector: (editing: Editing, issues: readonly Issue[]) => ReactNode
  readonly empty: ReactNode
  /** Why the server save is not available, until the admission route is live. */
  readonly saveBlocked: string
  /** Shown under the canvas for the current selection (a decision's preview). */
  readonly below?: (editing: Editing) => ReactNode
  /** Open with these selected (a link to a finding, or a contact-sheet state). */
  readonly initialSelection?: Selection
  /** Open with this notice (a contact-sheet state). */
  readonly initialNotice?: { readonly tone: "refusal" | "info"; readonly text: string }
}

interface Draft { readonly graph: g.Graph; readonly at: string }

function readDraft(key: string): Draft | undefined {
  try {
    const text = window.localStorage.getItem(`norn.canvas.draft.${key}`)
    return text ? JSON.parse(text) as Draft : undefined
  } catch { return undefined }
}

export function writeDraft(key: string, draft: Draft | undefined): boolean {
  try {
    if (draft) window.localStorage.setItem(`norn.canvas.draft.${key}`, JSON.stringify(draft))
    else window.localStorage.removeItem(`norn.canvas.draft.${key}`)
    return true
  } catch { return false }
}

// Text fields keep their own undo and copy; a checkbox or a button does not.
const TEXT = new Set(["text", "search", "number", "email", "url", "tel", "password"])
const typing = (target: EventTarget | null) => (target instanceof HTMLInputElement && TEXT.has(target.type)) || target instanceof HTMLTextAreaElement || target instanceof HTMLSelectElement

let clipboard: g.Graph | undefined

function Frame(props: CanvasFrameProps) {
  const { draftKey, initial, kinds } = props
  const restored = useRef(readDraft(draftKey))
  const [state, setState] = useState(() => g.history(restored.current?.graph ?? initial))
  const [draftAt, setDraftAt] = useState(restored.current?.at)
  const [selection, setSelection] = useState<Selection>(props.initialSelection ?? NONE)
  const [notice, setNotice] = useState<{ readonly tone: "refusal" | "info"; readonly text: string } | null>(props.initialNotice
    ?? (restored.current ? { tone: "info", text: `Restored your unsaved draft from ${new Date(restored.current.at).toLocaleString()}.` } : null))
  const flow = useReactFlow()
  const wide = useWide()
  const [map, setMap] = useState(true)
  const [drawer, setDrawer] = useState(false)
  const closeDrawer = useCallback(() => setDrawer(false), [])
  const surface = useRef<HTMLDivElement>(null)
  // Whether the canvas is in use: the last pointer-down landed on it, or keyboard focus moved into it
  // (React Flow nodes take focus and selection from the keyboard). Its shortcuts apply only then.
  const canvasActive = useRef(false)
  useEffect(() => {
    const inCanvas = (target: EventTarget | null) => target instanceof Element && target.closest("[data-canvas-surface]") !== null
    const pointer = (event: PointerEvent) => {
      canvasActive.current = inCanvas(event.target)
      // React Flow keeps the browser from clearing a page text selection on the canvas; clear it, so copy means the canvas.
      if (canvasActive.current) window.getSelection()?.removeAllRanges()
    }
    const focus = (event: FocusEvent) => { canvasActive.current = inCanvas(event.target) }
    window.addEventListener("pointerdown", pointer, true)
    window.addEventListener("focusin", focus, true)
    return () => { window.removeEventListener("pointerdown", pointer, true); window.removeEventListener("focusin", focus, true) }
  }, [])
  const graph = state.present
  const dirty = JSON.stringify(graph) !== JSON.stringify(initial)

  const edit = useCallback((next: g.Graph | g.Refusal) => {
    if (g.isRefusal(next)) { setNotice({ tone: "refusal", text: next.message }); return }
    setNotice(null)
    setState((current) => g.commit(current, next))
  }, [])

  // Autosave: every change is written at once (a debounce loses the last edit when the tab closes) and kept until discarded or saved.
  useEffect(() => {
    if (!dirty) { writeDraft(draftKey, undefined); setDraftAt(undefined); return }
    const at = new Date().toISOString()
    if (writeDraft(draftKey, { graph, at })) setDraftAt(at)
  }, [graph, dirty, draftKey])

  useEffect(() => {
    if (!dirty) return
    const warn = (event: BeforeUnloadEvent) => { event.preventDefault() }
    window.addEventListener("beforeunload", warn)
    return () => window.removeEventListener("beforeunload", warn)
  }, [dirty])

  const removable = (ids: readonly string[]) => ids.filter((id) => !graph.nodes.some((node) => node.id === id && props.fixed?.(node)))
  const copyable = (ids: readonly string[]) => removable(ids)
  const duplicate = (piece: g.Graph) => {
    if (piece.nodes.length === 0) return
    // The clipboard is shared across canvases; paste only what this canvas can hold.
    if (piece.nodes.some(({ kind }) => !(kind in kinds))) { setNotice({ tone: "refusal", text: "Those items were copied from another kind of canvas and cannot be pasted here." }); return }
    const { graph: next, nodes } = g.paste(graph, piece, 40, props.copyEdgeConfig)
    edit(next)
    setSelection({ nodes, edges: [] })
  }
  const saveDraft = () => {
    const at = new Date().toISOString()
    setNotice(writeDraft(draftKey, dirty ? { graph, at } : undefined)
      ? { tone: "info", text: `Draft saved in this browser. ${props.saveBlocked}` }
      : { tone: "refusal", text: "This browser does not allow saving a draft here." })
    if (dirty) setDraftAt(at)
  }
  const discard = () => {
    writeDraft(draftKey, undefined)
    setState(g.history(initial))
    setSelection(NONE)
    setDraftAt(undefined)
    setNotice({ tone: "info", text: "Draft discarded; showing the admitted definition." })
  }

  useEffect(() => {
    const keys = (event: KeyboardEvent) => {
      if (!(event.ctrlKey || event.metaKey) || typing(event.target)) return
      const key = event.key.toLowerCase()
      const act = (run: () => void) => { event.preventDefault(); run() }
      if (key === "z" && !event.shiftKey) act(() => setState(g.undo))
      else if ((key === "z" && event.shiftKey) || key === "y") act(() => setState(g.redo))
      // Copy, paste, duplicate and select-all belong to the canvas only while it is in use, and never take over
      // copying text the reader has selected elsewhere (a finding's message in Problems).
      else if (["c", "v", "d", "a"].includes(key) && (!canvasActive.current || event.shiftKey || event.altKey
        || (["c", "a"].includes(key) && (window.getSelection()?.toString() ?? "") !== ""))) return
      else if (key === "d") act(() => duplicate(g.subgraph(graph, copyable(selection.nodes))))
      else if (key === "c" && copyable(selection.nodes).length === 0) return
      else if (key === "c") act(() => { clipboard = g.subgraph(graph, copyable(selection.nodes)); setNotice({ tone: "info", text: `Copied ${clipboard.nodes.length} ${clipboard.nodes.length === 1 ? "item" : "items"}.` }) })
      else if (key === "v" && clipboard) act(() => clipboard && duplicate(clipboard))
      else if (key === "a") act(() => setSelection({ nodes: graph.nodes.map(({ id }) => id), edges: graph.edges.map(({ id }) => id) }))
      else if (key === "s") act(saveDraft)
    }
    window.addEventListener("keydown", keys)
    return () => window.removeEventListener("keydown", keys)
  })

  const { issues, status } = useMemo(() => props.issues(graph), [props.issues, graph])
  const on = (key: "node" | "edge", id: string) => issues.filter((issue) => issue[key] === id)
  const jump = (issue: Issue) => {
    if (issue.node) { setSelection({ nodes: [issue.node], edges: [] }); void flow.fitView({ nodes: [{ id: issue.node }], duration: 300, maxZoom: 1.2, minZoom: READABLE }) }
    else if (issue.edge) {
      const edge = graph.edges.find(({ id }) => id === issue.edge)
      setSelection({ nodes: [], edges: [issue.edge] })
      if (edge) void flow.fitView({ nodes: [{ id: edge.source }, { id: edge.target }], duration: 300, maxZoom: 1.2, minZoom: READABLE })
    }
  }
  const editing: Editing = { graph, selection, edit, select: setSelection }
  // Below the rail width the inspector is a drawer: selecting something opens it, clearing the selection closes it.
  const selected = selection.nodes.length + selection.edges.length > 0
  useEffect(() => { setDrawer(selected) }, [selected, selection])
  // Where the canvas opens, from the layout and the container's size: no measurement, so nothing is hidden or reframed.
  const [opening, setOpening] = useState<Viewport | null>(null)
  useLayoutEffect(() => {
    const box = surface.current?.getBoundingClientRect()
    const boxes = graph.nodes.map((node) => ({ ...node.position, ...(props.nodeSize?.(node) ?? DEFAULT_SIZE) }))
    // Framed on where it starts: an automation's start, else a lifecycle's initial state.
    const start = graph.nodes.findIndex(({ kind }) => kind === "start")
    const at = start >= 0 ? start : graph.nodes.findIndex(({ config }) => config.initial === "yes")
    setOpening(openingViewport(boxes, { width: box?.width ?? 0, height: box?.height ?? 0 }, boxes[at]))
  }, []) // Once, for the graph the canvas opened with.
  // The path in focus: selected connections and every connection of a selected node; a finding outranks it.
  const look = (edge: g.CanvasEdge): Edge => {
    const base = props.toEdge(edge, on("edge", edge.id), selection.edges.includes(edge.id), graph)
    const failing = on("edge", edge.id).length > 0
    const active = selection.edges.includes(edge.id) || selection.nodes.includes(edge.source) || selection.nodes.includes(edge.target)
    const tone = failing ? "var(--destructive)" : active ? "var(--edge-active)" : "var(--edge)"
    return { ...base, className: cn(base.className, failing ? "canvas-edge-failing" : active && "canvas-edge-active"), markerEnd: { type: MarkerType.ArrowClosed, color: tone, width: 14, height: 14 } }
  }
  const Sep = () => <span aria-hidden className="mx-0.5 h-5 w-px bg-border" />

  const inspector = props.inspector(editing, issues)
  const problems = (
    <Panel label="Problems" action={issues.length === 0 ? "None" : `${issues.length}`}>
      <Well className="space-y-2 p-3" >
        {issues.length === 0 && <p className="text-meta text-muted-foreground">No problems in this definition.</p>}
        {issues.map((issue, index) => (
          <button key={index} type="button" data-testid="problem" onClick={() => jump(issue)} disabled={!issue.node && !issue.edge}
            className="block w-full space-y-0.5 rounded-md border border-destructive/40 bg-background p-2 text-left text-meta enabled:hover:bg-muted">
            <span className="flex items-center gap-1 font-mono text-xs text-destructive"><CircleAlert className="size-3" />{issue.code}</span>
            <span className="block">{issue.message}</span>
            <span className="block text-muted-foreground">{issue.node ? `On ${issue.node}` : issue.edge ? `On step ${issue.edge}` : "On the whole definition"}</span>
          </button>
        ))}
      </Well>
    </Panel>
  )
  return (
    <div className={cn("grid gap-3", wide && "grid-cols-[minmax(0,1fr)_20rem]")}>
      <Panel label={<span className="flex items-center gap-2">{props.label}{dirty && <Badge variant="outline" data-testid="dirty">Unsaved changes{draftAt ? " · draft kept" : ""}</Badge>}</span>} action={status}>
        {notice && <p role={notice.tone === "refusal" ? "alert" : "status"} className={notice.tone === "refusal" ? "px-3 pb-2 text-meta text-destructive" : "px-3 pb-2 text-meta text-muted-foreground"}>{notice.text}</p>}
        <Well>
          <div ref={surface} data-canvas-surface className="relative h-[38rem] w-full">
            {graph.nodes.length === 0 && <div className="pointer-events-none absolute inset-0 z-10 flex items-center justify-center p-6 text-center text-sm text-muted-foreground">{props.empty}</div>}
            {opening && <ReactFlow defaultViewport={opening}
              nodes={graph.nodes.map((node) => {
                // The card's known size as React Flow's initial size: the minimap draws only nodes with a size, and these are rebuilt each render.
                const size = props.nodeSize?.(node) ?? DEFAULT_SIZE
                return { ...props.toNode(node, on("node", node.id), selection.nodes.includes(node.id)), initialWidth: size.width, initialHeight: size.height }
              })}
              edges={graph.edges.map(look)} className="bg-canvas-surface"
              nodeTypes={props.nodeTypes} {...props.edgeTypes ? { edgeTypes: props.edgeTypes } : {}} proOptions={{ hideAttribution: true }}
              multiSelectionKeyCode={["Shift", "Meta", "Control"]} selectionKeyCode="Shift" deleteKeyCode={["Backspace", "Delete"]} edgesReconnectable
              isValidConnection={(connection) => !g.isRefusal(g.connect(graph, kinds, { ...props.newEdge(connection as Connection, graph), id: "probe" }))}
              onConnect={(connection) => edit(g.connect(graph, kinds, props.newEdge(connection, graph)))}
              onReconnect={(old, connection) => edit(g.reconnect(graph, kinds, old.id, { source: connection.source, target: connection.target, sourcePort: connection.sourceHandle ?? old.sourceHandle ?? "", targetPort: connection.targetHandle ?? old.targetHandle ?? "" }))}
              onNodeDragStart={() => setState(g.checkpoint)}
              onNodesChange={(changes) => {
                for (const change of changes) {
                  if (change.type === "position" && change.position) setState((current) => g.replace(current, g.moveNode(current.present, change.id, change.position as g.CanvasNode["position"])))
                  if (change.type === "select") setSelection((current) => ({ ...current, nodes: change.selected ? [...new Set([...current.nodes, change.id])] : current.nodes.filter((id) => id !== change.id) }))
                }
              }}
              onEdgesChange={(changes) => {
                for (const change of changes) if (change.type === "select") setSelection((current) => ({ ...current, edges: change.selected ? [...new Set([...current.edges, change.id])] : current.edges.filter((id) => id !== change.id) }))
              }}
              onDelete={({ nodes, edges }) => {
                // A fixed node (an automation's start) stays, and so do its connections unless they were selected themselves.
                const kept = new Set(nodes.map(({ id }) => id).filter((id) => !removable([id]).length))
                const cut = edges.filter(({ id, source, target }) => selection.edges.includes(id) || !(kept.has(source) || kept.has(target))).map(({ id }) => id)
                edit(g.remove(graph, removable(nodes.map(({ id }) => id)), cut)); setSelection(NONE)
              }}>
              <Background variant={BackgroundVariant.Dots} gap={18} size={1.25} color="var(--canvas-dot)" />
              <Controls showInteractive={false} className="canvas-controls" fitViewOptions={{ padding: PADDING, minZoom: READABLE, maxZoom: 1 }} />
              {map && <MiniMap pannable zoomable position="bottom-right" className="canvas-minimap" nodeBorderRadius={6} ariaLabel="Where you are on the canvas" />}
              <FlowPanel position="top-left" className="!m-3">
                <div role="toolbar" aria-label="Canvas actions" className="flex w-max flex-nowrap items-center gap-1 rounded-xl border border-node-border bg-card/95 p-1 shadow-float backdrop-blur-sm">
                  {props.palette(editing)}
                  <Sep />
                  <Button size="sm" variant="ghost" aria-label="Duplicate" title="Duplicate (⌘D)" disabled={copyable(selection.nodes).length === 0} onClick={() => duplicate(g.subgraph(graph, copyable(selection.nodes)))}><Copy /></Button>
                  <Button size="sm" variant="ghost" aria-label="Undo" title="Undo (⌘Z)" disabled={state.past.length === 0} onClick={() => setState(g.undo)}><Undo2 /></Button>
                  <Button size="sm" variant="ghost" aria-label="Redo" title="Redo (⌘⇧Z)" disabled={state.future.length === 0} onClick={() => setState(g.redo)}><Redo2 /></Button>
                  <Button size="sm" variant="ghost" aria-label="Minimap" aria-pressed={map} title={map ? "Hide the minimap" : "Show the minimap"} className={cn(map && "bg-muted")} onClick={() => setMap((on) => !on)}><MapIcon /></Button>
                  <Sep />
                  <Button size="sm" variant="ghost" title="Save a draft (⌘S)" onClick={saveDraft}><Save />Save draft</Button>
                  {dirty && <Button size="sm" variant="ghost" onClick={discard}>Discard draft</Button>}
                  <Unavailable reason={props.saveBlocked}>Save to server</Unavailable>
                </div>
              </FlowPanel>
            </ReactFlow>}
          </div>
        </Well>
        {props.below && <div className="px-1.5 pt-3">{props.below(editing)}</div>}
      </Panel>
      {wide ? <div className="space-y-3"><Panel label="Inspector"><Well className="space-y-4 p-4 text-sm">{inspector}</Well></Panel>{problems}</div> : problems}
      {!wide && <InspectorDrawer open={drawer} onClose={closeDrawer}>{inspector}</InspectorDrawer>}
    </div>
  )
}

/** The editing frame every canvas surface shares, so each verb exists everywhere once. */
export function CanvasFrame(props: CanvasFrameProps) {
  return <ReactFlowProvider><Frame {...props} /></ReactFlowProvider>
}

/**
 * An action that exists but cannot run yet: drawn dashed and muted so it reads as unavailable, with its reason on
 * hover (on the wrapper, because a disabled button takes no pointer events) and in its accessible name.
 */
const PADDING = 0.14
/** A node's size on the canvas when a surface does not say: an automation step card. */
const DEFAULT_SIZE = { width: 208, height: 74 }
/** At this width and wider the inspector is a rail beside the canvas; narrower, it is a drawer. */
export const RAIL_MIN_WIDTH = 1440
const WIDE = `(min-width: ${RAIL_MIN_WIDTH}px)`
const useWide = () => useSyncExternalStore(
  (change) => { const query = window.matchMedia(WIDE); query.addEventListener("change", change); return () => query.removeEventListener("change", change) },
  () => window.matchMedia(WIDE).matches,
  () => true,
)

/** The inspector as a slide-over: it does not hold the canvas (no overlay, focus stays put); Esc or × closes it. */
function InspectorDrawer({ open, onClose, children }: { readonly open: boolean; readonly onClose: () => void; readonly children: ReactNode }) {
  useEffect(() => {
    if (!open) return
    const key = (event: KeyboardEvent) => { if (event.key === "Escape") { event.preventDefault(); onClose() } }
    window.addEventListener("keydown", key)
    return () => window.removeEventListener("keydown", key)
  }, [open, onClose])
  return (
    <aside role="dialog" aria-modal="false" aria-label="Inspector" aria-hidden={!open} inert={!open}
      className={cn("fixed inset-y-3 end-3 z-40 flex w-[min(24rem,calc(100vw-1.5rem))] flex-col rounded-node border border-node-border bg-card shadow-float transition-transform duration-200 ease-out motion-reduce:transition-none",
        open ? "translate-x-0" : "translate-x-[calc(100%+1.5rem)]")}>
      <header className="flex items-center justify-between border-b border-node-border px-4 py-3">
        <h2 className="text-sm font-semibold">Inspector</h2>
        <Button size="icon" variant="ghost" aria-label="Close inspector" title="Close (Esc)" onClick={onClose}><X /></Button>
      </header>
      <div className="min-h-0 flex-1 space-y-4 overflow-y-auto p-4 text-sm">{children}</div>
    </aside>
  )
}

export function Unavailable({ reason, children, className }: { readonly reason: string; readonly children: string; readonly className?: string }) {
  return (
    <span title={reason} className={cn("inline-flex cursor-not-allowed", className)}>
      <Button size="sm" variant="outline" disabled className="border-dashed border-muted-foreground/50 text-muted-foreground disabled:opacity-100">{children}<span className="sr-only">: {reason}</span></Button>
    </span>
  )
}

/** A finding in a side panel. */
export function IssueCard({ issue }: { readonly issue: Issue }) {
  return (
    <div className="space-y-0.5 rounded-md border border-destructive/40 bg-background p-2 text-meta">
      <div className="font-mono text-xs text-destructive">{issue.code}</div>
      <div>{issue.message}</div>
    </div>
  )
}
