import { useInfiniteQuery, useQueries, useQuery } from "@tanstack/react-query"
import { Link, useParams } from "@tanstack/react-router"
import {
  type ColumnDef, type SortingState, flexRender, getCoreRowModel, getFilteredRowModel,
  getPaginationRowModel, getSortedRowModel, useReactTable,
} from "@tanstack/react-table"
import { ArrowDown, ArrowUp, ArrowUpDown, Database, Link2, Rows3 } from "lucide-react"
import { useEffect, useMemo, useState, type ReactNode } from "react"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { EmptyState, Panel, Well } from "@/components/panel"
import { Input } from "@/components/ui/input"
import { TableCell, TableHead, TableRow } from "@/components/ui/table"
import { conceptTitle, systemName, type Concept } from "./lib/catalog.ts"
import { member } from "./lib/exact.ts"
import { collect, compareDecimals, factText, nextAfter, readProblem, recordName, type PageRow } from "./lib/objects.ts"
import { recordedTransport } from "./lib/transport.ts"
import { DenseTable, RECORDED_HINT, useCatalog } from "./sources.tsx"

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
      queryFn: () => recordedTransport.object(record.objectType, record.id),
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
        <Link to="/sources/$concept/$id" params={{ concept: row.original.record.objectType, id: row.original.record.id }} className="font-medium underline-offset-4 hover:underline">
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

