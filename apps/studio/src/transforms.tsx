import { Background, Handle, MarkerType, Position, ReactFlow, type Edge, type Node, type NodeProps } from "@xyflow/react"
import "@xyflow/react/dist/style.css"
import { Link, useParams } from "@tanstack/react-router"
import { useMutation } from "@tanstack/react-query"
import { Blend, FileCheck2, GitBranch, ListTree, Rows3 } from "lucide-react"
import { useEffect, useMemo, useRef, useState } from "react"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { Textarea } from "@/components/ui/textarea"
import { EmptyState, Panel, Well } from "@/components/panel"
import { TableCard } from "@/components/table-card"
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table"
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs"
import { member, type Read } from "@/lib/exact"
import { NotServed } from "@/lib/live-transport"
import { groupDigits } from "@/lib/format"
import { checkQuery, columnTitle, describeStep, exactValue, grainsOf, nodeDeclaration, nodeDefinition, periodOf, scaleOf, transformsOf, useTransformLineage, useTransformPlan, useTransformRows, type TransformColumn, type TransformNode, type TransformPage } from "@/lib/transforms"
import { cn } from "@/lib/utils"
import { useView, type View } from "@/lib/view"
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
      {read.data && <CheckQuery reads={transforms[0]?.inputs[0]} />}
    </div>
  )
}

/**
 * Check a query in the transform dialect (VIEW-SQL-1) against the installed definitions: the
 * cell's checker lowers and types it and says why it refuses one. Nothing runs, nothing installs.
 */
function CheckQuery({ reads }: { readonly reads: string | undefined }) {
  const { view } = useView()
  const [sql, setSql] = useState("")
  const check = useMutation({ mutationFn: checkQuery })
  const result = check.data?.document
  const plan = result?.plan ?? undefined
  const output = plan?.nodes.find((node) => node.id === plan.output)
  return (
    <Panel icon={FileCheck2} label="Check a query">
      <div className="space-y-3 px-2.5 pb-2.5">
        <p className="text-sm text-muted-foreground">Write a query in the transform dialect. The cell's checker types it against the installed definitions, as it would a transform, and says why it refuses one. Nothing runs and nothing is installed.</p>
        <Textarea aria-label="Query" rows={3} spellCheck={false} value={sql} placeholder={`SELECT t.id FROM "${reads ?? "namespace.Type"}" t`} className="font-mono text-xs" onChange={(event) => { setSql(event.target.value); check.reset() }} />
        <div className="flex gap-2">
          <Button size="sm" disabled={sql.trim() === "" || check.isPending} onClick={() => check.mutate(sql)}>{check.isPending ? "Checking…" : "Check"}</Button>
          <Button size="sm" variant="ghost" disabled={sql === ""} onClick={() => { setSql(""); check.reset() }}>Clear</Button>
        </div>
        {check.isError && <ReadError error={check.error} />}
        {result && !result.admitted && (
          <Well className="space-y-1.5 border-destructive/40 px-3 py-2 text-sm">
            <p role="status" className="font-medium text-destructive">Refused</p>
            <ul aria-label="Why the query is refused" className="space-y-1.5">
              {result.diagnostics.map((finding, at) => (
                <li key={`${finding.code}/${at}`}>
                  <p>{finding.message}</p>
                  {view === "technical" && <p className="font-mono text-meta text-muted-foreground">{finding.code}{finding.span === null ? "" : ` at ${finding.span.pointer}`}</p>}
                </li>
              ))}
            </ul>
          </Well>
        )}
        {result?.admitted && output && (
          <div className="space-y-2">
            <p role="status" className="text-sm"><span className="font-medium">Admitted</span> <span className="text-muted-foreground">· one row per {output.grain.length === 0 ? "query" : output.grain.join(", ")}</span></p>
            <ColumnList label="Columns of the checked query" node={output} view={view} />
          </div>
        )}
      </div>
    </Panel>
  )
}

/** A column's contract in one line: its type, whether it may be null, and how it may be aggregated (`decimal<2> · additive`). */
const contract = (column: TransformColumn) =>
  `${column.type}${column.nullable ? "?" : ""}${column.additivity === "undeclared" ? "" : ` · ${column.additivity.replaceAll("_", " ")}`}`

