import { useQuery } from "@tanstack/react-query"
import { useId } from "react"
import { Input } from "@/components/ui/input"
import { cellCall } from "../lib/cell-read.ts"

/** Attributes a form field passes to its control (id, aria-*). */
export type Aria = Record<string, string | boolean | undefined>

/** The form's select and input look. */
export const control = "h-9 w-full rounded-lg border border-node-border bg-background px-3 text-sm aria-invalid:border-destructive aria-invalid:ring-2 aria-invalid:ring-destructive/20"

interface PickedPage { readonly records?: readonly { readonly id: string; readonly values?: Readonly<Record<string, unknown>> }[]; readonly next?: string | null }

/** A record's words in the picker: its id, then its name (a `name`, `title` or `label` field), else its first other text. */
const pickLabel = (record: { readonly id: string; readonly values?: Readonly<Record<string, unknown>> }) => {
  const texts = Object.entries(record.values ?? {}).filter(([key, value]) => typeof value === "string" && value !== record.id && !key.endsWith("_id") && key !== "id")
  const named = (texts.find(([key]) => /name|title|label/.test(key)) ?? texts[0])?.[1]
  return typeof named === "string" ? `${record.id} · ${named}` : record.id
}

/** The records a picker offers: the first page of the type, and whether it is every record. */
export interface PickerRecords { readonly records: NonNullable<PickedPage["records"]>; readonly complete: boolean }

/**
 * How a picker asks for the id: `type` when the records can't be read (refused, or the read
 * failed), `suggest` (type it, the first page offered) when there is more than one page, else
 * `choose` from them.
 */
export function pickerMode(read: { readonly data: PickerRecords | null | undefined; readonly isError: boolean }): "type" | "suggest" | "choose" {
  if (read.data === null || read.isError) return "type"
  return read.data !== undefined && !read.data.complete ? "suggest" : "choose"
}

/**
 * A field that names an existing record: the records of its type this reader can see, to pick
 * from. The cell still decides the call; a record that doesn't exist is refused there. When the
 * records can't be read, or there are more than one page, the id is typed (the first page offered
 * as suggestions), so every record stays reachable.
 */
export function RecordPicker({ objectKind, value, onPick, aria }: { readonly objectKind: string; readonly value: string; readonly onPick: (id: string) => void; readonly aria: Aria }) {
  const listId = useId()
  const records = useQuery({
    queryKey: ["picker", objectKind],
    queryFn: async (): Promise<PickerRecords | null> => {
      const read = await cellCall<PickedPage>(`/v1/objects/${encodeURIComponent(objectKind)}?limit=100`)
      return read.kind === "ok" ? { records: read.value.records ?? [], complete: read.value.next == null } : null
    },
    retry: false, staleTime: 30_000,
  })
  const mode = pickerMode(records)
  // Unreadable (refused, or the read failed): type the id, as before.
  if (mode === "type") return <Input {...aria} className="h-9" value={value} onChange={(e) => onPick(e.target.value)} />
  if (mode === "suggest") {
    return <>
      <Input {...aria} className="h-9" list={listId} placeholder="Type or choose an id" value={value} onChange={(e) => onPick(e.target.value)} />
      <datalist id={listId}>{(records.data?.records ?? []).map((item) => <option key={item.id} value={item.id}>{pickLabel(item)}</option>)}</datalist>
    </>
  }
  const items = records.data?.records ?? []
  return (
    <select {...aria} className={control} value={value} disabled={records.isPending} onChange={(e) => onPick(e.target.value)}>
      <option value="">{records.isPending ? "Loading…" : items.length === 0 ? "No records to choose" : "Choose…"}</option>
      {value !== "" && !items.some((item) => item.id === value) && <option value={value}>{value}</option>}
      {items.map((item) => <option key={item.id} value={item.id}>{pickLabel(item)}</option>)}
    </select>
  )
}

