import { CellUnavailable, NO_DATA_CELL, StudioScopeError, readStudioScope, scopedHostFetch } from "../definitions.ts"
import { member, readDocument, type Read } from "../exact.ts"
import { day, percent } from "../format.ts"
import { isAnswer, type Fetch, type Refusal } from "../live.ts"
import type { Answer } from "../transport.ts"
import { VERDICTS, type Verdict } from "./episode.ts"
import { DecodeError } from "./objects.ts"

/**
 * Learning pack questions for the Misfit board and learning charts (#1208, #1220 L-4):
 *
 *   GET /v1/questions/learning.misfit_by_law/answer     override and exception rate per law
 *   GET /v1/questions/learning.triage_over_time/answer  verdict counts per week
 *
 * Both are ordinary published pack questions returning `NornQueryResult` through the generic
 * question route. Exact JSON number tokens are preserved via `readDocument`; D53-suppressed
 * buckets stay explicitly suppressed and are never coerced to zero.
 */

export const MISFIT_BY_LAW_QUESTION = "learning.misfit_by_law"
export const TRIAGE_OVER_TIME_QUESTION = "learning.triage_over_time"

export const MISFIT_UNAVAILABLE = "This cell doesn't serve the learning.misfit_by_law question yet."
export const TRIAGE_TREND_UNAVAILABLE = "This cell doesn't serve the learning.triage_over_time question yet."

export interface RateFigure {
  /** Exact server ratio text (e.g. "0.417"), or null when suppressed/unavailable. */
  readonly exact: string | null
  /** Display percentage derived from the exact decimal text (e.g. "41.7%"), or null. */
  readonly percent: string | null
  /** Plot value on a 0..100 scale for Recharts positioning only, or null. */
  readonly plotPercent: number | null
  /** Exact numerator and denominator texts from `totals`, when served. */
  readonly numerator: string | null
  readonly denominator: string | null
  readonly suppressed: boolean
}

export interface MisfitLawRow {
  readonly lawId: string
  readonly override: RateFigure | null
  readonly exception: RateFigure | null
  /** Episode count reported by the server's denominator or member set. */
  readonly episodeCount: number | null
  /** Distinct contributing episode identities listed on the rows. */
  readonly memberIds: readonly string[]
  readonly anySuppressed: boolean
}

export interface AnswerMeta {
  readonly freshness: Answer["freshness"]
  readonly complete: boolean
  readonly knownAsOf: number
  readonly nowMs: number
  readonly effectiveAsOf: string
  readonly engine: Answer["engine"]
  readonly exclusionCount: number
}

export interface MisfitBoardData {
  readonly rows: readonly MisfitLawRow[]
  readonly meta: AnswerMeta
}

export interface CountFigure {
  readonly exact: string | null
  /** Numeric plot height for Recharts only, or null when suppressed/unavailable. */
  readonly plot: number | null
  readonly suppressed: boolean
}

export interface TriageWeekRow {
  readonly weekStart: string
  readonly label: string
  readonly counts: Readonly<Record<Verdict, CountFigure>>
  /** Exact sum across verdicts only when no verdict bucket in the week was suppressed. */
  readonly total: number | null
  readonly anySuppressed: boolean
}

export interface TriageOverTimeData {
  readonly weeks: readonly TriageWeekRow[]
  readonly meta: AnswerMeta
}

export type LearningQuestionRead<T> =
  | { readonly kind: "ok"; readonly data: T }
  | { readonly kind: "malformed"; readonly problem: string }
  | { readonly kind: "refused"; readonly refusal: Refusal }
  | { readonly kind: "unavailable" }
  | { readonly kind: "no-cell" }

function metaOf(answer: Answer): AnswerMeta {
  return {
    freshness: answer.freshness,
    complete: answer.complete,
    knownAsOf: answer.knownAsOf,
    nowMs: answer.nowMs,
    effectiveAsOf: answer.effectiveAsOf,
    engine: answer.engine,
    exclusionCount: Object.keys(answer.exclusions).length,
  }
}

function rowCountOf(row: Answer["rows"][string]): number | null {
  const count = (row as { memberCount?: unknown }).memberCount
  if (typeof count === "number" && Number.isSafeInteger(count) && count >= 0) return count
  return Object.keys(row.members).length
}

