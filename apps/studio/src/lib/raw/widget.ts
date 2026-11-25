/**
 * The raw reads of a Widget UX page's data bindings (#1762 milestone C): one record, one page of a
 * type's records, or a record page's record with its links and lineage, at a decided workspace or
 * from the recording. Private to `lib/`
 * (`test/workspace-reads-lint.test.ts`): `<NornAppRenderer>` reads them only through a workspace's
 * reader (`lib/workspace.tsx`, `widget`).
 *
 * Each answer is classified as the page machine's call outcome (`@norn/react`'s `classifyResponse`):
 * what the cell said, never judged here. Documents are read exactly, every number its source text
 * (VAL-EXACT-1).
 */
import { parseExactJson } from "../../../../../packages/norn-sdk/src/index.ts"
import { classifyText, type Classified } from "../../../../../packages/norn-react/src/machine/outcome.ts"
import type { BindingRead } from "../../../../../packages/norn-react/src/reads.ts"
import { defaultFetch, NO_DATA_CELL, readAtScope, readStudioScope, scopedHostFetch, StudioScopeError } from "../definitions.ts"
import type { Fetch } from "../live-transport.ts"
import { recordsPath } from "../records-query.ts"
import type { DecidedScope } from "../workspace.tsx"
import { readDocument } from "../exact.ts"
import { readsOf } from "../record-name.ts"
import type { ObjectReads, ObjectRecord, SourceLineage } from "../transport.ts"
import { linkPage, objectPath, type CellLinkPage } from "./record.ts"

const pathOf = (read: Exclude<BindingRead, { readonly kind: "record" }>): string =>
  read.kind === "object"
    ? `/v1/objects/${encodeURIComponent(read.objectType)}/${encodeURIComponent(read.id)}`
    : recordsPath(read.objectType, { sort: [], filter: null, limit: read.size })

type Answer = { readonly status: number; readonly text: string }

/**
 * A record page's record (a context read, `BindingRead` `record`): the object, its links and its
 * lineage, read together as the record page always read them, classified as one answer. The object's
 * answer decides; then the links'; a lineage the cell does not have (404) is no lineage, any other
 * failure of it the answer. The body is the page's `ObjectReads`, every number exact.
 */
async function readRecord(get: (path: string) => Promise<Answer>, objectType: string, id: string): Promise<Classified> {
  const [object, links, lineage] = await Promise.all([
    get(objectPath(objectType, id)),
    get(objectPath(objectType, id, "links")),
    get(objectPath(objectType, id, "lineage")),
  ])
  for (const answer of [object, links, ...lineage.status === 404 ? [] : [lineage]]) {
    const classified = classifyText(answer.status, answer.text, parseExactJson)
    if (classified.outcome !== "accepted") return classified
  }
  const record = readDocument<ObjectRecord>(object.text, "live")
  const page = linkPage(readDocument<CellLinkPage>(links.text, "live"))
  // An answer that is not this record (another id, or links of another) is no answer for it.
  if (!readsOf(objectType, id, record.document, page.document)) return { outcome: "not_served", payload: {} }
  const reads: ObjectReads = { object: record, links: page, source: lineage.status === 404 ? null : readDocument<SourceLineage>(lineage.text, "live") }
  return { outcome: "accepted", payload: {}, body: reads }
}

/**
 * One binding's read at the decided workspace `scope`, and only while the session is still there,
 * checked before and after (`readAtScope`): an answer read in another workspace is never shown.
 */
export function readWidgetAtScope(read: BindingRead, scope: DecidedScope, signal: AbortSignal, fetcher: Fetch = defaultFetch): Promise<Classified> {
  const call = scopedHostFetch(fetcher, "/api/host", async () => scope)
  return readAtScope(scope, (s) => readStudioScope(fetcher, s), async () => {
    const get = async (path: string): Promise<Answer> => {
      const response = await call(path, { headers: { accept: "application/json" }, signal })
      return { status: response.status, text: await response.text() }
    }
    try {
      if (read.kind === "record") return await readRecord(get, read.objectType, read.id)
      const answer = await get(pathOf(read))
      return classifyText(answer.status, answer.text, parseExactJson)
    } catch (error) {
      if (signal.aborted) throw error
      // No data cell behind the workspace: the read is not served; any other failure is no answer.
      return error instanceof StudioScopeError && error.message === NO_DATA_CELL ? { outcome: "not_served", payload: {} } : { outcome: "network_failure", payload: {} }
    }
  }, signal)
}

/** One binding's read from the recording: what it recorded, or not served when it recorded none. */
export async function readRecordedWidget(read: BindingRead): Promise<Classified> {
  const { recordedRecords } = await import("./recorded-transport.ts")
  if (read.kind === "record") {
    const reads = recordedRecords.object(read.objectType, read.id)
    return reads === null ? { outcome: "not_served", payload: {} } : { outcome: "accepted", payload: {}, body: reads }
  }
  if (read.kind === "object") {
    const found = recordedRecords.object(read.objectType, read.id)
    return found === null ? { outcome: "not_served", payload: {} } : { outcome: "accepted", payload: {}, body: found.object.document }
  }
  const page = recordedRecords.objectPage(read.objectType, undefined)
  return page === null ? { outcome: "not_served", payload: {} } : { outcome: "accepted", payload: {}, body: page.document }
}
