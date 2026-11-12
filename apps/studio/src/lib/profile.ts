import { cellRead } from "./live-transport.ts"
import type { Read } from "./exact.ts"

/** One histogram bin as the cell sends it: bounds are values at the bin's edges, never computed ones. */
export interface Bin { readonly lower: unknown; readonly upper: unknown; readonly count: number }

/** One property's profile over the caller's authorized snapshot (`GET /v1/objects/{type}:profile`). */
export interface PropertyProfile {
  readonly property: string
  readonly kind: "numeric" | "temporal" | "boolean" | "text" | "other"
  readonly count: number
  readonly nulls: number
  readonly distinct: number
  readonly min: unknown
  readonly max: unknown
  readonly quantiles: readonly { readonly percent: number; readonly value: unknown }[]
  readonly histogram: { readonly kind: string; readonly bins: readonly Bin[] } | null
}

export interface ObjectProfile {
  readonly objectType: string
  readonly rows: number
  readonly withheldRows: number
  readonly quarantinedRows: number
  readonly approximate: boolean
  readonly scanLimit: number
  readonly engine: string
  readonly properties: readonly PropertyProfile[]
}

/** The cell's profile of one object type, every number in its exact text. */
export const readProfile = (objectType: string): Promise<Read<ObjectProfile>> =>
  cellRead<ObjectProfile>(`/v1/objects/${encodeURIComponent(objectType)}:profile`, "Profiles are")

/** A profiled value as sent: numbers by their exact text at `pointer`, text as is, nothing for null. */
export function profiled(read: Read<unknown>, pointer: string, value: unknown): string {
  if (value === null || value === undefined) return "—"
  if (typeof value === "number") return read.numbers.get(pointer) ?? String(value)
  return typeof value === "string" ? value : JSON.stringify(value)
}