function rateOf(read: Read<Answer>, key: string, row: Answer["rows"][string]): RateFigure {
  const suppressed = (row as { suppressed?: unknown }).suppressed === true
  if (suppressed) {
    return { exact: null, percent: null, plotPercent: null, numerator: null, denominator: null, suppressed: true }
  }
  const path = `/rows/${member(key)}`
  const exact = row.value === null ? null : (read.numbers.get(`${path}/value`) ?? String(row.value))
  const numerator = row.totals.length >= 1 ? (read.numbers.get(`${path}/totals/0`) ?? String(row.totals[0])) : null
  const denominator = row.totals.length >= 2 ? (read.numbers.get(`${path}/totals/1`) ?? String(row.totals[1])) : null
  const num = exact === null ? null : Number(exact)
  const plotPercent = num !== null && Number.isFinite(num) ? Math.round(num * 1000) / 10 : null
  return {
    exact,
    percent: exact === null ? null : percent(exact),
    plotPercent,
    numerator,
    denominator,
    suppressed: false,
  }
}

/**
 * Decode `learning.misfit_by_law` (`NornQueryResult`) fail-closed.
 *
 * Accepts rows grouped by `[lawId, "override" | "exception"]` (both rates per law) or single-key
 * `lawId` (combined override/misfit rate). Any other bucket shape refuses as malformed.
 */
export function decodeMisfitByLaw(read: Read<Answer>): { readonly kind: "ok"; readonly data: MisfitBoardData } | { readonly kind: "malformed"; readonly problem: string } {
  try {
    const byLaw = new Map<string, { override: RateFigure | null; exception: RateFigure | null; episodes: number | null; members: Set<string>; suppressed: boolean }>()
    for (const [key, row] of Object.entries(read.document.rows)) {
      const b = row.bucket
      let lawId: string
      let signal: "override" | "exception"
      if (Array.isArray(b) && b.length === 2 && typeof b[0] === "string" && b[0] !== "" && (b[1] === "override" || b[1] === "exception")) {
        lawId = b[0]
        signal = b[1]
      } else if (typeof b === "string" && b !== "") {
        lawId = b
        signal = "override"
      } else {
        throw new DecodeError(`/rows/${key}/bucket must be [lawId, "override"|"exception"] or lawId`)
      }
      const figure = rateOf(read, key, row)
      const entry = byLaw.get(lawId) ?? { override: null, exception: null, episodes: null, members: new Set<string>(), suppressed: false }
      if (signal === "override") {
        if (entry.override !== null) throw new DecodeError(`duplicate override bucket for ${lawId}`)
        entry.override = figure
      } else {
        if (entry.exception !== null) throw new DecodeError(`duplicate exception bucket for ${lawId}`)
        entry.exception = figure
      }
      if (figure.suppressed) entry.suppressed = true
      for (const id of Object.keys(row.members)) entry.members.add(id)
      const den = figure.denominator !== null && /^\d+$/.test(figure.denominator) ? Number(figure.denominator) : rowCountOf(row)
      if (den !== null) entry.episodes = entry.episodes === null ? den : Math.max(entry.episodes, den)
      byLaw.set(lawId, entry)
    }
    const rows: MisfitLawRow[] = [...byLaw.entries()]
      .map(([lawId, entry]) => ({
        lawId,
        override: entry.override,
        exception: entry.exception,
        episodeCount: entry.episodes,
        memberIds: [...entry.members].sort(),
        anySuppressed: entry.suppressed,
      }))
      .sort((a, b) => {
        const ao = a.override?.plotPercent ?? -1
        const bo = b.override?.plotPercent ?? -1
        if (bo !== ao) return bo - ao
        const ae = a.exception?.plotPercent ?? -1
        const be = b.exception?.plotPercent ?? -1
        if (be !== ae) return be - ae
        return a.lawId.localeCompare(b.lawId)
      })
    return { kind: "ok", data: { rows, meta: metaOf(read.document) } }
  } catch (error) {
    if (error instanceof DecodeError) return { kind: "malformed", problem: error.message }
    throw error
  }
}

const ZERO_COUNT: CountFigure = { exact: "0", plot: 0, suppressed: false }
const ISO_DAY = /^\d{4}-\d{2}-\d{2}/

/**
 * Decode `learning.triage_over_time` (`NornQueryResult`) fail-closed.
 *
 * Each row's bucket is `[weekStart, verdict]` where `verdict` is one of `exception`, `noise`,
 * `pattern`, `gap`. Weeks are ordered chronologically.
 */
