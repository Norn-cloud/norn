import { member } from "./exact.ts"
import { bucketLabel, percent } from "./format.ts"
import type { Answer, Metric, ObjectType, Question, QuestionAnswer } from "./transport.ts"
import { measured, operandUnits } from "./units.ts"

/** One output bucket with every number as the exact text the server sent. */
/** What a bucket the server suppressed reads as, everywhere: never a blank or a 0 (D53, #1015). */
export const SUPPRESSED = "Suppressed: fewer than 3 records"

export interface Bucket {
  readonly key: string
  readonly label: string
  /** The server's exact value, or `SUPPRESSED`. */
  readonly value: string
  /**
   * The server suppressed this bucket for this reader: fewer than 3 records,
   * and the reader lacks Values on one of them. It sent no value and no
   * totals; its members are still listed.
   */
  readonly suppressed?: boolean
  /** One sum, or numerator then denominator, as the server wrote them, each with its declared unit. */
  readonly totals: readonly string[]
  readonly members: readonly {
    readonly id: string
    /** The object version the server counted. */
    readonly version: number
    /** Each operand's exact text; `null` when the server withheld this record's amounts from the reader (DATA-VALUES-1). */
    readonly amounts: readonly (string | null)[] | null
  }[]
}

/** An answer beside the exact number texts of the document that carries it, and where it sits in that document. */
export interface ExactAnswer {
  readonly answer: Answer
  readonly numbers: ReadonlyMap<string, string>
  /** RFC 6901 pointer to the answer inside its document: `""`, or `/result` for an as-of read. */
  readonly at: string
}

export interface Declarations {
  readonly question: Question
  readonly metrics: readonly Metric[]
  readonly objectType: ObjectType
}

export const declarations = ({ question, metrics, objectType }: QuestionAnswer): Declarations =>
  ({ question: question.document, metrics: metrics.document, objectType: objectType.document })

export function buckets({ question, metrics, objectType }: Declarations, { answer, numbers, at }: ExactAnswer): Bucket[] {
  const units = operandUnits(question, metrics, objectType)
  const text = (pointer: string) => numbers.get(`${at}${pointer}`) ?? "unavailable"
  const operand = (pointer: string, index: number) => measured(text(pointer), units[index])
  return Object.entries(answer.rows).map(([key, row]) => {
    const path = `/rows/${member(key)}`
    // D53: `{ suppressed: true, value: null, totals: [] }`; nothing is computed in its place.
    const suppressed = (row as { suppressed?: unknown }).suppressed === true
    return {
      key,
      label: bucketLabel(row.bucket, question),
      value: suppressed ? SUPPRESSED : text(`${path}/value`),
      suppressed,
      totals: suppressed ? [] : row.totals.map((_, index) => operand(`${path}/totals/${index}`, index)),
      members: Object.entries(row.members).map(([id, { version, amounts }]) => ({
        id,
        version,
        // The server sends `amounts: null` for a member whose values the reader may not see.
        amounts: (amounts as readonly (number | null)[] | null) === null ? null : amounts.map((amount, index) => amount === null ? null : operand(`${path}/members/${member(id)}/amounts/${index}`, index)),
      })),
    }
  })
}

export const fraction = (totals: readonly (string | null)[]): string => totals.map((total) => total ?? "left out").join(" / ")

/** A member's contribution in words: its exact amounts, or "Withheld" when the server sent none for this reader. */
export const contribution = (amounts: readonly (string | null)[] | null): string => amounts === null ? "Withheld" : fraction(amounts)

/** A ratio reads as a percentage at one precision everywhere; a sum stays the server's text. */
export const shown = (bucket: Pick<Bucket, "value" | "totals">): string => bucket.totals.length > 1 ? percent(bucket.value) : bucket.value
