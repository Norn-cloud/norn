import { keepPreviousData, useQuery, useQueryClient } from "@tanstack/react-query"
import { MarkingChips, useMarkingCatalog } from "@/components/marking-chip"
import { Link, useNavigate } from "@tanstack/react-router"
import { useVirtualizer } from "@tanstack/react-virtual"
import { ArrowDown, ArrowUp, ArrowUpDown, ChevronDown, Columns3, ExternalLink, Filter, Play, Plus, RefreshCw, Rows3 } from "lucide-react"
import { useEffect, useMemo, useRef, useState, type KeyboardEvent, type ReactNode } from "react"
import { CopyId, PathText } from "@/components/copy-id"
import { LoadState, ReadErrorState, WorkspaceUnavailable, WorkspaceUndecided, isWorkspaceUnavailableError, useBoundedWait } from "@/components/load-state"
import { EmptyState } from "@/components/panel"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { DropdownMenu, DropdownMenuCheckboxItem, DropdownMenuContent, DropdownMenuItem, DropdownMenuLabel, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/ui/dropdown-menu"
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover"
import { Sheet, SheetBody, SheetContent, SheetDescription, SheetFooter, SheetHeader, SheetSection, SheetTitle } from "@/components/ui/sheet"
import { Skeleton } from "@/components/ui/skeleton"
import { cn } from "@/lib/utils"
import type { Concept, Field } from "../lib/catalog.ts"
import { CellUnavailable, NO_DATA_CELL, studioScopeKey, StudioScopeError, type StudioScope } from "../lib/definitions.ts"
import { recordName } from "../lib/objects.ts"
import { cycleSort, DATA_CHANGED, LIMITS, SERVER_QUERY_UNAVAILABLE, sortable, totalText, type Predicate, type RecordsQuery, type SortKey } from "../lib/records-query.ts"
import { diagnosticsAt, type RecordsRead } from "../lib/records-source.ts"
import { readPage, useWorkspace, type RecordedReader, type ScopedReader } from "../lib/workspace.tsx"
import { ACTIONS_UNAVAILABLE, writesWhole } from "../lib/actions.ts"
import { type ActionSummary } from "../../../../packages/norn-sdk/src/index.ts"
import { ActionSheet, useActions, type ActionScopeBinding, type OpenAction } from "../actions/action-sheet.tsx"
import { offeredFor, recordCatalog } from "../lib/screen/call.ts"
import { alignRight, isWithheld, TracedCell, WITHHELD_HINT, WITHHELD_ROW, WithheldMark, type GridRow, RecordName } from "./cells.tsx"
import { blankClause, FilterEditor, filterable, fromPredicate, toPredicate, type DraftClause } from "./filter-editor.tsx"
import { FacetsRail, FilterChips } from "./facets-rail.tsx"
import { RecordsTimeline } from "./records-timeline.tsx"
import { LifecycleView } from "@/components/lifecycle-view"
import { RevealScope } from "@/components/masked-value"
import { ToggleGroup, ToggleGroupItem } from "@/components/ui/toggle-group"
import { TIMELINE_MAX_USER_CLAUSES, stepAnchor, tryWindowQuery, windowOf, type Zoom } from "../lib/timeline.ts"

const ROW = 32
const DEFAULT_COLUMNS = 8
const PAGE_SIZES = [50, 100, LIMITS.maxPageSize] as const
const HIDDEN_FIELDS = new Set(["id", "label", "display_name"])
const RECORDED = "Replayed documents the engine produced. The cell doesn't serve live records here yet."
const RECORDED_ACTIONS = "Recorded records take no actions."


/** Where a grid reads its records: a decided workspace's cell, or the recording (`lib/workspace.tsx`). */
export type GridReader = ScopedReader | RecordedReader

const nameOf = (row: GridRow) => isWithheld(row) ? row.record.id : recordName(row.record)

function SortMark({ direction }: { readonly direction: "asc" | "desc" | undefined }) {
  if (direction === "asc") return <ArrowUp className="size-3" aria-hidden />
  if (direction === "desc") return <ArrowDown className="size-3" aria-hidden />
  return <ArrowUpDown className="size-3 opacity-40" aria-hidden />
}

/** The Records section: flat on the page (design v3), its heading, then the rail, the toolbar and the one framed table. */
function Shell({ children }: { readonly children: ReactNode }) {
  return (
    <section aria-label="Records" className="min-w-0">
      <h2 className="flex items-center gap-2 pb-3 text-heading"><Rows3 className="size-4 text-muted-foreground" aria-hidden />Records</h2>
      {children}
    </section>
  )
}

/** The Record column stays put while the others scroll sideways. */
const STICKY = "sticky start-0 z-10 shadow-[1px_0_0_var(--node-border)]"

/**
 * An object type's records, one server page at a time. Sorting, filtering and paging are the
 * cell's: the grid sends the query and shows what the page says was applied. While the cell
 * doesn't serve the object route, the recorded documents are shown and the controls say why
 * they're off.
 */
/** The query a grid shows, as a saved view keeps it. */
export interface GridQuery { readonly filter: Predicate | null; readonly sort: readonly SortKey[]; readonly columns: readonly string[] }

export type DisplayedGridQuery = GridQuery & { readonly initialColumns: readonly string[]; readonly scope?: StudioScope }

/**
 * Whether the latest read withdrew this reader's view of the records: the workspace changed, or
 * the session or the cell refused the reader (401 or 403, from the scope probe or the records
 * read). It wins over any cached page: TanStack keeps the last page on a failed refetch, and that
 * page must not stay on screen once the reader may no longer see it.
 */
export function readWithdrawn(error: Error | null): boolean {
  return error instanceof StudioScopeError || (error instanceof CellUnavailable && (error.status === 401 || error.status === 403))
}

/**
 * The drawer's row: the opened record's id resolved against the latest page the cell served, so a
 * later read that withholds, changes or drops the record shows that, never the row as first
 * opened. A refused, withdrawn or workspace-changed read, or a page without the record, closes the drawer.
 */
export function visibleDrawerRow(openedId: string | null, rows: readonly GridRow[], error: Error | null, refused = false): GridRow | null {
  if (openedId === null || readWithdrawn(error) || refused) return null
  return rows.find(({ record }) => record.id === openedId) ?? null
}

/**
 * A type page's records, shown only once the page's workspace is decided (#1745, #1754): a live cell's
 * grid reads at the workspace its mode was read at, and a recorded workspace's grid reads only the
 * recording, even when the session's cell would serve records. While the mode is read the Records
 * section shows the grid's skeleton; after a failed mode read (including a refetch that failed after
 * an earlier answer) it shows Workspace unavailable with the mode's own Retry, never a grid.
 */
export function DecidedRecordsGrid({ concept, timeline = false }: {
  readonly concept: Concept
  readonly timeline?: boolean
}) {
  const workspace = useWorkspace()
  if (workspace.kind === "deciding" || workspace.kind === "unavailable") {
    return <Shell><WorkspaceUndecided workspace={workspace} bone="records-table" loadingLabel="Reading the records" /></Shell>
  }
  const key = workspace.kind === "live" ? studioScopeKey(workspace.scope) : "recorded"
  return <RecordsGrid key={`${concept.name}:${key}`} concept={concept} reader={workspace.reader} timeline={timeline} />
}

export function RecordsGrid({ concept, reader, initial, onQuery, timeline = false }: {
  readonly concept: Concept
  /**
   * The decided workspace's reader: a live workspace's (its query cache, facets, actions, reveals and
   * drawer reads are bound to its scope), or the recording's.
   */
  readonly reader: GridReader
  /** The type is a `core.TimeInterval`: the records can also be read as a timeline (lib/timeline.ts). */
  readonly timeline?: boolean
  /** The query to open with: a saved view's. */
  readonly initial?: Partial<GridQuery>
  /** Told the query the grid shows, whenever it changes. */
  readonly onQuery?: (query: DisplayedGridQuery) => void
}) {
  const name = concept.name
  const scope: StudioScope | undefined = reader.kind === "live" ? reader.scope : undefined
  const fields = useMemo(() => concept.fields.filter(({ id }) => !HIDDEN_FIELDS.has(id)), [concept])
  const markings = useMarkingCatalog()
  // The Record column shows the record's name: the markings of the name facts it is read from.
  const nameMarkings = useMemo(() => [...new Set(concept.fields.filter(({ id }) => HIDDEN_FIELDS.has(id)).flatMap(({ markings: marked }) => marked ?? []))], [concept])
  const [sort, setSort] = useState<readonly SortKey[]>(initial?.sort ?? [])
  const [filter, setFilter] = useState<Predicate | null>(initial?.filter ?? null)
  // An opened view's filter is the draft too, so the editor opens on it with nothing unapplied.
  const [draft, setDraft] = useState<readonly DraftClause[]>(() => fromPredicate(initial?.filter ?? null))
  const [limit, setLimit] = useState<number>(LIMITS.pageSize)
  const [cursors, setCursors] = useState<readonly (string | undefined)[]>([undefined])
  const [pageIndex, setPageIndex] = useState(0)
  const [initialColumns] = useState<readonly string[]>(() => {
    const valid = new Set(fields.map(({ id }) => id))
    const saved = [...new Set(initial?.columns)].filter((id) => valid.has(id))
    return saved.length > 0 ? saved : fields.slice(0, DEFAULT_COLUMNS).map(({ id }) => id)
  })
  const [shownOrder, setShownOrder] = useState(initialColumns)
  const shown = useMemo(() => new Set(shownOrder), [shownOrder])
  const fieldById = useMemo(() => new Map(fields.map((field) => [field.id, field])), [fields])
  // Preserve saved column order when configured; fall back to catalog order for default columns.
  const columns = useMemo(() => shownOrder.flatMap((id) => { const f = fieldById.get(id); return f ? [f] : [] }), [shownOrder, fieldById])
  const shownList = useMemo(() => columns.map(({ id }) => id), [columns])
  const reported = JSON.stringify({ filter, sort, shownList, orgId: scope?.orgId, environmentId: scope?.environmentId })
  useEffect(() => { onQuery?.({ filter, sort, columns: shownList, initialColumns, ...(scope === undefined ? {} : { scope }) }) }, [reported])
  const [active, setActive] = useState(0)
  const [openedId, setOpenedId] = useState<string | null>(null)
  const [notice, setNotice] = useState<string | null>(null)
  const [acting, setActing] = useState<OpenAction | null>(null)
  const [viewing, setViewing] = useState<{ readonly id: string; readonly since: number } | null>(null)
  const navigate = useNavigate()
  const actionScope = useMemo<ActionScopeBinding | undefined>(() => scope === undefined ? undefined : { scope }, [scope])
  // Only a live workspace's cell offers actions: a recorded grid reads none, and offers none (#1754).
  const catalog = useActions(name, undefined, reader.kind === "live", actionScope)
  const actions = catalog.data?.kind === "catalog" ? catalog.data.items : []
  const creates = actions.filter(writesWhole)
  // Off with the reason while the cell takes no actions; absent when it offers this reader none.
  const actionsOff = reader.kind === "recorded" ? RECORDED_ACTIONS : catalog.isError ? (catalog.error instanceof Error ? catalog.error.message : ACTIONS_UNAVAILABLE)
    : catalog.data?.kind === "unavailable" ? ACTIONS_UNAVAILABLE : catalog.data?.kind === "refused" ? catalog.data.message : undefined
  const [filterOpen, setFilterOpen] = useState(false)
  const client = useQueryClient()
  const hasLifecycle = concept.lifecycles.length > 0
  // The Timeline reads the window's records: the reader's filter and the window's clauses, in start order, a full page at a time.
  // A type with a declared lifecycle also offers the type-wide Lifecycle view (UI-LIFECYCLE-2).
  const [layout, setLayout] = useState<"table" | "timeline" | "lifecycle">(() => {
    if (!hasLifecycle || typeof location === "undefined") return "table"
    const requested = location.hash === "#lifecycle" || new URLSearchParams(location.search).get("view") === "lifecycle"
    return requested ? "lifecycle" : "table"
  })
  const [zoom, setZoom] = useState<Zoom>("week")
  const [anchor, setAnchor] = useState(() => new Date())
  const shownTimeline = timeline && layout === "timeline"
  const shownLifecycle = hasLifecycle && layout === "lifecycle"
  const hasViewModes = timeline || hasLifecycle
  const span = useMemo(() => windowOf(zoom, anchor), [zoom, anchor])
  const windowed = shownTimeline ? tryWindowQuery(span, filter) : null
  const filterOverflow = windowed?.kind === "too_many" ? windowed.overflow : 0
  const query: RecordsQuery = {
    sort: windowed?.kind === "ok" ? windowed.sort : sort,
    filter: windowed?.kind === "ok" ? windowed.filter : filter,
    limit: shownTimeline ? LIMITS.maxPageSize : limit,
    ...cursors[pageIndex] === undefined ? {} : { after: cursors[pageIndex] },
  }
  // The Timeline's nearest-record reads go through the same reader.
  const readRecordsPage = useMemo(() => readPage(reader), [reader])
  const read = reader.records(name, query)
  const key = read.queryKey
  const prefix = key.slice(0, -1)
  const result = useQuery({ ...read, placeholderData: keepPreviousData, retry: false, enabled: filterOverflow === 0 })
  const data = result.data

  const restart = () => { setCursors([undefined]); setPageIndex(0); setActive(0) }
  useEffect(() => {
    if (data?.kind !== "restart") return
    // An unscoped restart leaves saved-view pages in their own caches.
    void client.removeQueries({ queryKey: prefix, predicate: ({ queryKey }) => queryKey.length === key.length })
    restart()
    setNotice(DATA_CHANGED)
  }, [data, client, name, reader])

  // Controls are on only when the last page says the cell applied the query.
  // A workspace change or an authorization refusal hides the cached page (and with it the drawer).
  const withdrawn = result.isError && readWithdrawn(result.error)
  const page = !withdrawn && data?.kind === "page" ? data : undefined
  // While a refused or restarting read is shown, keep what the last page said.
  const lastServed = useRef(false)
  if (page !== undefined) lastServed.current = page.served !== null
  const canQuery = lastServed.current

  // "View record" after a commit: once the page has been read again, open that record's drawer; off this page, its full page.
  useEffect(() => {
    // Only a page read after the commit can hold the record as committed.
    if (viewing === null || result.isFetching || page === undefined || result.dataUpdatedAt < viewing.since) return
    const row = page.read.document.records.findIndex((record) => record.id === viewing.id)
    if (row >= 0) setOpenedId(viewing.id)
    else void navigate({ to: "/sources/$concept/$", params: { concept: name, _splat: viewing.id } })
    setViewing(null)
  }, [viewing, result.isFetching, result.dataUpdatedAt, page, name, navigate])

  const rows = useMemo<readonly GridRow[]>(() => page === undefined ? [] : page.read.document.records.map((record, index) => ({ record, at: `/records/${index}`, numbers: page.read.numbers })), [page])
  const drawerRow = visibleDrawerRow(openedId, rows, result.error, data?.kind === "refused")
  // A record the latest read no longer serves stays closed: it never reopens on a later page.
  useEffect(() => { if (openedId !== null && drawerRow === null) setOpenedId(null) }, [openedId, drawerRow])
  // Columns never squeeze below a readable width; past the card's width the grid scrolls sideways.
  // Wide enough for a money amount with its currency ("7,800.00 USD") and a lifecycle state.
  const template = `minmax(14rem,1.4fr) ${columns.map((field) => alignRight(field) ? "minmax(10.5rem,1fr)" : "minmax(11rem,1fr)").join(" ")}`
  const width = `calc(14rem + ${columns.map((field) => alignRight(field) ? "10.5rem" : "11rem").join(" + ")})`

  const scroller = useRef<HTMLDivElement>(null)
  const [edges, setEdges] = useState({ right: false })
  const measureEdges = () => {
    const el = scroller.current
    if (el === null) return
    const right = el.scrollLeft + el.clientWidth < el.scrollWidth - 1
    setEdges((current) => current.right === right ? current : { right })
  }
  useEffect(() => {
    measureEdges()
    const el = scroller.current
    if (el === null || typeof ResizeObserver === "undefined") return
    const observer = new ResizeObserver(measureEdges)
    observer.observe(el)
    return () => observer.disconnect()
  }, [rows.length, columns.length])
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
    else if ((event.key === "Enter" || event.key === " ") && rows[active]) { event.preventDefault(); setOpenedId(rows[active]?.record.id ?? null) }
  }

  const gridWait = useBoundedWait(result.isPending)

  if (data?.kind === "no-cell") return <Shell><EmptyState icon={Rows3} title="No data cell">{NO_DATA_CELL}</EmptyState></Shell>
  if (data?.kind === "none") return <Shell><EmptyState icon={Rows3}>Records for this object type are not served by this cell.</EmptyState></Shell>

  const refused = data?.kind === "refused" ? data : undefined
  const sortProblems = refused ? diagnosticsAt(refused.diagnostics, "/sort") : []
  const filterCount = filter?.all.length ?? 0
  const draftDirty = JSON.stringify(draft) !== JSON.stringify(applied)
  const off = canQuery ? undefined : SERVER_QUERY_UNAVAILABLE
  const pill = "h-8 rounded-lg px-2.5 text-body"

  const facets = page?.origin === "live" && <FacetsRail objectType={name} fields={fields} filter={filter} scope={scope} onFilter={(next) => { setDraft(fromPredicate(next)); changeQuery({ filter: next }) }} />
  return (
    <RevealScope.Provider value={scope}>
    <Shell>
      <div className={cn("grid gap-4", facets && "xl:grid-cols-[15rem_minmax(0,1fr)]")}>
      {facets && <aside className="hidden max-h-[70vh] overflow-y-auto pe-1 xl:block">{facets}</aside>}
      {/* content-start: beside a taller facets rail, the rows keep their own height (no band between them). */}
      <div className="grid min-w-0 content-start gap-3">
        {facets && <details className="rounded-lg border border-node-border px-3 py-2 xl:hidden"><summary className="cursor-pointer text-body">Narrow by</summary><div className="pt-3">{facets}</div></details>}
        {/* Problems sit above the toolbar, so the filter popover (which opens below it) never covers them. */}
        {refused && (
          <div role="alert" className="flex flex-wrap items-center gap-x-3 gap-y-1 rounded-(--radius-well) border border-(--panel-border) bg-card px-3 py-2 text-body text-foreground">
            <span aria-hidden className="size-[6px] shrink-0 rounded-full bg-dot-bad" /><span>{refused.message}</span>
            {sortProblems.map((problem) => <span key={`${problem.pointer}:${problem.code}`} className="text-meta">{problem.message}</span>)}
            <span className="flex-1" />
            <Button variant="outline" size="sm" className={pill} onClick={() => { setDraft([]); changeQuery({ sort: [], filter: null }) }}>Reset sort and filter</Button>
          </div>
        )}
        {result.isError && (isWorkspaceUnavailableError(result.error)
          ? <WorkspaceUnavailable error={result.error} isFetching={result.isFetching} onRetry={() => void result.refetch()} compact />
          : <ReadErrorState error={result.error} isFetching={result.isFetching} onRetry={() => void result.refetch()} />)}
        <div className="flex flex-wrap items-center gap-2">
          {hasViewModes && (
            <ToggleGroup type="single" variant="outline" size="sm" aria-label="Show as" value={layout}
              onValueChange={(next) => {
                if (next === "table" || (next === "timeline" && timeline && canQuery) || (next === "lifecycle" && hasLifecycle)) {
                  setLayout(next)
                  if (next !== "lifecycle") {
                    if (typeof location !== "undefined" && typeof history !== "undefined" && location.hash === "#lifecycle") {
                      history.replaceState(null, "", `${location.pathname}${location.search}`)
                    }
                    restart(); setNotice(null)
                  }
                }
              }}>
              <ToggleGroupItem value="table" className="px-3">Table</ToggleGroupItem>
              {timeline && <ToggleGroupItem value="timeline" className="px-3" disabled={!canQuery && layout !== "timeline"} title={!canQuery ? off : undefined}>Timeline</ToggleGroupItem>}
              {hasLifecycle && <ToggleGroupItem value="lifecycle" className="px-3">Lifecycle</ToggleGroupItem>}
            </ToggleGroup>
          )}
          <div role="toolbar" aria-label="Records" className="flex w-max flex-nowrap items-center gap-0.5">
            <Popover open={filterOpen} onOpenChange={(open) => { setFilterOpen(open); if (open && draft.length === 0 && filterCount === 0) { const first = filterable(fields)[0]; if (first) setDraft([blankClause(first)]) } }}>
              <PopoverTrigger asChild>
                <Button variant="ghost" size="sm" className={pill} disabled={!canQuery || filterable(fields).length === 0} title={off}>
                  <Filter /> Filter{filterCount > 0 && <span className="ms-0.5 rounded-full bg-(--well) px-1.5 text-meta tabular-nums text-foreground ring-1 ring-line ring-inset">{filterCount}</span>}
                  {/* A dot with no word beside it: `--ring`, which keeps 3:1 on the button in every skin (DESIGN.md 4.6). */}
                  {draftDirty && <span className="size-[6px] rounded-full bg-ring" aria-label="Unapplied changes" />}
                </Button>
              </PopoverTrigger>
              <PopoverContent align="start" className="w-[min(44rem,90vw)] rounded-node border-node-border shadow-float">
                <FilterEditor fields={fields} draft={draft} onDraft={setDraft} onApply={applyFilter} onClear={clearFilter} applied={applied} diagnostics={refused?.diagnostics ?? []} maxClauses={shownTimeline ? TIMELINE_MAX_USER_CLAUSES : LIMITS.clauses} />
              </PopoverContent>
            </Popover>
            {!shownLifecycle && <>
              <DropdownMenu>
                <DropdownMenuTrigger asChild><Button variant="ghost" size="sm" className={pill}><Columns3 /> Columns</Button></DropdownMenuTrigger>
                <DropdownMenuContent align="start" className="max-h-80 overflow-y-auto rounded-node border-node-border shadow-float">
                  <DropdownMenuLabel>Show columns</DropdownMenuLabel>
                  <DropdownMenuSeparator />
                  {fields.map((field) => (
                    <DropdownMenuCheckboxItem key={field.id} checked={shown.has(field.id)} disabled={shown.has(field.id) && shown.size === 1}
                      onSelect={(event) => event.preventDefault()}
                      onCheckedChange={(checked) => setShownOrder((current) => checked ? (current.includes(field.id) ? current : [...current, field.id]) : current.filter((id) => id !== field.id))}>
                      {field.id.replaceAll("_", " ")}
                    </DropdownMenuCheckboxItem>
                  ))}
                </DropdownMenuContent>
              </DropdownMenu>
              <span className="mx-0.5 h-5 w-px bg-node-border" aria-hidden />
              <label className="flex items-center gap-1.5 px-1.5 text-meta text-muted-foreground">
                Per page
                <select className="h-7 rounded-md border border-node-border bg-background px-1.5 text-body text-foreground disabled:cursor-not-allowed disabled:opacity-50" value={limit} disabled={!canQuery || shownTimeline} title={shownTimeline ? "The timeline reads up to 200 records per window." : off}
                  onChange={(event) => changeQuery({ limit: Number(event.target.value) })}>
                  {PAGE_SIZES.map((size) => <option key={size} value={size}>{size}</option>)}
                </select>
              </label>
              {sort.length > 0 && !shownTimeline && <Button variant="ghost" size="sm" className={pill} onClick={() => changeQuery({ sort: [] })}>Clear sort</Button>}
            </>}
          </div>
          <span className="flex-1" />
          {page?.origin === "recorded" && <Badge variant="outline" className="border-node-border" title={RECORDED}>Recorded</Badge>}
          <NewRecord title={concept.title} creates={creates} off={actionsOff} pending={catalog.isPending} onPick={(action) => setActing({ action })} />
          {!shownLifecycle && (
            <span className="text-meta tabular-nums text-muted-foreground" aria-live="polite">
              {page?.total ? totalText(page.total) : page ? `${rows.length} on this page` : ""}
            </span>
          )}
        </div>
        {off !== undefined && page !== undefined && <p className="text-meta text-muted-foreground">{off}</p>}
        <FilterChips filter={filter} fields={fields} onFilter={(next) => { setDraft(fromPredicate(next)); changeQuery({ filter: next }) }} />
        {notice !== null && <p role="status" className="flex items-center gap-1 text-meta text-muted-foreground"><RefreshCw className="size-3" aria-hidden />{notice}</p>}
        {page !== undefined && page.read.document.quarantineCount > 0 && (
          <p className="text-meta text-muted-foreground">{page.read.document.quarantineCount} held back on this page for review{page.origin === "live" ? "; the Quarantine tab names them" : ""}.</p>
        )}

        {shownTimeline && (() => {
          const loaded = page !== undefined && !result.isPlaceholderData && !result.isError && filterOverflow === 0
          return (
            <RecordsTimeline concept={concept} rows={loaded ? rows : []} window={span} zoom={zoom}
              loaded={loaded} pending={filterOverflow === 0 && (result.isPending || result.isPlaceholderData)} fetching={result.isFetching} filter={filter}
              filterOverflow={filterOverflow} scope={scope} read={readRecordsPage}
              onZoom={(next) => { setZoom(next); restart() }} onStep={(by) => { setAnchor((at) => stepAnchor(zoom, at, by)); restart() }}
              onToday={() => { setAnchor(new Date()); restart() }} onJump={(at) => { setAnchor(new Date(at)); restart() }}
              onOpenFilter={() => setFilterOpen(true)} onClearFilter={clearFilter} onSwitchToTable={() => { setLayout("table"); restart(); setNotice(null) }}
              onOpen={(row) => setOpenedId(row.record.id)} />
          )
        })()}
        {shownLifecycle && (
          <div className="grid gap-4" data-testid="records-lifecycle">
            {concept.lifecycles.map((lifecycle) => (
              <LifecycleView key={`${concept.name}:${lifecycle.property}:${scope ? `${scope.orgId}:${scope.environmentId}` : "auto"}`} concept={concept} lifecycle={lifecycle} filter={filter} scope={scope} />
            ))}
          </div>
        )}
        <div data-slot="records-table-frame" className={cn("relative overflow-hidden rounded-xl border border-(--table-border) bg-card shadow-(--panel-shadow)", (shownTimeline || shownLifecycle) && "hidden")}>
          <div ref={scroller} role="grid" tabIndex={0} aria-label={`${concept.title} records`} aria-rowcount={rows.length + 1} aria-colcount={columns.length + 1}
            aria-busy={result.isFetching} aria-activedescendant={rows.length > 0 ? `row-${active}` : undefined} onKeyDown={onKey} onScroll={measureEdges}
            className={cn("max-h-[60vh] overflow-auto text-body outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset", result.isFetching && "opacity-70")}>
            <div style={{ minWidth: width }}>
              <div role="row" aria-rowindex={1} className="sticky top-0 z-20 grid border-b border-node-border bg-muted" style={{ gridTemplateColumns: template }}>
                <div role="columnheader" className={cn(STICKY, "flex items-center gap-1.5 bg-muted px-3 py-2 ")}>Record<MarkingChips markings={nameMarkings} catalog={markings} className="inline-flex items-center gap-1 font-normal" /></div>
                {columns.map((field: Field) => {
                  const key = sort.find(({ property }) => property === field.id)
                  const can = canQuery && sortable(field)
                  const label = field.id.replaceAll("_", " ")
                  return (
                    <div key={field.id} role="columnheader" aria-sort={key ? (key.direction === "asc" ? "ascending" : "descending") : undefined}
                      className={cn("flex min-w-0 items-center px-3 py-2", alignRight(field) && "justify-end")}>
                      <button type="button" disabled={!can} title={can ? `${label}: sort (Shift adds a key)` : `${label}: ${off ?? "this kind of value can't be sorted"}`}
                        className="inline-flex min-w-0 items-center gap-1 whitespace-nowrap disabled:cursor-default" onClick={(event) => changeQuery({ sort: cycleSort(sort, field.id, event.shiftKey) })}>
                        <span className="truncate">{label}</span>{can && <SortMark direction={key?.direction} />}
                        {sort.length > 1 && key && <sup className="tabular-nums" aria-hidden>{sort.indexOf(key) + 1}</sup>}
                      </button>
                      <MarkingChips markings={field.markings} catalog={markings} className="ms-1 inline-flex shrink-0 items-center gap-1 font-normal" />
                    </div>
                  )
                })}
              </div>
              {result.isPending && (gridWait === "timed-out"
                ? <div className="sticky start-0 w-full max-w-[min(100%,64rem)] p-4"><LoadState status="pending" timeoutMs={0} slowAfterMs={0} onRetry={() => void result.refetch()}>{() => null}</LoadState></div>
                : (
                  <div className="grid gap-1 p-3" aria-label="Reading records" data-load-state={gridWait === "slow" ? "slow" : "loading"}>
                    {Array.from({ length: 6 }, (_, i) => <Skeleton key={i} className="h-6" />)}
                    {gridWait === "slow" && (
                      <div role="status" className="flex items-center gap-2 pt-2 text-meta text-muted-foreground">
                        <span>Taking longer than usual to reach the workspace…</span>
                        <Button type="button" variant="ghost" size="xs" onClick={() => void result.refetch()}>Retry</Button>
                      </div>
                    )}
                  </div>
                ))}
              {page !== undefined && rows.length === 0 && (
                <div className="sticky start-0 w-full max-w-[min(100%,64rem)] p-6">
                  {filterCount > 0
                    ? <EmptyState icon={Filter} title="No matches" next={<Button size="sm" variant="outline" onClick={clearFilter}>Clear filter</Button>}>No records match this filter.</EmptyState>
                    : <EmptyState icon={Rows3} title="No records yet" next={<Button asChild size="sm" variant="outline"><Link to="/sources">Connect a source or upload a CSV</Link></Button>}>No {concept.title.toLowerCase()} records are stored yet.</EmptyState>}
                </div>
              )}
              <div className="relative" style={{ height: virtual.getTotalSize() }}>
                {virtual.getVirtualItems().map((item) => {
                  const row = rows[item.index]
                  if (row === undefined) return null
                  const withheld = isWithheld(row)
                  const current = item.index === active
                  return (
                    <div key={`${row.record.objectType}:${row.record.id}`} id={`row-${item.index}`} role="row" aria-rowindex={item.index + 2} aria-selected={current}
                      data-withheld={withheld || undefined}
                      className={cn("group absolute inset-x-0 grid cursor-pointer items-center border-b border-node-border/70", current ? "bg-[color-mix(in_oklab,var(--accent)_75%,var(--card))]" : "bg-card hover:bg-[color-mix(in_oklab,var(--muted)_70%,var(--card))]")}
                      style={{ gridTemplateColumns: template, height: ROW, transform: `translateY(${item.start}px)` }}
                      onClick={() => { setActive(item.index); setOpenedId(row.record.id) }}>
                      <div role="gridcell" className={cn(STICKY, "truncate bg-inherit px-3 ")}><RecordName row={row} fallback={nameOf(row)} /></div>
                      {withheld
                        ? <div role="gridcell" aria-colspan={columns.length} className="min-w-0 px-3" style={{ gridColumn: "2 / -1" }}><WithheldMark title={WITHHELD_HINT}>{WITHHELD_ROW}</WithheldMark></div>
                        : columns.map((field) => <div key={field.id} role="gridcell" className={cn("group/cell min-w-0 truncate px-3", alignRight(field) && "text-end")}><TracedCell field={field} row={row} live={page?.origin === "live"} /></div>)}
                    </div>
                  )
                })}
              </div>
            </div>
          </div>
          {edges.right && <div aria-hidden className="pointer-events-none absolute inset-y-0 right-0 z-30 w-12 bg-gradient-to-l from-card to-transparent" />}
        </div>

        {!shownLifecycle && (
          <nav className="flex items-center justify-end gap-2 text-meta text-muted-foreground" aria-label="Pages">
            <Button variant="outline" size="sm" className={cn(pill, "border-node-border shadow-node")} disabled={pageIndex === 0} onClick={() => { setPageIndex(pageIndex - 1); setActive(0) }}>Previous</Button>
            <span className="tabular-nums">Page {pageIndex + 1}</span>
            <Button variant="outline" size="sm" className={cn(pill, "border-node-border shadow-node")} disabled={page?.next === undefined || result.isFetching}
              onClick={() => { const next = page?.next; if (next === undefined) return; setCursors([...cursors.slice(0, pageIndex + 1), next]); setPageIndex(pageIndex + 1); setActive(0) }}>Next</Button>
          </nav>
        )}
      </div>
      </div>
      <RecordDrawer concept={concept} fields={fields} row={drawerRow} reader={reader} actions={actions} actionsOff={actionsOff} onClose={() => setOpenedId(null)}
        onAction={(action, id) => { setOpenedId(null); setActing({ action, prefill: { [action.objectIdField]: id } }) }} />
      <ActionSheet open={acting} onClose={() => setActing(null)} onView={(id) => setViewing({ id, since: Date.now() })} scopeBinding={actionScope} />
    </Shell>
    </RevealScope.Provider>
  )
}

