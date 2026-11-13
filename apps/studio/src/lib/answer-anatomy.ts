import { shown, type Bucket } from "./answer.ts"
import type { Question } from "./transport.ts"

/**
 * An answer's headline: the number it leads with and one plain sentence of facts the server sent,
 * with no arithmetic of its own. A time series leads with its latest bucket and names the one
 * before it side by side; an ungrouped answer is its one bucket; a breakdown by values leads with
 * no single number.
 */
export interface Headline {
  readonly bucket: Bucket | undefined
  readonly sentence: string
}

const records = (count: number) => `${count} ${count === 1 ? "record" : "records"}`
const lower = (label: string) => label.charAt(0).toLowerCase() + label.slice(1)

export function headlineOf(title: string, question: Pick<Question, "groupBy">, rows: readonly Bucket[]): Headline {
  const [by] = question.groupBy ?? []
  const latest = rows.at(-1)
  if (latest === undefined) return { bucket: undefined, sentence: `${title}: the cell answered with no buckets.` }
  if (by !== undefined && by.grain === undefined) {
    return { bucket: undefined, sentence: `${title} by ${by.path.property.replaceAll("_", " ")}, in ${rows.length} ${rows.length === 1 ? "group" : "groups"}.` }
  }
  const value = latest.suppressed ? lower(latest.value) : `${shown(latest)}, from ${records(latest.members.length)}`
  const head = by === undefined ? `${title} is ${value}.` : `${title} for the ${lower(latest.label)} is ${value}.`
  const before = rows.at(-2)
  if (by === undefined || before === undefined) return { bucket: latest, sentence: head }
  return { bucket: latest, sentence: `${head} The ${lower(before.label)}: ${before.suppressed ? lower(before.value) : shown(before)}.` }
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
const DAY = 86_400_000
const GRAIN_DAYS: Readonly<Record<string, number>> = { day: 1, week: 7 }

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
