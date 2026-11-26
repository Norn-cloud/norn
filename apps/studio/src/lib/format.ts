import type { Answer, Question } from "./transport.ts"

const capital = (text: string) => text.charAt(0).toUpperCase() + text.slice(1)

/**
 * A column the metric compiler names for a path through links (`norn_path_5_event_11_occurred_uat_value`,
 * each part length-prefixed with its underscores escaped as `_u`), in words: "event's occurred at".
 * Undefined when `name` is not one.
 */
export function pathWords(name: string): string | undefined {
  const body = /^norn_path_(.+)_value$/.exec(name)?.[1]
  if (body === undefined) return undefined
  const parts: string[] = []
  let at = 0
  while (at < body.length) {
    const length = /^(\d+)_/.exec(body.slice(at))
    if (length === null) return undefined
    at += length[0].length
    let part = ""
    while (part.length < Number(length[1]) && at < body.length) {
      if (body[at] === "_" && body[at + 1] === "u") { part += "_"; at += 2 } else { part += body[at]; at += 1 }
    }
    if (part.length !== Number(length[1])) return undefined
    parts.push(part.replaceAll("_", " "))
    if (at < body.length && body[at++] !== "_") return undefined
  }
  if (parts.length === 0) return undefined
  return [...parts.slice(0, -1).map((link) => `${link}'s`), parts.at(-1)].join(" ")
}

/**
 * A metric the cell derives from a type (`analysis.object_<type>.<property>.by_<time>` or `.as_of_<time>`)
 * in words: "Quantity", "Quantity, as of observed at". Undefined when `name` is not one.
 */
export function derivedMetricWords(name: string): string | undefined {
  const match = /^analysis\.object_[0-9a-f]+\.([a-z0-9_]+)\.(by|as_of)_(.+)$/.exec(name)
  if (match === null) return undefined
  const [, property = "", kind, time = ""] = match
  const measured = capital(property.replaceAll("_", " "))
  return kind === "as_of" ? `${measured}, as of ${pathWords(time) ?? time.replaceAll("_", " ")}` : measured
}

/**
 * A declared or generated name in words: `demo.yield_week` → `Yield week`; a derived metric or a
 * compiled link path by what it measures (see {@link derivedMetricWords}, {@link pathWords}).
 * The raw name stays for the Technical view.
 */
export function words(name: string): string {
  const derived = derivedMetricWords(name)
  if (derived !== undefined) return derived
  const last = name.slice(name.lastIndexOf(".") + 1)
  return capital(pathWords(last) ?? last.replaceAll("_", " "))
}

const date = new Intl.DateTimeFormat("en-GB", { day: "numeric", month: "short", year: "numeric", timeZone: "UTC" })
export const day = (instant: string | number): string => date.format(new Date(instant))

const clock = new Intl.DateTimeFormat("en-GB", { day: "numeric", month: "short", year: "numeric", hour: "2-digit", minute: "2-digit", hourCycle: "h23", timeZone: "UTC" })
/**
 * An exact number's text with its whole part grouped in thousands: `7905 g` → `7,905 g`,
 * `-1234567.891` → `-1,234,567.891`. Only the leading number is touched; nothing is rounded,
 * and text that does not start with a plain decimal is returned as it is.
 */
export function groupDigits(text: string): string {
  const match = /^(-?)(\d+)(\.\d+)?(.*)$/s.exec(text)
  if (match === null) return text
  const [, sign = "", whole = "", fraction = "", rest = ""] = match
  return `${sign}${whole.replace(/\B(?=(\d{3})+(?!\d))/g, ",")}${fraction}${rest}`
}

/**
 * A stored date or instant in words: `2026-11-01` → `1 Nov 2026`; `2026-11-01T09:30:00Z` →
 * `1 Nov 2026, 09:30 UTC`. Text that is not one is returned as it is.
 */
export function readableTime(text: string): string {
  if (/^\d{4}-\d{2}-\d{2}$/.test(text)) return Number.isNaN(Date.parse(`${text}T00:00:00Z`)) ? text : day(`${text}T00:00:00Z`)
  if (!/^\d{4}-\d{2}-\d{2}T/.test(text) || Number.isNaN(Date.parse(text))) return text
  return stamp(text)
}

/** An instant to the minute, in UTC, saying so: `13 Nov 2026, 01:03 UTC`. */
export const stamp = (instant: string | number): string => `${clock.format(new Date(instant))} UTC`

/** Local medium date and short time. */
export const formatTimestamp = (ms: number): string =>
  new Date(ms).toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" })

/** "just now", "4 min ago", or the local medium date and short time. */
export function formatRelativeOrTimestamp(ms: number, now = Date.now()): string {
  const seconds = Math.round((now - ms) / 1000)
  if (seconds >= 0 && seconds < 60) return "just now"
  if (seconds >= 60 && seconds < 3600) return `${Math.round(seconds / 60)} min ago`
  return formatTimestamp(ms)
}

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
    `${words(path.property)} ${OPERATOR[operator] ?? operator} ${value === null ? "empty" : String(value)}`)
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
  if (kinds === 0) return "—"
  if (versions.length === 0) return `${kinds} kind${kinds === 1 ? "" : "s"} of record`
  const latest = versions.reduce((a, b) => (b > a ? b : a))
  return `${latest.slice(0, 16)} · ${kinds} kind${kinds === 1 ? "" : "s"} of record`
}

/** The refusal a sync records when this workspace has not installed the connection's connector. */
export const CONNECTOR_NOT_INSTALLED = "source.connector_missing"

/** One connection's state as the Connections table shows it. */
export interface ConnectionStatus {
  readonly label: string
  readonly variant: "secondary" | "outline" | "destructive"
  /** A plain sentence under the badge, when there is something to say. */
  readonly note: string | null
  /** The cell's error code and message, for a real failure only. */
  readonly error: { readonly code: string; readonly message: string | null } | null
  /** Whether "Sync now" can do anything. */
  readonly syncable: boolean
}

const CONNECTION_STATUS: Readonly<Record<string, Pick<ConnectionStatus, "label" | "variant">>> = {
  ok: { label: "Synced", variant: "secondary" },
  syncing: { label: "Syncing", variant: "outline" },
  partial: { label: "Partly synced", variant: "outline" },
  failed: { label: "Failed", variant: "destructive" },
  never: { label: "Not synced yet", variant: "outline" },
}

/**
 * A connection's state in plain words. A connection whose connector this workspace has not
 * installed has not failed: it is not set up yet, and syncing it again changes nothing, so it is
 * said as such, without a red badge or a raw code. Every other failure keeps its code and message.
 */
export function connectionStatus(values: { readonly status?: string | null; readonly last_error_code?: string | null; readonly last_error_message?: string | null }): ConnectionStatus {
  if (values.last_error_code === CONNECTOR_NOT_INSTALLED) {
    return { label: "Not set up", variant: "outline", note: "Its connector is not installed in this workspace yet.", error: null, syncable: false }
  }
  const known = CONNECTION_STATUS[values.status ?? ""] ?? { label: values.status ?? "—", variant: "outline" as const }
  const error = values.last_error_code ? { code: values.last_error_code, message: values.last_error_message ?? null } : null
  return { ...known, note: null, error, syncable: true }
}
