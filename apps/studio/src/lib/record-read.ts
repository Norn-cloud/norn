import { queryOptions } from "@tanstack/react-query"
import { StudioScopeError, type StudioScope } from "./definitions.ts"
import type { Fetch } from "./cell-read.ts"
import { readObjectAtScope } from "./live-transport.ts"
import { recordedTransport, type ObjectReads } from "./transport.ts"
import { readFindings } from "../evidence/review.ts"

/**
 * One record's read, shared by the record page and the grid's drawer. With a scope (a live cell's
 * decided workspace) it is read at that workspace, checked before and after the read, and cached
 * under it: the same id in another workspace is another record. Without one it reads only the
 * recording, never the mode-deciding transport, and is cached under its own key: no live answer can
 * land there, so a page that has no decided scope can never show a live record from the cache.
 */
export function recordRead(objectType: string, id: string, scope: StudioScope | undefined, fetcher?: Fetch) {
  return {
    queryKey: scope === undefined ? ["object", "recorded", objectType, id] as const : ["object", objectType, id, scope.orgId, scope.environmentId] as const,
    queryFn: ({ signal }: { readonly signal?: AbortSignal }): Promise<ObjectReads | null> => scope === undefined
      ? recordedTransport.object(objectType, id)
      : readObjectAtScope(objectType, id, scope, signal, fetcher),
  }
}

/**
 * The grid drawer's read of a row's record, or `null` when it must read nothing. A row is read at
 * the workspace the grid's records were decided in; a recorded row, outside any workspace, from the
 * recording. A live row while no workspace is decided is not read at all: an unscoped read could
 * come back live and be cached and revealed without its workspace.
 */
export function drawerRead(objectType: string, id: string, scope: StudioScope | undefined, origin: "live" | "recorded" | undefined) {
  if (scope !== undefined) return recordRead(objectType, id, scope)
  return origin === "recorded" ? recordRead(objectType, id, undefined) : null
}

/**
 * One record's findings, shared by the record page's header, its Findings panel and a quarantined
 * record's page: read at the workspace the page's mode was decided in and cached under it, as the
 * record's own read is. The recording keeps no findings, so with no decided workspace nothing is
 * read and the key is one no answer reaches: no findings cached in another workspace, or before
 * the mode failed, can show without one (#1745).
 */
export function findingsQuery(objectType: string, id: string, scope: StudioScope | undefined, fetcher?: Fetch) {
  return queryOptions({
    queryKey: scope === undefined ? ["findings", "undecided", objectType, id] : ["findings", objectType, id, scope.orgId, scope.environmentId],
    queryFn: ({ signal }) => scope === undefined
      ? Promise.reject(new StudioScopeError("No workspace is decided for this record's findings."))
      : readFindings(objectType, id, scope, signal, fetcher),
    enabled: scope !== undefined,
    retry: false,
  })
}
