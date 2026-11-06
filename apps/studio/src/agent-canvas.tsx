import { useQuery, useQueryClient } from "@tanstack/react-query"
import { useCallback, useMemo, useState } from "react"
import { Handle, MarkerType, Position, type Connection, type Edge, type Node, type NodeProps } from "@xyflow/react"
import { Bot, Copy, Plus, Scale, Trash2, Wrench } from "lucide-react"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Textarea } from "@/components/ui/textarea"
import { EmptyState, Panel, Well } from "@/components/panel"
import { cn } from "@/lib/utils"
import { CanvasFrame, IssueCard, type CanvasFrameProps, type Editing, type Issue } from "./canvas-frame.tsx"
import { SAVE_BLOCKED } from "./canvas.tsx"
import { api, type Binding } from "./lib/connectors.ts"
import { AGENT_NODE, OFFERED_MODELS, agentGraph, agentKinds, agentOf, emptyAgent, lawNodeId, toolNodeId, wiredParts, type AgentDefinition } from "./lib/canvas/agent.ts"
import { loadAgentChecker } from "./lib/canvas/checker.ts"
import * as g from "./lib/canvas/graph.ts"
import { kindStore, type KindStore } from "./lib/canvas/store.ts"
import { words } from "./lib/format.ts"
import { recordedTransport } from "./lib/transport.ts"

export const AGENT_RUN_BLOCKED = "An agent's runs are flow runs in the cell, with a receipt for every tool call; the run route is not live yet."

type AgentPartNode = Node<{ kind: string; title: string; detail: string; issues: readonly Issue[] }, "part">

export function AgentPart({ data, selected }: NodeProps<AgentPartNode>) {
  const failing = data.issues.length > 0
  const Icon = data.kind === "agent" ? Bot : data.kind === "tool" ? Wrench : Scale
  return (
    <div title={data.issues.map(({ message }) => message).join("\n")} className={cn("w-52 rounded-lg border-2 bg-background px-3 py-2 text-sm", data.kind === "agent" && "w-60", selected ? "border-primary ring-2 ring-primary/30" : "border-border", failing && "border-destructive")}>
      {data.kind === "agent" && <><Handle type="target" position={Position.Left} id="tools" style={{ top: "35%" }} title="Tools" /><Handle type="target" position={Position.Right} id="laws" style={{ top: "35%" }} title="Laws" /></>}
      {data.kind === "tool" && <Handle type="source" position={Position.Right} id="out" />}
      {data.kind === "law" && <Handle type="source" position={Position.Left} id="out" />}
      <div className="flex items-center gap-1.5 text-meta text-muted-foreground"><Icon className="size-3.5" />{data.kind === "agent" ? "Agent" : data.kind === "tool" ? "Tool binding" : "Law"}</div>
      <div className="font-medium break-words">{data.title}</div>
      {data.detail && <div className="truncate text-meta text-muted-foreground">{data.detail}</div>}
      {failing && <div className="text-meta text-destructive">{data.issues.length} {data.issues.length === 1 ? "problem" : "problems"}</div>}
    </div>
  )
}

const NODE_TYPES = { part: AgentPart }

/** Where a finding about the payload lands: the i-th wired tool or law its pointer names, else the agent. */
function issueOf(graph: g.Graph, agent: AgentDefinition, code: string, message: string, pointer: string | null): Issue {
  const parts = wiredParts(graph)
  const at = /^\/(tools|laws|decisionPolicy\/laws)\/(\d+)/.exec(pointer ?? "")
  const index = Number(at?.[2])
  const law = at?.[1] === "decisionPolicy/laws" ? agent.decisionPolicy.laws?.[index] : undefined
  const target = at?.[1] === "tools" ? parts.tools[index] : at?.[1] === "laws" ? parts.laws[index] : law !== undefined ? parts.laws.find(({ config }) => config.law === law) : undefined
  return { code, message, node: target?.id ?? AGENT_NODE }
}

