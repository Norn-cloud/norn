/**
 * Studio over a connected cell: which documents the pages read (the cell's, or
 * the recording when no cell is connected) and the live reads themselves. Kept
 * apart from transport.ts, which finds recordings with Vite's import.meta.glob.
 */
import { useQuery } from "@tanstack/react-query"
import { CellUnavailable, StudioScopeError, parseUnavailableDetail, readStudioScope, recordCellReachable, scopedHostFetch, type ScopedRequestInit, type StudioScope, type UnavailableDetail } from "./definitions.ts"
import { readDocument, type Read } from "./exact.ts"
import { readsOf } from "./objects.ts"
import type { Artifact, Envelope, LinkPage, ObjectPage, ObjectRecord, SourceLineage, Transport } from "./transport.ts"
import type { CountedLinkPage, LinkCounts } from "./record-page.ts"
import { totalOf, type Total } from "./records-query.ts"

/** A read this cell does not serve. While a cell is connected nothing recorded stands in for it. */
export class NotServed extends Error {
  constructor(what: string) { super(`${what} not served by this cell yet.`) }
}

export type Fetch = (input: string, init?: RequestInit) => Promise<Response>

/** The documents under `prefix` (an RFC 6901 pointer) of one read, with their exact numbers. */
export function subRead<T>(read: Read<unknown>, prefix: string, document: T): Read<T> {
  const numbers = new Map<string, string>()
  for (const [pointer, text] of read.numbers) if (pointer === prefix || pointer.startsWith(`${prefix}/`)) numbers.set(pointer.slice(prefix.length), text)
  return { document, numbers, origin: read.origin }
}

/** The cell's link page (`next` cursor) in the shape the pages read (`hasMore`), with its counts when it sends them. */
function linkPage(read: Read<{ readonly objectType: string; readonly id: string; readonly items: LinkPage["items"]; readonly next?: string | null; readonly counts?: LinkCounts | null }>): Read<CountedLinkPage> {
  const { objectType, id, items, next, counts } = read.document
  return { ...read, document: { objectType, id, items, hasMore: typeof next === "string", ...counts ? { counts } : {} } }
}

/** Where Studio's documents come from: the connected cell, or the recording when no cell is connected. */
export type Mode = { readonly kind: "live"; readonly catalog: Read<readonly Artifact[]>; readonly counts: ObjectCounts } | { readonly kind: "recorded" }

/**
 * Each object type's record count, as the cell counted it for this caller with the records list's own
 * permissions. A type the caller may not list has no entry; the cell decides, and Studio only shows it.
 */
export type ObjectCounts = Readonly<Record<string, Total>>

/** The catalog's `objectCounts`, keeping only well-formed counts. */
export function countsOf(document: unknown): ObjectCounts {
  const raw = (document as { objectCounts?: unknown } | null)?.objectCounts
  if (raw === null || typeof raw !== "object" || Array.isArray(raw)) return {}
  return Object.fromEntries(Object.entries(raw).flatMap(([objectType, total]) => {
    const counted = totalOf({ total })
    return counted ? [[objectType, counted] as const] : []
  }))
}

/**
 * Decide once where documents come from. A workspace whose server reports a scope and whose cell
 * answers `GET /v1/definitions` is live, and every read then goes to that cell: a read it does not
 * serve says so ({@link NotServed}) and is never replaced by a recording. With no session, no cell
 * or no host connected (a 503, a 404, or a page that is not a document) Studio shows the recording,
 * and every recorded read says it is one. Any other answer is the cell's and is shown as an error.
 */
/** What the Studio Worker answers when this deployment has no Norn host configured (worker/index.ts). */
const NO_HOST = "No Norn host is connected to this deployment."

