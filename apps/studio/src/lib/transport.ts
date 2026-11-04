import type { Schemas } from "../../../../packages/norn-sdk/src/index.ts"
import { readDocument, type Read } from "./exact.ts"

export type Question = Schemas["NornIrQuestion"]
export type Answer = Schemas["NornQueryResult"]
export type Artifact = Schemas["NornArtifactEnvelope"]

export interface QuestionAnswer {
  readonly question: Read<Question>
  readonly answer: Read<Answer>
}

/** The app's only way to read server documents. Live HTTP joins once the host's read API lands. */
export interface Transport {
  readonly questionBoard: () => Promise<readonly QuestionAnswer[]>
  /** The admitted definitions the cell serves: concepts, relationships, laws and source mappings. */
  readonly catalog: () => Promise<Read<readonly Artifact[]>>
}

const recordings = import.meta.glob<string>(["../../../../fixtures/demo/answers/*.json", "../../../../fixtures/demo/explorer/*.json"], { query: "?raw", import: "default", eager: true })

function recorded<T>(name: string): Read<T> {
  const source = recordings[`../../../../fixtures/demo/${name}`]
  if (source === undefined) throw new Error(`No recorded document ${name}`)
  return readDocument<T>(source, "recorded")
}

/** Documents the engine produced for the synthetic scrap-yield and three-systems worlds, replayed byte for byte. */
export const recordedTransport: Transport = {
  questionBoard: async () => [{
    question: recorded<Question>("answers/scrap-yield.question.json"),
    answer: recorded<Answer>("answers/scrap-yield.k1.answer.json"),
  }],
  catalog: async () => recorded<readonly Artifact[]>("explorer/three-systems.definitions.json"),
}
