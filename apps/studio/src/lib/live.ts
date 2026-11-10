import { readDocument, type Read } from "./exact.ts"
import { CellUnavailable, NO_DATA_CELL, StudioScopeError, readStudioScope, scopedHostFetch } from "./definitions.ts"
import { FRESHNESS } from "./format.ts"
import type { Answer, Schemas } from "./transport.ts"

/** A refusal exactly as the server wrote it (`{code, category, message, explanation}`). */
export interface Refusal {
  readonly code: string
  readonly category?: string
  readonly message: string
  readonly explanation?: string
}

/** A live read: the server's document, its refusal, or why live reads are not available here. */
export type LiveRead<T> =
  | { readonly kind: "read"; readonly read: Read<T> }
  | { readonly kind: "refused"; readonly refusal: Refusal }
  | { readonly kind: "unavailable"; readonly status: number; readonly reason?: "no-cell" }

/** The one call this module makes to fetch; narrower than the global fetch type, so tests can pass a plain function. */
export type Fetch = (input: string, init?: RequestInit) => Promise<Response>

/** One original record behind an answer bucket (the host's `NornNumericContribution`). */
export interface Contribution {
  readonly fact: Schemas["NornIrFactRef"]
  /** Sum or numerator/denominator; `null` when the reader may not see this row's values (D52, DATA-VALUES-1). */
  readonly amounts: readonly (number | null)[] | null
  /** `null` when the bucket has a restricted record: the receipt hash commits to the values (DATA-VALUES-1, #1175). */
  readonly receiptHash: string | null
  readonly factDigest: string | null
  readonly source: { readonly status: "resolved" | "restricted" | "unavailable" }
  readonly commitSeq: number
  readonly recordedAtMs: number
  /** The server withheld this record's values from the reader's role. */
  readonly restricted: boolean
  readonly values: Readonly<Record<string, unknown>> | null
}

/** The host's `NornNumericTrace`: one bucket of an answer, record by record. */
export interface Trace {
  readonly result: Answer
  readonly bucket: string
  readonly contributions: readonly Contribution[]
}

/**
 * Read through the Studio Worker's host route as the signed-in actor; the host
 * and its policy decide what comes back. 401 and 503 (no host bound to this
 * stage) mean live reads are not available, and the page keeps its recording.
 */
export async function hostRead<T>(path: string, fetcher: Fetch = fetch, isDocument: (value: unknown) => boolean = () => true): Promise<LiveRead<T>> {
  let response: Response
  let text: string
  try {
    const scoped = scopedHostFetch(fetcher, "/api/host", () => readStudioScope(fetcher))
    response = await scoped(path, { headers: { accept: "application/json" } })
    text = await response.text()
  } catch (error) {
    return error instanceof StudioScopeError && error.message === NO_DATA_CELL
      ? { kind: "unavailable", status: 0, reason: "no-cell" }
      : { kind: "unavailable", status: error instanceof CellUnavailable ? error.status : 0 }
  }
  if (response.ok) {
    try {
      const read = readDocument<T>(text, "live")
      // Valid JSON that is not the document asked for (`null`, an error object) is no live read either.
      if (isDocument(read.document)) return { kind: "read", read }
    } catch { /* not JSON */ }
    return { kind: "unavailable", status: response.status }
  }
  if (response.status === 403) {
    try {
      const body = JSON.parse(text) as Partial<Refusal>
      if (typeof body.code === "string" && typeof body.message === "string") return { kind: "refused", refusal: body as Refusal }
    } catch { /* not the host's refusal document: the Worker's own */ }
  }
  return { kind: "unavailable", status: response.status }
}

const isRecord = (value: unknown): value is Record<string, unknown> => typeof value === "object" && value !== null && !Array.isArray(value)

/** Amounts as the host sends them; `null` is a row whose values the server withheld from the reader's role. */
const isAmounts = (value: unknown): boolean => value === null || (Array.isArray(value) && value.every((amount) => amount === null || typeof amount === "number"))

