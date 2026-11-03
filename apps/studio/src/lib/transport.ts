import type { Schemas } from "../../../../packages/norn-sdk/src/index.ts"
import { readDocument, type Read } from "./exact.ts"

export type Question = Schemas["NornIrQuestion"]
export type Answer = Schemas["NornQueryResult"]
export type Artifact = Schemas["NornArtifactEnvelope"]
export type ObjectPage = Schemas["NornObjectPage"]
export type ObjectRecord = Schemas["NornObjectRecord"]
export type LinkPage = Schemas["NornLinkPage"]
export type SourceLineage = Schemas["NornInstanceSourceProvenance"]

/** The reads behind one object: itself, the links touching it and where it came from. */
export interface ObjectReads {
  readonly object: Read<ObjectRecord>
  readonly links: Read<LinkPage>
  readonly source: Read<SourceLineage>
}

export interface QuestionAnswer {
  readonly question: Read<Question>
  readonly answer: Read<Answer>
}

/** The app's only way to read server documents. Live HTTP joins once the host's read API lands. */
export interface Transport {
  readonly questionBoard: () => Promise<readonly QuestionAnswer[]>
  /** The admitted definitions the cell serves: concepts, relationships, laws and source mappings. */
  readonly catalog: () => Promise<Read<readonly Artifact[]>>
  /** One concept's authoritative objects; `null` when the server has no such read yet. */
  readonly objectPage: (objectType: string) => Promise<Read<ObjectPage> | null>
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

/** Documents the engine produced for the synthetic scrap-yield and three-systems worlds, replayed byte for byte. */
export const recordedTransport: Transport = {
  questionBoard: async () => [{
    question: recorded<Question>("answers/scrap-yield.question.json"),
    answer: recorded<Answer>("answers/scrap-yield.k1.answer.json"),
  }],
  catalog: async () => recorded<readonly Artifact[]>("explorer/three-systems.definitions.json"),
  objectPage: async (objectType) => optional<ObjectPage>(`explorer/objects/${objectType}.page.json`),
  object: async (objectType, id) => {
    const object = optional<ObjectRecord>(objectFile(objectType, id, "object"))
    const links = optional<LinkPage>(objectFile(objectType, id, "links"))
    const source = optional<SourceLineage>(objectFile(objectType, id, "source"))
    return object && links && source ? { object, links, source } : null
  },
}
