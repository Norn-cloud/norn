import { useInfiniteQuery, useQueryClient } from "@tanstack/react-query"
import { Combobox } from "@base-ui/react/combobox"
import { Check, ChevronsUpDown, RefreshCw, Search, X } from "lucide-react"
import { useState } from "react"
import { Button } from "@/components/ui/button"
import { useLayer } from "@/components/ui/dialog"
import { Input } from "@/components/ui/input"
import { cn } from "@/lib/utils"
import type { Concept } from "../lib/catalog.ts"
import { useCatalog } from "../lib/use-catalog.ts"
import { DATA_CHANGED, type RecordsQuery } from "../lib/records-query.ts"
import { readRecords, type RecordsRead } from "../lib/records-source.ts"

/** Attributes a form field passes to its control (id, aria-*). */
export type Aria = Record<string, string | boolean | undefined>

/** The form's select and input look. */
export const control = "h-9 w-full rounded-lg border border-node-border bg-background px-3 text-sm aria-invalid:border-destructive aria-invalid:ring-2 aria-invalid:ring-destructive/20"

/** One record a picker offers: its id (the value it sets) and its title (what it reads as). */
export interface PickedRecord { readonly id: string; readonly title: string }

/** One page of a picker's records, and the cursor to the next; `snapshotMoved` signals 409 `object.snapshot_moved`. */
export interface PickerPage {
  readonly records: readonly PickedRecord[]
  readonly next: string | undefined
  readonly snapshotMoved?: true
}

const PAGE = 25

/**
 * The text property a picker searches and titles records by: the concept's declared `label` or
 * `display_name` (what `recordName` reads), else its first text property named like a name or
 * title. Undefined when it declares none: then the picker pages through records by id.
 */
export function titleProperty(concept: Concept | undefined): string | undefined {
  const texts = (concept?.fields ?? []).filter((field) => field.scalar.kind === "text")
  return (texts.find(({ id }) => id === "label") ?? texts.find(({ id }) => id === "display_name") ?? texts.find(({ id }) => /(^|_)(name|title)$/.test(id)))?.id
}

/** A record's title in the picker: its declared name (as `recordName` reads it), else its title property, else its id. */
export function pickedTitle(record: { readonly id: string; readonly values?: Readonly<Record<string, unknown>> | null }, property: string | undefined): string {
  const values = record.values ?? {}
  const named = [values["label"], values["display_name"], property === undefined ? undefined : values[property]].find((value) => typeof value === "string" && value !== "")
  return typeof named === "string" ? named : record.id
}

/**
 * The records query for one picker page: the type's records in the cell's order, narrowed on the
 * server to those whose title property contains `search`. Studio never filters a page itself, and
 * an unsearched read sends no query, so a cell that serves only recorded pages still lists them.
 */
export function pickerQuery(search: string, property: string | undefined, after: string | undefined): RecordsQuery {
  const text = search.trim()
  return {
    sort: [],
    filter: text === "" || property === undefined ? null : { all: [{ property, op: "contains", value: text }] },
    limit: PAGE,
    ...after === undefined ? {} : { after },
  }
}

/** A page read as the picker shows it; `snapshotMoved` on 409 `object.snapshot_moved`; null when unreadable. */
export function pickerPage(read: RecordsRead, property: string | undefined): PickerPage | null {
  if (read.kind === "restart") return { records: [], next: undefined, snapshotMoved: true }
  if (read.kind !== "page") return null
  // A withheld row has no title to read and no value to offer by it; it is left out, never guessed.
  const records = read.read.document.records.filter((record) => (record as { withheld?: unknown }).withheld !== true)
  return { records: records.map((record) => ({ id: record.id, title: pickedTitle(record, property) })), next: read.next }
}

/**
 * How a picker asks for the id: `type` when the records can't be read (refused, or the read
 * failed), else `search` from the records the cell lists.
 */
export function pickerMode(read: { readonly data: PickerPage | null | undefined; readonly isError: boolean }): "type" | "search" {
  return read.data === null || read.isError ? "type" : "search"
}

