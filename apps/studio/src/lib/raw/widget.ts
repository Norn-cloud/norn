/**
 * The raw reads of a Widget UX page's data bindings (#1762 milestone C): one record, or one page of a
 * type's records, at a decided workspace or from the recording. Private to `lib/`
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

const pathOf = (read: BindingRead): string =>
  read.kind === "object"
    ? `/v1/objects/${encodeURIComponent(read.objectType)}/${encodeURIComponent(read.id)}`
    : recordsPath(read.objectType, { sort: [], filter: null, limit: read.size })

/**
 * One binding's read at the decided workspace `scope`, and only while the session is still there,
 * checked before and after (`readAtScope`): an answer read in another workspace is never shown.
 */
export function readWidgetAtScope(read: BindingRead, scope: DecidedScope, signal: AbortSignal, fetcher: Fetch = defaultFetch): Promise<Classified> {
  const call = scopedHostFetch(fetcher, "/api/host", async () => scope)
  return readAtScope(scope, (s) => readStudioScope(fetcher, s), async () => {
    let response: Response
    try {
      response = await call(pathOf(read), { headers: { accept: "application/json" }, signal })
    } catch (error) {
      if (signal.aborted) throw error
      // No data cell behind the workspace: the read is not served; any other failure is no answer.
      return error instanceof StudioScopeError && error.message === NO_DATA_CELL ? { outcome: "not_served", payload: {} } : { outcome: "network_failure", payload: {} }
    }
    return classifyText(response.status, await response.text(), parseExactJson)
  }, signal)
}

/** One binding's read from the recording: what it recorded, or not served when it recorded none. */
export async function readRecordedWidget(read: BindingRead): Promise<Classified> {
  const { recordedRecords } = await import("./recorded-transport.ts")
  if (read.kind === "object") {
    const found = recordedRecords.object(read.objectType, read.id)
    return found === null ? { outcome: "not_served", payload: {} } : { outcome: "accepted", payload: {}, body: found.object.document }
  }
  const page = recordedRecords.objectPage(read.objectType, undefined)
  return page === null ? { outcome: "not_served", payload: {} } : { outcome: "accepted", payload: {}, body: page.document }
}
