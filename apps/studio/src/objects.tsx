import { useInfiniteQuery, useQueries, useQuery } from "@tanstack/react-query"
import { Link, useParams } from "@tanstack/react-router"
import { LifecycleStepper } from "@/components/lifecycle-stepper"
import {
  type ColumnDef, type SortingState, flexRender, getCoreRowModel, getFilteredRowModel,
  getPaginationRowModel, getSortedRowModel, useReactTable,
} from "@tanstack/react-table"
import { ArrowDown, ArrowUp, ArrowUpDown, Braces, Database, GitBranch, History, KeyRound, Link2, Rows3, ShieldCheck, ShieldOff, type LucideIcon } from "lucide-react"
import { useEffect, useMemo, useState, type ReactNode } from "react"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { EmptyState, Panel, Well } from "@/components/panel"
import { Input } from "@/components/ui/input"
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs"
import { CopyId } from "@/components/copy-id"
import { ProvenanceChain, ProvenanceValue } from "@/components/provenance"
import { useInspector } from "@/lib/inspector"
import { KINDS, STATUS } from "@/lib/kinds"
import { ViewToggle, useCardView, useView } from "@/lib/view"
import { useViewer } from "@/lib/viewer"
import { Code } from "@/components/code"
import { cn } from "@/lib/utils"
import { TableCell, TableHead, TableRow } from "@/components/ui/table"
import { conceptTitle, systemName, type Concept } from "./lib/catalog.ts"
import { member } from "./lib/exact.ts"
import { collect, compareDecimals, exactPretty, factText, nextAfter, readProblem, recordName, shouldReadNext, type PageRow } from "./lib/objects.ts"
import { transport, type LinkPage, type ObjectReads, type SourceLineage } from "./lib/transport.ts"
import { DenseTable, ObjectTypePage, RECORDED_HINT, useCatalog } from "./sources.tsx"

/** Pages read per concept before the list is shown as incomplete. */
const MAX_PAGES = 50

interface Row extends PageRow {
  readonly from: string
}

function SortIcon({ sorted }: { readonly sorted: false | "asc" | "desc" }) {
  if (sorted === "asc") return <ArrowUp className="size-3" />
  if (sorted === "desc") return <ArrowDown className="size-3" />
  return <ArrowUpDown className="size-3 opacity-40" />
}

