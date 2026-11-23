import type { ObjectVersion } from "../evidence/review.ts"
import type { Bucket, Declarations } from "./answer.ts"
import type { Answer, SourceLineage } from "./transport.ts"

/**
 * Where one value came from, in the one shape every provenance card reads: a record's fact (from
 * the version the server says wrote it) or an answer's number (from the answer document). Each
 * part is copied from a server document; nothing here is inferred.
 */
export interface ValueProvenance {
  /** The source records or systems read; empty when the server names none. */
  readonly sources: readonly SourceRef[]
  /** What turned the source into this value. */
  readonly transform: Transform | null
  /** The ledger receipt that pins it. */
  readonly receipt: Receipt | null
  /** Digests that pin the source document and mapping bundle; `null` when the server withheld one. */
  readonly digests: readonly Digest[]
  /** When it was true and when Norn knew it, as the server wrote each time. */
  readonly asOf: readonly AsOfPart[]
}

export interface SourceRef {
  readonly system: string
  /** The record's identity in that system; `null` for an answer, which reads whole systems. */
  readonly record: string | null
  readonly fields: readonly string[]
  /** For an answer: whether the server reports this source's partitions complete. */
  readonly complete?: boolean
}

export type Transform =
  | { readonly kind: "mapping"; readonly artifactId: string; readonly version: string; readonly sha256: string; readonly rule: string | null; readonly upstream: number }
  | { readonly kind: "action"; readonly actionId: string; readonly actor: string }
  | { readonly kind: "question"; readonly metrics: readonly string[]; readonly records: number; readonly engine: string | null }
  | { readonly kind: "other" }

export type Receipt =
  | { readonly kind: "commit"; readonly commit: number }
  | { readonly kind: "head"; readonly seq: number; readonly hash: string }

export interface Digest {
  readonly label: string
  readonly value: string | null
}

export interface AsOfPart {
  readonly label: string
  readonly value: string
}

/** A logical time (ms) as the instant it names, in UTC. */
const instant = (ms: number): string => new Date(ms).toISOString()

/**
 * One fact of one record version. `version` is the history entry for the version shown (its commit
 * and writer); `lineage` is the record's source lineage when no history entry is read (a recorded
 * snapshot). A mapping writer carries its own lineage, which wins.
 */
export function factProvenance(fact: string, version: ObjectVersion | null, lineage: SourceLineage | null): ValueProvenance {
  const writer = version?.writer
  const mapped = writer?.kind === "mapping" ? writer.lineage : writer === undefined ? lineage : null
  const field = mapped?.fieldProvenance[fact]
  const receipt: Receipt | null = version ? { kind: "commit", commit: version.commit } : null
  const written: AsOfPart[] = version ? [{ label: "Written", value: instant(version.logicalTime) }] : []
  if (writer?.kind === "action") {
    return { sources: [], transform: { kind: "action", actionId: writer.actionId, actor: writer.actor }, receipt, digests: [], asOf: written }
  }
  if (mapped === null || mapped === undefined) {
    return { sources: [], transform: writer?.kind === "other" ? { kind: "other" } : null, receipt, digests: [], asOf: written }
  }
  const { mappingPin } = mapped
  const transform: Transform = { kind: "mapping", artifactId: mappingPin.artifactId, version: field?.mappingVersion ?? mappingPin.version, sha256: mappingPin.sha256, rule: field?.transform ?? null, upstream: field?.upstream?.length ?? 0 }
  const digests: Digest[] = [{ label: "Bundle", value: mapped.bundleDigest ?? null }, { label: "Document", value: mapped.documentDigest ?? null }]
  const source: SourceRef = field
    ? { system: field.source.system, record: field.source.record, fields: field.sourceFields ?? [] }
    : { system: mapped.sourceRecordRef.system, record: mapped.sourceRecordRef.record, fields: [] }
  const asOf: AsOfPart[] = [
    ...field?.observedAt ? [{ label: "Observed", value: field.observedAt }] : [],
    ...field ? [{ label: "Recorded", value: field.recordedAt }] : [],
    ...written,
  ]
  return { sources: [source], transform, receipt, digests, asOf }
}

/** The metrics a question aggregates, by their declared labels (the metric's name when the declaration is not at hand). */
export const metricLabels = ({ question, metrics }: Pick<Declarations, "question" | "metrics">): string[] =>
  question.aggregate.map(({ metric }) => metrics.find(({ name }) => name === metric)?.label ?? metric)

/** One bucket of an answer: the systems it read, the metrics that computed it, the ledger head it answered at. */
export function bucketProvenance(answer: Answer, bucket: Pick<Bucket, "members">, labels: readonly string[], engine: string | null): ValueProvenance {
  const systems = [...new Set([...Object.keys(answer.watermarks), ...Object.keys(answer.sourceCursors)])].sort()
  const sources = systems.map((system): SourceRef => {
    const partitions = Object.values(answer.watermarks[system] ?? {})
    return { system, record: null, fields: [], ...partitions.length === 0 ? {} : { complete: partitions.every(({ complete }) => complete) } }
  })
  return {
    sources,
    transform: { kind: "question", metrics: labels, records: bucket.members.length, engine },
    receipt: { kind: "head", seq: answer.revision.seq, hash: answer.revision.hash },
    digests: [],
    asOf: [
      { label: "Known", value: `after ${answer.knownAsOf} ${answer.knownAsOf === 1 ? "commit" : "commits"}` },
      { label: "Window to", value: answer.effectiveAsOf },
      { label: "Read", value: instant(answer.nowMs) },
    ],
  }
}

/**
 * The history entry for the version a page showed. `newer` is the latest version the history lists
 * when it is past the one shown; `missing` when the history does not list that version.
 */
export function versionShown(versions: readonly ObjectVersion[], version: number): { readonly entry: ObjectVersion | null; readonly newer: number | null } {
  const entry = versions.find((candidate) => candidate.version === version) ?? null
  const latest = versions[0]?.version
  return { entry, newer: latest !== undefined && latest > version ? latest : null }
}
