import { Background, Controls, Handle, MarkerType, Position, ReactFlow, type Edge, type Node, type NodeProps } from "@xyflow/react"
import "@xyflow/react/dist/style.css"
import { Link, useParams } from "@tanstack/react-router"
import { Blend, GitBranch, ListTree, Rows3 } from "lucide-react"
import { useEffect, useMemo, useRef, useState, type CSSProperties } from "react"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { EmptyState, Panel, Well } from "@/components/panel"
import { TableCard } from "@/components/table-card"
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table"
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs"
import { member, type Read } from "@/lib/exact"
import { NotServed } from "@/lib/live-transport"
import { exactValue, scaleOf, transformsOf, useTransformLineage, useTransformPlan, useTransformRows, type TransformColumn, type TransformNode, type TransformPage } from "@/lib/transforms"
import { cn } from "@/lib/utils"
import { useCatalog } from "./sources.tsx"

/** A refusal or an unserved read, said plainly. */
function ReadError({ error }: { readonly error: Error }) {
  return error instanceof NotServed
    ? <EmptyState icon={Blend} title="Not served">{error.message}</EmptyState>
    : <p role="alert" className="text-destructive">{error.message}</p>
}

/** `/transforms`: every installed transform, with the types it reads. */
export function TransformsPage() {
  const { read } = useCatalog()
  const transforms = useMemo(() => read.data ? transformsOf(read.data.document) : [], [read.data])
  return (
    <div className="space-y-4">
      <header className="space-y-1">
        <h1 className="text-xl font-medium">Transforms</h1>
        <p className="text-muted-foreground">Derived datasets the cell's checker admits: every column typed, every join declared, every row traced to the records it comes from.</p>
      </header>
      {read.isPending && <p className="text-muted-foreground">Reading definitions…</p>}
      {read.isError && <ReadError error={read.error} />}
      {read.data && transforms.length === 0 && <EmptyState icon={Blend}>No transform is installed in this cell.</EmptyState>}
      {transforms.length > 0 && (
        <TableCard>
          <Table>
            <TableHeader><TableRow><TableHead scope="col">Transform</TableHead><TableHead scope="col">Reads</TableHead><TableHead scope="col">Version</TableHead></TableRow></TableHeader>
            <TableBody>
              {transforms.map((transform) => (
                <TableRow key={transform.id}>
                  <TableCell><Link to="/transforms/$id" params={{ id: transform.id }} className="font-medium underline-offset-4 hover:underline">{transform.title}</Link><div className="font-mono text-meta text-muted-foreground">{transform.id}</div></TableCell>
                  <TableCell className="space-x-1">{transform.inputs.map((input) => <Badge key={input} variant="secondary">{input}</Badge>)}</TableCell>
                  <TableCell className="font-mono text-meta">{transform.version}</TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </TableCard>
      )}
    </div>
  )
}

/** A column's contract in one line: its type, whether it may be null, and how it may be aggregated. */
function ColumnHead({ column }: { readonly column: TransformColumn }) {
  return (
    <div className="space-y-0.5">
      <div>{column.name}</div>
      <div className="font-mono text-[11px] font-normal text-muted-foreground">{column.type}{column.nullable ? "?" : ""}{column.additivity === "undeclared" ? "" : ` · ${column.additivity.replaceAll("_", " ")}`}</div>
    </div>
  )
}

/** The first column stays put while the others scroll sideways, as in the records grid. */
const STICKY = "sticky left-0 z-10 shadow-[1px_0_0_var(--node-border)]"

const numeric = (column: TransformColumn) => column.type.startsWith("decimal") || column.type === "integer" || column.type === "number"

/** One page of rows. While columns are hidden past the right edge, the edge fades, as the records grid's does. */
function RowsGrid({ read, page, onTrace }: { readonly read: Read<TransformPage>; readonly page: TransformPage; readonly onTrace: (rowId: string) => void }) {
  const frame = useRef<HTMLDivElement>(null)
  const [right, setRight] = useState(false)
  useEffect(() => {
    const scroller = frame.current?.querySelector<HTMLElement>("[data-slot=table-container]")
    if (scroller === null || scroller === undefined) return
    const measure = () => setRight(scroller.scrollLeft + scroller.clientWidth < scroller.scrollWidth - 1)
    measure()
    scroller.addEventListener("scroll", measure, { passive: true })
    const observer = typeof ResizeObserver === "undefined" ? undefined : new ResizeObserver(measure)
    observer?.observe(scroller)
    return () => {
      scroller.removeEventListener("scroll", measure)
      observer?.disconnect()
    }
  }, [page.rows.length, page.columns.length])
  return (
    <div ref={frame} className="relative">
      <TableCard>
        <Table>
          <TableHeader>
            <TableRow>
              {page.columns.map((column, at) => (
                <TableHead scope="col" key={column.name} className={cn(at === 0 && cn(STICKY, "bg-surface-subtle"), numeric(column) && "text-right")}><ColumnHead column={column} /></TableHead>
              ))}
              <TableHead scope="col">Lineage</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {page.rows.map((row, index) => (
              <TableRow key={row.rowId}>
                {page.columns.map((column, at) => (
                  <TableCell key={column.name} className={cn(at === 0 && cn(STICKY, "bg-card font-medium"), numeric(column) && "text-right font-mono tabular-nums")}>
                    {exactValue(read, `/rows/${index}/values/${member(column.name)}`, row.values[column.name], scaleOf(column.type))}
                  </TableCell>
                ))}
                <TableCell><Button variant="ghost" size="sm" onClick={() => onTrace(row.rowId)}>{row.memberCount} records</Button></TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      </TableCard>
      {right && <div aria-hidden data-edge="right" className="pointer-events-none absolute inset-y-px right-px z-20 w-12 rounded-e-sm bg-gradient-to-l from-card to-transparent" />}
    </div>
  )
}

/** The rows tab: one page at a time, each row a link to its lineage. */
function RowsTab({ id, onTrace }: { readonly id: string; readonly onTrace: (rowId: string) => void }) {
  const [cursors, setCursors] = useState<readonly (string | undefined)[]>([undefined])
  const after = cursors.at(-1)
  const rows = useTransformRows(id, after)
  if (rows.isPending) return <p className="text-muted-foreground">Reading rows…</p>
  if (rows.isError) return <ReadError error={rows.error} />
  const page = rows.data.document
  const withheld = Object.entries(page.withheld)
  return (
    <div className="space-y-3">
      <div className="flex flex-wrap items-center gap-2 text-meta text-muted-foreground">
        <span>{page.totalRows} rows</span><span aria-hidden>·</span><span>engine {page.engine}</span><span aria-hidden>·</span><span>at commit {page.commits}</span>
        {page.complete ? <Badge variant="secondary">complete</Badge> : <Badge variant="outline">incomplete for you</Badge>}
        {withheld.map(([input, count]) => <Badge key={input} variant="outline">{count} {input} withheld</Badge>)}
        {page.excluded > 0 && <Badge variant="outline">{page.excluded} quarantined</Badge>}
      </div>
      <RowsGrid read={rows.data} page={page} onTrace={onTrace} />
      <div className="flex gap-2">
        <Button variant="outline" size="sm" disabled={cursors.length === 1} onClick={() => setCursors(cursors.slice(0, -1))}>Previous</Button>
        <Button variant="outline" size="sm" disabled={page.nextAfter === null} onClick={() => page.nextAfter !== null && setCursors([...cursors, page.nextAfter])}>Next</Button>
      </div>
    </div>
  )
}

/** The lineage tab: the input records one row derives from, each a link to its record page. */
function LineageTab({ id, rowId }: { readonly id: string; readonly rowId: string | undefined }) {
  const lineage = useTransformLineage(id, rowId)
  const plan = useTransformPlan(id)
  const planDocument = plan.data?.document
  const output = planDocument === undefined ? undefined : planDocument.nodes.find((node) => node.id === planDocument.output)
  const scale = (name: string) => {
    const column = output?.columns.find((candidate) => candidate.name === name)
    return column === undefined ? undefined : scaleOf(column.type)
  }
  if (rowId === undefined) return <EmptyState icon={ListTree} title="Pick a row">Choose a row's records on the Rows tab to see where it comes from.</EmptyState>
  if (lineage.isPending) return <p className="text-muted-foreground">Reading lineage…</p>
  if (lineage.isError) return <ReadError error={lineage.error} />
  const document = lineage.data.document
  return (
    <div className="space-y-3">
      <Well className="flex flex-wrap gap-x-4 gap-y-1 px-3 py-2 text-sm">
        {Object.entries(document.values).map(([name, value]) => (
          <span key={name}><span className="text-muted-foreground">{name}</span> <span className="font-mono">{exactValue(lineage.data, `/values/${member(name)}`, value, scale(name))}</span></span>
        ))}
      </Well>
      <TableCard>
        <Table>
          <TableHeader><TableRow><TableHead scope="col">Input</TableHead><TableHead scope="col">Type</TableHead><TableHead scope="col">Record</TableHead><TableHead scope="col" className="text-right">Version</TableHead></TableRow></TableHeader>
          <TableBody>
            {document.members.map((source) => (
              <TableRow key={`${source.input}/${source.id}`}>
                <TableCell>{source.input}</TableCell>
                <TableCell className="font-mono text-meta">{source.objectType}</TableCell>
                <TableCell><Link to="/sources/$concept/$" params={{ concept: source.objectType, _splat: source.id }} className="font-mono underline-offset-4 hover:underline">{source.id}</Link></TableCell>
                <TableCell className="text-right font-mono tabular-nums">{source.version}</TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      </TableCard>
    </div>
  )
}

type PlanNode = Node<{ node: TransformNode; output: boolean }, "step">

const WIDTH = 240
const GAP_X = 36
const GAP_Y = 80
const HEIGHT = 84

function StepCard({ data: { node, output } }: NodeProps<PlanNode>) {
  return (
    <div className={`w-[240px] rounded-xl border bg-card px-3 py-2 shadow-sm ${output ? "ring-1 ring-foreground/30" : ""}`}>
      <Handle type="target" position={Position.Top} className="!opacity-0" />
      <div className="flex items-center gap-2 text-sm font-medium"><Badge variant="secondary" className="font-mono text-[11px]">{node.op}</Badge><span className="truncate">{node.id}</span></div>
      <div className="mt-1 truncate text-xs text-muted-foreground">{node.columns.length} columns · grain {node.grain.length === 0 ? "one row" : node.grain.join(", ")}</div>
      <Handle type="source" position={Position.Bottom} className="!opacity-0" />
    </div>
  )
}

const NODE_TYPES = { step: StepCard }

/** Lay the plan out top to bottom: each node one row below its deepest input. */
function planLayout(nodes: readonly TransformNode[]): ReadonlyMap<string, { readonly x: number; readonly y: number }> {
  const ids = new Set(nodes.map(({ id }) => id))
  const depth = new Map<string, number>()
  for (const node of nodes) depth.set(node.id, Math.max(0, ...node.inputs.filter((input) => ids.has(input) && input !== node.id).map((input) => (depth.get(input) ?? 0) + 1)))
  const rows = new Map<number, number>()
  const positions = new Map<string, { x: number; y: number }>()
  for (const node of nodes) {
    const at = depth.get(node.id) ?? 0
    const row = rows.get(at) ?? 0
    rows.set(at, row + 1)
    positions.set(node.id, { x: row * (WIDTH + GAP_X), y: at * (HEIGHT + GAP_Y) })
  }
  return positions
}

/** The plan tab: the admitted operator graph and the output's typed columns with their lineage. */
function PlanTab({ id }: { readonly id: string }) {
  const plan = useTransformPlan(id)
  const graph = useMemo(() => {
    if (!plan.data) return { nodes: [], edges: [], rows: 1 }
    const document = plan.data.document
    const positions = planLayout(document.nodes)
    const ids = new Set(document.nodes.map(({ id: node }) => node))
    const nodes: PlanNode[] = document.nodes.map((node) => ({ id: node.id, type: "step", position: positions.get(node.id) ?? { x: 0, y: 0 }, data: { node, output: node.id === document.output } }))
    const edges: Edge[] = document.nodes.flatMap((node) => node.inputs.filter((input) => ids.has(input) && input !== node.id).map((input) => ({ id: `${input}->${node.id}`, source: input, target: node.id, markerEnd: { type: MarkerType.ArrowClosed } })))
    const rows = Math.max(1, ...nodes.map(({ position }) => position.y / (HEIGHT + GAP_Y) + 1))
    return { nodes, edges, rows }
  }, [plan.data])
  if (plan.isPending) return <p className="text-muted-foreground">Reading the plan…</p>
  if (plan.isError) return <ReadError error={plan.error} />
  const document = plan.data.document
  const output = document.nodes.find((node) => node.id === document.output)
  return (
    <div className="space-y-4">
      <Panel icon={GitBranch} label="Definition graph" action={`${document.inputs.map(({ alias, objectType }) => `${alias}: ${objectType}`).join(" · ")}`}>
        <div className="h-[min(70vh,max(320px,calc(var(--rows)*164px+60px)))] overflow-hidden rounded-md border bg-surface-subtle" style={{ "--rows": graph.rows } as CSSProperties}>
          <ReactFlow nodes={graph.nodes} edges={graph.edges} nodeTypes={NODE_TYPES} fitView colorMode="system" minZoom={0.2}
            nodesDraggable={false} nodesConnectable={false} edgesFocusable={false} proOptions={{ hideAttribution: true }}>
            <Background />
            <Controls showInteractive={false} />
          </ReactFlow>
        </div>
      </Panel>
      {output && (
        <Panel icon={Rows3} label="Output columns">
          <TableCard>
            <Table>
              <TableHeader><TableRow><TableHead scope="col">Column</TableHead><TableHead scope="col">Type</TableHead><TableHead scope="col">Aggregation</TableHead><TableHead scope="col">Comes from</TableHead></TableRow></TableHeader>
              <TableBody>
                {output.columns.map((column) => (
                  <TableRow key={column.name}>
                    <TableCell className="font-medium">{column.name}{output.grain.includes(column.name) && <Badge variant="outline" className="ms-2">grain</Badge>}</TableCell>
                    <TableCell className="font-mono text-meta">{column.type}{column.nullable ? "?" : ""}</TableCell>
                    <TableCell>{column.additivity.replaceAll("_", " ")}</TableCell>
                    <TableCell className="font-mono text-meta">{column.origins.map(({ input, property }) => `${input}.${property}`).join(", ") || "—"}</TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          </TableCard>
        </Panel>
      )}
    </div>
  )
}

/** `/transforms/$id`: one transform's rows, a row's lineage and the admitted plan. */
export function TransformPage() {
  const { id = "" } = useParams({ strict: false })
  const { read } = useCatalog()
  const entry = useMemo(() => read.data ? transformsOf(read.data.document).find((transform) => transform.id === id) : undefined, [read.data, id])
  const [tab, setTab] = useState("rows")
  const [traced, setTraced] = useState<string | undefined>(undefined)
  return (
    <div className="space-y-4">
      {read.isPending && <p className="text-muted-foreground">Reading definitions…</p>}
      {read.isError && <ReadError error={read.error} />}
      {read.data && entry === undefined && <EmptyState icon={Blend}>No transform named {id} is installed.</EmptyState>}
      {entry && <>
        <header className="flex flex-wrap items-baseline gap-3">
          <h1 className="text-xl font-medium">{entry.title}</h1>
          <span className="font-mono text-meta text-muted-foreground">{entry.id}</span>
          <Link to="/transforms" className="text-meta underline-offset-4 hover:underline">All transforms</Link>
        </header>
        <Tabs value={tab} onValueChange={setTab}>
          <TabsList><TabsTrigger value="rows">Rows</TabsTrigger><TabsTrigger value="lineage">Lineage</TabsTrigger><TabsTrigger value="plan">Plan</TabsTrigger></TabsList>
          <TabsContent value="rows" className="pt-3"><RowsTab id={entry.id} onTrace={(rowId) => { setTraced(rowId); setTab("lineage") }} /></TabsContent>
          <TabsContent value="lineage" className="pt-3"><LineageTab id={entry.id} rowId={traced} /></TabsContent>
          <TabsContent value="plan" className="pt-3"><PlanTab id={entry.id} /></TabsContent>
        </Tabs>
      </>}
    </div>
  )
}
