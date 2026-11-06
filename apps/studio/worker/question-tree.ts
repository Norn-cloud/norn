import { ANALYTICS, GRAINS, questionCatalog } from "./questions.ts"

/**
 * The question tree's server side (D59): the options each column offers, the
 * top pick in each, and the plain-words restatement a person signs off on.
 * Everything here comes from the catalog's declarations and the cell's
 * admitted question; nothing is read from row values, and nothing is written
 * by a model.
 */

type Admit = (artifactsJson: string, metricsJson: string, questionJson: string) => string

interface Property { readonly name: string; readonly kind: string; readonly unit: string | null; readonly values?: readonly string[] }
interface Metric { readonly name: string; readonly label: string; readonly type: string; readonly type_params?: { readonly expr?: string; readonly numerator?: string; readonly denominator?: string } }
interface Question {
  readonly name?: string
  readonly objectSet?: string | null
  readonly aggregate?: readonly { readonly metric: string }[]
  readonly groupBy?: readonly { readonly path: { readonly property: string }; readonly grain?: string }[]
  readonly filter?: readonly { readonly path: { readonly property: string }; readonly operator: string; readonly value: unknown }[]
  readonly window?: { readonly start?: string; readonly end?: string }
}

/** One option in a column: what it is in words, what it adds to the question, and whether it is the top pick. */
export interface Choice {
  readonly id: string
  readonly label: string
  readonly detail: string
  readonly recommended: boolean
  /** The QuestionIR fragment this option sets. */
  readonly value: unknown
}

const words = (name: string) => name.replaceAll("_", " ")
const OPERATOR_WORDS: Record<string, string> = { eq: "is", ne: "is not", lt: "is below", le: "is at most", gt: "is above", ge: "is at least" }
const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"]
const day = (iso: string) => {
  const date = new Date(iso)
  return Number.isNaN(date.getTime()) ? null : { day: date.getUTCDate(), month: MONTHS[date.getUTCMonth()] ?? "", year: date.getUTCFullYear() }
}

function properties(): readonly Property[] {
  const [entity] = ANALYTICS.artifacts
  return (entity?.payload.objectType.properties ?? []).map(({ id, type }: { id: string; type: { kind: string; unit?: { code: string }; values?: readonly string[] } }) =>
    ({ name: id, kind: type.kind, unit: type.unit?.code ?? null, ...type.values ? { values: type.values } : {} }))
}
const metrics = () => ANALYTICS.metrics as readonly Metric[]
const metricByName = (name: string) => metrics().find((metric) => metric.name === name)
const unitOf = (property: string | undefined) => properties().find(({ name }) => name === property)?.unit ?? null

/** A metric in words: its label and how it is computed, e.g. "Yield (recovered ÷ input)". */
function measureWords(name: string): { readonly label: string; readonly how: string } {
  const metric = metricByName(name)
  if (metric === undefined) return { label: name, how: "" }
  if (metric.type === "ratio") {
    const property = (metricName?: string) => metricByName(metricName ?? "")?.type_params?.expr ?? words(metricName ?? "")
    return { label: metric.label, how: `${words(property(metric.type_params?.numerator))} ÷ ${words(property(metric.type_params?.denominator))}` }
  }
  return { label: metric.label, how: `sum of ${words(metric.type_params?.expr ?? metric.label)}` }
}

/**
 * The question in plain words, from its QuestionIR alone: the same question
 * always reads the same, and it is never a model's paraphrase.
 */
export function restate(question: Question): string {
  const measures = (question.aggregate ?? []).map(({ metric }) => {
    const { label, how } = measureWords(metric)
    return how ? `${label} (${how})` : label
  })
  const set = words((question.objectSet ?? "").split(".").pop() || "record")
  const [by] = question.groupBy ?? []
  const grouping = by === undefined ? "in total" : by.grain ? `by ${by.grain} of ${words(by.path.property)}` : `by ${words(by.path.property)}`
  const conditions = (question.filter ?? []).map(({ path, operator, value }) => `${words(path.property)} ${OPERATOR_WORDS[operator] ?? operator} ${String(value)}`)
  return [`${measures.join(" and ") || "No measure"} of ${set} records`, grouping, ...conditions.length ? [`where ${conditions.join(" and ")}`] : [], windowWords(question.window)].join(", ")
}

/** A half-open window in words: "from 6 Jul to 26 Oct 2026 (26 Oct not included)". */
export function windowWords(window: Question["window"]): string {
  const start = window?.start ? day(window.start) : null
  const end = window?.end ? day(window.end) : null
  if (!start || !end) return "over no stated window"
  return `from ${start.day} ${start.month}${start.year === end.year ? "" : ` ${start.year}`} to ${end.day} ${end.month} ${end.year} (${end.day} ${end.month} not included)`
}

