import type { Cell } from "./cell.ts"

/**
 * `/api/questions/*`: what Studio's question builders are built from, and the
 * server's verdict on a built question. Everything is read from the cell as
 * the signed-in actor: its catalog (only what this person may read, D67) and
 * its admission of a `question` definition (the same checker a tile's
 * question passes). Nothing is judged here, and no row value is read.
 */

interface PropertyType { readonly kind?: string; readonly unit?: { readonly code?: string }; readonly values?: readonly string[] }
export interface CellMetric {
  readonly name: string
  readonly label: string
  readonly type: string
  readonly type_params?: { readonly expr?: string; readonly numerator?: string; readonly denominator?: string; readonly metric_aggregation_params?: { readonly semantic_model?: string } }
}
/** The cell's `GET /v1/questions:catalog` document. */
export interface CellCatalog {
  readonly objectTypes: readonly { readonly name: string; readonly properties: readonly { readonly id: string; readonly type: PropertyType | null }[] }[]
  readonly metrics: readonly CellMetric[]
  readonly grains: readonly string[]
  readonly operators: readonly string[]
  readonly maxGroupBy: number
  readonly maxBuckets: number
}

const isRecord = (value: unknown): value is Record<string, unknown> => typeof value === "object" && value !== null && !Array.isArray(value)
const strings = (value: unknown): value is readonly string[] => Array.isArray(value) && value.every((item) => typeof item === "string")

export function cellCatalogOf(value: unknown): CellCatalog | undefined {
  if (!isRecord(value) || !Array.isArray(value.objectTypes) || !Array.isArray(value.metrics) || !strings(value.grains) || !strings(value.operators)) return undefined
  if (!value.objectTypes.every((type) => isRecord(type) && typeof type.name === "string" && Array.isArray(type.properties) && type.properties.every((property) => isRecord(property) && typeof property.id === "string"))) return undefined
  if (!value.metrics.every((metric) => isRecord(metric) && typeof metric.name === "string" && typeof metric.label === "string" && typeof metric.type === "string")) return undefined
  return value as unknown as CellCatalog
}

/**
 * The QuestionIR members Studio writes itself: the IR version it speaks, the
 * one calendar policy, and a live read no older than five seconds.
 */
export const QUESTION_DEFAULTS = { irVersion: "0.1", calendar: "iso_utc", freshness: { kind: "live", maxAgeMs: 5000 } } as const

/** The object type a metric ranges over: its own population, or (a ratio) its numerator's. */
export function populationOf(metric: CellMetric, metrics: readonly CellMetric[]): string | undefined {
  const own = metric.type_params?.metric_aggregation_params?.semantic_model
  if (own !== undefined) return own
  const numerator = metrics.find(({ name }) => name === metric.type_params?.numerator)
  return numerator?.type_params?.metric_aggregation_params?.semantic_model
}

/**
 * The builder's catalogue: one object type (the one most of this person's
 * metrics range over), its properties, the metrics over it, and what the
 * engine admits. No readable metric is an empty catalogue, not an error.
 */
export function studioCatalog(cell: CellCatalog) {
  const counts = new Map<string, number>()
  for (const metric of cell.metrics) {
    const over = populationOf(metric, cell.metrics)
    if (over !== undefined) counts.set(over, (counts.get(over) ?? 0) + 1)
  }
  const type = [...cell.objectTypes].sort((a, b) => (counts.get(b.name) ?? 0) - (counts.get(a.name) ?? 0))[0]
  const metrics = type === undefined ? [] : cell.metrics.filter((metric) => populationOf(metric, cell.metrics) === type.name)
  return {
    objectSet: type?.name ?? null,
    properties: (type?.properties ?? []).map(({ id, type: declared }) => ({ name: id, kind: declared?.kind ?? "unknown", unit: declared?.unit?.code ?? null })),
    metrics: metrics.map(({ name, label, type: kind }) => ({ name, label, type: kind })),
    grains: cell.grains,
    operators: cell.operators,
    example: QUESTION_DEFAULTS,
    declarations: { metrics, objectType: type ?? { name: null, properties: [] } },
    maxGroupBy: cell.maxGroupBy,
  }
}
export type StudioCatalog = ReturnType<typeof studioCatalog>

interface QuestionShape {
  readonly name?: unknown
  readonly objectSet?: unknown
  readonly aggregate?: readonly { readonly metric?: unknown }[]
}

