/**
 * The raw records read (`GET /v1/objects/{type}`): one page, at a decided workspace or from the
 * recording. Private to `lib/` (`test/workspace-reads-lint.test.ts`): pages read records only through
 * a workspace's reader (`lib/workspace.tsx`), so no page can read records at an undecided workspace.
 */
import { refusalOf } from "../cell-read.ts"
import { CellUnavailable, defaultFetch, parseUnavailableDetail, scopedHostFetch } from "../definitions.ts"
import { readDocument, type Read } from "../exact.ts"
import { nextCursor, recordsPath, servedQuery, snapshotMoved, totalOf, type RecordsQuery } from "../records-query.ts"
import type { RecordsRead } from "../records-source.ts"
import type { ObjectPage } from "../transport.ts"
import type { DecidedScope } from "../workspace.tsx"

type Fetch = (input: string, init?: RequestInit) => Promise<Response>

/** A recorded page as the grid reads it: it says it is recorded and that nothing was applied. */
export function recordedPage(read: Read<ObjectPage> | null): RecordsRead {
  if (read === null) return { kind: "none" }
  return { kind: "page", read, origin: "recorded", served: null, next: nextCursor(read.document), total: null }
}

/**
 * One page of the recording. It never sorts or filters: a query that asks for either is refused, as
 * the grid offers neither on a recorded page.
 */
export async function readRecordedRecords(objectType: string, query: RecordsQuery): Promise<RecordsRead> {
  if (query.sort.length > 0 || query.filter !== null) return { kind: "refused", status: 0, code: "studio.query_unavailable", message: "The recording can't sort or filter records.", diagnostics: [] }
  const { recordedRecords } = await import("./recorded-transport.ts")
  return recordedPage(recordedRecords.objectPage(objectType, query.after))
}

/**
 * One page of `objectType` records for `query`, sent to the decided workspace `scope` as the signed-in
 * actor. It never reads `/api/me` and never falls back to the recording: the caller checks the workspace
 * before and after (`readAtScope`).
 *
 * - A refusal is returned exactly, with every diagnostic. A 409 `object.snapshot_moved` is `restart`.
 * - A 503 (a cell outage such as `cell.unopenable`, or a proxy's) is {@link CellUnavailable}.
 * - A route the cell does not serve (404) is `none`; a sorted or filtered query it does not serve is
 *   refused with `studio.query_unavailable`.
 */
export async function readRecordsAtScope(
  objectType: string,
  query: RecordsQuery,
  scope: DecidedScope,
  fetcher: Fetch = defaultFetch,
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
  // A workspace's cell that does not answer is unavailable, never stood in for by the recording.
  if (response.status === 503) {
    let parsed: { code?: unknown; message?: unknown } | null = null
    if (text !== "") {
      try { parsed = JSON.parse(text) as { code?: unknown; message?: unknown } | null } catch { parsed = null }
    }
    const message = typeof parsed?.message === "string" && parsed.message !== "" ? parsed.message : undefined
    throw new CellUnavailable(response.status, message, parseUnavailableDetail(parsed))
  }
  if (response.status === 404) {
    if (query.sort.length > 0 || query.filter !== null) return { kind: "refused", status: response.status, code: "studio.query_unavailable", message: "This cell can't sort or filter records yet.", diagnostics: [] }
    return { kind: "none" }
  }
  const refusal = refusalOf(response.status, text)
  if (snapshotMoved(response.status, refusal?.code)) return { kind: "restart" }
  if (refusal !== null) {
    const { status, code, message, diagnostics } = refusal
    return { kind: "refused", status, code, message, diagnostics }
  }
  throw new CellUnavailable(response.status)
}