export async function readMode(fetcher: Fetch): Promise<Mode> {
  let scope: StudioScope | null
  try { scope = await readStudioScope(fetcher) } catch (error) {
    // No session server (a preview without the Worker, one that reports no scope, or no session): nothing is connected.
    // A session server that failed to answer (a network error or a 5xx) is not "nothing connected": it is an
    // outage, raised so Studio says it is reconnecting and retries, never swapping a live cell for the recording.
    if (error instanceof CellUnavailable && (error.status === 0 || error.status >= 500)) throw error
    if (error instanceof CellUnavailable || error instanceof StudioScopeError) return { kind: "recorded" }
    throw error
  }
  if (scope === null) return { kind: "recorded" }
  const active = scope
  let response: Response
  let text: string
  try {
    response = await scopedHostFetch(fetcher, "/api/host", async () => active)("/v1/definitions", { headers: { accept: "application/json" } })
    text = await response.text()
  } catch (error) {
    if (error instanceof CellUnavailable || error instanceof StudioScopeError) throw error
    throw new CellUnavailable(0, undefined, { retryable: true })
  }
  // Only 404 or this deployment's own "no host connected" 503 means there is no cell; any other
  // non-OK answer (including HTML 502/503/504 gateway errors or cell refusals) is raised so Studio
  // retries transient outages or shows the refusal rather than swapping a live cell for the recording.
  if (response.status === 404) return { kind: "recorded" }
  let parsed: unknown = null
  let isJson = false
  try { parsed = JSON.parse(text); isJson = true } catch { /* non-JSON body */ }
  if (response.status === 503 && isJson && (parsed as { error?: unknown } | null)?.error === NO_HOST) {
    return { kind: "recorded" }
  }
  if (!response.ok) {
    const raw = isJson ? (parsed as { message?: unknown } | null) : null
    const message = typeof raw?.message === "string" && raw.message !== "" ? raw.message : undefined
    throw new CellUnavailable(response.status, message, isJson ? parseUnavailableDetail(parsed) : {})
  }
  // A 200 HTML page (static SPA fallback with no Worker) means no live cell is connected.
  if (!isJson) return { kind: "recorded" }
  const artifacts = (parsed as { artifacts?: unknown } | null)?.artifacts
  if (!Array.isArray(artifacts)) throw new CellUnavailable(response.status, "The cell's definitions read carried no artifacts.")
  recordCellReachable()
  const read = readDocument<{ readonly artifacts: readonly Artifact[] }>(text, "live")
  return { kind: "live", catalog: subRead(read, "/artifacts", read.document.artifacts), counts: countsOf(read.document) }
}

/** The live transport over one connected cell's routes. */
export function liveTransport(catalog: Read<readonly Artifact[]>, fetcher: Fetch, counts: ObjectCounts = {}): Transport {
  const call = scopedHostFetch(fetcher, "/api/host", () => readStudioScope(fetcher))
  /** One read; `null` when the cell answers 404 (no such object, or none this principal may read). */
  const read = async <T,>(path: string): Promise<Read<T> | null> => {
    const response = await call(path, { headers: { accept: "application/json" } })
    const text = await response.text()
    if (response.status === 404) return null
    if (!response.ok) {
      let parsed: unknown = null
      try { parsed = JSON.parse(text) } catch { /* not a refusal document */ }
      const message = typeof (parsed as { message?: unknown } | null)?.message === "string" ? (parsed as { message: string }).message : undefined
      throw new CellUnavailable(response.status, message, parseUnavailableDetail(parsed))
    }
    return readDocument<T>(text, "live")
  }
  const objectPath = (objectType: string, id: string, leaf?: string) => `/v1/objects/${encodeURIComponent(objectType)}/${encodeURIComponent(id)}${leaf ? `/${leaf}` : ""}`
  return {
    catalog: async () => catalog,
    objectCounts: async () => counts,
    definitions: async () => catalog.document.map((artifact, index) => subRead(catalog, `/${index}`, artifact as Envelope)),
    questionBoard: async () => { throw new NotServed("Saved questions are") },
    restatement: async () => { throw new NotServed("Restated answers are") },
    guarantees: async () => { throw new NotServed("Model-checked proofs are") },
    objectPage: async (objectType, after) => read<ObjectPage>(`/v1/objects/${encodeURIComponent(objectType)}${after === undefined ? "" : `?after=${encodeURIComponent(after)}`}`),
    object: async (objectType, id) => {
      const [object, links, source] = await Promise.all([
        read<ObjectRecord>(objectPath(objectType, id)),
        read<{ objectType: string; id: string; items: LinkPage["items"]; next?: string | null; counts?: LinkCounts | null }>(objectPath(objectType, id, "links")),
        // 404: no mapping produced this object (or its source is not readable): there is no lineage to show.
        read<SourceLineage>(objectPath(objectType, id, "lineage")),
      ])
      if (object === null || links === null) return null
      const page = linkPage(links)
      return readsOf(objectType, id, object.document, page.document) ? { object, links: page, source } : null
    },
  }
}

