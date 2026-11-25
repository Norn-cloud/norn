/**
 * The cell's review reads for one object (HOST-READ-6): its quality findings
 * with the evidence objects linked to it, its version history with what wrote
 * each version, and a short-lived read of the scanned page an evidence object
 * points at. Every decision is the server's: a read the caller may not make
 * is the cell's refusal, shown as it is.
 */
import { cellRead, NotServed } from "../lib/live-transport.ts"
import { readAtScope, readStudioScope, type StudioScope } from "../lib/definitions.ts"
import type { Fetch } from "../lib/cell-read.ts"
import { member, type Read } from "../lib/exact.ts"
import type { ObjectRecord, SourceLineage } from "../lib/transport.ts"

/** The core interface every evidence type implements (registry/core/src/evidence.ts). */
export const SOURCE_EVIDENCE = "norn.SourceEvidence"
/** The core interface of scan provenance (bytes and extractor): shown with the evidence and in Technical views, not among a record's facts. */
export const SOURCE_DOCUMENT = "norn.SourceDocument"

export interface EvidenceRef { readonly objectType: string; readonly id: string }

export type RuleRef =
  | { readonly kind: "law"; readonly artifactId: string; readonly lawId: string; readonly version: string }
  | { readonly kind: "invariant"; readonly [key: string]: unknown }
  | { readonly kind: "builtin"; readonly id: string; readonly version: string }

export interface Finding {
  readonly id: string
  readonly ruleRef: RuleRef
  readonly severity: "info" | "warning" | "error" | "critical"
  readonly message: string
  readonly status: "open" | "acknowledged" | "resolved" | "superseded"
  readonly firstSeen: string
  readonly lastSeen: string
  /** DIAG-REASON-1: for a law finding, the values the law compared, by path, as the cell read them (a list for every line it summed). */
  readonly values?: Readonly<Record<string, unknown>>
}

export interface ObjectFindings {
  readonly objectType: string
  readonly id: string
  readonly quarantined: boolean
  readonly findings: readonly Finding[]
  readonly evidence: readonly EvidenceRef[]
}

export type VersionWriter =
  | { readonly kind: "mapping"; readonly lineage: SourceLineage }
  /** The action and its verified actor; the cell never lists the call's input. */
  | { readonly kind: "action"; readonly actionId: string; readonly actor: string }
  | { readonly kind: "other" }

export interface ObjectVersion {
  readonly version: number
  readonly commit: number
  readonly logicalTime: number
  readonly values: Readonly<Record<string, unknown>> | null
  readonly writer: VersionWriter
}

export interface ObjectHistory {
  readonly objectType: string
  readonly id: string
  readonly versions: readonly ObjectVersion[]
  readonly truncated: boolean
}

/** One evidence object's values, as `norn.SourceEvidence` declares them. */
export interface Evidence {
  readonly ref: EvidenceRef
  /** The property the value was read for, of `subject` when the evidence names one (a printed line), else of the record it is linked to. */
  readonly subjectProperty: string
  readonly subject: EvidenceRef | null
  readonly documentUri: string
  readonly documentDigest: string
  readonly mediaType: string
  readonly page: number
  /** The box as page fractions (top-left origin), exact text. */
  readonly box: { readonly x: string; readonly y: string; readonly w: string; readonly h: string }
  readonly text: string
  readonly extractor: string
  readonly extractorVersion: string
  readonly confidenceBp: number | null
}

const objectPath = (objectType: string, id: string, leaf?: string) =>
  `/v1/objects/${encodeURIComponent(objectType)}/${encodeURIComponent(id)}${leaf ? `/${leaf}` : ""}`

/**
 * One record's findings, read at `scope` (the workspace its record was read at) and only while the
 * session is still there, checked before and after the read: the same id in another workspace is
 * another record, whose findings are never this one's.
 */
export const readFindings = (objectType: string, id: string, scope: StudioScope, signal?: AbortSignal, fetcher?: Fetch): Promise<Read<ObjectFindings>> =>
  readAtScope(
    scope,
    (s) => readStudioScope(fetcher, s),
    () => cellRead<ObjectFindings>(objectPath(objectType, id, "findings"), "Findings", signal === undefined ? {} : { signal }, fetcher, scope),
    signal,
  )