/** What an admitted question measures, in the shape the verdict card reads: its population and the sums behind each measure. */
export function planOf(question: QuestionShape, metrics: readonly CellMetric[]) {
  const named = (name: unknown) => metrics.find((metric) => metric.name === name)
  const selected = (question.aggregate ?? []).map(({ metric }) => named(metric)).filter((metric) => metric !== undefined)
  const sums = selected.flatMap((metric) => metric.type === "ratio"
    ? [named(metric.type_params?.numerator), named(metric.type_params?.denominator)].flatMap((operand) => operand?.type_params?.expr ? [{ metric: operand.name, property: operand.type_params.expr }] : [])
    : metric.type_params?.expr ? [{ metric: metric.name, property: metric.type_params.expr }] : [])
  return { grain: typeof question.objectSet === "string" ? question.objectSet : "", question, sums, metrics: selected.map(({ name, label }) => ({ name, label })) }
}

interface Diagnostic { readonly code?: unknown; readonly message?: unknown; readonly span?: { readonly pointer?: unknown } | null }
const findingsOf = (diagnostics: unknown) => (Array.isArray(diagnostics) ? diagnostics as Diagnostic[] : []).map((d) => ({ code: String(d.code ?? ""), message: String(d.message ?? ""), pointer: typeof d.span?.pointer === "string" ? d.span.pointer : null }))

/** A definition id for checking a question: its own name when that is a valid id. */
const checkId = (name: unknown) => typeof name === "string" && /^[a-z][a-z0-9._-]{0,127}$/.test(name) ? name : "studio.question.check"

const json = (body: unknown, status = 200) => Response.json(body, { status, headers: { "cache-control": "no-store" } })
const refused = (status: number, error: string) => json({ error }, status)

/** A cell answer that is not a decision: no cell, a denial passed through as the cell wrote it, or the cell not answering. */
function unanswered(answer: Awaited<ReturnType<Cell["get"]>>): Response | undefined {
  if (answer.kind === "no-cell") return refused(409, "This workspace has no data cell yet.")
  if (answer.status === 401 || answer.status === 403) return json(answer.body ?? { error: "Your role may not do this." }, 403)
  if (answer.status === 404) return refused(503, "The cell does not serve the question catalog yet.")
  return answer.status === 200 ? undefined : refused(502, `The cell did not answer (${answer.status}).`)
}

export async function readCellCatalog(cell: Cell): Promise<CellCatalog | Response> {
  const answer = await cell.get("/v1/questions:catalog")
  const failed = unanswered(answer)
  if (failed !== undefined) return failed
  const catalog = answer.kind === "answer" ? cellCatalogOf(answer.body) : undefined
  return catalog ?? refused(502, "The cell's catalog is not one Studio reads.")
}

/**
 * The verdict on one built question: the cell admits it as a `question`
 * definition (nothing is installed), and an admitted one carries the
 * checker's restatement, the words a person signs off on.
 */
export async function checkQuestion(cell: Cell, question: unknown, metrics: readonly CellMetric[]) {
  const shape = isRecord(question) ? question as QuestionShape : {}
  const answer = await cell.post("/v1/definitions:admit", { kind: "question", id: checkId(shape.name), payload: question })
  if (answer.kind === "no-cell") return refused(409, "This workspace has no data cell yet.")
  const body = isRecord(answer.body) ? answer.body : {}
  if (answer.status !== 200) {
    // A malformed submission is the checker's finding; anything else is not a verdict.
    if (answer.status === 400 || answer.status === 422) return json({ kind: "question", question, admitted: false, plan: null, findings: [{ code: String(body.code ?? "question.invalid"), message: String(body.message ?? "The cell refused the question."), pointer: null }], restatement: null })
    return unanswered(answer) ?? refused(502, `The cell did not answer (${answer.status}).`)
  }
  const admitted = body.admitted === true
  const restatement = admitted && typeof body.restatement === "string" && typeof body.restatementSha256 === "string" ? { text: body.restatement, sha256: body.restatementSha256 } : null
  return json({ kind: "question", question, admitted, plan: admitted ? planOf(shape, metrics) : null, findings: findingsOf(body.diagnostics), restatement, ...admitted && restatement === null ? { restatementError: "The cell gave no restatement for this question." } : {} })
}

const words = (name: string) => name.replaceAll("_", " ")
const WEEK_MS = 7 * 86_400_000

