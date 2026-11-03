import { useQueries, useQuery } from "@tanstack/react-query"
import { Link, useParams } from "@tanstack/react-router"
import {
  type ColumnDef, type SortingState, flexRender, getCoreRowModel, getFilteredRowModel,
  getPaginationRowModel, getSortedRowModel, useReactTable,
} from "@tanstack/react-table"
import { ArrowDown, ArrowUp, ArrowUpDown } from "lucide-react"
import { useMemo, useState } from "react"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card"
import { Input } from "@/components/ui/input"
import { TableCell, TableHead, TableRow } from "@/components/ui/table"
import { conceptTitle, systemName, type Concept } from "./lib/catalog.ts"
import { member, type Read } from "./lib/exact.ts"
import { compareDecimals, factText, recordName } from "./lib/objects.ts"
import { recordedTransport, type ObjectRecord } from "./lib/transport.ts"
import { DenseTable, RECORDED_HINT, useCatalog } from "./sources.tsx"

interface Row {
  readonly record: ObjectRecord
  readonly at: string
  readonly from: string
}

function SortIcon({ sorted }: { readonly sorted: false | "asc" | "desc" }) {
  if (sorted === "asc") return <ArrowUp className="size-3" />
  if (sorted === "desc") return <ArrowDown className="size-3" />
  return <ArrowUpDown className="size-3 opacity-40" />
}

function ObjectTable({ concept, page }: { readonly concept: Concept; readonly page: Read<{ readonly records: readonly ObjectRecord[] }> }) {
  const [sorting, setSorting] = useState<SortingState>([])
  const [filter, setFilter] = useState("")
  const sources = useQueries({
    queries: page.document.records.map((record) => ({
      queryKey: ["object", record.objectType, record.id],
      queryFn: () => recordedTransport.object(record.objectType, record.id),
    })),
  })
  // Row values are cached against `data`, so each row carries its source system.
  const systems = JSON.stringify(sources.map(({ data }) => data?.source ? systemName(data.source.document.sourceRecordRef.system) : "—"))
  const rows = useMemo(() => {
    const from = JSON.parse(systems) as string[]
    return page.document.records.map((record, index): Row => ({ record, at: `/records/${index}`, from: from[index] ?? "—" }))
  }, [page, systems])
  const columns = useMemo<ColumnDef<Row>[]>(() => [
    {
      id: "record",
      header: "Record",
      accessorFn: ({ record }) => recordName(record),
      cell: ({ row }) => (
        <Link to="/sources/$concept/$id" params={{ concept: concept.name, id: row.original.record.id }} className="font-medium underline-offset-4 hover:underline">
          {recordName(row.original.record)}
        </Link>
      ),
    },
    ...concept.fields.filter(({ id }) => id !== "id" && id !== "label" && id !== "display_name").map((field): ColumnDef<Row> => ({
      id: field.id,
      header: field.id.replaceAll("_", " "),
      accessorFn: ({ record, at }) => factText(field, record, page.numbers, at),
      sortingFn: field.decimal
        ? (a, b) => {
          // Amounts in different currencies are grouped by currency, never ordered against each other.
          const currency = (row: Row) => String(field.decimal?.currencyFrom ? row.record.values[field.decimal.currencyFrom] ?? "" : "")
          return currency(a.original).localeCompare(currency(b.original))
            || compareDecimals(page.numbers.get(`${a.original.at}/values/${member(field.id)}`), page.numbers.get(`${b.original.at}/values/${member(field.id)}`))
        }
        : "alphanumeric",
      meta: { numeric: field.decimal !== undefined },
    })),
    { id: "from", header: "From", accessorFn: ({ from }) => from },
  ], [concept, page])
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
      <label className="grid w-full max-w-xs gap-1 text-sm text-muted-foreground">
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
      <div className="flex items-center justify-between text-sm text-muted-foreground">
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
  const { catalog } = useCatalog()
  const concept = catalog?.byName.get(name)
  const page = useQuery({ queryKey: ["objects", name], queryFn: () => recordedTransport.objectPage(name) })
  const title = concept?.title ?? conceptTitle(name)
  const held = page.data?.document.quarantineCount ?? 0
  return (
    <section className="space-y-6">
      <div className="space-y-2">
        <Link to="/sources" className="text-sm text-muted-foreground underline-offset-4 hover:underline">Sources</Link>
        <div className="flex flex-wrap items-center gap-2">
          <h1 className="text-3xl font-semibold tracking-tight">{title}</h1>
          {page.data?.origin === "recorded" && <Badge variant="outline" title={RECORDED_HINT}>Recorded</Badge>}
        </div>
        {concept?.description && <p className="max-w-2xl text-muted-foreground">{concept.description}</p>}
      </div>
      {page.data && <div className="flex flex-wrap gap-8">
        <div><div className="text-4xl font-semibold tabular-nums">{page.data.document.records.length}</div><div className="text-sm text-muted-foreground">records</div></div>
        <div><div className="text-4xl font-semibold tabular-nums">{held}</div><div className="text-sm text-muted-foreground">held back for review</div></div>
      </div>}
      {(page.isPending || !catalog) && <p className="text-muted-foreground">Reading records…</p>}
      {page.isError && <p className="text-destructive">{page.error.message}</p>}
      {catalog && !concept && <p className="text-muted-foreground">This concept is not in the business model.</p>}
      {concept && page.isSuccess && (page.data === null
        ? <p className="text-muted-foreground">Records for this concept are not available yet.</p>
        : page.data.document.records.length === 0
          ? <p className="text-muted-foreground">No {title.toLowerCase()} records are stored yet.</p>
          : <ObjectTable concept={concept} page={page.data} />)}
    </section>
  )
}