/** Prefix shared with invalidators (`["history", objectType, id]`) for every workspace scope. */
export const historyQueryPrefix = (objectType: string, id: string) => ["history", objectType, id] as const

/** Workspace-scoped TanStack Query key for record history, sharing {@link historyQueryPrefix}. */
export const historyQueryKey = (objectType: string, id: string, scope: StudioScope | null) =>
  [...historyQueryPrefix(objectType, id), scope ? `${scope.orgId}:${scope.environmentId}` : ""] as const

export const readHistory = (objectType: string, id: string, boundScope: StudioScope | null): Promise<Read<ObjectHistory>> =>
  cellRead<ObjectHistory>(objectPath(objectType, id, "history"), "History", {}, undefined, boundScope)

/**
 * What a history panel says when the read fails: a cell that does not serve history says so, and
 * any refusal or failure is one plain state. A refusal is opaque (absent and withheld read alike),
 * so its text is never shown.
 */
export const historyUnavailable = (error: unknown): string =>
  error instanceof NotServed ? "This cell does not serve record history yet." : "History unavailable"

/** One evidence object, its decimals at their exact text. */
export async function readEvidence(ref: EvidenceRef): Promise<Evidence> {
  const read = await cellRead<ObjectRecord>(objectPath(ref.objectType, ref.id), "Evidence")
  const values = read.document.values
  const text = (key: string) => typeof values[key] === "string" ? values[key] as string : ""
  const exact = (key: string) => read.numbers.get(`/values/${member(key)}`) ?? String(values[key] ?? "")
  return {
    ref,
    subjectProperty: text("subject_property"),
    subject: text("subject_type") !== "" && text("subject_key") !== "" ? { objectType: text("subject_type"), id: text("subject_key") } : null,
    documentUri: text("document_uri"),
    documentDigest: text("document_digest"),
    mediaType: text("media_type"),
    page: typeof values.page === "number" ? values.page : 1,
    box: { x: exact("bbox_x"), y: exact("bbox_y"), w: exact("bbox_w"), h: exact("bbox_h") },
    text: text("text"),
    extractor: text("extractor"),
    extractorVersion: text("extractor_version"),
    confidenceBp: typeof values.confidence_bp === "number" ? values.confidence_bp : null,
  }
}

/** A short-lived read of a scanned page the cell recorded, after the cell's policy decides. */
export const readScan = (uri: string): Promise<Read<{ readonly getUrl: string; readonly expiresAt: string }>> =>
  cellRead<{ readonly getUrl: string; readonly expiresAt: string }>("/v1/uploads:read", "Scanned pages", { method: "POST", body: JSON.stringify({ uri }) })

/**
 * The evidence for one fact: rows whose source field is one the fact was read
 * from (`total`), or a row of a child table it was computed from (`lines` →
 * `lines.2.amount`).
 */
export function evidenceForFields(evidence: readonly Evidence[], sourceFields: readonly string[]): Evidence[] {
  return evidence.filter((item) => sourceFields.some((field) => item.subjectProperty === field || item.subjectProperty.startsWith(`${field}.`)))
}

/** A version's value of one property as its exact text, `—` when absent. */
export function versionValue(read: Read<ObjectHistory>, index: number, property: string): string {
  const version = read.document.versions[index]
  const value = version?.values?.[property]
  if (value === undefined || value === null) return "—"
  return read.numbers.get(`/versions/${index}/values/${member(property)}`) ?? (typeof value === "string" ? value : JSON.stringify(value))
}

/** The properties whose value differs between a version and the one before it. */
export function changedProperties(read: Read<ObjectHistory>, index: number): string[] {
  const version = read.document.versions[index]
  const before = read.document.versions[index + 1]
  if (!version?.values) return []
  if (!before?.values) return Object.keys(version.values)
  return Object.keys(version.values).filter((property) => versionValue(read, index, property) !== versionValue(read, index + 1, property))
}
