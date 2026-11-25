import { CellUnavailable, NO_DATA_CELL, StudioScopeError, scopedHostFetch } from "../definitions.ts"
import { member, readDocument, type Read } from "../exact.ts"
import { day, percent } from "../format.ts"
import { isAnswer, type Fetch, type Refusal } from "../live.ts"
import type { Answer } from "../transport.ts"
import { VERDICTS, type Verdict } from "./episode.ts"
import { DecodeError, resolveBoundLearningScope, type ScopedReadOptions } from "./objects.ts"

/**
 * Learning pack questions for the Misfit board and learning charts (#1208, #1220 L-4):
 *
 *   GET /v1/questions/learning.misfit_by_law/answer     override and exception rate per law
 *   GET /v1/questions/learning.triage_over_time/answer  verdict counts per week
 *
 * Both are ordinary published pack questions returning `NornQueryResult` through the generic
 * question route. Exact JSON number tokens are preserved via `readDocument`; table ranking, KPI
 * winners, episode counts and weekly totals use exact decimal/rational and BigInt arithmetic.
 * Floating-point `Number` conversions stay strictly in Recharts chart series (`plot` /
 * `plotPercent`). D53-suppressed buckets stay explicitly suppressed and omitted buckets stay
 * explicitly unknown — neither is ever coerced to zero.
 *
 * PROVISIONAL CONTRACT: The `bucket` and row payload conventions below are provisional and gated
 * behind S1 capability discovery (`publishedQuestions` containing both questions on
 * `GET /v1/definitions`) until the backend learning-pack lane pins them with route-produced
 * fixtures.
 */

import { MISFIT_BY_LAW_QUESTION, TRIAGE_OVER_TIME_QUESTION } from "./resource-names.ts"
export { MISFIT_BY_LAW_QUESTION, TRIAGE_OVER_TIME_QUESTION }

export const MISFIT_UNAVAILABLE = "This cell doesn't serve the learning.misfit_by_law question yet."
export const TRIAGE_TREND_UNAVAILABLE = "This cell doesn't serve the learning.triage_over_time question yet."

export interface RateFigure {
  /** Exact server ratio text (e.g. "0.41704"), or null when suppressed/unavailable. */
  readonly exact: string | null
  /** Display percentage derived from the exact decimal text (e.g. "41.70%"), or null. */
  readonly percent: string | null
  /** Plot value on a 0..100 scale for Recharts positioning ONLY, or null. Never used to rank or pick KPI winners. */
  readonly plotPercent: number | null
  /** Exact numerator and denominator integer texts from `totals`, when served. */
  readonly numerator: string | null
  readonly denominator: string | null
  readonly suppressed: boolean
}

