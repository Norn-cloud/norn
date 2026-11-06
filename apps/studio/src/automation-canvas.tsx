import { useQuery } from "@tanstack/react-query"
import { useCallback } from "react"
import { BaseEdge, EdgeLabelRenderer, Handle, Position, useStoreApi, getBezierPath, type Connection, type Edge, type EdgeProps, type Node, type NodeProps } from "@xyflow/react"
import { ArrowDown, ArrowUp, Copy, Plus, Trash2 } from "lucide-react"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Textarea } from "@/components/ui/textarea"
import { cn } from "@/lib/utils"
import { CARD, EDGE_PILL, InspectorHead, InspectorSection, KindChip, NodeCard, cardHeight } from "@/components/canvas/node-card"
import { CanvasFrame, IssueCard, Unavailable, type CanvasFrameProps, type Editing, type Issue } from "./canvas-frame.tsx"
import { SAVE_BLOCKED } from "./canvas.tsx"
import { automationKinds, branchesOf, inputsOf, lower, setBranches, type Condition } from "./lib/canvas/automation.ts"
import { loadChecker } from "./lib/canvas/checker.ts"
import * as g from "./lib/canvas/graph.ts"
import { EXAMPLE, RECORDED_SCOPE, RECORDED_TOOLS } from "./lib/canvas/tools.ts"
import { RISKS, decisionView, effectOf, nextOptionId, optionPort, optionsOf, setOptions, type DecisionOption } from "./lib/canvas/decision.ts"
import { DecisionTree } from "./decision-tree.tsx"
import { STEP_SPACING, tidy } from "./lib/canvas/layout.ts"
import { LABEL_ROOM_MAX, exitLabel, exitLabels, formatCondition } from "./lib/canvas/exits.ts"
import { words } from "./lib/format.ts"

const META = { id: "studio.automation", version: "0.1.0" }
export const STEP_KINDS = { start: "Start", perform: "Tool", decide: "Ask a person", route: "Route", sleep: "Wait", succeed: "Outcome" } as const
export const RUN_BLOCKED = "Running happens in the cell with receipts; the run route is not live yet."

type StepNode = Node<{ kind: string; title: string; detail: string; issues: readonly Issue[]; outs: readonly { readonly id: string; readonly label: string }[] }, "step">

/** Exits sit this far apart down a step's right edge, so their labels (18px tall) never touch. */
const EXIT_PITCH = 22
/** The height a step needs for its exits stacked a pitch apart with a margin; a step grows only when that is taller than its content. */
const exitsHeight = (outs: number) => outs > 1 ? (outs - 1) * EXIT_PITCH + 2 * 12 : undefined
const outsOf = (node: g.CanvasNode) => (automationKinds[node.kind]?.(node) ?? []).filter(({ side }) => side === "out").length

export function StepBox({ data, selected }: NodeProps<StepNode>) {
  return (
    <div title={data.issues.map(({ message }) => message).join("\n")}>
      <NodeCard kind={data.kind} kindLabel={STEP_KINDS[data.kind as keyof typeof STEP_KINDS]} title={data.title} subtitle={data.detail} problems={data.issues.length} selected={selected} minHeight={exitsHeight(data.outs.length)}>
        {data.kind !== "start" && <Handle className="canvas-pill" type="target" position={Position.Left} id="in" />}
        {data.outs.map((port, index) => (
          <Handle key={port.id} className="canvas-pill" type="source" position={Position.Right} id={port.id} title={port.label} style={{ top: `calc(50% + ${(index - (data.outs.length - 1) / 2) * EXIT_PITCH}px)` }} />
        ))}
      </NodeCard>
    </div>
  )
}

const NODE_TYPES = { step: StepBox }

type ExitEdgeData = { readonly label?: string; readonly full?: string }

/**
 * A connection whose label sits at its exit, just past the step it leaves, so a long condition never lands on
 * the next step; the full condition is on hover.
 */