/** Whole-week windows ending at the coming Monday (UTC): the last 12, 26 and 52 weeks. */
export function windows(now: number) {
  const today = new Date(now)
  const midnight = Date.UTC(today.getUTCFullYear(), today.getUTCMonth(), today.getUTCDate())
  const end = midnight + ((8 - new Date(midnight).getUTCDay()) % 7 || 7) * 86_400_000
  return [12, 26, 52].map((weeks) => ({
    id: `last-${weeks}`,
    label: `Last ${weeks} weeks`,
    detail: "whole weeks, Monday to Monday",
    recommended: weeks === 26,
    value: { window: { start: new Date(end - weeks * WEEK_MS).toISOString().replace(".000Z", "Z"), end: new Date(end).toISOString().replace(".000Z", "Z") } },
  }))
}

/**
 * Each column of the question tree, from the catalog alone: the measures
 * this person may read, what a question can be grouped by and the grains the
 * engine admits for a time, each declared value as a condition, and whole-week
 * windows. Never row values; the checker still decides each question.
 */
export function recommendations(catalog: StudioCatalog, now: number) {
  const properties = catalog.declarations.objectType.properties
  const kindOf = (property: { readonly type: PropertyType | null }) => property.type?.kind ?? "unknown"
  const metricOf = (name: string) => catalog.declarations.metrics.find((metric) => metric.name === name)
  const measures = catalog.declarations.metrics.map((metric, index) => {
    const ratio = metric.type === "ratio"
    const how = ratio ? `${words(metricOf(metric.type_params?.numerator ?? "")?.type_params?.expr ?? "")} ÷ ${words(metricOf(metric.type_params?.denominator ?? "")?.type_params?.expr ?? "")}` : `sum of ${words(metric.type_params?.expr ?? metric.label)}`
    return { id: metric.name, label: metric.label, detail: how, recommended: index === 0, value: { aggregate: [{ metric: metric.name }] } }
  })
  const groupable = properties.filter((property) => ["timestamp", "text", "enum"].includes(kindOf(property)))
  const time = groupable.find((property) => kindOf(property) === "timestamp")
  const groupBy = [
    { id: "", label: "In total", detail: "one answer, not grouped", recommended: time === undefined, value: { groupBy: [] } },
    ...groupable.map((property) => ({ id: property.id, label: words(property.id), detail: kindOf(property) === "timestamp" ? "a time, grouped by a grain" : "each value apart", recommended: property === time, value: { groupBy: [{ path: { property: property.id } }] } })),
  ]
  const grains = Object.fromEntries(groupable.filter((property) => kindOf(property) === "timestamp").map((property) => [property.id, catalog.grains.map((grain, index) => ({ id: grain, label: grain.charAt(0).toUpperCase() + grain.slice(1), detail: `each ${grain} of ${words(property.id)}`, recommended: index === 0, value: { grain } }))]))
  const conditions = [
    ...properties.flatMap((property) => (property.type?.values ?? []).map((value) => ({ id: `${property.id}:eq:${value}`, label: `${words(property.id)} is ${value}`, detail: "a declared value", recommended: false, value: { filter: [{ path: { property: property.id }, operator: "eq", value }] } }))),
    { id: "every", label: "Every record", detail: "no condition", recommended: true, value: { filter: [] } },
  ]
  return {
    basis: "From the cell's catalog: what you may read and what its engine admits; never from row values.",
    objectSet: catalog.objectSet,
    measures,
    groupBy,
    grains,
    conditions,
    filterable: properties.filter((property) => kindOf(property) !== "timestamp").map((property) => ({ name: property.id, kind: kindOf(property), unit: property.type?.unit?.code ?? null, values: property.type?.values ?? [] })),
    windows: windows(now),
  }
}

/** One `/api/questions/*` request, already resolved to its actor's cell. */
export async function questions(request: Request, cell: Cell, route: string, now = Date.now()): Promise<Response> {
  if (route === "/check" && request.method === "GET") return json({ served: true })
  if (route === "/check" && request.method === "POST") {
    const body = await request.json().catch(() => null) as { question?: unknown } | null
    if (!isRecord(body) || !isRecord(body.question)) return refused(400, "Send { question }.")
    const catalog = await readCellCatalog(cell)
    return catalog instanceof Response ? catalog : checkQuestion(cell, body.question, catalog.metrics)
  }
  if ((route === "/catalog" || route === "/recommendations") && request.method === "GET") {
    const catalog = await readCellCatalog(cell)
    if (catalog instanceof Response) return catalog
    const studio = studioCatalog(catalog)
    return json(route === "/catalog" ? studio : recommendations(studio, now))
  }
  return route === "/catalog" || route === "/check" || route === "/recommendations" ? refused(405, "Method not allowed.") : refused(404, "No such endpoint.")
}
