import { keepPreviousData, useQuery, useQueryClient } from "@tanstack/react-query"
import { Link } from "@tanstack/react-router"
import { useVirtualizer } from "@tanstack/react-virtual"
import { ArrowDown, ArrowUp, ArrowUpDown, Columns3, ExternalLink, Filter, RefreshCw, Rows3 } from "lucide-react"
import { useEffect, useMemo, useRef, useState, type KeyboardEvent, type ReactNode } from "react"
import { CopyId } from "@/components/copy-id"
import { EmptyState, Panel, Well } from "@/components/panel"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { DropdownMenu, DropdownMenuCheckboxItem, DropdownMenuContent, DropdownMenuLabel, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/ui/dropdown-menu"
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover"
import { Sheet, SheetContent, SheetDescription, SheetHeader, SheetTitle } from "@/components/ui/sheet"
import { Skeleton } from "@/components/ui/skeleton"
import { cn } from "@/lib/utils"
import type { Concept, Field } from "../lib/catalog.ts"
import { NO_DATA_CELL } from "../lib/definitions.ts"
import { recordName } from "../lib/objects.ts"
import { cycleSort, DATA_CHANGED, LIMITS, recordsQueryParams, SERVER_QUERY_UNAVAILABLE, sortable, totalText, type Predicate, type RecordsQuery, type SortKey } from "../lib/records-query.ts"
import { diagnosticsAt, readRecords, type RecordsRead } from "../lib/records-source.ts"
import { transport } from "../lib/transport.ts"
import { alignRight, Cell, isWithheld, WITHHELD, WITHHELD_HINT, type GridRow } from "./cells.tsx"
import { blankClause, FilterEditor, filterable, fromPredicate, toPredicate, type DraftClause } from "./filter-editor.tsx"

const ROW = 36
const DEFAULT_COLUMNS = 8
const PAGE_SIZES = [50, 100, LIMITS.maxPageSize] as const
const HIDDEN_FIELDS = new Set(["id", "label", "display_name"])
const RECORDED = "Replayed documents the engine produced. The cell doesn't serve live records here yet."

export type Read = (objectType: string, query: RecordsQuery) => Promise<RecordsRead>

const nameOf = (row: GridRow) => isWithheld(row) ? row.record.id : recordName(row.record)

function SortMark({ direction }: { readonly direction: "asc" | "desc" | undefined }) {
  if (direction === "asc") return <ArrowUp className="size-3" aria-hidden />
  if (direction === "desc") return <ArrowDown className="size-3" aria-hidden />
  return <ArrowUpDown className="size-3 opacity-40" aria-hidden />
}

function Shell({ children }: { readonly children: ReactNode }) {
  return <Panel icon={Rows3} label="Records"><Well className="p-3">{children}</Well></Panel>
}

/**
 * An object type's records, one server page at a time. Sorting, filtering and paging are the
 * cell's: the grid sends the query and shows what the page says was applied. While the cell
 * doesn't serve the object route, the recorded documents are shown and the controls say why
 * they're off.
 */
export function RecordsGrid({ concept, read = readRecords }: { readonly concept: Concept; readonly read?: Read }) {
  const name = concept.name
  const fields = useMemo(() => concept.fields.filter(({ id }) => !HIDDEN_FIELDS.has(id)), [concept])
  const [sort, setSort] = useState<readonly SortKey[]>([])
  const [filter, setFilter] = useState<Predicate | null>(null)
  const [draft, setDraft] = useState<readonly DraftClause[]>([])
  const [limit, setLimit] = useState<number>(LIMITS.pageSize)
  const [cursors, setCursors] = useState<readonly (string | undefined)[]>([undefined])
  const [pageIndex, setPageIndex] = useState(0)
  const [shown, setShown] = useState<ReadonlySet<string>>(() => new Set(fields.slice(0, DEFAULT_COLUMNS).map(({ id }) => id)))
  const [active, setActive] = useState(0)
  const [opened, setOpened] = useState<GridRow | null>(null)
  const [notice, setNotice] = useState<string | null>(null)
  const [filterOpen, setFilterOpen] = useState(false)
  const client = useQueryClient()
  const query: RecordsQuery = { sort, filter, limit, ...cursors[pageIndex] === undefined ? {} : { after: cursors[pageIndex] } }
  const key = ["records", name, recordsQueryParams(query).toString()] as const
  const result = useQuery({ queryKey: key, queryFn: () => read(name, query), placeholderData: keepPreviousData, retry: false })
  const data = result.data

  const restart = () => { setCursors([undefined]); setPageIndex(0); setActive(0) }
  useEffect(() => {
    if (data?.kind !== "restart") return
    void client.removeQueries({ queryKey: ["records", name] })
    restart()
    setNotice(DATA_CHANGED)
  }, [data, client, name])

  // Controls are on only when the last page says the cell applied the query.
  const page = data?.kind === "page" ? data : undefined
  // While a refused or restarting read is shown, keep what the last page said.
  const lastServed = useRef(false)
  if (page !== undefined) lastServed.current = page.served !== null
  const canQuery = lastServed.current

  const rows = useMemo<readonly GridRow[]>(() => page === undefined ? [] : page.read.document.records.map((record, index) => ({ record, at: `/records/${index}`, numbers: page.read.numbers })), [page])
  const columns = fields.filter(({ id }) => shown.has(id))
  const template = `minmax(12rem,1.4fr) ${columns.map(() => "minmax(8rem,1fr)").join(" ")}`

  const scroller = useRef<HTMLDivElement>(null)
  const virtual = useVirtualizer({ count: rows.length, getScrollElement: () => scroller.current, estimateSize: () => ROW, overscan: 8, initialRect: { width: 1024, height: 480 } })

  const changeQuery = (next: { sort?: readonly SortKey[]; filter?: Predicate | null; limit?: number }) => {
    if (next.sort !== undefined) setSort(next.sort)
    if (next.filter !== undefined) setFilter(next.filter)
    if (next.limit !== undefined) setLimit(next.limit)
    restart()
    setNotice(null)
  }
  const applied = useMemo(() => fromPredicate(filter), [filter])
  const applyFilter = () => { changeQuery({ filter: toPredicate(draft, fields) }); setFilterOpen(false) }
  const clearFilter = () => { setDraft([]); changeQuery({ filter: null }) }

  const move = (to: number) => {
    const index = Math.max(0, Math.min(rows.length - 1, to))
    setActive(index)
    virtual.scrollToIndex(index, { align: "auto" })
  }
  const onKey = (event: KeyboardEvent<HTMLDivElement>) => {
    const steps: Record<string, number> = { ArrowDown: 1, ArrowUp: -1, PageDown: 10, PageUp: -10 }
    if (event.key in steps) { event.preventDefault(); move(active + (steps[event.key] ?? 0)) }
    else if (event.key === "Home") { event.preventDefault(); move(0) }
    else if (event.key === "End") { event.preventDefault(); move(rows.length - 1) }
    else if ((event.key === "Enter" || event.key === " ") && rows[active]) { event.preventDefault(); setOpened(rows[active] ?? null) }
  }

  if (data?.kind === "no-cell") return <Shell><EmptyState icon={Rows3} title="No data cell">{NO_DATA_CELL}</EmptyState></Shell>
  if (data?.kind === "none") return <Shell><EmptyState icon={Rows3}>Records for this object type are not available yet.</EmptyState></Shell>

  const refused = data?.kind === "refused" ? data : undefined
  const sortProblems = refused ? diagnosticsAt(refused.diagnostics, "/sort") : []
  const filterCount = filter?.all.length ?? 0
  const draftDirty = JSON.stringify(draft) !== JSON.stringify(applied)
  const off = canQuery ? undefined : SERVER_QUERY_UNAVAILABLE

  return (
    <Shell>
      <div className="grid gap-3">
        <div className="flex flex-wrap items-center gap-2" role="toolbar" aria-label="Records">
          <Popover open={filterOpen} onOpenChange={(open) => { setFilterOpen(open); if (open && draft.length === 0 && filterCount === 0) { const first = filterable(fields)[0]; if (first) setDraft([blankClause(first)]) } }}>
            <PopoverTrigger asChild>
              <Button variant="outline" size="sm" disabled={!canQuery || filterable(fields).length === 0} title={off}>
                <Filter /> Filter{filterCount > 0 && <Badge variant="secondary" className="ml-1">{filterCount}</Badge>}
                {draftDirty && <span className="size-1.5 rounded-full bg-primary" aria-label="Unapplied changes" />}
              </Button>
            </PopoverTrigger>
            <PopoverContent align="start" className="w-[min(44rem,90vw)]">
              <FilterEditor fields={fields} draft={draft} onDraft={setDraft} onApply={applyFilter} onClear={clearFilter} applied={applied} diagnostics={refused?.diagnostics ?? []} />
            </PopoverContent>
          </Popover>
          <DropdownMenu>
            <DropdownMenuTrigger asChild><Button variant="outline" size="sm"><Columns3 /> Columns</Button></DropdownMenuTrigger>
            <DropdownMenuContent align="start" className="max-h-80 overflow-y-auto">
              <DropdownMenuLabel>Show columns</DropdownMenuLabel>
              <DropdownMenuSeparator />
              {fields.map((field) => (
                <DropdownMenuCheckboxItem key={field.id} checked={shown.has(field.id)} disabled={shown.has(field.id) && shown.size === 1}
                  onSelect={(event) => event.preventDefault()}
                  onCheckedChange={(checked) => setShown((current) => { const next = new Set(current); if (checked) next.add(field.id); else next.delete(field.id); return next })}>
                  {field.id.replaceAll("_", " ")}
                </DropdownMenuCheckboxItem>
              ))}
            </DropdownMenuContent>
          </DropdownMenu>
          <label className="flex items-center gap-1 text-meta text-muted-foreground">
            Per page
            <select className="h-8 rounded-md border bg-background px-2 text-sm text-foreground" value={limit} disabled={!canQuery} title={off}
              onChange={(event) => changeQuery({ limit: Number(event.target.value) })}>
              {PAGE_SIZES.map((size) => <option key={size} value={size}>{size}</option>)}
            </select>
          </label>
          {sort.length > 0 && <Button variant="ghost" size="sm" onClick={() => changeQuery({ sort: [] })}>Clear sort</Button>}
          <span className="flex-1" />
          {page?.origin === "recorded" && <Badge variant="outline" title={RECORDED}>Recorded</Badge>}
          <span className="text-meta tabular-nums text-muted-foreground" aria-live="polite">
            {page?.total ? totalText(page.total) : page ? `${rows.length} on this page` : ""}
          </span>
        </div>
        {off !== undefined && page !== undefined && <p className="text-meta text-muted-foreground">{off}</p>}
        {notice !== null && <p role="status" className="flex items-center gap-1 text-meta text-muted-foreground"><RefreshCw className="size-3" aria-hidden />{notice}</p>}
        {refused && (
          <div role="alert" className="grid gap-1 rounded-md border border-destructive/40 p-3 text-sm text-destructive">
            <span>{refused.message}</span>
            {sortProblems.map((problem) => <span key={`${problem.pointer}:${problem.code}`} className="text-meta">{problem.message}</span>)}
            <span><Button variant="outline" size="sm" onClick={() => { setDraft([]); changeQuery({ sort: [], filter: null }) }}>Reset sort and filter</Button></span>
          </div>
        )}
        {result.isError && (
          <div role="alert" className="flex flex-wrap items-center gap-3 text-sm text-destructive">
            <span>{result.error instanceof Error ? result.error.message : "The records could not be read."}</span>
            <Button variant="outline" size="sm" onClick={() => void result.refetch()}>Retry</Button>
          </div>
        )}
        {page !== undefined && page.read.document.quarantineCount > 0 && (
          <p className="text-meta text-muted-foreground">{page.read.document.quarantineCount} held back on this page for review.</p>
        )}

        <div ref={scroller} role="grid" tabIndex={0} aria-label={`${concept.title} records`} aria-rowcount={rows.length + 1} aria-colcount={columns.length + 1}
          aria-busy={result.isFetching} aria-activedescendant={rows.length > 0 ? `row-${active}` : undefined} onKeyDown={onKey}
          className={cn("relative max-h-[60vh] overflow-auto rounded-lg border text-[13px] outline-none focus-visible:ring-2 focus-visible:ring-ring", result.isFetching && "opacity-70")}>
          <div role="row" aria-rowindex={1} className="sticky top-0 z-10 grid border-b bg-muted/60 backdrop-blur" style={{ gridTemplateColumns: template }}>
            <div role="columnheader" className="px-3 py-2 font-medium">Record</div>
            {columns.map((field: Field) => {
              const key = sort.find(({ property }) => property === field.id)
              const can = canQuery && sortable(field)
              return (
                <div key={field.id} role="columnheader" aria-sort={key ? (key.direction === "asc" ? "ascending" : "descending") : undefined} className={cn("px-3 py-2 font-medium", alignRight(field) && "text-right")}>
                  <button type="button" disabled={!can} title={can ? "Sort (Shift adds a key)" : off ?? "This kind of value can't be sorted."}
                    className="inline-flex items-center gap-1 disabled:cursor-default" onClick={(event) => changeQuery({ sort: cycleSort(sort, field.id, event.shiftKey) })}>
                    {field.id.replaceAll("_", " ")}{can && <SortMark direction={key?.direction} />}
                    {sort.length > 1 && key && <sup className="tabular-nums" aria-hidden>{sort.indexOf(key) + 1}</sup>}
                  </button>
                </div>
              )
            })}
          </div>
          {result.isPending && <div className="grid gap-1 p-3" aria-label="Reading records">{Array.from({ length: 6 }, (_, i) => <Skeleton key={i} className="h-6" />)}</div>}
          {page !== undefined && rows.length === 0 && (
            <div className="p-6">
              {filterCount > 0
                ? <EmptyState icon={Filter} title="No matches">No records match this filter. <Button variant="link" className="h-auto p-0" onClick={clearFilter}>Clear filter</Button></EmptyState>
                : <EmptyState icon={Rows3}>No {concept.title.toLowerCase()} records are stored yet.</EmptyState>}
            </div>
          )}
          <div className="relative" style={{ height: virtual.getTotalSize() }}>
            {virtual.getVirtualItems().map((item) => {
              const row = rows[item.index]
              if (row === undefined) return null
              const withheld = isWithheld(row)
              return (
                <div key={`${row.record.objectType}:${row.record.id}`} id={`row-${item.index}`} role="row" aria-rowindex={item.index + 2} aria-selected={item.index === active}
                  data-withheld={withheld || undefined}
                  className={cn("absolute inset-x-0 grid cursor-pointer items-center border-b hover:bg-muted/40", item.index === active && "bg-accent/60")}
                  style={{ gridTemplateColumns: template, height: ROW, transform: `translateY(${item.start}px)` }}
                  onClick={() => { setActive(item.index); setOpened(row) }}>
                  <div role="gridcell" className="truncate px-3 font-medium">{nameOf(row)}</div>
                  {columns.map((field) => <div key={field.id} role="gridcell" className={cn("truncate px-3", alignRight(field) && "text-right")}><Cell field={field} row={row} /></div>)}
                </div>
              )
            })}
          </div>
        </div>

        <nav className="flex items-center justify-end gap-2 text-meta text-muted-foreground" aria-label="Pages">
          <Button variant="outline" size="sm" disabled={pageIndex === 0} onClick={() => { setPageIndex(pageIndex - 1); setActive(0) }}>Previous</Button>
          <span className="tabular-nums">Page {pageIndex + 1}</span>
          <Button variant="outline" size="sm" disabled={page?.next === undefined || result.isFetching}
            onClick={() => { const next = page?.next; if (next === undefined) return; setCursors([...cursors.slice(0, pageIndex + 1), next]); setPageIndex(pageIndex + 1); setActive(0) }}>Next</Button>
        </nav>
      </div>
      <RecordDrawer concept={concept} fields={fields} row={opened} origin={page?.origin} onClose={() => setOpened(null)} />
    </Shell>
  )
}

/** One record, from the page that listed it; its links come from the object read when the cell serves it. */
function RecordDrawer({ concept, fields, row, origin, onClose }: {
  readonly concept: Concept; readonly fields: readonly Field[]; readonly row: GridRow | null; readonly origin: "live" | "recorded" | undefined; readonly onClose: () => void
}) {
  const record = row?.record
  const object = useQuery({
    queryKey: ["object", record?.objectType, record?.id],
    queryFn: () => record ? transport.object(record.objectType, record.id) : null,
    enabled: record !== undefined && row !== null && !isWithheld(row),
  })
  const links = object.data?.links.document.items ?? []
  return (
    <Sheet open={row !== null} onOpenChange={(open) => { if (!open) onClose() }}>
      <SheetContent className="w-[min(32rem,100vw)] overflow-y-auto sm:max-w-lg">
        {row !== null && record !== undefined && <>
          <SheetHeader>
            <SheetTitle>{nameOf(row)}</SheetTitle>
            <SheetDescription className="flex items-center gap-2"><span>{concept.title}</span><CopyId value={record.id} /></SheetDescription>
          </SheetHeader>
          <div className="grid gap-4 px-4 pb-6">
            {isWithheld(row)
              ? <p className="rounded-md border p-3 text-sm text-muted-foreground" title={WITHHELD_HINT}>{WITHHELD}: {WITHHELD_HINT}</p>
              : <dl className="grid grid-cols-[minmax(8rem,auto)_1fr] gap-x-4 gap-y-2 text-sm">
                {fields.map((field) => <div key={field.id} className="contents"><dt className="text-muted-foreground">{field.id.replaceAll("_", " ")}</dt><dd className="min-w-0 break-words"><Cell field={field} row={row} /></dd></div>)}
                <dt className="text-muted-foreground">version</dt><dd className="tabular-nums">{record.version}</dd>
              </dl>}
            {!isWithheld(row) && <section className="grid gap-2">
              <h3 className="text-sm font-medium">Linked records</h3>
              {object.isPending ? <Skeleton className="h-5" />
                : object.data === null || object.data === undefined
                  ? <p className="text-meta text-muted-foreground">{origin === "live" ? "This cell doesn't serve a record's links yet." : "No recorded links for this record."}</p>
                  : links.length === 0 ? <p className="text-meta text-muted-foreground">Nothing links to or from this record.</p>
                    : <ul className="grid gap-1 text-sm">{links.map((link) => {
                      const outgoing = link.source === record.id
                      const [type, id] = outgoing ? [link.targetType, link.target] : [link.sourceType, link.source]
                      return <li key={`${link.link}:${link.source}:${link.target}`}><span className="text-muted-foreground">{link.link.replaceAll("_", " ")} {outgoing ? "→" : "←"} </span>
                        <Link to="/sources/$concept/$" params={{ concept: type, _splat: id }} className="underline-offset-4 hover:underline">{id}</Link></li>
                    })}</ul>}
            </section>}
            <Button asChild variant="outline" size="sm" className="justify-self-start">
              <Link to="/sources/$concept/$" params={{ concept: record.objectType, _splat: record.id }}><ExternalLink /> Open full record</Link>
            </Button>
          </div>
        </>}
      </SheetContent>
    </Sheet>
  )
}