let mode: Promise<Mode> | undefined
const browserFetch: Fetch = (input, init) => fetch(input, init)

/** The mode this page load reads in; a failed decision is tried again on the next read. */
export function currentMode(fetcher: Fetch = browserFetch): Promise<Mode> {
  mode ??= readMode(fetcher).catch((error: unknown) => { mode = undefined; throw error })
  return mode
}

/** A refusal the cell sent for a read: its code, message, and structured detail, shown honestly. */
export class CellRefusal extends Error {
  readonly explanation: string | undefined
  readonly retryable: boolean | undefined
  readonly incidentId: string | undefined
  readonly category: string | undefined
  readonly decidedAt: string | undefined

  constructor(readonly status: number, readonly code: string, message: string, detail: UnavailableDetail = {}) {
    super(message)
    this.explanation = detail.explanation
    this.retryable = detail.retryable
    this.incidentId = detail.incidentId
    this.category = detail.category
    this.decidedAt = detail.decidedAt
  }
}

/**
 * One scoped read of a cell route, with every number's exact text. A route this cell does not
 * serve (a 404 `route.not_found`, or a deployment with no host configured) is {@link NotServed}
 * under `what`; a cell-level 503 refusal (such as `cell.unopenable`) preserves its code,
 * explanation, `retryable`, and `incidentId` as {@link CellUnavailable}; any other Norn refusal is
 * {@link CellRefusal}.
 */
export async function cellRead<T>(path: string, what: string, init: ScopedRequestInit = {}, fetcher: Fetch = browserFetch): Promise<Read<T>> {
  const call = scopedHostFetch(fetcher, "/api/host", () => readStudioScope(fetcher))
  const response = await call(path, { ...init, headers: { accept: "application/json", ...init.body === undefined ? {} : { "content-type": "application/json" }, ...init.headers } })
  const text = await response.text()
  if (response.ok) return readDocument<T>(text, "live")
  let refusal: { code?: unknown; message?: unknown; error?: unknown } | null = null
  try { refusal = JSON.parse(text) as { code?: unknown; message?: unknown; error?: unknown } } catch { /* not a refusal document */ }
  const detail = parseUnavailableDetail(refusal)
  if (response.status === 404 && (refusal?.code === "route.not_found" || refusal?.code === undefined)) throw new NotServed(what)
  if (response.status === 503 && refusal?.error === NO_HOST) throw new NotServed(what)
  if (response.status === 503) {
    const message = typeof refusal?.message === "string" && refusal.message !== "" ? refusal.message : undefined
    throw new CellUnavailable(response.status, message, detail)
  }
  if (typeof refusal?.code === "string") {
    throw new CellRefusal(response.status, refusal.code, typeof refusal.message === "string" ? refusal.message : refusal.code, detail)
  }
  throw new CellUnavailable(response.status, typeof refusal?.message === "string" ? refusal.message : undefined, detail)
}

/**
 * Whether a mode-read error is transient and worth a brief automatic retry. A refusal with
 * `retryable: false` (such as `503 cell.unopenable`) and non-transient 4xx answers are never
 * retried automatically, so Studio settles immediately into an honest unavailable state.
 */
export function isRetryableModeError(error: unknown): boolean {
  if (!(error instanceof CellUnavailable)) return false
  if (error.retryable === false) return false
  if (error.status >= 400 && error.status < 500 && error.status !== 408 && error.status !== 429) return false
  return true
}

/**
 * Brief, bounded automatic retries for transient outages (~5.6 s total) before settling into a
 * persistent Workspace unavailable state. Non-retryable refusals (`retryable: false`) settle
 * immediately without ever rendering "Retrying…".
 */
export const MODE_AUTO_RETRIES = 3

/** Where this page load reads from, for a page that shows something only a connected cell has. */
export const useMode = () => useQuery({
  queryKey: ["studio-mode"],
  queryFn: () => currentMode(),
  retry: (failureCount, error) => isRetryableModeError(error) && failureCount < MODE_AUTO_RETRIES,
  retryDelay: (attempt) => Math.min(800 * 2 ** attempt, 3200),
  staleTime: Infinity,
})

/** Forget the decided mode, so the next read takes the cell's current catalog (after a load installs a new type). */
export function refreshMode(): void { mode = undefined }