function ObjectTable({ concept, collected }: { readonly concept: Concept; readonly collected: readonly PageRow[] }) {
  const [sorting, setSorting] = useState<SortingState>([])
  const [filter, setFilter] = useState("")
  const sources = useQueries({
    queries: collected.map(({ record }) => ({
      queryKey: ["object", record.objectType, record.id],
      queryFn: () => transport.object(record.objectType, record.id),
    })),
  })
  // Row values are cached against `data`, so each row carries its source system.
  const systems = JSON.stringify(sources.map(({ data }) => data?.source ? systemName(data.source.document.sourceRecordRef.system) : "—"))
  const rows = useMemo(() => {
    const from = JSON.parse(systems) as string[]
    return collected.map((row, index): Row => ({ ...row, from: from[index] ?? "—" }))
  }, [collected, systems])
  const columns = useMemo<ColumnDef<Row>[]>(() => [
    {
      id: "record",
      header: "Record",
      accessorFn: ({ record }) => recordName(record),
      cell: ({ row }) => (
        <Link to="/sources/$concept/$" params={{ concept: row.original.record.objectType, _splat: row.original.record.id }} className="font-medium underline-offset-4 hover:underline">
          {recordName(row.original.record)}
        </Link>
      ),
    },
    ...concept.fields.filter(({ id }) => id !== "id" && id !== "label" && id !== "display_name").map((field): ColumnDef<Row> => ({
      id: field.id,
      header: field.id.replaceAll("_", " "),
      accessorFn: ({ record, at, numbers }) => factText(field, record, numbers, at),
      sortingFn: field.decimal
        ? (a, b) => {
          // Amounts in different currencies are grouped by currency, never ordered against each other.
          const currency = (row: Row) => String(field.decimal?.currencyFrom ? row.record.values[field.decimal.currencyFrom] ?? "" : "")
          return currency(a.original).localeCompare(currency(b.original))
            || compareDecimals(a.original.numbers.get(`${a.original.at}/values/${member(field.id)}`), b.original.numbers.get(`${b.original.at}/values/${member(field.id)}`))
        }
        : "alphanumeric",
      meta: { numeric: field.decimal !== undefined },
    })),
    { id: "from", header: "From", accessorFn: ({ from }) => from },
  ], [concept])
  const table = useReactTable({
    data: rows, columns, state: { sorting, globalFilter: filter },
    onSortingChange: setSorting, onGlobalFilterChange: setFilter,
    getCoreRowModel: getCoreRowModel(), getSortedRowModel: getSortedRowModel(),
    getFilteredRowModel: getFilteredRowModel(), getPaginationRowModel: getPaginationRowModel(),
    initialState: { pagination: { pageSize: 10 } },
  })
  const numeric = (id: string) => (table.getColumn(id)?.columnDef.meta as { numeric?: boolean } | undefined)?.numeric === true
  return (
    <div className="space-y-3">
      <label className="grid w-full max-w-xs gap-1 text-meta text-muted-foreground">
        Filter records
        <Input type="search" value={filter} onChange={(event) => setFilter(event.target.value)} />
      </label>
      <DenseTable head={table.getHeaderGroups().map((group) => (
        <TableRow key={group.id}>
          {group.headers.map((header) => (
            <TableHead key={header.id} className={numeric(header.column.id) ? "text-right" : undefined} aria-sort={header.column.getIsSorted() === "asc" ? "ascending" : header.column.getIsSorted() === "desc" ? "descending" : undefined}>
              <button type="button" className="inline-flex items-center gap-1" onClick={header.column.getToggleSortingHandler()}>
                {flexRender(header.column.columnDef.header, header.getContext())}<SortIcon sorted={header.column.getIsSorted()} />
              </button>
            </TableHead>
          ))}
        </TableRow>
      ))}>
        {table.getRowModel().rows.map((row) => (
          <TableRow key={row.id}>
            {row.getVisibleCells().map((cell) => (
              <TableCell key={cell.id} className={numeric(cell.column.id) ? "text-right" : undefined}>{flexRender(cell.column.columnDef.cell, cell.getContext())}</TableCell>
            ))}
          </TableRow>
        ))}
        {table.getRowModel().rows.length === 0 && <TableRow><TableCell colSpan={columns.length} className="text-muted-foreground">No record matches.</TableCell></TableRow>}
      </DenseTable>
      <div className="flex items-center justify-between text-meta text-muted-foreground">
        <span className="tabular-nums">{table.getFilteredRowModel().rows.length} of {rows.length} records</span>
        <div className="flex items-center gap-2">
          <Button variant="outline" size="sm" onClick={() => table.previousPage()} disabled={!table.getCanPreviousPage()}>Previous</Button>
          <span className="tabular-nums">Page {table.getState().pagination.pageIndex + 1} of {Math.max(table.getPageCount(), 1)}</span>
          <Button variant="outline" size="sm" onClick={() => table.nextPage()} disabled={!table.getCanNextPage()}>Next</Button>
        </div>
      </div>
    </div>
  )
}