/** A column header: its label and its contract in the tooltip (Business), or its name and contract on a second line (Technical). */
function ColumnHead({ column, view }: { readonly column: TransformColumn; readonly view: View }) {
  if (view === "business") return <span title={contract(column)}>{columnTitle(column, view)}</span>
  return (
    <div className="space-y-0.5">
      <div>{column.name}</div>
      <div className="font-mono text-[11px] font-normal text-muted-foreground">{contract(column)}</div>
    </div>
  )
}

/** The first column stays put while the others scroll sideways, as in the records grid. */
const STICKY = "sticky left-0 z-10 shadow-[1px_0_0_var(--node-border)]"

const numeric = (column: TransformColumn) => column.type.startsWith("decimal") || column.type === "integer" || column.type === "number"

/**
 * Whether content is hidden past the right edge of the scroller `selector` finds inside `frame`:
 * the edge then fades, as the records grid's does.
 */
function useRightEdge(selector: string, deps: readonly unknown[]) {
  const frame = useRef<HTMLDivElement>(null)
  const [right, setRight] = useState(false)
  useEffect(() => {
    const scroller = frame.current?.querySelector<HTMLElement>(selector)
    if (scroller === null || scroller === undefined) return
    const measure = () => setRight(scroller.scrollLeft + scroller.clientWidth < scroller.scrollWidth - 1)
    measure()
    scroller.addEventListener("scroll", measure, { passive: true })
    // The scroller keeps its box when its content widens (Technical's two-line headers): observe the content too.
    const observer = typeof ResizeObserver === "undefined" ? undefined : new ResizeObserver(measure)
    observer?.observe(scroller)
    for (const content of Array.from(scroller.children)) observer?.observe(content)
    return () => {
      scroller.removeEventListener("scroll", measure)
      observer?.disconnect()
    }
  }, [selector, ...deps])
  return { frame, right }
}

function EdgeFade() {
  return <div aria-hidden data-edge="right" className="pointer-events-none absolute inset-y-px right-px z-20 w-12 rounded-e-sm bg-gradient-to-l from-card to-transparent" />
}

/**
 * One value as the reader reads it. Technical: the exact text the cell sent. Business: numbers
 * grouped in thousands with the unit or currency their row names, and a bucketed date as its
 * period (`Week of 26 Oct 2026`).
 */
function displayed(read: Read<TransformPage>, at: number, column: TransformColumn, grains: ReadonlyMap<string, string>, view: View): string {
  const row = read.document.rows[at]
  const value = row?.values[column.name]
  const exact = exactValue(read, `/rows/${at}/values/${member(column.name)}`, value, scaleOf(column.type))
  if (view === "technical" || value === null || value === undefined) return exact
  const grain = grains.get(column.name)
  if (grain !== undefined && typeof value === "string") return periodOf(value, grain)
  if (!numeric(column)) return exact
  const sources = [...(column.unitFrom ?? []), ...(column.currencyFrom ? [column.currencyFrom] : [])]
  const unit = sources.map((source) => row?.values[source]).filter((part): part is string => typeof part === "string" && part !== "").join(" ")
  return unit === "" ? groupDigits(exact) : `${groupDigits(exact)} ${unit}`
}