export function ExitEdge({ id, source, sourceX, sourceY, targetX, targetY, sourcePosition, targetPosition, markerEnd, style, data }: EdgeProps<Edge<ExitEdgeData, "exit">>) {
  const [path] = getBezierPath({ sourceX, sourceY, sourcePosition, targetX, targetY, targetPosition })
  // A label is part of its edge: clicking it selects the edge, as clicking the line does.
  const store = useStoreApi()
  // The label never runs under a card: going forward it is clamped to the gap before the target card and ends at
  // its edge (cut with an ellipsis, in full on hover); a gap too small for a word shows none, the exit's handle names it.
  const forward = targetX > sourceX
  const room = forward ? Math.min(LABEL_MAX, targetX - sourceX - 2 * LABEL_GAP) : LABEL_MAX
  return (
    <>
      <BaseEdge id={id} path={path} markerEnd={markerEnd} style={style} />
      {data?.label && room >= LABEL_MIN && (
        <EdgeLabelRenderer>
          <div data-exit-label={id} data-exit-source={source} title={data.full} className={cn(EDGE_PILL, "cursor-pointer")} onClick={() => store.getState().addSelectedEdges([id])}
            style={{ maxWidth: room, transform: `translate(0, -50%) translate(${sourceX + LABEL_GAP}px, ${sourceY}px)` }}>{data.label}</div>
        </EdgeLabelRenderer>
      )}
    </>
  )
}

/** Exit labels: at most this wide, this far from either card, and none narrower than a short word. */
const LABEL_MAX = LABEL_ROOM_MAX
const LABEL_GAP = 8
const LABEL_MIN = 32

const EDGE_TYPES = { exit: ExitEdge }

export { exitLabel }

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
      <div className="text-meta text-muted-foreground">Branches, first match wins. Each branch leaves by its own exit; then otherwise, then next (continue after the route).</div>
      {branches.map((all, index) => (
        <div key={`${id}:${index}:${formatCondition(all)}`} className="space-y-1 rounded-md border p-2">
          <div className="flex items-center gap-1">
            <span className="text-meta font-medium">{index === 0 ? "When" : "Else when"}</span>
            <span className="font-mono text-[10px] text-muted-foreground" title="This branch's exit on the route">exit b{index}</span>
            <span className="ms-auto flex">
              <Button size="icon" variant="ghost" aria-label={`Move b${index} up`} disabled={index === 0} onClick={() => swap(index - 1, index)}><ArrowUp /></Button>
              <Button size="icon" variant="ghost" aria-label={`Move b${index} down`} disabled={index === branches.length - 1} onClick={() => swap(index, index + 1)}><ArrowDown /></Button>
              <Button size="icon" variant="ghost" aria-label={`Delete b${index}`} onClick={() => set(indices.filter((at) => at !== index))}><Trash2 /></Button>
            </span>
          </div>
          {/* The whole condition stays readable: it wraps instead of clipping, and hovering shows it in full. */}
          <Textarea aria-label={`Branch b${index}`} rows={2} className="min-h-0 resize-y font-mono text-xs break-all" title={formatCondition(all) || "No condition yet"} defaultValue={formatCondition(all)} placeholder="invoice.status == paid"
            onBlur={(event) => { const next = branches.map((one, at) => at === index ? parseCondition(event.target.value) : one); if (formatCondition(next[index] ?? []) !== formatCondition(all)) set(indices, next) }} />
        </div>
      ))}
      <Button size="sm" variant="outline" onClick={() => set([...indices, -1], [...branches, []])}><Plus />Branch</Button>
    </div>
  )
}