/** An object type's records, read page by page from the server, for its object-type page. */
export function ObjectRecords({ concept }: { readonly concept: Concept }) {
  const name = concept.name
  const { read: catalogRead } = useCatalog()
  const pages = useInfiniteQuery({
    queryKey: ["objects", name],
    initialPageParam: undefined as string | undefined,
    queryFn: ({ pageParam }) => transport.objectPage(name, pageParam),
    getNextPageParam: nextAfter,
  })
  // Keep reading server pages until the server returns no cursor, within a bound.
  const { hasNextPage, isFetchingNextPage, isFetchNextPageError, fetchNextPage } = pages
  const read = pages.data?.pages.length ?? 0
  const readNext = shouldReadNext({ hasNextPage, isFetchingNextPage, isFetchNextPageError, pagesRead: read }, MAX_PAGES)
  useEffect(() => { if (readNext) void fetchNextPage() }, [readNext, fetchNextPage])
  const collected = useMemo(() => collect(pages.data?.pages ?? []), [pages.data])
  const first = pages.data?.pages[0]
  const reading = pages.isPending || isFetchingNextPage || readNext
  const problem = readProblem(catalogRead, pages)
  const title = concept.title
  const records = (body: ReactNode) => <Panel icon={Rows3} label="Records"><Well className="p-3">{body}</Well></Panel>
  return (
    <section className="space-y-6">
      {first && <div className="flex flex-wrap gap-8">
        <div><div className="text-4xl font-medium tabular-nums">{collected.complete ? "" : "at least "}{collected.rows.length}</div><div className="text-meta text-muted-foreground">records</div></div>
        <div><div className="text-4xl font-medium tabular-nums">{collected.held}</div><div className="text-meta text-muted-foreground">held back for review</div></div>
      </div>}
      {problem !== undefined && <div role="alert" className="flex flex-wrap items-center gap-3 text-destructive">
        <span>{problem}</span>
        {isFetchNextPageError && <Button variant="outline" size="sm" onClick={() => void fetchNextPage()}>Retry</Button>}
      </div>}
      {problem === undefined && (catalogRead.isPending || reading) && <p className="text-muted-foreground">Reading records…</p>}
      {pages.data !== undefined && !reading && (first === null
        ? <Panel icon={Rows3} label="Records"><Well><EmptyState icon={Rows3}>Records for this object type are not available yet.</EmptyState></Well></Panel>
        : !collected.complete
          ? records(<>
            <p className="pb-3 text-meta text-muted-foreground">The server has more pages than could be read here, so this list is incomplete.</p>
            {collected.rows.length > 0 && <ObjectTable concept={concept} collected={collected.rows} />}
          </>)
          : collected.rows.length === 0
            ? <Panel icon={Rows3} label="Records"><Well><EmptyState icon={Rows3}>No {title.toLowerCase()} records are stored yet.</EmptyState></Well></Panel>
            : records(<ObjectTable concept={concept} collected={collected.rows} />))}
    </section>
  )
}

/**
 * `/sources/$concept/$`: a record, its id the rest of the path. The splat also
 * matches the object type's own URL with an empty id, which is the type's page.
 */
export function SourcesItemPage() {
  const { _splat: id = "" } = useParams({ strict: false })
  return id === "" ? <ObjectTypePage /> : <ObjectPage />
}

