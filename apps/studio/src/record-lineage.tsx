import { useQueries } from "@tanstack/react-query"
import { Link } from "@tanstack/react-router"
import { Background, Controls, Handle, MarkerType, Position, ReactFlow, type Edge, type Node, type NodeProps } from "@xyflow/react"
import "@xyflow/react/dist/style.css"
import { Box, Database, FileText, GitBranch, List, Rows3, type LucideIcon } from "lucide-react"
import { useEffect, useMemo, useState } from "react"
import { EmptyState, Panel, Well } from "@/components/panel"
import { TableCard } from "@/components/table-card"
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table"
import { ToggleGroup, ToggleGroupItem } from "@/components/ui/toggle-group"
import { cn } from "@/lib/utils"
import { CellRefusal, cellRead } from "./lib/live-transport.ts"
import { factKey, lineageGraph, type FactRef, type LineageGraph, type LineageNode } from "./lib/lineage-graph.ts"
import type { SourceLineage } from "./lib/transport.ts"

const HOPS = [[1, "1 hop"], [2, "2 hops"], [6, "3+ hops"]] as const
const WIDTH = 220
const GAP_X = 72
const ROW = 84
const HEIGHT = 56

const ICON: Readonly<Record<LineageNode["kind"], LucideIcon>> = { record: Box, fact: Rows3, source: Database, document: FileText }

type Card = Node<{ readonly node: LineageNode }, "card">

function NodeCard({ data: { node } }: NodeProps<Card>) {
  const Icon = ICON[node.kind]
  const body = (
    <div className={cn("w-[220px] rounded-node border border-node-border bg-card px-3 py-2 shadow-[var(--node-highlight),var(--elevation-node)]", node.kind === "record" && "ring-2 ring-ring/30")}>
      <Handle type="target" position={Position.Left} className="!opacity-0" />
      <div className="flex min-w-0 items-center gap-2"><Icon aria-hidden className="size-4 shrink-0 text-muted-foreground" /><span className="truncate text-sm font-medium">{node.title}</span></div>
      <div className="truncate ps-6 font-mono text-[11px] text-muted-foreground" title={node.detail}>{node.detail}</div>
      <Handle type="source" position={Position.Right} className="!opacity-0" />
    </div>
  )
  return node.kind === "fact" && node.ref ? <Link to="/sources/$concept/$" params={{ concept: node.ref.objectType, _splat: node.ref.key }} hash="lineage">{body}</Link> : body
}

const NODE_TYPES = { card: NodeCard }

/** An edge's label: the one fact it feeds by name, or how many; the list view names them all. */
const facts = (names: readonly string[]) => names.length === 0 ? "" : names.length === 1 ? (names[0] ?? "").replaceAll("_", " ") : `${names.length} facts`

/** Sources on the left, the record on the right: each hop one column, its nodes in the order the lineage names them. */
function Graph({ graph }: { readonly graph: LineageGraph }) {
  const { nodes, edges } = useMemo(() => {
    const deepest = Math.max(0, ...graph.nodes.map(({ hop }) => hop))
    const rows = new Map<number, number>()
    const nodes: Card[] = graph.nodes.map((node) => {
      const row = rows.get(node.hop) ?? 0
      rows.set(node.hop, row + 1)
      // A known size lets the first fit include every card before the cards are measured.
      return { id: node.id, type: "card", position: { x: (deepest - node.hop) * (WIDTH + GAP_X), y: row * ROW }, width: WIDTH, height: HEIGHT, data: { node } }
    })
    const edges: Edge[] = graph.edges.map(({ from, to, facts: names }) => ({
      id: `${from}→${to}`, source: from, target: to, type: "smoothstep", label: facts(names), labelStyle: { fontSize: 11 }, markerEnd: { type: MarkerType.ArrowClosed },
    }))
    return { nodes, edges }
  }, [graph])
  return (
    <div className="h-80 overflow-hidden rounded-md border border-border/70 bg-surface-subtle">
      <ReactFlow nodes={nodes} edges={edges} nodeTypes={NODE_TYPES} colorMode="system" minZoom={0.2}
        // Fit once the cards are drawn and measured; a fit before that leaves the record off the edge.
        onInit={(flow) => { requestAnimationFrame(() => void flow.fitView({ maxZoom: 1, padding: 0.15 })) }}
        nodesDraggable={false} nodesConnectable={false} edgesFocusable={false} proOptions={{ hideAttribution: true }}>
        <Background />
        <Controls showInteractive={false} />
      </ReactFlow>
    </div>
  )
}

