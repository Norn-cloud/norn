import { useQueries, useQuery } from "@tanstack/react-query"
import { CalendarRange, ChevronLeft, ChevronRight } from "lucide-react"
import { useMemo, useRef, useState, type KeyboardEvent } from "react"
import { EmptyState } from "@/components/panel"
import { Button } from "@/components/ui/button"
import { Skeleton } from "@/components/ui/skeleton"
import { ToggleGroup, ToggleGroupItem } from "@/components/ui/toggle-group"
import { cn } from "@/lib/utils"
import type { Concept } from "../lib/catalog.ts"
import { cellCall } from "../lib/cell-read.ts"
import { recordName } from "../lib/objects.ts"
import type { Predicate, RecordsQuery } from "../lib/records-query.ts"
import type { RecordsRead } from "../lib/records-source.ts"
import type { LinkPage } from "../lib/transport.ts"
import { ENDS_AT, STARTS_AT, ZOOMS, geometry, instantOf, laneLinks, nearestQuery, stack, ticksOf, timeText, windowTitle, zoneOf, type Span, type Zoom } from "../lib/timeline.ts"
import { isWithheld, type GridRow } from "./cells.tsx"

const BAR = 26
const GAP = 4
const ZOOM_WORDS: Readonly<Record<Zoom, string>> = { day: "Day", week: "Week", month: "Month" }

/** A lane: the records that link to one target through the chosen link (or none), stacked so no two overlap. */
interface Lane { readonly key: string; readonly label: string; readonly rows: number; readonly placed: ReturnType<typeof stack<GridRow>>["placed"]; readonly overlapping: number }

/** One record's target through a link, from the cell's links read: its id, `null` when it links to none, or unreadable. */
type Target = { readonly id: string | null } | { readonly unreadable: true }

async function targetOf(objectType: string, id: string, link: string): Promise<Target> {
  const params = new URLSearchParams({ direction: "out", link, limit: "1" })
  const read = await cellCall<LinkPage>(`/v1/objects/${encodeURIComponent(objectType)}/${encodeURIComponent(id)}/links?${params}`)
  if (read.kind !== "ok") return { unreadable: true }
  return { id: read.value.items[0]?.target ?? null }
}

/**
 * The Timeline of a `core.TimeInterval` type: the window's records as bars from `starts_at` to
 * `ends_at`, on the reader's clock, each bar's own times in the record's `timezone` when it names
 * one. The records are the page the cell returned for the window; lanes follow one declared to-one
 * link, read from the cell per record. Opening a bar opens the record's drawer (and its offered
 * actions), exactly as a row does in the table.
 */
