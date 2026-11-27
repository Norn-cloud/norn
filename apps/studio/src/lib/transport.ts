import type { Schemas } from "../../../../packages/norn-sdk/src/index.ts"
import type { Read } from "./exact.ts"
import { currentMode, liveTransport, type ObjectCounts } from "./live-transport.ts"
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

/**
 * The model documents the pages read: the connected cell's, or the recording when no cell is connected.
 * Records are not read here: a page reads them through its decided workspace's reader
 * (`lib/workspace.tsx`), so no record is read without a decided workspace (#1762).
 */
export interface Transport {
  readonly questionBoard: () => Promise<readonly QuestionAnswer[]>
  /** The admitted core definitions: object types, actions, laws and the rest, as artifact envelopes. */
  readonly definitions: () => Promise<readonly Read<Envelope>[]>
  readonly restatement: () => Promise<Restatement>
  /** The admitted definitions the cell serves: concepts, relationships, laws and source mappings. */
  readonly catalog: () => Promise<Read<readonly Artifact[]>>
  /**
   * Studio's built-in pages the cell serves beside its definitions, each only while the workspace
   * installed none with its id (`builtins` on `GET /v1/definitions`). The recording serves none: its
   * catalog holds its pages.
   */
  readonly builtins: () => Promise<readonly Envelope[]>
  /** Each object type's record count for this caller, as the cell counted it: none for a type it may not list, and none recorded. */
  readonly objectCounts: () => Promise<ObjectCounts>
  /** The guarantees the model checker recorded for a law, one document per checked definition, keyed by recording name. */
  readonly guarantees: () => Promise<readonly { readonly name: string; readonly read: Read<Guarantees> }[]>
}

let recordedPromise: Promise<Transport> | null = null
const loadRecorded = (): Promise<Transport> =>
  (recordedPromise ??= import("./raw/recorded-transport.ts")
    .then((module) => module.recordedTransportImpl)
    .catch((error) => {
      recordedPromise = null
      throw error
    }))

const through = (pick: (transport: Transport) => (...args: never[]) => Promise<unknown>) =>
  async (...args: never[]) => {
    const active = await currentMode()
    const chosen = active.kind === "live"
      ? liveTransport(active.catalog, active.counts, active.builtins)
      : await loadRecorded()
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
  builtins: through((t) => t.builtins) as Transport["builtins"],
  objectCounts: through((t) => t.objectCounts) as Transport["objectCounts"],
  guarantees: through((t) => t.guarantees) as Transport["guarantees"],
}

