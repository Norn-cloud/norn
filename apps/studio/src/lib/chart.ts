import { percent } from "./format.ts"

/** One bucket as the chart needs it: its label, the server's exact value, and its operands. */
export interface ChartRow {
  readonly label: string
  readonly value: string
  readonly totals: readonly string[]
  /** A bucket the server suppressed: a gap in the line, its label in the tooltip. */
  readonly suppressed?: boolean
}

export interface ChartFrame {
  /** Plot positions only; every label shown is the server's text or its exact percentage. */
  readonly points: readonly { readonly label: string; readonly position: number | null; readonly shown: string; readonly exact: string }[]
  readonly scale: readonly [number, number]
  /** "%" when every bucket is a ratio (numerator and denominator), else none. */
  readonly unit: "%" | ""
}

/**
 * The chart's points and scale, or `undefined` when no bucket has a finite
 * value. Ratios plot as whole percents padded by one point; sums plot at
 * their own value, padded to whole units.
 */
/** What the empty chart says: no buckets at all, buckets that are all suppressed (the table still lists them), or none with a value. */
export function chartEmpty(rows: readonly ChartRow[]): string {
  if (rows.length === 0) return "The server returned no answers for this window."
  if (rows.every(({ suppressed }) => suppressed)) return "Every week in this window is suppressed, because too few records stand behind it. There is nothing to plot; the table lists each week."
  return "No week in this window has a value to plot."
}

export function chartFrame(rows: readonly ChartRow[]): ChartFrame | undefined {
  const answered = rows.filter(({ suppressed }) => !suppressed)
  const ratio = answered.length > 0 && answered.every(({ totals }) => totals.length > 1)
  const points = rows
    .map(({ label, value, suppressed }) => suppressed
      ? { label, position: null, shown: value, exact: value }
      : { label, position: Number(value) * (ratio ? 100 : 1), shown: ratio ? percent(value) : value, exact: value })
    .filter(({ position }) => position === null || Number.isFinite(position))
  const positions = points.flatMap(({ position }) => position === null ? [] : [position])
  if (positions.length === 0) return undefined
  const pad = ratio ? 1 : 0
  return { points, scale: [Math.floor(Math.min(...positions)) - pad, Math.ceil(Math.max(...positions)) + pad], unit: ratio ? "%" : "" }
}