const hex = (bytes: ArrayBuffer) => [...new Uint8Array(bytes)].map((byte) => byte.toString(16).padStart(2, "0")).join("")

/** The restatement and its SHA-256: a sign-off records the hash of exactly the words the person read. */
export async function restatement(question: Question): Promise<{ readonly text: string; readonly sha256: string }> {
  const text = restate(question)
  return { text, sha256: hex(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(text))) }
}

/**
 * Each column's options, top pick first marked: the measures declared, the
 * properties a question can be grouped by and the grains the checker admits
 * for each, the conditions (the cell's own and each declared enum value), and
 * the windows. The top pick follows the cell's admitted question.
 */
export function questionRecommendations(admit: Admit) {
  const catalog = questionCatalog()
  const example = ANALYTICS.example as Question
  const picked = {
    metric: example.aggregate?.[0]?.metric,
    by: example.groupBy?.[0]?.path.property,
    grain: example.groupBy?.[0]?.grain,
  }
  const measures: Choice[] = metrics().map((metric) => {
    const { how } = measureWords(metric.name)
    const units = metric.type === "ratio" ? "" : unitOf(metric.type_params?.expr) ?? ""
    return { id: metric.name, label: metric.label, detail: [how, units].filter(Boolean).join(" · "), recommended: metric.name === picked.metric, value: { aggregate: [{ metric: metric.name }] } }
  })
  const groupable = properties().filter(({ kind }) => kind === "timestamp" || kind === "text" || kind === "enum")
  const groupBy: Choice[] = [
    { id: "", label: "In total", detail: "one answer, not grouped", recommended: picked.by === undefined, value: { groupBy: [] } },
    ...groupable.map(({ name, kind }) => ({ id: name, label: words(name), detail: kind === "timestamp" ? "a time, grouped by a grain" : "each value apart", recommended: name === picked.by, value: { groupBy: [{ path: { property: name } }] } })),
  ]
  // A grain is offered only when the checker admits the cell's question grouped by it.
  const admits = (question: unknown) => {
    try {
      return (JSON.parse(admit(JSON.stringify(ANALYTICS.artifacts), JSON.stringify(ANALYTICS.metrics), JSON.stringify(question))) as { admitted?: unknown }).admitted === true
    } catch {
      return false
    }
  }
  const grains: Record<string, Choice[]> = Object.fromEntries(groupable.filter(({ kind }) => kind === "timestamp").map(({ name }) => {
    const valid = GRAINS.filter((grain) => admits({ ...example, groupBy: [{ path: { property: name }, grain }] }))
    const top = valid.includes(picked.grain as (typeof GRAINS)[number]) && name === picked.by ? picked.grain : valid[0]
    return [name, valid.map((grain) => ({ id: grain, label: grain[0]?.toUpperCase() + grain.slice(1), detail: `each ${grain} of ${words(name)}`, recommended: grain === top, value: { grain } }))]
  }))
  const conditions: Choice[] = [
    ...(example.filter ?? []).map((condition) => ({ id: `${condition.path.property}:${condition.operator}:${String(condition.value)}`, label: `${words(condition.path.property)} ${OPERATOR_WORDS[condition.operator] ?? condition.operator} ${String(condition.value)}`, detail: "as the cell's admitted question asks", recommended: true, value: { filter: [condition] } })),
    ...properties().flatMap(({ name, values }) => (values ?? []).map((value) => ({ id: `${name}:eq:${value}`, label: `${words(name)} is ${value}`, detail: "a declared value", recommended: false, value: { filter: [{ path: { property: name }, operator: "eq", value }] } }))),
    { id: "every", label: "Every record", detail: "no condition", recommended: (example.filter ?? []).length === 0, value: { filter: [] } },
  ]
  const windows: Choice[] = example.window?.start && example.window.end
    ? [{ id: "cell", label: windowWords(example.window), detail: "the cell's admitted window", recommended: true, value: { window: example.window } }]
    : []
  return {
    basis: `From the catalog's declarations and the cell's admitted question ${example.name ?? "(none)"}; never from row values.`,
    objectSet: catalog.objectSet,
    measures,
    groupBy,
    grains,
    conditions,
    filterable: properties().filter(({ kind }) => kind !== "timestamp").map(({ name, kind, unit, values }) => ({ name, kind, unit, values: values ?? [] })),
    windows,
  }
}
