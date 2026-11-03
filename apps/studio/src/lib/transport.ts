import type { Schemas } from "../../../../packages/norn-sdk/src/index.ts"
import { readDocument, type Read } from "./exact.ts"

export type Question = Schemas["NornIrQuestion"]
export type Answer = Schemas["NornQueryResult"]
export type HistoricalAnswer = Schemas["NornHistoricalQuestionResult"]
export type Metric = Schemas["NornIrMetricDefinition"]
export type ObjectType = Schemas["NornIrObjectTypeDef"]

export interface QuestionAnswer {
  readonly question: Read<Question>
  readonly answer: Read<Answer>
  /** The metric declarations the question was admitted with. */
  readonly metrics: Read<readonly Metric[]>
  /** The object type the answer's records belong to, with each property's unit. */
  readonly objectType: Read<ObjectType>
}

/** One answer published, the same question answered again after a source correction, and the first answer read back at its knowledge cutoff. */
export interface Restatement extends QuestionAnswer {
  readonly corrected: Read<Answer>
  readonly asOf: Read<HistoricalAnswer>
}

/** The app's only way to read server documents. Live HTTP joins once the host's read API lands. */
export interface Transport {
  readonly questionBoard: () => Promise<readonly QuestionAnswer[]>
  readonly restatement: () => Promise<Restatement>
}

const RECORDINGS = "../../../../fixtures/demo/answers/"
const recordings = import.meta.glob<string>("../../../../fixtures/demo/answers/*.json", { query: "?raw", import: "default", eager: true })

function recorded<T>(name: string): Read<T> {
  const source = recordings[RECORDINGS + name]
  if (source === undefined) throw new Error(`No recorded document ${name}`)
  return readDocument<T>(source, "recorded")
}

const scrapYield = (): QuestionAnswer => ({
  question: recorded<Question>("scrap-yield.question.json"),
  answer: recorded<Answer>("scrap-yield.k1.answer.json"),
  metrics: recorded<readonly Metric[]>("scrap-yield.metrics.json"),
  objectType: recorded<ObjectType>("scrap-yield.object-type.json"),
})

/** Documents the engine produced for the synthetic scrap-yield world, replayed byte for byte. */
export const recordedTransport: Transport = {
  questionBoard: async () => [scrapYield()],
  restatement: async () => ({
    ...scrapYield(),
    corrected: recorded<Answer>("scrap-yield.k2.answer.json"),
    asOf: recorded<HistoricalAnswer>("scrap-yield.k1-as-of.answer.json"),
  }),
}
