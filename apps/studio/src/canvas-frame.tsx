import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react"
import { Background, Controls, ReactFlow, ReactFlowProvider, useReactFlow, type Connection, type Edge, type Node, type NodeTypes } from "@xyflow/react"
import "@xyflow/react/dist/style.css"
import { CircleAlert, Copy, Redo2, Save, Undo2 } from "lucide-react"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { Panel, Well } from "@/components/panel"
import { cn } from "@/lib/utils"
import * as g from "./lib/canvas/graph.ts"

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
  readonly issues: (graph: g.Graph) => { readonly issues: readonly Issue[]; readonly status: ReactNode }
  readonly toNode: (node: g.CanvasNode, issues: readonly Issue[], selected: boolean) => Node
  readonly toEdge: (edge: g.CanvasEdge, issues: readonly Issue[], selected: boolean) => Edge
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
      else if (key === "d") act(() => duplicate(g.subgraph(graph, copyable(selection.nodes))))
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
    if (issue.node) { setSelection({ nodes: [issue.node], edges: [] }); void flow.fitView({ nodes: [{ id: issue.node }], duration: 300, maxZoom: 1.2 }) }
    else if (issue.edge) {
      const edge = graph.edges.find(({ id }) => id === issue.edge)
      setSelection({ nodes: [], edges: [issue.edge] })
      if (edge) void flow.fitView({ nodes: [{ id: edge.source }, { id: edge.target }], duration: 300, maxZoom: 1.2 })
    }
  }
  const editing: Editing = { graph, selection, edit, select: setSelection }

  return (
    <div className="grid gap-3 2xl:grid-cols-[minmax(0,1fr)_22rem]">
      <Panel label={<span className="flex items-center gap-2">{props.label}{dirty && <Badge variant="outline" data-testid="dirty">Unsaved changes{draftAt ? " · draft kept" : ""}</Badge>}</span>} action={status}>
        <div className="flex flex-wrap items-center gap-1.5 px-2.5 pb-2">
          {props.palette(editing)}
          <Button size="sm" variant="outline" aria-label="Duplicate" title="Duplicate (⌘D)" disabled={copyable(selection.nodes).length === 0} onClick={() => duplicate(g.subgraph(graph, copyable(selection.nodes)))}><Copy /></Button>
          <Button size="sm" variant="outline" aria-label="Undo" title="Undo (⌘Z)" disabled={state.past.length === 0} onClick={() => setState(g.undo)}><Undo2 /></Button>
          <Button size="sm" variant="outline" aria-label="Redo" title="Redo (⌘⇧Z)" disabled={state.future.length === 0} onClick={() => setState(g.redo)}><Redo2 /></Button>
          <Button size="sm" variant="outline" title="Save a draft (⌘S)" onClick={saveDraft}><Save />Save draft</Button>
          <Button size="sm" disabled title={props.saveBlocked}>Save to server</Button>
          {dirty && <Button size="sm" variant="ghost" onClick={discard}>Discard draft</Button>}
        </div>
        {notice && <p role={notice.tone === "refusal" ? "alert" : "status"} className={notice.tone === "refusal" ? "px-3 pb-2 text-meta text-destructive" : "px-3 pb-2 text-meta text-muted-foreground"}>{notice.text}</p>}
        <Well>
          <div className="relative h-[36rem] w-full">
            {graph.nodes.length === 0 && <div className="pointer-events-none absolute inset-0 z-10 flex items-center justify-center p-6 text-center text-sm text-muted-foreground">{props.empty}</div>}
            <ReactFlow
              nodes={graph.nodes.map((node) => props.toNode(node, on("node", node.id), selection.nodes.includes(node.id)))}
              edges={graph.edges.map((edge) => props.toEdge(edge, on("edge", edge.id), selection.edges.includes(edge.id)))}
              nodeTypes={props.nodeTypes} fitView fitViewOptions={{ padding: 0.1, maxZoom: 1 }} proOptions={{ hideAttribution: true }}
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
              onDelete={({ nodes, edges }) => { edit(g.remove(graph, removable(nodes.map(({ id }) => id)), edges.map(({ id }) => id))); setSelection(NONE) }}>
              <Background /><Controls showInteractive={false} />
            </ReactFlow>
          </div>
        </Well>
      </Panel>
      <div className="space-y-3">
        <Panel label="Inspector">
          <Well className="space-y-3 p-3 text-sm">{props.inspector(editing, issues)}</Well>
        </Panel>
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
      </div>
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
export function Unavailable({ reason, children, className }: { readonly reason: string; readonly children: string; readonly className?: string }) {
  return (
    <span title={reason} className={cn("inline-flex cursor-not-allowed", className)}>
      <Button size="sm" variant="outline" disabled className="border-dashed text-muted-foreground">{children}<span className="sr-only">: {reason}</span></Button>
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