function Inspector({ editing, issues, bindings, laws }: { readonly editing: Editing; readonly issues: readonly Issue[]; readonly bindings: readonly Binding[]; readonly laws: readonly string[] }) {
  const { graph, selection, edit, select } = editing
  const node = graph.nodes.find(({ id }) => id === selection.nodes[0])
  const removable = selection.nodes.filter((id) => id !== AGENT_NODE)
  const remove = () => { edit(g.remove(graph, removable, selection.edges)); select({ nodes: [], edges: [] }) }
  const field = (label: string, key: string, extra: { type?: string; placeholder?: string } = {}) => node && (
    <label className="block space-y-1"><span className="text-meta text-muted-foreground">{label}</span>
      <Input aria-label={label} key={`${key}:${node.config[key] ?? ""}`} defaultValue={node.config[key]} onBlur={(event) => edit(g.configureNode(graph, node.id, { [key]: event.target.value.trim() }))} {...extra} /></label>
  )
  if (selection.nodes.length + selection.edges.length > 1) return <><p>{selection.nodes.length} parts and {selection.edges.length} wires selected.</p><Button size="sm" variant="outline" onClick={remove}><Trash2 />Delete selected</Button></>
  if (selection.edges.length === 1) return <><p className="text-muted-foreground">A wire gives the agent this tool or puts it under this law. Delete it to take that away.</p><Button size="sm" variant="outline" onClick={remove}><Trash2 />Delete</Button></>
  if (!node) return <p className="text-muted-foreground">Select the agent to set its model, prompt, budgets and when a person decides. Wire tool bindings into its left side and laws into its right side.</p>
  const own = issues.filter((issue) => issue.node === node.id)
  return (
    <>
      {node.kind === "agent" && <>
        {field("Name", "name", { placeholder: "support.triage" })}
        {field("Title", "title", { placeholder: "Support triage" })}
        <label className="block space-y-1"><span className="text-meta text-muted-foreground">Model</span>
          <select aria-label="Model" className="w-full rounded-md border bg-background px-2 py-1" value={node.config.model} onChange={(event) => edit(g.configureNode(graph, node.id, { model: event.target.value }))}>
            {!OFFERED_MODELS.includes(node.config.model as typeof OFFERED_MODELS[number]) && <option value={node.config.model}>{node.config.model}</option>}
            {OFFERED_MODELS.map((model) => <option key={model} value={model}>{model}</option>)}
          </select></label>
        <label className="block space-y-1"><span className="text-meta text-muted-foreground">Prompt</span>
          <Textarea aria-label="Prompt" rows={6} key={`prompt:${node.config.prompt ?? ""}`} defaultValue={node.config.prompt} onBlur={(event) => edit(g.configureNode(graph, node.id, { prompt: event.target.value }))} /></label>
        {field("Most runs", "maxRuns", { type: "number" })}
        {field("Most tool calls", "maxToolCalls", { type: "number" })}
        {field("Most cost (USD)", "maxCostUsd", { placeholder: "10.00" })}
        <label className="block space-y-1"><span className="text-meta text-muted-foreground">A person decides</span>
          <select aria-label="A person decides" className="w-full rounded-md border bg-background px-2 py-1" value={node.config.approve} onChange={(event) => edit(g.configureNode(graph, node.id, { approve: event.target.value }))}>
            <option value="always">Before every tool call</option><option value="laws">When a marked law applies</option><option value="never">Never</option>
          </select></label>
        {field("Who decides (roles, comma separated)", "deciderRoles")}
      </>}
      {node.kind === "tool" && <>
        <label className="block space-y-1"><span className="text-meta text-muted-foreground">Tool binding</span>
          <select aria-label="Tool binding" className="w-full rounded-md border bg-background px-2 py-1" value={node.config.binding} onChange={(event) => edit(g.configureNode(graph, node.id, { binding: event.target.value }))}>
            {!bindings.some(({ id }) => id === node.config.binding) && <option value={node.config.binding}>{node.config.binding} (not bound here)</option>}
            {bindings.map((binding) => <option key={binding.id} value={binding.id}>{binding.label} · {binding.app}/{binding.tool}</option>)}
          </select></label>
      </>}
      {node.kind === "law" && <>
        <label className="block space-y-1"><span className="text-meta text-muted-foreground">Law</span>
          <select aria-label="Law" className="w-full rounded-md border bg-background px-2 py-1" value={node.config.law} onChange={(event) => edit(g.configureNode(graph, node.id, { law: event.target.value }))}>
            {!laws.includes(node.config.law ?? "") && <option value={node.config.law}>{node.config.law} (not admitted)</option>}
            {laws.map((law) => <option key={law} value={law}>{law}</option>)}
          </select></label>
        <label className="flex items-center gap-2"><input type="checkbox" aria-label="A person decides when this law applies" checked={node.config.decides === "yes"} onChange={(event) => edit(g.configureNode(graph, node.id, { decides: event.target.checked ? "yes" : "" }))} />A person decides when this law applies</label>
      </>}
      {node.id !== AGENT_NODE && <Button size="sm" variant="outline" onClick={remove}><Trash2 />Delete</Button>}
      {own.map((issue, index) => <IssueCard key={index} issue={issue} />)}
    </>
  )
}

