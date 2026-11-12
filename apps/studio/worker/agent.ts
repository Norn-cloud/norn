import type { Cell } from "./cell.ts"
import type { Answer, Ask, Model } from "./model.ts"
import { QUESTION_DEFAULTS, populationOf, readCellCatalog, type CellCatalog, type CellMetric } from "./questions.ts"

/**
 * The dashboard assistant (server side). It reads the cell's catalog as the
 * signed-in person, asks the model for a typed proposal whose every name is
 * drawn from that catalog, compiles the proposal into the cell's `dashboard`
 * definition, and has the cell's checker decide it (`:admit`, nothing is
 * installed). A refusal goes back to the model once, with the checker's own
 * findings. The person accepts or declines; only acceptance installs, as them.
 */

export const VIEWS = ["number", "line", "area", "bar", "pie", "table"] as const
type ViewName = (typeof VIEWS)[number]

/** What the model proposes for one tile: names only from the catalog, the window as whole days. */
export interface ProposedTile {
  readonly title: string
  readonly metric: string
  /** A property to group by, or "" for one value. */
  readonly groupBy: string
  /** A grain for a time group-by, or "". */
  readonly grain: string
  readonly filters: readonly { readonly property: string; readonly operator: string; readonly value: string }[]
  /** First day included, YYYY-MM-DD. */
  readonly start: string
  /** First day not included, YYYY-MM-DD. */
  readonly end: string
  readonly view: ViewName
  readonly width: "half" | "full"
}
export interface Proposed {
  readonly title: string
  /** What the dashboard shows, and any part of the request the catalog cannot answer. */
  readonly note: string
  readonly tiles: readonly ProposedTile[]
}

/** Every property id the catalog's object types declare, once. */
const propertyIds = (catalog: CellCatalog) => [...new Set(catalog.objectTypes.flatMap(({ properties }) => properties.map(({ id }) => id)))]

/** The proposal's JSON Schema, its enums drawn from the catalog: a name the cell does not declare cannot be proposed. */
export function proposalSchema(catalog: CellCatalog): Record<string, unknown> {
  const properties = propertyIds(catalog)
  const text = (description: string) => ({ type: "string", description })
  return {
    type: "object",
    additionalProperties: false,
    required: ["title", "note", "tiles"],
    properties: {
      title: text("The dashboard's title, at most 120 characters."),
      note: text("One or two sentences for the person: what the dashboard shows, and any part of the request the catalog cannot answer."),
      tiles: {
        type: "array",
        description: "One to eight tiles, in reading order.",
        items: {
          type: "object",
          additionalProperties: false,
          required: ["title", "metric", "groupBy", "grain", "filters", "start", "end", "view", "width"],
          properties: {
            title: text("The tile's title, at most 120 characters."),
            metric: { type: "string", enum: catalog.metrics.map(({ name }) => name) },
            groupBy: { type: "string", enum: ["", ...properties], description: "A property of the metric's object type to group by, or empty for one value in total." },
            grain: { type: "string", enum: ["", ...catalog.grains], description: "For a time property only: the calendar grain. Empty otherwise." },
            filters: {
              type: "array",
              items: {
                type: "object",
                additionalProperties: false,
                required: ["property", "operator", "value"],
                properties: { property: { type: "string", enum: properties, description: "A property of the metric's object type." }, operator: { type: "string", enum: [...catalog.operators] }, value: { type: "string" } },
              },
            },
            start: text("First day included, YYYY-MM-DD."),
            end: text("First day not included, YYYY-MM-DD, after start."),
            view: { type: "string", enum: [...VIEWS] },
            width: { type: "string", enum: ["half", "full"] },
          },
        },
      },
    },
  }
}

const day = (ms: number) => new Date(ms).toISOString().slice(0, 10)

/** One metric in words: its name, label and how it is computed. */
function metricWords(metric: CellMetric, metrics: readonly CellMetric[]): string {
  const named = (name: string | undefined) => metrics.find((declared) => declared.name === name)
  const how = metric.type === "ratio"
    ? `ratio of ${named(metric.type_params?.numerator)?.label ?? "?"} to ${named(metric.type_params?.denominator)?.label ?? "?"} (not summable)`
    : `sum of ${metric.type_params?.expr ?? metric.name}`
  return `- ${metric.name} ("${metric.label}"): ${how}`
}