/** One page of rows, with a sticky first column and a faded edge while columns are hidden past it. */
function RowsGrid({ read, grains, onTrace }: { readonly read: Read<TransformPage>; readonly grains: ReadonlyMap<string, string>; readonly onTrace: (rowId: string) => void }) {
  const page = read.document
  const { view } = useView()
  const { frame, right } = useRightEdge("[data-slot=table-container]", [page.rows.length, page.columns.length])
  return (
    <div ref={frame} className="relative">
      <TableCard>
        <Table>
          <TableHeader>
            <TableRow>
              {page.columns.map((column, at) => (
                <TableHead scope="col" key={column.name} className={cn(at === 0 && cn(STICKY, "bg-surface-subtle"), numeric(column) && "text-right")}><ColumnHead column={column} view={view} /></TableHead>
              ))}
              <TableHead scope="col">Lineage</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {page.rows.map((row, index) => (
              <TableRow key={row.rowId}>
                {page.columns.map((column, at) => (
                  <TableCell key={column.name} className={cn("whitespace-nowrap", at === 0 && cn(STICKY, "bg-card font-medium"), numeric(column) && "text-right font-mono tabular-nums")}>
                    {displayed(read, index, column, grains, view)}
                  </TableCell>
                ))}
                <TableCell><Button variant="ghost" size="sm" onClick={() => onTrace(row.rowId)}>{row.memberCount} {row.memberCount === 1 ? "record" : "records"}</Button></TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      </TableCard>
      {right && <EdgeFade />}
    </div>
  )
}

/** The rows tab: one page at a time, each row a link to its lineage. */
function RowsTab({ id, onTrace }: { readonly id: string; readonly onTrace: (rowId: string) => void }) {
  const [cursors, setCursors] = useState<readonly (string | undefined)[]>([undefined])
  const after = cursors.at(-1)
  const rows = useTransformRows(id, after)
  const { read: catalog } = useCatalog()
  const grains = useMemo(() => catalog.data ? grainsOf(catalog.data.document, id) : new Map<string, string>(), [catalog.data, id])
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
      <RowsGrid read={rows.data} grains={grains} onTrace={onTrace} />
      <div className="flex gap-2">
        <Button variant="outline" size="sm" disabled={cursors.length === 1} onClick={() => setCursors(cursors.slice(0, -1))}>Previous</Button>
        <Button variant="outline" size="sm" disabled={page.nextAfter === null} onClick={() => page.nextAfter !== null && setCursors([...cursors, page.nextAfter])}>Next</Button>
      </div>
    </div>
  )
}

/** The lineage tab: the input records one row derives from, each a link to its record page. */
function LineageTab({ id, rowId }: { readonly id: string; readonly rowId: string | undefined }) {
  const { view } = useView()
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
          <span key={name}><span className="text-muted-foreground">{columnTitle(output?.columns.find((column) => column.name === name) ?? { name }, view)}</span> <span className="font-mono">{exactValue(lineage.data, `/values/${member(name)}`, value, scale(name))}</span></span>
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

/** A node's columns: each one's title, contract and where it comes from. */
function ColumnList({ label, node, view }: { readonly label: string; readonly node: TransformNode; readonly view: View }) {
  return (
    <ul aria-label={label} className="divide-y rounded-md border text-sm">
      {node.columns.map((column) => (
        <li key={column.name} className="space-y-0.5 px-3 py-2">
          <div className="flex flex-wrap items-center gap-2">
            <span className="font-medium">{columnTitle(column, view)}</span>
            {node.grain.includes(column.name) && <Badge variant="outline">grain</Badge>}
            <span className="ms-auto font-mono text-meta text-muted-foreground">{contract(column)}</span>
          </div>
          <div className="break-words font-mono text-meta text-muted-foreground">
            {column.origins.length === 0 ? "computed here" : `from ${column.origins.map(({ input, property }) => `${input}.${property}`).join(", ")}`}
            {column.currencyFrom ? ` · per ${column.currencyFrom}` : ""}
          </div>
        </li>
      ))}
    </ul>
  )
}

/** How the graph is shaded: by operator only, or by the rows each node produced. */
type Lens = "operator" | "volume"

type PlanNode = Node<{ node: TransformNode; output: boolean; selected: boolean; rows: number | undefined; share: number | undefined }, "step">

const WIDTH = 220
const GAP_X = 64
const GAP_Y = 28
const HEIGHT = 76

const count = new Intl.NumberFormat("en")

/** One operator: its kind and id, its shape, and (under the volume lens) its rows, shaded by share of the largest node. */
function StepCard({ data: { node, output, selected, rows, share } }: NodeProps<PlanNode>) {
  const shade = share === undefined ? undefined : `color-mix(in oklab, var(--chart-1) ${Math.round(4 + share * 18)}%, var(--card))`
  return (
    <div data-node={node.id} data-selected={selected || undefined} style={shade === undefined ? undefined : { background: shade }}
      className={cn("w-[220px] rounded-lg border bg-card px-3 py-2 shadow-sm transition-shadow", output && "border-foreground/40", selected && "ring-2 ring-ring")}>
      <Handle type="target" position={Position.Left} className="!opacity-0" />
      <div className="flex items-center gap-2 text-sm font-medium"><Badge variant="secondary" className="font-mono text-[11px]">{node.op}</Badge><span className="truncate">{node.id}</span></div>
      <div className="mt-1 flex items-center justify-between gap-2 text-xs text-muted-foreground">
        <span className="truncate">{node.columns.length} columns · {node.grain.length === 0 ? "one row" : node.grain.join(", ")}</span>
        {rows !== undefined && <span className="shrink-0 font-mono tabular-nums text-foreground">{count.format(rows)}</span>}
      </div>
      {share !== undefined && <div aria-hidden className="mt-1.5 h-1 rounded-full bg-muted"><div className="h-1 rounded-full bg-chart-1/70" style={{ width: `${Math.max(2, share * 100)}%` }} /></div>}
      <Handle type="source" position={Position.Right} className="!opacity-0" />
    </div>
  )
}

const NODE_TYPES = { step: StepCard }

/** Lay the plan out left to right: each node one column right of its deepest input, each column centred. */
function planLayout(nodes: readonly TransformNode[]): ReadonlyMap<string, { readonly x: number; readonly y: number }> {
  const ids = new Set(nodes.map(({ id }) => id))
  const depth = new Map<string, number>()
  for (const node of nodes) depth.set(node.id, Math.max(0, ...node.inputs.filter((input) => ids.has(input) && input !== node.id).map((input) => (depth.get(input) ?? 0) + 1)))
  const columns = new Map<number, string[]>()
  for (const node of nodes) {
    const at = depth.get(node.id) ?? 0
    columns.set(at, [...(columns.get(at) ?? []), node.id])
  }
  const tallest = Math.max(1, ...[...columns.values()].map((column) => column.length))
  const positions = new Map<string, { x: number; y: number }>()
  for (const [at, column] of columns) {
    const offset = ((tallest - column.length) * (HEIGHT + GAP_Y)) / 2
    column.forEach((id, row) => positions.set(id, { x: at * (WIDTH + GAP_X), y: offset + row * (HEIGHT + GAP_Y) }))
  }
  return positions
}

/**
 * The plan tab: the admitted operator graph left to right, a list of the same nodes, and the selected
 * node's definition and typed columns. Selecting in either view selects in both. The volume lens shades
 * each node by the rows it produced for this reader, as the cell reports them.
 */
function PlanTab({ id }: { readonly id: string }) {
  const plan = useTransformPlan(id)
  const rows = useTransformRows(id, undefined)
  const { read } = useCatalog()
  const { view } = useView()
  const [lens, setLens] = useState<Lens>("operator")
  const [picked, setPicked] = useState<string | undefined>(undefined)
  const volumes = rows.data?.document.nodeRows
  const document = plan.data?.document
  const selected = picked ?? document?.output
  const graph = useMemo(() => {
    if (!document) return { nodes: [], edges: [], height: 1, width: 1 }
    const positions = planLayout(document.nodes)
    const ids = new Set(document.nodes.map(({ id: node }) => node))
    const largest = Math.max(1, ...Object.values(volumes ?? {}))
    const nodes: PlanNode[] = document.nodes.map((node) => {
      const counted = volumes?.[node.id]
      const shown = lens === "volume" ? counted : undefined
      return {
        id: node.id, type: "step", position: positions.get(node.id) ?? { x: 0, y: 0 },
        data: { node, output: node.id === document.output, selected: node.id === selected, rows: shown, share: shown === undefined ? undefined : Math.log1p(shown) / Math.log1p(largest) },
      }
    })
    const edges: Edge[] = document.nodes.flatMap((node) => node.inputs.filter((input) => ids.has(input) && input !== node.id).map((input) => ({
      id: `${input}->${node.id}`, source: input, target: node.id, markerEnd: { type: MarkerType.ArrowClosed },
      ...(input === selected || node.id === selected ? { style: { strokeWidth: 2 } } : {}),
    })))
    const height = Math.max(1, ...nodes.map(({ position }) => position.y + HEIGHT))
    const width = Math.max(1, ...nodes.map(({ position }) => position.x + WIDTH))
    return { nodes, edges, height, width }
  }, [document, volumes, lens, selected])
  const edge = useRightEdge("[data-slot=plan-scroller]", [graph.width])
  if (plan.isPending) return <p className="text-muted-foreground">Reading the plan…</p>
  if (plan.isError) return <ReadError error={plan.error} />
  const shown = plan.data.document
  const focus = shown.nodes.find((node) => node.id === selected)
  const definition = read.data ? nodeDefinition(read.data.document, id, selected) : undefined
  const declared = read.data ? nodeDeclaration(read.data.document, id, selected) : undefined
  const inputs = new Map(shown.inputs.map(({ alias, objectType }) => [alias, objectType]))
  return (
    <div className="space-y-4">
      <Panel icon={GitBranch} label="Definition graph" action={
        <div role="radiogroup" aria-label="Shade nodes by" className="inline-flex rounded-md border p-0.5 text-meta">
          {(["operator", "volume"] as const).map((option) => (
            <button key={option} type="button" role="radio" aria-checked={lens === option} disabled={option === "volume" && volumes === undefined}
              title={option === "volume" && volumes === undefined ? "This cell does not report node volumes" : undefined}
              onClick={() => setLens(option)}
              className={cn("rounded px-2 py-0.5 capitalize disabled:opacity-50", lens === option ? "bg-muted font-medium text-foreground" : "text-muted-foreground")}>{option}</button>
          ))}
        </div>
      }>
        {/* Read at full size, never shrunk to fit: a wide plan scrolls sideways, and the panel is as tall as the plan. */}
        <div ref={edge.frame} className="relative">
          <div data-slot="plan-scroller" className="overflow-x-auto rounded-md border bg-surface-subtle">
            <div style={{ width: graph.width + 48, height: graph.height + 48 }}>
              <ReactFlow nodes={graph.nodes} edges={graph.edges} nodeTypes={NODE_TYPES} colorMode="system" defaultViewport={{ x: 24, y: 24, zoom: 1 }}
                onNodeClick={(_, node) => setPicked(node.id)} zoomOnScroll={false} zoomOnPinch={false} zoomOnDoubleClick={false} panOnDrag={false} preventScrolling={false}
                nodesDraggable={false} nodesConnectable={false} edgesFocusable={false} proOptions={{ hideAttribution: true }}>
                <Background />
              </ReactFlow>
            </div>
          </div>
          {edge.right && <EdgeFade />}
        </div>
        <p className="mt-2 px-2.5 pb-1 text-meta text-muted-foreground">{shown.inputs.map(({ alias, objectType }) => `${alias}: ${objectType}`).join(" · ")}</p>
      </Panel>
      <div className="grid gap-4 lg:grid-cols-[minmax(0,1fr)_minmax(0,24rem)]">
        <Panel icon={ListTree} label="Nodes">
          <TableCard>
            <Table aria-label="Plan nodes">
              <TableHeader><TableRow><TableHead scope="col">Node</TableHead><TableHead scope="col">Operator</TableHead><TableHead scope="col">Reads</TableHead><TableHead scope="col" className="text-right">Rows</TableHead></TableRow></TableHeader>
              <TableBody>
                {shown.nodes.map((node) => (
                  <TableRow key={node.id} aria-selected={node.id === selected} data-state={node.id === selected ? "selected" : undefined}
                    className="cursor-pointer" onClick={() => setPicked(node.id)}>
                    <TableCell className="font-medium"><button type="button" className="text-start underline-offset-4 hover:underline" onClick={() => setPicked(node.id)}>{node.id}</button>{node.id === shown.output && <Badge variant="outline" className="ms-2">output</Badge>}</TableCell>
                    <TableCell className="font-mono text-meta">{node.op}</TableCell>
                    <TableCell className="font-mono text-meta">{node.inputs.join(", ")}</TableCell>
                    <TableCell className="text-right font-mono tabular-nums">{volumes?.[node.id] === undefined ? "—" : count.format(volumes[node.id] ?? 0)}</TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          </TableCard>
        </Panel>
        {focus && (
          <Panel icon={Rows3} label={`Node ${focus.id}`}>
            <div className="space-y-3">
              <div className="flex flex-wrap items-center gap-2 text-meta text-muted-foreground">
                <Badge variant="secondary" className="font-mono text-[11px]">{focus.op}</Badge>
                <span>grain {focus.grain.length === 0 ? "one row" : focus.grain.join(", ")}</span>
              </div>
              {view === "business" && declared !== undefined && (
                <p aria-label={`What ${focus.id} does`} className="text-sm">{describeStep(declared, inputs)}</p>
              )}
              {view === "technical" && definition !== undefined && (
                <Well className="max-h-64 overflow-auto px-3 py-2">
                  <pre aria-label={`Definition of ${focus.id}`} className="font-mono text-[12px] leading-relaxed whitespace-pre-wrap">{definition}</pre>
                </Well>
              )}
              <ColumnList label={`Columns of ${focus.id}`} node={focus} view={view} />
            </div>
          </Panel>
        )}
      </div>
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