const bindingsQuery = () => api<{ bindings: readonly Binding[] }>("/bindings").then(({ bindings }) => bindings)

/** One agent on the canvas: wire tools and laws, set its policy; the checker runs on every edit, the cell decides on save. */
function AgentEditor({ id, stored, store, onSaved, bindings, laws }: { readonly id: string; readonly stored: { readonly version: number; readonly payload: AgentDefinition } | null; readonly store: KindStore<AgentDefinition>; readonly onSaved: () => void; readonly bindings: readonly Binding[]; readonly laws: readonly string[] }) {
  const checker = useQuery({ queryKey: ["checker", "agent"], queryFn: () => loadAgentChecker(), staleTime: Infinity, retry: false })
  const [cellIssues, setCellIssues] = useState<readonly Issue[]>([])
  const initial = useMemo(() => agentGraph(stored?.payload ?? emptyAgent(id)), [stored, id])
  const issues = useCallback((graph: g.Graph) => {
    const agent = agentOf(graph)
    if (!agent) return { issues: [{ code: "canvas.agent.missing", message: "The agent node is missing" }], status: <Badge variant="destructive">1 finding</Badge> }
    if (checker.isError) return { issues: cellIssues, status: <span className="text-destructive" role="alert">{checker.error.message}: no live agent checks</span> }
    if (!checker.data) return { issues: cellIssues, status: <span role="status">Loading the checker…</span> }
    const result = checker.data(agent, { models: [...OFFERED_MODELS], bindings: bindings.map(({ id: binding }) => binding), laws })
    const found = [...result.diagnostics.diagnostics.map(({ code, message, span }) => issueOf(graph, agent, code, message, span?.pointer ?? null)), ...cellIssues]
    return { issues: found, status: found.length === 0 ? <Badge variant="outline">Checker: admitted</Badge> : <Badge variant="destructive">Checker: {found.length} {found.length === 1 ? "finding" : "findings"}</Badge> }
  }, [checker.isError, checker.error, checker.data, bindings, laws, cellIssues])

  const save = async (graph: g.Graph) => {
    const agent = agentOf(graph)
    if (!agent) return { tone: "refusal" as const, text: "The agent node is missing." }
    // A new agent or a copy has no version yet: installing it must not claim one.
    const result = await store.install(id, agent, stored && stored.version > 0 ? stored.version : undefined)
    if (result.installed) { setCellIssues([]); onSaved(); return { tone: "info" as const, text: `Saved as version ${result.version}; the cell checked and installed it (receipt ${result.receipt.receiptHash.slice(0, 12)}…).` } }
    if ("conflict" in result) return { tone: "refusal" as const, text: "Someone saved a newer version first. Reopen the agent to see it; your draft is kept." }
    setCellIssues(result.findings.map(({ code, message, pointer, law }) => issueOf(graph, agent, code, law ? `${message} (law ${law})` : message, pointer)))
    return { tone: "refusal" as const, text: `The cell refused to install it: ${result.findings.map(({ law, code }) => law ?? code).join(", ")}.` }
  }

  const props: CanvasFrameProps = {
    draftKey: `agent:${id}:${stored?.version ?? "new"}`, label: `Agent: ${id}`, initial, kinds: agentKinds, nodeTypes: NODE_TYPES, issues, saveBlocked: SAVE_BLOCKED, save,
    empty: "The agent node is missing; undo to bring it back.", fixed: (node) => node.id === AGENT_NODE,
    toNode: (node, found, selected): AgentPartNode => ({
      id: node.id, type: "part", position: node.position, selected,
      data: {
        kind: node.kind, issues: found,
        title: node.kind === "agent" ? node.config.title || node.config.name || "Untitled agent" : node.kind === "tool" ? bindings.find(({ id: binding }) => binding === node.config.binding)?.label ?? node.config.binding ?? "" : node.config.law ?? "",
        detail: node.kind === "agent" ? `${node.config.model} · ${node.config.approve === "never" ? "acts alone" : node.config.approve === "laws" ? "asks when a law applies" : "asks before each tool call"}` : node.kind === "tool" ? (() => { const binding = bindings.find(({ id: one }) => one === node.config.binding); return binding ? `${binding.app}/${binding.tool} · runs as ${binding.runRole}` : "" })() : node.config.decides === "yes" ? "A person decides" : "",
      },
    }),
    toEdge: (edge, found, selected): Edge => ({ id: edge.id, source: edge.source, target: edge.target, sourceHandle: edge.sourcePort, targetHandle: edge.targetPort, selected, markerEnd: { type: MarkerType.ArrowClosed }, style: found.length > 0 ? { stroke: "var(--destructive)", strokeWidth: 2 } : selected ? { stroke: "var(--primary)", strokeWidth: 2 } : undefined }),
    newEdge: ({ source, target, sourceHandle, targetHandle }: Connection, graph) => ({ id: g.freshName(graph.edges.map(({ id: edge }) => edge), `${source}->${target}`), source, target, sourcePort: sourceHandle ?? "out", targetPort: targetHandle ?? "tools", config: {} }),
    palette: ({ graph, edit, select }) => {
      const add = (kind: "tool" | "law") => {
        const value = kind === "tool" ? bindings.find(({ id: binding }) => !graph.nodes.some(({ config }) => config.binding === binding))?.id ?? "" : laws.find((law) => !graph.nodes.some(({ config }) => config.law === law)) ?? ""
        const node = g.freshId(graph, kind === "tool" ? toolNodeId(value || "new") : lawNodeId(value || "new"))
        const placed = g.addNode(graph, { id: node, kind, position: { x: kind === "tool" ? 0 : 720, y: graph.nodes.filter((one) => one.kind === kind).length * 90 }, config: kind === "tool" ? { binding: value } : { law: value, decides: "" } })
        edit(g.connect(placed, agentKinds, { id: `${node}->agent`, source: node, sourcePort: "out", target: AGENT_NODE, targetPort: kind === "tool" ? "tools" : "laws", config: {} }))
        select({ nodes: [node], edges: [] })
      }
      return <><Button size="sm" variant="outline" onClick={() => add("tool")}><Plus />Tool</Button><Button size="sm" variant="outline" onClick={() => add("law")}><Plus />Law</Button></>
    },
    inspector: (editing, found) => <Inspector editing={editing} issues={found} bindings={bindings} laws={laws} />,
  }
  return (
    <div className="space-y-2">
      <CanvasFrame {...props} />
      <div className="flex justify-end"><Button size="sm" disabled title={AGENT_RUN_BLOCKED}>Run the agent</Button></div>
    </div>
  )
}