/** The system prompt: the rules of a typed dashboard and the catalog in words, by object type. Stable for a catalog and day, so it caches. */
export function systemPrompt(catalog: CellCatalog, now: number): string {
  const sections = catalog.objectTypes.flatMap(({ name, properties }) => {
    const metrics = catalog.metrics.filter((metric) => populationOf(metric, catalog.metrics) === name)
    if (metrics.length === 0) return []
    return [
      `Object type ${name}`,
      "Metrics:",
      ...metrics.map((metric) => metricWords(metric, catalog.metrics)),
      "Properties:",
      ...properties.map(({ id, type }) => `- ${id}: ${type?.kind ?? "unknown"}${type?.unit?.code ? ` in ${type.unit.code}` : ""}${type?.values?.length ? ` (one of ${type.values.join(", ")})` : ""}`),
      "",
    ]
  })
  return [
    "You design dashboards for Norn Studio. A dashboard is a typed definition, not free text: a title and tiles; each tile is one question over the data and a view that draws its answer.",
    "Propose only what the catalog below can answer. Every name you use must come from it. If part of the request cannot be answered, leave it out and say so in the note; never invent a metric or property.",
    "Each question measures one metric over that metric's object type, optionally grouped by one property of the same type (a time property needs a grain), optionally filtered on properties of the same type (all filters must hold), over a window of whole days.",
    "Views must fit the answer, and the server's checker refuses those that do not:",
    "- number: one value, so the tile has no group-by.",
    "- line or area: a time series, so group by a time property with a grain.",
    "- bar: a breakdown by a category, or a time series.",
    "- pie: the parts of one whole, so one summable metric (never a ratio) grouped by a category without a grain.",
    "- table: any answer.",
    "Never add amounts of different units or currencies: when a summed metric's object type has unit or currency properties, filter each to one value (or group by it). The checker refuses a sum that mixes them.",
    "Prefer a few strong tiles (two to six): a headline number first, then trends, then breakdowns. Use width full for time series and half for numbers and pies.",
    `Today is ${day(now)}. Unless the request names a period, use the last 26 whole weeks ending on the coming Monday.`,
    `Grains: ${catalog.grains.join(", ") || "none"}. Filter operators: ${catalog.operators.join(", ")}. At most ${catalog.maxGroupBy} group-by per question.`,
    "",
    ...sections,
  ].join("\n")
}

const slug = (title: string) => title.trim().toLowerCase().replace(/[^a-z0-9]+/g, "_").replace(/^_+|_+$/g, "") || "untitled"
const DAY = /^\d{4}-\d{2}-\d{2}$/

/**
 * The cell's `dashboard` payload for a proposal, exactly as Studio's builder
 * writes a question (the same members, defaults and numeric filter values).
 * It judges nothing: the cell's checker decides.
 */
export function compile(proposed: Proposed, catalog: CellCatalog, origin: { readonly proposal: string; readonly model: string; readonly request: string }, newId: () => string = () => crypto.randomUUID()) {
  const typeOf = (metric: string) => {
    const declared = catalog.metrics.find(({ name }) => name === metric)
    return declared === undefined ? null : populationOf(declared, catalog.metrics) ?? null
  }
  const kindOf = (objectSet: string | null, property: string) => catalog.objectTypes.find(({ name }) => name === objectSet)?.properties.find(({ id }) => id === property)?.type?.kind
  const value = (objectSet: string | null, property: string, text: string) => ["decimal", "integer"].includes(kindOf(objectSet, property) ?? "") && text.trim() !== "" && Number.isFinite(Number(text)) ? Number(text) : text
  let x = 0
  let y = 0
  const tiles = proposed.tiles.map((tile) => {
    const w = tile.width === "full" ? 2 : 1
    if (x + w > 2) { x = 0; y += 1 }
    const layout = { x, y, w, h: 1 }
    x += w
    if (x >= 2) { x = 0; y += 1 }
    const objectSet = typeOf(tile.metric)
    return {
      id: newId(),
      title: tile.title,
      question: {
        irVersion: QUESTION_DEFAULTS.irVersion,
        name: `studio.${slug(tile.title)}`,
        objectSet,
        aggregate: [{ metric: tile.metric }],
        groupBy: tile.groupBy === "" ? [] : [{ path: { property: tile.groupBy }, ...tile.grain !== "" ? { grain: tile.grain } : {} }],
        filter: tile.filters.map(({ property, operator, value: text }) => ({ path: { property }, operator, value: value(objectSet, property, text) })),
        window: { start: `${tile.start}T00:00:00Z`, end: `${tile.end}T00:00:00Z` },
        calendar: QUESTION_DEFAULTS.calendar,
        freshness: QUESTION_DEFAULTS.freshness,
      },
      view: tile.view === "number" || tile.view === "table" ? { kind: tile.view } : { kind: "chart", chart: tile.view },
      layout,
    }
  })
  return { title: proposed.title, tiles, origin: { by: "assistant" as const, proposal: origin.proposal, model: origin.model, request: origin.request.slice(0, 2000) } }
}
export type Definition = ReturnType<typeof compile>

const isRecord = (value: unknown): value is Record<string, unknown> => typeof value === "object" && value !== null && !Array.isArray(value)
/** The model's document as a proposal, if it has every member the schema requires; anything else is no proposal. */
export function proposedOf(value: unknown): Proposed | undefined {
  if (!isRecord(value) || typeof value.title !== "string" || typeof value.note !== "string" || !Array.isArray(value.tiles) || value.tiles.length === 0) return undefined
  const ok = value.tiles.every((tile) => isRecord(tile) && ["title", "metric", "groupBy", "grain", "start", "end", "view", "width"].every((key) => typeof tile[key] === "string")
    && DAY.test(String(tile.start)) && DAY.test(String(tile.end)) && VIEWS.includes(tile.view as ViewName)
    && Array.isArray(tile.filters) && tile.filters.every((filter) => isRecord(filter) && typeof filter.property === "string" && typeof filter.operator === "string" && typeof filter.value === "string"))
  return ok ? value as unknown as Proposed : undefined
}