/** Error text for a failed subsequent picker page ("Show more"), or null for a readable/restart page. */
export function pickerPageError(read: RecordsRead): string | null {
  if (read.kind === "page" || read.kind === "restart") return null
  if (read.kind === "refused") return read.message
  if (read.kind === "no-cell") return "This workspace has no data cell yet."
  return "More records could not be read from this cell."
}

/**
 * Fetch one picker page: on page 1 (`after === undefined`), returns `null` for unreadable types so
 * `RecordPicker` falls back to a typed ID input; on subsequent pages (`after !== undefined`), a
 * refusal or unreadable response throws so TanStack Query preserves page 1, keeps the cursor, and
 * surfaces an inline error with Retry.
 */
export async function fetchPickerPage(
  read: typeof readRecords,
  objectKind: string,
  search: string,
  property: string | undefined,
  after: string | undefined,
): Promise<PickerPage | null> {
  const result = await read(objectKind, pickerQuery(search, property, after))
  const page = pickerPage(result, property)
  if (page !== null || after === undefined) return page
  throw new Error(pickerPageError(result) ?? "Could not load more records.")
}

/** Whether any loaded picker page hit 409 `object.snapshot_moved` and pagination must restart from page 1. */
export const pickerSnapshotMoved = (pages: readonly (PickerPage | null | undefined)[] | undefined): boolean =>
  (pages ?? []).some((page) => page?.snapshotMoved === true)

/**
 * One option in a picker's list. Every option is keyed by its kind, and a record by its id under
 * that kind, so a record whose id reads like an action ("__clear") is still that record.
 */
export type PickerChoice =
  | { readonly kind: "record"; readonly key: string; readonly id: string; readonly title: string }
  | { readonly kind: "clear"; readonly key: string }
  | { readonly kind: "more"; readonly key: string }
  | { readonly kind: "reload"; readonly key: string }
  | { readonly kind: "typed"; readonly key: string; readonly id: string }

/** A choice's key: its kind, then a record's or typed id. Two choices are the same option only when their keys match. */
export const choiceKey = (choice: Pick<PickerChoice, "kind"> & { readonly id?: string }): string =>
  choice.id === undefined ? choice.kind : `${choice.kind}:${choice.id}`

const recordChoice = (record: PickedRecord): PickerChoice => ({ kind: "record", key: choiceKey({ kind: "record", id: record.id }), ...record })

/**
 * The options a picker lists, in order: Reload when the snapshot moved; else Clear (optional
 * fields with a value, and only while nothing is searched), the records, Show more (or Retry), and
 * "Use the id" for a typed id that no listed record has. A search highlights its first option, so
 * Clear is never listed then: Enter after a search picks the match, never clears the field (the X
 * beside the field still clears it).
 */
export function pickerChoices({ records, moved, optional, value, more, typed }: {
  readonly records: readonly PickedRecord[]; readonly moved: boolean; readonly optional: boolean
  readonly value: string; readonly more: boolean; readonly typed: string
}): readonly PickerChoice[] {
  const choices: PickerChoice[] = []
  if (moved) choices.push({ kind: "reload", key: "reload" })
  if (!moved && optional && value !== "" && typed === "") choices.push({ kind: "clear", key: "clear" })
  if (!moved) choices.push(...records.map(recordChoice))
  if (!moved && more) choices.push({ kind: "more", key: "more" })
  if (typed !== "" && !records.some((record) => record.id === typed)) choices.push({ kind: "typed", key: choiceKey({ kind: "typed", id: typed }), id: typed })
  return choices
}

const sameChoice = (a: PickerChoice, b: PickerChoice): boolean => a.key === b.key

const option = "flex cursor-default items-center gap-2 rounded-lg px-3 py-2 text-sm outline-none select-none data-highlighted:bg-muted data-highlighted:text-foreground data-disabled:pointer-events-none data-disabled:opacity-50"

/**
 * A field that names an existing record (the cell's `inputLinks`): a searchable list of the
 * records of its type this reader can see, by title, which sets the record's id. Search runs on
 * the server over the type's title property; more records load a page at a time; an id can always
 * be typed, so every record stays reachable. Optional fields can always be cleared back to empty.
 * The cell still decides the call; a record that doesn't exist is refused there. When the records
 * can't be read, the id is typed.
 *
 * Base UI's Combobox (input inside the popup) owns the keyboard, focus, highlight and ARIA wiring;
 * the cell lists and narrows the records, so the combobox never filters them itself.
 */
