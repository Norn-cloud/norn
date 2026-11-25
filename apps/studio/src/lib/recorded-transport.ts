import { readDocument, type Read } from "./exact.ts"
import { readsOf } from "./record-name.ts"
import type {
  Answer,
  Artifact,
  Envelope,
  Guarantees,
  HistoricalAnswer,
  LinkPage,
  Metric,
  ObjectPage,
  ObjectRecord,
  ObjectType,
  Question,
  QuestionAnswer,
  SourceLineage,
  Transport,
} from "./transport.ts"

// Called, not evaluated at import: only Vite provides import.meta.glob, and tests may import transport without Vite.
const recordings = () => import.meta.glob<string>(
  [
    "../../../../fixtures/demo/answers/*.json",
    "../../../../fixtures/demo/explorer/**/*.json",
    "../../../../fixtures/demo/guarantees/*.json",
  ],
  { query: "?raw", import: "default", eager: true },
)

const optional = <T,>(name: string): Read<T> | null =>
  recordings()[`../../../../fixtures/demo/${name}`] === undefined ? null : recorded<T>(name)

/** Recorded object documents are named by the object's id with `/` written as `~`. */
const objectFile = (objectType: string, id: string, read: string) =>
  `explorer/objects/${objectType}/${id.replaceAll("/", "~")}.${read}.json`

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
export const recordedTransportImpl: Transport = {
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
