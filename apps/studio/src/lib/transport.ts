import type { Schemas } from "../../../../packages/norn-sdk/src/index.ts"
import { readDocument, type Read } from "./exact.ts"

export type Question = Schemas["NornIrQuestion"]
export type Answer = Schemas["NornQueryResult"]
export type HistoricalAnswer = Schemas["NornHistoricalQuestionResult"]
export type Metric = Schemas["NornIrMetricDefinition"]
export type ObjectType = Schemas["NornIrObjectTypeDef"]
export type Artifact = Schemas["NornArtifactEnvelope"]

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
  /** The admitted definitions the cell serves: concepts, relationships, laws and source mappings. */
  readonly catalog: () => Promise<Read<readonly Artifact[]>>
}

const recordings = import.meta.glob<string>(["../../../../fixtures/demo/answers/*.json", "../../../../fixtures/demo/explorer/*.json"], { query: "?raw", import: "default", eager: true })

function recorded<T>(name: string): Read<T> {
  const source = recordings[`../../../../fixtures/demo/${name}`]
  if (source === undefined) throw new Error(`No recorded document ${name}`)
  return readDocument<T>(source, "recorded")
}

const scrapYield = (): QuestionAnswer => ({
  question: recorded<Question>("answers/scrap-yield.question.json"),
  answer: recorded<Answer>("answers/scrap-yield.k1.answer.json"),
  metrics: recorded<readonly Metric[]>("answers/scrap-yield.metrics.json"),
  objectType: recorded<ObjectType>("answers/scrap-yield.object-type.json"),
})

/** Documents the engine produced for the synthetic scrap-yield and three-systems worlds, replayed byte for byte. */
export const recordedTransport: Transport = {
  questionBoard: async () => [scrapYield()],
  restatement: async () => ({
    ...scrapYield(),
    corrected: recorded<Answer>("answers/scrap-yield.k2.answer.json"),
    asOf: recorded<HistoricalAnswer>("answers/scrap-yield.k1-as-of.answer.json"),
  }),
  catalog: async () => recorded<readonly Artifact[]>("explorer/three-systems.definitions.json"),
}