/** Everything known about one record, tab by tab; each value opens where it came from. */
export function ObjectPage() {
  // The id is the rest of the path: ids hold `/`, and Cloudflare redirects an encoded `%2F` to a decoded one, so it cannot be one segment.
  const { concept: name = "", _splat: id = "" } = useParams({ strict: false })
  const { read: catalogRead, catalog } = useCatalog()
  const concept = catalog?.byName.get(name)
  const reads = useQuery({ queryKey: ["object", name, id], queryFn: () => transport.object(name, id) })
  const [tab, setTab] = useState("overview")
  const { view } = useView()
  const data = reads.data
  const lineage = data?.source?.document
  const problem = readProblem(catalogRead, reads)
  const { icon: TypeIcon, text, fill } = KINDS.objectType
  return (
    <section className="space-y-6">
      <header className="flex flex-wrap items-start gap-4">
        <span className={cn("flex size-11 shrink-0 items-center justify-center rounded-xl", fill, text)}><TypeIcon className="size-6" /></span>
        <div className="min-w-0 flex-1 space-y-1.5">
          <div className="text-meta text-muted-foreground">{concept?.title ?? conceptTitle(name)}</div>
          <div className="flex flex-wrap items-center gap-2">
            <h1 className="text-title font-medium">{data ? recordName(data.object.document) : id}</h1>
            {data?.object.origin === "recorded" && <Badge variant="outline" title={RECORDED_HINT}>Recorded</Badge>}
          </div>
          {data && <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-muted-foreground">
            <CopyId value={id} />
            <span className="text-meta">version {data.object.document.version}</span>
          </div>}
          {data && concept && <div className="flex flex-wrap gap-1.5 pt-1">
            {concept.rules.map((rule) => <Badge key={rule} variant="outline" className="max-w-xs gap-1" title={rule}><ShieldCheck className={KINDS.guarantee.text} /><span className="truncate">{rule}</span></Badge>)}
            {/* Personal data is marked by the server in the documents it sends; these mark none. */}
            <Badge variant="outline" className="gap-1 text-muted-foreground" title="The documents the server sent for this record mark no field as personal data."><ShieldOff />No personal data marked</Badge>
          </div>}
        </div>
      </header>
      {problem !== undefined && <p role="alert" className="text-destructive">{problem}</p>}
      {problem === undefined && (reads.isPending || catalogRead.isPending) && <p className="text-muted-foreground">Reading the record…</p>}
      {reads.isError && <p className="text-destructive">{reads.error.message}</p>}
      {reads.isSuccess && data === null && <p className="text-muted-foreground">This record is not available yet.</p>}
      {data && concept && (
        <Tabs value={tab} onValueChange={setTab}>
          <TabsList variant="line" className="w-full justify-start overflow-x-auto">
            {TABS.map(([value, label]) => <TabsTrigger key={value} value={value} className="flex-none">{label}</TabsTrigger>)}
          </TabsList>
          <TabsContent value="overview" className="space-y-6 pt-4">
            {concept.lifecycles.map((lifecycle) => (
              <Panel key={lifecycle.property} icon={GitBranch} label={`Lifecycle · ${lifecycle.property.replaceAll("_", " ")}`}>
                <Well className="px-4 py-3"><LifecycleStepper lifecycle={lifecycle} record={data.object.document} /></Well>
              </Panel>
            ))}
            <p className="text-panel">{summaryOf(concept, data, catalog?.concepts ?? [])}</p>
            <Facts concept={concept} reads={data} />
            <RelatedInline concept={concept} id={id} links={data.links.document} concepts={catalog?.concepts ?? []} />
            {view === "technical" && <Technical name={name} lineage={lineage} />}
          </TabsContent>
          <TabsContent value="relations" className="pt-4"><Relations concept={concept} id={id} links={data.links.document} /></TabsContent>
          <TabsContent value="lineage" className="pt-4"><Lineage lineage={lineage} /></TabsContent>
          <TabsContent value="guarantees" className="pt-4">
            <Panel icon={ShieldCheck} label="Laws over this object type"><Well className="p-3">
              {concept.rules.length === 0
                ? <Empty icon={ShieldCheck} action={<Button asChild variant="outline" size="sm"><Link to="/guarantees">See every guarantee</Link></Button>}>No law is declared over {concept.title.toLowerCase()} records.</Empty>
                : <ul className="space-y-2 text-sm">{concept.rules.map((rule) => <li key={rule} className="flex gap-2"><ShieldCheck className={cn("mt-0.5 size-4 shrink-0", KINDS.guarantee.text)} />{rule}</li>)}</ul>}
            </Well></Panel>
          </TabsContent>
          <TabsContent value="ledger" className="pt-4">
            <Panel icon={History} label="Ledger"><Well>
              <Empty icon={History} action={<Button variant="outline" size="sm" onClick={() => setTab("lineage")}>See where it came from</Button>}>The server does not publish this record's change history yet. Version {data.object.document.version} is the one stored.</Empty>
            </Well></Panel>
          </TabsContent>
          <TabsContent value="access" className="pt-4">
            <Panel icon={KeyRound} label="Access"><Well>
              <Empty icon={KeyRound} action={<Button asChild variant="outline" size="sm"><Link to="/status">See what is live</Link></Button>}>The recorded demo has no sign-in, so the server made no access decision for this read. With sign-in, each permit or refusal shows here with its policy.</Empty>
            </Well></Panel>
          </TabsContent>
        </Tabs>
      )}
    </section>
  )
}

const TABS = [["overview", "Overview"], ["relations", "Relations"], ["lineage", "Lineage"], ["guarantees", "Guarantees"], ["ledger", "Ledger"], ["access", "Access"]] as const

/** An empty tab: a picture, one sentence and one way forward. */
function Empty({ icon: Icon, action, children }: { readonly icon: LucideIcon; readonly action: ReactNode; readonly children: ReactNode }) {
  return (
    <div className="flex flex-col items-center gap-3 px-6 py-10 text-center">
      <span className="flex size-12 items-center justify-center rounded-full bg-muted"><Icon aria-hidden className="size-6 text-muted-foreground" /></span>
      <p className="max-w-sm text-sm text-muted-foreground">{children}</p>
      {action}
    </div>
  )
}