export function ObjectPage() {
  const { concept: name = "", id = "" } = useParams({ strict: false })
  const { catalog } = useCatalog()
  const concept = catalog?.byName.get(name)
  const reads = useQuery({ queryKey: ["object", name, id], queryFn: () => recordedTransport.object(name, id) })
  const data = reads.data
  const lineage = data?.source?.document
  return (
    <section className="space-y-6">
      <div className="space-y-2">
        <div className="flex gap-1 text-sm text-muted-foreground">
          <Link to="/sources" className="underline-offset-4 hover:underline">Sources</Link>/
          <Link to="/sources/$concept" params={{ concept: name }} className="underline-offset-4 hover:underline">{concept?.title ?? conceptTitle(name)}</Link>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          <h1 className="text-3xl font-semibold tracking-tight">{data ? recordName(data.object.document) : id}</h1>
          {data?.object.origin === "recorded" && <Badge variant="outline" title={RECORDED_HINT}>Recorded</Badge>}
        </div>
        {data && <p className="text-sm text-muted-foreground"><code>{id}</code> · version {data.object.document.version}</p>}
      </div>
      {(reads.isPending || !catalog) && <p className="text-muted-foreground">Reading the record…</p>}
      {reads.isError && <p className="text-destructive">{reads.error.message}</p>}
      {reads.isSuccess && data === null && <p className="text-muted-foreground">This record is not available yet.</p>}
      {data && concept && <>
        <section>
          <h2 className="pb-2 text-lg font-medium">Facts</h2>
          <DenseTable head={<TableRow><TableHead>Fact</TableHead><TableHead>Value</TableHead></TableRow>}>
            {concept.fields.map((field) => (
              <TableRow key={field.id}>
                <TableCell className="font-medium">{field.id.replaceAll("_", " ")}</TableCell>
                <TableCell className={field.decimal ? "tabular-nums" : undefined}>{factText(field, data.object.document, data.object.numbers, "")}</TableCell>
              </TableRow>
            ))}
          </DenseTable>
        </section>
        <section>
          <h2 className="pb-2 text-lg font-medium">Related records</h2>
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
          {data.links.document.hasMore && <p className="pt-2 text-sm text-muted-foreground">More links exist than this read returned; the ones shown are the first page.</p>}
        </section>
        {!lineage && <p className="text-sm text-muted-foreground">No source record is retained for this record.</p>}
        {lineage && <Card>
          <CardHeader>
            <CardDescription>Where it came from</CardDescription>
            <CardTitle className="text-xl">{systemName(lineage.sourceRecordRef.system)}</CardTitle>
            <CardDescription>Source record <code>{lineage.documentRef.record}</code>, mapped by <code>{lineage.mappingPin.artifactId}</code> version {lineage.mappingPin.version}</CardDescription>
          </CardHeader>
          <CardContent>
            <DenseTable head={<TableRow><TableHead>Fact</TableHead><TableHead>Read from</TableHead><TableHead>Recorded</TableHead></TableRow>}>
              {Object.entries(lineage.fieldProvenance).map(([fact, provenance]) => (
                <TableRow key={fact}>
                  <TableCell className="font-medium">{fact.replaceAll("_", " ")}</TableCell>
                  <TableCell><code className="text-xs">{(provenance.sourceFields ?? []).join(", ") || "—"}</code></TableCell>
                  <TableCell>{provenance.recordedAt}</TableCell>
                </TableRow>
              ))}
            </DenseTable>
          </CardContent>
        </Card>}
      </>}
    </section>
  )
}
