import { inAnalyticsLane, untilNotPending } from "./evaluate.ts"
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
/** How long a question waits for its type's profile (only a date range is read from it) before it asks the latest 52 weeks instead. */
export const PROFILE_BUDGET_MS = 5_000

/** The profile did not settle within its budget: the question asks a default window and says so. */
export class ProfileTimedOut extends Error {
  constructor() { super(`The cell's profile didn't arrive within ${PROFILE_BUDGET_MS / 1000} seconds.`) }
}

/** A profile is computed by the analytics engine: it waits its turn, a "pending" refusal is asked again, and each ask has a budget. */
export const readProfile = (objectType: string, budgetMs: number = PROFILE_BUDGET_MS): Promise<Read<ObjectProfile>> =>
  inAnalyticsLane(async () => {
    const ask = () => cellRead<ObjectProfile>(`/v1/objects/${encodeURIComponent(objectType)}:profile`, "Profiles are", { signal: AbortSignal.timeout(budgetMs) }).then((read) => ({ read }), (error: unknown) => ({
      error: error instanceof DOMException && (error.name === "TimeoutError" || error.name === "AbortError") ? new ProfileTimedOut() : error,
    }))
    const result = await untilNotPending(ask, (outcome) => "error" in outcome && outcome.error instanceof Error && /analytics engine call is pending/i.test(outcome.error.message))
    if ("error" in result) throw result.error
    return result.read
  })

/** A profiled value as sent: numbers by their exact text at `pointer`, text as is, nothing for null. */
export function profiled(read: Read<unknown>, pointer: string, value: unknown): string {
  if (value === null || value === undefined) return "—"
  if (typeof value === "number") return read.numbers.get(pointer) ?? String(value)
  return typeof value === "string" ? value : JSON.stringify(value)
}
