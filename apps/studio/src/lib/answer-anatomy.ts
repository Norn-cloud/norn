import { shown, type Bucket } from "./answer.ts"
import { groupDigits } from "./format.ts"
import type { Question } from "./transport.ts"

/**
 * An answer's headline: the number it leads with and one plain sentence of facts the server sent,
 * with no arithmetic of its own. A time series leads with its latest bucket and names the one
 * before it side by side; an ungrouped answer is its one bucket; a breakdown by values leads with
 * no single number.
 */
export interface Headline {
  readonly bucket: Bucket | undefined
  /** The headline bucket's label, saying "so far" when its period had not ended at the cell's clock. */
  readonly label: string
  readonly sentence: string
}

/**
 * A bucket's figure as a reader reads it: a ratio as its percentage, an amount as its exact digits
 * grouped in thousands with its unit. Suppressed buckets keep their words.
 */
export function figure(bucket: Pick<Bucket, "value" | "totals" | "suppressed">, unit?: string | null): string {
  if (bucket.suppressed || bucket.totals.length > 1) return shown(bucket)
  return `${groupDigits(bucket.value)}${unit ? ` ${unit}` : ""}`
}

/** Whether a time bucket's period runs past the cell's clock: its figure is the period so far. */
export function openPeriod(bucket: Pick<Bucket, "bucket">, grain: string | undefined, nowMs: number): boolean {
  const days = grain === undefined ? undefined : GRAIN_DAYS[grain]
  if (days === undefined || typeof bucket.bucket !== "string") return false
  const start = Date.parse(bucket.bucket.length === 10 ? `${bucket.bucket}T00:00:00Z` : bucket.bucket)
  return !Number.isNaN(start) && start + days * DAY > nowMs
}

const DAY = 86_400_000
const GRAIN_DAYS: Readonly<Record<string, number>> = { day: 1, week: 7 }
const records = (count: number) => `${count} ${count === 1 ? "record" : "records"}`
const lower = (label: string) => label.charAt(0).toLowerCase() + label.slice(1)

export function headlineOf(title: string, question: Pick<Question, "groupBy">, rows: readonly Bucket[], nowMs: number, unit?: string | null): Headline {
  const [by] = question.groupBy ?? []
  const latest = rows.at(-1)
  if (latest === undefined) return { bucket: undefined, label: "", sentence: `${title}: the cell answered with no buckets.` }
  if (by !== undefined && by.grain === undefined) {
    return { bucket: undefined, label: "", sentence: `${title} by ${by.path.property.replaceAll("_", " ")}, in ${rows.length} ${rows.length === 1 ? "group" : "groups"}.` }
  }
  const open = openPeriod(latest, by?.grain, nowMs)
  const label = open ? `${latest.label}, so far` : latest.label
  const value = latest.suppressed ? lower(latest.value) : `${figure(latest, unit)}, from ${records(latest.members.length)}`
  const head = by === undefined ? `${title} is ${value}.` : `${title} for the ${lower(label)} is ${value}.`
  const before = rows.at(-2)
  if (by === undefined || before === undefined) return { bucket: latest, label, sentence: head }
  return { bucket: latest, label, sentence: `${head} The ${lower(before.label)}: ${before.suppressed ? lower(before.value) : figure(before, unit)}.` }
}

/** One group of an answer grouped by a metric's unit sources: the values that name it and its bucket. */
export interface UnitGroup {
  readonly key: string
  /** The group's value of each unit source, in the question's group-by order. */
  readonly values: Readonly<Record<string, unknown>>
  /** Its unit: the first source with a value, or `null` when none has one. */
  readonly unit: string | null
  readonly bucket: Bucket
}

/**
 * The groups of an answer grouped by `sources` alone, as the cell returned them, most records first
 * (by the records each group counted; ties keep the cell's order). Only combinations the cell
 * returned appear.
 */
export function unitGroups(sources: readonly string[], rows: readonly Bucket[]): UnitGroup[] {
  return rows
    .map((bucket, order) => {
      const parts = sources.length === 1 ? [bucket.bucket] : Array.isArray(bucket.bucket) ? bucket.bucket as unknown[] : []
      const values = Object.fromEntries(sources.map((source, index) => [source, parts[index] ?? null]))
      const named = parts.find((part) => typeof part === "string" && part !== "")
      return { group: { key: bucket.key, values, unit: typeof named === "string" ? named : null, bucket }, order }
    })
    .sort((a, b) => b.group.bucket.members.length - a.group.bucket.members.length || a.order - b.order)
    .map(({ group }) => group)
}

/** One property of the answer's object set, as the cell's question catalog declares it. */
export interface CatalogProperty { readonly id: string; readonly type?: { readonly kind?: unknown } | null }

/** A drill: a question derived from the answered one, for the cell to answer or refuse. */
export interface Drill {
  readonly label: string
  readonly property: string
  readonly question: Question
}

const BREAKS = new Set(["text", "enum", "boolean"])

const iso = (ms: number) => new Date(ms).toISOString().replace(".000Z", "Z")

/**
 * The questions "break out by …" asks for one bucket: the same metrics and conditions, held to the
 * bucket (its period for a time grain, its value for a breakdown), grouped by each property the
 * cell's catalog declares as text, a choice or yes/no, other than those the question already
 * groups or fixes. The cell decides whether each is a question; nothing is added up here.
 */
export function breakOuts(question: Question, bucket: Pick<Bucket, "bucket">, properties: readonly CatalogProperty[]): Drill[] {
  const [by] = question.groupBy ?? []
  const fixed = new Set([...(question.filter ?? []).map(({ path }) => path.property), ...(by ? [by.path.property] : [])])
  let window = question.window
  let filter = [...question.filter ?? []]
  if (by !== undefined && by.grain !== undefined) {
    const days = GRAIN_DAYS[by.grain]
    const start = typeof bucket.bucket === "string" ? Date.parse(bucket.bucket.length === 10 ? `${bucket.bucket}T00:00:00Z` : bucket.bucket) : Number.NaN
    if (days === undefined || Number.isNaN(start)) return []
    const end = Math.min(start + days * DAY, Date.parse(question.window.end))
    window = { start: iso(Math.max(start, Date.parse(question.window.start))), end: iso(end) }
  } else if (by !== undefined) {
    filter = [...filter, { path: { property: by.path.property }, operator: "eq", value: bucket.bucket as never }]
  }
  return properties
    .filter(({ id, type }) => !fixed.has(id) && typeof type?.kind === "string" && BREAKS.has(type.kind))
    .map(({ id }) => ({
      label: `Break out by ${id.replaceAll("_", " ")}`,
      property: id,
      question: { ...question, name: "studio.drill", groupBy: [{ path: { property: id } }], filter, window } as Question,
    }))
}
