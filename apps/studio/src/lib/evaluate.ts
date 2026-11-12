import type { Answer } from "./transport.ts"
import { readDocument, type Read } from "./exact.ts"
import { isAnswer, isTrace, type Fetch, type LiveRead, type Refusal, type Trace } from "./live.ts"
import { CellUnavailable, NO_DATA_CELL, StudioScopeError, readStudioScope, scopedHostFetch } from "./definitions.ts"

/**
 * Running a question built in Studio (F2b, CELL-DO's final shape, query/evaluate
 * 33d21bea0d): POST /v1/questions:evaluate?orgId=&environmentId= with
 * { question }. The cell checks it first; an admitted question is answered with
 * the same document and through the same read gates as a published one
 * (withheld values, D53 suppression). Nothing is computed here.
 */
/** One checker finding: its code is the contract, its message what the reader sees; the pointer starts at /question. */
export interface CheckerFinding { readonly code: string; readonly severity: string | null; readonly message: string; readonly pointer: string | null }

export type Evaluation =
  | { readonly kind: "answer"; readonly read: Read<Answer> }
  /** The cell's checker refused the question (422 question.refused): each finding's code, message and field. */
  | { readonly kind: "not-a-question"; readonly findings: readonly CheckerFinding[] }
  /** The read gates refused this reader, or the question names a metric it may not read (the host's opaque 403 document). */
  | { readonly kind: "refused"; readonly refusal: Refusal }
  /** Over the cell's bounds (413): never a partial answer. */
  | { readonly kind: "too-large"; readonly code: string; readonly message: string }
  /** The cell refused the request itself (400, e.g. an unknown member). */
  | { readonly kind: "bad-request"; readonly message: string }
  /** Not answered: no host on this stage, signed out, the route not there yet, or no answer. */
  | { readonly kind: "unavailable"; readonly status: number; readonly reason?: "no-cell" }

type PostResult = { readonly status: number; readonly text: string } | { readonly noCell: true } | { readonly scopeStatus: number } | undefined
async function post(path: string, body: unknown, fetcher: Fetch): Promise<PostResult> {
  try {
    const scoped = scopedHostFetch(fetcher, "/api/host", () => readStudioScope(fetcher))
    const response = await scoped(path, { method: "POST", headers: { "content-type": "application/json", accept: "application/json" }, body: JSON.stringify(body) })
    return { status: response.status, text: await response.text() }
  } catch (error) {
    if (error instanceof StudioScopeError && error.message === NO_DATA_CELL) return { noCell: true }
    return error instanceof CellUnavailable ? { scopeStatus: error.status } : undefined
  }
}

const parse = (text: string): unknown => {
  try { return JSON.parse(text) } catch { return undefined }
}

const refusalOf = (status: number, text: string): Refusal | undefined => {
  if (status !== 403) return undefined
  const body = parse(text) as Partial<Refusal> | undefined
  return typeof body?.code === "string" && typeof body.message === "string" ? body as Refusal : undefined
}

export async function evaluateQuestion(question: unknown, fetcher: Fetch = fetch): Promise<Evaluation> {
  const response = await post("/v1/questions:evaluate", { question }, fetcher)
  if (response === undefined) return { kind: "unavailable", status: 0 }
  if ("noCell" in response) return { kind: "unavailable", status: 0, reason: "no-cell" }
  if ("scopeStatus" in response) return { kind: "unavailable", status: response.scopeStatus }
  const { status, text } = response
  if (status === 200) return isAnswer(parse(text)) ? { kind: "answer", read: readDocument<Answer>(text, "live") } : { kind: "unavailable", status }
  if (status === 422) {
    const body = parse(text) as { code?: unknown; diagnostics?: readonly { code?: unknown; severity?: unknown; message?: unknown; span?: { pointer?: unknown } | null }[] } | undefined
    if (body?.code === "question.refused" && Array.isArray(body.diagnostics)) {
      return { kind: "not-a-question", findings: body.diagnostics.map((d) => ({ code: String(d.code ?? ""), severity: typeof d.severity === "string" ? d.severity : null, message: String(d.message ?? ""), pointer: typeof d.span?.pointer === "string" ? d.span.pointer : null })) }
    }
  }
  if (status === 413 || status === 400) {
    const body = parse(text) as { code?: unknown; message?: unknown; error?: unknown } | undefined
    const message = typeof body?.message === "string" ? body.message : typeof body?.error === "string" ? body.error : ""
    if (status === 413) return { kind: "too-large", code: typeof body?.code === "string" ? body.code : "request.too_large", message }
    return { kind: "bad-request", message }
  }
  const refusal = refusalOf(status, text)
  return refusal ? { kind: "refused", refusal } : { kind: "unavailable", status }
}

/** One bucket's records for a question built in Studio: POST /v1/questions:trace, passing lineageRef back exactly as received. */
export async function traceQuestion(question: unknown, lineageRef: string, bucket: string, fetcher: Fetch = fetch, answered?: { readonly knownAsOf: number; readonly nowMs: number }): Promise<LiveRead<Trace>> {
  // With the answer's knownAsOf and nowMs the cell traces the answer as it was given, even after later commits.
  const response = await post("/v1/questions:trace", { question, lineageRef, bucket, ...answered ?? {} }, fetcher)
  if (response === undefined) return { kind: "unavailable", status: 0 }
  if ("noCell" in response) return { kind: "unavailable", status: 0, reason: "no-cell" }
  if ("scopeStatus" in response) return { kind: "unavailable", status: response.scopeStatus }
  const { status, text } = response
  if (status === 200) return isTrace(parse(text)) ? { kind: "read", read: readDocument<Trace>(text, "live") } : { kind: "unavailable", status }
  const refusal = refusalOf(status, text)
  return refusal ? { kind: "refused", refusal } : { kind: "unavailable", status }
}
