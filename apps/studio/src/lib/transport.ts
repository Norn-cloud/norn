import type { Schemas } from "../../../../packages/norn-sdk/src/index.ts"
import { readDocument, type Read } from "./exact.ts"
import { currentMode, liveTransport, type ObjectCounts } from "./live-transport.ts"
import { readsOf } from "./objects.ts"
import type { CountedLinkPage } from "./record-page.ts"

export type { Schemas }
export type Envelope = Schemas["NornArtifactEnvelope"]
export type Question = Schemas["NornIrQuestion"]
export type Answer = Schemas["NornQueryResult"]
export type HistoricalAnswer = Schemas["NornHistoricalQuestionResult"]
export type Metric = Schemas["NornIrMetricDefinition"]
export type ObjectType = Schemas["NornIrObjectTypeDef"]
export type Artifact = Schemas["NornArtifactEnvelope"]
export type Guarantees = Schemas["NornGuaranteesDocument"]
export type ObjectPage = Schemas["NornObjectPage"]
export type ObjectRecord = Schemas["NornObjectRecord"]
export type LinkPage = Schemas["NornLinkPage"]
export type SourceLineage = Schemas["NornInstanceSourceProvenance"]

/** The reads behind one object: itself, the links touching it and where it came from. */
export interface ObjectReads {
  readonly object: Read<ObjectRecord>
  /** The first page of its links, with the server's counts by link type when the cell counts them. */
  readonly links: Read<CountedLinkPage>
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
  /** Each object type's record count for this caller, as the cell counted it: none for a type it may not list, and none recorded. */
  readonly objectCounts: () => Promise<ObjectCounts>
  /** The guarantees the model checker recorded for a law, one document per checked definition, keyed by recording name. */
  readonly guarantees: () => Promise<readonly { readonly name: string; readonly read: Read<Guarantees> }[]>
  /** One page of a concept's authoritative objects after `after`; `null` when the server has no such read yet. */
  readonly objectPage: (objectType: string, after?: string) => Promise<Read<ObjectPage> | null>
  /** One object with its links and source lineage; `null` when it is not readable. */
  readonly object: (objectType: string, id: string) => Promise<ObjectReads | null>
}

// Called, not evaluated at import: only Vite provides import.meta.glob, and tests import this module through the pages.
const recordings = () => import.meta.glob<string>(["../../../../fixtures/demo/answers/*.json", "../../../../fixtures/demo/explorer/**/*.json", "../../../../fixtures/demo/guarantees/*.json"], { query: "?raw", import: "default", eager: true })

const optional = <T,>(name: string): Read<T> | null =>
  recordings()[`../../../../fixtures/demo/${name}`] === undefined ? null : recorded<T>(name)

/** Recorded object documents are named by the object's id with `/` written as `~`. */
const objectFile = (objectType: string, id: string, read: string) => `explorer/objects/${objectType}/${id.replaceAll("/", "~")}.${read}.json`

function recorded<T>(name: string): Read<T> {
  const source = recordings()[`../../../../fixtures/demo/${name}`]
  if (source === undefined) throw new Error(`No recorded document ${name}`)
  return readDocument<T>(source, "recorded")
}

const DEFINITIONS = () => import.meta.glob<string>("../../../../registry/core/artifacts/*.json", { query: "?raw", import: "default", eager: true })

/**
 * The one recorded question's document of `kind` (`question.json`, `k1.answer.json`, …), whatever
 * the question is named: Studio names no recorded world in its source, so exactly one document in
 * `fixtures/demo/answers` must end in `.<kind>`.
 */
function recordedAnswer<T>(kind: string): Read<T> {
  const answers = "../../../../fixtures/demo/answers/"
  // `<question>.<kind>`, where the question's own name has no dot: `k1.answer.json` never matches `….k1-as-of.answer.json`.
  const names = Object.keys(recordings()).filter((path) => {
    if (!path.startsWith(answers) || !path.endsWith(`.${kind}`)) return false
    const question = path.slice(answers.length, -kind.length - 1)
    return question !== "" && !question.includes(".")
  })
  if (names.length !== 1) throw new Error(`Expected one recorded ${kind}, found ${names.length}`)
  return recorded<T>(`answers/${names[0]!.slice(answers.length)}`)
}

const recordedQuestion = (): QuestionAnswer => ({
  question: recordedAnswer<Question>("question.json"),
  answer: recordedAnswer<Answer>("k1.answer.json"),
  metrics: recordedAnswer<readonly Metric[]>("metrics.json"),
  objectType: recordedAnswer<ObjectType>("object-type.json"),
})

/** Documents the engine produced for the synthetic recorded question and three-systems worlds, replayed byte for byte. */
export const recordedTransport: Transport = {
  definitions: async () => Object.values(DEFINITIONS()).map((source) => readDocument<Envelope>(source, "recorded")),
  questionBoard: async () => [recordedQuestion()],
  restatement: async () => ({
    ...recordedQuestion(),
    corrected: recordedAnswer<Answer>("k2.answer.json"),
    asOf: recordedAnswer<HistoricalAnswer>("k1-as-of.answer.json"),
  }),
  catalog: async () => recorded<readonly Artifact[]>("explorer/three-systems.definitions.json"),
  objectCounts: async () => ({}),
  guarantees: async () => ["original", "mutant"].map((name) => ({ name, read: recorded<Guarantees>(`guarantees/constitution.${name}.guarantees.json`) })),
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

const through = (pick: (transport: Transport) => (...args: never[]) => Promise<unknown>) =>
  async (...args: never[]) => {
    const active = await currentMode()
    const chosen = active.kind === "live" ? liveTransport(active.catalog, (input, init) => fetch(input, init), active.counts, active.scope) : recordedTransport
    return pick(chosen)(...args)
  }

/**
 * The transport the pages read through: the connected cell's documents, or the
 * recording when no cell is connected, decided once per page load (live-transport.ts `readMode`).
 */
export const transport: Transport = {
  questionBoard: through((t) => t.questionBoard) as Transport["questionBoard"],
  definitions: through((t) => t.definitions) as Transport["definitions"],
  restatement: through((t) => t.restatement) as Transport["restatement"],
  catalog: through((t) => t.catalog) as Transport["catalog"],
  objectCounts: through((t) => t.objectCounts) as Transport["objectCounts"],
  guarantees: through((t) => t.guarantees) as Transport["guarantees"],
  objectPage: through((t) => t.objectPage) as Transport["objectPage"],
  object: through((t) => t.object) as Transport["object"],
}

