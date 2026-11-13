import { Link } from "@tanstack/react-router"
import { Lock } from "lucide-react"
import type { CSSProperties } from "react"
import { FactProvenance } from "@/components/provenance"
import { MaskedValue } from "@/components/masked-value"
import { cn } from "@/lib/utils"
import type { Field } from "../lib/catalog.ts"
import { member } from "../lib/exact.ts"
import { displayOf, factText } from "../lib/objects.ts"
import { useView, type View } from "../lib/view.tsx"
import { propertyWithheld, rowWithheld } from "../lib/records-query.ts"
import type { ObjectRecord, SourceLineage } from "../lib/transport.ts"
import { maskedOf } from "../lib/privacy.ts"

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

/** A choice as a chip in its value's colour; `label` is what the reader reads (its declared label in Business, the stored value in Technical). */
export function EnumChip({ field, value, label = value }: { readonly field: Field; readonly value: string; readonly label?: string }) {
  const index = field.scalar.kind === "enum" ? field.scalar.values.indexOf(value) : -1
  const tint = TINTS[(index < 0 ? 0 : index) % TINTS.length] ?? TINTS[0]
  return (
    <span title={value} style={{ "--tint": `var(${tint})` } as CSSProperties}
      className="inline-block max-w-full truncate rounded-md bg-[color-mix(in_oklab,var(--tint)_14%,transparent)] px-1.5 py-0.5 text-xs leading-tight text-[color-mix(in_oklab,var(--tint)_80%,var(--foreground))] ring-1 ring-[color-mix(in_oklab,var(--tint)_22%,transparent)] ring-inset">
      {label}
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
  if (isWithheld(row)) return WITHHELD
  const masked = maskedOf(row.record, field.id)
  if (masked !== null) {
    const projected = row.record.values[field.id]
    return masked.mode === "refuse" ? WITHHELD : typeof projected === "string" ? projected : projected === null || projected === undefined ? "—" : JSON.stringify(projected)
  }
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

/** What a cell reads as in `view`: Business groups numbers and names dates and choices in words; Technical is {@link cellText}. */
export function cellDisplay(field: Field, row: GridRow, view: View): string {
  const exact = cellText(field, row)
  const value = row.record.values[field.id]
  return view === "technical" || fieldWithheld(row, field) || maskedOf(row.record, field.id) !== null || value === null || value === undefined ? exact : displayOf(field, value, exact)
}

/**
 * A typed cell, one renderer per value kind. Amounts keep their exact digits and their unit; in the
 * Business view digits are grouped, dates and times read in words (UTC, said so) and a choice by
 * its label. The exact text is always the title.
 */
export function Cell({ field, row }: { readonly field: Field; readonly row: GridRow }) {
  const { view } = useView()
  const masked = isWithheld(row) ? null : maskedOf(row.record, field.id)
  // A marked property is the server's projection, never the value: a chip, and "Reveal" when allowed.
  if (masked !== null) return <MaskedValue objectType={row.record.objectType} id={row.record.id} property={field.id} masked={masked} projected={row.record.values[field.id]} />
  if (fieldWithheld(row, field)) return <WithheldMark />
  const value = row.record.values[field.id]
  if (value === null || value === undefined) return <span className="text-muted-foreground" aria-label="No value">—</span>
  const exact = cellText(field, row)
  const text = cellDisplay(field, row, view)
  switch (field.scalar.kind) {
    case "enum": return <EnumChip field={field} value={exact} label={text} />
    case "boolean": return <span>{text}</span>
    case "date":
    case "timestamp": return <time className={cn("tabular-nums", view === "technical" && "font-mono text-xs")} dateTime={exact} title={exact}>{text}</time>
    case "duration": return <time className="font-mono text-xs tabular-nums">{text}</time>
    case "record":
    case "json-ext": return <code className="block max-w-64 truncate text-xs" title={text}>{text}</code>
    case "decimal":
    case "integer": return <span className="tabular-nums" title={exact}>{text}</span>
    default: return <span className="block max-w-80 truncate" title={text}>{text}</span>
  }
}

/** The way from a fact's card to the record's full lineage. */
export function LineageLink({ objectType, id }: { readonly objectType: string; readonly id: string }) {
  return <Link to="/sources/$concept/$" params={{ concept: objectType, _splat: id }} hash="lineage" className="text-sm font-medium underline-offset-4 hover:underline">Open the record's lineage</Link>
}

/**
 * A typed cell whose value opens its provenance card: the same renderer, with the card one click
 * away. Withheld and empty values have no card; a recorded page has no history to read.
 */
export function TracedCell({ field, row, live, lineage = null, variant = "glyph" }: {
  readonly field: Field
  readonly row: GridRow
  readonly live: boolean
  /** The record's lineage, when the page read it: a recorded snapshot's card is drawn from it. */
  readonly lineage?: SourceLineage | null
  readonly variant?: "inline" | "glyph"
}) {
  const { view } = useView()
  const value = row.record.values[field.id]
  if (fieldWithheld(row, field) || maskedOf(row.record, field.id) !== null || value === null || value === undefined) return <Cell field={field} row={row} />
  const { objectType, id, version } = row.record
  const label = field.id.replaceAll("_", " ")
  return (
    <FactProvenance objectType={objectType} id={id} version={version} fact={field.id} label={label} value={cellDisplay(field, row, view)} lineage={lineage} live={live}
      variant={variant} trace={<LineageLink objectType={objectType} id={id} />}>
      <Cell field={field} row={row} />
    </FactProvenance>
  )
}