function Relations({ concept, id, links }: { readonly concept: Concept; readonly id: string; readonly links: LinkPage }) {
  return (
    <Panel icon={Link2} label="Related records"><Well className="p-3">
      {concept.relationships.length === 0 && links.items.length === 0
        ? <p className="text-sm text-muted-foreground">This concept declares no relationships.</p>
        : <ul className="space-y-2 text-sm">
          {concept.relationships.map((relationship) => {
            const edges = links.items.filter((edge) => edge.link === relationship.id && edge.source === id && edge.sourceType === concept.name)
            return (
              <li key={relationship.id}>
                <span className="text-muted-foreground">{relationship.id.replaceAll("_", " ")} ({relationship.count}) → </span>
                {edges.length === 0
                  ? <span className="text-muted-foreground">{links.hasMore ? "none on this page" : "none stored yet"}</span>
                  : edges.map((edge) => <Link key={`${edge.targetType}/${edge.target}`} to="/sources/$concept/$" params={{ concept: edge.targetType, _splat: edge.target }} className="mr-2 font-medium underline-offset-4 hover:underline">{edge.target}</Link>)}
              </li>
            )
          })}
          {links.items.filter((edge) => edge.target === id).map((edge) => (
            <li key={`${edge.sourceType}/${edge.source}/${edge.link}`}>
              <span className="text-muted-foreground">{edge.link.replaceAll("_", " ")} of </span>
              <Link to="/sources/$concept/$" params={{ concept: edge.sourceType, _splat: edge.source }} className="font-medium underline-offset-4 hover:underline">{edge.source}</Link>
              <span className="text-muted-foreground"> ({conceptTitle(edge.sourceType)})</span>
            </li>
          ))}
        </ul>}
      {links.hasMore && <p className="pt-2 text-meta text-muted-foreground">More links may exist; these are the first page the server returned.</p>}
    </Well></Panel>
  )
}

function Lineage({ lineage }: { readonly lineage: SourceLineage | undefined }) {
  const { inspect } = useInspector()
  return (
    <Panel icon={Database} label="Where it came from"><Well className="space-y-3 p-3">
      {!lineage && <EmptyState icon={Database}>No source record is retained for this record.</EmptyState>}
      {lineage && <>
        <div>
          <div className="text-panel font-medium">{systemName(lineage.sourceRecordRef.system)}</div>
          <p className="text-meta text-muted-foreground">Source record <code>{lineage.documentRef.record}</code>, mapped by <code>{lineage.mappingPin.artifactId}</code> version {lineage.mappingPin.version}</p>
        </div>
        <DenseTable head={<TableRow><TableHead>Fact</TableHead><TableHead>Read from</TableHead><TableHead>Rule</TableHead><TableHead>Recorded</TableHead></TableRow>}>
          {Object.entries(lineage.fieldProvenance).map(([fact, provenance]) => (
            <TableRow key={fact}>
              <TableCell className="font-medium"><button type="button" className="underline decoration-dotted underline-offset-4" onClick={() => inspect({ title: fact.replaceAll("_", " "), body: <ProvenanceChain lineage={lineage} fact={fact} /> })}>{fact.replaceAll("_", " ")}</button></TableCell>
              <TableCell><code className="text-xs">{(provenance.sourceFields ?? []).join(", ") || "—"}</code></TableCell>
              <TableCell><code className="text-xs">{provenance.transform}</code></TableCell>
              <TableCell>{provenance.recordedAt}</TableCell>
            </TableRow>
          ))}
        </DenseTable>
      </>}
    </Well></Panel>
  )
}

/** The record's facts in words, or, in the technical view, the exact document the server sent. */
function Facts({ concept, reads }: { readonly concept: Concept; readonly reads: ObjectReads }) {
  const card = useCardView()
  const lineage = reads.source?.document
  const toggle = <ViewToggle view={card.view} onChange={card.setView} label="View of this card" />
  if (card.view === "technical") {
    return <Panel icon={Braces} label="Record document" action={toggle}><Well className="p-3"><Code code={exactPretty(reads.object.document, reads.object.numbers, "")} /></Well></Panel>
  }
  return (
    <Panel icon={Rows3} label="Facts" action={toggle}><Well className="space-y-2 p-3">
      {lineage && <p className="text-meta text-muted-foreground">Select a value to see where it came from.</p>}
      <DenseTable head={<TableRow><TableHead>Fact</TableHead><TableHead>Value</TableHead></TableRow>}>
        {concept.fields.map((field) => (
          <TableRow key={field.id}>
            <TableCell className="font-medium">{field.id.replaceAll("_", " ")}</TableCell>
            <TableCell className={field.decimal ? "tabular-nums" : undefined}><ProvenanceValue lineage={lineage} fact={field.id}>{factText(field, reads.object.document, reads.object.numbers, "")}</ProvenanceValue></TableCell>
          </TableRow>
        ))}
      </DenseTable>
    </Well></Panel>
  )
}

