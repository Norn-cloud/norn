import type { Schemas } from "../../../../packages/norn-sdk/src/index.ts"
import { readDocument, type Read } from "./exact.ts"
import { readsOf } from "./objects.ts"

export type { Schemas }
export type Envelope = Schemas["NornArtifactEnvelope"]
export type Question = Schemas["NornIrQuestion"]
export type Answer = Schemas["NornQueryResult"]
export type HistoricalAnswer = Schemas["NornHistoricalQuestionResult"]
export type Metric = Schemas["NornIrMetricDefinition"]
export type ObjectType = Schemas["NornIrObjectTypeDef"]
export type Artifact = Schemas["NornArtifactEnvelope"]
export type ObjectPage = Schemas["NornObjectPage"]
export type ObjectRecord = Schemas["NornObjectRecord"]
export type LinkPage = Schemas["NornLinkPage"]
export type SourceLineage = Schemas["NornInstanceSourceProvenance"]

/** The reads behind one object: itself, the links touching it and where it came from. */
export interface ObjectReads {
  readonly object: Read<ObjectRecord>
  readonly links: Read<LinkPage>
  /** `null` for an object no mapping produced. */
  readonly source: Read<SourceLineage> | null
}

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
  /** The admitted core definitions: object types, actions, laws and the rest, as artifact envelopes. */
  readonly definitions: () => Promise<readonly Read<Envelope>[]>
  readonly restatement: () => Promise<Restatement>
  /** The admitted definitions the cell serves: concepts, relationships, laws and source mappings. */
  readonly catalog: () => Promise<Read<readonly Artifact[]>>
  /** One page of a concept's authoritative objects after `after`; `null` when the server has no such read yet. */
  readonly objectPage: (objectType: string, after?: string) => Promise<Read<ObjectPage> | null>
  /** One object with its links and source lineage; `null` when it is not readable. */
  readonly object: (objectType: string, id: string) => Promise<ObjectReads | null>
}

const recordings = import.meta.glob<string>(["../../../../fixtures/demo/answers/*.json", "../../../../fixtures/demo/explorer/**/*.json"], { query: "?raw", import: "default", eager: true })

const optional = <T,>(name: string): Read<T> | null =>
  recordings[`../../../../fixtures/demo/${name}`] === undefined ? null : recorded<T>(name)

/** Recorded object documents are named by the object's id with `/` written as `~`. */
const objectFile = (objectType: string, id: string, read: string) => `explorer/objects/${objectType}/${id.replaceAll("/", "~")}.${read}.json`

function recorded<T>(name: string): Read<T> {
  const source = recordings[`../../../../fixtures/demo/${name}`]
  if (source === undefined) throw new Error(`No recorded document ${name}`)
  return readDocument<T>(source, "recorded")
}

const DEFINITIONS = import.meta.glob<string>("../../../../registry/core/artifacts/*.json", { query: "?raw", import: "default", eager: true })

const scrapYield = (): QuestionAnswer => ({
  question: recorded<Question>("answers/scrap-yield.question.json"),
  answer: recorded<Answer>("answers/scrap-yield.k1.answer.json"),
  metrics: recorded<readonly Metric[]>("answers/scrap-yield.metrics.json"),
  objectType: recorded<ObjectType>("answers/scrap-yield.object-type.json"),
})

/** Documents the engine produced for the synthetic scrap-yield and three-systems worlds, replayed byte for byte. */
export const recordedTransport: Transport = {
  definitions: async () => Object.values(DEFINITIONS).map((source) => readDocument<Envelope>(source, "recorded")),
  questionBoard: async () => [scrapYield()],
  restatement: async () => ({
    ...scrapYield(),
    corrected: recorded<Answer>("answers/scrap-yield.k2.answer.json"),
    asOf: recorded<HistoricalAnswer>("answers/scrap-yield.k1-as-of.answer.json"),
  }),
  catalog: async () => recorded<readonly Artifact[]>("explorer/three-systems.definitions.json"),
  objectPage: async (objectType, after) =>
    optional<ObjectPage>(`explorer/objects/${objectType}${after === undefined ? "" : `.after.${after.replaceAll("/", "~")}`}.page.json`),
  object: async (objectType, id) => {
    const object = optional<ObjectRecord>(objectFile(objectType, id, "object"))
    const links = optional<LinkPage>(objectFile(objectType, id, "links"))
    const source = optional<SourceLineage>(objectFile(objectType, id, "source"))
    // Recorded file names fold `/` into `~`, so a read must match the identity it was asked for.
    return object && links && readsOf(objectType, id, object.document, links.document) ? { object, links, source } : null
  },
}