/** One answer row as `buckets` reads it: a bucket key, its value (a number, or null), its totals, and each member's version and amounts. */
const isRow = (row: unknown): boolean =>
  isRecord(row) && "bucket" in row && (row.value === null || typeof row.value === "number") && Array.isArray(row.totals) && isRecord(row.members) &&
  Object.values(row.members).every((member) => isRecord(member) && typeof member.version === "number" && isAmounts(member.amounts))

/** The fields of a `NornQueryResult` the question card reads, down to each row and watermark partition. */
export const isAnswer = (value: unknown): boolean =>
  isRecord(value) && isRecord(value.rows) && Object.values(value.rows).every(isRow) &&
  isRecord(value.watermarks) && Object.values(value.watermarks).every((partitions) => isRecord(partitions) && Object.values(partitions).every((partition) => isRecord(partition) && typeof partition.complete === "boolean")) &&
  typeof value.freshness === "string" && Object.hasOwn(FRESHNESS, value.freshness) &&
  typeof value.lineageRef === "string" && typeof value.nowMs === "number" && typeof value.knownAsOf === "number"

/** The fields of a `NornNumericTrace` the records drawer reads, for every contribution. */
export const isTrace = (value: unknown): boolean =>
  isRecord(value) && typeof value.bucket === "string" && Array.isArray(value.contributions) &&
  value.contributions.every((contribution) => isRecord(contribution) && isRecord(contribution.fact) &&
    typeof contribution.fact.key === "string" && typeof contribution.fact.version === "number" && isAmounts(contribution.amounts) &&
    (typeof contribution.receiptHash === "string" || contribution.receiptHash === null) && typeof contribution.commitSeq === "number" && typeof contribution.restricted === "boolean")

export const liveAnswer = (question: string, fetcher?: Fetch) =>
  hostRead<Answer>(`/v1/questions/${encodeURIComponent(question)}/answer`, fetcher, isAnswer)

/** One bucket of a live answer; `bucket` is the answer's row key exactly as the server wrote it. */
export const liveTrace = (question: string, lineageRef: string, bucket: string, fetcher?: Fetch) => {
  const query = new URLSearchParams({ lineageRef, bucket })
  return hostRead<Trace>(`/v1/questions/${encodeURIComponent(question)}/trace?${query}`, fetcher, isTrace)
}

/**
 * A contribution's amounts as the exact number texts the server sent, or `null`
 * when it sent `amounts: null`: the row's values are withheld from the reader's
 * role. Nothing is ever computed in their place.
 */
export function amountTexts(read: Read<Trace>, index: number): readonly (string | null)[] | null {
  const amounts = read.document.contributions[index]?.amounts
  if (amounts == null) return null
  return amounts.map((amount, at) => amount === null ? null : read.numbers.get(`/contributions/${index}/amounts/${at}`) ?? String(amount))
}

/** The host's access document (`GET /v1/access`): who the server verified, and its decision per read capability, for the active cell. */
export interface Access {
  readonly principal: { readonly subjectId: string; readonly actorKind: string; readonly roles: readonly string[]; readonly roleTable: string }
  readonly policy: string
  readonly grantTable: string
  readonly capabilities: Readonly<Record<"population" | "values" | "sources", { readonly decision: "permit" | "deny"; readonly rule: string | null }>>
}

const isCapability = (value: unknown): boolean =>
  isRecord(value) && (value.decision === "permit" || value.decision === "deny") && (value.rule === null || typeof value.rule === "string")

/** The fields of an `Access` document that the decision panel reads. */
export const isAccess = (value: unknown): boolean =>
  isRecord(value) && isRecord(value.principal) && typeof value.principal.subjectId === "string" &&
  Array.isArray(value.principal.roles) && typeof value.principal.roleTable === "string" &&
  typeof value.policy === "string" && typeof value.grantTable === "string" && isRecord(value.capabilities) &&
  (["population", "values", "sources"] as const).every((name) => isCapability((value.capabilities as Record<string, unknown>)[name]))

/** The signed-in account's access decisions in the demo cell, computed by the host on each read. */
export const liveAccess = (fetcher?: Fetch) => hostRead<Access>("/v1/access", fetcher, isAccess)
