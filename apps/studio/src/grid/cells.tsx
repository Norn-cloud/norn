import { Badge } from "@/components/ui/badge"
import type { Field } from "../lib/catalog.ts"
import { member } from "../lib/exact.ts"
import { factText } from "../lib/objects.ts"
import { rowWithheld } from "../lib/records-query.ts"
import type { ObjectRecord } from "../lib/transport.ts"

/** One record as a page sent it, with that page's exact number texts. */
export interface GridRow {
  readonly record: ObjectRecord
  readonly at: string
  readonly numbers: ReadonlyMap<string, string>
}

export const WITHHELD = "Withheld"
export const WITHHELD_HINT = "The server withheld this record's values from your role. Nothing is shown in their place."

export const isWithheld = (row: GridRow): boolean => rowWithheld(row.record) || row.record.values === null

const right = (field: Field) => field.scalar.kind === "decimal" || field.scalar.kind === "integer" || field.scalar.kind === "duration"

export const alignRight = right

/** The value as plain text, for titles, copy and tests. Exact as the server sent it. */
export function cellText(field: Field, row: GridRow): string {
  if (isWithheld(row)) return WITHHELD
  const value = row.record.values[field.id]
  if (value === null || value === undefined) return "—"
  switch (field.scalar.kind) {
    case "decimal": return factText(field, row.record, row.numbers, row.at)
    case "integer": return row.numbers.get(`${row.at}/values/${member(field.id)}`) ?? String(value)
    case "boolean": return value === true ? "Yes" : value === false ? "No" : JSON.stringify(value)
    case "record":
    case "json-ext": return JSON.stringify(value)
    default: return typeof value === "string" ? value : JSON.stringify(value)
  }
}

/** A typed cell. Dates and timestamps are shown as sent, with their offset; amounts keep their exact digits and unit. */
export function Cell({ field, row }: { readonly field: Field; readonly row: GridRow }) {
  if (isWithheld(row)) return <span className="text-muted-foreground italic" title={WITHHELD_HINT}>{WITHHELD}</span>
  const value = row.record.values[field.id]
  if (value === null || value === undefined) return <span className="text-muted-foreground" aria-label="No value">—</span>
  const text = cellText(field, row)
  switch (field.scalar.kind) {
    case "enum": return <Badge variant="secondary" className="font-normal">{text}</Badge>
    case "boolean": return <span>{text}</span>
    case "date":
    case "timestamp":
    case "duration": return <time className="font-mono text-xs tabular-nums" dateTime={field.scalar.kind === "duration" ? undefined : text}>{text}</time>
    case "record":
    case "json-ext": return <code className="block max-w-64 truncate text-xs" title={text}>{text}</code>
    case "decimal":
    case "integer": return <span className="tabular-nums">{text}</span>
    default: return <span className="block max-w-80 truncate" title={text}>{text}</span>
  }
}
