import { member, readDocument, type Read } from "./exact.ts"
import type { Answer, Artifact, QuestionAnswer, Transport } from "./transport.ts"

/** Where the live cell host is and who is asking. */
export interface LiveConfig {
  /** The cell host's origin, for example `https://cell.example` (no trailing slash). */
  readonly baseUrl: string
  /**
   * The request's `Authorization` header value, or `undefined` for an
   * anonymous request. This is the one place sign-in (#817) supplies a session.
   */
  readonly authorization: () => string | undefined
  readonly fetch?: typeof fetch
}

/** The cells the live host publishes, by explicit scope. */
const CATALOG_SCOPE = { orgId: "org_demo", environmentId: "dev" }
const ANALYTICS_SCOPE = { orgId: "org_demo", environmentId: "analytics" }
const QUESTION = "demo.yield_week"

/** A refused read: the host's stable code and its one-line message. */
export class Refused extends Error {
  constructor(readonly status: number, readonly code: string, message: string) {
    super(message)
  }
}

/** The member at `pointer` as its own read, keeping each number's exact server text. */
export function within<T>(read: Read<unknown>, pointer: string): Read<T> {
  const tokens = pointer.split("/").slice(1).map((token) => token.replaceAll("~1", "/").replaceAll("~0", "~"))
  const document = tokens.reduce<unknown>((value, token) => (value as Record<string, unknown>)[token], read.document) as T
  const numbers = new Map<string, string>()
  for (const [at, text] of read.numbers) if (at.startsWith(`${pointer}/`)) numbers.set(at.slice(pointer.length), text)
  return { document, numbers, origin: read.origin }
}

/**
 * Documents served live by the cell host. Reads without a live route yet use
 * `fallback`; each read keeps its own origin, so the UI marks recorded ones.
 */
export function liveTransport(config: LiveConfig, fallback: Transport): Transport {
  const get = async <T,>(path: string, query: Record<string, string>): Promise<Read<T>> => {
    const headers: Record<string, string> = {}
    const authorization = config.authorization()
    if (authorization !== undefined) headers.authorization = authorization
    const response = await (config.fetch ?? fetch)(`${config.baseUrl}${path}?${new URLSearchParams(query)}`, { headers })
    const source = await response.text()
    if (!response.ok) {
      // A gateway may answer with plain text or HTML; keep its status either way.
      let refusal: { code?: unknown; message?: unknown } = {}
      try {
        const parsed: unknown = JSON.parse(source)
        if (typeof parsed === "object" && parsed !== null) refusal = parsed as typeof refusal
      } catch {}
      const code = typeof refusal.code === "string" ? refusal.code : "host.error"
      const message = typeof refusal.message === "string" ? refusal.message : `HTTP ${response.status}`
      throw new Refused(response.status, code, message)
    }
    return readDocument<T>(source, "live")
  }
  const answer = () => get<Answer>(`/v1/questions/${encodeURIComponent(QUESTION)}/answer`, ANALYTICS_SCOPE)
  return {
    ...fallback,
    catalog: async () => within<readonly Artifact[]>(await get("/v1/definitions", CATALOG_SCOPE), `/${member("artifacts")}`),
    questionBoard: async () => {
      const [recorded] = await fallback.questionBoard()
      if (recorded === undefined) return []
      const live: QuestionAnswer = { ...recorded, answer: await answer() }
      return [live]
    },
  }
}
