import { useQueries, useQuery } from "@tanstack/react-query"
import { CalendarRange, ChevronLeft, ChevronRight, Filter } from "lucide-react"
import { useEffect, useMemo, useRef, useState, type KeyboardEvent } from "react"
import { EmptyState } from "@/components/panel"
import { Button } from "@/components/ui/button"
import { Skeleton } from "@/components/ui/skeleton"
import { ToggleGroup, ToggleGroupItem } from "@/components/ui/toggle-group"
import { useLocale } from "@/lib/locale"
import { cn } from "@/lib/utils"
import type { Concept } from "../lib/catalog.ts"
import { cellCall } from "../lib/cell-read.ts"
import { recordName } from "../lib/objects.ts"
import { LIMITS, type Predicate, type RecordsQuery } from "../lib/records-query.ts"
import type { RecordsRead } from "../lib/records-source.ts"
import type { LinkPage } from "../lib/transport.ts"
import {
  ENDS_AT, NEAREST_MAX_USER_CLAUSES, STARTS_AT, TIMELINE_MAX_USER_CLAUSES, ZOOMS,
  clampFocus, clockText, formatLocale, geometry, groupLanes, instantOf, laneLinks, nearestJumpTarget, nearestQuery, resolveBarFocus, ticksOf, timeText, windowTitle, zoneOf,
  type LaneTarget, type Span, type Zoom,
} from "../lib/timeline.ts"
import { isWithheld, type GridRow } from "./cells.tsx"

const BAR = 26
const GAP = 4
const ZOOM_WORDS: Readonly<Record<Zoom, string>> = { day: "Day", week: "Week", month: "Month" }

async function targetOf(objectType: string, id: string, link: string): Promise<LaneTarget> {
  const params = new URLSearchParams({ direction: "out", link, limit: "1" })
  const read = await cellCall<LinkPage>(`/v1/objects/${encodeURIComponent(objectType)}/${encodeURIComponent(id)}/links?${params}`)
  if (read.kind !== "ok") return { kind: "unavailable" }
  const first = read.value.items[0]?.target
  if (first !== undefined) return { kind: "target", id: first }
  if (read.value.hasMore) return { kind: "unavailable" }
  return { kind: "none" }
}

/**
 * The Timeline of a `core.TimeInterval` type: the window's records as bars from `starts_at` to
 * `ends_at`, on the reader's clock, each bar's own times in the record's `timezone` when it names
 * one. The records are the page the cell returned for the window; lanes follow one declared to-one
 * link, read from the cell per record. Opening a bar opens the record's drawer (and its offered
 * actions), exactly as a row does in the table.
 */
