import { Lock } from "lucide-react"
import type { CSSProperties } from "react"
import { cn } from "@/lib/utils"
import type { Field } from "../lib/catalog.ts"
import { member } from "../lib/exact.ts"
import { factText } from "../lib/objects.ts"
import { propertyWithheld, rowWithheld } from "../lib/records-query.ts"
import type { ObjectRecord } from "../lib/transport.ts"

/** One record as a page sent it, with that page's exact number texts. */
export interface GridRow {
  readonly record: ObjectRecord
  readonly at: string
  readonly numbers: ReadonlyMap<string, string>
}

export const WITHHELD = "Withheld"
/** A whole row the server withheld: one message across the row, not one per cell. */
export const WITHHELD_ROW = "Withheld: you can't see this record's values"
export const WITHHELD_HINT = "The server withheld this record's values from your role. Nothing is shown in their place."
export const WITHHELD_PROPERTY_HINT = "The server withheld this value from your role."

export const isWithheld = (row: GridRow): boolean => rowWithheld(row.record) || row.record.values === null
export const fieldWithheld = (row: GridRow, field: Field): boolean => isWithheld(row) || propertyWithheld(row.record, field.id)

/** The canvas tints (D65, `--tint-*`), one per declared enum value by its position, so a value keeps its colour everywhere. */
const TINTS = ["--tint-state", "--tint-tool", "--tint-route", "--tint-outcome", "--tint-agent", "--tint-ask", "--tint-wait", "--tint-law"] as const

export function EnumChip({ field, value }: { readonly field: Field; readonly value: string }) {
  const index = field.scalar.kind === "enum" ? field.scalar.values.indexOf(value) : -1
  const tint = TINTS[(index < 0 ? 0 : index) % TINTS.length] ?? TINTS[0]
  return (
    <span title={value} style={{ "--tint": `var(${tint})` } as CSSProperties}
      className="inline-block max-w-full truncate rounded-md bg-[color-mix(in_oklab,var(--tint)_14%,transparent)] px-1.5 py-0.5 text-xs leading-tight text-[color-mix(in_oklab,var(--tint)_80%,var(--foreground))] ring-1 ring-[color-mix(in_oklab,var(--tint)_22%,transparent)] ring-inset">
      {value}
    </span>
  )
}

export function WithheldMark({ className, children = WITHHELD, title = WITHHELD_PROPERTY_HINT }: { readonly className?: string; readonly children?: string; readonly title?: string }) {
  return <span className={cn("inline-flex min-w-0 items-center gap-1.5 text-muted-foreground", className)} title={title}><Lock className="size-3 shrink-0" aria-hidden /><span className="truncate">{children}</span></span>
}

const right = (field: Field) => field.scalar.kind === "decimal" || field.scalar.kind === "integer" || field.scalar.kind === "duration"

export const alignRight = right

/** The value as plain text, for titles, copy and tests. Exact as the server sent it. */
export function cellText(field: Field, row: GridRow): string {
  if (fieldWithheld(row, field)) return WITHHELD
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
  if (fieldWithheld(row, field)) return <WithheldMark />
  const value = row.record.values[field.id]
  if (value === null || value === undefined) return <span className="text-muted-foreground" aria-label="No value">—</span>
  const text = cellText(field, row)
  switch (field.scalar.kind) {
    case "enum": return <EnumChip field={field} value={text} />
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