export function ObjectsPage() {
  const { concept: name = "" } = useParams({ strict: false })
  const { read: catalogRead, catalog } = useCatalog()
  const concept = catalog?.byName.get(name)
  const pages = useInfiniteQuery({
    queryKey: ["objects", name],
    initialPageParam: undefined as string | undefined,
    queryFn: ({ pageParam }) => recordedTransport.objectPage(name, pageParam),
    getNextPageParam: nextAfter,
  })
  // Keep reading server pages until the server returns no cursor, within a bound.
  const { hasNextPage, isFetchingNextPage, fetchNextPage } = pages
  const read = pages.data?.pages.length ?? 0
  useEffect(() => { if (hasNextPage && !isFetchingNextPage && read < MAX_PAGES) void fetchNextPage() }, [hasNextPage, isFetchingNextPage, read, fetchNextPage])
  const collected = useMemo(() => collect(pages.data?.pages ?? []), [pages.data])
  const first = pages.data?.pages[0]
  const reading = pages.isPending || isFetchingNextPage || (hasNextPage && read < MAX_PAGES)
  const problem = readProblem(catalogRead, pages)
  const title = concept?.title ?? conceptTitle(name)
  const records = (body: ReactNode) => <Panel icon={Rows3} label="Records"><Well className="p-3">{body}</Well></Panel>
  return (
    <section className="space-y-6">
      <div className="space-y-2">
        <Link to="/sources" className="text-sm text-muted-foreground underline-offset-4 hover:underline">Sources</Link>
        <div className="flex flex-wrap items-center gap-2">
          <h1 className="text-title font-medium">{title}</h1>
          {first?.origin === "recorded" && <Badge variant="outline" title={RECORDED_HINT}>Recorded</Badge>}
        </div>
        {concept?.description && <p className="max-w-2xl text-muted-foreground">{concept.description}</p>}
      </div>
      {first && <div className="flex flex-wrap gap-8">
        <div><div className="text-4xl font-medium tabular-nums">{collected.complete ? "" : "at least "}{collected.rows.length}</div><div className="text-meta text-muted-foreground">records</div></div>
        <div><div className="text-4xl font-medium tabular-nums">{collected.held}</div><div className="text-meta text-muted-foreground">held back for review</div></div>
      </div>}
      {problem !== undefined && <p role="alert" className="text-destructive">{problem}</p>}
      {problem === undefined && (catalogRead.isPending || reading) && <p className="text-muted-foreground">Reading records…</p>}
      {catalog && !concept && <p className="text-muted-foreground">This concept is not in the business model.</p>}
      {concept && pages.isSuccess && !reading && (first === null
        ? <Panel icon={Rows3} label="Records"><Well><EmptyState icon={Rows3}>Records for this concept are not available yet.</EmptyState></Well></Panel>
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

export function ObjectPage() {
  const { concept: name = "", id = "" } = useParams({ strict: false })
  const { read: catalogRead, catalog } = useCatalog()
  const concept = catalog?.byName.get(name)
  const reads = useQuery({ queryKey: ["object", name, id], queryFn: () => recordedTransport.object(name, id) })
  const data = reads.data
  const lineage = data?.source?.document
  const problem = readProblem(catalogRead, reads)
  return (
    <section className="space-y-6">
      <div className="space-y-2">
        <div className="flex gap-1 text-sm text-muted-foreground">
          <Link to="/sources" className="underline-offset-4 hover:underline">Sources</Link>/
          <Link to="/sources/$concept" params={{ concept: name }} className="underline-offset-4 hover:underline">{concept?.title ?? conceptTitle(name)}</Link>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          <h1 className="text-title font-medium">{data ? recordName(data.object.document) : id}</h1>
          {data?.object.origin === "recorded" && <Badge variant="outline" title={RECORDED_HINT}>Recorded</Badge>}
        </div>
        {data && <p className="text-sm text-muted-foreground"><code>{id}</code> · version {data.object.document.version}</p>}
      </div>
      {problem !== undefined && <p role="alert" className="text-destructive">{problem}</p>}
      {problem === undefined && (reads.isPending || catalogRead.isPending) && <p className="text-muted-foreground">Reading the record…</p>}
      {reads.isError && <p className="text-destructive">{reads.error.message}</p>}
      {reads.isSuccess && data === null && <p className="text-muted-foreground">This record is not available yet.</p>}
      {data && concept && <>
        <Panel icon={Rows3} label="Facts"><Well className="p-3">
          <DenseTable head={<TableRow><TableHead>Fact</TableHead><TableHead>Value</TableHead></TableRow>}>
            {concept.fields.map((field) => (
              <TableRow key={field.id}>
                <TableCell className="font-medium">{field.id.replaceAll("_", " ")}</TableCell>
                <TableCell className={field.decimal ? "tabular-nums" : undefined}>{factText(field, data.object.document, data.object.numbers, "")}</TableCell>
              </TableRow>
            ))}
          </DenseTable>
        </Well></Panel>
        <Panel icon={Link2} label="Related records"><Well className="p-3">
          {concept.relationships.length === 0 && data.links.document.items.length === 0
            ? <p className="text-sm text-muted-foreground">This concept declares no relationships.</p>
            : <ul className="space-y-2 text-sm">
              {concept.relationships.map((relationship) => {
                const edges = data.links.document.items.filter((edge) => edge.link === relationship.id && edge.source === id)
                return (
                  <li key={relationship.id}>
                    <span className="text-muted-foreground">{relationship.id.replaceAll("_", " ")} → </span>
                    {edges.length === 0
                      ? <span className="text-muted-foreground">{data.links.document.hasMore ? "none on this page" : "none stored yet"}</span>
                      : edges.map((edge) => <Link key={`${edge.targetType}/${edge.target}`} to="/sources/$concept/$id" params={{ concept: edge.targetType, id: edge.target }} className="mr-2 font-medium underline-offset-4 hover:underline">{edge.target}</Link>)}
                  </li>
                )
              })}
              {data.links.document.items.filter((edge) => edge.target === id).map((edge) => (
                <li key={`${edge.sourceType}/${edge.source}/${edge.link}`}>
                  <span className="text-muted-foreground">{edge.link.replaceAll("_", " ")} of </span>
                  <Link to="/sources/$concept/$id" params={{ concept: edge.sourceType, id: edge.source }} className="font-medium underline-offset-4 hover:underline">{edge.source}</Link>
                  <span className="text-muted-foreground"> ({conceptTitle(edge.sourceType)})</span>
                </li>
              ))}
            </ul>}
          {data.links.document.hasMore && <p className="pt-2 text-meta text-muted-foreground">More links may exist; these are the first page the server returned.</p>}
        </Well></Panel>
        <Panel icon={Database} label="Where it came from"><Well className="space-y-3 p-3">
          {!lineage && <EmptyState icon={Database}>No source record is retained for this record.</EmptyState>}
          {lineage && <>
            <div>
              <div className="text-panel font-medium">{systemName(lineage.sourceRecordRef.system)}</div>
              <p className="text-meta text-muted-foreground">Source record <code>{lineage.documentRef.record}</code>, mapped by <code>{lineage.mappingPin.artifactId}</code> version {lineage.mappingPin.version}</p>
            </div>
            <DenseTable head={<TableRow><TableHead>Fact</TableHead><TableHead>Read from</TableHead><TableHead>Recorded</TableHead></TableRow>}>
              {Object.entries(lineage.fieldProvenance).map(([fact, provenance]) => (
                <TableRow key={fact}>
                  <TableCell className="font-medium">{fact.replaceAll("_", " ")}</TableCell>
                  <TableCell><code className="text-xs">{(provenance.sourceFields ?? []).join(", ") || "—"}</code></TableCell>
                  <TableCell>{provenance.recordedAt}</TableCell>
                </TableRow>
              ))}
            </DenseTable>
          </>}
        </Well></Panel>
      </>}
    </section>
  )
}