export interface MisfitLawRow {
  readonly lawId: string
  readonly override: RateFigure | null
  readonly exception: RateFigure | null
  /** Exact episode count as a decimal integer string (BigInt-derived), or null. */
  readonly episodeCount: string | null
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

export type CountStatus = "value" | "suppressed" | "unknown"

export interface CountFigure {
  /** Whether the cell returned a count, suppressed it under D53, or omitted the bucket. */
  readonly status: CountStatus
  /** Exact integer count token as a decimal string when `status === "value"`, else null. */
  readonly exact: string | null
  /** Numeric plot height for Recharts ONLY when `status === "value"`, else null. */
  readonly plot: number | null
  readonly suppressed: boolean
}

export interface TriageWeekRow {
  readonly weekStart: string
  readonly label: string
  readonly counts: Readonly<Record<Verdict, CountFigure>>
  /** Exact BigInt sum across all four verdicts as a string, or null when any verdict is suppressed or unknown. */
  readonly total: string | null
  readonly anySuppressed: boolean
  readonly anyUnknown: boolean
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
  /** Server-reported scope moved away from the gate's verified scope; zero request reached the new cell. */
  | { readonly kind: "scope-changed" }

// ---- Exact decimal / rational / BigInt arithmetic (S5) ------------------------------------------

interface ExactDecimal {
  readonly coeff: bigint
  readonly exp: number
}

interface ExactRational {
  readonly num: bigint
  readonly den: bigint
}

const JSON_DECIMAL = /^(-?)(0|[1-9]\d*)(?:\.(\d+))?(?:[eE]([+-]?\d+))?$/
const MAX_DECIMAL_EXP = 10_000
const MAX_INT_EXP = 1_000

/** Parse a JSON number token into exact `coeff * 10^exp` without floating-point rounding. */
export function parseExactDecimal(token: string): ExactDecimal | null {
  const match = JSON_DECIMAL.exec(token)
  if (match === null) return null
  const [, sign = "", whole = "0", frac = "", expText] = match
  const expNum = expText === undefined ? 0 : Number(expText)
  if (!Number.isSafeInteger(expNum) || Math.abs(expNum) > MAX_DECIMAL_EXP) return null
  const rawCoeff = BigInt(whole + frac)
  if (rawCoeff === 0n) return { coeff: 0n, exp: 0 }
  const exp = expNum - frac.length
  if (Math.abs(exp) > MAX_DECIMAL_EXP) return null
  return { coeff: sign === "-" ? -rawCoeff : rawCoeff, exp }
}

/** Exact total order on two parsed decimal tokens (`< 0`, `0`, `> 0`), zero floats. */
export function compareExactDecimal(a: ExactDecimal, b: ExactDecimal): number {
  if (a.coeff === 0n && b.coeff === 0n) return 0
  if (a.coeff <= 0n && b.coeff > 0n) return -1
  if (a.coeff >= 0n && b.coeff < 0n) return 1
  if (a.exp === b.exp) return a.coeff < b.coeff ? -1 : a.coeff > b.coeff ? 1 : 0
  if (a.exp > b.exp) {
    const scaled = a.coeff * 10n ** BigInt(a.exp - b.exp)
    return scaled < b.coeff ? -1 : scaled > b.coeff ? 1 : 0
  }
  const scaled = b.coeff * 10n ** BigInt(b.exp - a.exp)
  return a.coeff < scaled ? -1 : a.coeff > scaled ? 1 : 0
}

function decimalToRational(dec: ExactDecimal): ExactRational {
  if (dec.coeff === 0n) return { num: 0n, den: 1n }
  if (dec.exp >= 0) return { num: dec.coeff * 10n ** BigInt(dec.exp), den: 1n }
  return { num: dec.coeff, den: 10n ** BigInt(-dec.exp) }
}

function compareExactRational(a: ExactRational, b: ExactRational): number {
  const lhs = a.num * b.den
  const rhs = b.num * a.den
  return lhs < rhs ? -1 : lhs > rhs ? 1 : 0
}

function rationalOf(fig: RateFigure, decimal: ExactDecimal): ExactRational {
  const num = fig.numerator !== null ? parseExactNonNegInt(fig.numerator) : null
  const den = fig.denominator !== null ? parseExactNonNegInt(fig.denominator) : null
  return num !== null && den !== null && den > 0n ? { num, den } : decimalToRational(decimal)
}

/** Convert an `ExactDecimal` into plain decimal text (`1.5e-2` → `0.015`) for exact `percent()` formatting. */
function plainDecimalText(dec: ExactDecimal): string {
  if (dec.coeff === 0n) return "0"
  const neg = dec.coeff < 0n
  const digits = (neg ? -dec.coeff : dec.coeff).toString()
  if (dec.exp >= 0) return `${neg ? "-" : ""}${digits}${"0".repeat(dec.exp)}`
  const scale = -dec.exp
  if (digits.length <= scale) {
    return `${neg ? "-" : ""}0.${"0".repeat(scale - digits.length)}${digits}`
  }
  const split = digits.length - scale
  return `${neg ? "-" : ""}${digits.slice(0, split)}.${digits.slice(split)}`
}

/**
 * Parse a JSON number token as an exact non-negative integer (`BigInt`).
 * Accepts whole integers (`"12"`, `"9007199254740993"`) and zero-fraction decimals (`"12.00"`).
 * Rejects negative numbers, non-integer fractions (`"1.5"`), and malformed tokens.
 */
export function parseExactNonNegInt(token: string): bigint | null {
  const dec = parseExactDecimal(token)
  if (dec === null || dec.coeff < 0n) return null
  if (dec.exp >= 0) {
    if (dec.exp > MAX_INT_EXP) return null
    return dec.coeff * 10n ** BigInt(dec.exp)
  }
  if (-dec.exp > MAX_INT_EXP) return null
  const factor = 10n ** BigInt(-dec.exp)
  if (dec.coeff % factor !== 0n) return null
  return dec.coeff / factor
}

/**
 * Total ascending preorder on `RateFigure`s (`< 0` if `a < b`, `> 0` if `a > b`, `0` if equal).
 * Missing/suppressed/null figures sort strictly below any concrete rate.
 * Every concrete rate maps consistently to `(exactDecimal, exactRational)` where rows with positive
 * integer totals use `numerator / denominator` and rows lacking totals use `exactDecimal` itself as
 * an exact rational — guaranteeing transitivity and permutation-invariance across mixed rows.
 * Never uses `plotPercent`.
 */
export function compareRateFigures(a: RateFigure | null, b: RateFigure | null): number {
  const aDec = a !== null && !a.suppressed && a.exact !== null ? parseExactDecimal(a.exact) : null
  const bDec = b !== null && !b.suppressed && b.exact !== null ? parseExactDecimal(b.exact) : null
  if (aDec === null && bDec === null) return 0
  if (aDec === null) return -1
  if (bDec === null) return 1
  const decCmp = compareExactDecimal(aDec, bDec)
  if (decCmp !== 0) return decCmp
  return compareExactRational(rationalOf(a!, aDec), rationalOf(b!, bDec))
}

const compareLawId = (a: string, b: string): number => (a < b ? -1 : a > b ? 1 : 0)

/** Strict total descending order on Misfit rows: override rate, exception rate, then law id. */
export function compareMisfitRows(a: MisfitLawRow, b: MisfitLawRow): number {
  return compareRateFigures(b.override, a.override)
    || compareRateFigures(b.exception, a.exception)
    || compareLawId(a.lawId, b.lawId)
}

/** Pick the KPI winner for override or exception rate using the exact same total order (highest rate, then law id). */
export function topRateRow(
  rows: readonly MisfitLawRow[],
  pickRate: (row: MisfitLawRow) => RateFigure | null,
): MisfitLawRow | undefined {
  let best: MisfitLawRow | undefined
  for (const row of rows) {
    const figure = pickRate(row)
    if (figure === null || figure.suppressed || figure.exact === null) continue
    if (best === undefined) {
      best = row
      continue
    }
    const cmp = compareRateFigures(figure, pickRate(best))
    if (cmp > 0 || (cmp === 0 && compareLawId(row.lawId, best.lawId) < 0)) {
      best = row
    }
  }
  return best
}

// ---- Strict envelope & row validation (S6) ------------------------------------------------------

const VALID_ENGINES = new Set<string>(["ivm", "cell", "duckdb", "datafusion"])

const isPlainObject = (v: unknown): v is Record<string, unknown> =>
  typeof v === "object" && v !== null && !Array.isArray(v)

/**
 * Validate every field of `NornQueryResult` consumed by the learning decoders so malformed answers
 * (such as missing `exclusions`, non-boolean `complete`, invalid `engine`, fractional/negative
 * `memberCount` or `nullCounts`, or contradictory D53 suppression fields) always return
 * `{ kind: "malformed" }` instead of throwing.
 */
function validateAnswerEnvelope(read: Read<Answer>): Answer {
  const value: unknown = read.document
  if (!isAnswer(value)) throw new DecodeError("answer did not match NornQueryResult")
  const doc = value as unknown as Record<string, unknown>
  if (typeof doc.complete !== "boolean") throw new DecodeError("/complete must be a boolean")
  if (typeof doc.effectiveAsOf !== "string" || doc.effectiveAsOf === "") throw new DecodeError("/effectiveAsOf must be a timestamp string")
  if (typeof doc.engine !== "string" || !VALID_ENGINES.has(doc.engine)) throw new DecodeError("/engine must be ivm, cell, duckdb, or datafusion")
  if (typeof doc.knownAsOf !== "number" || !Number.isSafeInteger(doc.knownAsOf) || doc.knownAsOf < 0) throw new DecodeError("/knownAsOf must be a non-negative integer")
  if (typeof doc.nowMs !== "number" || !Number.isSafeInteger(doc.nowMs) || doc.nowMs < 0) throw new DecodeError("/nowMs must be a non-negative integer")
  if (!isPlainObject(doc.exclusions)) throw new DecodeError("/exclusions must be an object")
  for (const [id, pin] of Object.entries(doc.exclusions)) {
    if (typeof pin !== "string") throw new DecodeError(`/exclusions/${id} must be a string`)
  }
  const rows = doc.rows as Record<string, Record<string, unknown>>
  for (const [key, row] of Object.entries(rows)) {
    const rowPath = `/rows/${member(key)}`
    const rawSuppressed = row.suppressed
    if (rawSuppressed !== undefined && typeof rawSuppressed !== "boolean") {
      throw new DecodeError(`/rows/${key}/suppressed must be a boolean`)
    }
    const totals = row.totals as unknown[]
    if (!totals.every((t) => typeof t === "number" && Number.isFinite(t))) {
      throw new DecodeError(`/rows/${key}/totals must contain only finite numbers`)
    }
    if (rawSuppressed === true && (row.value !== null || totals.length !== 0)) {
      throw new DecodeError(`/rows/${key} is suppressed but carries a value or totals`)
    }
    const rawMemberCount = row.memberCount
    if (rawMemberCount !== undefined && rawMemberCount !== null) {
      if (typeof rawMemberCount !== "number" || !Number.isFinite(rawMemberCount)) {
        throw new DecodeError(`/rows/${key}/memberCount must be a non-negative integer`)
      }
      const token = read.numbers.get(`${rowPath}/memberCount`) ?? String(rawMemberCount)
      const parsed = parseExactNonNegInt(token)
      if (parsed === null) {
        throw new DecodeError(`/rows/${key}/memberCount must be a non-negative integer`)
      }
      const listedMembers = BigInt(Object.keys(row.members as Record<string, unknown>).length)
      if (parsed < listedMembers) {
        throw new DecodeError(`/rows/${key}/memberCount is smaller than /rows/${key}/members`)
      }
    }
    const rawNullCounts = row.nullCounts
    if (rawNullCounts !== undefined) {
      if (!Array.isArray(rawNullCounts)) {
        throw new DecodeError(`/rows/${key}/nullCounts must be a list of non-negative integers`)
      }
      for (let i = 0; i < rawNullCounts.length; i += 1) {
        const entry = rawNullCounts[i]
        if (typeof entry !== "number" || !Number.isFinite(entry)) {
          throw new DecodeError(`/rows/${key}/nullCounts/${i} must be a non-negative integer`)
        }
        const token = read.numbers.get(`${rowPath}/nullCounts/${i}`) ?? String(entry)
        if (parseExactNonNegInt(token) === null) {
          throw new DecodeError(`/rows/${key}/nullCounts/${i} must be a non-negative integer`)
        }
      }
    }
  }
  return value as Answer
}

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

function rowCountOf(read: Read<Answer>, key: string, row: Answer["rows"][string]): bigint {
  const path = `/rows/${member(key)}`
  const rawCount = (row as { memberCount?: unknown }).memberCount
  if (rawCount !== undefined && rawCount !== null) {
    const token = read.numbers.get(`${path}/memberCount`) ?? String(rawCount)
    const parsed = parseExactNonNegInt(token)
    if (parsed === null) throw new DecodeError(`${path}/memberCount is not a non-negative integer`)
    return parsed
  }
  return BigInt(Object.keys(row.members).length)
}

function rateOf(read: Read<Answer>, key: string, row: Answer["rows"][string]): { readonly figure: RateFigure; readonly denominatorInt: bigint | null } {
  const suppressed = (row as { suppressed?: unknown }).suppressed === true
  if (suppressed) {
    return {
      figure: { exact: null, percent: null, plotPercent: null, numerator: null, denominator: null, suppressed: true },
      denominatorInt: null,
    }
  }
  const path = `/rows/${member(key)}`
  if (row.totals.length !== 0 && row.totals.length !== 2) {
    throw new DecodeError(`${path}/totals must have 0 or 2 entries for a rate`)
  }
  let numerator: string | null = null
  let denominator: string | null = null
  let denominatorInt: bigint | null = null
  if (row.totals.length === 2) {
    const numToken = read.numbers.get(`${path}/totals/0`) ?? String(row.totals[0])
    const denToken = read.numbers.get(`${path}/totals/1`) ?? String(row.totals[1])
    const numInt = parseExactNonNegInt(numToken)
    const denInt = parseExactNonNegInt(denToken)
    if (numInt === null) throw new DecodeError(`${path}/totals/0 is not a non-negative integer count`)
    if (denInt === null) throw new DecodeError(`${path}/totals/1 is not a non-negative integer count`)
    numerator = numInt.toString()
    denominator = denInt.toString()
    denominatorInt = denInt
  }

  let exact: string | null = null
  let percentText: string | null = null
  let plotPercent: number | null = null
  if (row.value === null) {
    if (denominatorInt !== null && denominatorInt > 0n) {
      throw new DecodeError(`${path}/value is null on a non-suppressed row with positive denominator`)
    }
  } else {
    if (denominatorInt !== null && denominatorInt === 0n) {
      throw new DecodeError(`${path}/value must be null when denominator is zero`)
    }
    const rawValue = read.numbers.get(`${path}/value`) ?? String(row.value)
    const dec = parseExactDecimal(rawValue)
    if (dec === null || dec.coeff < 0n) throw new DecodeError(`${path}/value is not a non-negative decimal rate`)
    exact = rawValue
    percentText = percent(plainDecimalText(dec))
    // Numeric conversion kept strictly for Recharts chart positioning.
    const num = Number(rawValue)
    plotPercent = Number.isFinite(num) ? Math.round(num * 1000) / 10 : null
  }

  return {
    figure: { exact, percent: percentText, plotPercent, numerator, denominator, suppressed: false },
    denominatorInt,
  }
}

/**
 * Decode `learning.misfit_by_law` (`NornQueryResult`) fail-closed.
 *
 * Accepts rows grouped by `[lawId, "override" | "exception"]` (both rates per law) or single-key
 * `lawId` (combined override/misfit rate). Any other bucket shape, duplicate `(lawId, signal)`,
 * malformed envelope, or invalid count/rate fails closed.
 */
export function decodeMisfitByLaw(read: Read<Answer>): { readonly kind: "ok"; readonly data: MisfitBoardData } | { readonly kind: "malformed"; readonly problem: string } {
  try {
    const answer = validateAnswerEnvelope(read)
    const byLaw = new Map<string, { override: RateFigure | null; exception: RateFigure | null; episodes: bigint | null; members: Set<string>; suppressed: boolean }>()
    for (const [key, row] of Object.entries(answer.rows)) {
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
      const { figure, denominatorInt } = rateOf(read, key, row)
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
      const episodes = denominatorInt ?? rowCountOf(read, key, row)
      entry.episodes = entry.episodes === null || episodes > entry.episodes ? episodes : entry.episodes
      byLaw.set(lawId, entry)
    }
    const rows: MisfitLawRow[] = [...byLaw.entries()]
      .map(([lawId, entry]) => ({
        lawId,
        override: entry.override,
        exception: entry.exception,
        episodeCount: entry.episodes === null ? null : entry.episodes.toString(),
        memberIds: [...entry.members].sort(),
        anySuppressed: entry.suppressed,
      }))
      // Strict total order by exact decimal/rational key, then deterministic law id — never rounded chart values.
      .sort(compareMisfitRows)
    return { kind: "ok", data: { rows, meta: metaOf(answer) } }
  } catch (error) {
    if (error instanceof DecodeError) return { kind: "malformed", problem: error.message }
    throw error
  }
}

const UNKNOWN_COUNT: CountFigure = { status: "unknown", exact: null, plot: null, suppressed: false }
const ISO_DATE_ONLY = /^\d{4}-\d{2}-\d{2}$/

function isValidIsoWeekStart(text: string): boolean {
  if (!ISO_DATE_ONLY.test(text)) return false
  const parsed = Date.parse(`${text}T00:00:00Z`)
  if (Number.isNaN(parsed)) return false
  return new Date(parsed).toISOString().slice(0, 10) === text
}

/**
 * Decode `learning.triage_over_time` (`NornQueryResult`) fail-closed.
 *
 * Each row's bucket is `[weekStart, verdict]` where `verdict` is one of `exception`, `noise`,
 * `pattern`, `gap`. Duplicate `(weekStart, verdict)` buckets (including suppressed rows) and
 * invalid/non-integer counts fail closed. Missing verdict buckets are represented as `unknown`
 * (never coerced to zero) unless the server explicitly returns a `0` bucket.
 */
export function decodeTriageOverTime(read: Read<Answer>): { readonly kind: "ok"; readonly data: TriageOverTimeData } | { readonly kind: "malformed"; readonly problem: string } {
  try {
    const answer = validateAnswerEnvelope(read)
    const byWeek = new Map<string, Partial<Record<Verdict, { readonly figure: CountFigure; readonly exactInt: bigint | null }>>>()
    for (const [key, row] of Object.entries(answer.rows)) {
      const b = row.bucket
      if (!Array.isArray(b) || b.length !== 2 || typeof b[0] !== "string" || !isValidIsoWeekStart(b[0]) || !VERDICTS.includes(b[1] as Verdict)) {
        throw new DecodeError(`/rows/${key}/bucket must be [YYYY-MM-DD, verdict]`)
      }
      const weekStart = b[0]
      const verdict = b[1] as Verdict
      const week = byWeek.get(weekStart) ?? {}
      if (week[verdict] !== undefined) {
        throw new DecodeError(`duplicate triage bucket for ${weekStart} / ${verdict}`)
      }
      const suppressed = (row as { suppressed?: unknown }).suppressed === true
      const path = `/rows/${member(key)}`
      if (suppressed) {
        week[verdict] = { figure: { status: "suppressed", exact: null, plot: null, suppressed: true }, exactInt: null }
      } else {
        if (row.value === null) throw new DecodeError(`/rows/${key}/value is null on a non-suppressed count`)
        const rawValue = read.numbers.get(`${path}/value`) ?? String(row.value)
        const count = parseExactNonNegInt(rawValue)
        if (count === null) throw new DecodeError(`/rows/${key}/value is not a non-negative integer count`)
        if (row.totals.length > 1) {
          throw new DecodeError(`/rows/${key}/totals must have 0 or 1 entry for a count`)
        }
        if (row.totals.length === 1) {
          const totalToken = read.numbers.get(`${path}/totals/0`) ?? String(row.totals[0])
          const totalInt = parseExactNonNegInt(totalToken)
          if (totalInt === null) throw new DecodeError(`/rows/${key}/totals/0 is not a non-negative integer count`)
          if (totalInt !== count) {
            throw new DecodeError(`/rows/${key}/totals/0 contradicts /rows/${key}/value`)
          }
        }
        const plot = Number(count)
        week[verdict] = {
          figure: { status: "value", exact: count.toString(), plot: Number.isFinite(plot) ? plot : null, suppressed: false },
          exactInt: count,
        }
      }
      byWeek.set(weekStart, week)
    }

    const weeks: TriageWeekRow[] = [...byWeek.entries()]
      .sort(([a], [b]) => a.localeCompare(b))
      .map(([weekStart, entries]) => {
        const counts: Record<Verdict, CountFigure> = {
          exception: entries.exception?.figure ?? UNKNOWN_COUNT,
          noise: entries.noise?.figure ?? UNKNOWN_COUNT,
          pattern: entries.pattern?.figure ?? UNKNOWN_COUNT,
          gap: entries.gap?.figure ?? UNKNOWN_COUNT,
        }
        const figures = VERDICTS.map((v) => counts[v])
        const anySuppressed = figures.some((c) => c.status === "suppressed")
        const anyUnknown = figures.some((c) => c.status === "unknown")
        const total = anySuppressed || anyUnknown
          ? null
          : VERDICTS.reduce((sum, v) => sum + (entries[v]?.exactInt ?? 0n), 0n).toString()
        return { weekStart, label: day(`${weekStart}T00:00:00Z`), counts, total, anySuppressed, anyUnknown }
      })
    return { kind: "ok", data: { weeks, meta: metaOf(answer) } }
  } catch (error) {
    if (error instanceof DecodeError) return { kind: "malformed", problem: error.message }
    throw error
  }
}

async function readQuestion<T>(
  questionName: string,
  decode: (read: Read<Answer>) => { readonly kind: "ok"; readonly data: T } | { readonly kind: "malformed"; readonly problem: string },
  fetcher: Fetch = (input, init) => fetch(input, init),
  options: ScopedReadOptions = {},
): Promise<LearningQuestionRead<T>> {
  let response: Response
  let raw: string
  try {
    const resolved = await resolveBoundLearningScope(fetcher, options)
    if (resolved.kind !== "ok") return resolved
    const scoped = scopedHostFetch(fetcher, "/api/host", async () => resolved.scope)
    response = await scoped(
      `/v1/questions/${encodeURIComponent(questionName)}/answer`,
      { headers: { accept: "application/json" }, ...(options.signal === undefined ? {} : { signal: options.signal }) },
    )
    raw = await response.text()
  } catch (error) {
    if (options.signal?.aborted) throw error
    if (error instanceof StudioScopeError && error.message === NO_DATA_CELL) return { kind: "no-cell" }
    if (error instanceof CellUnavailable) return { kind: "unavailable" }
    throw error
  }
  if (options.signal?.aborted) return { kind: "scope-changed" }
  if (response.ok) {
    let read: Read<Answer>
    try { read = readDocument<Answer>(raw, "live") } catch { return { kind: "malformed", problem: "The cell's question answer was not JSON." } }
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

export const readMisfitByLaw = (fetcher?: Fetch, options?: ScopedReadOptions): Promise<LearningQuestionRead<MisfitBoardData>> =>
  readQuestion(MISFIT_BY_LAW_QUESTION, decodeMisfitByLaw, fetcher, options)

export const readTriageOverTime = (fetcher?: Fetch, options?: ScopedReadOptions): Promise<LearningQuestionRead<TriageOverTimeData>> =>
  readQuestion(TRIAGE_OVER_TIME_QUESTION, decodeTriageOverTime, fetcher, options)