/** An ask step's options: label, scope, the agent's pick; add, duplicate, reorder, delete. Exits follow their option. */
function Options({ editing, id, options, pick }: { readonly editing: Editing; readonly id: string; readonly options: readonly DecisionOption[]; readonly pick: string }) {
  const { graph, edit } = editing
  const set = (next: readonly DecisionOption[]) => edit(setOptions(graph, id, next))
  const move = (from: number, to: number) => { const next = [...options]; const [one] = next.splice(from, 1); if (one) next.splice(to, 0, one); set(next) }
  return (
    <div className="space-y-2">
      <div className="text-meta text-muted-foreground">Options, in the order the decider sees them</div>
      {options.map((option, index) => (
        <div key={`${option.id}:${option.label}:${option.scope}:${JSON.stringify(option.effect ?? null)}`} className="space-y-1 rounded-md border p-2">
          <div className="flex items-center gap-1">
            <span className="text-meta font-medium">Option {index + 1}</span>
            <span className="ms-auto flex">
            <Button size="icon" variant="ghost" aria-label={`Move option ${index + 1} up`} disabled={index === 0} onClick={() => move(index, index - 1)}><ArrowUp /></Button>
            <Button size="icon" variant="ghost" aria-label={`Move option ${index + 1} down`} disabled={index === options.length - 1} onClick={() => move(index, index + 1)}><ArrowDown /></Button>
            <Button size="icon" variant="ghost" aria-label={`Duplicate option ${index + 1}`} onClick={() => set([...options.slice(0, index + 1), { ...option, id: nextOptionId(options), label: `${option.label} (copy)` }, ...options.slice(index + 1)])}><Copy /></Button>
            <Button size="icon" variant="ghost" aria-label={`Delete option ${index + 1}`} onClick={() => set(options.filter((one) => one.id !== option.id))}><Trash2 /></Button>
            </span>
          </div>
          {/* The label gets the full width, so it reads whole; hovering shows it too. */}
          <Input aria-label={`Option ${index + 1} label`} title={option.label} defaultValue={option.label} onBlur={(event) => { const label = event.target.value.trim(); if (label && label !== option.label) set(options.map((one) => one.id === option.id ? { ...one, label } : one)) }} />
          <div className="flex gap-1">
            <select aria-label={`Option ${index + 1} starts`} className="rounded-md border bg-background px-2 py-1 text-xs" value={effectOf(option)?.kind ?? "binding"}
              onChange={(event) => set(options.map((one) => one.id === option.id ? { ...one, effect: event.target.value === "action" ? { action: effectOf(one)?.ref ?? "" } : { binding: effectOf(one)?.ref ?? "" } } : one))}>
              <option value="binding">Tool binding</option><option value="action">Action</option>
            </select>
            {effectOf(option)?.kind === "action"
              ? <Input aria-label={`Option ${index + 1} action`} className="text-xs" placeholder="action id, e.g. billing.send_reminder" defaultValue={effectOf(option)?.ref} onBlur={(event) => { const ref = event.target.value.trim(); if (ref !== effectOf(option)?.ref) set(options.map((one) => one.id === option.id ? { ...one, effect: { action: ref } } : one)) }} />
              : <select aria-label={`Option ${index + 1} tool binding`} className="w-full rounded-md border bg-background px-2 py-1 text-xs" value={effectOf(option)?.ref ?? ""} onChange={(event) => set(options.map((one) => one.id === option.id ? { ...one, effect: { binding: event.target.value } } : one))}>
                <option value="">Choose a tool binding</option>
                {RECORDED_TOOLS.map(({ id: tool }) => <option key={tool} value={tool}>{tool}</option>)}
              </select>}
          </div>
          <Input aria-label={`Option ${index + 1} scope`} className="text-xs" placeholder="Who it reaches, e.g. #incidents only" defaultValue={option.scope} onBlur={(event) => { const scope = event.target.value.trim(); if (scope !== option.scope) set(options.map((one) => one.id === option.id ? { ...one, scope } : one)) }} />
          <label className="flex items-center gap-2 text-meta"><input type="radio" name={`${id}:pick`} aria-label={`Agent's pick: option ${index + 1}`} checked={pick === option.id} onChange={() => edit(g.configureNode(graph, id, { agentPick: option.id }))} />Agent's pick</label>
        </div>
      ))}
      <Button size="sm" variant="outline" onClick={() => set([...options, { id: nextOptionId(options), label: "New option", scope: "", effect: { binding: "" } }])}><Plus />Option</Button>
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
      <p className="text-meta text-muted-foreground">⌘Z undo · ⌘⇧Z redo · ⌘D duplicate · ⌘C/⌘V copy and paste · ⌘A select all · ⌘S save draft · ⇧⌘L tidy up · Delete removes</p>
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
    <label className="block space-y-1.5"><span className="text-xs font-medium text-foreground/80">{label}</span>
      <Input aria-label={label} key={`${node.id}:${key}:${value ?? ""}`} defaultValue={value} onBlur={(event) => onSave(event.target.value)} {...extra} /></label>
  )
  return (
    <>
      <InspectorHead kind={node.kind} kindLabel={STEP_KINDS[node.kind as keyof typeof STEP_KINDS]} id={node.id} />
      {issues.filter((issue) => issue.node === node.id).map((issue, index) => <IssueCard key={index} issue={issue} />)}
      {node.kind === "start" && <p className="text-muted-foreground">Every automation has one start. Connect it to the first step.</p>}
      {node.kind === "perform" && <>
        <label className="block space-y-1.5"><span className="text-xs font-medium text-foreground/80">Tool</span>
          <select aria-label="Tool" className="h-9 w-full rounded-md border border-input bg-background px-2.5 text-sm shadow-xs outline-none focus-visible:ring-2 focus-visible:ring-ring/50" value={node.config.tool} onChange={(event) => edit(g.configureNode(graph, node.id, { tool: event.target.value, input: "{}" }))}>
            {!tool && <option value={node.config.tool}>{node.config.tool} (not connected)</option>}
            {RECORDED_TOOLS.map(({ id }) => <option key={id} value={id}>{id}</option>)}
          </select></label>
        {field("Name its result", "bind", node.config.bind, (text) => edit(g.configureNode(graph, node.id, { bind: text.trim() })))}
        {Object.entries(tool?.input.properties ?? {}).map(([name, schema]) => (
          <div key={name}>{field(`${name} (${String(schema.type)})`, `input.${name}`, inputsOf(node)[name], (text) => edit(g.configureNode(graph, node.id, { input: JSON.stringify({ ...inputsOf(node), [name]: text }) })), { placeholder: "a value, or result.field" })}</div>
        ))}
      </>}
      {node.kind === "route" && <Branches editing={editing} id={node.id} branches={branchesOf(node)} />}
      {node.kind === "decide" && <>
        <InspectorSection title="The ask">
        {field("Question", "question", node.config.question, (text) => edit(g.configureNode(graph, node.id, { question: text.trim() })), { placeholder: "What should I do?" })}
        <label className="block space-y-1.5"><span className="text-xs font-medium text-foreground/80">Risk</span>
          <select aria-label="Risk" className="h-9 w-full rounded-md border border-input bg-background px-2.5 text-sm shadow-xs outline-none focus-visible:ring-2 focus-visible:ring-ring/50" value={node.config.risk ?? "medium"} onChange={(event) => edit(g.configureNode(graph, node.id, { risk: event.target.value }))}>
            {RISKS.map((risk) => <option key={risk} value={risk}>{words(risk)}</option>)}
          </select></label>
        </InspectorSection>
        <InspectorSection title="Who decides">
        {field("Who decides (roles, comma separated)", "roles", node.config.deciderRoles, (text) => edit(g.configureNode(graph, node.id, { deciderRoles: text.split(",").map((role) => role.trim()).filter(Boolean).join(",") })))}
        {field("Under law (optional)", "law", node.config.deciderLaw, (text) => edit(g.configureNode(graph, node.id, { deciderLaw: text.trim() })), { placeholder: "the law that makes this a person's call" })}
        {field("Expires after (hours, optional)", "expires", node.config.expiresInMs ? String(Number(node.config.expiresInMs) / 3_600_000) : "", (text) => edit(g.configureNode(graph, node.id, { expiresInMs: /^\d+(\.\d+)?$/.test(text.trim()) ? String(Math.round(Number(text) * 3_600_000)) : "" })), { type: "number" })}
        </InspectorSection>
        <Options editing={editing} id={node.id} options={optionsOf(node)} pick={node.config.agentPick ?? ""} />
      </>}
      {node.kind === "sleep" && field("Wait (milliseconds)", "millis", node.config.millis, (text) => edit(g.configureNode(graph, node.id, { millis: text })), { type: "number" })}
      {node.kind === "succeed" && field("Outcome", "outcome", node.config.outcome, (text) => edit(g.configureNode(graph, node.id, { outcome: text })))}
      {node.kind !== "start" && <div className="border-t border-node-border pt-3"><Button size="sm" variant="ghost" className="text-destructive hover:bg-destructive/10 hover:text-destructive" onClick={remove}><Trash2 />Delete</Button></div>}
    </>
  )
}