export function decodeTriageOverTime(read: Read<Answer>): { readonly kind: "ok"; readonly data: TriageOverTimeData } | { readonly kind: "malformed"; readonly problem: string } {
  try {
    const byWeek = new Map<string, Record<Verdict, CountFigure>>()
    for (const [key, row] of Object.entries(read.document.rows)) {
      const b = row.bucket
      if (!Array.isArray(b) || b.length !== 2 || typeof b[0] !== "string" || !ISO_DAY.test(b[0]) || !VERDICTS.includes(b[1] as Verdict)) {
        throw new DecodeError(`/rows/${key}/bucket must be [weekStart, verdict]`)
      }
      const weekStart = b[0]
      const verdict = b[1] as Verdict
      const suppressed = (row as { suppressed?: unknown }).suppressed === true
      const path = `/rows/${member(key)}`
      let figure: CountFigure
      if (suppressed) {
        figure = { exact: null, plot: null, suppressed: true }
      } else {
        if (row.value === null) throw new DecodeError(`/rows/${key}/value is null on a non-suppressed count`)
        const exact = read.numbers.get(`${path}/value`) ?? String(row.value)
        const plot = Number(exact)
        if (!Number.isFinite(plot) || plot < 0) throw new DecodeError(`/rows/${key}/value is not a non-negative count`)
        figure = { exact, plot, suppressed: false }
      }
      const counts = byWeek.get(weekStart) ?? { exception: ZERO_COUNT, noise: ZERO_COUNT, pattern: ZERO_COUNT, gap: ZERO_COUNT }
      counts[verdict] = figure
      byWeek.set(weekStart, counts)
    }
    const weeks: TriageWeekRow[] = [...byWeek.entries()]
      .sort(([a], [b]) => a.localeCompare(b))
      .map(([weekStart, counts]) => {
        const values = VERDICTS.map((v) => counts[v])
        const anySuppressed = values.some((c) => c.suppressed)
        const total = anySuppressed ? null : values.reduce((sum, c) => sum + (c.plot ?? 0), 0)
        return { weekStart, label: day(weekStart), counts, total, anySuppressed }
      })
    return { kind: "ok", data: { weeks, meta: metaOf(read.document) } }
  } catch (error) {
    if (error instanceof DecodeError) return { kind: "malformed", problem: error.message }
    throw error
  }
}

async function readQuestion<T>(
  questionName: string,
  decode: (read: Read<Answer>) => { readonly kind: "ok"; readonly data: T } | { readonly kind: "malformed"; readonly problem: string },
  fetcher: Fetch = (input, init) => fetch(input, init),
): Promise<LearningQuestionRead<T>> {
  let response: Response
  let raw: string
  try {
    const scoped = scopedHostFetch(fetcher, "/api/host", () => readStudioScope(fetcher))
    response = await scoped(`/v1/questions/${encodeURIComponent(questionName)}/answer`, { headers: { accept: "application/json" } })
    raw = await response.text()
  } catch (error) {
    if (error instanceof StudioScopeError && error.message === NO_DATA_CELL) return { kind: "no-cell" }
    if (error instanceof CellUnavailable) return { kind: "unavailable" }
    throw error
  }
  if (response.ok) {
    let read: Read<Answer>
    try { read = readDocument<Answer>(raw, "live") } catch { return { kind: "malformed", problem: "The cell's question answer was not JSON." } }
    if (!isAnswer(read.document)) return { kind: "malformed", problem: "The cell's answer did not match NornQueryResult." }
    return decode(read)
  }
  let body: unknown = null
  try { body = JSON.parse(raw) } catch { /* decided by status */ }
  const code = (body as { code?: unknown } | null)?.code
  const message = (body as { message?: unknown } | null)?.message
  if (response.status !== 404 && response.status !== 503 && typeof code === "string") {
    const explanation = (body as { explanation?: unknown } | null)?.explanation
    const category = (body as { category?: unknown } | null)?.category
    return {
      kind: "refused",
      refusal: {
        code,
        message: typeof message === "string" ? message : code,
        ...typeof category === "string" ? { category } : {},
        ...typeof explanation === "string" ? { explanation } : {},
      },
    }
  }
  if (response.status === 404 || response.status === 503) return { kind: "unavailable" }
  throw new CellUnavailable(response.status)
}

export const readMisfitByLaw = (fetcher?: Fetch): Promise<LearningQuestionRead<MisfitBoardData>> =>
  readQuestion(MISFIT_BY_LAW_QUESTION, decodeMisfitByLaw, fetcher)

export const readTriageOverTime = (fetcher?: Fetch): Promise<LearningQuestionRead<TriageOverTimeData>> =>
  readQuestion(TRIAGE_OVER_TIME_QUESTION, decodeTriageOverTime, fetcher)
