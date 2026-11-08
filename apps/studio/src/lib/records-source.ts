import { CellUnavailable, readStudioScope, scopedHostFetch, type StudioScope } from "./definitions.ts"
import { readDocument, type Read } from "./exact.ts"
import { nextCursor, recordsPath, servedQuery, snapshotMoved, totalOf, type RecordsQuery, type Served, type Total } from "./records-query.ts"
import { transport, type ObjectPage } from "./transport.ts"

/** One diagnostic of a refusal, its pointer into the request (`/sort/0`, `/filter/all/1/value`, `/after`). */
export interface QueryDiagnostic { readonly code: string; readonly message: string; readonly pointer: string | null }

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

const defaults: RecordsDeps = { fetcher: (input, init) => fetch(input, init), recorded: transport.objectPage }

function recordedPage(read: Read<ObjectPage> | null): RecordsRead {
  if (read === null) return { kind: "none" }
  return { kind: "page", read, origin: "recorded", served: null, next: nextCursor(read.document), total: null }
}

function diagnosticsOf(body: unknown): readonly QueryDiagnostic[] {
  const list = (body as { diagnostics?: unknown } | null)?.diagnostics
  if (!Array.isArray(list)) return []
  return list.map((d: { code?: unknown; message?: unknown; pointer?: unknown }) => ({
    code: String(d.code ?? ""), message: String(d.message ?? ""), pointer: typeof d.pointer === "string" ? d.pointer : null,
  }))
}

/**
 * Read one page of `objectType` records for `query` from the cell, as the signed-in actor.
 *
 * - A workspace without a cell makes no call and reads `no-cell`.
 * - While the cell does not serve the object route (404 without a refusal, or 503: no host bound),
 *   the recorded documents are read instead, and they say so (`origin: "recorded"`, `served: null`).
 *   A query is never sent to the recording: it cannot sort or filter, and the grid offers neither.
 * - A refusal is returned exactly, with every diagnostic. A 409 `object.snapshot_moved` is `restart`.
 */
export async function readRecords(objectType: string, query: RecordsQuery, deps: RecordsDeps = defaults): Promise<RecordsRead> {
  let scope: StudioScope | null
  try {
    scope = await readStudioScope(deps.fetcher)
  } catch (error) {
    // No signed-in session reachable (local preview, tests): only the recording can be read.
    if (error instanceof CellUnavailable) return recordedPage(await deps.recorded(objectType, query.after))
    throw error
  }
  if (scope === null) return { kind: "no-cell" }
  const active = scope
  const response = await scopedHostFetch(deps.fetcher, "/api/host", async () => active)(recordsPath(objectType, query), { headers: { accept: "application/json" } })
  const text = await response.text()
  let body: unknown = null
  try { body = JSON.parse(text) } catch { /* not JSON: handled by status below */ }
  if (response.ok) {
    const read = readDocument<ObjectPage>(text, "live")
    return { kind: "page", read, origin: "live", served: servedQuery(read.document), next: nextCursor(read.document), total: totalOf(read.document) }
  }
  const code = (body as { code?: unknown } | null)?.code
  if (snapshotMoved(response.status, typeof code === "string" ? code : undefined)) return { kind: "restart" }
  if (typeof code === "string") {
    const message = (body as { message?: unknown }).message
    return { kind: "refused", status: response.status, code, message: typeof message === "string" ? message : code, diagnostics: diagnosticsOf(body) }
  }
  // The route is not served on this cell yet (no refusal document), or no host is bound to this stage.
  if (response.status === 404 || response.status === 503) {
    if (query.sort.length > 0 || query.filter !== null) return { kind: "refused", status: response.status, code: "studio.query_unavailable", message: "This cell can't sort or filter records yet.", diagnostics: [] }
    return recordedPage(await deps.recorded(objectType, query.after))
  }
  throw new CellUnavailable(response.status)
}

/** The diagnostics that point at one part of the request, e.g. `/filter/all/2`. */
export const diagnosticsAt = (diagnostics: readonly QueryDiagnostic[], prefix: string): readonly QueryDiagnostic[] =>
  diagnostics.filter(({ pointer }) => pointer === prefix || pointer?.startsWith(`${prefix}/`) === true)