/** A labelled id row. */
const Id = ({ label, value, short }: { readonly label: string; readonly value: string; readonly short?: number }) => (
  <div className="flex min-w-0 flex-col gap-x-3 sm:flex-row sm:items-baseline"><dt className="shrink-0 text-meta text-muted-foreground sm:w-32">{label}</dt><dd className="min-w-0 flex-1"><CopyId value={value} short={short} /></dd></div>
)

/** Only Norn artefacts: the typed definition and its laws, the mapping, the ids that pin them, and what a recorded snapshot cannot hold. */
function Technical({ name, lineage }: { readonly name: string; readonly lineage: SourceLineage | undefined }) {
  const { read } = useCatalog()
  // Who is reading, for display; the server decides what they may see.
  const viewer = useViewer()
  const artifacts = read.data?.document ?? []
  const index = artifacts.findIndex((artifact) => artifact.kind === "entity" && "objectType" in artifact.payload && artifact.payload.objectType.name === name)
  const entity = artifacts[index]
  const laws = artifacts.filter((artifact) => artifact.kind === "law" && artifact.payload.scope.objectTypes.includes(name))
  return <>
    {entity && "objectType" in entity.payload && (
      <Panel icon={KINDS.objectType.icon} label="Definition"><Well className="space-y-3 p-3">
        <dl className="space-y-1">
          <Id label="Artefact" value={`${entity.artifactId}@${entity.version}`} />
          {laws.map((law) => <Id key={law.artifactId} label="Law" value={`${law.artifactId}@${law.version}`} />)}
        </dl>
        <Code code={exactPretty(entity.payload.objectType, read.data?.numbers ?? new Map(), `/${index}/payload/objectType`)} />
      </Well></Panel>
    )}
    {lineage && (
      <Panel icon={KINDS.mapping.icon} label="Mapping and lineage"><Well className="space-y-3 p-3">
        <dl className="space-y-1">
          <Id label="Mapping" value={`${lineage.mappingPin.artifactId}@${lineage.mappingPin.version}`} />
          <Id label="Mapping sha256" value={lineage.mappingPin.sha256} short={16} />
          <Id label="Source record" value={lineage.sourceRecordRef.record} />
          <Id label="Bundle" value={lineage.bundleDigest} short={16} />
          <Id label="Document" value={lineage.documentDigest} short={16} />
        </dl>
        <DenseTable head={<TableRow><TableHead>Property</TableHead><TableHead>Rule</TableHead><TableHead>Source fields</TableHead></TableRow>}>
          {Object.entries(lineage.fieldProvenance).map(([fact, provenance]) => (
            <TableRow key={fact}><TableCell className="font-mono text-xs">{fact}</TableCell><TableCell className="font-mono text-xs">{provenance.transform}</TableCell><TableCell className="font-mono text-xs">{(provenance.sourceFields ?? []).join(", ")}</TableCell></TableRow>
          ))}
        </DenseTable>
      </Well></Panel>
    )}
    <Panel icon={KINDS.policy.icon} label="Decision and receipt"><Well className="space-y-3 p-3">
      <p className="text-sm">This page shows a recorded snapshot. Once live reads are available, they will return the access decision and receipt for your account.</p>
      <dl className="space-y-1 text-sm">
        <div className="flex flex-col gap-x-3 sm:flex-row"><dt className="shrink-0 text-meta text-muted-foreground sm:w-32">Viewing as</dt><dd>{viewer.name}{viewer.role !== null && <span className="text-muted-foreground"> · {viewer.role}</span>}</dd></div>
        <div className="flex flex-col gap-x-3 sm:flex-row"><dt className="shrink-0 text-meta text-muted-foreground sm:w-32">Access decision</dt><dd className="flex items-center gap-2"><span className={cn("size-2 shrink-0 rounded-full", STATUS.unknown.dot)} />Not in the recorded snapshot</dd></div>
        <div className="flex flex-col gap-x-3 sm:flex-row"><dt className="shrink-0 text-meta text-muted-foreground sm:w-32">Receipt</dt><dd className="flex items-center gap-2"><span className={cn("size-2 shrink-0 rounded-full", STATUS.unknown.dot)} />Not in the recorded snapshot</dd></div>
      </dl>
    </Well></Panel>
  </>
}

