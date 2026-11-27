/**
 * The raw reads of one record: the object with its links and lineage, and its findings, at a decided
 * workspace; or the recorded object. Private to `lib/` (`test/workspace-reads-lint.test.ts`): pages read
 * a record only through a workspace's reader (`lib/workspace.tsx`).
 */
import { cellRead, type Fetch } from "../live-transport.ts"
import { CellUnavailable, defaultFetch, parseUnavailableDetail, readAtScope, readStudioScope, scopedHostFetch } from "../definitions.ts"
import { readDocument, type Read } from "../exact.ts"
import { readsOf } from "../record-name.ts"
import type { CountedLinkPage, LinkCounts } from "../record-page.ts"
import type { LinkPage, ObjectReads, ObjectRecord, SourceLineage } from "../transport.ts"
import type { DecidedScope } from "../workspace.tsx"
import type { ObjectFindings } from "../../evidence/review.ts"

export type CellLinkPage = { readonly objectType: string; readonly id: string; readonly items: LinkPage["items"]; readonly next?: string | null; readonly counts?: LinkCounts | null }

/** The cell's link page (`next` cursor) in the shape the pages read (`hasMore`), with its counts when it sends them. */
export function linkPage(read: Read<CellLinkPage>): Read<CountedLinkPage> {
  const { objectType, id, items, next, counts } = read.document
  return { ...read, document: { objectType, id, items, hasMore: typeof next === "string", ...counts ? { counts } : {} } }
}

export const objectPath = (objectType: string, id: string, leaf?: string) =>
  `/v1/objects/${encodeURIComponent(objectType)}/${encodeURIComponent(id)}${leaf ? `/${leaf}` : ""}`

/** The recorded object with its links and lineage; `null` when the recording has none. */
export async function readRecordedObject(objectType: string, id: string): Promise<ObjectReads | null> {
  const { recordedRecords } = await import("./recorded-transport.ts")
  return recordedRecords.object(objectType, id)
}

/**
 * One live record, its links and its lineage, read at the decided workspace `scope` and only while the
 * session is still there, checked before and after (`readAtScope`): the same id in another workspace is
 * another record. `null` when the cell answers 404 (no such object, or none this principal may read).
 */
export async function readObjectAtScope(
  objectType: string,
  id: string,
  scope: DecidedScope,
  signal?: AbortSignal,
  fetcher: Fetch = defaultFetch,
): Promise<ObjectReads | null> {
  const call = scopedHostFetch(fetcher, "/api/host", async () => scope)
  const read = async <T,>(path: string): Promise<Read<T> | null> => {
    const response = await call(path, { headers: { accept: "application/json" }, ...(signal === undefined ? {} : { signal }) })
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
  return readAtScope(
    scope,
    (s) => readStudioScope(fetcher, s),
    async () => {
      const [object, links, source] = await Promise.all([
        read<ObjectRecord>(objectPath(objectType, id)),
        read<CellLinkPage>(objectPath(objectType, id, "links")),
        // 404: no mapping produced this object (or its source is not readable): there is no lineage to show.
        read<SourceLineage>(objectPath(objectType, id, "lineage")),
      ])
      if (object === null || links === null) return null
      const page = linkPage(links)
      return readsOf(objectType, id, object.document, page.document) ? { object, links: page, source } : null
    },
    signal,
  )
}

/**
 * One record's findings, read at the decided workspace `scope` (the workspace its record was read at) and
 * only while the session is still there, checked before and after: the same id in another workspace is
 * another record, whose findings are never this one's.
 */
export const readFindings = (objectType: string, id: string, scope: DecidedScope, signal?: AbortSignal, fetcher?: Fetch): Promise<Read<ObjectFindings>> =>
  readAtScope(
    scope,
    (s) => readStudioScope(fetcher, s),
    () => cellRead<ObjectFindings>(objectPath(objectType, id, "findings"), "Findings", signal === undefined ? {} : { signal }, fetcher, scope),
    signal,
  )