export function RecordPicker({ objectKind, value, onPick, aria, read = readRecords }: {
  readonly objectKind: string; readonly value: string; readonly onPick: (id: string) => void; readonly aria: Aria
  readonly read?: typeof readRecords
}) {
  const { catalog } = useCatalog()
  const client = useQueryClient()
  const layer = useLayer()
  const target = catalog?.byName.get(objectKind)
  const property = titleProperty(target)
  const [open, setOpen] = useState(false)
  const [search, setSearch] = useState("")
  // Titles seen in any page, so the chosen record keeps its title once the search moves on.
  const [titles, setTitles] = useState<ReadonlyMap<string, string>>(new Map())
  const queryKey = ["picker", objectKind, property ?? null, search.trim()] as const
  const pages = useInfiniteQuery({
    queryKey,
    initialPageParam: undefined as string | undefined,
    queryFn: ({ pageParam }) => fetchPickerPage(read, objectKind, search, property, pageParam),
    getNextPageParam: (last) => last?.snapshotMoved ? undefined : last?.next,
    retry: false, staleTime: 30_000,
  })
  const first = pages.data?.pages[0]
  const moved = pickerSnapshotMoved(pages.data?.pages)
  const pageFailure = pages.isFetchNextPageError
    ? (pages.error instanceof Error ? pages.error.message : "Could not load more records.")
    : null
  const records = moved ? [] : (pages.data?.pages ?? []).flatMap((page) => page?.records ?? [])
  // Only the unsearched first page decides that the type can't be read; a refused search or page-2 failure keeps the list.
  if (search.trim() === "" && !moved && pickerMode({ data: first, isError: pages.isError && first === undefined }) === "type") {
    return <Input {...aria} className="h-9" placeholder="Type the record's id" value={value} onChange={(e) => onPick(e.target.value)} />
  }
  const optional = aria["aria-required"] !== true
  const typed = search.trim()
  const choices = pickerChoices({ records, moved, optional, value, more: pages.hasNextPage || pageFailure !== null, typed })
  const chosenTitle = value === "" ? null : titles.get(value) ?? records.find((record) => record.id === value)?.title ?? value
  const selected = value === "" ? null : recordChoice({ id: value, title: chosenTitle ?? value })
  const noun = target?.title.toLowerCase() ?? "record"
  const close = () => { setOpen(false); setSearch("") }
  const choose = (choice: PickerChoice | null, details: Combobox.Root.ChangeEventDetails) => {
    if (choice === null) return
    // Show more and Reload act on the list and keep it open; they never become the field's value.
    if (choice.kind === "more" || choice.kind === "reload") {
      details.cancel()
      if (choice.kind === "more") void pages.fetchNextPage()
      else void client.resetQueries({ queryKey })
      return
    }
    if (choice.kind === "record") setTitles((known) => new Map(known).set(choice.id, choice.title))
    onPick(choice.kind === "clear" ? "" : choice.id)
    close()
  }
  return (
    <div className="flex items-center gap-1.5">
      <Combobox.Root<PickerChoice>
        items={choices}
        filter={null}
        value={selected}
        onValueChange={choose}
        isItemEqualToValue={sameChoice}
        itemToStringLabel={(choice) => choice.kind === "record" ? choice.title : choice.key}
        inputValue={search}
        onInputValueChange={(next) => setSearch(next)}
        open={open}
        onOpenChange={(next) => { if (next) setOpen(true); else close() }}
        autoHighlight
      >
        <Combobox.Trigger {...aria}
          className={cn(control, "flex min-w-0 flex-1 items-center justify-between gap-2 font-normal outline-none focus-visible:ring-2 focus-visible:ring-ring/50")}>
          {chosenTitle === null
            ? <span className="text-muted-foreground">Choose {noun}…</span>
            : <span className="flex min-w-0 items-baseline gap-2"><span className="truncate">{chosenTitle}</span>{chosenTitle !== value && <span className="truncate font-mono text-xs text-muted-foreground">{value}</span>}</span>}
          <ChevronsUpDown className="size-3.5 shrink-0 text-muted-foreground" aria-hidden />
        </Combobox.Trigger>
        <Combobox.Portal container={layer}>
          <Combobox.Positioner align="start" sideOffset={4} className="z-50 outline-none">
            <Combobox.Popup aria-label={`Choose ${noun}`}
              className="flex max-h-[min(24rem,var(--available-height))] w-(--anchor-width) max-w-(--available-width) min-w-64 origin-(--transform-origin) flex-col overflow-hidden rounded-node border border-node-border bg-popover p-1 text-popover-foreground shadow-float outline-none transition-[opacity,scale] duration-100 data-starting-style:scale-95 data-starting-style:opacity-0 data-ending-style:scale-95 data-ending-style:opacity-0">
              <div className="flex items-center gap-2 border-b border-border/60 px-3">
                <Search className="size-4 shrink-0 text-muted-foreground" aria-hidden />
                <Combobox.Input aria-label={`Search ${noun} records`}
                  placeholder={property === undefined ? "Type an id" : `Search ${noun} by ${property.replaceAll("_", " ")} or type an id`}
                  className="h-10 w-full bg-transparent text-sm outline-none placeholder:text-muted-foreground" />
              </div>
              <Combobox.Status className="grid gap-1 px-3 empty:hidden [&>p]:py-2 text-meta text-muted-foreground">
                {pages.isPending && <p>Reading {noun} records…</p>}
                {moved && <p className="flex items-center gap-1.5"><RefreshCw className="size-3.5 shrink-0" aria-hidden />{DATA_CHANGED} Reload to read from page 1.</p>}
                {!pages.isPending && !moved && records.length === 0 && <p>{typed === "" ? `No ${noun} records to choose.` : property === undefined ? "Search isn't offered for this type." : `No ${noun} matches “${typed}”.`}</p>}
                {typed !== "" && !moved && first === null && <p>The cell can't search these records; use the id.</p>}
              </Combobox.Status>
              {!moved && pageFailure !== null && <p role="alert" className="px-3 py-2 text-meta text-destructive">{pageFailure}</p>}
              <Combobox.List aria-label={`${target?.title ?? "Record"} records`} className="min-h-0 flex-1 scroll-py-1 overflow-y-auto overscroll-contain empty:hidden">
                {choices.map((choice) => (
                  <Combobox.Item key={choice.key} value={choice} className={option}
                    disabled={choice.kind === "more" && pages.isFetchingNextPage}>
                    <ChoiceLabel choice={choice} value={value} fetching={pages.isFetchingNextPage} failed={pageFailure !== null} />
                  </Combobox.Item>
                ))}
              </Combobox.List>
            </Combobox.Popup>
          </Combobox.Positioner>
        </Combobox.Portal>
      </Combobox.Root>
      {optional && value !== "" && (
        <Button type="button" variant="ghost" size="icon" className="size-9 shrink-0 text-muted-foreground hover:text-foreground"
          aria-label="Clear selection" title="Clear selection" onClick={() => onPick("")}>
          <X className="size-4" aria-hidden />
        </Button>
      )}
    </div>
  )
}

/** What one option reads as. */
function ChoiceLabel({ choice, value, fetching, failed }: { readonly choice: PickerChoice; readonly value: string; readonly fetching: boolean; readonly failed: boolean }) {
  switch (choice.kind) {
    case "record":
      return <>
        <Check className={cn("size-3.5 shrink-0", choice.id === value ? "opacity-100" : "opacity-0")} aria-hidden />
        <span className="min-w-0 flex-1 truncate">{choice.title}</span>
        {choice.title !== choice.id && <span className="truncate font-mono text-xs text-muted-foreground">{choice.id}</span>}
      </>
    case "clear": return <><X className="size-3.5 text-muted-foreground" aria-hidden /> Clear selection</>
    case "reload": return <><RefreshCw className="size-3.5" aria-hidden /> Reload list</>
    case "more": return <>{fetching ? "Reading…" : failed ? "Retry" : "Show more"}</>
    case "typed": return <>Use the id <code className="text-xs">{choice.id}</code></>
  }
}