/** One record, from the page that listed it; its links come from the object read when the cell serves it. */
/** "New …": the upsert actions the cell offers this reader for the type. One opens directly; several are a menu. */
function NewRecord({ title, creates, off, pending, onPick }: {
  readonly title: string; readonly creates: readonly ActionSummary[]; readonly off: string | undefined; readonly pending: boolean; readonly onPick: (action: ActionSummary) => void
}) {
  const label = `New ${title.toLowerCase()}`
  if (off !== undefined) return <Button size="sm" className="h-8 rounded-lg" disabled title={off}><Plus /> {label}</Button>
  if (pending || creates.length === 0) return null
  const [only] = creates
  if (creates.length === 1 && only) return <Button size="sm" className="h-8 rounded-lg shadow-node" onClick={() => onPick(only)}><Plus /> {label}</Button>
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild><Button size="sm" className="h-8 rounded-lg shadow-node"><Plus /> {label}<ChevronDown /></Button></DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="rounded-node border-node-border shadow-float">
        {creates.map((action) => <DropdownMenuItem key={action.actionId} onSelect={() => onPick(action)}>{action.title}</DropdownMenuItem>)}
      </DropdownMenuContent>
    </DropdownMenu>
  )
}

function RecordDrawer({ concept, fields, row, reader, actions, actionsOff, onClose, onAction }: {
  readonly concept: Concept; readonly fields: readonly Field[]; readonly row: GridRow | null; readonly reader: GridReader; readonly onClose: () => void
  readonly actions: readonly ActionSummary[]; readonly actionsOff: string | undefined; readonly onAction: (action: ActionSummary, id: string) => void
}) {
  const record = row?.record
  const markings = useMarkingCatalog()
  // The grid exists only in a decided workspace, so its rows are that workspace's: live rows from its
  // cell, recorded rows from the recording, never a live row without a workspace (#1754).
  const live = reader.kind === "live"
  const scope = live ? reader.scope : undefined
  const scopeBinding = useMemo<ActionScopeBinding | undefined>(() => scope === undefined ? undefined : { scope }, [scope])
  // What can act on this record now: the cell narrows the type's catalog to the record's state.
  const narrowed = useActions(record?.objectType ?? "", record?.id, record !== undefined && live, scopeBinding)
  const offered = recordCatalog(live, narrowed, actions)
  // The row's record, read through the grid's reader and cached under the same key as its full page (#1740).
  const object = useQuery({
    ...reader.object(record?.objectType ?? concept.name, record?.id ?? ""),
    enabled: record !== undefined && row !== null && !isWithheld(row),
  })
  const links = object.data?.links.document.items ?? []
  return (
    <Sheet open={row !== null} onOpenChange={(open) => { if (!open) onClose() }}>
      <SheetContent className="overflow-y-auto">
        {row !== null && record !== undefined && <>
          <SheetHeader>
            <SheetTitle className="[overflow-wrap:break-word]"><PathText value={nameOf(row)} /></SheetTitle>
            <SheetDescription className="flex items-center gap-2"><span>{concept.title}</span><CopyId value={record.id} /></SheetDescription>
          </SheetHeader>
          <SheetBody>
            <SheetSection label="What Norn found">
              {isWithheld(row)
                ? <p className="px-2.5 py-2.5 text-body"><WithheldMark title={WITHHELD_HINT}>{WITHHELD_ROW}</WithheldMark></p>
                : <dl className="grid grid-cols-[minmax(8rem,auto)_1fr] text-body">
                  {fields.map((field) => <div key={field.id} className="contents"><dt className="flex min-h-10 flex-wrap items-center gap-1 px-2.5 py-2.5 text-muted-foreground">{field.id.replaceAll("_", " ")}<MarkingChips markings={field.markings} catalog={markings} /></dt><dd className="flex min-h-10 min-w-0 items-center px-2.5 py-2.5 break-words"><TracedCell field={field} row={row} live={live} lineage={object.data?.source?.document ?? null} variant="inline" /></dd></div>)}
                  <dt className="flex min-h-10 items-center px-2.5 py-2.5 text-muted-foreground">version</dt><dd className="flex min-h-10 items-center px-2.5 py-2.5 tabular-nums">{record.version}</dd>
                </dl>}
            </SheetSection>
            {!isWithheld(row) && <SheetSection label="Linked records">
              <div className="px-2.5 py-2">
                {object.isPending ? <Skeleton className="h-5" />
                  : object.data === null || object.data === undefined
                    ? <p className="text-meta text-muted-foreground">{live ? "This cell doesn't serve a record's links yet." : "No recorded links for this record."}</p>
                    : links.length === 0 ? <p className="text-meta text-muted-foreground">Nothing links to or from this record.</p>
                      : <ul className="grid gap-1 text-body">{links.map((link) => {
                        const outgoing = link.source === record.id
                        const [type, id] = outgoing ? [link.targetType, link.target] : [link.sourceType, link.source]
                        return <li key={`${link.link}:${link.source}:${link.target}`}><span className="text-muted-foreground">{link.link.replaceAll("_", " ")} <span className="inline-block rtl:rotate-180">{outgoing ? "→" : "←"}</span> </span>
                          <Link to="/sources/$concept/$" params={{ concept: type, _splat: id }} className="underline-offset-4 hover:underline"><bdi>{id}</bdi></Link></li>
                      })}</ul>}
              </div>
            </SheetSection>}
            <SheetSection label="Actions" aria-label="Actions">
              <div className="px-2.5 py-2">
                {actionsOff !== undefined
                  ? <p className="text-meta text-muted-foreground">{actionsOff}</p>
                  : offered.kind === "reading" ? <Skeleton className="h-8" aria-label="Reading the actions on this record" />
                  : offered.kind === "unreadable" ? <p className="text-meta text-muted-foreground">The cell's actions on this record could not be read.</p>
                  : offeredFor(offered.items, record).length === 0
                    ? <p className="text-meta text-muted-foreground">The cell offers you no actions on {concept.title.toLowerCase()} records.</p>
                    : <div className="flex flex-wrap gap-2">{offeredFor(offered.items, record).map((action) => (
                      <Button key={action.actionId} variant="outline" size="sm" onClick={() => onAction(action, record.id)}>
                        <Play /> {writesWhole(action) ? `${action.title} (edit)` : action.title}
                      </Button>
                    ))}</div>}
              </div>
            </SheetSection>
          </SheetBody>
          <SheetFooter className="justify-end">
            <Button asChild size="sm">
              <Link to="/sources/$concept/$" params={{ concept: record.objectType, _splat: record.id }}><ExternalLink /> Open full record</Link>
            </Button>
          </SheetFooter>
        </>}
      </SheetContent>
    </Sheet>
  )
}
