import { readDocument, type Read } from "./exact.ts"
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
  | { readonly kind: "unavailable"; readonly status: number }

/** The one call this module makes to fetch; narrower than the global fetch type, so tests can pass a plain function. */
export type Fetch = (input: string, init?: RequestInit) => Promise<Response>

/** One original record behind an answer bucket (the host's `NornNumericContribution`). */
export interface Contribution {
  readonly fact: Schemas["NornIrFactRef"]
  readonly amounts: readonly (number | null)[]
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
  let response: Response
  let text: string
  try {
    response = await fetcher(`/api/host${path}`, { headers: { accept: "application/json" } })
    text = await response.text()
  } catch {
    // The network failed: no live read, so the page shows its recording rather than a stale "Live".
    return { kind: "unavailable", status: 0 }
  }
  if (response.ok) {
    try {
      return { kind: "read", read: readDocument<T>(text, "live") }
    } catch {
      return { kind: "unavailable", status: response.status }
    }
  }
  if (response.status === 403) {
    try {
      const body = JSON.parse(text) as Partial<Refusal>
      if (typeof body.code === "string" && typeof body.message === "string") return { kind: "refused", refusal: body as Refusal }
    } catch { /* not the host's refusal document: the Worker's own */ }
  }
  return { kind: "unavailable", status: response.status }
}

const DEMO = "orgId=org_demo"

export const liveAnswer = (question: string, fetcher?: Fetch) =>
  hostRead<Answer>(`/v1/questions/${encodeURIComponent(question)}/answer?${DEMO}&environmentId=analytics`, fetcher)

/** One bucket of a live answer; `bucket` is the answer's row key exactly as the server wrote it. */
export const liveTrace = (question: string, lineageRef: string, bucket: string, fetcher?: Fetch) =>
  hostRead<Trace>(`/v1/questions/${encodeURIComponent(question)}/trace?${DEMO}&environmentId=analytics&lineageRef=${encodeURIComponent(lineageRef)}&bucket=${encodeURIComponent(bucket)}`, fetcher)

/** A contribution's amounts as the exact number texts the server sent. */
export const amountTexts = (read: Read<Trace>, index: number): readonly (string | null)[] =>
  (read.document.contributions[index]?.amounts ?? []).map((amount, at) =>
    amount === null ? null : read.numbers.get(`/contributions/${index}/amounts/${at}`) ?? String(amount))
