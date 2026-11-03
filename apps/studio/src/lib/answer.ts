import { member } from "./exact.ts"
import { bucketLabel } from "./format.ts"
import type { Answer, Metric, ObjectType, Question, QuestionAnswer } from "./transport.ts"
import { measured, operandUnits } from "./units.ts"

/** One output bucket with every number as the exact text the server sent. */
export interface Bucket {
  readonly key: string
  readonly label: string
  readonly value: string
  /** One sum, or numerator then denominator, as the server wrote them, each with its declared unit. */
  readonly totals: readonly string[]
  readonly members: readonly {
    readonly id: string
    /** The object version the server counted. */
    readonly version: number
    readonly amounts: readonly (string | null)[]
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
    return {
      key,
      label: bucketLabel(row.bucket, question),
      value: text(`${path}/value`),
      totals: row.totals.map((_, index) => operand(`${path}/totals/${index}`, index)),
      members: Object.entries(row.members).map(([id, { version, amounts }]) => ({
        id,
        version,
        amounts: amounts.map((amount, index) => amount === null ? null : operand(`${path}/members/${member(id)}/amounts/${index}`, index)),
      })),
    }
  })
}

export const fraction = (totals: readonly (string | null)[]): string => totals.map((total) => total ?? "left out").join(" / ")