export function RecordsTimeline({ concept, rows, window, zoom, pending, fetching, filter, read, onZoom, onStep, onToday, onJump, onOpen }: {
  readonly concept: Concept
  readonly rows: readonly GridRow[]
  readonly window: Span
  readonly zoom: Zoom
  readonly pending: boolean
  readonly fetching: boolean
  /** The reader's own filter, which the empty state's nearest-record reads keep. */
  readonly filter: Predicate | null
  readonly read: (objectType: string, query: RecordsQuery) => Promise<RecordsRead>
  readonly onZoom: (zoom: Zoom) => void
  readonly onStep: (by: number) => void
  readonly onToday: () => void
  /** Move the window to hold this instant. */
  readonly onJump: (at: number) => void
  readonly onOpen: (row: GridRow) => void
}) {
  const links = laneLinks(concept)
  const [lane, setLane] = useState<string | null>(null)
  const ticks = useMemo(() => ticksOf(zoom, window), [zoom, window])
  const spans = useMemo(() => rows.flatMap((row) => {
    if (isWithheld(row)) return []
    const start = instantOf(row.record.values[STARTS_AT]), end = instantOf(row.record.values[ENDS_AT])
    return start === null || end === null || end <= start ? [] : [{ item: row, start, end }]
  }), [rows])
  const unplaced = rows.length - spans.length

  const targets = useQueries({
    queries: lane === null ? [] : spans.map(({ item }) => ({
      queryKey: ["timeline-lane", item.record.objectType, item.record.id, lane],
      queryFn: () => targetOf(item.record.objectType, item.record.id, lane),
      staleTime: 30_000, retry: false,
    })),
  })
  const laneLink = links.find(({ id }) => id === lane)
  const lanes = useMemo((): readonly Lane[] => {
    const groups = new Map<string, { label: string; spans: typeof spans }>()
    spans.forEach((span, index) => {
      let key = "", label = concept.title
      if (lane !== null) {
        const target = targets[index]?.data
        if (target === undefined) { key = "\u0000reading"; label = "Reading links…" }
        else if ("unreadable" in target) { key = "\u0000unreadable"; label = "Link not readable" }
        else if (target.id === null) { key = "\u0000none"; label = `No ${(laneLink?.id ?? lane).replaceAll("_", " ")}` }
        else { key = target.id; label = target.id }
      }
      const group = groups.get(key) ?? { label, spans: [] }
      groups.set(key, { label, spans: [...group.spans, span] })
    })
    return [...groups].sort(([a], [b]) => (a.startsWith("\u0000") ? 1 : 0) - (b.startsWith("\u0000") ? 1 : 0) || a.localeCompare(b)).map(([key, group]) => {
      const { placed, rows: height } = stack(group.spans)
      return { key, label: group.label, rows: Math.max(1, height), placed, overlapping: placed.filter(({ overlaps }) => overlaps).length }
    })
  }, [spans, targets, lane, laneLink, concept.title])

  // Roving focus over the bars: one bar is in the tab order; arrows move between bars and lanes.
  const order = useMemo(() => lanes.flatMap((l, laneIndex) => [...l.placed].sort((a, b) => a.start - b.start).map((bar) => ({ laneIndex, bar }))), [lanes])
  const [focused, setFocused] = useState(0)
  const barRefs = useRef(new Map<string, HTMLButtonElement>())
  const focusAt = (index: number) => {
    const next = order[Math.max(0, Math.min(order.length - 1, index))]
    if (next === undefined) return
    setFocused(order.indexOf(next))
    barRefs.current.get(next.bar.item.record.id)?.focus()
  }
  const onKey = (event: KeyboardEvent<HTMLDivElement>) => {
    const current = order[focused]
    if (current === undefined) return
    const inLane = (laneIndex: number) => order.map((entry, index) => ({ entry, index })).filter(({ entry }) => entry.laneIndex === laneIndex)
    if (event.key === "ArrowRight" || event.key === "ArrowLeft") { event.preventDefault(); focusAt(focused + (event.key === "ArrowRight" ? 1 : -1)) }
    else if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault()
      const target = inLane(current.laneIndex + (event.key === "ArrowDown" ? 1 : -1))
      // The bar in the next lane nearest in time to this one.
      const nearest = target.sort((a, b) => Math.abs(a.entry.bar.start - current.bar.start) - Math.abs(b.entry.bar.start - current.bar.start))[0]
      if (nearest) focusAt(nearest.index)
    } else if (event.key === "Home") { event.preventDefault(); focusAt(0) }
    else if (event.key === "End") { event.preventDefault(); focusAt(order.length - 1) }
  }

  const empty = !pending && rows.length === 0
  const after = useQuery({ queryKey: ["timeline-nearest", concept.name, "after", window.end, JSON.stringify(filter)], enabled: empty, retry: false, queryFn: () => read(concept.name, { ...nearestQuery(window, filter, "after"), limit: 1 }) })
  const before = useQuery({ queryKey: ["timeline-nearest", concept.name, "before", window.start, JSON.stringify(filter)], enabled: empty, retry: false, queryFn: () => read(concept.name, { ...nearestQuery(window, filter, "before"), limit: 1 }) })
  const nearestAt = (result: RecordsRead | undefined) => {
    const record = result?.kind === "page" ? result.read.document.records[0] : undefined
    return record === undefined || record.values === null ? null : instantOf(record.values[STARTS_AT])
  }
  const next = nearestAt(after.data), previous = nearestAt(before.data)
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
        <h3 className="text-sm font-medium" aria-live="polite">{windowTitle(zoom, window)}</h3>
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

      <div className={cn("overflow-x-auto rounded-xl border border-node-border bg-card", fetching && "opacity-70")} aria-busy={fetching}>
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
          {pending && <div className="grid gap-1 p-3" aria-label="Reading records">{Array.from({ length: 4 }, (_, i) => <Skeleton key={i} className="h-6" />)}</div>}
          {empty && (
            <div className="p-6">
              <EmptyState icon={CalendarRange} title={`Nothing in this ${zoom}`}>
                No {concept.title.toLowerCase()} records fall in {windowTitle(zoom, window)}{filter !== null && filter.all.length > 0 ? " with this filter" : ""}.
                <span className="mt-3 flex flex-wrap justify-center gap-2">
                  {previous !== null && <Button variant="outline" size="sm" onClick={() => onJump(previous)}><ChevronLeft /> Latest before ({timeText(previous, null)})</Button>}
                  {next !== null && <Button variant="outline" size="sm" onClick={() => onJump(next)}>Next after ({timeText(next, null)}) <ChevronRight /></Button>}
                </span>
              </EmptyState>
            </div>
          )}
          {!pending && lanes.length > 0 && (
            <div role="group" aria-label="Bars" onKeyDown={onKey}>
              {lanes.map((l, laneIndex) => (
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
                      const words = `${title}, ${timeText(bar.start, zone)} to ${timeText(bar.end, zone)}${zone === null ? "" : ` (${zone})`}${bar.overlaps ? ", overlaps another in this lane" : ""}`
                      return (
                        <button key={row.record.id} type="button" ref={(el) => { if (el) barRefs.current.set(row.record.id, el); else barRefs.current.delete(row.record.id) }}
                          tabIndex={index === Math.min(focused, order.length - 1) ? 0 : -1} aria-label={words} title={words} data-overlaps={bar.overlaps || undefined}
                          onFocus={() => setFocused(index)} onClick={() => onOpen(row)}
                          className={cn("absolute flex min-w-1.5 items-center gap-1 overflow-hidden rounded-md border px-1.5 text-left text-xs shadow-node outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-1",
                            "border-primary/40 bg-[color-mix(in_oklab,var(--primary)_14%,var(--card))] text-foreground hover:bg-[color-mix(in_oklab,var(--primary)_22%,var(--card))]",
                            bar.overlaps && "border-dashed border-[var(--tint-wait)]",
                            at.clippedStart && "rounded-l-none border-l-0", at.clippedEnd && "rounded-r-none border-r-0")}
                          style={{ left: `${at.left}%`, width: `${at.width}%`, top: GAP + bar.row * (BAR + GAP), height: BAR }}>
                          {at.clippedStart && <ChevronLeft className="size-3 shrink-0 text-muted-foreground" aria-hidden />}
                          <span className="truncate font-medium">{title}</span>
                          <span className="truncate text-muted-foreground tabular-nums">{timeText(bar.start, zone).split(", ").at(-1)}–{timeText(bar.end, zone).split(", ").at(-1)}{zone !== null && ` ${zone}`}</span>
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
      {lanes.length > 0 && <p className="text-meta text-muted-foreground">Arrow keys move between bars; Enter opens the record.</p>}
    </section>
  )
}