const DEFAULTS: Record<string, Record<string, string>> = {
  perform: { tool: RECORDED_TOOLS[0]?.id ?? "", input: "{}" },
  decide: { question: "", risk: "medium", deciderRoles: "admin", deciderLaw: "", expiresInMs: "", agentPick: "", options: JSON.stringify([{ id: "o1", label: "Approve", scope: "", effect: { binding: RECORDED_TOOLS[0]?.id ?? "" } }, { id: "o2", label: "Decline", scope: "Nothing happens", effect: { action: "norn.decline" } }]) },
  route: { branches: "[[]]" },
  sleep: { millis: "60000" },
  succeed: { outcome: "done" },
}

/** The frame's props for an automation; the sheet renders the same props with fixed findings. */
export function automationFrame(issues: CanvasFrameProps["issues"], initial: g.Graph = EXAMPLE): CanvasFrameProps {
  return {
    draftKey: `automation:${META.id}`, label: "Automation", initial, kinds: automationKinds, nodeTypes: NODE_TYPES, edgeTypes: EDGE_TYPES, issues,
    saveBlocked: SAVE_BLOCKED, empty: "This automation has no steps. Add one; its start comes with it.",
    fixed: (node) => node.kind === "start",
    tidy: (graph) => tidy(graph, STEP_SPACING),
    nodeSize: (node) => ({ width: CARD.step, height: Math.max(cardHeight({ kindLine: node.kind !== "start", subtitle: node.kind !== "start" }), exitsHeight(outsOf(node)) ?? 0) }),
    toNode: (node, found, selected): StepNode => ({
      id: node.id, type: "step", position: node.position, selected,
      data: {
        kind: node.kind, title: words(node.id), issues: found,
        detail: node.kind === "perform" ? node.config.tool ?? "" : node.kind === "sleep" ? `${node.config.millis ?? 0} ms` : node.kind === "succeed" ? node.config.outcome ?? "" : node.kind === "decide" ? `${optionsOf(node).length} ${optionsOf(node).length === 1 ? "option" : "options"}` : node.kind === "route" ? `${branchesOf(node).length} ${branchesOf(node).length === 1 ? "branch" : "branches"}` : "",
        outs: (automationKinds[node.kind]?.(node) ?? []).filter(({ side }) => side === "out").map(({ id: port }) => ({ id: port, label: exitLabel(node, port) ?? "next (continue after this step)" })),
      },
    }),
    toEdge: (edge, found, selected, graph): Edge => {
      const source = graph.nodes.find(({ id: node }) => node === edge.source)
      const drawn = source ? exitLabels(source).get(edge.sourcePort) : undefined
      return {
        id: edge.id, source: edge.source, target: edge.target, sourceHandle: edge.sourcePort, targetHandle: edge.targetPort, selected, type: "exit",
        data: drawn ? { label: drawn.label, full: drawn.full } : {},
      }
    },
    newEdge: ({ source, target, sourceHandle, targetHandle }: Connection, graph) =>
      ({ id: g.freshName(graph.edges.map(({ id }) => id), `${source}.${sourceHandle ?? "next"}`), source, target, sourcePort: sourceHandle ?? "next", targetPort: targetHandle ?? "in", config: {} }),
    palette: ({ graph, edit, select }) => Object.keys(DEFAULTS).map((kind) => (
      <Button key={kind} size="sm" variant="ghost" className="gap-1.5 pl-1.5" onClick={() => {
        const base = graph.nodes.some((node) => node.kind === "start") ? graph : g.addNode(graph, { id: "start", kind: "start", position: { x: 0, y: 120 }, config: {} })
        const id = g.freshId(base, kind === "perform" ? "call" : kind)
        edit(g.addNode(base, { id, kind, position: { x: 40, y: 360 + graph.nodes.length * 10 }, config: { ...DEFAULTS[kind] } }))
        select({ nodes: [id], edges: [] })
      }}><KindChip kind={kind} className="size-5 rounded-md" />{STEP_KINDS[kind as keyof typeof STEP_KINDS]}</Button>
    )),
    inspector: (editing, found) => <Inspector editing={editing} issues={found} />,
    below: ({ graph, selection }) => {
      const view = selection.nodes.length === 1 && selection.edges.length === 0 ? decisionView(graph, selection.nodes[0] ?? "") : null
      return view ? <DecisionTree key={JSON.stringify(view)} view={view} /> : null
    },
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
      <div className="flex justify-end"><Unavailable reason={RUN_BLOCKED}>Run in the cell</Unavailable></div>
    </div>
  )
}
