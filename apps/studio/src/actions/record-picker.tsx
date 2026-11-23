import { useInfiniteQuery, useQueryClient } from "@tanstack/react-query"
import { Check, ChevronsUpDown, RefreshCw, X } from "lucide-react"
import { useState } from "react"
import { Button } from "@/components/ui/button"
import { Command, CommandEmpty, CommandGroup, CommandInput, CommandItem, CommandList } from "@/components/ui/command"
import { Input } from "@/components/ui/input"
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover"
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
export interface PickerPage { readonly records: readonly PickedRecord[]; readonly next: string | undefined; readonly snapshotMoved?: true }

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

/** Whether any loaded picker page hit 409 `object.snapshot_moved` and pagination must restart from page 1. */
export const pickerSnapshotMoved = (pages: readonly (PickerPage | null | undefined)[] | undefined): boolean =>
  (pages ?? []).some((page) => page?.snapshotMoved === true)

/**
 * A field that names an existing record (the cell's `inputLinks`): a searchable list of the
 * records of its type this reader can see, by title, which sets the record's id. Search runs on
 * the server over the type's title property; more records load a page at a time; an id can always
 * be typed, so every record stays reachable. Optional fields can always be cleared back to empty.
 * The cell still decides the call; a record that doesn't exist is refused there. When the records
 * can't be read, the id is typed.
 */
export function RecordPicker({ objectKind, value, onPick, aria, read = readRecords }: {
  readonly objectKind: string; readonly value: string; readonly onPick: (id: string) => void; readonly aria: Aria
  readonly read?: typeof readRecords
}) {
  const { catalog } = useCatalog()
  const client = useQueryClient()
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
    queryFn: async ({ pageParam }) => pickerPage(await read(objectKind, pickerQuery(search, property, pageParam)), property),
    getNextPageParam: (last) => last?.snapshotMoved ? undefined : last?.next,
    retry: false, staleTime: 30_000,
  })
  const first = pages.data?.pages[0]
  const moved = pickerSnapshotMoved(pages.data?.pages)
  const records = moved ? [] : (pages.data?.pages ?? []).flatMap((page) => page?.records ?? [])
  // Only the unsearched read decides that the type can't be read; a refused search keeps the list.
  if (search.trim() === "" && !moved && pickerMode({ data: first, isError: pages.isError }) === "type") {
    return <Input {...aria} className="h-9" placeholder="Type the record's id" value={value} onChange={(e) => onPick(e.target.value)} />
  }
  const pick = (id: string, title?: string) => {
    if (title !== undefined) setTitles((known) => new Map(known).set(id, title))
    onPick(id)
    setOpen(false)
    setSearch("")
  }
  const reload = () => { void client.resetQueries({ queryKey }) }
  const optional = aria["aria-required"] !== true
  const chosen = value === "" ? null : titles.get(value) ?? records.find((record) => record.id === value)?.title ?? value
  const typed = search.trim()
  const noun = target?.title.toLowerCase() ?? "record"
  return (
    <div className="flex items-center gap-1.5">
      <Popover open={open} onOpenChange={setOpen}>
        <PopoverTrigger asChild>
          <Button {...aria} type="button" variant="outline" role="combobox" aria-expanded={open}
            className={cn(control, "flex min-w-0 flex-1 items-center justify-between gap-2 font-normal")}>
            {chosen === null
              ? <span className="text-muted-foreground">Choose a {noun}…</span>
              : <span className="flex min-w-0 items-baseline gap-2"><span className="truncate">{chosen}</span>{chosen !== value && <span className="truncate font-mono text-xs text-muted-foreground">{value}</span>}</span>}
            <ChevronsUpDown className="size-3.5 shrink-0 text-muted-foreground" aria-hidden />
          </Button>
        </PopoverTrigger>
        <PopoverContent align="start" className="w-[min(28rem,90vw)] rounded-node border-node-border p-0 shadow-float">
          {/* The cell lists and narrows the records; the list shows them in its order, unfiltered here. */}
          <Command shouldFilter={false} label={`Choose a ${noun}`}>
            <CommandInput placeholder={property === undefined ? "Type an id" : `Search ${noun} by ${property.replaceAll("_", " ")} or type an id`} value={search} onValueChange={setSearch} />
            <CommandList>
              {pages.isPending && <p className="px-3 py-2 text-meta text-muted-foreground">Reading {noun} records…</p>}
              {moved && (
                <div role="status" className="grid gap-2 px-3 py-2 text-meta text-muted-foreground">
                  <p className="flex items-center gap-1.5"><RefreshCw className="size-3.5 shrink-0" aria-hidden />{DATA_CHANGED} Reload to read from page 1.</p>
                </div>
              )}
              {moved && (
                <CommandItem value="__reload" onSelect={reload}>
                  <RefreshCw className="size-3.5" aria-hidden /> Reload list
                </CommandItem>
              )}
              {!pages.isPending && !moved && records.length === 0 && <CommandEmpty>{typed === "" ? `No ${noun} records to choose.` : property === undefined ? "Search isn't offered for this type." : `No ${noun} matches “${typed}”.`}</CommandEmpty>}
              {search.trim() !== "" && !moved && first === null && <p className="px-3 py-2 text-meta text-muted-foreground">The cell can't search these records; use the id.</p>}
              {optional && value !== "" && (
                <CommandItem value="__clear" onSelect={() => pick("")}>
                  <X className="size-3.5 text-muted-foreground" aria-hidden /> Clear selection
                </CommandItem>
              )}
              {records.length > 0 && (
                <CommandGroup>
                  {records.map((record) => (
                    <CommandItem key={record.id} value={record.id} onSelect={() => pick(record.id, record.title)}>
                      <Check className={cn("size-3.5", record.id === value ? "opacity-100" : "opacity-0")} aria-hidden />
                      <span className="min-w-0 flex-1 truncate">{record.title}</span>
                      {record.title !== record.id && <span className="truncate font-mono text-xs text-muted-foreground">{record.id}</span>}
                    </CommandItem>
                  ))}
                </CommandGroup>
              )}
              {!moved && pages.hasNextPage && (
                <CommandItem value="__more" onSelect={() => void pages.fetchNextPage()} disabled={pages.isFetchingNextPage}>
                  {pages.isFetchingNextPage ? "Reading…" : "Show more"}
                </CommandItem>
              )}
              {typed !== "" && !records.some((record) => record.id === typed) && (
                <CommandItem value="__typed" onSelect={() => pick(typed)}>Use the id <code className="text-xs">{typed}</code></CommandItem>
              )}
            </CommandList>
          </Command>
        </PopoverContent>
      </Popover>
      {optional && value !== "" && (
        <Button type="button" variant="ghost" size="icon" className="size-9 shrink-0 text-muted-foreground hover:text-foreground"
          aria-label="Clear selection" title="Clear selection" onClick={() => onPick("")}>
          <X className="size-4" aria-hidden />
        </Button>
      )}
    </div>
  )
}