export function RecordsTimeline({ concept, rows, window, zoom, pending, fetching, filter, filterOverflow = 0, read, onZoom, onStep, onToday, onJump, onOpenFilter, onClearFilter, onSwitchToTable, onOpen }: {
  readonly concept: Concept
  readonly rows: readonly GridRow[]
  readonly window: Span
  readonly zoom: Zoom
  readonly pending: boolean
  readonly fetching: boolean
  /** The reader's own filter, which the empty state's nearest-record reads keep. */
  readonly filter: Predicate | null
  /** How many clauses the active user filter exceeds the Timeline's 6-clause budget by. */
  readonly filterOverflow?: number
  readonly read: (objectType: string, query: RecordsQuery) => Promise<RecordsRead>
  readonly onZoom: (zoom: Zoom) => void
  readonly onStep: (by: number) => void
  readonly onToday: () => void
  /** Move the window to hold this instant. */
  readonly onJump: (at: number) => void
  readonly onOpenFilter?: () => void
  readonly onClearFilter?: () => void
  readonly onSwitchToTable?: () => void
  readonly onOpen: (row: GridRow) => void
}) {
  const { locale } = useLocale()
  const intlLocale = formatLocale(locale)
  const links = laneLinks(concept)
  const [lane, setLane] = useState<string | null>(null)
  const ticks = useMemo(() => ticksOf(zoom, window, intlLocale), [zoom, window, intlLocale])
  const spans = useMemo(() => pending || filterOverflow > 0 ? [] : rows.flatMap((row) => {
    if (isWithheld(row)) return []
    const start = instantOf(row.record.values[STARTS_AT]), end = instantOf(row.record.values[ENDS_AT])
    return start === null || end === null || end <= start ? [] : [{ item: row, start, end }]
  }), [rows, pending, filterOverflow])
  const unplaced = pending || filterOverflow > 0 ? 0 : rows.length - spans.length

  const targets = useQueries({
    queries: lane === null || pending || filterOverflow > 0 ? [] : spans.map(({ item }) => ({
      queryKey: ["timeline-lane", item.record.objectType, item.record.id, lane],
      queryFn: () => targetOf(item.record.objectType, item.record.id, lane),
      staleTime: 30_000, retry: false,
    })),
  })
  const laneLink = links.find(({ id }) => id === lane)
  const resolvedTargets: readonly LaneTarget[] = useMemo(() => targets.map((query): LaneTarget => {
    if (query.isError) return { kind: "unavailable" }
    if (query.data === undefined) return { kind: "pending" }
    return query.data
  }), [targets])
  const lanes = useMemo(() => groupLanes(spans, resolvedTargets, { lane, conceptTitle: concept.title, linkName: laneLink?.id }), [spans, resolvedTargets, lane, laneLink?.id, concept.title])

  // Roving focus over the bars: one bar is in the tab order; arrows move between bars and lanes.
  const order = useMemo(() => lanes.flatMap((l, laneIndex) => [...l.placed].sort((a, b) => a.start - b.start).map((bar) => ({ laneIndex, bar }))), [lanes])
  const [focused, setFocused] = useState(0)
  const barsGroupRef = useRef<HTMLDivElement>(null)
  const focusInBars = useRef(false)
  const focusedId = useRef<string | null>(null)
  const barRefs = useRef(new Map<string, HTMLButtonElement>())
  const activeIndex = clampFocus(focused, order.length)

  useEffect(() => {
    const activeEl = typeof document === "undefined" ? null : document.activeElement
    const bodyEl = typeof document === "undefined" ? null : document.body
    const resolution = resolveBarFocus({
      orderIds: order.map((entry) => entry.bar.item.record.id),
      focusedIndex: focused,
      focusedId: focusedId.current,
      wasInside: focusInBars.current,
      activeElement: activeEl,
      bodyElement: bodyEl,
      activeInsideGroup: activeEl !== null && barsGroupRef.current?.contains(activeEl) === true,
      buttonFor: (id) => barRefs.current.get(id),
    })
    if (resolution.index !== focused) setFocused(resolution.index)
    if (resolution.id === null) {
      focusedId.current = null
      focusInBars.current = false
    } else if (resolution.focusTarget !== null) {
      focusedId.current = resolution.id
      resolution.focusTarget.focus()
    }
  }, [order, focused])

  const focusAt = (index: number) => {
    const clamped = clampFocus(index, order.length)
    const next = order[clamped]
    if (next === undefined) return
    setFocused(clamped)
    focusedId.current = next.bar.item.record.id
    focusInBars.current = true
    barRefs.current.get(next.bar.item.record.id)?.focus()
  }
  const onKey = (event: KeyboardEvent<HTMLDivElement>) => {
    const current = order[activeIndex]
    if (current === undefined) return
    const inLane = (laneIndex: number) => order.map((entry, index) => ({ entry, index })).filter(({ entry }) => entry.laneIndex === laneIndex)
    if (event.key === "ArrowRight" || event.key === "ArrowLeft") { event.preventDefault(); focusAt(activeIndex + (event.key === "ArrowRight" ? 1 : -1)) }
    else if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault()
      const target = inLane(current.laneIndex + (event.key === "ArrowDown" ? 1 : -1))
      // The bar in the next lane nearest in time to this one.
      const nearest = target.sort((a, b) => Math.abs(a.entry.bar.start - current.bar.start) - Math.abs(b.entry.bar.start - current.bar.start))[0]
      if (nearest) focusAt(nearest.index)
    } else if (event.key === "Home") { event.preventDefault(); focusAt(0) }
    else if (event.key === "End") { event.preventDefault(); focusAt(order.length - 1) }
  }

  const empty = !pending && filterOverflow === 0 && rows.length === 0
  const nearestAllowed = empty && (filter?.all.length ?? 0) <= NEAREST_MAX_USER_CLAUSES
  const after = useQuery({ queryKey: ["timeline-nearest", concept.name, "after", window.end, JSON.stringify(filter)], enabled: nearestAllowed, retry: false, queryFn: () => read(concept.name, { ...nearestQuery(window, filter, "after"), limit: 1 }) })
  const before = useQuery({ queryKey: ["timeline-nearest", concept.name, "before", window.start, JSON.stringify(filter)], enabled: nearestAllowed, retry: false, queryFn: () => read(concept.name, { ...nearestQuery(window, filter, "before"), limit: 1 }) })
  const nearestOf = (result: RecordsRead | undefined, direction: "after" | "before") => {
    const record = result?.kind === "page" ? result.read.document.records[0] : undefined
    return nearestJumpTarget(record?.values, direction)
  }
  const next = nearestOf(after.data, "after"), previous = nearestOf(before.data, "before")
  const now = Date.now()
  const zoneNote = Intl.DateTimeFormat().resolvedOptions().timeZone

  return (
    <section aria-label={`${concept.title} timeline`} className="grid gap-3">
      <div className="flex flex-wrap items-center gap-2">
        <div className="flex items-center gap-0.5 rounded-xl border border-node-border bg-card/95 p-1 shadow-float">
          <Button variant="ghost" size="icon" className="size-8" aria-label={`Previous ${zoom}`} onClick={() => onStep(-1)}><ChevronLeft /></Button>
          <Button variant="ghost" size="sm" className="h-8 rounded-lg px-2.5" onClick={onToday}>Today</Button>
          <Button variant="ghost" size="icon" className="size-8" aria-label={`Next ${zoom}`} onClick={() => onStep(1)}><ChevronRight /></Button>
        </div>
        <h3 className="text-sm font-medium" aria-live="polite">{windowTitle(zoom, window, intlLocale)}</h3>
        <span className="flex-1" />
        {links.length > 0 && (
          <label className="flex items-center gap-1.5 text-meta text-muted-foreground">
            Lanes
            <select className="h-8 rounded-lg border border-node-border bg-background px-2 text-sm text-foreground" value={lane ?? ""} onChange={(event) => setLane(event.target.value === "" ? null : event.target.value)}>
              <option value="">None</option>
              {links.map((link) => <option key={link.id} value={link.id}>By {link.id.replaceAll("_", " ")}</option>)}
            </select>
          </label>
        )}
        <ToggleGroup type="single" variant="outline" size="sm" aria-label="Zoom" value={zoom} onValueChange={(value) => { if (ZOOMS.includes(value as Zoom)) onZoom(value as Zoom) }}>
          {ZOOMS.map((z) => <ToggleGroupItem key={z} value={z} className="px-3">{ZOOM_WORDS[z]}</ToggleGroupItem>)}
        </ToggleGroup>
      </div>
      <p className="text-meta text-muted-foreground">Times in {zoneNote}; a bar's own times read in the record's time zone when it names one. The cell chose which records fall in this window.</p>
      {unplaced > 0 && <p role="status" className="text-meta text-muted-foreground">{unplaced} {unplaced === 1 ? "record" : "records"} on this page can't be placed (withheld, or without a readable start and end); Table lists {unplaced === 1 ? "it" : "them"}.</p>}

      <div className={cn("overflow-x-auto rounded-xl border border-node-border bg-card", fetching && !pending && "opacity-70")} aria-busy={pending || fetching}>
        <div className="min-w-[48rem]">
          <div className="grid grid-cols-[12rem_minmax(0,1fr)] border-b border-node-border bg-muted text-meta">
            <div className="px-3 py-2 font-medium">{laneLink === undefined ? "" : laneLink.id.replaceAll("_", " ")}</div>
            <div className="relative h-8" aria-hidden>
              {ticks.map((tick, index) => {
                const left = ((tick.at - window.start) / (window.end - window.start)) * 100
                const shown = zoom !== "day" || index % 2 === 0
                return shown && <span key={tick.at} className="absolute top-2 -translate-x-1/2 whitespace-nowrap text-muted-foreground tabular-nums first:translate-x-0" style={{ left: `${left}%` }}>{tick.label}</span>
              })}
            </div>
          </div>
          {filterOverflow > 0 && (
            <div role="alert" className="p-6">
              <EmptyState icon={Filter} title="Too many filter conditions for Timeline">
                Timeline reserves 2 of the cell's {LIMITS.clauses} filter conditions for the time window, leaving at most {TIMELINE_MAX_USER_CLAUSES}. Remove {filterOverflow} {filterOverflow === 1 ? "condition" : "conditions"} or switch back to Table.
                <span className="mt-3 flex flex-wrap justify-center gap-2">
                  {onOpenFilter && <Button variant="outline" size="sm" onClick={onOpenFilter}>Edit filter</Button>}
                  {onClearFilter && <Button variant="outline" size="sm" onClick={onClearFilter}>Clear filter</Button>}
                  {onSwitchToTable && <Button variant="outline" size="sm" onClick={onSwitchToTable}>Switch to Table</Button>}
                </span>
              </EmptyState>
            </div>
          )}
          {filterOverflow === 0 && pending && <div className="grid gap-1 p-3" aria-label="Reading records">{Array.from({ length: 4 }, (_, i) => <Skeleton key={i} className="h-6" />)}</div>}
          {empty && (
            <div className="p-6">
              <EmptyState icon={CalendarRange} title={`Nothing in this ${zoom}`}>
                No {concept.title.toLowerCase()} records fall in {windowTitle(zoom, window, intlLocale)}{filter !== null && filter.all.length > 0 ? " with this filter" : ""}.
                <span className="mt-3 flex flex-wrap justify-center gap-2">
                  {previous !== null && <Button variant="outline" size="sm" onClick={() => onJump(previous.jumpAt)}><ChevronLeft /> Latest before ({timeText(previous.at, null, intlLocale)})</Button>}
                  {next !== null && <Button variant="outline" size="sm" onClick={() => onJump(next.jumpAt)}>Next after ({timeText(next.at, null, intlLocale)}) <ChevronRight /></Button>}
                </span>
              </EmptyState>
            </div>
          )}
          {filterOverflow === 0 && !pending && lanes.length > 0 && (
            <div ref={barsGroupRef} role="group" aria-label="Bars" onKeyDown={onKey}
              onFocus={() => { focusInBars.current = true }}
              onBlur={(event) => { if (!event.currentTarget.contains(event.relatedTarget as Node | null)) focusInBars.current = false }}>
              {lanes.map((l) => (
                <div key={l.key} role="group" aria-label={l.label} className="grid grid-cols-[12rem_minmax(0,1fr)] border-b border-node-border/70 last:border-b-0">
                  <div className="flex min-w-0 flex-col justify-center gap-0.5 px-3 py-2 text-sm">
                    <span className="truncate font-medium" title={l.label}>{l.label}</span>
                    {l.overlapping > 0 && <span className="text-meta text-muted-foreground">{l.overlapping} overlapping</span>}
                  </div>
                  <div className="relative" style={{ height: l.rows * (BAR + GAP) + GAP * 2 }}>
                    {ticks.map((tick) => <span key={tick.at} aria-hidden className={cn("absolute inset-y-0 w-px", tick.day ? "bg-node-border" : "bg-node-border/40")} style={{ left: `${((tick.at - window.start) / (window.end - window.start)) * 100}%` }} />)}
                    {now >= window.start && now < window.end && <span aria-hidden className="absolute inset-y-0 w-0.5 bg-primary/60" style={{ left: `${((now - window.start) / (window.end - window.start)) * 100}%` }} />}
                    {l.placed.map((bar) => {
                      const row = bar.item
                      const zone = zoneOf(row.record.values)
                      const title = recordName(row.record)
                      const at = geometry(bar, window)
                      const index = order.findIndex((entry) => entry.bar === bar)
                      const words = `${title}, ${timeText(bar.start, zone, intlLocale)} to ${timeText(bar.end, zone, intlLocale)}${zone === null ? "" : ` (${zone})`}${bar.overlaps ? ", overlaps another in this lane" : ""}`
                      return (
                        <button key={row.record.id} type="button" ref={(el) => { if (el) barRefs.current.set(row.record.id, el); else barRefs.current.delete(row.record.id) }}
                          tabIndex={index === activeIndex ? 0 : -1} aria-label={words} title={words} data-overlaps={bar.overlaps || undefined}
                          onFocus={() => { setFocused(index); focusedId.current = row.record.id; focusInBars.current = true }} onClick={() => onOpen(row)}
                          className={cn("absolute flex min-w-1.5 items-center gap-1 overflow-hidden rounded-md border px-1.5 text-left text-xs shadow-node outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-1",
                            "border-primary/40 bg-[color-mix(in_oklab,var(--primary)_14%,var(--card))] text-foreground hover:bg-[color-mix(in_oklab,var(--primary)_22%,var(--card))]",
                            bar.overlaps && "border-dashed border-[var(--tint-wait)]",
                            at.clippedStart && "rounded-l-none border-l-0", at.clippedEnd && "rounded-r-none border-r-0")}
                          style={{ left: `${at.left}%`, width: `${at.width}%`, top: GAP + bar.row * (BAR + GAP), height: BAR }}>
                          {at.clippedStart && <ChevronLeft className="size-3 shrink-0 text-muted-foreground" aria-hidden />}
                          <span className="truncate font-medium">{title}</span>
                          <span className="truncate text-muted-foreground tabular-nums">{clockText(bar.start, zone, intlLocale)}–{clockText(bar.end, zone, intlLocale)}{zone !== null && ` ${zone}`}</span>
                          {at.clippedEnd && <ChevronRight className="ms-auto size-3 shrink-0 text-muted-foreground" aria-hidden />}
                        </button>
                      )
                    })}
                  </div>
                </div>
              ))}
            </div>
          )}
        </div>
      </div>
      {filterOverflow === 0 && !pending && lanes.length > 0 && <p className="text-meta text-muted-foreground">Arrow keys move between bars; Enter opens the record.</p>}
    </section>
  )
}