export interface Finding { readonly code: string; readonly message: string; readonly pointer: string | null }
export interface Verdict { readonly admitted: boolean; readonly findings: readonly Finding[]; readonly restatement: string | null; readonly restatementSha256: string | null }

const findingsOf = (diagnostics: unknown): Finding[] => (Array.isArray(diagnostics) ? diagnostics : []).map((d: { code?: unknown; message?: unknown; span?: { pointer?: unknown } | null }) =>
  ({ code: String(d.code ?? ""), message: String(d.message ?? ""), pointer: typeof d.span?.pointer === "string" ? d.span.pointer : null }))

/** The cell's checker on a definition, as the person: admitted (with its restatement) or refused (with its findings). */
export async function admit(cell: Cell, id: string, definition: unknown): Promise<Verdict | { readonly unavailable: string }> {
  const answer = await cell.post("/v1/definitions:admit", { kind: "dashboard", id, payload: definition })
  if (answer.kind === "no-cell") return { unavailable: "This workspace has no data cell yet." }
  if (answer.status === 403) return { unavailable: "Your role may not create dashboards here." }
  if (answer.status !== 200 || !isRecord(answer.body)) return { unavailable: `The cell did not answer (${answer.status}).` }
  const body = answer.body
  return { admitted: body.admitted === true, findings: findingsOf(body.diagnostics), restatement: typeof body.restatement === "string" ? body.restatement : null, restatementSha256: typeof body.restatementSha256 === "string" ? body.restatementSha256 : null }
}

/** A refusal, in the words the model is asked to repair: each finding with the tile it is about. */
export function refusalWords(findings: readonly Finding[]): string {
  return findings.map(({ code, message, pointer }) => {
    const tile = /^\/tiles\/(\d+)/.exec(pointer ?? "")?.[1]
    return `- ${tile === undefined ? "dashboard" : `tile ${Number(tile) + 1}`}: ${message} (${code}${pointer ? ` at ${pointer}` : ""})`
  }).join("\n")
}

export interface Outcome {
  readonly definition: Definition | null
  readonly verdict: Verdict | null
  readonly note: string
  /** Every model call made, each paid for. */
  readonly answers: readonly Answer[]
  /** Why there is no proposal, when there is none. */
  readonly failure?: string
}

/**
 * One proposal for `request`: read the catalog, ask, compile, admit; on a
 * refusal ask once more with the checker's findings. Returns the last
 * definition the model proposed and the checker's verdict on it.
 */
export async function propose(cell: Cell, model: Model & { answerAll?(ask: Ask): Promise<readonly Answer[]> }, request: string, options: {
  readonly proposalId: string
  /** The definition id to admit under (admission does not depend on it). */
  readonly targetId: string
  readonly now: number
  /** What the origin quotes: the person's words, without any context the server added. */
  readonly originRequest?: string
  readonly attempts?: number
}): Promise<Outcome> {
  const cellCatalog = await readCellCatalog(cell)
  if (cellCatalog instanceof Response) return { definition: null, verdict: null, note: "", answers: [], failure: ((await cellCatalog.json().catch(() => ({}))) as { error?: string }).error ?? "The cell's catalog could not be read." }
  const catalog = cellCatalog
  if (catalog.metrics.length === 0) return { definition: null, verdict: null, note: "", answers: [], failure: "There is nothing you may read in this workspace to build a dashboard from." }
  const system = systemPrompt(catalog, options.now)
  const schema = proposalSchema(catalog)
  const answers: Answer[] = []
  const turns: { role: "user" | "assistant"; content: string }[] = [{ role: "user", content: request }]
  let last: Outcome = { definition: null, verdict: null, note: "", answers }
  for (let attempt = 0; attempt < (options.attempts ?? 2); attempt += 1) {
    const ask = { system, turns: [...turns], schema, maxTokens: 8000 }
    const made = model.answerAll ? await model.answerAll(ask) : [await model.answer(ask)]
    answers.push(...made)
    const answer = made.at(-1)
    const proposed = proposedOf(answer?.json)
    if (answer === undefined || proposed === undefined) return { ...last, answers, failure: last.definition ? undefined : answer?.failure ?? "The model gave no proposal." }
    const definition = compile(proposed, catalog, { proposal: options.proposalId, model: answer.model, request: options.originRequest ?? request })
    const verdict = await admit(cell, options.targetId, definition)
    if ("unavailable" in verdict) return { definition, verdict: null, note: proposed.note, answers, failure: verdict.unavailable }
    last = { definition, verdict, note: proposed.note, answers }
    if (verdict.admitted) return last
    turns.push({ role: "assistant", content: JSON.stringify(proposed) }, { role: "user", content: `The server's checker refused this proposal:\n${refusalWords(verdict.findings)}\nPropose a corrected dashboard the checker admits. Change only what the findings name; leave out what the catalog cannot answer and say so in the note.` })
  }
  return last
}
