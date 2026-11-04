import type { Answer, Question } from "./transport.ts"

/** The last segment of a declared name, in words: `demo.yield_week` → `Yield week`. */
export function words(name: string): string {
  const last = name.slice(name.lastIndexOf(".") + 1).replaceAll("_", " ")
  return last.charAt(0).toUpperCase() + last.slice(1)
}

const date = new Intl.DateTimeFormat("en-GB", { day: "numeric", month: "short", year: "numeric", timeZone: "UTC" })
export const day = (instant: string | number): string => date.format(new Date(instant))

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