/** Other object types' declared relations that point at this one: `Holding.item → Item` is "held by" from the item's side. */
const incomingOf = (concept: Concept, concepts: readonly Concept[]) =>
  concepts.flatMap((other) => other.relationships.filter(({ target }) => target === concept.name).map((relationship) => ({ from: other, relationship })))

/** One plain sentence from what the server sent: the record, its type, where it was read from and what its stored links say. */
export function summaryOf(concept: Concept, reads: ObjectReads, concepts: readonly Concept[]): string {
  const source = reads.source?.document
  const { id, objectType } = reads.object.document
  const stored = reads.links.document.items.filter((edge) => (edge.source === id && edge.sourceType === objectType) || (edge.target === id && edge.targetType === objectType)).length
  const declared = concept.relationships.length + incomingOf(concept, concepts).length
  const from = source ? `, read from ${systemName(source.sourceRecordRef.system)} by mapping ${source.mappingPin.artifactId}` : ""
  // Only the first page of links may have been returned; then the count is a floor, not the whole.
  const more = reads.links.document.hasMore
  const links = declared === 0 ? "Its type declares no relations."
    : stored === 0 ? (more ? "No stored link appears on the first page the server returned; more may exist." : declared === 1 ? "Its one declared relation has no stored link in this snapshot." : `None of the ${declared} relations its type declares has a stored link in this snapshot.`)
    : `${more ? "At least " : ""}${stored} stored ${stored === 1 ? "link connects" : "links connect"} it to other records.`
  return `${recordName(reads.object.document)} is ${/^[aeiou]/i.test(concept.title) ? "an" : "a"} ${concept.title.toLowerCase()} record${from}. ${links}`
}

/** The record's relations on its overview, both directions, each with the stored links the server returned or an honest "none". */
function RelatedInline({ concept, id, links, concepts }: { readonly concept: Concept; readonly id: string; readonly links: LinkPage; readonly concepts: readonly Concept[] }) {
  const incoming = incomingOf(concept, concepts)
  if (concept.relationships.length === 0 && incoming.length === 0) return null
  const none = <span className="text-muted-foreground">{links.hasMore ? "none on this page" : "none in this snapshot"}</span>
  // One stored link's other end. Contested links (sources disagree, nothing added) never reach `items`, so nothing is shown for them.
  const record = (edge: LinkPage["items"][number], type: string, target: string) => (
    <span key={`${edge.link}/${type}/${target}`} className="me-3 inline-flex items-center gap-1">
      <Link to="/sources/$concept/$" params={{ concept: type, _splat: target }} className="font-medium underline-offset-4 hover:underline">{target}</Link>
      {/* Slot: the link's per-claim source lineage (observed_link_provenance) goes here once the server records it. */}
    </span>
  )
  return (
    <Panel icon={Link2} label="Related"><Well className="p-3">
      <dl className="space-y-2 text-sm">
        {concept.relationships.map((relationship) => {
          const edges = links.items.filter((edge) => edge.link === relationship.id && edge.source === id && edge.sourceType === concept.name)
          return <div key={`out/${relationship.id}`} className="flex flex-col gap-x-3 sm:flex-row"><dt className="shrink-0 text-muted-foreground sm:w-56">{relationship.id.replaceAll("_", " ")} → {relationship.count} {conceptTitle(relationship.target)}</dt><dd>{edges.length === 0 ? none : edges.map((edge) => record(edge, edge.targetType, edge.target))}</dd></div>
        })}
        {incoming.map(({ from, relationship }) => {
          const edges = links.items.filter((edge) => edge.link === relationship.id && edge.target === id && edge.targetType === concept.name && edge.sourceType === from.name)
          return <div key={`in/${from.name}/${relationship.id}`} className="flex flex-col gap-x-3 sm:flex-row"><dt className="shrink-0 text-muted-foreground sm:w-56">{from.title} records ({relationship.id.replaceAll("_", " ")})</dt><dd>{edges.length === 0 ? none : edges.map((edge) => record(edge, edge.sourceType, edge.source))}</dd></div>
        })}
      </dl>
    </Well></Panel>
  )
}
