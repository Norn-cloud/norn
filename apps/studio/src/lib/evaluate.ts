import type { Answer } from "./transport.ts"
import { readDocument, type Read } from "./exact.ts"
import type { Fetch, LiveRead, Refusal, Trace } from "./live.ts"

/**
 * Running a question built in Studio (F2b, CELL-DO's planned contract,
 * 2026-11-06): POST /v1/questions:evaluate with { question }. The cell checks
 * it first; an admitted question is answered with the same document and
 * through the same read gates as a published one (withheld values, D53
 * suppression). Nothing is computed here.
 */
export interface CheckerFinding { readonly code: string; readonly message: string; readonly law: string | null; readonly pointer: string | null }

export type Evaluation =
  | { readonly kind: "answer"; readonly read: Read<Answer> }
  /** The cell's checker refused the question (422 question.refused), naming the law and the field. */
  | { readonly kind: "not-a-question"; readonly findings: readonly CheckerFinding[] }
  /** The read gates refused this reader (the host's 403 document). */
  | { readonly kind: "refused"; readonly refusal: Refusal }
  /** Not answered: no host on this stage, signed out, the route not there yet, or no answer. */
  | { readonly kind: "unavailable"; readonly status: number }

async function post(path: string, body: unknown, fetcher: Fetch): Promise<{ readonly status: number; readonly text: string } | undefined> {
  try {
    const response = await fetcher(`/api/host${path}`, { method: "POST", headers: { "content-type": "application/json", accept: "application/json" }, body: JSON.stringify(body) })
    return { status: response.status, text: await response.text() }
  } catch {
    return undefined
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
  const { status, text } = response
  if (status === 200 && parse(text) !== undefined) return { kind: "answer", read: readDocument<Answer>(text, "live") }
  if (status === 422) {
    const body = parse(text) as { code?: unknown; diagnostics?: readonly { code?: unknown; message?: unknown; law?: unknown; span?: { pointer?: unknown } | null }[] } | undefined
    if (body?.code === "question.refused" && Array.isArray(body.diagnostics)) {
      return { kind: "not-a-question", findings: body.diagnostics.map((d) => ({ code: String(d.code ?? ""), message: String(d.message ?? ""), law: typeof d.law === "string" ? d.law : null, pointer: typeof d.span?.pointer === "string" ? d.span.pointer : null })) }
    }
  }
  const refusal = refusalOf(status, text)
  return refusal ? { kind: "refused", refusal } : { kind: "unavailable", status }
}

/** One bucket's records for a question built in Studio: POST /v1/questions:trace, passing lineageRef back exactly as received. */
export async function traceQuestion(question: unknown, lineageRef: string, bucket: string, fetcher: Fetch = fetch): Promise<LiveRead<Trace>> {
  const response = await post("/v1/questions:trace", { question, lineageRef, bucket }, fetcher)
  if (response === undefined) return { kind: "unavailable", status: 0 }
  const { status, text } = response
  if (status === 200 && parse(text) !== undefined) return { kind: "read", read: readDocument<Trace>(text, "live") }
  const refusal = refusalOf(status, text)
  return refusal ? { kind: "refused", refusal } : { kind: "unavailable", status }
}
