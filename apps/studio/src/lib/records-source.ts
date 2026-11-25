import { refusalOf, type Diagnostic } from "./cell-read.ts"
import { CellUnavailable, parseUnavailableDetail, readStudioScope, scopedHostFetch, type StudioScope } from "./definitions.ts"
import { readDocument, type Read } from "./exact.ts"
import { nextCursor, recordsPath, servedQuery, snapshotMoved, totalOf, type RecordsQuery, type Served, type Total } from "./records-query.ts"
import { currentMode } from "./live-transport.ts"
import { recordedTransport, type ObjectPage } from "./transport.ts"

/** One diagnostic of a refusal, its pointer into the request (`/sort/0`, `/filter/all/1/value`, `/after`). */
export type QueryDiagnostic = Diagnostic

/** What one page read came back as. Each kind is a state the grid shows; none is guessed. */
export type RecordsRead =
  | {
    readonly kind: "page"
    readonly read: Read<ObjectPage>
    /** `live` from the cell's object route; `recorded` from the replayed documents while that route is not served. */
    readonly origin: "live" | "recorded"
    /** What the server applied; null when it did not sort or filter. */
    readonly served: Served | null
    readonly next: string | undefined
    readonly total: Total | null
  }
  | { readonly kind: "refused"; readonly status: number; readonly code: string; readonly message: string; readonly diagnostics: readonly QueryDiagnostic[] }
  /** The data moved under the cursor (409 `object.snapshot_moved`): read again from page one. */
  | { readonly kind: "restart" }
  /** There is no read of this type at all (no recording, and the cell has no route for it). */
  | { readonly kind: "none" }
  | { readonly kind: "no-cell" }

type Fetch = (input: string, init?: RequestInit) => Promise<Response>

export interface RecordsDeps {
  readonly fetcher: Fetch
  readonly recorded: (objectType: string, after?: string) => Promise<Read<ObjectPage> | null>
}

/**
 * The recording is read only when no cell is connected: a connected cell's 404 is its answer, never
 * replaced. It is read from the recording itself, never through the mode-deciding transport, so a
 * mode decided live between the check and the read cannot answer here unscoped.
 */
const defaults: RecordsDeps = {
  fetcher: (input, init) => fetch(input, init),
  recorded: async (objectType, after) => (await currentMode()).kind === "live" ? null : recordedTransport.objectPage(objectType, after),
}

function recordedPage(read: Read<ObjectPage> | null): RecordsRead {
  if (read === null) return { kind: "none" }
  return { kind: "page", read, origin: "recorded", served: null, next: nextCursor(read.document), total: null }
}

async function fetchLiveRecords(
  objectType: string,
  query: RecordsQuery,
  scope: StudioScope,
  fetcher: Fetch,
  recorded: RecordsDeps["recorded"] | null,
  signal?: AbortSignal,
): Promise<RecordsRead> {
  const response = await scopedHostFetch(fetcher, "/api/host", async () => scope)(
    recordsPath(objectType, query),
    { headers: { accept: "application/json" }, ...(signal ? { signal } : {}) },
  )
  const text = await response.text()
  if (response.ok) {
    const read = readDocument<ObjectPage>(text, "live")
    return { kind: "page", read, origin: "live", served: servedQuery(read.document), next: nextCursor(read.document), total: totalOf(read.document) }
  }
  if (response.status === 503) {
    let parsed: { code?: unknown; message?: unknown } | null = null
    if (text !== "") {
      try { parsed = JSON.parse(text) as { code?: unknown; message?: unknown } | null } catch { parsed = null }
    }
    // Scoped live previews (`recorded === null`) and structured cell outages (such as `cell.unopenable`)
    // must surface their availability failure instead of pretending the type or query is unserved.
    if (recorded === null || typeof parsed?.code === "string") {
      const message = typeof parsed?.message === "string" && parsed.message !== "" ? parsed.message : undefined
      throw new CellUnavailable(response.status, message, parseUnavailableDetail(parsed))
    }
  }
  // A 404 or bare proxy 503 falls back before decoding the body, which may come from the proxy.
  if (response.status === 404 || response.status === 503) {
    if (query.sort.length > 0 || query.filter !== null) return { kind: "refused", status: response.status, code: "studio.query_unavailable", message: "This cell can't sort or filter records yet.", diagnostics: [] }
    return recorded === null ? { kind: "none" } : recordedPage(await recorded(objectType, query.after))
  }
  const refusal = refusalOf(response.status, text)
  if (snapshotMoved(response.status, refusal?.code)) return { kind: "restart" }
  if (refusal !== null) {
    const { status, code, message, diagnostics } = refusal
    return { kind: "refused", status, code, message, diagnostics }
  }
  throw new CellUnavailable(response.status)
}

/**
 * Read one page of `objectType` records pinned explicitly to `scope` without re-reading `/api/me`
 * or falling back to recorded fixtures. Callers verify the active workspace before and after dispatch.
 */
export async function readRecordsAtScope(
  objectType: string,
  query: RecordsQuery,
  scope: StudioScope,
  fetcher: Fetch = defaults.fetcher,
  signal?: AbortSignal,
): Promise<RecordsRead> {
  return fetchLiveRecords(objectType, query, scope, fetcher, null, signal)
}

/**
 * Read one page of `objectType` records for `query` from the cell, as the signed-in actor.
 *
 * - A workspace without a cell makes no call and reads `no-cell`.
 * - While the cell does not serve the object route (any 404 or 503, whatever its body),
 *   the recorded documents are read instead, and they say so (`origin: "recorded"`, `served: null`).
 *   A query is never sent to the recording: it cannot sort or filter, and the grid offers neither.
 * - A refusal is returned exactly, with every diagnostic. A 409 `object.snapshot_moved` is `restart`.
 */
export async function readRecords(objectType: string, query: RecordsQuery, deps: RecordsDeps = defaults, signal?: AbortSignal): Promise<RecordsRead> {
  let scope: StudioScope | null
  try {
    scope = await readStudioScope(deps.fetcher, signal)
  } catch (error) {
    if (signal?.aborted) throw error
    // No signed-in session reachable (local preview, tests): only the recording can be read.
    if (error instanceof CellUnavailable) return recordedPage(await deps.recorded(objectType, query.after))
    throw error
  }
  if (scope === null) return { kind: "no-cell" }
  return fetchLiveRecords(objectType, query, scope, deps.fetcher, deps.recorded, signal)
}

/** The diagnostics that point at one part of the request, e.g. `/filter/all/2`. */
export const diagnosticsAt = (diagnostics: readonly QueryDiagnostic[], prefix: string): readonly QueryDiagnostic[] =>
  diagnostics.filter(({ pointer }) => pointer === prefix || pointer?.startsWith(`${prefix}/`) === true)