/** The same edges as a table: what each read feeds, by which rules, and which facts. */
function EdgeList({ graph }: { readonly graph: LineageGraph }) {
  const byId = new Map(graph.nodes.map((node) => [node.id, node]))
  const cell = (id: string) => {
    const node = byId.get(id)
    return node ? <><div className="font-medium">{node.title}</div><div className="truncate font-mono text-xs text-muted-foreground" title={node.detail}>{node.detail}</div></> : id
  }
  return (
    <TableCard><Table>
      <TableHeader><TableRow><TableHead>From</TableHead><TableHead>To</TableHead><TableHead className="max-sm:hidden">Rules</TableHead><TableHead>Facts</TableHead></TableRow></TableHeader>
      <TableBody>
        {graph.edges.map((edge) => (
          <TableRow key={`${edge.from}→${edge.to}`}>
            <TableCell className="max-w-56">{cell(edge.from)}</TableCell>
            <TableCell className="max-w-56">{cell(edge.to)}</TableCell>
            <TableCell className="font-mono text-xs max-sm:hidden">{edge.rules.join(", ") || "—"}</TableCell>
            <TableCell className="text-sm">{edge.facts.map((name) => name.replaceAll("_", " ")).join(", ") || "—"}</TableCell>
          </TableRow>
        ))}
      </TableBody>
    </Table></TableCard>
  )
}

/** One upstream fact's lineage at its version; `null` when the cell keeps none for it (or none this reader may see). */
async function upstreamLineage(ref: FactRef): Promise<SourceLineage | null> {
  try {
    const read = await cellRead<SourceLineage>(`/v1/objects/${encodeURIComponent(ref.objectType)}/${encodeURIComponent(ref.key)}/lineage?version=${ref.version}`, "Lineage")
    return read.document
  } catch (error) {
    if (error instanceof CellRefusal && error.status === 404) return null
    throw error
  }
}

/**
 * Where a record came from, one to three or more hops back: its source records, their documents and
 * any upstream facts a transform read, each upstream fact's own lineage read from the cell as the
 * hops grow. A graph and the same edges as a list; nothing is inferred beyond what the cell sent.
 */
export function RecordLineage({ root, lineage, live }: { readonly root: FactRef; readonly lineage: SourceLineage; readonly live: boolean }) {
  const [hops, setHops] = useState<number>(1)
  const [view, setView] = useState<"graph" | "list">("graph")
  // Upstream facts whose lineage has been asked for; the graph grows a hop as each read lands.
  const [requested, setRequested] = useState<readonly FactRef[]>([])
  const reads = useQueries({
    queries: requested.map((ref) => ({ queryKey: ["lineage-at", ref.objectType, ref.key, ref.version], queryFn: () => upstreamLineage(ref), retry: false, staleTime: Infinity })),
  })
  const lineages = new Map(requested.flatMap((ref, index) => {
    const data = reads[index]?.data
    return data === undefined ? [] : [[factKey(ref), data] as const]
  }))
  const graph = lineageGraph(root, lineage, lineages, hops)
  const missing = live ? graph.unread.filter((ref) => !requested.some((asked) => factKey(asked) === factKey(ref))) : []
  const missingKey = missing.map(factKey).join("|")
  useEffect(() => { if (missing.length > 0) setRequested((current) => [...current, ...missing]) }, [missingKey])
  const reachable = (depth: number) => depth === 1 || lineageGraph(root, lineage, lineages, depth - 1).further
  const failed = reads.find((read) => read.isError)
  return (
    <Panel icon={GitBranch} label="Lineage" action={
      <div className="flex flex-wrap items-center gap-2">
        <ToggleGroup type="single" variant="outline" size="sm" aria-label="How far back" value={String(hops)} onValueChange={(next) => { if (next) setHops(Number(next)) }}>
          {HOPS.map(([depth, label]) => <ToggleGroupItem key={depth} value={String(depth)} className="px-2.5" disabled={!reachable(depth)}>{label}</ToggleGroupItem>)}
        </ToggleGroup>
        <ToggleGroup type="single" variant="outline" size="sm" aria-label="Show as" value={view} onValueChange={(next) => { if (next === "graph" || next === "list") setView(next) }}>
          <ToggleGroupItem value="graph" className="px-2.5" aria-label="Graph"><GitBranch /></ToggleGroupItem>
          <ToggleGroupItem value="list" className="px-2.5" aria-label="List"><List /></ToggleGroupItem>
        </ToggleGroup>
      </div>
    }>
      <Well className="space-y-2 p-3">
        {graph.edges.length === 0 ? <EmptyState icon={GitBranch}>The cell names no source for this record's facts.</EmptyState>
          : view === "graph" ? <Graph graph={graph} /> : <EdgeList graph={graph} />}
        <p className="text-meta text-muted-foreground">
          {graph.nodes.length - 1} {graph.nodes.length === 2 ? "input" : "inputs"} within {hops === 6 ? "every hop the cell names" : `${hops} ${hops === 1 ? "hop" : "hops"}`}
          {graph.further ? "; the lineage goes further back." : "; nothing further back."}
          {reads.some((read) => read.isPending) ? " Reading further back…" : ""}
          {failed?.error instanceof Error ? ` An upstream read failed: ${failed.error.message}` : ""}
        </p>
      </Well>
    </Panel>
  )
}
