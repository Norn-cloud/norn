import type { Answer, Question } from "./transport.ts"

/** The last segment of a declared name, in words: `demo.yield_week` → `Yield week`. */
export function words(name: string): string {
  const last = name.slice(name.lastIndexOf(".") + 1).replaceAll("_", " ")
  return last.charAt(0).toUpperCase() + last.slice(1)
}

const date = new Intl.DateTimeFormat("en-GB", { day: "numeric", month: "short", year: "numeric", timeZone: "UTC" })
export const day = (instant: string | number): string => date.format(new Date(instant))

const clock = new Intl.DateTimeFormat("en-GB", { day: "numeric", month: "short", year: "numeric", hour: "2-digit", minute: "2-digit", hourCycle: "h23", timeZone: "UTC" })
/** An instant to the minute, in UTC, saying so: `13 Nov 2026, 01:03 UTC`. */
export const stamp = (instant: string | number): string => `${clock.format(new Date(instant))} UTC`

/** The question's window, with its excluded end shown as the last included day. */
export const period = ({ start, end }: Question["window"]): string =>
  `${day(start)} – ${day(Date.parse(end) - 86_400_000)}`

const OPERATOR: Record<string, string> = { eq: "is", ne: "is not", lt: "below", le: "at most", gt: "above", ge: "at least" }

export function headline(question: Question): string {
  const metric = question.aggregate.map(({ metric }) => words(metric)).join(", ")
  const grain = question.groupBy?.[0]?.grain
  return grain ? `${metric} by ${grain}` : metric
}

export function scope(question: Question): string {
  const filters = (question.filter ?? []).map(({ path, operator, value }) =>
    `${words(path.property)} ${OPERATOR[operator] ?? operator} ${String(value)}`)
  return [`${words(question.objectSet)} records`, ...filters].join(" · ")
}

export function bucketLabel(bucket: unknown, question: Question): string {
  if (bucket === null) return "All"
  const grain = question.groupBy?.[0]?.grain
  if (grain && typeof bucket === "string") return `${words(grain)} of ${day(bucket)}`
  return String(bucket)
}

export const FRESHNESS: Record<Answer["freshness"], { readonly label: string; readonly meaning: string }> = {
  exact: { label: "Up to date", meaning: "Every source is complete and current." },
  fresh: { label: "Up to date", meaning: "Every source is complete, within the allowed delay." },
  stale: { label: "Out of date", meaning: "Sources are complete but older than allowed." },
  incomplete: { label: "Incomplete", meaning: "Some inputs are missing or were left out." },
}

/**
 * A server decimal as a percentage rounded half away from zero to `digits`
 * places, computed on the decimal text so no binary float is involved:
 * `0.979685` → `97.97%`, `0.98` → `98.00%`. Text that is not a plain decimal
 * is returned unchanged.
 */
export function percent(text: string, digits = 2): string {
  const match = /^(-?)(\d+)(?:\.(\d+))?$/.exec(text)
  if (match === null) return text
  const [, sign = "", whole = "0", fraction = ""] = match
  const scaled = BigInt(whole + fraction) * 10n ** BigInt(2 + digits + 1) / 10n ** BigInt(fraction.length)
  const rounded = (scaled + 5n) / 10n
  const digitsText = rounded.toString().padStart(digits + 1, "0")
  const integer = digitsText.slice(0, digitsText.length - digits)
  const decimals = digitsText.slice(digitsText.length - digits)
  const negative = sign === "-" && rounded !== 0n
  return `${negative ? "-" : ""}${integer}${digits > 0 ? `.${decimals}` : ""}%`
}

/**
 * How far the last sync read, in words: the connector's watermark is either one mark or, per kind
 * of record, the source's own modified-since version; the latest of those is shown, with how many
 * kinds it covers. The full mark stays in the cell's text, shown on hover.
 */
export function watermarkText(watermark: string | null | undefined): string {
  if (!watermark) return "—"
  let marks: unknown
  try { marks = JSON.parse(watermark) } catch { return watermark }
  if (marks === null || typeof marks !== "object" || Array.isArray(marks)) return watermark
  const versions = Object.values(marks).flatMap((mark) => typeof (mark as { version?: unknown } | null)?.version === "string" ? [(mark as { version: string }).version] : [])
  const kinds = Object.keys(marks).length
  if (versions.length === 0) return `${kinds} kind${kinds === 1 ? "" : "s"} of record`
  const latest = versions.reduce((a, b) => (b > a ? b : a))
  return `${latest.slice(0, 16)} · ${kinds} kind${kinds === 1 ? "" : "s"} of record`
}