const agentId = (name: string) => `studio.agent.${name.trim().toLowerCase().replace(/[^a-z0-9]+/g, "_").replace(/^_+|_+$/g, "") || "untitled"}`

/** The org's agents: open one, start one, duplicate or retire it. */
export function AgentsView({ store = kindStore<AgentDefinition>("agent") }: { readonly store?: KindStore<AgentDefinition> }) {
  const client = useQueryClient()
  const list = useQuery({ queryKey: ["definitions", "agent"], queryFn: () => store.list(), retry: false })
  const bindings = useQuery({ queryKey: ["connectors", "bindings", "agent"], queryFn: bindingsQuery, retry: false })
  const laws = useQuery({ queryKey: ["definitions"], queryFn: recordedTransport.definitions, select: (reads) => reads.map(({ document }) => document).filter(({ kind }) => kind === "law").map(({ payload }) => (payload as { id: string }).id) })
  const [open, setOpen] = useState<{ readonly id: string; readonly copyOf?: string } | null>(null)
  const stored = useQuery({ queryKey: ["definitions", "agent", open?.copyOf ?? open?.id], // A query may not resolve to undefined: a definition the cell does not have yet is null (a new agent).
    queryFn: async () => (await store.get(open?.copyOf ?? open?.id ?? "")) ?? null, enabled: open !== null, retry: false })
  const [newName, setNewName] = useState("")
  const [notice, setNotice] = useState<string | null>(null)
  const refresh = () => void client.invalidateQueries({ queryKey: ["definitions", "agent"] })
  const retire = async (id: string, version: number) => {
    const result = await store.retire(id, version).catch((error: unknown) => ({ failed: error instanceof Error ? error.message : String(error) }))
    setNotice("failed" in result ? result.failed : result.installed ? `Retired ${id}.` : "conflict" in result ? "Someone changed it first; reopen it." : `The cell refused: ${result.findings.map(({ law, code }) => law ?? code).join(", ")}.`)
    if (open?.id === id) setOpen(null)
    refresh()
  }
  const payload = stored.data && open?.copyOf ? { version: 0, payload: { ...stored.data.payload, name: open.id.replace(/^studio\.agent\./, ""), title: `${stored.data.payload.title} (copy)` } } : stored.data ?? null

  return (
    <div className="space-y-3">
      <Panel icon={Bot} label="Agents" action={list.data ? `${list.data.items.length}` : undefined}>
        <Well className="space-y-2 p-3 text-sm">
          {list.isPending && <p role="status" className="text-muted-foreground">Reading agents…</p>}
          {list.isError && <p role="alert" className="text-destructive">{list.error.message}</p>}
          {list.data?.items.length === 0 && <EmptyState icon={Bot}>No agents yet. Name one below to start.</EmptyState>}
          {list.data?.items.map((item) => (
            <div key={item.id} className="flex flex-wrap items-center gap-2 rounded-md border bg-background p-2">
              <button type="button" className="font-medium hover:underline" onClick={() => setOpen({ id: item.id })}>{item.title || words(item.id.replace(/^studio\.agent\./, ""))}</button>
              <span className="font-mono text-xs text-muted-foreground">{item.id} · v{item.version}</span>
              <span className="ms-auto flex gap-1">
                <Button size="sm" variant="ghost" aria-label={`Duplicate ${item.id}`} onClick={() => setOpen({ id: `${item.id}_copy`, copyOf: item.id })}><Copy /></Button>
                <Button size="sm" variant="ghost" aria-label={`Retire ${item.id}`} onClick={() => void retire(item.id, item.version)}><Trash2 /></Button>
              </span>
            </div>
          ))}
          <form className="flex gap-2" onSubmit={(event) => { event.preventDefault(); if (newName.trim()) { setOpen({ id: agentId(newName) }); setNewName("") } }}>
            <Input aria-label="New agent name" placeholder="New agent name" value={newName} onChange={(event) => setNewName(event.target.value)} />
            <Button size="sm" type="submit" disabled={!newName.trim()}><Plus />New agent</Button>
          </form>
          {notice && <p role="status" className="text-meta text-muted-foreground">{notice}</p>}
          {bindings.isError && <p role="alert" className="text-meta text-destructive">Tool bindings could not be read ({bindings.error.message}); tools show by id only.</p>}
        </Well>
      </Panel>
      {open && (stored.isPending ? <p role="status" className="text-muted-foreground">Opening {open.id}…</p>
        : stored.isError ? <p role="alert" className="text-destructive">{stored.error.message}</p>
          : <AgentEditor key={`${open.id}:${payload?.version ?? "new"}`} id={open.id} stored={payload} store={store} onSaved={() => { setOpen({ id: open.id }); refresh(); void client.invalidateQueries({ queryKey: ["definitions", "agent", open.id] }) }} bindings={bindings.data ?? []} laws={laws.data ?? []} />)}
    </div>
  )
}
