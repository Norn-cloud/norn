import { readDocument, type Read } from "./exact.ts"
import { CellUnavailable, NO_DATA_CELL, StudioScopeError, readStudioScope, scopedHostFetch } from "./definitions.ts"
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
  readonly receiptHash: string
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
export async function hostRead<T>(path: string, fetcher: Fetch = fetch): Promise<LiveRead<T>> {
  try {
    const scoped = scopedHostFetch(fetcher, "/api/host", () => readStudioScope(fetcher))
    const response = await scoped(path, { headers: { accept: "application/json" } })
    const text = await response.text()
    if (response.ok) {
      try { return { kind: "read", read: readDocument<T>(text, "live") } }
      catch { return { kind: "unavailable", status: response.status } }
    }
    if (response.status === 403) {
      try {
        const body = JSON.parse(text) as Partial<Refusal>
        if (typeof body.code === "string" && typeof body.message === "string") return { kind: "refused", refusal: body as Refusal }
      } catch { /* not the host's refusal document: the Worker's own */ }
    }
    return { kind: "unavailable", status: response.status }
  } catch (error) {
    return error instanceof StudioScopeError && error.message === NO_DATA_CELL
      ? { kind: "unavailable", status: 0, reason: "no-cell" }
      : { kind: "unavailable", status: error instanceof CellUnavailable ? error.status : 0 }
  }
}

export const liveAnswer = (question: string, fetcher?: Fetch) =>
  hostRead<Answer>(`/v1/questions/${encodeURIComponent(question)}/answer`, fetcher)

/** One bucket of a live answer; `bucket` is the answer's row key exactly as the server wrote it. */
export const liveTrace = (question: string, lineageRef: string, bucket: string, fetcher?: Fetch) => {
  const query = new URLSearchParams({ lineageRef, bucket })
  return hostRead<Trace>(`/v1/questions/${encodeURIComponent(question)}/trace?${query}`, fetcher)
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

/** The host's access document: the verified principal and decisions for the active cell. */
export interface Access {
  readonly principal: { readonly subjectId: string; readonly actorKind: string; readonly roles: readonly string[]; readonly roleTable: string }
  readonly policy: string
  readonly grantTable: string
  readonly capabilities: Readonly<Record<"population" | "values" | "sources", { readonly decision: "permit" | "deny"; readonly rule: string | null }>>
}

export const liveAccess = (fetcher?: Fetch) => hostRead<Access>("/v1/access", fetcher)
