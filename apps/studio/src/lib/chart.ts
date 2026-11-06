import { percent } from "./format.ts"

/** One bucket as the chart needs it: its label, the server's exact value, and its operands. */
export interface ChartRow {
  readonly label: string
  readonly value: string
  readonly totals: readonly string[]
}

export interface ChartFrame {
  /** Plot positions only; every label shown is the server's text or its exact percentage. */
  readonly points: readonly { readonly label: string; readonly position: number; readonly shown: string; readonly exact: string }[]
  readonly scale: readonly [number, number]
  /** "%" when every bucket is a ratio (numerator and denominator), else none. */
  readonly unit: "%" | ""
}

/**
 * The chart's points and scale, or `undefined` when no bucket has a finite
 * value. Ratios plot as whole percents padded by one point; sums plot at
 * their own value, padded to whole units.
 */
export function chartFrame(rows: readonly ChartRow[]): ChartFrame | undefined {
  const ratio = rows.length > 0 && rows.every(({ totals }) => totals.length > 1)
  const points = rows
    .map(({ label, value }) => ({ label, position: Number(value) * (ratio ? 100 : 1), shown: ratio ? percent(value) : value, exact: value }))
    .filter(({ position }) => Number.isFinite(position))
  if (points.length === 0) return undefined
  const positions = points.map(({ position }) => position)
  const pad = ratio ? 1 : 0
  return { points, scale: [Math.floor(Math.min(...positions)) - pad, Math.ceil(Math.max(...positions)) + pad], unit: ratio ? "%" : "" }
}
